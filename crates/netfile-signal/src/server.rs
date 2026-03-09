use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::protocol::{read_c2s, write_s2c, C2sMsg, FriendInfo, OfflineMsg, S2cMsg};

struct OnlineEntry {
    instance_name: String,
    transfer_addr: String,
    iroh_addr: String,
    tx: mpsc::Sender<S2cMsg>,
}

struct InviteEntry {
    device_id: String,
    created_at: Instant,
}

pub struct ServerState {
    online: RwLock<HashMap<String, OnlineEntry>>,
    friends: RwLock<HashMap<String, HashSet<String>>>,
    invite_codes: RwLock<HashMap<String, InviteEntry>>,
    offline_msgs: RwLock<HashMap<String, Vec<OfflineMsg>>>,
    pub relay_addr: Option<String>,
    pub stun_addr: Option<String>,
    pub iroh_relay_url: Option<String>,
}

impl ServerState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            online: RwLock::new(HashMap::new()),
            friends: RwLock::new(HashMap::new()),
            invite_codes: RwLock::new(HashMap::new()),
            offline_msgs: RwLock::new(HashMap::new()),
            relay_addr: None,
            stun_addr: None,
            iroh_relay_url: None,
        })
    }

    pub fn new_with_relay(relay_addr: String) -> Arc<Self> {
        Arc::new(Self {
            online: RwLock::new(HashMap::new()),
            friends: RwLock::new(HashMap::new()),
            invite_codes: RwLock::new(HashMap::new()),
            offline_msgs: RwLock::new(HashMap::new()),
            relay_addr: Some(relay_addr),
            stun_addr: None,
            iroh_relay_url: None,
        })
    }

    pub fn new_full(relay_addr: Option<String>, stun_addr: Option<String>, iroh_relay_url: Option<String>) -> Arc<Self> {
        Arc::new(Self {
            online: RwLock::new(HashMap::new()),
            friends: RwLock::new(HashMap::new()),
            invite_codes: RwLock::new(HashMap::new()),
            offline_msgs: RwLock::new(HashMap::new()),
            relay_addr,
            stun_addr,
            iroh_relay_url,
        })
    }

    pub async fn cleanup_expired_invites(&self) {
        let mut codes = self.invite_codes.write().await;
        let before = codes.len();
        codes.retain(|_, entry| entry.created_at.elapsed() <= Duration::from_secs(600));
        let removed = before - codes.len();
        if removed > 0 {
            info!("Cleaned up {} expired invite codes", removed);
        }
    }
}

pub async fn handle_connection(state: Arc<ServerState>, mut stream: TcpStream) {
    let peer_addr = match stream.peer_addr() {
        Ok(addr) => addr,
        Err(e) => {
            debug!("Failed to get peer address: {}", e);
            return;
        }
    };
    let observed_ip = peer_addr.ip().to_string();

    let (device_id, instance_name, transfer_addr, _nat_type) = match read_c2s(&mut stream).await {
        Ok(C2sMsg::Register { device_id, instance_name, transfer_addr, nat_type }) => {
            (device_id, instance_name, transfer_addr, nat_type)
        }
        Ok(other) => {
            warn!("First message is not Register: {:?}", other);
            return;
        }
        Err(e) => {
            debug!("Failed to read first message: {}", e);
            return;
        }
    };
    let mut current_transfer_addr = transfer_addr.clone();
    let mut current_iroh_addr = String::new();

    info!("[{}] registered, instance_name={:?}, transfer_addr={:?}, observed_ip={}", device_id, instance_name, transfer_addr, observed_ip);

    let offline = {
        let mut store = state.offline_msgs.write().await;
        store.remove(&device_id).unwrap_or_default()
    };

    let online_friends = {
        let friends_map = state.friends.read().await;
        let online_map = state.online.read().await;
        let friend_ids = friends_map.get(&device_id).cloned().unwrap_or_default();
        let mut result = Vec::new();
        for fid in &friend_ids {
            if let Some(entry) = online_map.get(fid) {
                result.push(FriendInfo {
                    device_id: fid.clone(),
                    instance_name: entry.instance_name.clone(),
                    online: true,
                    transfer_addr: Some(entry.transfer_addr.clone()),
                    iroh_addr: if entry.iroh_addr.is_empty() { None } else { Some(entry.iroh_addr.clone()) },
                });
            } else {
                result.push(FriendInfo {
                    device_id: fid.clone(),
                    instance_name: String::new(),
                    online: false,
                    transfer_addr: None,
                    iroh_addr: None,
                });
            }
        }
        result
    };

    if !offline.is_empty() {
        info!("[{}] pushing {} offline messages", device_id, offline.len());
        if write_s2c(&mut stream, &S2cMsg::OfflineMessages { messages: offline }).await.is_err() {
            warn!("[{}] failed to send offline messages, disconnecting", device_id);
            return;
        }
    }

    if write_s2c(&mut stream, &S2cMsg::Registered { friends: online_friends.clone(), observed_addr: observed_ip.clone(), relay_addr: state.relay_addr.clone(), stun_addr: state.stun_addr.clone(), iroh_relay_url: state.iroh_relay_url.clone() }).await.is_err() {
        warn!("[{}] failed to send Registered, disconnecting", device_id);
        return;
    }

    debug!("[{}] sent Registered with {} friends", device_id, online_friends.len());

    let (tx, mut rx) = mpsc::channel::<S2cMsg>(64);

    {
        let mut online_map = state.online.write().await;
        online_map.insert(device_id.clone(), OnlineEntry {
            instance_name: instance_name.clone(),
            transfer_addr: current_transfer_addr.clone(),
            iroh_addr: current_iroh_addr.clone(),
            tx: tx.clone(),
        });
    }

    {
        let friends_map = state.friends.read().await;
        let online_map = state.online.read().await;
        if let Some(friend_ids) = friends_map.get(&device_id) {
            for fid in friend_ids {
                if let Some(entry) = online_map.get(fid) {
                    debug!("[{}] notifying friend {} of online", device_id, fid);
                    if entry.tx.try_send(S2cMsg::FriendOnline {
                        device_id: device_id.clone(),
                        instance_name: instance_name.clone(),
                        transfer_addr: current_transfer_addr.clone(),
                        iroh_addr: if current_iroh_addr.is_empty() { None } else { Some(current_iroh_addr.clone()) },
                    }).is_err() {
                        warn!("[{}] failed to notify friend [{}] of online (channel full or closed)", device_id, fid);
                    }
                }
            }
        }
    }

    let (read_half, write_half) = stream.into_split();
    let mut read_half = read_half;
    let mut write_half = write_half;

    let did_write = device_id.clone();
    let write_task = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        while let Some(msg) = rx.recv().await {
            if let Ok(encoded) = crate::protocol::encode_s2c(&msg) {
                if write_half.write_all(&encoded).await.is_err() {
                    debug!("[{}] write half closed", did_write);
                    break;
                }
            }
        }
    });

    loop {
        let msg = match read_c2s_half(&mut read_half).await {
            Ok(m) => m,
            Err(e) => {
                debug!("[{}] read error: {}", device_id, e);
                break;
            }
        };

        match msg {
            C2sMsg::Register { .. } => {
                warn!("[{}] received duplicate Register, ignoring", device_id);
            }
            C2sMsg::Heartbeat => {
                debug!("[{}] heartbeat", device_id);
            }
            C2sMsg::UpdateTransferAddr { transfer_addr: new_addr } => {
                if new_addr != current_transfer_addr {
                    current_transfer_addr = new_addr.clone();
                    let txs = collect_friend_txs_and_update_addr(
                        &state, &device_id, Some(new_addr), None,
                    ).await;
                    notify_friends_online(&txs, &device_id, &instance_name, &current_transfer_addr, &current_iroh_addr);
                }
            }
            C2sMsg::UpdateIrohAddr { iroh_addr } => {
                if iroh_addr != current_iroh_addr {
                    info!("[{}] updated iroh_addr", device_id);
                    current_iroh_addr = iroh_addr.clone();
                    let txs = collect_friend_txs_and_update_addr(
                        &state, &device_id, None, Some(iroh_addr),
                    ).await;
                    notify_friends_online(&txs, &device_id, &instance_name, &current_transfer_addr, &current_iroh_addr);
                }
            }
            C2sMsg::GenerateInvite => {
                let code = generate_code();
                info!("[{}] generated invite code: {}", device_id, code);
                {
                    let mut codes = state.invite_codes.write().await;
                    codes.insert(code.clone(), InviteEntry {
                        device_id: device_id.clone(),
                        created_at: Instant::now(),
                    });
                }
                let _ = tx.send(S2cMsg::InviteCode { code }).await;
            }
            C2sMsg::AcceptInvite { code } => {
                debug!("[{}] accepting invite code: {}", device_id, code);
                let entry = {
                    let mut codes = state.invite_codes.write().await;
                    codes.remove(&code)
                };
                match entry {
                    None => {
                        warn!("[{}] invalid invite code: {}", device_id, code);
                        let _ = tx.send(S2cMsg::InviteResult {
                            success: false,
                            friend: None,
                            error: Some("邀请码无效或已过期".to_string()),
                        }).await;
                    }
                    Some(inv) if inv.created_at.elapsed() > Duration::from_secs(600) => {
                        warn!("[{}] expired invite code: {}", device_id, code);
                        let _ = tx.send(S2cMsg::InviteResult {
                            success: false,
                            friend: None,
                            error: Some("邀请码已过期".to_string()),
                        }).await;
                    }
                    Some(inv) => {
                        let initiator_id = inv.device_id.clone();
                        info!("[{}] paired with [{}] via invite code", device_id, initiator_id);
                        {
                            let mut friends_map = state.friends.write().await;
                            friends_map.entry(device_id.clone()).or_default().insert(initiator_id.clone());
                            friends_map.entry(initiator_id.clone()).or_default().insert(device_id.clone());
                        }

                        let (my_info, initiator_info) = {
                            let online_map = state.online.read().await;
                            let my_info = FriendInfo {
                                device_id: device_id.clone(),
                                instance_name: instance_name.clone(),
                                online: true,
                                transfer_addr: Some(current_transfer_addr.clone()),
                                iroh_addr: if current_iroh_addr.is_empty() { None } else { Some(current_iroh_addr.clone()) },
                            };
                            let initiator_info = online_map.get(&initiator_id).map(|e| FriendInfo {
                                device_id: initiator_id.clone(),
                                instance_name: e.instance_name.clone(),
                                online: true,
                                transfer_addr: Some(e.transfer_addr.clone()),
                                iroh_addr: if e.iroh_addr.is_empty() { None } else { Some(e.iroh_addr.clone()) },
                            });
                            (my_info, initiator_info)
                        };

                        let _ = tx.send(S2cMsg::InviteResult {
                            success: true,
                            friend: initiator_info.clone(),
                            error: None,
                        }).await;

                        if let Some(_fi) = initiator_info {
                            let online_map = state.online.read().await;
                            if let Some(entry) = online_map.get(&initiator_id) {
                                debug!("[{}] notifying initiator [{}] of pairing", device_id, initiator_id);
                                if entry.tx.try_send(S2cMsg::FriendOnline {
                                    device_id: my_info.device_id.clone(),
                                    instance_name: my_info.instance_name.clone(),
                                    transfer_addr: my_info.transfer_addr.clone().unwrap_or_default(),
                                    iroh_addr: my_info.iroh_addr.clone(),
                                }).is_err() {
                                    warn!("[{}] failed to notify initiator [{}] of pairing (channel full or closed)", device_id, initiator_id);
                                }
                                if entry.tx.try_send(S2cMsg::InviteResult {
                                    success: true,
                                    friend: Some(my_info),
                                    error: None,
                                }).is_err() {
                                    warn!("[{}] failed to send InviteResult to initiator [{}] (channel full or closed)", device_id, initiator_id);
                                }
                            }
                        }
                    }
                }
            }
            C2sMsg::RequestRelay { to_device_id } => {
                let relay_addr = match &state.relay_addr {
                    Some(addr) => addr.clone(),
                    None => {
                        let _ = tx.send(S2cMsg::Error { message: "服务器未启用relay".to_string() }).await;
                        continue;
                    }
                };
                let is_friend = {
                    let friends_map = state.friends.read().await;
                    friends_map.get(&device_id).map_or(false, |s| s.contains(&to_device_id))
                };
                if !is_friend {
                    let _ = tx.send(S2cMsg::Error { message: "非好友".to_string() }).await;
                    continue;
                }
                let session_key = Uuid::new_v4().to_string();
                let _ = tx.send(S2cMsg::RelayReady {
                    session_key: session_key.clone(),
                    relay_addr: relay_addr.clone(),
                }).await;
                let online_map = state.online.read().await;
                if let Some(target) = online_map.get(&to_device_id) {
                    if target.tx.try_send(S2cMsg::RelayReady {
                        session_key,
                        relay_addr,
                    }).is_err() {
                        warn!("[{}] failed to send RelayReady to [{}] (channel full or closed)", device_id, to_device_id);
                    }
                }
            }
            C2sMsg::RelayMessage { to_device_id, content, timestamp } => {
                let is_friend = {
                    let friends_map = state.friends.read().await;
                    friends_map.get(&device_id).map_or(false, |s| s.contains(&to_device_id))
                };
                if !is_friend {
                    warn!("[{}] relay message to non-friend [{}]", device_id, to_device_id);
                    let _ = tx.send(S2cMsg::Error { message: "非好友".to_string() }).await;
                    continue;
                }
                let relay_msg = S2cMsg::RelayedMessage {
                    from_device_id: device_id.clone(),
                    from_instance_name: instance_name.clone(),
                    content: content.clone(),
                    timestamp,
                };
                let online_map = state.online.read().await;
                if let Some(target) = online_map.get(&to_device_id) {
                    debug!("[{}] relay message to online [{}]", device_id, to_device_id);
                    if target.tx.try_send(relay_msg).is_err() {
                        warn!("[{}] failed to relay message to [{}] (channel full or closed)", device_id, to_device_id);
                    }
                } else {
                    drop(online_map);
                    debug!("[{}] target [{}] offline, queuing message", device_id, to_device_id);
                    let offline_msg = OfflineMsg {
                        from_device_id: device_id.clone(),
                        from_instance_name: instance_name.clone(),
                        content,
                        timestamp,
                    };
                    let mut store = state.offline_msgs.write().await;
                    let queue = store.entry(to_device_id.clone()).or_default();
                    if queue.len() < 200 {
                        queue.push(offline_msg);
                    } else {
                        warn!("[{}] offline queue for [{}] full (200), dropping message", device_id, to_device_id);
                    }
                }
            }
        }
    }

    write_task.abort();

    {
        let mut online_map = state.online.write().await;
        online_map.remove(&device_id);
    }

    info!("[{}] disconnected", device_id);

    {
        let friends_map = state.friends.read().await;
        let online_map = state.online.read().await;
        if let Some(friend_ids) = friends_map.get(&device_id) {
            for fid in friend_ids {
                if let Some(entry) = online_map.get(fid) {
                    debug!("[{}] notifying friend [{}] of offline", device_id, fid);
                    if entry.tx.try_send(S2cMsg::FriendOffline { device_id: device_id.clone() }).is_err() {
                        warn!("[{}] failed to notify friend [{}] of offline (channel full or closed)", device_id, fid);
                    }
                }
            }
        }
    }
}

async fn collect_friend_txs_and_update_addr(
    state: &Arc<ServerState>,
    device_id: &str,
    new_transfer_addr: Option<String>,
    new_iroh_addr: Option<String>,
) -> Vec<mpsc::Sender<S2cMsg>> {
    let mut online_map = state.online.write().await;
    if let Some(entry) = online_map.get_mut(device_id) {
        if let Some(addr) = new_transfer_addr {
            entry.transfer_addr = addr;
        }
        if let Some(addr) = new_iroh_addr {
            entry.iroh_addr = addr;
        }
    }
    let friends_map = state.friends.read().await;
    let mut txs = Vec::new();
    if let Some(friend_ids) = friends_map.get(device_id) {
        for fid in friend_ids {
            if let Some(entry) = online_map.get(fid) {
                txs.push(entry.tx.clone());
            }
        }
    }
    txs
}

fn notify_friends_online(
    txs: &[mpsc::Sender<S2cMsg>],
    device_id: &str,
    instance_name: &str,
    transfer_addr: &str,
    iroh_addr: &str,
) {
    for ftx in txs {
        if ftx.try_send(S2cMsg::FriendOnline {
            device_id: device_id.to_string(),
            instance_name: instance_name.to_string(),
            transfer_addr: transfer_addr.to_string(),
            iroh_addr: if iroh_addr.is_empty() { None } else { Some(iroh_addr.to_string()) },
        }).is_err() {
            warn!("[{}] failed to send FriendOnline to friend channel (full or closed)", device_id);
        }
    }
}

async fn read_c2s_half(stream: &mut tokio::net::tcp::OwnedReadHalf) -> anyhow::Result<C2sMsg> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    let msg = serde_json::from_slice(&buf)?;
    Ok(msg)
}

fn generate_code() -> String {
    let id = Uuid::new_v4().to_string().replace('-', "");
    id[..8].to_uppercase()
}
