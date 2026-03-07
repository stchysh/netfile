use super::file_transfer::FileReceiver;
use super::history::{HistoryStore, TransferRecord};
use sha2::{Digest, Sha256};
use super::progress::ProgressTracker;
use crate::protocol::{DataStreamHeader, Message, TextAck, TextMessage, TransferComplete, TransferError, TransferRequest, TransferResponse};
use crate::message_store::{ChatMessage, MessageStore};
use crate::iroh_net::IrohManager;
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, RwLock, Semaphore};
use tokio::task::JoinSet;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

pub struct TransferService {
    tcp_listener: Arc<TcpListener>,
    iroh_manager: Arc<IrohManager>,
    transfer_port: u16,
    iroh_conn_cache: Arc<RwLock<HashMap<iroh::EndpointId, iroh::endpoint::Connection>>>,
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
    iroh_stream_count: Arc<RwLock<u32>>,
    message_store: Arc<MessageStore>,
    history_store: Arc<HistoryStore>,
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

        let tcp_listener = TcpListener::bind(format!("0.0.0.0:{}", port)).await?;
        let iroh_manager = IrohManager::new(data_dir.clone()).await?;

        info!("Transfer service listening on port {} (TCP + iroh)", port);

        let message_store = Arc::new(MessageStore::new(data_dir.clone()));
        let history_store = Arc::new(HistoryStore::new(data_dir.clone()));
        let semaphore = Arc::new(RwLock::new(Arc::new(Semaphore::new(max_concurrent))));

        Ok(Self {
            tcp_listener: Arc::new(tcp_listener),
            iroh_manager,
            transfer_port: port,
            iroh_conn_cache: Arc::new(RwLock::new(HashMap::new())),
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
            iroh_stream_count: Arc::new(RwLock::new(4)),
            message_store,
            history_store,
            semaphore,
        })
    }

    async fn find_available_port() -> Result<u16> {
        for port in 37050..37100 {
            match tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port)).await {
                Ok(_) => return Ok(port),
                Err(_) => continue,
            }
        }
        Err(anyhow::anyhow!("No available port found"))
    }

    pub async fn start(self: Arc<Self>) {
        let accept_iroh_task = {
            let service = self.clone();
            tokio::spawn(async move {
                service.accept_iroh_loop().await;
            })
        };

        let accept_tcp_task = {
            let service = self.clone();
            tokio::spawn(async move {
                service.accept_tcp_loop().await;
            })
        };

        let _ = tokio::join!(accept_iroh_task, accept_tcp_task);
    }

    async fn accept_iroh_loop(&self) {
        while let Some(incoming) = self.iroh_manager.accept().await {
            let service = Arc::new(self.clone());
            tokio::spawn(async move {
                let conn = match incoming.accept() {
                    Ok(connecting) => match connecting.await {
                        Ok(c) => c,
                        Err(e) => { error!("iroh connection failed: {}", e); return; }
                    },
                    Err(e) => { error!("iroh accept error: {}", e); return; }
                };
                // Per-connection routing table for multi-stream data streams:
                // file_id -> Vec of senders (one per data stream)
                let data_routes: Arc<RwLock<HashMap<String, Vec<mpsc::Sender<(u32, bool, Vec<u8>)>>>>> =
                    Arc::new(RwLock::new(HashMap::new()));
                loop {
                    let (send, recv) = match conn.accept_bi().await {
                        Ok(s) => s,
                        Err(e) => { debug!("iroh connection closed: {}", e); break; }
                    };
                    let svc = service.clone();
                    let routes = data_routes.clone();
                    tokio::spawn(async move {
                        svc.handle_iroh_stream(send, recv, routes).await;
                    });
                }
            });
        }
    }

    async fn handle_iroh_stream<S, R>(
        &self,
        mut send: S,
        mut recv: R,
        data_routes: Arc<RwLock<HashMap<String, Vec<mpsc::Sender<(u32, bool, Vec<u8>)>>>>>,
    ) where
        S: AsyncWrite + Unpin + Send + 'static,
        R: AsyncRead + Unpin + Send + 'static,
    {
        let msg = match Self::read_msg(&mut recv).await {
            Ok(m) => m,
            Err(e) => { debug!("[recv/iroh] read first msg error: {}", e); return; }
        };
        match msg {
            Message::TransferRequest(req) => {
                let stream_count = req.stream_count.unwrap_or(1);
                if stream_count > 1 {
                    if let Err(e) = self.handle_iroh_multi_control(&mut send, &mut recv, req, data_routes).await {
                        warn!("[recv/iroh/multi] control error: {}", e);
                    }
                } else if let Err(e) = self.handle_transfer_request(&mut send, &mut recv, req, "iroh").await {
                    debug!("[recv/iroh] transfer error: {}", e);
                }
            }
            Message::DataStreamHeader(h) => {
                let tx_opt = {
                    let routes = data_routes.read().await;
                    routes.get(&h.file_id).and_then(|v| v.get(h.stream_index as usize).cloned())
                };
                if let Some(tx) = tx_opt {
                    loop {
                        match Self::read_chunk_raw(&mut recv).await {
                            Ok((chunk_index, compressed, data)) => {
                                if tx.send((chunk_index, compressed, data)).await.is_err() { break; }
                            }
                            Err(_) => break,
                        }
                    }
                } else {
                    warn!("[recv/iroh/multi] no route for file_id={} stream={}", h.file_id, h.stream_index);
                }
            }
            Message::TextMessage(msg) => {
                let _ = self.handle_text_message(&mut send, msg).await;
            }
            _ => { warn!("[recv/iroh] unexpected first message on stream"); }
        }
    }

    async fn handle_iroh_multi_control<S, R>(
        &self,
        send: &mut S,
        recv: &mut R,
        request: TransferRequest,
        data_routes: Arc<RwLock<HashMap<String, Vec<mpsc::Sender<(u32, bool, Vec<u8>)>>>>>,
    ) -> Result<()>
    where
        S: AsyncWrite + Unpin,
        R: AsyncRead + Unpin,
    {
        let file_id = request.file_id.clone();
        let file_size = request.file_size;
        let chunk_size = request.chunk_size;
        let stream_count = request.stream_count.unwrap_or(1);
        let total_chunks = ((file_size + chunk_size as u64 - 1) / chunk_size as u64) as u32;
        let progress_id = format!("recv:{}", file_id);

        info!(
            "[recv/iroh/multi] request: file_id={} name={:?} size={} streams={}",
            file_id, request.file_name, file_size, stream_count
        );

        if *self.require_confirmation.read().await {
            self.progress_tracker
                .register_pending_confirm(progress_id.clone(), request.file_name.clone(), file_size)
                .await;
            self.progress_tracker.set_transfer_method(&progress_id, "iroh").await;
            let (tx, rx) = oneshot::channel();
            self.pending_confirmations.write().await.insert(progress_id.clone(), tx);
            let accepted = rx.await.unwrap_or(false);
            self.progress_tracker.remove_progress(&progress_id).await;
            if !accepted {
                info!("[recv/iroh/multi] user rejected: {}", file_id);
                let _ = Self::write_msg(send, &Message::TransferResponse(TransferResponse {
                    file_id: file_id.clone(),
                    accepted: false,
                    save_path: None,
                    resume_from_chunk: None,
                })).await;
                return Ok(());
            }
            info!("[recv/iroh/multi] user accepted: {}", file_id);
        }

        let download_dir = self.download_dir.read().await.clone();
        let temp_file_path = self.data_dir.join("temp").join(format!("{}.tmp", file_id));
        let final_path = if let Some(rel) = &request.relative_path {
            download_dir.join(rel)
        } else {
            download_dir.join(&request.file_name)
        };

        if let Some(p) = temp_file_path.parent() { tokio::fs::create_dir_all(p).await?; }
        if let Some(p) = final_path.parent() { tokio::fs::create_dir_all(p).await?; }

        {
            let f = tokio::fs::File::create(&temp_file_path).await?;
            f.set_len(file_size).await?;
        }

        // Setup channels and routing table before sending response
        let mut channel_senders = Vec::with_capacity(stream_count as usize);
        let mut channel_receivers = Vec::with_capacity(stream_count as usize);
        for _ in 0..stream_count {
            let (tx, rx) = mpsc::channel::<(u32, bool, Vec<u8>)>(16);
            channel_senders.push(tx);
            channel_receivers.push(rx);
        }
        data_routes.write().await.insert(file_id.clone(), channel_senders);

        self.progress_tracker
            .start_transfer(progress_id.clone(), request.file_name.clone(), file_size, total_chunks, "receive".to_string())
            .await;
        self.progress_tracker.set_transfer_method(&progress_id, "iroh").await;
        self.progress_tracker.set_active(&progress_id).await;

        if let Err(e) = Self::write_msg(send, &Message::TransferResponse(TransferResponse {
            file_id: file_id.clone(),
            accepted: true,
            save_path: Some(download_dir.to_string_lossy().to_string()),
            resume_from_chunk: None,
        })).await {
            data_routes.write().await.remove(&file_id);
            self.progress_tracker.remove_progress(&progress_id).await;
            let _ = tokio::fs::remove_file(&temp_file_path).await;
            return Err(e);
        }

        // Spawn writer tasks — each seeks once to the first chunk's offset, then writes sequentially
        let t_write_start = Instant::now();
        let mut writer_set: JoinSet<Result<(u64, u64, u64, u64)>> = JoinSet::new();
        for (stream_idx, rx) in channel_receivers.into_iter().enumerate() {
            let temp_path = temp_file_path.clone();
            let chunk_size_u64 = chunk_size as u64;
            let tracker = self.progress_tracker.clone();
            let pid = progress_id.clone();
            let fid = file_id.clone();
            writer_set.spawn(async move {
                let t_task = Instant::now();
                let mut file = tokio::fs::OpenOptions::new().write(true).open(&temp_path).await?;
                let mut rx = rx;
                let mut first = true;
                let mut total_decompress_ms = 0u64;
                let mut total_write_ms = 0u64;
                let mut total_bytes_written = 0u64;
                while let Some((chunk_index, compressed, data)) = rx.recv().await {
                    let t_decomp = Instant::now();
                    let write_data = if compressed {
                        crate::compression::Compressor::decompress(&data)?
                    } else {
                        data
                    };
                    total_decompress_ms += t_decomp.elapsed().as_millis() as u64;

                    // Seek only on the first chunk of this stream's range
                    let t_write = Instant::now();
                    if first {
                        let offset = chunk_index as u64 * chunk_size_u64;
                        file.seek(std::io::SeekFrom::Start(offset)).await?;
                        first = false;
                    }
                    let wlen = write_data.len() as u64;
                    file.write_all(&write_data).await?;
                    total_write_ms += t_write.elapsed().as_millis() as u64;
                    total_bytes_written += wlen;

                    tracker.update_progress(&pid, wlen).await;
                }
                let task_elapsed_ms = t_task.elapsed().as_millis() as u64;
                let mbps = if task_elapsed_ms > 0 {
                    (total_bytes_written as f64 / 1_048_576.0) / (task_elapsed_ms as f64 / 1000.0)
                } else { 0.0 };
                info!(
                    "[recv/iroh/multi] stream={} bytes={} elapsed_ms={} decompress_ms={} write_ms={} throughput={:.2}MB/s",
                    stream_idx, total_bytes_written, task_elapsed_ms, total_decompress_ms, total_write_ms, mbps
                );
                debug!("[recv/iroh/multi] writer task done for {}", fid);
                Ok((total_decompress_ms, total_write_ms, total_bytes_written, task_elapsed_ms))
            });
        }

        while let Some(result) = writer_set.join_next().await {
            match result {
                Err(e) => error!("[recv/iroh/multi] writer task panicked: {:?}", e),
                Ok(Err(e)) => error!("[recv/iroh/multi] writer error: {}", e),
                Ok(Ok(_)) => {}
            }
        }
        info!("[recv/iroh/multi] all writes done for {} in {}ms", file_id, t_write_start.elapsed().as_millis());

        data_routes.write().await.remove(&file_id);

        // Read TransferComplete from control stream
        let tc = match Self::read_msg(recv).await {
            Ok(Message::TransferComplete(tc)) => tc,
            Ok(_) => {
                self.progress_tracker.remove_progress(&progress_id).await;
                let _ = tokio::fs::remove_file(&temp_file_path).await;
                return Err(anyhow::anyhow!("expected TransferComplete"));
            }
            Err(e) => {
                self.progress_tracker.remove_progress(&progress_id).await;
                let _ = tokio::fs::remove_file(&temp_file_path).await;
                return Err(e);
            }
        };

        if tokio::fs::rename(&temp_file_path, &final_path).await.is_err() {
            tokio::fs::copy(&temp_file_path, &final_path).await?;
            tokio::fs::remove_file(&temp_file_path).await?;
        }

        // Send ACK immediately — do not block on hash verification
        if let Err(e) = Self::write_msg(send, &Message::TransferComplete(TransferComplete {
            file_id: file_id.clone(),
            file_hash: tc.file_hash,
        })).await {
            self.progress_tracker.remove_progress(&progress_id).await;
            return Err(e);
        }

        // Background hash verification — does not block the sender
        let verify_path = final_path.clone();
        let expected_hash = tc.file_hash;
        let fid_bg = file_id.clone();
        tokio::spawn(async move {
            let t_hash = Instant::now();
            let mut hasher = Sha256::new();
            match tokio::fs::File::open(&verify_path).await {
                Ok(mut f) => {
                    let mut buf = vec![0u8; 4 * 1024 * 1024];
                    loop {
                        match f.read(&mut buf).await {
                            Ok(0) => break,
                            Ok(n) => hasher.update(&buf[..n]),
                            Err(e) => { warn!("[recv/iroh/multi] hash verify read error for {}: {}", fid_bg, e); return; }
                        }
                    }
                    let r = hasher.finalize();
                    let mut actual = [0u8; 32];
                    actual.copy_from_slice(&r);
                    let hash_ms = t_hash.elapsed().as_millis();
                    if actual == expected_hash {
                        info!("[recv/iroh/multi] bg hash OK for {} hash_ms={}", fid_bg, hash_ms);
                    } else {
                        error!("[recv/iroh/multi] bg hash MISMATCH for {} hash_ms={}", fid_bg, hash_ms);
                    }
                }
                Err(e) => warn!("[recv/iroh/multi] bg hash open error for {}: {}", fid_bg, e),
            }
        });

        let (elapsed, method) = self.progress_tracker.get_progress(&progress_id).await
            .map(|p| (p.start_time.elapsed().as_secs(), p.transfer_method.clone()))
            .unwrap_or((0, None));
        self.progress_tracker.remove_progress(&progress_id).await;

        info!("[recv/iroh/multi] completed file_id={} name={:?} size={} elapsed={}s",
            file_id, request.file_name, file_size, elapsed);

        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
        let _ = self.history_store.add_record(TransferRecord {
            id: Uuid::new_v4().to_string(),
            file_name: request.file_name.clone(),
            file_size,
            direction: "receive".to_string(),
            status: "completed".to_string(),
            error: None,
            timestamp: ts,
            elapsed_secs: elapsed,
            save_path: Some(final_path.to_string_lossy().to_string()),
            transfer_method: method,
        }).await;

        Ok(())
    }

    async fn accept_tcp_loop(&self) {
        loop {
            match self.tcp_listener.accept().await {
                Ok((stream, addr)) => {
                    let service = Arc::new(self.clone());
                    tokio::spawn(async move {
                        if let Err(e) = service.handle_tcp_connection(stream, addr).await {
                            debug!("TCP connection from {} ended: {}", addr, e);
                        }
                    });
                }
                Err(e) => {
                    error!("TCP accept error: {}", e);
                }
            }
        }
    }

    async fn handle_tcp_connection(&self, stream: TcpStream, addr: SocketAddr) -> Result<()> {
        stream.set_nodelay(true)?;
        info!("New TCP connection from {}", addr);
        let (mut read_half, mut write_half) = stream.into_split();
        self.handle_connection(&mut write_half, &mut read_half, "lan").await
    }

    async fn handle_connection<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
        &self,
        send: &mut W,
        recv: &mut R,
        transfer_method: &str,
    ) -> Result<()> {
        let message = Self::read_msg(recv).await?;
        match message {
            Message::TransferRequest(request) => {
                self.handle_transfer_request(send, recv, request, transfer_method).await?;
            }
            Message::TextMessage(msg) => {
                self.handle_text_message(send, msg).await?;
            }
            #[allow(unreachable_patterns)]
            _ => {
                warn!("Unexpected message type");
            }
        }
        Ok(())
    }

    async fn read_msg<R: AsyncRead + Unpin>(recv: &mut R) -> Result<Message> {
        let mut len_buf = [0u8; 4];
        recv.read_exact(&mut len_buf).await?;
        let len = u32::from_be_bytes(len_buf) as usize;
        if len > 64 * 1024 * 1024 {
            return Err(anyhow::anyhow!("Message too large: {} bytes", len));
        }
        let mut buf = vec![0u8; len];
        recv.read_exact(&mut buf).await?;
        Message::from_bytes(&buf)
    }

    async fn write_msg<W: AsyncWrite + Unpin>(send: &mut W, msg: &Message) -> Result<()> {
        let data = msg.to_bytes()?;
        let len = (data.len() as u32).to_be_bytes();
        send.write_all(&len).await?;
        send.write_all(&data).await?;
        send.flush().await?;
        Ok(())
    }

    async fn write_chunk_raw<W: AsyncWrite + Unpin>(
        send: &mut W,
        chunk_index: u32,
        compressed: bool,
        data: &[u8],
    ) -> Result<()> {
        let mut header = [0u8; 9];
        header[0..4].copy_from_slice(&(data.len() as u32).to_le_bytes());
        header[4..8].copy_from_slice(&chunk_index.to_le_bytes());
        header[8] = compressed as u8;
        send.write_all(&header).await?;
        send.write_all(data).await?;
        Ok(())
    }

    async fn read_chunk_raw<R: AsyncRead + Unpin>(recv: &mut R) -> Result<(u32, bool, Vec<u8>)> {
        let mut header = [0u8; 9];
        recv.read_exact(&mut header).await?;
        let data_len = u32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
        let chunk_index = u32::from_le_bytes(header[4..8].try_into().unwrap());
        let compressed = header[8] != 0;
        if data_len > 128 * 1024 * 1024 {
            return Err(anyhow::anyhow!("Chunk too large: {} bytes", data_len));
        }
        let mut data = vec![0u8; data_len];
        recv.read_exact(&mut data).await?;
        Ok((chunk_index, compressed, data))
    }

    async fn handle_transfer_request<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
        &self,
        send: &mut W,
        recv: &mut R,
        request: TransferRequest,
        transfer_method: &str,
    ) -> Result<()> {
        info!(
            "[recv/{}] received request: file_id={} name={:?} size={} chunks={}",
            transfer_method, request.file_id, request.file_name, request.file_size,
            (request.file_size + request.chunk_size as u64 - 1) / request.chunk_size as u64
        );

        let file_id = request.file_id.clone();
        let progress_id = format!("recv:{}", file_id);
        let total_chunks = ((request.file_size + request.chunk_size as u64 - 1)
            / request.chunk_size as u64) as u32;

        if *self.require_confirmation.read().await {
            self.progress_tracker
                .register_pending_confirm(progress_id.clone(), request.file_name.clone(), request.file_size)
                .await;
            self.progress_tracker.set_transfer_method(&progress_id, transfer_method).await;
            let (tx, rx) = oneshot::channel();
            self.pending_confirmations.write().await.insert(progress_id.clone(), tx);
            let accepted = rx.await.unwrap_or(false);
            self.progress_tracker.remove_progress(&progress_id).await;
            if !accepted {
                info!("[recv/{}] user rejected transfer: {}", transfer_method, file_id);
                let _ = Self::write_msg(send, &Message::TransferResponse(TransferResponse {
                    file_id: file_id.clone(),
                    accepted: false,
                    save_path: None,
                    resume_from_chunk: None,
                })).await;
                return Ok(());
            }
            info!("[recv/{}] user accepted transfer: {}", transfer_method, file_id);
        }

        let download_dir = self.download_dir.read().await.clone();
        let mut receiver = FileReceiver::new(
            request.clone(),
            download_dir.clone(),
            self.data_dir.clone(),
        )
        .await?;

        let resume_from_chunk = receiver.resume_from_chunk();
        if resume_from_chunk > 0 {
            info!("[recv/{}] resuming from chunk {} for {}", transfer_method, resume_from_chunk, file_id);
        }

        self.progress_tracker
            .start_transfer(
                progress_id.clone(),
                request.file_name.clone(),
                request.file_size,
                total_chunks,
                "receive".to_string(),
            )
            .await;
        self.progress_tracker.set_transfer_method(&progress_id, transfer_method).await;

        let response = TransferResponse {
            file_id: file_id.clone(),
            accepted: true,
            save_path: Some(download_dir.to_string_lossy().to_string()),
            resume_from_chunk: if resume_from_chunk > 0 { Some(resume_from_chunk) } else { None },
        };

        if let Err(e) = Self::write_msg(send, &Message::TransferResponse(response)).await {
            self.progress_tracker.remove_progress(&progress_id).await;
            return Err(e);
        }

        for _ in 0..(total_chunks - resume_from_chunk) {
            let (chunk_index, compressed, data) = match Self::read_chunk_raw(recv).await {
                Ok(r) => r,
                Err(e) => {
                    receiver.cleanup().await;
                    self.progress_tracker.remove_progress(&progress_id).await;
                    return Err(e);
                }
            };

            let write_data: Vec<u8> = if compressed {
                match crate::compression::Compressor::decompress(&data) {
                    Ok(d) => d,
                    Err(e) => {
                        let _ = Self::write_msg(send, &Message::TransferError(TransferError {
                            file_id: file_id.clone(),
                            error: e.to_string(),
                        })).await;
                        receiver.cleanup().await;
                        self.progress_tracker.remove_progress(&progress_id).await;
                        return Err(e);
                    }
                }
            } else {
                data
            };

            let chunk_bytes_len = write_data.len() as u64;
            if let Err(e) = receiver.write_chunk_raw(chunk_index, &write_data).await {
                let _ = Self::write_msg(send, &Message::TransferError(TransferError {
                    file_id: file_id.clone(),
                    error: e.to_string(),
                })).await;
                receiver.cleanup().await;
                self.progress_tracker.remove_progress(&progress_id).await;
                return Err(e);
            }

            self.progress_tracker.update_progress(&progress_id, chunk_bytes_len).await;
        }

        let tc = match Self::read_msg(recv).await {
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
                info!("[recv/{}] completed file_id={} name={:?} size={}", transfer_method, file_id, request.file_name, request.file_size);
                if let Err(e) = Self::write_msg(send, &Message::TransferComplete(TransferComplete {
                    file_id: file_id.clone(),
                    file_hash: tc.file_hash,
                })).await {
                    self.progress_tracker.remove_progress(&progress_id).await;
                    return Err(e);
                }
            }
            Err(e) => {
                error!("[recv/{}] finalize failed for {}: {}", transfer_method, file_id, e);
                let _ = Self::write_msg(send, &Message::TransferError(TransferError {
                    file_id: file_id.clone(),
                    error: e.to_string(),
                })).await;
                receiver.cleanup().await;
                self.progress_tracker.remove_progress(&progress_id).await;
                return Err(e);
            }
        }

        let (elapsed, method) = self.progress_tracker.get_progress(&progress_id).await
            .map(|p| (p.start_time.elapsed().as_secs(), p.transfer_method.clone()))
            .unwrap_or((0, None));
        self.progress_tracker.remove_progress(&progress_id).await;

        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let _ = self.history_store.add_record(TransferRecord {
            id: Uuid::new_v4().to_string(),
            file_name: request.file_name.clone(),
            file_size: request.file_size,
            direction: "receive".to_string(),
            status: "completed".to_string(),
            error: None,
            timestamp: ts,
            elapsed_secs: elapsed,
            save_path: Some(download_dir.join(request.relative_path.as_deref().unwrap_or(&request.file_name)).to_string_lossy().to_string()),
            transfer_method: method,
        }).await;

        Ok(())
    }

    async fn handle_text_message<W: AsyncWrite + Unpin>(&self, send: &mut W, msg: TextMessage) -> Result<()> {
        let chat_msg = ChatMessage {
            id: msg.id.clone(),
            from_instance_id: msg.from_instance_id.clone(),
            from_instance_name: msg.from_instance_name.clone(),
            content: msg.content.clone(),
            timestamp: msg.timestamp,
            local_seq: 0,
            is_self: false,
        };
        self.message_store.save_message(&msg.from_instance_id, chat_msg).await?;
        Self::write_msg(send, &Message::TextAck(TextAck {
            message_id: msg.id,
        })).await?;
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

    pub async fn send_file_with_options(
        &self,
        file_path: PathBuf,
        relative_path: Option<String>,
        target_addr: SocketAddr,
        enable_compression: bool,
    ) -> Result<String> {
        self.do_send_file(file_path, relative_path, target_addr, enable_compression).await
    }

    pub async fn send_folder(
        &self,
        folder_path: PathBuf,
        target_addr: SocketAddr,
        enable_compression: bool,
    ) -> Result<()> {
        const METHOD: &str = "lan";
        let folder_name = folder_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("folder")
            .to_string();

        let entries = super::directory::scan_directory(&folder_path).await?;
        let file_entries: Vec<_> = entries.into_iter().filter(|e| !e.is_dir).collect();

        if file_entries.is_empty() {
            return Ok(());
        }

        let chunk_size = *self.chunk_size.read().await;
        let total_size: u64 = file_entries.iter().map(|e| e.size).sum();
        let total_chunks: u32 = file_entries
            .iter()
            .map(|e| ((e.size + chunk_size as u64 - 1) / chunk_size as u64) as u32)
            .sum();

        let folder_id = Uuid::new_v4().to_string();

        self.progress_tracker
            .register_queued(
                folder_id.clone(),
                format!("{}/", folder_name),
                total_size,
                total_chunks,
                "send".to_string(),
            )
            .await;
        self.progress_tracker.set_transfer_method(&folder_id, METHOD).await;

        info!(
            "[send/lan/folder] start folder_id={} name={:?} files={} total_size={} target={}",
            folder_id, folder_name, file_entries.len(), total_size, target_addr
        );

        let _permit = {
            let sem = self.semaphore.read().await.clone();
            match sem.acquire_owned().await {
                Ok(p) => p,
                Err(_) => {
                    self.progress_tracker.remove_progress(&folder_id).await;
                    return Err(anyhow::anyhow!("Transfer semaphore closed"));
                }
            }
        };

        self.progress_tracker.set_active(&folder_id).await;

        for entry in &file_entries {
            if self.cancelled.read().await.contains(&folder_id) {
                info!("[send/lan/folder] cancelled: {}", folder_id);
                break;
            }

            let current_name = entry.relative_path.to_string_lossy().replace('\\', "/");
            self.progress_tracker
                .set_current_file(&folder_id, current_name.clone())
                .await;

            let abs_path = folder_path.join(&entry.relative_path);
            let rel = format!("{}/{}", folder_name, current_name);

            if let Err(e) = self
                .send_file_for_folder(&folder_id, abs_path, Some(rel), target_addr, enable_compression, chunk_size)
                .await
            {
                warn!("[send/lan/folder] failed to send file {} in folder {}: {}", current_name, folder_id, e);
            }
        }

        self.progress_tracker.remove_progress(&folder_id).await;
        self.cancelled.write().await.remove(&folder_id);

        info!("[send/lan/folder] completed folder_id={} name={:?}", folder_id, folder_name);
        Ok(())
    }

    pub async fn send_via_iroh(
        &self,
        file_path: PathBuf,
        endpoint_addr: iroh::EndpointAddr,
        enable_compression: bool,
    ) -> Result<String> {
        self.do_send_file_iroh(file_path, None, endpoint_addr, enable_compression).await
    }

    pub async fn send_via_iroh_str(
        &self,
        file_path: PathBuf,
        endpoint_addr_json: &str,
        enable_compression: bool,
    ) -> Result<String> {
        let endpoint_addr: iroh::EndpointAddr = serde_json::from_str(endpoint_addr_json)
            .map_err(|e| anyhow::anyhow!("invalid iroh addr: {}", e))?;
        self.do_send_file_iroh(file_path, None, endpoint_addr, enable_compression).await
    }

    pub async fn send_folder_via_iroh(
        &self,
        folder_path: PathBuf,
        endpoint_addr: iroh::EndpointAddr,
        enable_compression: bool,
    ) -> Result<()> {
        const METHOD: &str = "iroh";
        let folder_name = folder_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("folder")
            .to_string();

        let entries = super::directory::scan_directory(&folder_path).await?;
        let file_entries: Vec<_> = entries.into_iter().filter(|e| !e.is_dir).collect();

        if file_entries.is_empty() {
            return Ok(());
        }

        let chunk_size = *self.chunk_size.read().await;
        let total_size: u64 = file_entries.iter().map(|e| e.size).sum();
        let total_chunks: u32 = file_entries
            .iter()
            .map(|e| ((e.size + chunk_size as u64 - 1) / chunk_size as u64) as u32)
            .sum();

        let folder_id = Uuid::new_v4().to_string();

        self.progress_tracker
            .register_queued(
                folder_id.clone(),
                format!("{}/", folder_name),
                total_size,
                total_chunks,
                "send".to_string(),
            )
            .await;
        self.progress_tracker.set_transfer_method(&folder_id, METHOD).await;

        info!(
            "[send/iroh/folder] start folder_id={} name={:?} files={} total_size={}",
            folder_id, folder_name, file_entries.len(), total_size
        );

        let _permit = {
            let sem = self.semaphore.read().await.clone();
            match sem.acquire_owned().await {
                Ok(p) => p,
                Err(_) => {
                    self.progress_tracker.remove_progress(&folder_id).await;
                    return Err(anyhow::anyhow!("Transfer semaphore closed"));
                }
            }
        };

        self.progress_tracker.set_active(&folder_id).await;

        for entry in &file_entries {
            if self.cancelled.read().await.contains(&folder_id) {
                info!("[send/iroh/folder] cancelled: {}", folder_id);
                break;
            }

            let current_name = entry.relative_path.to_string_lossy().replace('\\', "/");
            self.progress_tracker
                .set_current_file(&folder_id, current_name.clone())
                .await;

            let abs_path = folder_path.join(&entry.relative_path);
            let rel = format!("{}/{}", folder_name, current_name);

            if let Err(e) = self
                .send_file_for_folder_iroh(&folder_id, abs_path, Some(rel), endpoint_addr.clone(), enable_compression, chunk_size)
                .await
            {
                warn!("[send/iroh/folder] failed to send file {} in folder {}: {}", current_name, folder_id, e);
            }
        }

        self.progress_tracker.remove_progress(&folder_id).await;
        self.cancelled.write().await.remove(&folder_id);

        info!("[send/iroh/folder] completed folder_id={} name={:?}", folder_id, folder_name);
        Ok(())
    }

    pub async fn send_folder_via_iroh_str(
        &self,
        folder_path: PathBuf,
        endpoint_addr_json: &str,
        enable_compression: bool,
    ) -> Result<()> {
        let endpoint_addr: iroh::EndpointAddr = serde_json::from_str(endpoint_addr_json)
            .map_err(|e| anyhow::anyhow!("invalid iroh addr: {}", e))?;
        self.send_folder_via_iroh(folder_path, endpoint_addr, enable_compression).await
    }

    fn is_compressible(file_name: &str) -> bool {
        let ext = file_name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        !matches!(ext.as_str(),
            "mp4" | "mov" | "mkv" | "avi" | "wmv" | "flv" | "webm" |
            "mp3" | "aac" | "ogg" | "flac" | "m4a" | "wav" |
            "jpg" | "jpeg" | "png" | "gif" | "webp" | "heic" | "avif" |
            "zip" | "gz" | "bz2" | "xz" | "7z" | "rar" | "zst" | "lz4" |
            "pdf" | "docx" | "xlsx" | "pptx"
        )
    }

    // Each stream reads its own continuous chunk range independently.
    // Computes whole-file hash upfront (one sequential pass), then N tasks send in parallel.
    async fn send_data_streams(
        &self,
        conn: &iroh::endpoint::Connection,
        file_path: &PathBuf,
        file_id: &str,
        file_size: u64,
        chunk_size: u32,
        total_chunks: u32,
        enable_compression: bool,
        speed_limit_bytes_per_sec: u64,
        n_streams: u32,
    ) -> Result<[u8; 32]> {
        let file_name = file_path
            .file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        let compress = enable_compression && Self::is_compressible(&file_name);

        // Step 1: compute whole-file hash upfront (sequential pass)
        let t_hash = Instant::now();
        let file_hash = {
            let mut hasher = Sha256::new();
            let mut f = tokio::fs::File::open(file_path).await?;
            let mut buf = vec![0u8; 4 * 1024 * 1024];
            loop {
                let n = f.read(&mut buf).await?;
                if n == 0 { break; }
                hasher.update(&buf[..n]);
            }
            let r = hasher.finalize();
            let mut h = [0u8; 32];
            h.copy_from_slice(&r);
            h
        };
        let hash_ms = t_hash.elapsed().as_millis();
        info!("[send/iroh/multi] file_id={} upfront_hash_ms={}", file_id, hash_ms);

        // Step 2: partition chunks into N ranges
        let n = n_streams as usize;
        let chunks_per_stream = (total_chunks + n_streams - 1) / n_streams;

        // Step 3: open N data streams and spawn N independent range tasks
        let t_parallel_start = Instant::now();
        let mut tasks: JoinSet<Result<(usize, u64, u64, u64, u64, u64)>> = JoinSet::new();
        for i in 0..n {
            let start_chunk = (i as u32) * chunks_per_stream;
            if start_chunk >= total_chunks { break; }
            let end_chunk = ((i as u32 + 1) * chunks_per_stream).min(total_chunks);

            let (mut ds_send, _ds_recv) = conn.open_bi().await
                .map_err(|e| anyhow::anyhow!("open data stream {} failed: {}", i, e))?;
            Self::write_msg(&mut ds_send, &Message::DataStreamHeader(DataStreamHeader {
                file_id: file_id.to_string(),
                stream_index: i as u32,
            })).await?;

            let fp = file_path.clone();
            let pt = self.progress_tracker.clone();
            let fid = file_id.to_string();
            let spdlim = speed_limit_bytes_per_sec;

            tasks.spawn(async move {
                let t_task = Instant::now();
                let byte_offset = start_chunk as u64 * chunk_size as u64;
                let mut file = tokio::fs::File::open(&fp).await?;
                file.seek(std::io::SeekFrom::Start(byte_offset)).await?;

                let mut total_read_ms = 0u64;
                let mut total_compress_ms = 0u64;
                let mut total_send_ms = 0u64;
                let mut total_bytes_sent = 0u64;

                for chunk_index in start_chunk..end_chunk {
                    let offset = chunk_index as u64 * chunk_size as u64;
                    let remaining = file_size - offset;
                    let read_size = (chunk_size as u64).min(remaining) as usize;
                    let mut buffer = vec![0u8; read_size];

                    let t_read = Instant::now();
                    file.read_exact(&mut buffer).await
                        .map_err(|e| anyhow::anyhow!("read chunk {} failed: {}", chunk_index, e))?;
                    total_read_ms += t_read.elapsed().as_millis() as u64;

                    let t_compress = Instant::now();
                    let (send_data, compressed) = if compress && buffer.len() > 1024 {
                        match crate::compression::Compressor::compress(&buffer) {
                            Ok(c) if c.len() < buffer.len() => (c, true),
                            _ => (buffer, false),
                        }
                    } else {
                        (buffer, false)
                    };
                    total_compress_ms += t_compress.elapsed().as_millis() as u64;

                    let bytes_len = send_data.len() as u64;
                    total_bytes_sent += bytes_len;

                    let t_send = Instant::now();
                    Self::write_chunk_raw(&mut ds_send, chunk_index, compressed, &send_data).await?;
                    let chunk_send_ms = t_send.elapsed().as_millis() as u64;
                    total_send_ms += chunk_send_ms;

                    // Warn if a single chunk send stalled — likely QUIC flow control
                    if chunk_send_ms > 500 {
                        warn!(
                            "[send/iroh/multi] stream={} chunk={} STALL send_ms={} (possible QUIC flow-control)",
                            i, chunk_index, chunk_send_ms
                        );
                    }

                    pt.update_progress(&fid, bytes_len).await;

                    if spdlim > 0 {
                        let delay = (bytes_len * 1_000_000) / spdlim;
                        tokio::time::sleep(tokio::time::Duration::from_micros(delay)).await;
                    }
                }

                let task_elapsed_ms = t_task.elapsed().as_millis() as u64;
                let mbps = if task_elapsed_ms > 0 {
                    (total_bytes_sent as f64 / 1_048_576.0) / (task_elapsed_ms as f64 / 1000.0)
                } else { 0.0 };

                info!(
                    "[send/iroh/multi] stream={} chunks={}..{} bytes={} elapsed_ms={} read_ms={} compress_ms={} send_ms={} throughput={:.2}MB/s",
                    i, start_chunk, end_chunk, total_bytes_sent, task_elapsed_ms,
                    total_read_ms, total_compress_ms, total_send_ms, mbps
                );
                Ok((i, total_read_ms, total_compress_ms, total_send_ms, total_bytes_sent, task_elapsed_ms))
            });
        }

        let mut total_bytes_all = 0u64;
        while let Some(result) = tasks.join_next().await {
            match result {
                Err(e) => warn!("[send/iroh/multi] stream task panicked: {:?}", e),
                Ok(Err(e)) => warn!("[send/iroh/multi] stream error: {}", e),
                Ok(Ok((_, _, _, _, bytes, _))) => { total_bytes_all += bytes; }
            }
        }

        let parallel_ms = t_parallel_start.elapsed().as_millis() as u64;
        let agg_mbps = if parallel_ms > 0 {
            (total_bytes_all as f64 / 1_048_576.0) / (parallel_ms as f64 / 1000.0)
        } else { 0.0 };
        info!(
            "[send/iroh/multi] file_id={} all_streams_done total_bytes={} parallel_ms={} aggregate={:.2}MB/s",
            file_id, total_bytes_all, parallel_ms, agg_mbps
        );

        Ok(file_hash)
    }

    async fn iroh_get_or_connect(&self, endpoint_addr: iroh::EndpointAddr) -> Result<iroh::endpoint::Connection> {
        let id = endpoint_addr.id;
        {
            let cache = self.iroh_conn_cache.read().await;
            if let Some(conn) = cache.get(&id) {
                if conn.close_reason().is_none() {
                    debug!("Reusing cached iroh connection to {:?}", id);
                    return Ok(conn.clone());
                }
            }
        }
        self.iroh_conn_cache.write().await.remove(&id);

        info!("Establishing new iroh connection to {:?}", id);
        let conn = self.iroh_manager.connect(endpoint_addr).await?;
        self.iroh_conn_cache.write().await.insert(id, conn.clone());
        Ok(conn)
    }

    async fn do_send_file(
        &self,
        file_path: PathBuf,
        relative_path: Option<String>,
        target_addr: SocketAddr,
        enable_compression: bool,
    ) -> Result<String> {
        const METHOD: &str = "lan";
        let chunk_size = *self.chunk_size.read().await;
        let speed_limit_bytes_per_sec = *self.speed_limit_bytes_per_sec.read().await;

        let metadata = match tokio::fs::metadata(&file_path).await {
            Ok(m) => m,
            Err(e) => {
                error!("Failed to read file metadata {:?}: {}", file_path, e);
                return Err(e.into());
            }
        };
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
        self.progress_tracker.set_transfer_method(&file_id, METHOD).await;

        info!(
            "[send/lan] start file_id={} name={:?} size={} target={} chunks={}",
            file_id, file_name, file_size, target_addr, total_chunks
        );

        let request = TransferRequest {
            file_id: file_id.clone(),
            file_name: file_name.clone(),
            relative_path,
            file_size,
            chunk_size,
            device_id: String::new(),
            password_hash: None,
            stream_count: None,
        };

        let stream = match TcpStream::connect(target_addr).await {
            Ok(s) => {
                debug!("[send/lan] TCP connected to {}", target_addr);
                s
            }
            Err(e) => {
                error!("[send/lan] TCP connect to {} failed: {}", target_addr, e);
                self.progress_tracker.remove_progress(&file_id).await;
                return Err(e.into());
            }
        };
        stream.set_nodelay(true)?;
        let (mut read_half, mut write_half) = stream.into_split();

        if let Err(e) = Self::write_msg(&mut write_half, &Message::TransferRequest(request)).await {
            error!("[send/lan] failed to send TransferRequest for {}: {}", file_id, e);
            self.progress_tracker.set_error(&file_id, format!("Send request failed: {}", e)).await;
            return Err(e);
        }

        let resume_from = {
            let response = match Self::read_msg(&mut read_half).await {
                Ok(r) => r,
                Err(e) => {
                    error!("[send/lan] failed to read TransferResponse for {}: {}", file_id, e);
                    self.progress_tracker.remove_progress(&file_id).await;
                    return Err(e);
                }
            };
            if let Message::TransferResponse(resp) = response {
                if !resp.accepted {
                    warn!("[send/lan] transfer rejected by receiver: {}", file_id);
                    self.progress_tracker.remove_progress(&file_id).await;
                    return Err(anyhow::anyhow!("Transfer rejected by receiver"));
                }
                let resume = resp.resume_from_chunk.unwrap_or(0);
                if resume > 0 {
                    info!("[send/lan] resuming from chunk {} for {}", resume, file_id);
                } else {
                    debug!("[send/lan] transfer accepted, starting from chunk 0 for {}", file_id);
                }
                resume
            } else {
                0
            }
        };

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

        self.progress_tracker.set_active(&file_id).await;

        let mut file = match tokio::fs::File::open(&file_path).await {
            Ok(f) => f,
            Err(e) => {
                self.progress_tracker.remove_progress(&file_id).await;
                return Err(e.into());
            }
        };
        let mut hasher = Sha256::new();

        if resume_from > 0 {
            let skip_bytes = resume_from as u64 * chunk_size as u64;
            let mut remaining_skip = skip_bytes;
            let mut skip_buf = vec![0u8; 4 * 1024 * 1024];
            while remaining_skip > 0 {
                let read_size = (skip_buf.len() as u64).min(remaining_skip) as usize;
                if let Err(e) = file.read_exact(&mut skip_buf[..read_size]).await {
                    self.progress_tracker.remove_progress(&file_id).await;
                    return Err(e.into());
                }
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
                tokio::time::sleep(Duration::from_millis(50)).await;
            }

            let offset = chunk_index as u64 * chunk_size as u64;
            let remaining = file_size - offset;
            let read_size = (chunk_size as u64).min(remaining) as usize;
            let mut buffer = vec![0u8; read_size];
            if let Err(e) = file.read_exact(&mut buffer).await {
                error!("[send/lan] file read error at chunk {} for {}: {}", chunk_index, file_id, e);
                self.progress_tracker.remove_progress(&file_id).await;
                return Err(e.into());
            }

            hasher.update(&buffer);

            let (send_data, compressed): (Vec<u8>, bool) = if enable_compression && Self::is_compressible(&file_name) && buffer.len() > 1024 {
                match crate::compression::Compressor::compress(&buffer) {
                    Ok(c) if c.len() < buffer.len() => (c, true),
                    _ => (buffer, false),
                }
            } else {
                (buffer, false)
            };

            let chunk_bytes_len = send_data.len() as u64;
            if let Err(e) = Self::write_chunk_raw(&mut write_half, chunk_index, compressed, &send_data).await {
                error!("[send/lan] chunk write error at chunk {} for {}: {}", chunk_index, file_id, e);
                self.progress_tracker.remove_progress(&file_id).await;
                return Err(e);
            }
            self.progress_tracker.update_progress(&file_id, chunk_bytes_len).await;

            if speed_limit_bytes_per_sec > 0 {
                let delay_micros = (chunk_bytes_len * 1_000_000) / speed_limit_bytes_per_sec;
                tokio::time::sleep(tokio::time::Duration::from_micros(delay_micros)).await;
            }
        }

        let hash_result = hasher.finalize();
        let mut file_hash = [0u8; 32];
        file_hash.copy_from_slice(&hash_result);

        if let Err(e) = Self::write_msg(&mut write_half, &Message::TransferComplete(TransferComplete {
            file_id: file_id.clone(),
            file_hash,
        })).await {
            error!("[send/lan] failed to send TransferComplete for {}: {}", file_id, e);
            self.progress_tracker.remove_progress(&file_id).await;
            return Err(e);
        }

        match Self::read_msg(&mut read_half).await {
            Ok(Message::TransferComplete(_)) => {}
            Ok(Message::TransferError(e)) => {
                error!("[send/lan] receiver reported error for {}: {}", file_id, e.error);
                self.progress_tracker.remove_progress(&file_id).await;
                return Err(anyhow::anyhow!("Receiver error: {}", e.error));
            }
            Ok(_) => {}
            Err(e) => {
                error!("[send/lan] failed to read final ack for {}: {}", file_id, e);
                self.progress_tracker.remove_progress(&file_id).await;
                return Err(e);
            }
        }

        let (elapsed, method) = self.progress_tracker.get_progress(&file_id).await
            .map(|p| (p.start_time.elapsed().as_secs(), p.transfer_method.clone()))
            .unwrap_or((0, None));
        self.progress_tracker.remove_progress(&file_id).await;
        info!("[send/lan] completed file_id={} name={:?} size={} elapsed={}s", file_id, file_name, file_size, elapsed);

        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let _ = self.history_store.add_record(TransferRecord {
            id: Uuid::new_v4().to_string(),
            file_name: file_name.clone(),
            file_size,
            direction: "send".to_string(),
            status: "completed".to_string(),
            error: None,
            timestamp: ts,
            elapsed_secs: elapsed,
            save_path: Some(file_path.to_string_lossy().to_string()),
            transfer_method: method,
        }).await;

        Ok(file_id)
    }

    async fn send_file_for_folder(
        &self,
        folder_id: &str,
        file_path: PathBuf,
        relative_path: Option<String>,
        target_addr: SocketAddr,
        enable_compression: bool,
        chunk_size: u32,
    ) -> Result<()> {
        let speed_limit_bytes_per_sec = *self.speed_limit_bytes_per_sec.read().await;

        let metadata = tokio::fs::metadata(&file_path).await?;
        let file_size = metadata.len();

        let file_name = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        let total_chunks = ((file_size + chunk_size as u64 - 1) / chunk_size as u64) as u32;

        let request = TransferRequest {
            file_id: {
                let mut h = Sha256::new();
                h.update(file_name.as_bytes());
                h.update(&file_size.to_le_bytes());
                let r = h.finalize();
                r[..16].iter().map(|b| format!("{:02x}", b)).collect::<String>()
            },
            file_name: file_name.clone(),
            relative_path,
            file_size,
            chunk_size,
            device_id: String::new(),
            password_hash: None,
            stream_count: None,
        };

        let stream = TcpStream::connect(target_addr).await?;
        stream.set_nodelay(true)?;
        let (mut read_half, mut write_half) = stream.into_split();

        Self::write_msg(&mut write_half, &Message::TransferRequest(request)).await?;

        let resume_from = {
            let response = Self::read_msg(&mut read_half).await?;
            if let Message::TransferResponse(resp) = response {
                if !resp.accepted {
                    return Err(anyhow::anyhow!("Transfer rejected by receiver"));
                }
                resp.resume_from_chunk.unwrap_or(0)
            } else {
                0
            }
        };

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
            if self.cancelled.read().await.contains(folder_id) {
                return Ok(());
            }

            loop {
                if !self.paused.read().await.contains(folder_id) { break; }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }

            let offset = chunk_index as u64 * chunk_size as u64;
            let remaining = file_size - offset;
            let read_size = (chunk_size as u64).min(remaining) as usize;
            let mut buffer = vec![0u8; read_size];
            file.read_exact(&mut buffer).await?;

            hasher.update(&buffer);

            let (send_data, compressed): (Vec<u8>, bool) = if enable_compression && Self::is_compressible(&file_name) && buffer.len() > 1024 {
                match crate::compression::Compressor::compress(&buffer) {
                    Ok(c) if c.len() < buffer.len() => (c, true),
                    _ => (buffer, false),
                }
            } else {
                (buffer, false)
            };

            let chunk_bytes_len = send_data.len() as u64;
            Self::write_chunk_raw(&mut write_half, chunk_index, compressed, &send_data).await?;
            self.progress_tracker.update_progress(folder_id, chunk_bytes_len).await;

            if speed_limit_bytes_per_sec > 0 {
                let delay_micros = (chunk_bytes_len * 1_000_000) / speed_limit_bytes_per_sec;
                tokio::time::sleep(tokio::time::Duration::from_micros(delay_micros)).await;
            }
        }

        let hash_result = hasher.finalize();
        let mut file_hash = [0u8; 32];
        file_hash.copy_from_slice(&hash_result);

        Self::write_msg(&mut write_half, &Message::TransferComplete(TransferComplete {
            file_id: String::new(),
            file_hash,
        })).await?;

        match Self::read_msg(&mut read_half).await? {
            Message::TransferComplete(_) => {}
            Message::TransferError(e) => {
                return Err(anyhow::anyhow!("Receiver error: {}", e.error));
            }
            _ => {}
        }

        Ok(())
    }

    async fn do_send_file_iroh(
        &self,
        file_path: PathBuf,
        relative_path: Option<String>,
        endpoint_addr: iroh::EndpointAddr,
        enable_compression: bool,
    ) -> Result<String> {
        const METHOD: &str = "iroh";
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

        const MULTI_STREAM_THRESHOLD: u64 = 8 * 1024 * 1024;
        let configured_streams = *self.iroh_stream_count.read().await;
        let n_streams = if configured_streams > 1 && file_size >= MULTI_STREAM_THRESHOLD { configured_streams } else { 1 };

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
        self.progress_tracker.set_transfer_method(&file_id, METHOD).await;

        info!(
            "[send/iroh] start file_id={} name={:?} size={} chunks={} streams={}",
            file_id, file_name, file_size, total_chunks, n_streams
        );

        let request = TransferRequest {
            file_id: file_id.clone(),
            file_name: file_name.clone(),
            relative_path,
            file_size,
            chunk_size,
            device_id: String::new(),
            password_hash: None,
            stream_count: if n_streams > 1 { Some(n_streams) } else { None },
        };

        let conn = match self.iroh_get_or_connect(endpoint_addr).await {
            Ok(c) => c,
            Err(e) => {
                error!("[send/iroh] failed to connect iroh endpoint for {}: {}", file_id, e);
                self.progress_tracker.remove_progress(&file_id).await;
                return Err(e);
            }
        };

        let (mut send, mut recv) = match conn.open_bi().await {
            Ok(s) => {
                debug!("[send/iroh] opened bi stream for {}", file_id);
                s
            }
            Err(e) => {
                error!("[send/iroh] failed to open bi stream for {}: {}", file_id, e);
                self.progress_tracker.set_error(&file_id, format!("iroh stream failed: {}", e)).await;
                return Err(e.into());
            }
        };

        if let Err(e) = Self::write_msg(&mut send, &Message::TransferRequest(request)).await {
            error!("[send/iroh] failed to send TransferRequest for {}: {}", file_id, e);
            self.progress_tracker.set_error(&file_id, format!("Send request failed: {}", e)).await;
            return Err(e);
        }

        let resume_from = {
            let response = match Self::read_msg(&mut recv).await {
                Ok(r) => r,
                Err(e) => {
                    error!("[send/iroh] failed to read TransferResponse for {}: {}", file_id, e);
                    self.progress_tracker.remove_progress(&file_id).await;
                    return Err(e);
                }
            };
            if let Message::TransferResponse(resp) = response {
                if !resp.accepted {
                    warn!("[send/iroh] transfer rejected by receiver: {}", file_id);
                    self.progress_tracker.remove_progress(&file_id).await;
                    return Err(anyhow::anyhow!("Transfer rejected by receiver"));
                }
                let resume = resp.resume_from_chunk.unwrap_or(0);
                if resume > 0 {
                    info!("[send/iroh] resuming from chunk {} for {}", resume, file_id);
                } else {
                    debug!("[send/iroh] transfer accepted, starting from chunk 0 for {}", file_id);
                }
                resume
            } else {
                0
            }
        };

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

        self.progress_tracker.set_active(&file_id).await;

        let file_hash = if n_streams > 1 {
            info!("[send/iroh/multi] starting {} streams for {}", n_streams, file_id);
            match self.send_data_streams(
                &conn,
                &file_path,
                &file_id,
                file_size,
                chunk_size,
                total_chunks,
                enable_compression,
                speed_limit_bytes_per_sec,
                n_streams,
            ).await {
                Ok(h) => h,
                Err(e) => {
                    error!("[send/iroh/multi] send_data_streams failed for {}: {}", file_id, e);
                    self.progress_tracker.remove_progress(&file_id).await;
                    return Err(e);
                }
            }
        } else {
            let mut file = match tokio::fs::File::open(&file_path).await {
                Ok(f) => f,
                Err(e) => {
                    self.progress_tracker.remove_progress(&file_id).await;
                    return Err(e.into());
                }
            };
            let mut hasher = Sha256::new();

            if resume_from > 0 {
                let skip_bytes = resume_from as u64 * chunk_size as u64;
                let mut remaining_skip = skip_bytes;
                let mut skip_buf = vec![0u8; 4 * 1024 * 1024];
                while remaining_skip > 0 {
                    let read_size = (skip_buf.len() as u64).min(remaining_skip) as usize;
                    if let Err(e) = file.read_exact(&mut skip_buf[..read_size]).await {
                        self.progress_tracker.remove_progress(&file_id).await;
                        return Err(e.into());
                    }
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
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }

                let offset = chunk_index as u64 * chunk_size as u64;
                let remaining = file_size - offset;
                let read_size = (chunk_size as u64).min(remaining) as usize;
                let mut buffer = vec![0u8; read_size];
                if let Err(e) = file.read_exact(&mut buffer).await {
                    error!("[send/iroh] file read error at chunk {} for {}: {}", chunk_index, file_id, e);
                    self.progress_tracker.remove_progress(&file_id).await;
                    return Err(e.into());
                }

                hasher.update(&buffer);

                let (send_data, compressed): (Vec<u8>, bool) = if enable_compression && buffer.len() > 1024 {
                    match crate::compression::Compressor::compress(&buffer) {
                        Ok(c) if c.len() < buffer.len() => (c, true),
                        _ => (buffer, false),
                    }
                } else {
                    (buffer, false)
                };

                let chunk_bytes_len = send_data.len() as u64;
                if let Err(e) = Self::write_chunk_raw(&mut send, chunk_index, compressed, &send_data).await {
                    error!("[send/iroh] chunk write error at chunk {} for {}: {}", chunk_index, file_id, e);
                    self.progress_tracker.remove_progress(&file_id).await;
                    return Err(e);
                }
                self.progress_tracker.update_progress(&file_id, chunk_bytes_len).await;

                if speed_limit_bytes_per_sec > 0 {
                    let delay_micros = (chunk_bytes_len * 1_000_000) / speed_limit_bytes_per_sec;
                    tokio::time::sleep(tokio::time::Duration::from_micros(delay_micros)).await;
                }
            }

            let hash_result = hasher.finalize();
            let mut h = [0u8; 32];
            h.copy_from_slice(&hash_result);
            h
        };

        if let Err(e) = Self::write_msg(&mut send, &Message::TransferComplete(TransferComplete {
            file_id: file_id.clone(),
            file_hash,
        })).await {
            error!("[send/iroh] failed to send TransferComplete for {}: {}", file_id, e);
            self.progress_tracker.remove_progress(&file_id).await;
            return Err(e);
        }

        let t_ack_wait = Instant::now();
        match Self::read_msg(&mut recv).await {
            Ok(Message::TransferComplete(_)) => {}
            Ok(Message::TransferError(e)) => {
                error!("[send/iroh] receiver reported error for {}: {}", file_id, e.error);
                self.progress_tracker.remove_progress(&file_id).await;
                return Err(anyhow::anyhow!("Receiver error: {}", e.error));
            }
            Ok(_) => {}
            Err(e) => {
                error!("[send/iroh] failed to read final ack for {}: {}", file_id, e);
                self.progress_tracker.remove_progress(&file_id).await;
                return Err(e);
            }
        }
        info!("[send/iroh] ack_wait_ms={} file_id={} (receiver write+rename+RTT)", t_ack_wait.elapsed().as_millis(), file_id);

        let _ = send.finish();

        let (elapsed, method) = self.progress_tracker.get_progress(&file_id).await
            .map(|p| (p.start_time.elapsed().as_secs(), p.transfer_method.clone()))
            .unwrap_or((0, None));
        self.progress_tracker.remove_progress(&file_id).await;
        let total_mbps = if elapsed > 0 { (file_size as f64 / 1_048_576.0) / elapsed as f64 } else { 0.0 };
        info!("[send/iroh] completed file_id={} name={:?} size={} elapsed={}s avg={:.2}MB/s streams={}", file_id, file_name, file_size, elapsed, total_mbps, n_streams);

        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let _ = self.history_store.add_record(TransferRecord {
            id: Uuid::new_v4().to_string(),
            file_name: file_name.clone(),
            file_size,
            direction: "send".to_string(),
            status: "completed".to_string(),
            error: None,
            timestamp: ts,
            elapsed_secs: elapsed,
            save_path: Some(file_path.to_string_lossy().to_string()),
            transfer_method: method,
        }).await;

        Ok(file_id)
    }

    async fn send_file_for_folder_iroh(
        &self,
        folder_id: &str,
        file_path: PathBuf,
        relative_path: Option<String>,
        endpoint_addr: iroh::EndpointAddr,
        enable_compression: bool,
        chunk_size: u32,
    ) -> Result<()> {
        let speed_limit_bytes_per_sec = *self.speed_limit_bytes_per_sec.read().await;

        let metadata = tokio::fs::metadata(&file_path).await?;
        let file_size = metadata.len();

        let file_name = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        let total_chunks = ((file_size + chunk_size as u64 - 1) / chunk_size as u64) as u32;

        let request = TransferRequest {
            file_id: {
                let mut h = Sha256::new();
                h.update(file_name.as_bytes());
                h.update(&file_size.to_le_bytes());
                let r = h.finalize();
                r[..16].iter().map(|b| format!("{:02x}", b)).collect::<String>()
            },
            file_name: file_name.clone(),
            relative_path,
            file_size,
            chunk_size,
            device_id: String::new(),
            password_hash: None,
            stream_count: None,
        };

        let conn = self.iroh_get_or_connect(endpoint_addr).await?;
        let (mut send, mut recv) = conn.open_bi().await?;

        Self::write_msg(&mut send, &Message::TransferRequest(request)).await?;

        let resume_from = {
            let response = Self::read_msg(&mut recv).await?;
            if let Message::TransferResponse(resp) = response {
                if !resp.accepted {
                    return Err(anyhow::anyhow!("Transfer rejected by receiver"));
                }
                resp.resume_from_chunk.unwrap_or(0)
            } else {
                0
            }
        };

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
            if self.cancelled.read().await.contains(folder_id) {
                return Ok(());
            }

            loop {
                if !self.paused.read().await.contains(folder_id) { break; }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }

            let offset = chunk_index as u64 * chunk_size as u64;
            let remaining = file_size - offset;
            let read_size = (chunk_size as u64).min(remaining) as usize;
            let mut buffer = vec![0u8; read_size];
            file.read_exact(&mut buffer).await?;

            hasher.update(&buffer);

            let (send_data, compressed): (Vec<u8>, bool) = if enable_compression && Self::is_compressible(&file_name) && buffer.len() > 1024 {
                match crate::compression::Compressor::compress(&buffer) {
                    Ok(c) if c.len() < buffer.len() => (c, true),
                    _ => (buffer, false),
                }
            } else {
                (buffer, false)
            };

            let chunk_bytes_len = send_data.len() as u64;
            Self::write_chunk_raw(&mut send, chunk_index, compressed, &send_data).await?;
            self.progress_tracker.update_progress(folder_id, chunk_bytes_len).await;

            if speed_limit_bytes_per_sec > 0 {
                let delay_micros = (chunk_bytes_len * 1_000_000) / speed_limit_bytes_per_sec;
                tokio::time::sleep(tokio::time::Duration::from_micros(delay_micros)).await;
            }
        }

        let hash_result = hasher.finalize();
        let mut file_hash = [0u8; 32];
        file_hash.copy_from_slice(&hash_result);

        Self::write_msg(&mut send, &Message::TransferComplete(TransferComplete {
            file_id: String::new(),
            file_hash,
        })).await?;

        match Self::read_msg(&mut recv).await? {
            Message::TransferComplete(_) => {}
            Message::TransferError(e) => {
                return Err(anyhow::anyhow!("Receiver error: {}", e.error));
            }
            _ => {}
        }

        let _ = send.finish();
        Ok(())
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
            .as_millis() as u64;
        let id = Uuid::new_v4().to_string();

        let chat_msg = ChatMessage {
            id: id.clone(),
            from_instance_id: from_instance_id.clone(),
            from_instance_name: from_instance_name.clone(),
            content: content.clone(),
            timestamp,
            local_seq: 0,
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

        let stream = TcpStream::connect(target_addr).await?;
        stream.set_nodelay(true)?;
        let (mut read_half, mut write_half) = stream.into_split();
        Self::write_msg(&mut write_half, &Message::TextMessage(msg)).await?;

        match Self::read_msg(&mut read_half).await? {
            Message::TextAck(_) => {}
            _ => {}
        }

        Ok(())
    }

    pub async fn cancel_transfer(&self, file_id: &str) {
        let is_error = self.progress_tracker.get_progress(file_id).await
            .map(|p| p.status == "error")
            .unwrap_or(false);
        if is_error {
            self.progress_tracker.remove_progress(file_id).await;
        } else {
            self.cancelled.write().await.insert(file_id.to_string());
        }
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

    pub async fn update_transfer_config(
        &self,
        download_dir: PathBuf,
        chunk_size: u32,
        enable_compression: bool,
        speed_limit_bytes_per_sec: u64,
        require_confirmation: bool,
        iroh_stream_count: u32,
    ) {
        *self.download_dir.write().await = download_dir;
        *self.chunk_size.write().await = chunk_size;
        *self.enable_compression.write().await = enable_compression;
        *self.speed_limit_bytes_per_sec.write().await = speed_limit_bytes_per_sec;
        *self.require_confirmation.write().await = require_confirmation;
        *self.iroh_stream_count.write().await = iroh_stream_count;
    }

    pub async fn update_max_concurrent(&self, n: usize) {
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

    pub fn iroh_manager(&self) -> Arc<IrohManager> {
        self.iroh_manager.clone()
    }

    pub fn message_store(&self) -> Arc<MessageStore> {
        self.message_store.clone()
    }

    pub fn history_store(&self) -> Arc<HistoryStore> {
        self.history_store.clone()
    }
}

impl Clone for TransferService {
    fn clone(&self) -> Self {
        Self {
            tcp_listener: self.tcp_listener.clone(),
            iroh_manager: self.iroh_manager.clone(),
            transfer_port: self.transfer_port,
            iroh_conn_cache: self.iroh_conn_cache.clone(),
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
            iroh_stream_count: self.iroh_stream_count.clone(),
            message_store: self.message_store.clone(),
            history_store: self.history_store.clone(),
            semaphore: self.semaphore.clone(),
        }
    }
}
