use crate::message_store::{ChatMessage, MessageStore};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, Mutex, RwLock};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FriendInfo {
    pub device_id: String,
    pub instance_name: String,
    pub online: bool,
    pub transfer_addr: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SignalStatus {
    Disconnected,
    Connecting,
    Connected,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum C2sMsg {
    Register {
        device_id: String,
        instance_name: String,
        transfer_addr: String,
        #[serde(default)]
        nat_type: String,
    },
    GenerateInvite,
    AcceptInvite {
        code: String,
    },
    RequestPunch {
        target_device_id: String,
        #[serde(default)]
        nat_type: String,
    },
    PunchReady {
        target_device_id: String,
    },
    RelayMessage {
        to_device_id: String,
        content: String,
        timestamp: u64,
    },
    RequestRelay {
        target_device_id: String,
        session_id: String,
    },
    UpdateTransferAddr {
        transfer_addr: String,
    },
    Heartbeat,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum S2cMsg {
    Registered {
        friends: Vec<FriendInfo>,
    },
    InviteCode {
        code: String,
    },
    InviteResult {
        success: bool,
        friend: Option<FriendInfo>,
        error: Option<String>,
    },
    FriendOnline {
        device_id: String,
        instance_name: String,
        transfer_addr: String,
    },
    FriendOffline {
        device_id: String,
    },
    PunchCoordinate {
        peer_addr: String,
        peer_device_id: String,
        #[serde(default)]
        peer_nat_type: String,
    },
    PunchRequest {
        initiator_device_id: String,
        initiator_addr: String,
        #[serde(default)]
        initiator_nat_type: String,
    },
    PunchStart {
        peer_addr: String,
        peer_device_id: String,
        peer_nat_type: String,
    },
    RelayedMessage {
        from_device_id: String,
        from_instance_name: String,
        content: String,
        timestamp: u64,
    },
    OfflineMessages {
        messages: Vec<OfflineMsg>,
    },
    RelaySession {
        session_id: String,
        relay_port: u16,
    },
    IncomingRelay {
        session_id: String,
        relay_port: u16,
    },
    RelayUnavailable {
        session_id: String,
        reason: String,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct OfflineMsg {
    from_device_id: String,
    from_instance_name: String,
    content: String,
    timestamp: u64,
}

fn encode_c2s(msg: &C2sMsg) -> Result<Vec<u8>> {
    let json = serde_json::to_vec(msg)?;
    let len = json.len() as u32;
    let mut buf = Vec::with_capacity(4 + json.len());
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(&json);
    Ok(buf)
}

async fn read_s2c(stream: &mut tokio::net::tcp::OwnedReadHalf) -> Result<S2cMsg> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    let msg = serde_json::from_slice(&buf)?;
    Ok(msg)
}

pub struct SignalClient {
    device_id: String,
    instance_name: Arc<RwLock<String>>,
    transfer_addr: Arc<RwLock<String>>,
    nat_type: Arc<RwLock<String>>,
    server_addr: String,
    local_transfer_port: u16,
    status: Arc<RwLock<SignalStatus>>,
    friends: Arc<RwLock<Vec<FriendInfo>>>,
    message_store: Arc<MessageStore>,
    outgoing_tx: Arc<Mutex<Option<mpsc::Sender<Vec<u8>>>>>,
    pending_invite: Arc<Mutex<Option<oneshot::Sender<String>>>>,
    pending_accept: Arc<Mutex<Option<oneshot::Sender<Result<FriendInfo, String>>>>>,
    pending_punch: Arc<RwLock<HashMap<String, oneshot::Sender<String>>>>,
    pending_relay: Arc<RwLock<HashMap<String, oneshot::Sender<Result<u16, String>>>>>,
    punch_handler: Arc<RwLock<Option<Arc<dyn Fn(std::net::SocketAddr) + Send + Sync>>>>,
}

impl SignalClient {
    pub fn new(
        device_id: String,
        instance_name: String,
        transfer_addr: String,
        server_addr: String,
        local_transfer_port: u16,
        message_store: Arc<MessageStore>,
    ) -> Arc<Self> {
        Arc::new(Self {
            device_id,
            instance_name: Arc::new(RwLock::new(instance_name)),
            transfer_addr: Arc::new(RwLock::new(transfer_addr)),
            nat_type: Arc::new(RwLock::new(String::new())),
            server_addr,
            local_transfer_port,
            status: Arc::new(RwLock::new(SignalStatus::Disconnected)),
            friends: Arc::new(RwLock::new(Vec::new())),
            message_store,
            outgoing_tx: Arc::new(Mutex::new(None)),
            pending_invite: Arc::new(Mutex::new(None)),
            pending_accept: Arc::new(Mutex::new(None)),
            pending_punch: Arc::new(RwLock::new(HashMap::new())),
            pending_relay: Arc::new(RwLock::new(HashMap::new())),
            punch_handler: Arc::new(RwLock::new(None)),
        })
    }

    pub async fn set_punch_handler(&self, handler: Arc<dyn Fn(std::net::SocketAddr) + Send + Sync>) {
        *self.punch_handler.write().await = Some(handler);
    }

    pub async fn connect(self: &Arc<Self>) -> Result<()> {
        *self.status.write().await = SignalStatus::Connecting;
        let stream = TcpStream::connect(&self.server_addr).await?;
        let (mut read_half, mut write_half) = stream.into_split();

        let instance_name = self.instance_name.read().await.clone();
        let transfer_addr = self.transfer_addr.read().await.clone();
        let nat_type = self.nat_type.read().await.clone();
        let register_msg = encode_c2s(&C2sMsg::Register {
            device_id: self.device_id.clone(),
            instance_name,
            transfer_addr,
            nat_type,
        })?;
        write_half.write_all(&register_msg).await?;

        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(64);
        *self.outgoing_tx.lock().await = Some(tx);

        let write_task = tokio::spawn(async move {
            while let Some(data) = rx.recv().await {
                if write_half.write_all(&data).await.is_err() {
                    break;
                }
            }
        });

        let client = self.clone();
        tokio::spawn(async move {
            loop {
                match read_s2c(&mut read_half).await {
                    Ok(msg) => {
                        client.handle_incoming(msg).await;
                    }
                    Err(_) => break,
                }
            }
            *client.status.write().await = SignalStatus::Disconnected;
            *client.outgoing_tx.lock().await = None;
            write_task.abort();
        });

        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                match self.status.read().await.clone() {
                    SignalStatus::Connected => return Ok(()),
                    SignalStatus::Disconnected => return Err(anyhow::anyhow!("服务器断开连接")),
                    SignalStatus::Connecting => {}
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!("连接超时：未收到服务器响应"))??;

        Ok(())
    }

    pub async fn disconnect(&self) {
        *self.outgoing_tx.lock().await = None;
        *self.status.write().await = SignalStatus::Disconnected;
    }

    pub async fn status(&self) -> SignalStatus {
        self.status.read().await.clone()
    }

    pub async fn get_friends(&self) -> Vec<FriendInfo> {
        self.friends.read().await.clone()
    }

    pub async fn generate_invite(&self) -> Result<String> {
        let (tx, rx) = oneshot::channel();
        *self.pending_invite.lock().await = Some(tx);
        self.send_msg(&C2sMsg::GenerateInvite).await?;
        tokio::time::timeout(std::time::Duration::from_secs(10), rx)
            .await
            .map_err(|_| anyhow::anyhow!("超时"))?
            .map_err(|_| anyhow::anyhow!("通道关闭"))
    }

    pub async fn accept_invite(&self, code: String) -> Result<FriendInfo> {
        let (tx, rx) = oneshot::channel();
        *self.pending_accept.lock().await = Some(tx);
        self.send_msg(&C2sMsg::AcceptInvite { code }).await?;
        let result = tokio::time::timeout(std::time::Duration::from_secs(10), rx)
            .await
            .map_err(|_| anyhow::anyhow!("超时"))?
            .map_err(|_| anyhow::anyhow!("通道关闭"))?;
        result.map_err(|e| anyhow::anyhow!(e))
    }

    pub async fn update_nat_type(&self, nat_type: String) {
        *self.nat_type.write().await = nat_type;
    }

    pub async fn request_punch(&self, target_device_id: String) -> Result<String> {
        let (tx, rx) = oneshot::channel();
        self.pending_punch.write().await.insert(target_device_id.clone(), tx);
        let nat_type = self.nat_type.read().await.clone();
        self.send_msg(&C2sMsg::RequestPunch {
            target_device_id: target_device_id.clone(),
            nat_type,
        }).await?;
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), rx)
            .await
            .map_err(|_| {
                let _ = self.pending_punch.try_write().map(|mut m| m.remove(&target_device_id));
                anyhow::anyhow!("超时")
            })?
            .map_err(|_| anyhow::anyhow!("通道关闭"))?;
        Ok(result)
    }

    pub async fn send_punch_ready(&self, target_device_id: String) -> Result<()> {
        self.send_msg(&C2sMsg::PunchReady { target_device_id }).await
    }

    pub async fn send_relay_message(&self, to: &str, content: String, timestamp: u64) -> Result<()> {
        self.send_msg(&C2sMsg::RelayMessage {
            to_device_id: to.to_string(),
            content,
            timestamp,
        })
        .await
    }

    pub async fn request_relay(self: &Arc<Self>, target_device_id: &str) -> Result<SocketAddr> {
        let session_id = Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel::<Result<u16, String>>();
        self.pending_relay.write().await.insert(session_id.clone(), tx);

        self.send_msg(&C2sMsg::RequestRelay {
            target_device_id: target_device_id.to_string(),
            session_id: session_id.clone(),
        })
        .await?;

        let relay_port = tokio::time::timeout(std::time::Duration::from_secs(10), rx)
            .await
            .map_err(|_| {
                let _ = self.pending_relay.try_write().map(|mut m| m.remove(&session_id));
                anyhow::anyhow!("relay 请求超时")
            })?
            .map_err(|_| anyhow::anyhow!("通道关闭"))?
            .map_err(|e| anyhow::anyhow!(e))?;

        let server_host = self.server_addr
            .rsplit_once(':')
            .map(|(h, _)| h.to_string())
            .unwrap_or_else(|| self.server_addr.clone());
        let relay_server_addr = format!("{}:{}", server_host, relay_port);

        let mut relay_stream = TcpStream::connect(&relay_server_addr).await?;
        relay_stream.write_all(session_id.as_bytes()).await?;

        let local_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let local_addr = local_listener.local_addr()?;

        tokio::spawn(async move {
            if let Ok((mut local_conn, _)) = local_listener.accept().await {
                let _ = tokio::io::copy_bidirectional(&mut local_conn, &mut relay_stream).await;
            }
        });

        Ok(local_addr)
    }

    pub async fn update_instance_name(&self, name: String) {
        *self.instance_name.write().await = name;
    }

    pub async fn update_transfer_addr(&self, addr: String) {
        *self.transfer_addr.write().await = addr.clone();
        if !addr.is_empty() {
            let _ = self.send_msg(&C2sMsg::UpdateTransferAddr { transfer_addr: addr }).await;
        }
    }

    async fn send_msg(&self, msg: &C2sMsg) -> Result<()> {
        let encoded = encode_c2s(msg)?;
        let guard = self.outgoing_tx.lock().await;
        match guard.as_ref() {
            Some(tx) => tx.send(encoded).await.map_err(|_| anyhow::anyhow!("未连接")),
            None => Err(anyhow::anyhow!("未连接到信令服务器")),
        }
    }

    async fn handle_incoming(self: &Arc<Self>, msg: S2cMsg) {
        match msg {
            S2cMsg::Registered { friends } => {
                *self.friends.write().await = friends;
                *self.status.write().await = SignalStatus::Connected;
            }
            S2cMsg::FriendOnline { device_id, instance_name, transfer_addr } => {
                let mut friends = self.friends.write().await;
                if let Some(f) = friends.iter_mut().find(|f| f.device_id == device_id) {
                    f.instance_name = instance_name;
                    f.online = true;
                    f.transfer_addr = Some(transfer_addr);
                } else {
                    friends.push(FriendInfo {
                        device_id,
                        instance_name,
                        online: true,
                        transfer_addr: Some(transfer_addr),
                    });
                }
            }
            S2cMsg::FriendOffline { device_id } => {
                let mut friends = self.friends.write().await;
                if let Some(f) = friends.iter_mut().find(|f| f.device_id == device_id) {
                    f.online = false;
                    f.transfer_addr = None;
                }
            }
            S2cMsg::InviteCode { code } => {
                if let Some(tx) = self.pending_invite.lock().await.take() {
                    let _ = tx.send(code);
                }
            }
            S2cMsg::InviteResult { success, friend, error } => {
                if let Some(tx) = self.pending_accept.lock().await.take() {
                    if success {
                        if let Some(f) = friend.clone() {
                            let _ = tx.send(Ok(f));
                        } else {
                            let _ = tx.send(Err("服务器返回空好友信息".to_string()));
                        }
                    } else {
                        let _ = tx.send(Err(error.unwrap_or_else(|| "未知错误".to_string())));
                    }
                }
                if success {
                    if let Some(f) = friend {
                        let mut friends = self.friends.write().await;
                        if !friends.iter().any(|x| x.device_id == f.device_id) {
                            friends.push(f);
                        }
                    }
                }
            }
            S2cMsg::PunchCoordinate { peer_addr, peer_device_id, peer_nat_type: _ } => {
                let tx = self.pending_punch.write().await.remove(&peer_device_id);
                if let Some(tx) = tx {
                    let _ = tx.send(peer_addr);
                }
                let _ = self.send_msg(&C2sMsg::PunchReady {
                    target_device_id: peer_device_id,
                }).await;
            }
            S2cMsg::PunchRequest { initiator_device_id, initiator_addr, initiator_nat_type: _ } => {
                if let Ok(addr) = initiator_addr.parse::<std::net::SocketAddr>() {
                    if let Some(handler) = self.punch_handler.read().await.as_ref() {
                        handler(addr);
                    }
                }
                let _ = self.send_msg(&C2sMsg::PunchReady {
                    target_device_id: initiator_device_id,
                }).await;
            }
            S2cMsg::PunchStart { peer_addr, peer_device_id: _, peer_nat_type: _ } => {
                if let Ok(addr) = peer_addr.parse::<std::net::SocketAddr>() {
                    if let Some(handler) = self.punch_handler.read().await.as_ref() {
                        handler(addr);
                    }
                }
            }
            S2cMsg::RelayedMessage { from_device_id, from_instance_name, content, timestamp } => {
                let msg = ChatMessage {
                    id: Uuid::new_v4().to_string(),
                    from_instance_id: from_device_id.clone(),
                    from_instance_name,
                    content,
                    timestamp,
                    is_self: false,
                };
                let _ = self.message_store.save_message(&from_device_id, msg).await;
            }
            S2cMsg::OfflineMessages { messages } => {
                for om in messages {
                    let msg = ChatMessage {
                        id: Uuid::new_v4().to_string(),
                        from_instance_id: om.from_device_id.clone(),
                        from_instance_name: om.from_instance_name,
                        content: om.content,
                        timestamp: om.timestamp,
                        is_self: false,
                    };
                    let _ = self.message_store.save_message(&om.from_device_id, msg).await;
                }
            }
            S2cMsg::RelaySession { session_id, relay_port } => {
                let tx = self.pending_relay.write().await.remove(&session_id);
                if let Some(tx) = tx {
                    let _ = tx.send(Ok(relay_port));
                }
            }
            S2cMsg::RelayUnavailable { session_id, reason } => {
                let tx = self.pending_relay.write().await.remove(&session_id);
                if let Some(tx) = tx {
                    let _ = tx.send(Err(reason));
                }
            }
            S2cMsg::IncomingRelay { session_id, relay_port } => {
                let server_host = self.server_addr
                    .rsplit_once(':')
                    .map(|(h, _)| h.to_string())
                    .unwrap_or_else(|| self.server_addr.clone());
                let local_transfer_port = self.local_transfer_port;
                tokio::spawn(async move {
                    let relay_server_addr = format!("{}:{}", server_host, relay_port);
                    let mut relay_stream = match TcpStream::connect(&relay_server_addr).await {
                        Ok(s) => s,
                        Err(_) => return,
                    };
                    if relay_stream.write_all(session_id.as_bytes()).await.is_err() {
                        return;
                    }
                    let mut local_stream =
                        match TcpStream::connect(format!("127.0.0.1:{}", local_transfer_port)).await {
                            Ok(s) => s,
                            Err(_) => return,
                        };
                    let _ = tokio::io::copy_bidirectional(&mut relay_stream, &mut local_stream).await;
                });
            }
            S2cMsg::Error { .. } => {}
        }
    }
}
