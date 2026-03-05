use netfile_core::{
    generate_random_name, ChatMessage, Config, Device, DiscoveryService, FriendInfo, HistoryStore,
    MessageStore, SignalClient, SignalStatus, TransferProgress, TransferRecord, TransferService,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{Manager, State};
use tokio::sync::RwLock;
use uuid::Uuid;

pub struct AppState {
    pub config: Arc<RwLock<Config>>,
    pub discovery_service: Arc<DiscoveryService>,
    pub transfer_service: Arc<TransferService>,
    pub message_store: Arc<MessageStore>,
    pub history_store: Arc<HistoryStore>,
    pub signal_client: Arc<RwLock<Option<Arc<SignalClient>>>>,
}

#[tauri::command]
async fn get_devices(state: State<'_, AppState>) -> Result<Vec<Device>, String> {
    Ok(state.discovery_service.get_devices().await)
}

#[tauri::command]
async fn get_transfers(state: State<'_, AppState>) -> Result<Vec<TransferProgress>, String> {
    Ok(state.transfer_service.progress_tracker().list_all().await)
}

#[tauri::command]
async fn send_file(
    state: State<'_, AppState>,
    target_addr: String,
    file_path: String,
    enable_compression: Option<bool>,
    _public_addr: Option<String>,
    peer_discovery_addr: Option<String>,
    peer_device_id: Option<String>,
) -> Result<(), String> {
    let addr: Option<std::net::SocketAddr> = target_addr.parse().ok();
    let path = PathBuf::from(&file_path);
    if !path.exists() {
        return Err(format!("文件不存在: {}", file_path));
    }
    let compression = enable_compression.unwrap_or(false);

    if let Some(ref da) = peer_discovery_addr {
        if let Ok(disc_addr) = da.parse() {
            let _ = state.discovery_service.send_punch(disc_addr).await;
        }
    }

    if let Some(device_id) = peer_device_id.as_deref() {
        let sc_guard = state.signal_client.read().await;
        if let Some(sc) = sc_guard.as_ref() {
            if let Ok(peer_addr_str) = sc.request_punch(device_id.to_string()).await {
                if let Ok(peer_addr) = peer_addr_str.parse::<std::net::SocketAddr>() {
                    state.transfer_service.punch_hole(peer_addr).await;
                }
            }
        }
    }

    if path.is_dir() {
        let service = state.transfer_service.clone();
        let signal_client = state.signal_client.clone();
        let peer_device_id = peer_device_id.clone();
        tokio::spawn(async move {
            let mut direct_ok = false;
            if let Some(a) = addr {
                if service.send_folder(path.clone(), a, compression).await.is_ok() {
                    direct_ok = true;
                }
            }
            if !direct_ok {
                if let Some(device_id) = peer_device_id.as_deref() {
                    let sc_guard = signal_client.read().await;
                    if let Some(sc) = sc_guard.as_ref() {
                        if let Ok(relay_addr) = sc.request_relay(device_id).await {
                            if let Err(e) = service.send_folder(path, relay_addr, compression).await {
                                tracing::error!("Failed to send folder via relay: {}", e);
                            }
                            return;
                        }
                    }
                }
                tracing::error!("Failed to send folder: no valid address and no relay available");
            }
        });
        Ok(())
    } else {
        if let Some(a) = addr {
            let result = state.transfer_service.send_file_compressed(path.clone(), a, compression).await;
            if result.is_ok() {
                return Ok(());
            }
        }
        if let Some(device_id) = peer_device_id.as_deref() {
            let sc_guard = state.signal_client.read().await;
            if let Some(sc) = sc_guard.as_ref() {
                if let Ok(relay_addr) = sc.request_relay(device_id).await {
                    return state.transfer_service
                        .send_file_compressed(path, relay_addr, compression)
                        .await
                        .map(|_| ())
                        .map_err(|e| e.to_string());
                }
            }
        }
        Err("无有效地址且无可用中继".to_string())
    }
}

#[tauri::command]
async fn get_my_public_addr(state: State<'_, AppState>) -> Result<Option<String>, String> {
    Ok(state.discovery_service.get_my_public_transfer_addr().await)
}

#[tauri::command]
async fn cancel_transfer(
    state: State<'_, AppState>,
    file_id: String,
) -> Result<(), String> {
    state.transfer_service.cancel_transfer(&file_id).await;
    Ok(())
}

#[tauri::command]
async fn pause_transfer(
    state: State<'_, AppState>,
    file_id: String,
) -> Result<(), String> {
    state.transfer_service.pause_transfer(&file_id).await;
    Ok(())
}

#[tauri::command]
async fn resume_transfer(
    state: State<'_, AppState>,
    file_id: String,
) -> Result<(), String> {
    state.transfer_service.resume_transfer(&file_id).await;
    Ok(())
}

#[tauri::command]
async fn pause_all_transfers(state: State<'_, AppState>) -> Result<(), String> {
    state.transfer_service.pause_all().await;
    Ok(())
}

#[tauri::command]
async fn resume_all_transfers(state: State<'_, AppState>) -> Result<(), String> {
    state.transfer_service.resume_all().await;
    Ok(())
}

#[tauri::command]
async fn confirm_transfer(
    state: State<'_, AppState>,
    file_id: String,
) -> Result<(), String> {
    state.transfer_service.confirm_transfer(&file_id).await;
    Ok(())
}

#[tauri::command]
async fn reject_transfer(
    state: State<'_, AppState>,
    file_id: String,
) -> Result<(), String> {
    state.transfer_service.reject_transfer(&file_id).await;
    Ok(())
}

#[tauri::command]
async fn send_text_message(
    state: State<'_, AppState>,
    peer_instance_id: String,
    target_addr: String,
    content: String,
) -> Result<(), String> {
    let addr = target_addr.parse().ok();
    let (from_instance_id, from_instance_name) = {
        let config = state.config.read().await;
        (config.instance.instance_id.clone(), config.instance.instance_name.clone())
    };

    let mut direct_ok = false;
    if let Some(a) = addr {
        let result = state
            .transfer_service
            .send_text_message(&peer_instance_id, a, content.clone(), from_instance_id.clone(), from_instance_name.clone())
            .await;
        if result.is_ok() {
            direct_ok = true;
        }
    }

    if !direct_ok {
        let sc_guard = state.signal_client.read().await;
        if let Some(sc) = sc_guard.as_ref() {
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            sc.send_relay_message(&peer_instance_id, content.clone(), ts)
                .await
                .map_err(|e| e.to_string())?;
            if addr.is_none() {
                let chat_msg = ChatMessage {
                    id: uuid::Uuid::new_v4().to_string(),
                    from_instance_id,
                    from_instance_name,
                    content,
                    timestamp: ts,
                    is_self: true,
                };
                let _ = state.message_store.save_message(&peer_instance_id, chat_msg).await;
            }
            return Ok(());
        }
        return Err("无有效地址且无可用中继".to_string());
    }
    Ok(())
}

#[tauri::command]
async fn get_conversation(
    state: State<'_, AppState>,
    peer_instance_id: String,
) -> Result<Vec<ChatMessage>, String> {
    state
        .message_store
        .load_conversation(&peer_instance_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn get_random_name() -> String {
    generate_random_name()
}

#[tauri::command]
async fn get_message_counts(state: State<'_, AppState>) -> Result<HashMap<String, usize>, String> {
    Ok(state.message_store.get_all_counts().await)
}

#[tauri::command]
async fn get_transfer_history(state: State<'_, AppState>) -> Result<Vec<TransferRecord>, String> {
    Ok(state.history_store.load_history().await)
}

#[tauri::command]
async fn clear_transfer_history(state: State<'_, AppState>) -> Result<(), String> {
    state
        .history_store
        .clear_history()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_config(state: State<'_, AppState>) -> Result<Config, String> {
    Ok(state.config.read().await.clone())
}

#[tauri::command]
async fn update_config(
    state: State<'_, AppState>,
    config: Config,
) -> Result<(), String> {
    let old_signal_addr = state.config.read().await.network.signal_server_addr.clone();
    let new_signal_addr = config.network.signal_server_addr.clone();

    *state.config.write().await = config.clone();
    let config_path = Config::default_path();
    config
        .save(&config_path)
        .map_err(|e| format!("Failed to save config: {}", e))?;

    state
        .discovery_service
        .update_device_info(
            config.instance.device_name.clone(),
            config.instance.instance_name.clone(),
        )
        .await;
    state
        .discovery_service
        .update_broadcast_interval(config.network.broadcast_interval)
        .await;

    let download_dir = if config.transfer.download_dir.is_empty() {
        dirs::download_dir()
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
            .join("NetFile")
    } else {
        PathBuf::from(&config.transfer.download_dir)
    };
    state
        .transfer_service
        .update_transfer_config(
            download_dir,
            config.transfer.chunk_size,
            config.transfer.enable_compression,
            config.transfer.speed_limit_mbps as u64 * 1024 * 1024,
            config.transfer.require_confirmation,
        )
        .await;
    state
        .transfer_service
        .update_max_concurrent(config.transfer.max_concurrent)
        .await;

    if old_signal_addr != new_signal_addr {
        let mut sc_guard = state.signal_client.write().await;
        if let Some(old_sc) = sc_guard.take() {
            old_sc.disconnect().await;
        }
        if !new_signal_addr.is_empty() {
            let message_store = state.message_store.clone();
            let device_id = config.instance.instance_id.clone();
            let instance_name = config.instance.instance_name.clone();
            let transfer_addr = state.discovery_service.get_my_public_transfer_addr().await
                .unwrap_or_default();
            let local_transfer_port = state.transfer_service.local_port();
            let sc = SignalClient::new(device_id, instance_name, transfer_addr, new_signal_addr, local_transfer_port, message_store);
            if let Err(e) = sc.connect().await {
                tracing::warn!("Signal connect failed: {}", e);
            } else {
                let ts = state.transfer_service.clone();
                sc.set_punch_handler(std::sync::Arc::new(move |addr| {
                    let ts = ts.clone();
                    tokio::spawn(async move { ts.punch_hole(addr).await; });
                })).await;
                let ds = state.discovery_service.clone();
                let sc_clone = sc.clone();
                *sc_guard = Some(sc);
                spawn_stun_watcher(sc_clone, ds);
            }
        }
    }

    Ok(())
}

#[tauri::command]
async fn connect_signal_server(
    state: State<'_, AppState>,
    server_addr: String,
) -> Result<(), String> {
    let config = state.config.read().await.clone();
    let message_store = state.message_store.clone();
    let transfer_addr = state.discovery_service.get_my_public_transfer_addr().await
        .unwrap_or_default();
    let local_transfer_port = state.transfer_service.local_port();
    let sc = SignalClient::new(
        config.instance.instance_id.clone(),
        config.instance.instance_name.clone(),
        transfer_addr,
        server_addr,
        local_transfer_port,
        message_store,
    );
    sc.connect().await.map_err(|e| e.to_string())?;
    let ts = state.transfer_service.clone();
    sc.set_punch_handler(std::sync::Arc::new(move |addr| {
        let ts = ts.clone();
        tokio::spawn(async move { ts.punch_hole(addr).await; });
    })).await;
    let ds = state.discovery_service.clone();
    let sc_clone = sc.clone();
    let mut guard = state.signal_client.write().await;
    if let Some(old) = guard.take() {
        old.disconnect().await;
    }
    *guard = Some(sc);
    drop(guard);
    spawn_stun_watcher(sc_clone, ds);
    Ok(())
}

#[tauri::command]
async fn disconnect_signal_server(state: State<'_, AppState>) -> Result<(), String> {
    let mut guard = state.signal_client.write().await;
    if let Some(sc) = guard.take() {
        sc.disconnect().await;
    }
    Ok(())
}

#[tauri::command]
async fn get_signal_status(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let guard = state.signal_client.read().await;
    let connected = match guard.as_ref() {
        Some(sc) => sc.status().await == SignalStatus::Connected,
        None => false,
    };
    Ok(serde_json::json!({ "connected": connected }))
}

#[tauri::command]
async fn generate_invite_code(state: State<'_, AppState>) -> Result<String, String> {
    let guard = state.signal_client.read().await;
    match guard.as_ref() {
        Some(sc) => sc.generate_invite().await.map_err(|e| e.to_string()),
        None => Err("未连接到信令服务器".to_string()),
    }
}

#[tauri::command]
async fn accept_invite_code(
    state: State<'_, AppState>,
    code: String,
) -> Result<FriendInfo, String> {
    let guard = state.signal_client.read().await;
    match guard.as_ref() {
        Some(sc) => sc.accept_invite(code).await.map_err(|e| e.to_string()),
        None => Err("未连接到信令服务器".to_string()),
    }
}

#[tauri::command]
async fn get_signal_friends(state: State<'_, AppState>) -> Result<Vec<FriendInfo>, String> {
    let guard = state.signal_client.read().await;
    match guard.as_ref() {
        Some(sc) => Ok(sc.get_friends().await),
        None => Ok(Vec::new()),
    }
}

#[tauri::command]
async fn send_relay_message(
    state: State<'_, AppState>,
    to_device_id: String,
    content: String,
) -> Result<(), String> {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let guard = state.signal_client.read().await;
    match guard.as_ref() {
        Some(sc) => sc.send_relay_message(&to_device_id, content, ts).await.map_err(|e| e.to_string()),
        None => Err("未连接到信令服务器".to_string()),
    }
}

fn spawn_stun_watcher(sc: Arc<SignalClient>, discovery_service: Arc<DiscoveryService>) {
    tokio::spawn(async move {
        for _ in 0..60 {
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            if let Some(addr) = discovery_service.get_my_public_transfer_addr().await {
                sc.update_transfer_addr(addr).await;
                break;
            }
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            get_devices,
            get_transfers,
            send_file,
            cancel_transfer,
            pause_transfer,
            resume_transfer,
            pause_all_transfers,
            resume_all_transfers,
            confirm_transfer,
            reject_transfer,
            send_text_message,
            get_conversation,
            get_random_name,
            get_message_counts,
            get_transfer_history,
            clear_transfer_history,
            get_config,
            update_config,
            get_my_public_addr,
            connect_signal_server,
            disconnect_signal_server,
            get_signal_status,
            generate_invite_code,
            accept_invite_code,
            get_signal_friends,
            send_relay_message,
        ])
        .setup(|app| {
            tauri::async_runtime::block_on(async {
                let config_path = Config::default_path();
                let config = if config_path.exists() {
                    Config::load(&config_path).unwrap_or_default()
                } else {
                    let config = Config::default();
                    config.save(&config_path).ok();
                    config
                };

                let data_dir = dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".netfile")
                    .join("data");

                let download_dir = if config.transfer.download_dir.is_empty() {
                    dirs::download_dir()
                        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
                        .join("NetFile")
                } else {
                    PathBuf::from(&config.transfer.download_dir)
                };

                tokio::fs::create_dir_all(&data_dir).await.ok();
                tokio::fs::create_dir_all(&download_dir).await.ok();

                let speed_limit_bytes_per_sec = config.transfer.speed_limit_mbps as u64 * 1024 * 1024;

                let transfer_service = Arc::new(
                    TransferService::new_with_compression(
                        config.network.transfer_port,
                        config.transfer.max_concurrent,
                        config.transfer.chunk_size,
                        data_dir.clone(),
                        download_dir.clone(),
                        config.transfer.enable_compression,
                        speed_limit_bytes_per_sec,
                    )
                    .await
                    .expect("Failed to create transfer service"),
                );

                transfer_service.update_transfer_config(
                    download_dir,
                    config.transfer.chunk_size,
                    config.transfer.enable_compression,
                    speed_limit_bytes_per_sec,
                    config.transfer.require_confirmation,
                ).await;

                let message_store = transfer_service.message_store();
                let history_store = transfer_service.history_store();
                let transfer_port = transfer_service.local_port();

                let session_instance_id = Uuid::new_v4().to_string();

                let discovery_service = Arc::new(
                    DiscoveryService::new(
                        config.network.discovery_port,
                        config.instance.instance_id.clone(),
                        session_instance_id,
                        config.instance.device_name.clone(),
                        config.instance.instance_name.clone(),
                        transfer_port,
                        config.network.broadcast_interval,
                    )
                    .await
                    .expect("Failed to create discovery service"),
                );

                let _discovery_handle = {
                    let service = discovery_service.clone();
                    tokio::spawn(async move {
                        service.start().await;
                    })
                };

                let _transfer_handle = {
                    let service = transfer_service.clone();
                    tokio::spawn(async move {
                        service.start().await;
                    })
                };

                let signal_client: Arc<RwLock<Option<Arc<SignalClient>>>> =
                    Arc::new(RwLock::new(None));

                if !config.network.signal_server_addr.is_empty() {
                    let transfer_addr = discovery_service.get_my_public_transfer_addr().await
                        .unwrap_or_default();
                    let local_transfer_port = transfer_service.local_port();
                    let sc = SignalClient::new(
                        config.instance.instance_id.clone(),
                        config.instance.instance_name.clone(),
                        transfer_addr,
                        config.network.signal_server_addr.clone(),
                        local_transfer_port,
                        message_store.clone(),
                    );
                    if let Ok(()) = sc.connect().await {
                        let ts = transfer_service.clone();
                        sc.set_punch_handler(std::sync::Arc::new(move |addr| {
                            let ts = ts.clone();
                            tokio::spawn(async move { ts.punch_hole(addr).await; });
                        })).await;
                        spawn_stun_watcher(sc.clone(), discovery_service.clone());
                        *signal_client.write().await = Some(sc);
                    }
                }

                app.manage(AppState {
                    config: Arc::new(RwLock::new(config)),
                    discovery_service,
                    transfer_service,
                    message_store,
                    history_store,
                    signal_client,
                });

                Ok(())
            })
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
