use netfile_core::{Config, Device, DiscoveryService, TransferProgress, TransferService};
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{Manager, State};
use tokio::sync::RwLock;
use uuid::Uuid;

pub struct AppState {
    pub config: Arc<RwLock<Config>>,
    pub discovery_service: Arc<DiscoveryService>,
    pub transfer_service: Arc<TransferService>,
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
) -> Result<(), String> {
    use std::io::Write;
    let log_path = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("netfile_debug.log");
    let mut log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .ok();
    let mut wlog = |s: &str| {
        if let Some(ref mut f) = log_file {
            let _ = writeln!(f, "{}", s);
        }
    };

    wlog(&format!("=== send_file called ==="));
    wlog(&format!("target_addr: {}", target_addr));
    wlog(&format!("file_path: {}", file_path));

    let addr = match target_addr.parse() {
        Ok(a) => a,
        Err(e) => {
            let msg = format!("无效地址: {}", e);
            wlog(&format!("ERROR: {}", msg));
            return Err(msg);
        }
    };

    let path = PathBuf::from(&file_path);
    if !path.exists() {
        let msg = format!("文件不存在: {}", file_path);
        wlog(&format!("ERROR: {}", msg));
        return Err(msg);
    }

    wlog("文件存在，开始传输...");

    let result = state
        .transfer_service
        .send_file_compressed(path, addr, enable_compression.unwrap_or(false))
        .await
        .map(|_| ())
        .map_err(|e| e.to_string());

    match &result {
        Ok(_) => wlog("传输成功"),
        Err(e) => wlog(&format!("传输失败: {}", e)),
    }

    result
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
        .map_err(|e| format!("Failed to save config: {}", e))
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
            get_config,
            update_config,
        ])
        .setup(|app| {
            tauri::async_runtime::block_on(async {
                {
                    use std::io::Write;
                    let log_path = dirs::home_dir()
                        .unwrap_or_else(|| PathBuf::from("."))
                        .join("netfile_debug.log");
                    if let Ok(mut f) = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&log_path)
                    {
                        let _ = writeln!(f, "=== App started ===");
                    }
                }

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

                let download_dir = dirs::download_dir()
                    .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
                    .join("NetFile");

                tokio::fs::create_dir_all(&data_dir).await.ok();
                tokio::fs::create_dir_all(&download_dir).await.ok();

                let transfer_service = Arc::new(
                    TransferService::new_with_compression(
                        config.network.transfer_port,
                        config.transfer.max_concurrent,
                        config.transfer.chunk_size,
                        data_dir.clone(),
                        download_dir,
                        config.transfer.enable_compression,
                    )
                    .await
                    .expect("Failed to create transfer service"),
                );

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
                });

                Ok(())
            })
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
