use clap::{Parser, Subcommand, ValueEnum};
use netfile_core::Config;
use std::path::PathBuf;
use std::net::SocketAddr;
use tracing_subscriber;

#[derive(Debug, Clone, ValueEnum)]
enum OutputFormat {
    Table,
    Json,
    Simple,
}

#[derive(Parser)]
#[command(name = "netfile")]
#[command(about = "内网文件传输工具", long_about = None)]
struct Cli {
    #[arg(long, help = "配置文件路径")]
    config: Option<PathBuf>,

    #[arg(long, help = "实例名称")]
    name: Option<String>,

    #[arg(long, help = "无 GUI 模式")]
    no_gui: bool,

    #[arg(long, short = 'o', help = "输出格式", value_enum, default_value = "table")]
    output: OutputFormat,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    #[command(about = "设备管理")]
    Devices {
        #[command(subcommand)]
        action: DeviceAction,
    },
    #[command(about = "发送文件")]
    Send {
        #[arg(help = "目标设备 IP:端口")]
        target: String,
        #[arg(help = "文件或文件夹路径")]
        path: PathBuf,
        #[arg(short, long, help = "递归发送文件夹")]
        recursive: bool,
    },
    #[command(about = "传输管理")]
    Transfers {
        #[command(subcommand)]
        action: TransferAction,
    },
    #[command(about = "配置管理")]
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    #[command(about = "授权管理")]
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },
}

#[derive(Subcommand)]
enum DeviceAction {
    #[command(about = "列出在线设备")]
    List,
    #[command(about = "显示设备详情")]
    Info {
        #[arg(help = "设备实例 ID")]
        instance_id: String,
    },
}

#[derive(Subcommand)]
enum TransferAction {
    #[command(about = "列出传输任务")]
    List,
    #[command(about = "显示传输详情")]
    Info {
        #[arg(help = "任务 ID")]
        task_id: String,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    #[command(about = "显示当前配置")]
    Show,
    #[command(about = "设置配置项")]
    Set {
        #[arg(help = "配置项名称（如 instance.name）")]
        key: String,
        #[arg(help = "配置项值")]
        value: String,
    },
    #[command(about = "重置配置")]
    Reset,
}

#[derive(Subcommand)]
enum AuthAction {
    #[command(about = "列出授权设备")]
    List,
    #[command(about = "添加授权设备")]
    Allow {
        #[arg(help = "设备 ID")]
        device_id: String,
    },
    #[command(about = "移除授权设备")]
    Deny {
        #[arg(help = "设备 ID")]
        device_id: String,
    },
    #[command(about = "设置密码")]
    SetPassword {
        #[arg(help = "新密码")]
        password: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    let cli = Cli::parse();

    let config_path = cli.config.unwrap_or_else(|| Config::default_path());

    let mut config = if config_path.exists() {
        Config::load(&config_path)?
    } else {
        let config = Config::default();
        config.save(&config_path)?;
        config
    };

    if let Some(name) = cli.name {
        config.instance.instance_name = name;
        config.save(&config_path)?;
    }

    match cli.command {
        Some(Commands::Devices { action }) => match action {
            DeviceAction::List => {
                list_devices(config, cli.output).await?;
            }
            DeviceAction::Info { instance_id } => {
                show_device_info(config, instance_id, cli.output).await?;
            }
        },
        Some(Commands::Send { target, path, recursive }) => {
            let addr: SocketAddr = target.parse()?;
            if path.is_dir() {
                if !recursive {
                    return Err(anyhow::anyhow!("发送文件夹需要使用 --recursive 参数"));
                }
                send_directory(config, path, addr).await?;
            } else {
                send_file(config, path, addr).await?;
            }
        },
        Some(Commands::Transfers { action }) => match action {
            TransferAction::List => {
                list_transfers(config, cli.output).await?;
            }
            TransferAction::Info { task_id } => {
                show_transfer_info(config, task_id, cli.output).await?;
            }
        },
        Some(Commands::Config { action }) => match action {
            ConfigAction::Show => {
                show_config(config, cli.output).await?;
            }
            ConfigAction::Set { key, value } => {
                set_config(config, config_path, key, value).await?;
            }
            ConfigAction::Reset => {
                reset_config(config_path).await?;
            }
        },
        Some(Commands::Auth { action }) => match action {
            AuthAction::List => {
                list_allowed_devices(config, cli.output).await?;
            }
            AuthAction::Allow { device_id } => {
                allow_device(config, config_path, device_id).await?;
            }
            AuthAction::Deny { device_id } => {
                deny_device(config, config_path, device_id).await?;
            }
            AuthAction::SetPassword { password } => {
                set_password(config, config_path, password).await?;
            }
        },
        None => {
            if cli.no_gui {
                run_cli_mode(config).await?;
            } else {
                println!("GUI 模式开发中...");
                run_cli_mode(config).await?;
            }
        }
    }

    Ok(())
}

async fn run_cli_mode(config: Config) -> anyhow::Result<()> {
    use netfile_core::{DiscoveryService, TransferService};
    use std::sync::Arc;

    println!("启动 NetFile CLI 模式...");
    println!("实例: {}", config.instance.instance_name);
    println!("设备: {}", config.instance.device_name);

    let data_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".netfile")
        .join("data");

    let download_dir = dirs::download_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
        .join("NetFile");

    tokio::fs::create_dir_all(&data_dir).await?;
    tokio::fs::create_dir_all(&download_dir).await?;

    let transfer_service = Arc::new(
        TransferService::new_with_compression(
            config.network.transfer_port,
            config.transfer.max_concurrent,
            config.transfer.chunk_size,
            data_dir,
            download_dir,
            config.transfer.enable_compression,
            config.transfer.speed_limit_mbps as u64 * 1024 * 1024,
        )
        .await?,
    );
    let transfer_port = transfer_service.local_port();

    let discovery_service = Arc::new(
        DiscoveryService::new(
            config.network.discovery_port,
            config.instance.device_name.clone(),
            config.instance.instance_id.clone(),
            config.instance.device_name.clone(),
            config.instance.instance_name.clone(),
            transfer_port,
            config.network.broadcast_interval,
        )
        .await?,
    );

    let discovery_port = discovery_service.local_port()?;
    println!("发现端口: {}", discovery_port);
    println!("传输端口: {}", transfer_port);
    println!("\n按 Ctrl+C 退出");

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

    tokio::signal::ctrl_c().await?;
    println!("\n正在关闭...");

    Ok(())
}

async fn send_file(config: Config, file_path: PathBuf, target: SocketAddr) -> anyhow::Result<()> {
    use netfile_core::{TransferService, TransferProgress};
    use std::sync::Arc;

    if !file_path.exists() {
        return Err(anyhow::anyhow!("文件不存在: {}", file_path.display()));
    }

    let metadata = tokio::fs::metadata(&file_path).await?;
    let file_size = metadata.len();

    println!("准备发送文件: {}", file_path.display());
    println!("文件大小: {}", TransferProgress::format_size(file_size));
    println!("目标地址: {}", target);

    let data_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".netfile")
        .join("data");

    let download_dir = dirs::download_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
        .join("NetFile");

    tokio::fs::create_dir_all(&data_dir).await?;

    let transfer_service = Arc::new(
        TransferService::new_with_compression(
            0,
            config.transfer.max_concurrent,
            config.transfer.chunk_size,
            data_dir,
            download_dir,
            config.transfer.enable_compression,
            config.transfer.speed_limit_mbps as u64 * 1024 * 1024,
        )
        .await?,
    );

    let progress_tracker = transfer_service.progress_tracker();

    println!("开始传输...\n");

    let service_clone = transfer_service.clone();
    let file_path_clone = file_path.clone();
    let transfer_task = tokio::spawn(async move {
        service_clone.send_file(file_path_clone, target).await
    });

    let mut last_display = String::new();
    loop {
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

        let progresses = progress_tracker.list_all().await;
        if let Some(progress) = progresses.first() {
            let display = progress.display();
            if display != last_display {
                print!("\r{}", display);
                use std::io::Write;
                std::io::stdout().flush().ok();
                last_display = display;
            }
        }

        if transfer_task.is_finished() {
            break;
        }
    }

    let file_id = transfer_task.await??;
    println!("\n传输完成! 文件 ID: {}", file_id);

    Ok(())
}

async fn send_directory(config: Config, dir_path: PathBuf, target: SocketAddr) -> anyhow::Result<()> {
    use netfile_core::{TransferService, scan_directory, calculate_total_size, count_files};
    use std::sync::Arc;

    if !dir_path.exists() || !dir_path.is_dir() {
        return Err(anyhow::anyhow!("目录不存在: {}", dir_path.display()));
    }

    println!("正在扫描目录: {}", dir_path.display());
    let entries = scan_directory(&dir_path).await?;
    let total_size = calculate_total_size(&entries);
    let file_count = count_files(&entries);

    println!("找到 {} 个文件，总大小: {} 字节", file_count, total_size);
    println!("目标地址: {}", target);

    let data_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".netfile")
        .join("data");

    let download_dir = dirs::download_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
        .join("NetFile");

    tokio::fs::create_dir_all(&data_dir).await?;

    let transfer_service = Arc::new(
        TransferService::new_with_compression(
            0,
            config.transfer.max_concurrent,
            config.transfer.chunk_size,
            data_dir,
            download_dir,
            config.transfer.enable_compression,
            config.transfer.speed_limit_mbps as u64 * 1024 * 1024,
        )
        .await?,
    );

    println!("开始传输文件夹...");
    let mut completed = 0;
    for entry in entries {
        if entry.is_dir {
            continue;
        }

        let file_path = dir_path.join(&entry.relative_path);
        let relative_path_str = entry.relative_path.to_string_lossy().to_string();

        println!("[{}/{}] 传输: {}", completed + 1, file_count, entry.relative_path.display());

        match transfer_service
            .send_file_with_relative_path(file_path, Some(relative_path_str), target)
            .await
        {
            Ok(_) => {
                completed += 1;
            }
            Err(e) => {
                eprintln!("传输失败: {} - {}", entry.relative_path.display(), e);
            }
        }
    }

    println!("文件夹传输完成! 成功: {}/{}", completed, file_count);

    Ok(())
}

async fn list_devices(config: Config, format: OutputFormat) -> anyhow::Result<()> {
    use netfile_core::DiscoveryService;
    use std::sync::Arc;

    let discovery_service = Arc::new(
        DiscoveryService::new(
            config.network.discovery_port,
            config.instance.device_name.clone(),
            config.instance.instance_id.clone(),
            config.instance.device_name.clone(),
            config.instance.instance_name.clone(),
            0,
            config.network.broadcast_interval,
        )
        .await?,
    );

    let service_clone = discovery_service.clone();
    let discovery_handle = tokio::spawn(async move {
        service_clone.start().await;
    });

    println!("正在扫描设备...");
    tokio::time::sleep(tokio::time::Duration::from_secs(6)).await;

    let devices = discovery_service.get_devices().await;

    if devices.is_empty() {
        println!("未发现在线设备");
    } else {
        match format {
            OutputFormat::Table => {
                println!("\n在线设备 ({}):", devices.len());
                println!("{:<20} {:<20} {:<15} {:<6}", "设备名", "实例名", "IP 地址", "端口");
                println!("{}", "-".repeat(65));
                for device in devices {
                    println!(
                        "{:<20} {:<20} {:<15} {:<6}",
                        device.device_name, device.instance_name, device.ip, device.port
                    );
                }
            }
            OutputFormat::Json => {
                let json = serde_json::to_string_pretty(&devices)?;
                println!("{}", json);
            }
            OutputFormat::Simple => {
                for device in devices {
                    println!("{} - {} ({}:{})",
                        device.device_name, device.instance_name, device.ip, device.port);
                }
            }
        }
    }

    discovery_handle.abort();
    Ok(())
}

async fn show_device_info(config: Config, instance_id: String, format: OutputFormat) -> anyhow::Result<()> {
    use netfile_core::DiscoveryService;
    use std::sync::Arc;

    let discovery_service = Arc::new(
        DiscoveryService::new(
            config.network.discovery_port,
            config.instance.device_name.clone(),
            config.instance.instance_id.clone(),
            config.instance.device_name.clone(),
            config.instance.instance_name.clone(),
            0,
            config.network.broadcast_interval,
        )
        .await?,
    );

    let service_clone = discovery_service.clone();
    let discovery_handle = tokio::spawn(async move {
        service_clone.start().await;
    });

    println!("正在查找设备...");
    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

    if let Some(device) = discovery_service.get_device(&instance_id).await {
        match format {
            OutputFormat::Table => {
                println!("\n设备详情:");
                println!("  设备名称: {}", device.device_name);
                println!("  实例名称: {}", device.instance_name);
                println!("  设备 ID: {}", device.device_id);
                println!("  实例 ID: {}", device.instance_id);
                println!("  IP 地址: {}", device.ip);
                println!("  传输端口: {}", device.port);
                println!("  协议版本: {}", device.version);
                println!("  在线状态: 在线");
            }
            OutputFormat::Json => {
                let json = serde_json::to_string_pretty(&device)?;
                println!("{}", json);
            }
            OutputFormat::Simple => {
                println!("{} - {} ({}:{})",
                    device.device_name, device.instance_name, device.ip, device.port);
            }
        }
    } else {
        println!("未找到设备: {}", instance_id);
    }

    discovery_handle.abort();
    Ok(())
}

async fn list_transfers(config: Config, format: OutputFormat) -> anyhow::Result<()> {
    use netfile_core::TransferService;
    use std::sync::Arc;

    let data_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".netfile")
        .join("data");

    let download_dir = dirs::download_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
        .join("NetFile");

    let transfer_service = Arc::new(
        TransferService::new_with_compression(
            0,
            config.transfer.max_concurrent,
            config.transfer.chunk_size,
            data_dir,
            download_dir,
            config.transfer.enable_compression,
            config.transfer.speed_limit_mbps as u64 * 1024 * 1024,
        )
        .await?,
    );

    let progress_tracker = transfer_service.progress_tracker();
    let progresses = progress_tracker.list_all().await;

    if progresses.is_empty() {
        println!("当前没有进行中的传输任务");
    } else {
        match format {
            OutputFormat::Table => {
                println!("\n传输任务 ({}):", progresses.len());
                println!("{:<40} {:<20} {:<10} {:<15}", "文件 ID", "文件名", "进度", "速度");
                println!("{}", "-".repeat(90));
                for progress in progresses {
                    println!(
                        "{:<40} {:<20} {:<10.1}% {:<15.2} MB/s",
                        progress.file_id,
                        progress.file_name,
                        progress.progress_percent(),
                        progress.speed_mbps()
                    );
                }
            }
            OutputFormat::Json => {
                let json = serde_json::to_string_pretty(&progresses)?;
                println!("{}", json);
            }
            OutputFormat::Simple => {
                for progress in progresses {
                    println!("{} - {:.1}% - {:.2} MB/s",
                        progress.file_name,
                        progress.progress_percent(),
                        progress.speed_mbps());
                }
            }
        }
    }

    Ok(())
}

async fn show_transfer_info(config: Config, task_id: String, format: OutputFormat) -> anyhow::Result<()> {
    use netfile_core::{TransferService, TransferProgress};
    use std::sync::Arc;

    let data_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".netfile")
        .join("data");

    let download_dir = dirs::download_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
        .join("NetFile");

    let transfer_service = Arc::new(
        TransferService::new_with_compression(
            0,
            config.transfer.max_concurrent,
            config.transfer.chunk_size,
            data_dir,
            download_dir,
            config.transfer.enable_compression,
            config.transfer.speed_limit_mbps as u64 * 1024 * 1024,
        )
        .await?,
    );

    let progress_tracker = transfer_service.progress_tracker();

    if let Some(progress) = progress_tracker.get_progress(&task_id).await {
        match format {
            OutputFormat::Table => {
                println!("\n传输任务详情:");
                println!("  文件 ID: {}", progress.file_id);
                println!("  文件名: {}", progress.file_name);
                println!("  总大小: {}", TransferProgress::format_size(progress.total_size));
                println!("  已传输: {}", TransferProgress::format_size(progress.transferred));
                println!("  进度: {:.1}%", progress.progress_percent());
                println!("  总块数: {}", progress.total_chunks);
                println!("  已完成块: {}", progress.completed_chunks);
                println!("  传输速度: {:.2} MB/s", progress.speed_mbps());
                println!("  预计剩余: {}", TransferProgress::format_duration(progress.eta()));
            }
            OutputFormat::Json => {
                let json = serde_json::to_string_pretty(&progress)?;
                println!("{}", json);
            }
            OutputFormat::Simple => {
                println!("{} - {:.1}% - {:.2} MB/s - ETA: {}",
                    progress.file_name,
                    progress.progress_percent(),
                    progress.speed_mbps(),
                    TransferProgress::format_duration(progress.eta()));
            }
        }
    } else {
        println!("未找到传输任务: {}", task_id);
    }

    Ok(())
}

async fn show_config(config: Config, format: OutputFormat) -> anyhow::Result<()> {
    match format {
        OutputFormat::Table => {
            println!("\n配置信息:");
            println!("\n[实例配置]");
            println!("  实例 ID: {}", config.instance.instance_id);
            println!("  实例名称: {}", config.instance.instance_name);
            println!("  设备名称: {}", config.instance.device_name);
            println!("\n[网络配置]");
            println!("  发现端口: {}", config.network.discovery_port);
            println!("  传输端口: {}", config.network.transfer_port);
            println!("  广播间隔: {} 秒", config.network.broadcast_interval);
            println!("\n[传输配置]");
            println!("  块大小: {} 字节", config.transfer.chunk_size);
            println!("  最大并发: {}", config.transfer.max_concurrent);
            println!("  启用压缩: {}", config.transfer.enable_compression);
            println!("\n[安全配置]");
            println!("  需要认证: {}", config.security.require_auth);
            println!("  密码: {}", if config.security.password.is_empty() { "(未设置)" } else { "******" });
        }
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(&config)?;
            println!("{}", json);
        }
        OutputFormat::Simple => {
            println!("{}", toml::to_string_pretty(&config)?);
        }
    }
    Ok(())
}

async fn set_config(
    mut config: Config,
    config_path: PathBuf,
    key: String,
    value: String,
) -> anyhow::Result<()> {
    let parts: Vec<&str> = key.split('.').collect();
    if parts.len() != 2 {
        return Err(anyhow::anyhow!(
            "配置项格式错误，应为 section.key（如 instance.name）"
        ));
    }

    let section = parts[0];
    let field = parts[1];

    match section {
        "instance" => match field {
            "name" => config.instance.instance_name = value.clone(),
            "device_name" => config.instance.device_name = value.clone(),
            _ => return Err(anyhow::anyhow!("未知配置项: {}", key)),
        },
        "network" => match field {
            "discovery_port" => {
                config.network.discovery_port = value.parse()?;
            }
            "transfer_port" => {
                config.network.transfer_port = value.parse()?;
            }
            "broadcast_interval" => {
                config.network.broadcast_interval = value.parse()?;
            }
            _ => return Err(anyhow::anyhow!("未知配置项: {}", key)),
        },
        "transfer" => match field {
            "chunk_size" => {
                config.transfer.chunk_size = value.parse()?;
            }
            "max_concurrent" => {
                config.transfer.max_concurrent = value.parse()?;
            }
            "enable_compression" => {
                config.transfer.enable_compression = value.parse()?;
            }
            _ => return Err(anyhow::anyhow!("未知配置项: {}", key)),
        },
        "security" => match field {
            "require_auth" => {
                config.security.require_auth = value.parse()?;
            }
            "password" => config.security.password = value.clone(),
            _ => return Err(anyhow::anyhow!("未知配置项: {}", key)),
        },
        _ => return Err(anyhow::anyhow!("未知配置节: {}", section)),
    }

    config.save(&config_path)?;
    println!("配置已更新: {} = {}", key, value);

    Ok(())
}

async fn reset_config(config_path: PathBuf) -> anyhow::Result<()> {
    let config = Config::default();
    config.save(&config_path)?;
    println!("配置已重置为默认值");
    Ok(())
}

async fn list_allowed_devices(config: Config, format: OutputFormat) -> anyhow::Result<()> {
    use netfile_core::AuthManager;

    let auth_manager = AuthManager::new(config);
    let devices = auth_manager.list_allowed_devices();

    if devices.is_empty() {
        println!("未设置授权设备列表（允许所有设备）");
    } else {
        match format {
            OutputFormat::Table => {
                println!("\n授权设备列表 ({}):", devices.len());
                println!("{:<40}", "设备 ID");
                println!("{}", "-".repeat(40));
                for device_id in devices {
                    println!("{:<40}", device_id);
                }
            }
            OutputFormat::Json => {
                let json = serde_json::to_string_pretty(&devices)?;
                println!("{}", json);
            }
            OutputFormat::Simple => {
                for device_id in devices {
                    println!("{}", device_id);
                }
            }
        }
    }

    Ok(())
}

async fn allow_device(
    mut config: Config,
    config_path: PathBuf,
    device_id: String,
) -> anyhow::Result<()> {
    use netfile_core::AuthManager;

    let mut auth_manager = AuthManager::new(config.clone());
    auth_manager.add_allowed_device(device_id.clone());
    auth_manager.save_config(&config_path)?;

    println!("已添加授权设备: {}", device_id);
    Ok(())
}

async fn deny_device(
    mut config: Config,
    config_path: PathBuf,
    device_id: String,
) -> anyhow::Result<()> {
    use netfile_core::AuthManager;

    let mut auth_manager = AuthManager::new(config.clone());
    auth_manager.remove_allowed_device(&device_id);
    auth_manager.save_config(&config_path)?;

    println!("已移除授权设备: {}", device_id);
    Ok(())
}

async fn set_password(
    mut config: Config,
    config_path: PathBuf,
    password: String,
) -> anyhow::Result<()> {
    use netfile_core::AuthManager;

    let hashed = AuthManager::hash_password(&password);
    config.security.password = hashed;
    config.save(&config_path)?;

    println!("密码已设置");
    Ok(())
}
