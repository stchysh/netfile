use netfile_core::{
    compute_file_sha256, generate_random_name, BookmarkEntry, BookmarkStore, ChatMessage, Config,
    ConversationDelta, Device, DiscoveryService, FriendInfo, HistoryStore, IrohManager,
    MessageStore, ShareEntry, ShareStore, SignalClient, SignalStatus, TransferProgress,
    TransferRecord, TransferService,
};
use netfile_core::protocol::ShareListResponse;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
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
    pub share_store: Arc<ShareStore>,
    pub bookmark_store: Arc<BookmarkStore>,
    pub signal_client: Arc<RwLock<Option<Arc<SignalClient>>>>,
    pub iroh_manager: Arc<IrohManager>,
    pub iroh_watcher: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

fn is_lan_addr(addr: SocketAddr) -> bool {
    match addr.ip() {
        IpAddr::V4(ip) => ip.is_private() || ip.is_loopback() || ip.is_link_local(),
        IpAddr::V6(ip) => ip.is_loopback() || ip.is_unicast_link_local() || ip.is_unique_local(),
    }
}

async fn try_direct_transfer(
    service: &Arc<TransferService>,
    path: &PathBuf,
    candidate: SocketAddr,
    compression: bool,
    path_for_log: &str,
) -> bool {
    tracing::info!("trying direct transfer to {} for {}", candidate, path_for_log);
    let result = if path.is_dir() {
        service.send_folder(path.clone(), candidate, compression).await.map(|_| ())
    } else {
        service.send_file_compressed(path.clone(), candidate, compression).await.map(|_| ())
    };
    match result {
        Ok(()) => {
            tracing::info!("direct transfer succeeded via {} for {}", candidate, path_for_log);
            true
        }
        Err(e) => {
            tracing::warn!("direct transfer failed via {} for {}: {}", candidate, path_for_log, e);
            false
        }
    }
}

async fn try_iroh_transfer(
    service: &Arc<TransferService>,
    path: &PathBuf,
    iroh_addr_json: &str,
    compression: bool,
    path_for_log: &str,
) -> bool {
    tracing::info!("trying iroh transfer for {}", path_for_log);
    let result = if path.is_dir() {
        service.send_folder_via_iroh_str(path.clone(), iroh_addr_json, compression).await
    } else {
        service.send_via_iroh_str(path.clone(), iroh_addr_json, compression).await.map(|_| ())
    };
    match result {
        Ok(()) => {
            tracing::info!("iroh transfer succeeded for {}", path_for_log);
            true
        }
        Err(e) => {
            tracing::warn!("iroh transfer failed for {}: {}", path_for_log, e);
            false
        }
    }
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
    let addr: Option<SocketAddr> = target_addr.parse().ok();
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

    let lan_addr = addr.filter(|candidate| is_lan_addr(*candidate));

    let service = state.transfer_service.clone();
    let signal_client = state.signal_client.clone();
    let peer_device_id = peer_device_id.clone();
    let path_for_log = path.display().to_string();

    tokio::spawn(async move {
        tracing::info!("send_file dispatch: path={:?} lan_addr={:?} peer_device_id={:?} compression={}", path_for_log, lan_addr, peer_device_id, compression);
        let mut direct_ok = false;

        if let Some(candidate) = lan_addr {
            tracing::info!("trying LAN transfer to {}", candidate);
            direct_ok = try_direct_transfer(&service, &path, candidate, compression, &path_for_log).await;
        } else {
            tracing::info!("no LAN addr available, skipping direct transfer");
        }

        if !direct_ok {
            if let Some(device_id) = peer_device_id.as_deref() {
                let sc_guard = signal_client.read().await;
                if let Some(sc) = sc_guard.as_ref() {
                    if let Some(iroh_addr_json) = sc.get_peer_iroh_addr(device_id).await {
                        tracing::info!("trying iroh transfer for peer_device_id={}", device_id);
                        direct_ok = try_iroh_transfer(&service, &path, &iroh_addr_json, compression, &path_for_log).await;
                    } else {
                        tracing::warn!("no iroh_addr for peer_device_id={}, cannot fallback to iroh", device_id);
                    }
                } else {
                    tracing::warn!("signal client not connected, cannot get iroh_addr for peer_device_id={:?}", peer_device_id);
                }
            } else {
                tracing::warn!("no peer_device_id provided, cannot try iroh fallback");
            }
        }

        if !direct_ok {
            tracing::error!("all transfer paths failed for {}", path_for_log);
        }
    });
    Ok(())
}

#[tauri::command]
async fn get_my_public_addr(_state: State<'_, AppState>) -> Result<Option<String>, String> {
    Ok(None)
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
async fn confirm_transfer_save_as(
    state: State<'_, AppState>,
    file_id: String,
    save_path: String,
) -> Result<(), String> {
    let path = PathBuf::from(&save_path);
    if !path.exists() {
        tokio::fs::create_dir_all(&path).await.map_err(|e| format!("{}", e))?;
    }
    state.transfer_service.confirm_transfer_save_as(&file_id, path).await;
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
                .as_millis() as u64;
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
                    local_seq: 0,
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
async fn get_conversation_delta(
    state: State<'_, AppState>,
    peer_instance_id: String,
    cursor: u64,
) -> Result<ConversationDelta, String> {
    state
        .message_store
        .load_conversation_delta(&peer_instance_id, cursor)
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
fn open_file(path: String) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/c", "start", "", &path])
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(&path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn open_folder(path: String) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(&path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(&path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
async fn clear_transfer_history(state: State<'_, AppState>) -> Result<(), String> {
    state
        .history_store
        .clear_history()
        .await
        .map_err(|e| e.to_string())?;
    state
        .share_store
        .clear_all()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_share_entries(state: State<'_, AppState>) -> Result<Vec<ShareEntry>, String> {
    Ok(state.share_store.load_entries().await)
}

#[tauri::command]
async fn set_share_excluded(
    state: State<'_, AppState>,
    record_id: String,
    excluded: bool,
) -> Result<(), String> {
    state.share_store.set_excluded(&record_id, excluded).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn update_share_tags(
    state: State<'_, AppState>,
    record_id: String,
    tags: Vec<String>,
) -> Result<(), String> {
    state.share_store.update_tags(&record_id, tags).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn update_share_remark(
    state: State<'_, AppState>,
    record_id: String,
    remark: String,
) -> Result<(), String> {
    state.share_store.update_remark(&record_id, remark).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn query_device_shares(
    state: State<'_, AppState>,
    transfer_addr: String,
) -> Result<ShareListResponse, String> {
    state.transfer_service.query_device_shares(&transfer_addr).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn query_all_shares(state: State<'_, AppState>) -> Result<Vec<serde_json::Value>, String> {
    let mut results = Vec::new();

    // Include local device's shared files directly from ShareStore
    {
        let config = state.config.read().await;
        if config.transfer.enable_sharing {
            let entries = state.share_store.get_shared_entries().await;
            let require_confirm = config.transfer.sharing_require_confirm;
            let local_files: Vec<serde_json::Value> = entries.into_iter().map(|e| {
                serde_json::json!({
                    "file_id": e.record_id,
                    "file_name": e.file_name,
                    "file_size": e.file_size,
                    "file_md5": e.file_md5,
                    "tags": e.tags,
                    "remark": e.remark,
                    "download_count": e.download_count,
                    "require_confirm": require_confirm,
                    "timestamp": e.timestamp,
                })
            }).collect();
            results.push(serde_json::json!({
                "instance_id": config.instance.instance_id,
                "instance_name": format!("{} (本机)", config.instance.instance_name),
                "transfer_addr": format!("127.0.0.1:{}", state.transfer_service.local_port()),
                "require_confirm": require_confirm,
                "files": local_files,
                "loaded": true,
                "is_self": true,
            }));
        }
    }

    let devices = state.discovery_service.get_devices().await;
    for device in devices {
        if device.is_self {
            continue;
        }
        let addr = format!("{}:{}", device.ip, device.port);
        match state.transfer_service.query_device_shares(&addr).await {
            Ok(resp) => {
                results.push(serde_json::json!({
                    "instance_id": device.instance_id,
                    "instance_name": device.instance_name,
                    "transfer_addr": addr,
                    "require_confirm": resp.require_confirm,
                    "files": resp.entries,
                    "loaded": true,
                }));
            }
            Err(e) => {
                results.push(serde_json::json!({
                    "instance_id": device.instance_id,
                    "instance_name": device.instance_name,
                    "transfer_addr": addr,
                    "require_confirm": false,
                    "files": [],
                    "loaded": false,
                    "error": e.to_string(),
                }));
            }
        }
    }
    Ok(results)
}

#[tauri::command]
async fn get_bookmarks(state: State<'_, AppState>) -> Result<Vec<BookmarkEntry>, String> {
    Ok(state.bookmark_store.get_bookmarks().await)
}

#[tauri::command]
async fn add_bookmark(
    state: State<'_, AppState>,
    entry: BookmarkEntry,
) -> Result<(), String> {
    state.bookmark_store.add_bookmark(entry).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn remove_bookmark(
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    state.bookmark_store.remove_bookmark(&id).await.map_err(|e| e.to_string())
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
            config.transfer.iroh_stream_count,
        )
        .await;
    state
        .transfer_service
        .update_max_concurrent(config.transfer.max_concurrent)
        .await;

    state
        .transfer_service
        .update_sharing_config(
            config.instance.instance_name.clone(),
            config.transfer.enable_sharing,
            config.transfer.sharing_require_confirm,
        )
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
            let transfer_port = state.transfer_service.local_port();
            let sc = SignalClient::new(device_id, instance_name, format!("0.0.0.0:{}", transfer_port), new_signal_addr, message_store);
            if let Err(e) = sc.connect().await {
                tracing::warn!("Signal connect failed: {}", e);
            } else {
                let iroh_mgr = state.iroh_manager.clone();
                let sc_clone = sc.clone();
                *sc_guard = Some(sc);
                let handle = spawn_iroh_addr_watcher(sc_clone, iroh_mgr);
                let mut watcher = state.iroh_watcher.lock().await;
                if let Some(old) = watcher.take() { old.abort(); }
                *watcher = Some(handle);
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
    let transfer_port = state.transfer_service.local_port();
    let sc = SignalClient::new(
        config.instance.instance_id.clone(),
        config.instance.instance_name.clone(),
        format!("0.0.0.0:{}", transfer_port),
        server_addr,
        message_store,
    );
    sc.connect().await.map_err(|e| e.to_string())?;
    let iroh_mgr = state.iroh_manager.clone();
    let sc_clone = sc.clone();
    let mut guard = state.signal_client.write().await;
    if let Some(old) = guard.take() {
        old.disconnect().await;
    }
    *guard = Some(sc);
    drop(guard);
    let handle = spawn_iroh_addr_watcher(sc_clone, iroh_mgr);
    let mut watcher = state.iroh_watcher.lock().await;
    if let Some(old) = watcher.take() { old.abort(); }
    *watcher = Some(handle);
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
        .as_millis() as u64;
    let guard = state.signal_client.read().await;
    match guard.as_ref() {
        Some(sc) => sc.send_relay_message(&to_device_id, content, ts).await.map_err(|e| e.to_string()),
        None => Err("未连接到信令服务器".to_string()),
    }
}

fn spawn_iroh_addr_watcher(sc: Arc<SignalClient>, iroh_manager: Arc<IrohManager>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        tracing::info!("iroh addr watcher started");
        iroh_manager.endpoint_ref().online().await;
        let addr = iroh_manager.endpoint_addr();
        let mut last_addr_json = String::new();
        if let Ok(addr_json) = serde_json::to_string(&addr) {
            tracing::info!("initial iroh addr update");
            sc.update_iroh_addr(addr_json.clone()).await;
            last_addr_json = addr_json;
        }

        loop {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            let addr = iroh_manager.endpoint_addr();
            if let Ok(addr_json) = serde_json::to_string(&addr) {
                if addr_json != last_addr_json {
                    tracing::debug!("iroh addr changed, updating");
                    sc.update_iroh_addr(addr_json.clone()).await;
                    last_addr_json = addr_json;
                }
            }
        }
    })
}

fn cleanup_old_logs(log_dir: &std::path::Path, max_files: usize) {
    let Ok(entries) = std::fs::read_dir(log_dir) else { return };
    let mut files: Vec<_> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("netfile.log"))
        .collect();
    files.sort_by_key(|e| e.file_name());
    if files.len() > max_files {
        for old in &files[..files.len() - max_files] {
            std::fs::remove_file(old.path()).ok();
        }
    }
}

fn init_logging() -> tracing_appender::non_blocking::WorkerGuard {
    use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

    let log_dir = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".netfile")
        .join("logs");

    std::fs::create_dir_all(&log_dir).ok();
    cleanup_old_logs(&log_dir, 14);

    let file_appender = tracing_appender::rolling::daily(&log_dir, "netfile.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(fmt::layer().with_writer(non_blocking).with_ansi(false))
        .init();

    guard
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _log_guard = init_logging();

    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    tauri::Builder::default()
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
            confirm_transfer_save_as,
            reject_transfer,
            send_text_message,
            get_conversation,
            get_conversation_delta,
            get_random_name,
            get_message_counts,
            get_transfer_history,
            clear_transfer_history,
            open_file,
            open_folder,
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
            get_share_entries,
            set_share_excluded,
            update_share_tags,
            update_share_remark,
            query_device_shares,
            query_all_shares,
            get_bookmarks,
            add_bookmark,
            remove_bookmark,
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
                        config.transfer.quic_stream_window_mb,
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
                    config.transfer.iroh_stream_count,
                ).await;

                transfer_service.update_sharing_config(
                    config.instance.instance_name.clone(),
                    config.transfer.enable_sharing,
                    config.transfer.sharing_require_confirm,
                ).await;

                let iroh_manager = transfer_service.iroh_manager();
                let message_store = transfer_service.message_store();
                let history_store = transfer_service.history_store();
                let share_store = transfer_service.share_store();
                let bookmark_store = Arc::new(BookmarkStore::new(data_dir.clone()));
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
                let iroh_watcher: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>> =
                    Arc::new(tokio::sync::Mutex::new(None));

                if !config.network.signal_server_addr.is_empty() {
                    let sc = SignalClient::new(
                        config.instance.instance_id.clone(),
                        config.instance.instance_name.clone(),
                        format!("0.0.0.0:{}", transfer_port),
                        config.network.signal_server_addr.clone(),
                        message_store.clone(),
                    );
                    let sc_clone = sc.clone();
                    let iroh_manager_clone = iroh_manager.clone();
                    let signal_client_clone = signal_client.clone();
                    let iroh_watcher_clone = iroh_watcher.clone();
                    tokio::spawn(async move {
                        if let Ok(()) = sc_clone.connect().await {
                            let handle = spawn_iroh_addr_watcher(sc_clone.clone(), iroh_manager_clone);
                            *iroh_watcher_clone.lock().await = Some(handle);
                            *signal_client_clone.write().await = Some(sc_clone);
                        }
                    });
                }

                // Startup share sync: 3 seconds after launch, sync share list with history
                {
                    let share_store_bg = share_store.clone();
                    let history_store_bg = history_store.clone();
                    let enable_sharing = config.transfer.enable_sharing;
                    let instance_name = config.instance.instance_name.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
                        if !enable_sharing {
                            return;
                        }
                        let records = history_store_bg.load_history().await;
                        let _ = share_store_bg.sync_from_history(&records, &instance_name).await;
                        // Compute hashes for entries that don't have one yet
                        let entries = share_store_bg.load_entries().await;
                        for entry in entries {
                            if entry.file_md5.is_some() {
                                continue;
                            }
                            let path = std::path::PathBuf::from(&entry.save_path);
                            let store = share_store_bg.clone();
                            let rid = entry.record_id.clone();
                            tokio::spawn(async move {
                                if let Ok(hash) = compute_file_sha256(&path).await {
                                    let _ = store.update_md5(&rid, hash).await;
                                }
                            });
                        }
                    });
                }

                app.manage(AppState {
                    config: Arc::new(RwLock::new(config)),
                    discovery_service,
                    transfer_service,
                    message_store,
                    history_store,
                    share_store,
                    bookmark_store,
                    signal_client,
                    iroh_manager,
                    iroh_watcher,
                });

                Ok(())
            })
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
