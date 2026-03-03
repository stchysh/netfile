use super::file_transfer::FileReceiver;
use super::file_utils::calculate_chunk_checksum;
use sha2::{Digest, Sha256};
use super::task_queue::{TaskQueue, TransferTask};
use super::progress::ProgressTracker;
use crate::protocol::{ChunkData, Message, TransferComplete, TransferError, TransferRequest, TransferResponse};
use anyhow::Result;
use bytes::Bytes;
use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

pub struct TransferService {
    listener: Arc<TcpListener>,
    transfer_port: u16,
    task_queue: Arc<TaskQueue>,
    progress_tracker: Arc<ProgressTracker>,
    cancelled: Arc<RwLock<HashSet<String>>>,
    data_dir: PathBuf,
    download_dir: PathBuf,
    chunk_size: u32,
    enable_compression: bool,
    speed_limit_bytes_per_sec: u64,
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

        Ok(Self {
            listener: Arc::new(listener),
            transfer_port: port,
            task_queue: Arc::new(TaskQueue::new(max_concurrent)),
            progress_tracker: Arc::new(ProgressTracker::new()),
            cancelled: Arc::new(RwLock::new(HashSet::new())),
            data_dir,
            download_dir,
            chunk_size,
            enable_compression,
            speed_limit_bytes_per_sec,
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

    async fn handle_connection(&self, mut stream: TcpStream, addr: SocketAddr) -> Result<()> {
        stream.set_nodelay(true)?;
        info!("New connection from {}", addr);

        let message = Self::read_msg(&mut stream).await?;

        match message {
            Message::TransferRequest(request) => {
                self.handle_transfer_request(stream, request).await?;
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

        let mut receiver = FileReceiver::new(
            request.clone(),
            self.download_dir.clone(),
            self.data_dir.clone(),
        )
        .await?;

        let file_id = receiver.file_id().to_string();
        let total_chunks = ((request.file_size + request.chunk_size as u64 - 1)
            / request.chunk_size as u64) as u32;

        self.progress_tracker
            .start_transfer(
                file_id.clone(),
                request.file_name.clone(),
                request.file_size,
                total_chunks,
                "receive".to_string(),
            )
            .await;

        let response = TransferResponse {
            file_id: file_id.clone(),
            accepted: true,
            save_path: Some(self.download_dir.to_string_lossy().to_string()),
        };

        Self::write_msg(&mut stream, &Message::TransferResponse(response)).await?;

        loop {
            let message = match Self::read_msg(&mut stream).await {
                Ok(m) => m,
                Err(_) => break,
            };

            match message {
                Message::ChunkData(mut chunk) => {
                    let chunk_index = chunk.chunk_index;

                    if chunk.compressed {
                        match crate::compression::Compressor::decompress(chunk.data.as_ref()) {
                            Ok(decompressed) => {
                                debug!(
                                    "Decompressed chunk {} from {} to {} bytes",
                                    chunk_index,
                                    chunk.data.len(),
                                    decompressed.len()
                                );
                                chunk.data = Bytes::from(decompressed);
                                chunk.compressed = false;
                            }
                            Err(e) => {
                                warn!("Failed to decompress chunk {}: {}", chunk_index, e);
                                break;
                            }
                        }
                    }

                    let chunk_size = chunk.data.len() as u64;
                    match receiver.write_chunk(chunk).await {
                        Ok(()) => {}
                        Err(e) => {
                            let _ = Self::write_msg(&mut stream, &Message::TransferError(TransferError {
                                file_id: file_id.clone(),
                                error: e.to_string(),
                            })).await;
                            return Err(e);
                        }
                    }

                    self.progress_tracker.update_progress(&file_id, chunk_size).await;
                }
                Message::TransferComplete(tc) => {
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
                            return Err(e);
                        }
                    }
                    break;
                }
                _ => {
                    warn!("Unexpected message during chunk transfer");
                    break;
                }
            }
        }

        self.progress_tracker.remove_progress(&file_id).await;

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
        self.do_send_file(file_path, None, target_addr, self.enable_compression).await
    }

    pub async fn send_file_with_relative_path(
        &self,
        file_path: PathBuf,
        relative_path: Option<String>,
        target_addr: SocketAddr,
    ) -> Result<String> {
        self.do_send_file(file_path, relative_path, target_addr, self.enable_compression).await
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
        let file_id = Uuid::new_v4().to_string();

        let metadata = tokio::fs::metadata(&file_path).await?;
        let file_size = metadata.len();

        let file_name = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        let request = TransferRequest {
            file_id: file_id.clone(),
            file_name: file_name.clone(),
            relative_path,
            file_size,
            chunk_size: self.chunk_size,
            device_id: String::new(),
            password_hash: None,
        };

        let mut stream = TcpStream::connect(target_addr).await?;
        stream.set_nodelay(true)?;

        Self::write_msg(&mut stream, &Message::TransferRequest(request)).await?;

        let response = Self::read_msg(&mut stream).await?;
        if let Message::TransferResponse(resp) = response {
            if !resp.accepted {
                return Err(anyhow::anyhow!("Transfer rejected by receiver"));
            }
        }

        let total_chunks = ((file_size + self.chunk_size as u64 - 1) / self.chunk_size as u64) as u32;

        self.progress_tracker
            .start_transfer(
                file_id.clone(),
                file_name,
                file_size,
                total_chunks,
                "send".to_string(),
            )
            .await;

        let mut file = tokio::fs::File::open(&file_path).await?;
        let mut hasher = Sha256::new();

        for chunk_index in 0..total_chunks {
            if self.cancelled.read().await.contains(&file_id) {
                self.progress_tracker.remove_progress(&file_id).await;
                self.cancelled.write().await.remove(&file_id);
                info!("Transfer cancelled: {}", file_id);
                return Ok(file_id);
            }

            let offset = chunk_index as u64 * self.chunk_size as u64;
            let remaining = file_size - offset;
            let read_size = (self.chunk_size as u64).min(remaining) as usize;
            let mut buffer = vec![0u8; read_size];
            file.read_exact(&mut buffer).await?;

            hasher.update(&buffer);
            let checksum = calculate_chunk_checksum(&buffer);
            let mut chunk = ChunkData {
                file_id: file_id.clone(),
                chunk_index,
                data: Bytes::from(buffer),
                checksum,
                compressed: false,
            };

            if enable_compression && chunk.data.len() > 1024 {
                match crate::compression::Compressor::compress(chunk.data.as_ref()) {
                    Ok(compressed) if compressed.len() < chunk.data.len() => {
                        debug!(
                            "Compressed chunk {} from {} to {} bytes ({:.1}%)",
                            chunk_index,
                            chunk.data.len(),
                            compressed.len(),
                            (compressed.len() as f64 / chunk.data.len() as f64) * 100.0
                        );
                        chunk.data = Bytes::from(compressed);
                        chunk.compressed = true;
                    }
                    _ => {
                        debug!("Chunk {} not compressed (no benefit)", chunk_index);
                    }
                }
            }

            let chunk_bytes_len = chunk.data.len() as u64;

            Self::write_msg(&mut stream, &Message::ChunkData(chunk)).await?;

            self.progress_tracker
                .update_progress(&file_id, chunk_bytes_len)
                .await;

            debug!("Sent chunk {}/{}", chunk_index + 1, total_chunks);

            if self.speed_limit_bytes_per_sec > 0 {
                let delay_micros = (chunk_bytes_len * 1_000_000) / self.speed_limit_bytes_per_sec;
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

    pub fn progress_tracker(&self) -> Arc<ProgressTracker> {
        self.progress_tracker.clone()
    }

    pub fn local_port(&self) -> u16 {
        self.transfer_port
    }

    pub fn task_queue(&self) -> Arc<TaskQueue> {
        self.task_queue.clone()
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
            data_dir: self.data_dir.clone(),
            download_dir: self.download_dir.clone(),
            chunk_size: self.chunk_size,
            enable_compression: self.enable_compression,
            speed_limit_bytes_per_sec: self.speed_limit_bytes_per_sec,
        }
    }
}
