use netfile_core::{ChatMessage, Config, Device, DiscoveryService, HistoryStore, MessageStore, TransferProgress, TransferRecord, TransferService};
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
    public_addr: Option<String>,
    peer_discovery_addr: Option<String>,
) -> Result<(), String> {
    let addr = target_addr.parse().map_err(|e| format!("无效地址: {}", e))?;
    let path = PathBuf::from(&file_path);
    if !path.exists() {
        return Err(format!("文件不存在: {}", file_path));
    }
    let compression = enable_compression.unwrap_or(false);
    let fallback_addr = public_addr.as_deref().and_then(|s| s.parse().ok());

    if let Some(ref da) = peer_discovery_addr {
        if let Ok(disc_addr) = da.parse() {
            let ds = state.discovery_service.clone();
            tokio::spawn(async move {
                let _ = ds.send_punch(disc_addr).await;
            });
        }
    }

    if path.is_dir() {
        let service = state.transfer_service.clone();
        tokio::spawn(async move {
            if let Err(e) = service.send_folder_with_fallback(path, addr, fallback_addr, compression).await {
                tracing::error!("Failed to send folder: {}", e);
            }
        });
        Ok(())
    } else {
        state
            .transfer_service
            .send_file_with_fallback(path, addr, fallback_addr, compression)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
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
    let addr = target_addr.parse().map_err(|e| format!("无效地址: {}", e))?;
    let config = state.config.read().await;
    let from_instance_id = config.instance.instance_id.clone();
    let from_instance_name = config.instance.instance_name.clone();
    drop(config);
    state
        .transfer_service
        .send_text_message(&peer_instance_id, addr, content, from_instance_id, from_instance_name)
        .await
        .map_err(|e| e.to_string())
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

    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
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
            get_message_counts,
            get_transfer_history,
            clear_transfer_history,
            get_config,
            update_config,
            get_my_public_addr,
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

                let discovery_handle = {
                    let service = discovery_service.clone();
                    tokio::spawn(async move {
                        service.start().await;
                    })
                };

                let transfer_handle = {
                    let service = transfer_service.clone();
                    tokio::spawn(async move {
                        service.start().await;
                    })
                };

                app.manage(AppState {
                    config: Arc::new(RwLock::new(config)),
                    discovery_service,
                    transfer_service,
                    message_store,
                    history_store,
                });

                Ok(())
            })
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
