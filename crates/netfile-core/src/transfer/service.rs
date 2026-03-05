use super::file_transfer::FileReceiver;
use super::history::{HistoryStore, TransferRecord};
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

use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::DigitallySignedStruct;

#[derive(Debug)]
struct SkipServerVerification;

impl ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::ED25519,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
            rustls::SignatureScheme::RSA_PSS_SHA512,
        ]
    }
}

pub struct TransferService {
    tcp_listener: Arc<TcpListener>,
    quic_endpoint: Arc<quinn::Endpoint>,
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
        let quic_endpoint = Self::create_quic_endpoint(port, &data_dir).await?;
        info!("Transfer service listening on port {} (QUIC + TCP relay)", port);

        let message_store = Arc::new(MessageStore::new(data_dir.clone()));
        let history_store = Arc::new(HistoryStore::new(data_dir.clone()));
        let semaphore = Arc::new(RwLock::new(Arc::new(Semaphore::new(max_concurrent))));

        Ok(Self {
            tcp_listener: Arc::new(tcp_listener),
            quic_endpoint: Arc::new(quic_endpoint),
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
            history_store,
            semaphore,
        })
    }

    async fn create_quic_endpoint(port: u16, data_dir: &PathBuf) -> Result<quinn::Endpoint> {
        let tls_mgr = crate::tls::TlsManager::new(data_dir.clone());
        tls_mgr.ensure_certificate().await?;

        let cert_path = data_dir.join("certs/cert.pem");
        let key_path = data_dir.join("certs/key.pem");
        let cert_pem = tokio::fs::read(&cert_path).await?;
        let key_pem = tokio::fs::read(&key_path).await?;

        let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut cert_pem.as_slice())
            .filter_map(|c| c.ok())
            .map(|c| c.into_owned())
            .collect();
        let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut key_pem.as_slice())?
            .ok_or_else(|| anyhow::anyhow!("No private key found"))?
            .clone_key();

        let server_config = quinn::ServerConfig::with_single_cert(certs, key)
            .map_err(|e| anyhow::anyhow!("QUIC server config error: {}", e))?;

        let client_crypto = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(SkipServerVerification))
            .with_no_client_auth();
        let quic_client = quinn::crypto::rustls::QuicClientConfig::try_from(client_crypto)
            .map_err(|e| anyhow::anyhow!("QUIC client config error: {}", e))?;
        let client_config = quinn::ClientConfig::new(Arc::new(quic_client));

        let quic_addr: SocketAddr = format!("0.0.0.0:{}", port).parse()?;
        let mut endpoint = quinn::Endpoint::server(server_config, quic_addr)?;
        endpoint.set_default_client_config(client_config);

        info!("QUIC endpoint listening on UDP port {}", port);
        Ok(endpoint)
    }

    async fn find_available_port() -> Result<u16> {
        for port in 37050..37100 {
            match tokio::net::UdpSocket::bind(format!("0.0.0.0:{}", port)).await {
                Ok(_) => return Ok(port),
                Err(_) => continue,
            }
        }
        Err(anyhow::anyhow!("No available port found"))
    }

    pub async fn start(self: Arc<Self>) {
        let accept_quic_task = {
            let service = self.clone();
            tokio::spawn(async move {
                service.accept_quic_loop().await;
            })
        };

        let accept_tcp_task = {
            let service = self.clone();
            tokio::spawn(async move {
                service.accept_tcp_loop().await;
            })
        };

        let process_task = {
            let service = self.clone();
            tokio::spawn(async move {
                service.process_queue_loop().await;
            })
        };

        let _ = tokio::join!(accept_quic_task, accept_tcp_task, process_task);
    }

    async fn accept_quic_loop(&self) {
        while let Some(incoming) = self.quic_endpoint.accept().await {
            let service = Arc::new(self.clone());
            tokio::spawn(async move {
                let conn = match incoming.accept() {
                    Ok(c) => match c.await {
                        Ok(c) => c,
                        Err(e) => { error!("QUIC connection failed: {}", e); return; }
                    },
                    Err(e) => { error!("QUIC accept error: {}", e); return; }
                };
                let addr = conn.remote_address();
                match conn.accept_bi().await {
                    Ok((send, recv)) => {
                        if let Err(e) = service.handle_connection(send, recv, addr).await {
                            debug!("QUIC connection from {} ended: {}", addr, e);
                        }
                    }
                    Err(_) => {
                        debug!("QUIC punch connection from {}", addr);
                    }
                }
            });
        }
    }

    async fn accept_tcp_loop(&self) {
        loop {
            match self.tcp_listener.accept().await {
                Ok((stream, addr)) => {
                    let service = Arc::new(self.clone());
                    tokio::spawn(async move {
                        if let Err(e) = service.handle_tcp_connection(stream, addr).await {
                            debug!("TCP relay connection from {} ended: {}", addr, e);
                        }
                    });
                }
                Err(e) => {
                    error!("TCP accept error: {}", e);
                }
            }
        }
    }

    async fn handle_tcp_connection(&self, mut stream: TcpStream, addr: SocketAddr) -> Result<()> {
        stream.set_nodelay(true)?;
        info!("New TCP relay connection from {}", addr);
        let message = Self::read_msg_tcp(&mut stream).await?;
        match message {
            Message::TransferRequest(request) => {
                self.handle_transfer_request_tcp(stream, request).await?;
            }
            Message::TextMessage(msg) => {
                self.handle_text_message_tcp(stream, msg).await?;
            }
            _ => {
                warn!("Unexpected TCP message type from {}", addr);
            }
        }
        Ok(())
    }

    async fn read_msg_tcp(stream: &mut TcpStream) -> Result<Message> {
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

    async fn write_msg_tcp(stream: &mut TcpStream, msg: &Message) -> Result<()> {
        let data = msg.to_bytes()?;
        let len = (data.len() as u32).to_be_bytes();
        stream.write_all(&len).await?;
        stream.write_all(&data).await?;
        stream.flush().await?;
        Ok(())
    }

    async fn write_chunk_raw_tcp(
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

    async fn read_chunk_raw_tcp(stream: &mut TcpStream) -> Result<(u32, bool, Vec<u8>)> {
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

    async fn handle_transfer_request_tcp(
        &self,
        mut stream: TcpStream,
        request: TransferRequest,
    ) -> Result<()> {
        info!("Received TCP relay transfer request: {} ({} bytes)", request.file_name, request.file_size);
        let file_id = request.file_id.clone();
        let progress_id = format!("recv:{}", file_id);
        let total_chunks = ((request.file_size + request.chunk_size as u64 - 1) / request.chunk_size as u64) as u32;

        if *self.require_confirmation.read().await {
            self.progress_tracker.register_pending_confirm(progress_id.clone(), request.file_name.clone(), request.file_size).await;
            let (tx, rx) = oneshot::channel();
            self.pending_confirmations.write().await.insert(progress_id.clone(), tx);
            let accepted = rx.await.unwrap_or(false);
            self.progress_tracker.remove_progress(&progress_id).await;
            if !accepted {
                let _ = Self::write_msg_tcp(&mut stream, &Message::TransferResponse(TransferResponse {
                    file_id: file_id.clone(), accepted: false, save_path: None, resume_from_chunk: None,
                })).await;
                return Ok(());
            }
        }

        let download_dir = self.download_dir.read().await.clone();
        let mut receiver = FileReceiver::new(request.clone(), download_dir.clone(), self.data_dir.clone()).await?;
        let resume_from_chunk = receiver.resume_from_chunk();

        self.progress_tracker.start_transfer(progress_id.clone(), request.file_name.clone(), request.file_size, total_chunks, "receive".to_string()).await;

        Self::write_msg_tcp(&mut stream, &Message::TransferResponse(TransferResponse {
            file_id: file_id.clone(), accepted: true,
            save_path: Some(download_dir.to_string_lossy().to_string()),
            resume_from_chunk: if resume_from_chunk > 0 { Some(resume_from_chunk) } else { None },
        })).await?;

        for _ in 0..(total_chunks - resume_from_chunk) {
            let (chunk_index, compressed, data) = match Self::read_chunk_raw_tcp(&mut stream).await {
                Ok(r) => r,
                Err(e) => { receiver.cleanup().await; self.progress_tracker.remove_progress(&progress_id).await; return Err(e); }
            };
            let write_data: Vec<u8> = if compressed {
                match crate::compression::Compressor::decompress(&data) {
                    Ok(d) => d,
                    Err(e) => {
                        let _ = Self::write_msg_tcp(&mut stream, &Message::TransferError(TransferError { file_id: file_id.clone(), error: e.to_string() })).await;
                        receiver.cleanup().await;
                        self.progress_tracker.remove_progress(&progress_id).await;
                        return Err(e);
                    }
                }
            } else { data };
            let chunk_bytes_len = write_data.len() as u64;
            if let Err(e) = receiver.write_chunk_raw(chunk_index, &write_data).await {
                let _ = Self::write_msg_tcp(&mut stream, &Message::TransferError(TransferError { file_id: file_id.clone(), error: e.to_string() })).await;
                receiver.cleanup().await; self.progress_tracker.remove_progress(&progress_id).await; return Err(e);
            }
            self.progress_tracker.update_progress(&progress_id, chunk_bytes_len).await;
        }

        let tc = match Self::read_msg_tcp(&mut stream).await {
            Ok(Message::TransferComplete(tc)) => tc,
            Ok(_) => { receiver.cleanup().await; self.progress_tracker.remove_progress(&progress_id).await; return Ok(()); }
            Err(e) => { receiver.cleanup().await; self.progress_tracker.remove_progress(&progress_id).await; return Err(e); }
        };

        match receiver.finalize(tc.file_hash).await {
            Ok(()) => {
                Self::write_msg_tcp(&mut stream, &Message::TransferComplete(TransferComplete { file_id: file_id.clone(), file_hash: tc.file_hash })).await?;
            }
            Err(e) => {
                let _ = Self::write_msg_tcp(&mut stream, &Message::TransferError(TransferError { file_id: file_id.clone(), error: e.to_string() })).await;
                receiver.cleanup().await; self.progress_tracker.remove_progress(&progress_id).await; return Err(e);
            }
        }

        let elapsed = self.progress_tracker.get_progress(&progress_id).await.map(|p| p.start_time.elapsed().as_secs()).unwrap_or(0);
        self.progress_tracker.remove_progress(&progress_id).await;
        let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
        let _ = self.history_store.add_record(TransferRecord {
            id: Uuid::new_v4().to_string(), file_name: request.file_name.clone(), file_size: request.file_size,
            direction: "receive".to_string(), status: "completed".to_string(), error: None, timestamp: ts, elapsed_secs: elapsed,
        }).await;
        Ok(())
    }

    async fn handle_text_message_tcp(&self, mut stream: TcpStream, msg: TextMessage) -> Result<()> {
        let chat_msg = ChatMessage {
            id: msg.id.clone(), from_instance_id: msg.from_instance_id.clone(),
            from_instance_name: msg.from_instance_name.clone(), content: msg.content.clone(),
            timestamp: msg.timestamp, is_self: false,
        };
        self.message_store.save_message(&msg.from_instance_id, chat_msg).await?;
        Self::write_msg_tcp(&mut stream, &Message::TextAck(TextAck { message_id: msg.id })).await?;
        Ok(())
    }

    async fn handle_connection(
        &self,
        send: quinn::SendStream,
        mut recv: quinn::RecvStream,
        addr: SocketAddr,
    ) -> Result<()> {
        info!("New QUIC connection from {}", addr);
        let message = Self::read_msg(&mut recv).await?;
        match message {
            Message::TransferRequest(request) => {
                self.handle_transfer_request(send, recv, request).await?;
            }
            Message::TextMessage(msg) => {
                self.handle_text_message(send, msg).await?;
            }
            #[allow(unreachable_patterns)]
            _ => {
                warn!("Unexpected message type from {}", addr);
            }
        }
        Ok(())
    }

    async fn read_msg(recv: &mut quinn::RecvStream) -> Result<Message> {
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

    async fn write_msg(send: &mut quinn::SendStream, msg: &Message) -> Result<()> {
        let data = msg.to_bytes()?;
        let len = (data.len() as u32).to_be_bytes();
        send.write_all(&len).await?;
        send.write_all(&data).await?;
        Ok(())
    }

    async fn write_chunk_raw(
        send: &mut quinn::SendStream,
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

    async fn read_chunk_raw(recv: &mut quinn::RecvStream) -> Result<(u32, bool, Vec<u8>)> {
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

    async fn handle_transfer_request(
        &self,
        mut send: quinn::SendStream,
        mut recv: quinn::RecvStream,
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
            let accepted = rx.await.unwrap_or(false);
            self.progress_tracker.remove_progress(&progress_id).await;
            if !accepted {
                let _ = Self::write_msg(&mut send, &Message::TransferResponse(TransferResponse {
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

        Self::write_msg(&mut send, &Message::TransferResponse(response)).await?;

        for _ in 0..(total_chunks - resume_from_chunk) {
            let (chunk_index, compressed, data) = match Self::read_chunk_raw(&mut recv).await {
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
                        let _ = Self::write_msg(&mut send, &Message::TransferError(TransferError {
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
                let _ = Self::write_msg(&mut send, &Message::TransferError(TransferError {
                    file_id: file_id.clone(),
                    error: e.to_string(),
                })).await;
                receiver.cleanup().await;
                self.progress_tracker.remove_progress(&progress_id).await;
                return Err(e);
            }

            self.progress_tracker.update_progress(&progress_id, chunk_bytes_len).await;
        }

        let tc = match Self::read_msg(&mut recv).await {
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
                Self::write_msg(&mut send, &Message::TransferComplete(TransferComplete {
                    file_id: file_id.clone(),
                    file_hash: tc.file_hash,
                })).await?;
                let _ = send.finish();
            }
            Err(e) => {
                let _ = Self::write_msg(&mut send, &Message::TransferError(TransferError {
                    file_id: file_id.clone(),
                    error: e.to_string(),
                })).await;
                receiver.cleanup().await;
                self.progress_tracker.remove_progress(&progress_id).await;
                return Err(e);
            }
        }

        let elapsed = self.progress_tracker.get_progress(&progress_id).await
            .map(|p| p.start_time.elapsed().as_secs())
            .unwrap_or(0);
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
        }).await;

        Ok(())
    }

    async fn handle_text_message(&self, mut send: quinn::SendStream, msg: TextMessage) -> Result<()> {
        let chat_msg = ChatMessage {
            id: msg.id.clone(),
            from_instance_id: msg.from_instance_id.clone(),
            from_instance_name: msg.from_instance_name.clone(),
            content: msg.content.clone(),
            timestamp: msg.timestamp,
            is_self: false,
        };
        self.message_store.save_message(&msg.from_instance_id, chat_msg).await?;
        Self::write_msg(&mut send, &Message::TextAck(TextAck {
            message_id: msg.id,
        })).await?;
        let _ = send.finish();
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
                warn!("Failed to send file in folder {}: {}", current_name, e);
            }
        }

        self.progress_tracker.remove_progress(&folder_id).await;
        self.cancelled.write().await.remove(&folder_id);

        Ok(())
    }

    pub async fn send_file_with_fallback(
        &self,
        file_path: PathBuf,
        primary_addr: SocketAddr,
        _fallback_addr: Option<SocketAddr>,
        enable_compression: bool,
    ) -> Result<String> {
        self.do_send_file(file_path, None, primary_addr, enable_compression).await
    }

    pub async fn send_folder_with_fallback(
        &self,
        folder_path: PathBuf,
        primary_addr: SocketAddr,
        _fallback_addr: Option<SocketAddr>,
        enable_compression: bool,
    ) -> Result<()> {
        self.send_folder(folder_path, primary_addr, enable_compression).await
    }

    pub async fn punch_hole(&self, peer_addr: SocketAddr) {
        if let Ok(connecting) = self.quic_endpoint.connect(peer_addr, "netfile") {
            match tokio::time::timeout(
                std::time::Duration::from_secs(5),
                connecting,
            ).await {
                Ok(Ok(conn)) => {
                    info!("QUIC punch hole succeeded to {}", peer_addr);
                    conn.close(0u32.into(), b"");
                }
                Ok(Err(e)) => {
                    debug!("QUIC punch hole failed to {}: {}", peer_addr, e);
                }
                Err(_) => {
                    debug!("QUIC punch hole timed out to {}", peer_addr);
                }
            }
        }
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

        let request = TransferRequest {
            file_id: file_id.clone(),
            file_name: file_name.clone(),
            relative_path,
            file_size,
            chunk_size,
            device_id: String::new(),
            password_hash: None,
        };

        let conn = match self.quic_endpoint.connect(target_addr, "netfile") {
            Ok(c) => match tokio::time::timeout(std::time::Duration::from_secs(3), c).await {
                Ok(Ok(c)) => c,
                Ok(Err(e)) => {
                    self.progress_tracker.remove_progress(&file_id).await;
                    return Err(e.into());
                }
                Err(_) => {
                    self.progress_tracker.remove_progress(&file_id).await;
                    return Err(anyhow::anyhow!("QUIC connection timed out"));
                }
            },
            Err(e) => {
                self.progress_tracker.set_error(&file_id, format!("QUIC connection failed: {}", e)).await;
                return Err(e.into());
            }
        };

        let (mut send, mut recv) = match conn.open_bi().await {
            Ok(s) => s,
            Err(e) => {
                self.progress_tracker.set_error(&file_id, format!("QUIC stream failed: {}", e)).await;
                return Err(e.into());
            }
        };

        if let Err(e) = Self::write_msg(&mut send, &Message::TransferRequest(request)).await {
            self.progress_tracker.set_error(&file_id, format!("Send request failed: {}", e)).await;
            return Err(e);
        }

        let resume_from = {
            let response = match Self::read_msg(&mut recv).await {
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
                    Ok(c) if c.len() < buffer.len() => (c, true),
                    _ => (buffer, false),
                }
            } else {
                (buffer, false)
            };

            let chunk_bytes_len = send_data.len() as u64;
            Self::write_chunk_raw(&mut send, chunk_index, compressed, &send_data).await?;
            self.progress_tracker.update_progress(&file_id, chunk_bytes_len).await;

            if speed_limit_bytes_per_sec > 0 {
                let delay_micros = (chunk_bytes_len * 1_000_000) / speed_limit_bytes_per_sec;
                tokio::time::sleep(tokio::time::Duration::from_micros(delay_micros)).await;
            }
        }

        let hash_result = hasher.finalize();
        let mut file_hash = [0u8; 32];
        file_hash.copy_from_slice(&hash_result);

        Self::write_msg(&mut send, &Message::TransferComplete(TransferComplete {
            file_id: file_id.clone(),
            file_hash,
        })).await?;

        match Self::read_msg(&mut recv).await? {
            Message::TransferComplete(_) => {}
            Message::TransferError(e) => {
                self.progress_tracker.remove_progress(&file_id).await;
                return Err(anyhow::anyhow!("Receiver error: {}", e.error));
            }
            _ => {}
        }

        let _ = send.finish();

        let elapsed = self.progress_tracker.get_progress(&file_id).await
            .map(|p| p.start_time.elapsed().as_secs())
            .unwrap_or(0);
        self.progress_tracker.remove_progress(&file_id).await;
        info!("File transfer completed: {}", file_id);

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

        let file_id = {
            let mut h = Sha256::new();
            h.update(file_name.as_bytes());
            h.update(&file_size.to_le_bytes());
            let r = h.finalize();
            r[..16].iter().map(|b| format!("{:02x}", b)).collect::<String>()
        };

        let total_chunks = ((file_size + chunk_size as u64 - 1) / chunk_size as u64) as u32;

        let request = TransferRequest {
            file_id: file_id.clone(),
            file_name: file_name.clone(),
            relative_path,
            file_size,
            chunk_size,
            device_id: String::new(),
            password_hash: None,
        };

        let conn = self.quic_endpoint.connect(target_addr, "netfile")?.await?;
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
            file_id: file_id.clone(),
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

        let conn = match self.quic_endpoint.connect(target_addr, "netfile") {
            Ok(c) => match tokio::time::timeout(Duration::from_secs(3), c).await {
                Ok(Ok(c)) => c,
                Ok(Err(e)) => return Err(e.into()),
                Err(_) => return Err(anyhow::anyhow!("QUIC connection timed out")),
            },
            Err(e) => return Err(e.into()),
        };
        let (mut send, mut recv) = conn.open_bi().await?;
        Self::write_msg(&mut send, &Message::TextMessage(msg)).await?;

        match Self::read_msg(&mut recv).await? {
            Message::TextAck(_) => {}
            _ => {}
        }

        Ok(())
    }

    pub async fn send_file_via_relay(
        &self,
        file_path: PathBuf,
        relay_addr: SocketAddr,
        enable_compression: bool,
    ) -> Result<String> {
        let chunk_size = *self.chunk_size.read().await;
        let speed_limit_bytes_per_sec = *self.speed_limit_bytes_per_sec.read().await;
        let metadata = tokio::fs::metadata(&file_path).await?;
        let file_size = metadata.len();
        let file_name = file_path.file_name().and_then(|n| n.to_str()).unwrap_or("unknown").to_string();

        let file_id = {
            let mut h = Sha256::new();
            h.update(file_name.as_bytes());
            h.update(&file_size.to_le_bytes());
            let r = h.finalize();
            r[..16].iter().map(|b| format!("{:02x}", b)).collect::<String>()
        };

        let total_chunks = ((file_size + chunk_size as u64 - 1) / chunk_size as u64) as u32;

        self.progress_tracker.register_queued(file_id.clone(), file_name.clone(), file_size, total_chunks, "send".to_string()).await;

        let request = TransferRequest {
            file_id: file_id.clone(), file_name: file_name.clone(), relative_path: None,
            file_size, chunk_size, device_id: String::new(), password_hash: None,
        };

        let mut stream = match TcpStream::connect(relay_addr).await {
            Ok(s) => s,
            Err(e) => {
                self.progress_tracker.set_error(&file_id, format!("Relay connection failed: {}", e)).await;
                return Err(e.into());
            }
        };
        stream.set_nodelay(true)?;

        Self::write_msg_tcp(&mut stream, &Message::TransferRequest(request)).await?;

        let resume_from = match Self::read_msg_tcp(&mut stream).await? {
            Message::TransferResponse(resp) => {
                if !resp.accepted {
                    self.progress_tracker.remove_progress(&file_id).await;
                    return Err(anyhow::anyhow!("Transfer rejected by receiver"));
                }
                resp.resume_from_chunk.unwrap_or(0)
            }
            _ => 0,
        };

        let _permit = {
            let sem = self.semaphore.read().await.clone();
            sem.acquire_owned().await.map_err(|_| anyhow::anyhow!("Transfer semaphore closed"))?
        };
        self.progress_tracker.set_active(&file_id).await;

        let mut file = tokio::fs::File::open(&file_path).await?;
        let mut hasher = Sha256::new();

        if resume_from > 0 {
            let mut remaining_skip = resume_from as u64 * chunk_size as u64;
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
                return Ok(file_id);
            }
            loop { if !self.paused.read().await.contains(&file_id) { break; } tokio::time::sleep(Duration::from_millis(200)).await; }

            let offset = chunk_index as u64 * chunk_size as u64;
            let remaining = file_size - offset;
            let read_size = (chunk_size as u64).min(remaining) as usize;
            let mut buffer = vec![0u8; read_size];
            file.read_exact(&mut buffer).await?;
            hasher.update(&buffer);

            let (send_data, compressed): (Vec<u8>, bool) = if enable_compression && buffer.len() > 1024 {
                match crate::compression::Compressor::compress(&buffer) {
                    Ok(c) if c.len() < buffer.len() => (c, true),
                    _ => (buffer, false),
                }
            } else { (buffer, false) };

            let chunk_bytes_len = send_data.len() as u64;
            Self::write_chunk_raw_tcp(&mut stream, chunk_index, compressed, &send_data).await?;
            self.progress_tracker.update_progress(&file_id, chunk_bytes_len).await;

            if speed_limit_bytes_per_sec > 0 {
                let delay_micros = (chunk_bytes_len * 1_000_000) / speed_limit_bytes_per_sec;
                tokio::time::sleep(Duration::from_micros(delay_micros)).await;
            }
        }

        let hash_result = hasher.finalize();
        let mut file_hash = [0u8; 32];
        file_hash.copy_from_slice(&hash_result);

        Self::write_msg_tcp(&mut stream, &Message::TransferComplete(TransferComplete { file_id: file_id.clone(), file_hash })).await?;

        match Self::read_msg_tcp(&mut stream).await? {
            Message::TransferComplete(_) => {}
            Message::TransferError(e) => { self.progress_tracker.remove_progress(&file_id).await; return Err(anyhow::anyhow!("Receiver error: {}", e.error)); }
            _ => {}
        }

        let elapsed = self.progress_tracker.get_progress(&file_id).await.map(|p| p.start_time.elapsed().as_secs()).unwrap_or(0);
        self.progress_tracker.remove_progress(&file_id).await;
        info!("Relay file transfer completed: {}", file_id);

        let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
        let _ = self.history_store.add_record(TransferRecord {
            id: Uuid::new_v4().to_string(), file_name, file_size,
            direction: "send".to_string(), status: "completed".to_string(), error: None, timestamp: ts, elapsed_secs: elapsed,
        }).await;

        Ok(file_id)
    }

    pub async fn send_folder_via_relay(
        &self,
        folder_path: PathBuf,
        relay_addr: SocketAddr,
        enable_compression: bool,
    ) -> Result<()> {
        let folder_name = folder_path.file_name().and_then(|n| n.to_str()).unwrap_or("folder").to_string();
        let entries = super::directory::scan_directory(&folder_path).await?;
        let file_entries: Vec<_> = entries.into_iter().filter(|e| !e.is_dir).collect();
        if file_entries.is_empty() { return Ok(()); }

        let chunk_size = *self.chunk_size.read().await;
        let total_size: u64 = file_entries.iter().map(|e| e.size).sum();
        let total_chunks: u32 = file_entries.iter().map(|e| ((e.size + chunk_size as u64 - 1) / chunk_size as u64) as u32).sum();
        let folder_id = Uuid::new_v4().to_string();
        self.progress_tracker.register_queued(folder_id.clone(), format!("{}/", folder_name), total_size, total_chunks, "send".to_string()).await;

        let _permit = {
            let sem = self.semaphore.read().await.clone();
            sem.acquire_owned().await.map_err(|_| anyhow::anyhow!("Transfer semaphore closed"))?
        };
        self.progress_tracker.set_active(&folder_id).await;

        for entry in &file_entries {
            if self.cancelled.read().await.contains(&folder_id) { break; }
            let abs_path = folder_path.join(&entry.relative_path);
            let rel = format!("{}/{}", folder_name, entry.relative_path.to_string_lossy().replace('\\', "/"));
            if let Err(e) = self.send_file_via_relay(abs_path, relay_addr, enable_compression).await {
                warn!("Failed to send file in folder via relay: {}", e);
            }
        }

        self.progress_tracker.remove_progress(&folder_id).await;
        self.cancelled.write().await.remove(&folder_id);
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

    pub fn history_store(&self) -> Arc<HistoryStore> {
        self.history_store.clone()
    }
}

impl Clone for TransferService {
    fn clone(&self) -> Self {
        Self {
            tcp_listener: self.tcp_listener.clone(),
            quic_endpoint: self.quic_endpoint.clone(),
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
            history_store: self.history_store.clone(),
            semaphore: self.semaphore.clone(),
        }
    }
}
