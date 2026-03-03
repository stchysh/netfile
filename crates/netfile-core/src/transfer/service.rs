use super::file_transfer::{FileReceiver, FileSender};
use super::task_queue::{TaskQueue, TransferTask};
use super::progress::ProgressTracker;
use crate::protocol::{ChunkAck, ChunkData, Message, TransferRequest, TransferResponse};
use anyhow::Result;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

pub struct TransferService {
    listener: Arc<TcpListener>,
    transfer_port: u16,
    task_queue: Arc<TaskQueue>,
    active_senders: Arc<RwLock<HashMap<String, FileSender>>>,
    active_receivers: Arc<RwLock<HashMap<String, FileReceiver>>>,
    progress_tracker: Arc<ProgressTracker>,
    data_dir: PathBuf,
    download_dir: PathBuf,
    chunk_size: u32,
    enable_compression: bool,
}

impl TransferService {
    pub async fn new(
        transfer_port: u16,
        max_concurrent: usize,
        chunk_size: u32,
        data_dir: PathBuf,
        download_dir: PathBuf,
    ) -> Result<Self> {
        Self::new_with_compression(transfer_port, max_concurrent, chunk_size, data_dir, download_dir, false).await
    }

    pub async fn new_with_compression(
        transfer_port: u16,
        max_concurrent: usize,
        chunk_size: u32,
        data_dir: PathBuf,
        download_dir: PathBuf,
        enable_compression: bool,
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
            active_senders: Arc::new(RwLock::new(HashMap::new())),
            active_receivers: Arc::new(RwLock::new(HashMap::new())),
            progress_tracker: Arc::new(ProgressTracker::new()),
            data_dir,
            download_dir,
            chunk_size,
            enable_compression,
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

    async fn handle_connection(&self, mut stream: TcpStream, addr: SocketAddr) -> Result<()> {
        info!("New connection from {}", addr);

        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf).await?;
        let len = u32::from_be_bytes(len_buf) as usize;

        if len > 10 * 1024 * 1024 {
            return Err(anyhow::anyhow!("Message too large: {} bytes", len));
        }

        let mut buf = vec![0u8; len];
        stream.read_exact(&mut buf).await?;

        let message = Message::from_bytes(&buf)?;

        match message {
            Message::TransferRequest(request) => {
                self.handle_transfer_request(stream, request).await?;
            }
            Message::ChunkData(chunk) => {
                self.handle_chunk_data(stream, chunk).await?;
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

        let receiver = FileReceiver::new(
            request.clone(),
            self.download_dir.clone(),
            self.data_dir.clone(),
        )
        .await?;

        let file_id = receiver.file_id().to_string();
        self.active_receivers
            .write()
            .await
            .insert(file_id.clone(), receiver);

        let response = TransferResponse {
            file_id: file_id.clone(),
            accepted: true,
            save_path: Some(self.download_dir.to_string_lossy().to_string()),
        };

        let response_msg = Message::TransferResponse(response);
        let response_data = response_msg.to_bytes()?;
        let len = (response_data.len() as u32).to_be_bytes();

        stream.write_all(&len).await?;
        stream.write_all(&response_data).await?;
        stream.flush().await?;

        info!("Accepted transfer request for file: {}", file_id);

        Ok(())
    }

    async fn handle_chunk_data(&self, mut stream: TcpStream, mut chunk: ChunkData) -> Result<()> {
        let file_id = chunk.file_id.clone();
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
                    chunk.data = bytes::Bytes::from(decompressed);
                    chunk.compressed = false;
                }
                Err(e) => {
                    warn!("Failed to decompress chunk {}: {}", chunk_index, e);
                    return Err(e);
                }
            }
        }

        let mut receivers = self.active_receivers.write().await;
        if let Some(receiver) = receivers.get_mut(&file_id) {
            receiver.write_chunk(chunk).await?;

            let ack = Message::ChunkAck(ChunkAck {
                file_id: file_id.clone(),
                chunk_index,
            });
            let ack_data = ack.to_bytes()?;
            let len = (ack_data.len() as u32).to_be_bytes();

            stream.write_all(&len).await?;
            stream.write_all(&ack_data).await?;
            stream.flush().await?;

            if receiver.is_complete() {
                receiver.finalize().await?;
                receivers.remove(&file_id);
                info!("Transfer completed for file: {}", file_id);
            }
        } else {
            warn!("Received chunk for unknown file: {}", file_id);
        }

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
        self.send_file_with_relative_path(file_path, None, target_addr).await
    }

    pub async fn send_file_with_relative_path(
        &self,
        file_path: PathBuf,
        relative_path: Option<String>,
        target_addr: SocketAddr,
    ) -> Result<String> {
        let mut sender = FileSender::new(file_path.clone(), self.chunk_size, self.data_dir.clone());
        if let Some(rel_path) = relative_path {
            sender = sender.with_relative_path(rel_path);
        }

        let request = sender.prepare().await?;
        let file_id = sender.file_id().to_string();

        self.active_senders
            .write()
            .await
            .insert(file_id.clone(), sender);

        let mut stream = TcpStream::connect(target_addr).await?;

        let request_msg = Message::TransferRequest(request.clone());
        let request_data = request_msg.to_bytes()?;
        let len = (request_data.len() as u32).to_be_bytes();

        stream.write_all(&len).await?;
        stream.write_all(&request_data).await?;
        stream.flush().await?;

        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf).await?;
        let len = u32::from_be_bytes(len_buf) as usize;

        if len > 10 * 1024 * 1024 {
            return Err(anyhow::anyhow!("Message too large: {} bytes", len));
        }

        let mut buf = vec![0u8; len];
        stream.read_exact(&mut buf).await?;

        let response = Message::from_bytes(&buf)?;
        if let Message::TransferResponse(resp) = response {
            if !resp.accepted {
                return Err(anyhow::anyhow!("Transfer rejected by receiver"));
            }
        }

        drop(stream);

        let total_chunks = ((request.file_size + self.chunk_size as u64 - 1)
            / self.chunk_size as u64) as u32;

        self.progress_tracker
            .start_transfer(
                file_id.clone(),
                request.file_name.clone(),
                request.file_size,
                total_chunks,
            )
            .await;

        for chunk_index in 0..total_chunks {
            let chunk = {
                let senders = self.active_senders.read().await;
                let sender = senders
                    .get(&file_id)
                    .ok_or_else(|| anyhow::anyhow!("Sender not found"))?;
                sender.read_chunk(chunk_index).await?
            };

            let mut chunk = chunk;

            if self.enable_compression && chunk.data.len() > 1024 {
                match crate::compression::Compressor::compress(chunk.data.as_ref()) {
                    Ok(compressed) if compressed.len() < chunk.data.len() => {
                        debug!(
                            "Compressed chunk {} from {} to {} bytes ({:.1}%)",
                            chunk_index,
                            chunk.data.len(),
                            compressed.len(),
                            (compressed.len() as f64 / chunk.data.len() as f64) * 100.0
                        );
                        chunk.data = bytes::Bytes::from(compressed);
                        chunk.compressed = true;
                    }
                    _ => {
                        debug!("Chunk {} not compressed (no benefit)", chunk_index);
                    }
                }
            }

            let chunk_size = chunk.data.len() as u64;

            let chunk_msg = Message::ChunkData(chunk);
            let chunk_data = chunk_msg.to_bytes()?;

            let mut stream = TcpStream::connect(target_addr).await?;

            let len = (chunk_data.len() as u32).to_be_bytes();
            stream.write_all(&len).await?;
            stream.write_all(&chunk_data).await?;
            stream.flush().await?;

            let mut len_buf = [0u8; 4];
            stream.read_exact(&mut len_buf).await?;
            let len = u32::from_be_bytes(len_buf) as usize;

            if len > 0 {
                let mut buf = vec![0u8; len];
                stream.read_exact(&mut buf).await?;
            }

            self.progress_tracker
                .update_progress(&file_id, chunk_size)
                .await;

            debug!("Sent chunk {}/{}", chunk_index + 1, total_chunks);
        }

        self.progress_tracker.remove_progress(&file_id).await;
        self.active_senders.write().await.remove(&file_id);
        info!("File transfer completed: {}", file_id);

        Ok(file_id)
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
            active_senders: self.active_senders.clone(),
            active_receivers: self.active_receivers.clone(),
            progress_tracker: self.progress_tracker.clone(),
            data_dir: self.data_dir.clone(),
            download_dir: self.download_dir.clone(),
            chunk_size: self.chunk_size,
            enable_compression: self.enable_compression,
        }
    }
}
