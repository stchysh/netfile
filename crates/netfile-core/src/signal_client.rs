use crate::message_store::{ChatMessage, MessageStore};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, Mutex, RwLock};
use tracing::{info, warn};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FriendInfo {
    pub device_id: String,
    pub instance_name: String,
    pub online: bool,
    pub transfer_addr: Option<String>,
    #[serde(default)]
    pub iroh_addr: Option<String>,
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
    RelayMessage {
        to_device_id: String,
        content: String,
        timestamp: u64,
    },
    UpdateTransferAddr {
        transfer_addr: String,
    },
    UpdateIrohAddr {
        iroh_addr: String,
    },
    Heartbeat,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum S2cMsg {
    Registered {
        friends: Vec<FriendInfo>,
        #[serde(default)]
        observed_addr: String,
        #[serde(default)]
        stun_addr: Option<String>,
        #[serde(default)]
        iroh_relay_url: Option<String>,
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
        #[serde(default)]
        iroh_addr: Option<String>,
    },
    FriendOffline {
        device_id: String,
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
    server_addr: String,
    status: Arc<RwLock<SignalStatus>>,
    friends: Arc<RwLock<Vec<FriendInfo>>>,
    message_store: Arc<MessageStore>,
    outgoing_tx: Arc<Mutex<Option<mpsc::Sender<Vec<u8>>>>>,
    pending_invite: Arc<Mutex<Option<oneshot::Sender<String>>>>,
    pending_accept: Arc<Mutex<Option<oneshot::Sender<Result<FriendInfo, String>>>>>,
    pub stun_addr: Arc<RwLock<Option<String>>>,
    pub iroh_relay_url: Arc<RwLock<Option<String>>>,
}

impl SignalClient {
    pub fn new(
        device_id: String,
        instance_name: String,
        transfer_addr: String,
        server_addr: String,
        message_store: Arc<MessageStore>,
    ) -> Arc<Self> {
        Arc::new(Self {
            device_id,
            instance_name: Arc::new(RwLock::new(instance_name)),
            transfer_addr: Arc::new(RwLock::new(transfer_addr)),
            server_addr,
            status: Arc::new(RwLock::new(SignalStatus::Disconnected)),
            friends: Arc::new(RwLock::new(Vec::new())),
            message_store,
            outgoing_tx: Arc::new(Mutex::new(None)),
            pending_invite: Arc::new(Mutex::new(None)),
            pending_accept: Arc::new(Mutex::new(None)),
            stun_addr: Arc::new(RwLock::new(None)),
            iroh_relay_url: Arc::new(RwLock::new(None)),
        })
    }

    pub async fn connect(self: &Arc<Self>) -> Result<()> {
        *self.status.write().await = SignalStatus::Connecting;
        info!("connecting to signal server {}", self.server_addr);
        let stream = TcpStream::connect(&self.server_addr).await?;
        let (mut read_half, mut write_half) = stream.into_split();

        let instance_name = self.instance_name.read().await.clone();
        let transfer_addr = self.transfer_addr.read().await.clone();
        let register_msg = encode_c2s(&C2sMsg::Register {
            device_id: self.device_id.clone(),
            instance_name,
            transfer_addr,
            nat_type: String::new(),
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
                    Err(e) => {
                        warn!("signal read loop ended: {}", e);
                        break;
                    }
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

        info!("connected to signal server {}", self.server_addr);
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

    pub async fn get_transfer_addr(&self) -> String {
        self.transfer_addr.read().await.clone()
    }

    pub async fn get_peer_iroh_addr(&self, device_id: &str) -> Option<String> {
        let friends = self.friends.read().await;
        friends.iter()
            .find(|f| f.device_id == device_id)
            .and_then(|f| f.iroh_addr.clone())
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

    pub async fn send_relay_message(&self, to: &str, content: String, timestamp: u64) -> Result<()> {
        self.send_msg(&C2sMsg::RelayMessage {
            to_device_id: to.to_string(),
            content,
            timestamp,
        })
        .await
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

    pub async fn update_iroh_addr(&self, iroh_addr: String) {
        info!("update_iroh_addr");
        let _ = self.send_msg(&C2sMsg::UpdateIrohAddr { iroh_addr }).await;
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
            S2cMsg::Registered { friends, observed_addr, stun_addr, iroh_relay_url } => {
                info!("received Registered: friends={}, observed_addr={}", friends.len(), observed_addr);
                *self.friends.write().await = friends;

                if let Some(addr) = stun_addr {
                    *self.stun_addr.write().await = Some(addr);
                }
                if let Some(url) = iroh_relay_url {
                    *self.iroh_relay_url.write().await = Some(url);
                }

                if !observed_addr.is_empty() {
                    let current_addr = self.transfer_addr.read().await.clone();
                    let port = current_addr
                        .rsplit(':')
                        .next()
                        .and_then(|p| p.parse::<u16>().ok())
                        .unwrap_or(0);
                    if port > 0 {
                        let full_addr = format!("{}:{}", observed_addr, port);
                        *self.transfer_addr.write().await = full_addr.clone();
                        let _ = self.send_msg(&C2sMsg::UpdateTransferAddr { transfer_addr: full_addr }).await;
                    }
                }

                *self.status.write().await = SignalStatus::Connected;
            }
            S2cMsg::FriendOnline { device_id, instance_name, transfer_addr, iroh_addr } => {
                let mut friends = self.friends.write().await;
                if let Some(f) = friends.iter_mut().find(|f| f.device_id == device_id) {
                    f.instance_name = instance_name;
                    f.online = true;
                    f.transfer_addr = Some(transfer_addr);
                    f.iroh_addr = iroh_addr;
                } else {
                    friends.push(FriendInfo {
                        device_id,
                        instance_name,
                        online: true,
                        transfer_addr: Some(transfer_addr),
                        iroh_addr,
                    });
                }
            }
            S2cMsg::FriendOffline { device_id } => {
                let mut friends = self.friends.write().await;
                if let Some(f) = friends.iter_mut().find(|f| f.device_id == device_id) {
                    f.online = false;
                    f.transfer_addr = None;
                    f.iroh_addr = None;
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
            S2cMsg::RelayedMessage { from_device_id, from_instance_name, content, timestamp } => {
                let msg = ChatMessage {
                    id: Uuid::new_v4().to_string(),
                    from_instance_id: from_device_id.clone(),
                    from_instance_name,
                    content,
                    timestamp,
                    local_seq: 0,
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
                        local_seq: 0,
                        is_self: false,
                    };
                    let _ = self.message_store.save_message(&om.from_device_id, msg).await;
                }
            }
            S2cMsg::Error { message } => {
                warn!("received Error from signal server: {}", message);
            }
        }
    }
}
