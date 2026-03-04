use super::file_transfer::FileReceiver;
use sha2::{Digest, Sha256};
use super::task_queue::{TaskQueue, TransferTask};
use super::progress::ProgressTracker;
use crate::protocol::{Message, TextAck, TextMessage, TransferComplete, TransferError, TransferRequest, TransferResponse};
use crate::message_store::{ChatMessage, MessageStore};
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{oneshot, RwLock, Semaphore};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

pub struct TransferService {
    listener: Arc<TcpListener>,
    transfer_port: u16,
    task_queue: Arc<TaskQueue>,
    progress_tracker: Arc<ProgressTracker>,
    cancelled: Arc<RwLock<HashSet<String>>>,
    paused: Arc<RwLock<HashSet<String>>>,
    pending_confirmations: Arc<RwLock<HashMap<String, oneshot::Sender<bool>>>>,
    require_confirmation: Arc<RwLock<bool>>,
    data_dir: PathBuf,
    download_dir: Arc<RwLock<PathBuf>>,
    chunk_size: Arc<RwLock<u32>>,
    enable_compression: Arc<RwLock<bool>>,
    speed_limit_bytes_per_sec: Arc<RwLock<u64>>,
    message_store: Arc<MessageStore>,
    semaphore: Arc<RwLock<Arc<Semaphore>>>,
}

impl TransferService {
    pub async fn new(
        transfer_port: u16,
        max_concurrent: usize,
        chunk_size: u32,
        data_dir: PathBuf,
        download_dir: PathBuf,
    ) -> Result<Self> {
        Self::new_with_compression(transfer_port, max_concurrent, chunk_size, data_dir, download_dir, false, 0).await
    }

    pub async fn new_with_compression(
        transfer_port: u16,
        max_concurrent: usize,
        chunk_size: u32,
        data_dir: PathBuf,
        download_dir: PathBuf,
        enable_compression: bool,
        speed_limit_bytes_per_sec: u64,
    ) -> Result<Self> {
        let port = if transfer_port == 0 {
            Self::find_available_port().await?
        } else {
            transfer_port
        };

        let listener = TcpListener::bind(format!("0.0.0.0:{}", port)).await?;
        info!("Transfer service listening on port {}", port);

        let message_store = Arc::new(MessageStore::new(data_dir.clone()));
        let semaphore = Arc::new(RwLock::new(Arc::new(Semaphore::new(max_concurrent))));

        Ok(Self {
            listener: Arc::new(listener),
            transfer_port: port,
            task_queue: Arc::new(TaskQueue::new(max_concurrent)),
            progress_tracker: Arc::new(ProgressTracker::new()),
            cancelled: Arc::new(RwLock::new(HashSet::new())),
            paused: Arc::new(RwLock::new(HashSet::new())),
            pending_confirmations: Arc::new(RwLock::new(HashMap::new())),
            require_confirmation: Arc::new(RwLock::new(false)),
            data_dir,
            download_dir: Arc::new(RwLock::new(download_dir)),
            chunk_size: Arc::new(RwLock::new(chunk_size)),
            enable_compression: Arc::new(RwLock::new(enable_compression)),
            speed_limit_bytes_per_sec: Arc::new(RwLock::new(speed_limit_bytes_per_sec)),
            message_store,
            semaphore,
        })
    }

    async fn find_available_port() -> Result<u16> {
        for port in 37050..37100 {
            if let Ok(listener) = TcpListener::bind(format!("0.0.0.0:{}", port)).await {
                drop(listener);
                return Ok(port);
            }
        }
        Err(anyhow::anyhow!("No available port found"))
    }

    pub async fn start(self: Arc<Self>) {
        let accept_task = {
            let service = self.clone();
            tokio::spawn(async move {
                service.accept_loop().await;
            })
        };

        let process_task = {
            let service = self.clone();
            tokio::spawn(async move {
                service.process_queue_loop().await;
            })
        };

        let _ = tokio::join!(accept_task, process_task);
    }

    async fn accept_loop(&self) {
        loop {
            match self.listener.accept().await {
                Ok((stream, addr)) => {
                    let service = Arc::new(self.clone());
                    tokio::spawn(async move {
                        if let Err(e) = service.handle_connection(stream, addr).await {
                            error!("Failed to handle connection from {}: {}", addr, e);
                        }
                    });
                }
                Err(e) => {
                    error!("Failed to accept connection: {}", e);
                }
            }
        }
    }

    async fn read_msg(stream: &mut TcpStream) -> Result<Message> {
        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf).await?;
        let len = u32::from_be_bytes(len_buf) as usize;
        if len > 64 * 1024 * 1024 {
            return Err(anyhow::anyhow!("Message too large: {} bytes", len));
        }
        let mut buf = vec![0u8; len];
        stream.read_exact(&mut buf).await?;
        Message::from_bytes(&buf)
    }

    async fn write_msg(stream: &mut TcpStream, msg: &Message) -> Result<()> {
        let data = msg.to_bytes()?;
        let len = (data.len() as u32).to_be_bytes();
        stream.write_all(&len).await?;
        stream.write_all(&data).await?;
        stream.flush().await?;
        Ok(())
    }

    async fn write_chunk_raw(
        stream: &mut TcpStream,
        chunk_index: u32,
        compressed: bool,
        data: &[u8],
    ) -> Result<()> {
        let mut header = [0u8; 9];
        header[0..4].copy_from_slice(&(data.len() as u32).to_le_bytes());
        header[4..8].copy_from_slice(&chunk_index.to_le_bytes());
        header[8] = compressed as u8;
        stream.write_all(&header).await?;
        stream.write_all(data).await?;
        Ok(())
    }

    async fn read_chunk_raw(stream: &mut TcpStream) -> Result<(u32, bool, Vec<u8>)> {
        let mut header = [0u8; 9];
        stream.read_exact(&mut header).await?;
        let data_len = u32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
        let chunk_index = u32::from_le_bytes(header[4..8].try_into().unwrap());
        let compressed = header[8] != 0;
        if data_len > 128 * 1024 * 1024 {
            return Err(anyhow::anyhow!("Chunk too large: {} bytes", data_len));
        }
        let mut data = vec![0u8; data_len];
        stream.read_exact(&mut data).await?;
        Ok((chunk_index, compressed, data))
    }

    async fn handle_connection(&self, mut stream: TcpStream, addr: SocketAddr) -> Result<()> {
        stream.set_nodelay(true)?;
        info!("New connection from {}", addr);

        let message = Self::read_msg(&mut stream).await?;

        match message {
            Message::TransferRequest(request) => {
                self.handle_transfer_request(stream, request).await?;
            }
            Message::TextMessage(msg) => {
                self.handle_text_message(stream, msg).await?;
            }
            _ => {
                warn!("Unexpected message type from {}", addr);
            }
        }

        Ok(())
    }

    async fn handle_transfer_request(
        &self,
        mut stream: TcpStream,
        request: TransferRequest,
    ) -> Result<()> {
        info!(
            "Received transfer request: {} ({} bytes)",
            request.file_name, request.file_size
        );

        let file_id = request.file_id.clone();
        let progress_id = format!("recv:{}", file_id);
        let total_chunks = ((request.file_size + request.chunk_size as u64 - 1)
            / request.chunk_size as u64) as u32;

        if *self.require_confirmation.read().await {
            self.progress_tracker
                .register_pending_confirm(progress_id.clone(), request.file_name.clone(), request.file_size)
                .await;
            let (tx, rx) = oneshot::channel();
            self.pending_confirmations.write().await.insert(progress_id.clone(), tx);
            let accepted = tokio::time::timeout(
                Duration::from_secs(60),
                rx,
            ).await.unwrap_or(Ok(false)).unwrap_or(false);
            self.progress_tracker.remove_progress(&progress_id).await;
            if !accepted {
                let _ = Self::write_msg(&mut stream, &Message::TransferResponse(TransferResponse {
                    file_id: file_id.clone(),
                    accepted: false,
                    save_path: None,
                    resume_from_chunk: None,
                })).await;
                return Ok(());
            }
        }

        let download_dir = self.download_dir.read().await.clone();
        let mut receiver = FileReceiver::new(
            request.clone(),
            download_dir.clone(),
            self.data_dir.clone(),
        )
        .await?;

        let resume_from_chunk = receiver.resume_from_chunk();

        self.progress_tracker
            .start_transfer(
                progress_id.clone(),
                request.file_name.clone(),
                request.file_size,
                total_chunks,
                "receive".to_string(),
            )
            .await;

        let response = TransferResponse {
            file_id: file_id.clone(),
            accepted: true,
            save_path: Some(download_dir.to_string_lossy().to_string()),
            resume_from_chunk: if resume_from_chunk > 0 { Some(resume_from_chunk) } else { None },
        };

        Self::write_msg(&mut stream, &Message::TransferResponse(response)).await?;

        for _ in 0..(total_chunks - resume_from_chunk) {
            let (chunk_index, compressed, data) = match Self::read_chunk_raw(&mut stream).await {
                Ok(r) => r,
                Err(e) => {
                    receiver.cleanup().await;
                    self.progress_tracker.remove_progress(&progress_id).await;
                    return Err(e);
                }
            };

            let write_data: Vec<u8> = if compressed {
                match crate::compression::Compressor::decompress(&data) {
                    Ok(d) => {
                        debug!(
                            "Decompressed chunk {} from {} to {} bytes",
                            chunk_index,
                            data.len(),
                            d.len()
                        );
                        d
                    }
                    Err(e) => {
                        let _ = Self::write_msg(&mut stream, &Message::TransferError(TransferError {
                            file_id: file_id.clone(),
                            error: e.to_string(),
                        })).await;
                        receiver.cleanup().await;
                        self.progress_tracker.remove_progress(&progress_id).await;
                        return Err(e.into());
                    }
                }
            } else {
                data
            };

            let chunk_bytes_len = write_data.len() as u64;
            if let Err(e) = receiver.write_chunk_raw(chunk_index, &write_data).await {
                let _ = Self::write_msg(&mut stream, &Message::TransferError(TransferError {
                    file_id: file_id.clone(),
                    error: e.to_string(),
                })).await;
                receiver.cleanup().await;
                self.progress_tracker.remove_progress(&progress_id).await;
                return Err(e);
            }

            self.progress_tracker.update_progress(&progress_id, chunk_bytes_len).await;
        }

        let tc = match Self::read_msg(&mut stream).await {
            Ok(Message::TransferComplete(tc)) => tc,
            Ok(_) => {
                warn!("Expected TransferComplete but got unexpected message");
                receiver.cleanup().await;
                self.progress_tracker.remove_progress(&progress_id).await;
                return Ok(());
            }
            Err(e) => {
                receiver.cleanup().await;
                self.progress_tracker.remove_progress(&progress_id).await;
                return Err(e);
            }
        };

        match receiver.finalize(tc.file_hash).await {
            Ok(()) => {
                info!("Transfer completed for file: {}", file_id);
                Self::write_msg(&mut stream, &Message::TransferComplete(TransferComplete {
                    file_id: file_id.clone(),
                    file_hash: tc.file_hash,
                })).await?;
            }
            Err(e) => {
                let _ = Self::write_msg(&mut stream, &Message::TransferError(TransferError {
                    file_id: file_id.clone(),
                    error: e.to_string(),
                })).await;
                receiver.cleanup().await;
                self.progress_tracker.remove_progress(&progress_id).await;
                return Err(e);
            }
        }

        self.progress_tracker.remove_progress(&progress_id).await;

        Ok(())
    }

    async fn handle_text_message(&self, mut stream: TcpStream, msg: TextMessage) -> Result<()> {
        let chat_msg = ChatMessage {
            id: msg.id.clone(),
            from_instance_id: msg.from_instance_id.clone(),
            from_instance_name: msg.from_instance_name.clone(),
            content: msg.content.clone(),
            timestamp: msg.timestamp,
            is_self: false,
        };
        self.message_store.save_message(&msg.from_instance_id, chat_msg).await?;
        Self::write_msg(&mut stream, &Message::TextAck(TextAck {
            message_id: msg.id,
        })).await?;
        Ok(())
    }

    async fn process_queue_loop(&self) {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(100));
        loop {
            interval.tick().await;

            if !self.task_queue.can_start_new().await {
                continue;
            }

            if let Some(task) = self.task_queue.get_next_pending().await {
                let service = Arc::new(self.clone());
                tokio::spawn(async move {
                    if let Err(e) = service.process_task(task).await {
                        error!("Failed to process task: {}", e);
                    }
                });
            }
        }
    }

    async fn process_task(&self, task: TransferTask) -> Result<()> {
        info!("Processing task: {} -> {}", task.file_name, task.target_device);
        self.task_queue.complete_task(&task.task_id).await;
        Ok(())
    }

    pub async fn send_file(&self, file_path: PathBuf, target_addr: SocketAddr) -> Result<String> {
        let enable_compression = *self.enable_compression.read().await;
        self.do_send_file(file_path, None, target_addr, enable_compression).await
    }

    pub async fn send_file_with_relative_path(
        &self,
        file_path: PathBuf,
        relative_path: Option<String>,
        target_addr: SocketAddr,
    ) -> Result<String> {
        let enable_compression = *self.enable_compression.read().await;
        self.do_send_file(file_path, relative_path, target_addr, enable_compression).await
    }

    pub async fn send_file_compressed(
        &self,
        file_path: PathBuf,
        target_addr: SocketAddr,
        enable_compression: bool,
    ) -> Result<String> {
        self.do_send_file(file_path, None, target_addr, enable_compression).await
    }

    async fn do_send_file(
        &self,
        file_path: PathBuf,
        relative_path: Option<String>,
        target_addr: SocketAddr,
        enable_compression: bool,
    ) -> Result<String> {
        let chunk_size = *self.chunk_size.read().await;
        let speed_limit_bytes_per_sec = *self.speed_limit_bytes_per_sec.read().await;

        let metadata = tokio::fs::metadata(&file_path).await?;
        let file_size = metadata.len();

        let file_name = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        let file_id = {
            let mut h = Sha256::new();
            h.update(file_name.as_bytes());
            h.update(&file_size.to_le_bytes());
            let r = h.finalize();
            r[..16].iter().map(|b| format!("{:02x}", b)).collect::<String>()
        };

        let total_chunks = ((file_size + chunk_size as u64 - 1) / chunk_size as u64) as u32;

        self.progress_tracker
            .register_queued(
                file_id.clone(),
                file_name.clone(),
                file_size,
                total_chunks,
                "send".to_string(),
            )
            .await;

        let _permit = {
            let sem = self.semaphore.read().await.clone();
            match sem.acquire_owned().await {
                Ok(p) => p,
                Err(_) => {
                    self.progress_tracker.remove_progress(&file_id).await;
                    return Err(anyhow::anyhow!("Transfer semaphore closed"));
                }
            }
        };

        let request = TransferRequest {
            file_id: file_id.clone(),
            file_name: file_name.clone(),
            relative_path,
            file_size,
            chunk_size,
            device_id: String::new(),
            password_hash: None,
        };

        let mut stream = match TcpStream::connect(target_addr).await {
            Ok(s) => s,
            Err(e) => {
                self.progress_tracker.remove_progress(&file_id).await;
                return Err(e.into());
            }
        };
        stream.set_nodelay(true)?;

        if let Err(e) = Self::write_msg(&mut stream, &Message::TransferRequest(request)).await {
            self.progress_tracker.remove_progress(&file_id).await;
            return Err(e);
        }

        let resume_from = {
            let response = match Self::read_msg(&mut stream).await {
                Ok(r) => r,
                Err(e) => {
                    self.progress_tracker.remove_progress(&file_id).await;
                    return Err(e);
                }
            };
            if let Message::TransferResponse(resp) = response {
                if !resp.accepted {
                    self.progress_tracker.remove_progress(&file_id).await;
                    return Err(anyhow::anyhow!("Transfer rejected by receiver"));
                }
                resp.resume_from_chunk.unwrap_or(0)
            } else {
                0
            }
        };

        self.progress_tracker.set_active(&file_id).await;

        let mut file = tokio::fs::File::open(&file_path).await?;
        let mut hasher = Sha256::new();

        if resume_from > 0 {
            let skip_bytes = resume_from as u64 * chunk_size as u64;
            let mut remaining_skip = skip_bytes;
            let mut skip_buf = vec![0u8; 4 * 1024 * 1024];
            while remaining_skip > 0 {
                let read_size = (skip_buf.len() as u64).min(remaining_skip) as usize;
                file.read_exact(&mut skip_buf[..read_size]).await?;
                hasher.update(&skip_buf[..read_size]);
                remaining_skip -= read_size as u64;
            }
        }

        for chunk_index in resume_from..total_chunks {
            if self.cancelled.read().await.contains(&file_id) {
                self.progress_tracker.remove_progress(&file_id).await;
                self.cancelled.write().await.remove(&file_id);
                info!("Transfer cancelled: {}", file_id);
                return Ok(file_id);
            }

            loop {
                if !self.paused.read().await.contains(&file_id) { break; }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }

            let offset = chunk_index as u64 * chunk_size as u64;
            let remaining = file_size - offset;
            let read_size = (chunk_size as u64).min(remaining) as usize;
            let mut buffer = vec![0u8; read_size];
            file.read_exact(&mut buffer).await?;

            hasher.update(&buffer);

            let (send_data, compressed): (Vec<u8>, bool) = if enable_compression && buffer.len() > 1024 {
                match crate::compression::Compressor::compress(&buffer) {
                    Ok(c) if c.len() < buffer.len() => {
                        debug!(
                            "Compressed chunk {} from {} to {} bytes ({:.1}%)",
                            chunk_index,
                            buffer.len(),
                            c.len(),
                            (c.len() as f64 / buffer.len() as f64) * 100.0
                        );
                        (c, true)
                    }
                    _ => {
                        debug!("Chunk {} not compressed (no benefit)", chunk_index);
                        (buffer, false)
                    }
                }
            } else {
                (buffer, false)
            };

            let chunk_bytes_len = send_data.len() as u64;

            Self::write_chunk_raw(&mut stream, chunk_index, compressed, &send_data).await?;

            self.progress_tracker
                .update_progress(&file_id, chunk_bytes_len)
                .await;

            debug!("Sent chunk {}/{}", chunk_index + 1, total_chunks);

            if speed_limit_bytes_per_sec > 0 {
                let delay_micros = (chunk_bytes_len * 1_000_000) / speed_limit_bytes_per_sec;
                tokio::time::sleep(tokio::time::Duration::from_micros(delay_micros)).await;
            }
        }

        let hash_result = hasher.finalize();
        let mut file_hash = [0u8; 32];
        file_hash.copy_from_slice(&hash_result);

        Self::write_msg(&mut stream, &Message::TransferComplete(TransferComplete {
            file_id: file_id.clone(),
            file_hash,
        })).await?;

        match Self::read_msg(&mut stream).await? {
            Message::TransferComplete(_) => {}
            Message::TransferError(e) => {
                self.progress_tracker.remove_progress(&file_id).await;
                return Err(anyhow::anyhow!("Receiver error: {}", e.error));
            }
            _ => {}
        }

        self.progress_tracker.remove_progress(&file_id).await;
        info!("File transfer completed: {}", file_id);

        Ok(file_id)
    }

    pub async fn cancel_transfer(&self, file_id: &str) {
        self.cancelled.write().await.insert(file_id.to_string());
    }

    pub async fn pause_transfer(&self, file_id: &str) {
        self.paused.write().await.insert(file_id.to_string());
        self.progress_tracker.set_paused(file_id, true).await;
    }

    pub async fn resume_transfer(&self, file_id: &str) {
        self.paused.write().await.remove(file_id);
        self.progress_tracker.set_paused(file_id, false).await;
    }

    pub async fn confirm_transfer(&self, file_id: &str) {
        if let Some(tx) = self.pending_confirmations.write().await.remove(file_id) {
            let _ = tx.send(true);
        }
    }

    pub async fn reject_transfer(&self, file_id: &str) {
        if let Some(tx) = self.pending_confirmations.write().await.remove(file_id) {
            let _ = tx.send(false);
        }
    }

    pub async fn send_text_message(
        &self,
        peer_instance_id: &str,
        target_addr: SocketAddr,
        content: String,
        from_instance_id: String,
        from_instance_name: String,
    ) -> Result<()> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let id = Uuid::new_v4().to_string();

        let chat_msg = ChatMessage {
            id: id.clone(),
            from_instance_id: from_instance_id.clone(),
            from_instance_name: from_instance_name.clone(),
            content: content.clone(),
            timestamp,
            is_self: true,
        };
        self.message_store.save_message(peer_instance_id, chat_msg).await?;

        let msg = TextMessage {
            id,
            from_instance_id,
            from_instance_name,
            content,
            timestamp,
        };

        let mut stream = TcpStream::connect(target_addr).await?;
        stream.set_nodelay(true)?;
        Self::write_msg(&mut stream, &Message::TextMessage(msg)).await?;

        match Self::read_msg(&mut stream).await? {
            Message::TextAck(_) => {}
            _ => {}
        }

        Ok(())
    }

    pub async fn update_transfer_config(
        &self,
        download_dir: PathBuf,
        chunk_size: u32,
        enable_compression: bool,
        speed_limit_bytes_per_sec: u64,
        require_confirmation: bool,
    ) {
        *self.download_dir.write().await = download_dir;
        *self.chunk_size.write().await = chunk_size;
        *self.enable_compression.write().await = enable_compression;
        *self.speed_limit_bytes_per_sec.write().await = speed_limit_bytes_per_sec;
        *self.require_confirmation.write().await = require_confirmation;
    }

    pub async fn update_max_concurrent(&self, n: usize) {
        self.task_queue.update_max_concurrent(n).await;
        *self.semaphore.write().await = Arc::new(Semaphore::new(n));
    }

    pub async fn pause_all(&self) {
        let all = self.progress_tracker.list_all().await;
        for p in all {
            if p.direction == "send" && p.status == "active" && !p.paused {
                self.pause_transfer(&p.file_id).await;
            }
        }
    }

    pub async fn resume_all(&self) {
        let all = self.progress_tracker.list_all().await;
        for p in all {
            if p.paused {
                self.resume_transfer(&p.file_id).await;
            }
        }
    }

    pub fn progress_tracker(&self) -> Arc<ProgressTracker> {
        self.progress_tracker.clone()
    }

    pub fn local_port(&self) -> u16 {
        self.transfer_port
    }

    pub fn task_queue(&self) -> Arc<TaskQueue> {
        self.task_queue.clone()
    }

    pub fn message_store(&self) -> Arc<MessageStore> {
        self.message_store.clone()
    }
}

impl Clone for TransferService {
    fn clone(&self) -> Self {
        Self {
            listener: self.listener.clone(),
            transfer_port: self.transfer_port,
            task_queue: self.task_queue.clone(),
            progress_tracker: self.progress_tracker.clone(),
            cancelled: self.cancelled.clone(),
            paused: self.paused.clone(),
            pending_confirmations: self.pending_confirmations.clone(),
            require_confirmation: self.require_confirmation.clone(),
            data_dir: self.data_dir.clone(),
            download_dir: self.download_dir.clone(),
            chunk_size: self.chunk_size.clone(),
            enable_compression: self.enable_compression.clone(),
            speed_limit_bytes_per_sec: self.speed_limit_bytes_per_sec.clone(),
            message_store: self.message_store.clone(),
            semaphore: self.semaphore.clone(),
        }
    }
}
