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
    nat_type: String,
    tx: mpsc::Sender<S2cMsg>,
}

struct PunchSession {
    initiator_id: String,
    target_id: String,
    initiator_addr: String,
    target_addr: String,
    initiator_nat_type: String,
    target_nat_type: String,
    initiator_ready: bool,
    target_ready: bool,
    created_at: Instant,
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
    relay_sessions: RwLock<HashMap<String, mpsc::Sender<TcpStream>>>,
    punch_sessions: RwLock<HashMap<String, PunchSession>>,
    pub relay_port: Option<u16>,
}

impl ServerState {
    pub fn new(relay_port: Option<u16>) -> Arc<Self> {
        Arc::new(Self {
            online: RwLock::new(HashMap::new()),
            friends: RwLock::new(HashMap::new()),
            invite_codes: RwLock::new(HashMap::new()),
            offline_msgs: RwLock::new(HashMap::new()),
            relay_sessions: RwLock::new(HashMap::new()),
            punch_sessions: RwLock::new(HashMap::new()),
            relay_port,
        })
    }

    fn punch_key(a: &str, b: &str) -> String {
        let mut pair = [a, b];
        pair.sort();
        format!("{}:{}", pair[0], pair[1])
    }
}

pub async fn handle_connection(state: Arc<ServerState>, mut stream: TcpStream) {
    let (device_id, instance_name, transfer_addr, nat_type) = match read_c2s(&mut stream).await {
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

    info!("[{}] registered, instance_name={:?}, transfer_addr={:?}", device_id, instance_name, transfer_addr);

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
        info!("[{}] pushing {} offline messages", device_id, offline.len());
        if write_s2c(&mut stream, &S2cMsg::OfflineMessages { messages: offline }).await.is_err() {
            warn!("[{}] failed to send offline messages, disconnecting", device_id);
            return;
        }
    }

    if write_s2c(&mut stream, &S2cMsg::Registered { friends: online_friends.clone() }).await.is_err() {
        warn!("[{}] failed to send Registered, disconnecting", device_id);
        return;
    }

    debug!("[{}] sent Registered with {} friends", device_id, online_friends.len());

    let (tx, mut rx) = mpsc::channel::<S2cMsg>(64);

    {
        let mut online_map = state.online.write().await;
        online_map.insert(device_id.clone(), OnlineEntry {
            instance_name: instance_name.clone(),
            transfer_addr: transfer_addr.clone(),
            nat_type: nat_type.clone(),
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
                info!("[{}] updated transfer_addr: {:?}", device_id, new_addr);
                let friend_txs = {
                    let mut online_map = state.online.write().await;
                    if let Some(entry) = online_map.get_mut(&device_id) {
                        entry.transfer_addr = new_addr.clone();
                    }
                    let friends_map = state.friends.read().await;
                    let mut txs = Vec::new();
                    if let Some(friend_ids) = friends_map.get(&device_id) {
                        for fid in friend_ids {
                            if let Some(entry) = online_map.get(fid) {
                                txs.push(entry.tx.clone());
                            }
                        }
                    }
                    txs
                };
                let inst = instance_name.clone();
                let did = device_id.clone();
                let addr = new_addr.clone();
                for ftx in friend_txs {
                    debug!("[{}] notifying friend of updated transfer_addr", device_id);
                    let _ = ftx.try_send(S2cMsg::FriendOnline {
                        device_id: did.clone(),
                        instance_name: inst.clone(),
                        transfer_addr: addr.clone(),
                    });
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
                                debug!("[{}] notifying initiator [{}] of pairing", device_id, initiator_id);
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
            C2sMsg::RequestPunch { target_device_id, nat_type: initiator_nat } => {
                debug!("[{}] request punch to [{}], nat_type={}", device_id, target_device_id, initiator_nat);
                let online_map = state.online.read().await;
                match online_map.get(&target_device_id) {
                    None => {
                        warn!("[{}] punch target [{}] not online", device_id, target_device_id);
                        let _ = tx.send(S2cMsg::Error { message: "目标设备不在线".to_string() }).await;
                    }
                    Some(target) => {
                        let target_transfer_addr = target.transfer_addr.clone();
                        let target_nat = target.nat_type.clone();
                        let target_tx = target.tx.clone();
                        drop(online_map);

                        info!("[{}] punch: initiator_nat={}, target_nat={}, target_addr={}",
                            device_id, initiator_nat, target_nat, target_transfer_addr);

                        let _ = tx.send(S2cMsg::PunchCoordinate {
                            peer_addr: target_transfer_addr.clone(),
                            peer_device_id: target_device_id.clone(),
                            peer_nat_type: target_nat.clone(),
                        }).await;
                        let _ = target_tx.try_send(S2cMsg::PunchRequest {
                            initiator_device_id: device_id.clone(),
                            initiator_addr: transfer_addr.clone(),
                            initiator_nat_type: initiator_nat.clone(),
                        });

                        let key = ServerState::punch_key(&device_id, &target_device_id);
                        let mut sessions = state.punch_sessions.write().await;
                        sessions.insert(key, PunchSession {
                            initiator_id: device_id.clone(),
                            target_id: target_device_id.clone(),
                            initiator_addr: transfer_addr.clone(),
                            target_addr: target_transfer_addr,
                            initiator_nat_type: initiator_nat,
                            target_nat_type: target_nat,
                            initiator_ready: false,
                            target_ready: false,
                            created_at: Instant::now(),
                        });
                    }
                }
            }
            C2sMsg::PunchReady { target_device_id } => {
                let key = ServerState::punch_key(&device_id, &target_device_id);
                let mut sessions = state.punch_sessions.write().await;
                if let Some(session) = sessions.get_mut(&key) {
                    if session.created_at.elapsed() > Duration::from_secs(15) {
                        debug!("[{}] punch session expired for [{}]", device_id, target_device_id);
                        sessions.remove(&key);
                        continue;
                    }
                    if device_id == session.initiator_id {
                        session.initiator_ready = true;
                    } else {
                        session.target_ready = true;
                    }

                    if session.initiator_ready && session.target_ready {
                        let session = sessions.remove(&key).unwrap();
                        drop(sessions);
                        info!("[{}] punch: both sides ready, sending PunchStart", device_id);

                        let online_map = state.online.read().await;
                        if let Some(initiator) = online_map.get(&session.initiator_id) {
                            let _ = initiator.tx.try_send(S2cMsg::PunchStart {
                                peer_addr: session.target_addr.clone(),
                                peer_device_id: session.target_id.clone(),
                                peer_nat_type: session.target_nat_type.clone(),
                            });
                        }
                        if let Some(target) = online_map.get(&session.target_id) {
                            let _ = target.tx.try_send(S2cMsg::PunchStart {
                                peer_addr: session.initiator_addr.clone(),
                                peer_device_id: session.initiator_id.clone(),
                                peer_nat_type: session.initiator_nat_type.clone(),
                            });
                        }
                    } else {
                        debug!("[{}] punch ready, waiting for peer [{}]", device_id, target_device_id);
                    }
                } else {
                    debug!("[{}] no punch session found for [{}]", device_id, target_device_id);
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
                    let _ = target.tx.try_send(relay_msg);
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
                        debug!("[{}] offline queue for [{}] now {} messages", device_id, to_device_id, queue.len());
                    } else {
                        warn!("[{}] offline queue for [{}] full (200), dropping message", device_id, to_device_id);
                    }
                }
            }
            C2sMsg::RequestRelay { target_device_id, session_id } => {
                debug!("[{}] request relay to [{}], session={}", device_id, target_device_id, session_id);
                let rport = match state.relay_port {
                    None => {
                        warn!("[{}] relay requested but relay is disabled", device_id);
                        let _ = tx.send(S2cMsg::RelayUnavailable {
                            session_id,
                            reason: "服务端未开启文件中继".to_string(),
                        }).await;
                        continue;
                    }
                    Some(p) => p,
                };

                let is_friend = {
                    let friends_map = state.friends.read().await;
                    friends_map.get(&device_id).map_or(false, |s| s.contains(&target_device_id))
                };
                if !is_friend {
                    warn!("[{}] relay to non-friend [{}]", device_id, target_device_id);
                    let _ = tx.send(S2cMsg::RelayUnavailable {
                        session_id,
                        reason: "非好友".to_string(),
                    }).await;
                    continue;
                }

                let target_tx = {
                    let online_map = state.online.read().await;
                    online_map.get(&target_device_id).map(|e| e.tx.clone())
                };
                match target_tx {
                    None => {
                        warn!("[{}] relay target [{}] not online", device_id, target_device_id);
                        let _ = tx.send(S2cMsg::RelayUnavailable {
                            session_id,
                            reason: "目标设备不在线".to_string(),
                        }).await;
                    }
                    Some(target_tx) => {
                        info!("[{}] creating relay session {} for target [{}]", device_id, session_id, target_device_id);
                        let (stream_tx, stream_rx) = mpsc::channel::<TcpStream>(2);
                        {
                            let mut sessions = state.relay_sessions.write().await;
                            sessions.insert(session_id.clone(), stream_tx);
                        }
                        let state_clone = state.clone();
                        let sid = session_id.clone();
                        tokio::spawn(async move {
                            pipe_relay_session(state_clone, sid, stream_rx).await;
                        });
                        let _ = target_tx.try_send(S2cMsg::IncomingRelay {
                            session_id: session_id.clone(),
                            relay_port: rport,
                        });
                        let _ = tx.send(S2cMsg::RelaySession {
                            session_id,
                            relay_port: rport,
                        }).await;
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
                    let _ = entry.tx.try_send(S2cMsg::FriendOffline { device_id: device_id.clone() });
                }
            }
        }
    }
}

pub async fn handle_relay_connection(state: Arc<ServerState>, mut stream: TcpStream) {
    let mut id_buf = [0u8; 36];
    if stream.read_exact(&mut id_buf).await.is_err() {
        debug!("relay connection: failed to read session_id");
        return;
    }
    let session_id = match std::str::from_utf8(&id_buf) {
        Ok(s) => s.to_string(),
        Err(_) => {
            warn!("relay connection: invalid session_id encoding");
            return;
        }
    };
    debug!("relay connection: session_id={}", session_id);
    let tx = state.relay_sessions.read().await.get(&session_id).cloned();
    match tx {
        Some(tx) => {
            debug!("relay connection: session {} found, sending stream", session_id);
            let _ = tx.send(stream).await;
        }
        None => {
            warn!("relay connection: session {} not found (expired or invalid)", session_id);
        }
    }
}

async fn pipe_relay_session(
    state: Arc<ServerState>,
    session_id: String,
    mut rx: mpsc::Receiver<TcpStream>,
) {
    let timeout = Duration::from_secs(30);
    debug!("relay session {}: waiting for first connection", session_id);

    let stream_a = match tokio::time::timeout(timeout, rx.recv()).await {
        Ok(Some(s)) => s,
        _ => {
            warn!("relay session {}: timed out waiting for first connection", session_id);
            state.relay_sessions.write().await.remove(&session_id);
            return;
        }
    };
    debug!("relay session {}: first connection arrived, waiting for second", session_id);

    let stream_b = match tokio::time::timeout(timeout, rx.recv()).await {
        Ok(Some(s)) => s,
        _ => {
            warn!("relay session {}: timed out waiting for second connection", session_id);
            state.relay_sessions.write().await.remove(&session_id);
            return;
        }
    };
    state.relay_sessions.write().await.remove(&session_id);

    info!("relay session {}: both ends connected, pipe started", session_id);

    let (mut ra, mut wa) = stream_a.into_split();
    let (mut rb, mut wb) = stream_b.into_split();
    let t1 = tokio::spawn(async move { tokio::io::copy(&mut ra, &mut wb).await });
    let t2 = tokio::spawn(async move { tokio::io::copy(&mut rb, &mut wa).await });
    let (r1, r2) = tokio::join!(t1, t2);

    let bytes_a = r1.ok().and_then(|r| r.ok()).unwrap_or(0);
    let bytes_b = r2.ok().and_then(|r| r.ok()).unwrap_or(0);
    info!("relay session {}: pipe closed, forwarded {} + {} bytes", session_id, bytes_a, bytes_b);
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
