use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, RwLock};
use uuid::Uuid;

use crate::protocol::{read_c2s, write_s2c, C2sMsg, FriendInfo, OfflineMsg, S2cMsg};

struct OnlineEntry {
    instance_name: String,
    transfer_addr: String,
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
}

impl ServerState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            online: RwLock::new(HashMap::new()),
            friends: RwLock::new(HashMap::new()),
            invite_codes: RwLock::new(HashMap::new()),
            offline_msgs: RwLock::new(HashMap::new()),
        })
    }
}

pub async fn handle_connection(state: Arc<ServerState>, mut stream: TcpStream) {
    let (device_id, instance_name, transfer_addr) = match read_c2s(&mut stream).await {
        Ok(C2sMsg::Register { device_id, instance_name, transfer_addr }) => {
            (device_id, instance_name, transfer_addr)
        }
        _ => return,
    };

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
                });
            } else {
                result.push(FriendInfo {
                    device_id: fid.clone(),
                    instance_name: String::new(),
                    online: false,
                    transfer_addr: None,
                });
            }
        }
        result
    };

    if !offline.is_empty() {
        if write_s2c(&mut stream, &S2cMsg::OfflineMessages { messages: offline }).await.is_err() {
            return;
        }
    }

    if write_s2c(&mut stream, &S2cMsg::Registered { friends: online_friends }).await.is_err() {
        return;
    }

    let (tx, mut rx) = mpsc::channel::<S2cMsg>(64);

    {
        let mut online_map = state.online.write().await;
        online_map.insert(device_id.clone(), OnlineEntry {
            instance_name: instance_name.clone(),
            transfer_addr: transfer_addr.clone(),
            tx: tx.clone(),
        });
    }

    {
        let friends_map = state.friends.read().await;
        let online_map = state.online.read().await;
        if let Some(friend_ids) = friends_map.get(&device_id) {
            for fid in friend_ids {
                if let Some(entry) = online_map.get(fid) {
                    let _ = entry.tx.try_send(S2cMsg::FriendOnline {
                        device_id: device_id.clone(),
                        instance_name: instance_name.clone(),
                        transfer_addr: transfer_addr.clone(),
                    });
                }
            }
        }
    }

    let (read_half, write_half) = stream.into_split();
    let mut read_half = read_half;
    let mut write_half = write_half;

    let write_task = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        while let Some(msg) = rx.recv().await {
            if let Ok(encoded) = crate::protocol::encode_s2c(&msg) {
                if write_half.write_all(&encoded).await.is_err() {
                    break;
                }
            }
        }
    });

    loop {
        let msg = match read_c2s_half(&mut read_half).await {
            Ok(m) => m,
            Err(_) => break,
        };

        match msg {
            C2sMsg::Register { .. } => {}
            C2sMsg::Heartbeat => {}
            C2sMsg::GenerateInvite => {
                let code = generate_code();
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
                let entry = {
                    let mut codes = state.invite_codes.write().await;
                    codes.remove(&code)
                };
                match entry {
                    None => {
                        let _ = tx.send(S2cMsg::InviteResult {
                            success: false,
                            friend: None,
                            error: Some("邀请码无效或已过期".to_string()),
                        }).await;
                    }
                    Some(inv) if inv.created_at.elapsed() > Duration::from_secs(600) => {
                        let _ = tx.send(S2cMsg::InviteResult {
                            success: false,
                            friend: None,
                            error: Some("邀请码已过期".to_string()),
                        }).await;
                    }
                    Some(inv) => {
                        let initiator_id = inv.device_id.clone();
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
                                transfer_addr: Some(transfer_addr.clone()),
                            };
                            let initiator_info = online_map.get(&initiator_id).map(|e| FriendInfo {
                                device_id: initiator_id.clone(),
                                instance_name: e.instance_name.clone(),
                                online: true,
                                transfer_addr: Some(e.transfer_addr.clone()),
                            });
                            (my_info, initiator_info)
                        };

                        let _ = tx.send(S2cMsg::InviteResult {
                            success: true,
                            friend: initiator_info.clone(),
                            error: None,
                        }).await;

                        if let Some(fi) = initiator_info {
                            let online_map = state.online.read().await;
                            if let Some(entry) = online_map.get(&initiator_id) {
                                let _ = entry.tx.try_send(S2cMsg::FriendOnline {
                                    device_id: my_info.device_id.clone(),
                                    instance_name: my_info.instance_name.clone(),
                                    transfer_addr: my_info.transfer_addr.clone().unwrap_or_default(),
                                });
                                let _ = entry.tx.try_send(S2cMsg::InviteResult {
                                    success: true,
                                    friend: Some(fi),
                                    error: None,
                                });
                            }
                        }
                    }
                }
            }
            C2sMsg::RequestPunch { target_device_id } => {
                let online_map = state.online.read().await;
                match online_map.get(&target_device_id) {
                    None => {
                        let _ = tx.send(S2cMsg::Error { message: "目标设备不在线".to_string() }).await;
                    }
                    Some(target) => {
                        let target_addr = target.transfer_addr.clone();
                        let _ = tx.send(S2cMsg::PunchCoordinate {
                            peer_addr: target_addr.clone(),
                            peer_device_id: target_device_id.clone(),
                        }).await;
                        let _ = target.tx.try_send(S2cMsg::PunchRequest {
                            initiator_device_id: device_id.clone(),
                            initiator_addr: transfer_addr.clone(),
                        });
                    }
                }
            }
            C2sMsg::RelayMessage { to_device_id, content, timestamp } => {
                let is_friend = {
                    let friends_map = state.friends.read().await;
                    friends_map.get(&device_id).map_or(false, |s| s.contains(&to_device_id))
                };
                if !is_friend {
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
                    let _ = target.tx.try_send(relay_msg);
                } else {
                    drop(online_map);
                    let offline_msg = OfflineMsg {
                        from_device_id: device_id.clone(),
                        from_instance_name: instance_name.clone(),
                        content,
                        timestamp,
                    };
                    let mut store = state.offline_msgs.write().await;
                    let queue = store.entry(to_device_id).or_default();
                    if queue.len() < 200 {
                        queue.push(offline_msg);
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

    {
        let friends_map = state.friends.read().await;
        let online_map = state.online.read().await;
        if let Some(friend_ids) = friends_map.get(&device_id) {
            for fid in friend_ids {
                if let Some(entry) = online_map.get(fid) {
                    let _ = entry.tx.try_send(S2cMsg::FriendOffline { device_id: device_id.clone() });
                }
            }
        }
    }
}

async fn read_c2s_half(stream: &mut tokio::net::tcp::OwnedReadHalf) -> anyhow::Result<C2sMsg> {
    use tokio::io::AsyncReadExt;
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
