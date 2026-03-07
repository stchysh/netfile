use anyhow::{Context, Result};
use netfile_core::{MessageStore, SignalClient, TransferService};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::{sleep, Instant};
use tracing::{info, warn};

#[derive(Clone, Copy)]
enum Role {
    Initiator,
    Receiver,
}

struct Settings {
    role: Role,
    signal_addr: String,
    device_id: String,
    target_device_id: String,
    transfer_port: u16,
    transfer_addr: String,
    data_root: PathBuf,
    payload_name: String,
    payload_content: String,
    wait_timeout_secs: u64,
    start_delay_secs: u64,
    nat_type: String,
}

impl Settings {
    fn from_env() -> Result<Self> {
        let role_raw = std::env::var("ROLE").unwrap_or_else(|_| "receiver".to_string());
        let role = match role_raw.to_ascii_lowercase().as_str() {
            "initiator" | "a" | "sender" => Role::Initiator,
            "receiver" | "b" => Role::Receiver,
            other => anyhow::bail!("invalid ROLE={other}, expected initiator/receiver"),
        };

        let signal_addr = std::env::var("SIGNAL_ADDR").unwrap_or_else(|_| "signal:37200".to_string());
        let nat_type = std::env::var("NAT_TYPE").unwrap_or_else(|_| "cone".to_string());
        let payload_name =
            std::env::var("PAYLOAD_NAME").unwrap_or_else(|_| "dual_nat_payload.txt".to_string());
        let payload_content = std::env::var("PAYLOAD_CONTENT")
            .unwrap_or_else(|_| "hello-from-peer-a-through-double-nat".to_string());

        let wait_timeout_secs = std::env::var("WAIT_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(90);

        let default_start_delay = match role {
            Role::Initiator => 5,
            Role::Receiver => 0,
        };
        let start_delay_secs = std::env::var("START_DELAY_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(default_start_delay);

        let (default_device_id, default_target_id, default_port) = match role {
            Role::Initiator => ("peer-a", "peer-b", 37050),
            Role::Receiver => ("peer-b", "peer-a", 37051),
        };

        let device_id = std::env::var("DEVICE_ID").unwrap_or_else(|_| default_device_id.to_string());
        let target_device_id =
            std::env::var("TARGET_DEVICE_ID").unwrap_or_else(|_| default_target_id.to_string());

        let transfer_port = std::env::var("TRANSFER_PORT")
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(default_port);

        let transfer_addr = std::env::var("TRANSFER_ADDR").unwrap_or_else(|_| "".to_string());
        if transfer_addr.is_empty() {
            anyhow::bail!("TRANSFER_ADDR must be set, e.g. 172.20.0.10:37050");
        }

        let data_root = std::env::var("DATA_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(format!("/tmp/netfile_dual_nat_{}", device_id)));

        Ok(Self {
            role,
            signal_addr,
            device_id,
            target_device_id,
            transfer_port,
            transfer_addr,
            data_root,
            payload_name,
            payload_content,
            wait_timeout_secs,
            start_delay_secs,
            nat_type,
        })
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("failed to install rustls ring provider"))?;

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let settings = Settings::from_env()?;
    let role_name = match settings.role {
        Role::Initiator => "initiator",
        Role::Receiver => "receiver",
    };
    info!(
        "dual-nat peer start: role={}, device_id={}, target_device_id={}, signal_addr={}, transfer_addr={}",
        role_name,
        settings.device_id,
        settings.target_device_id,
        settings.signal_addr,
        settings.transfer_addr
    );

    let data_dir = settings.data_root.join("data");
    let download_dir = settings.data_root.join("download");
    tokio::fs::create_dir_all(&data_dir).await?;
    tokio::fs::create_dir_all(&download_dir).await?;

    let transfer_service = Arc::new(
        TransferService::new_with_compression(
            settings.transfer_port,
            2,
            1024 * 1024,
            data_dir.clone(),
            download_dir.clone(),
            false,
            0,
        )
        .await
        .context("create transfer service failed")?,
    );

    {
        let svc = transfer_service.clone();
        tokio::spawn(async move { svc.start().await });
    }
    sleep(Duration::from_millis(800)).await;

    let message_store = Arc::new(MessageStore::new(data_dir.clone()));
    let signal_client = SignalClient::new(
        settings.device_id.clone(),
        settings.device_id.clone(),
        settings.transfer_addr.clone(),
        settings.signal_addr.clone(),
        transfer_service.local_port(),
        message_store,
    );

    signal_client.update_nat_type(settings.nat_type.clone()).await;
    {
        let svc = transfer_service.clone();
        signal_client
            .set_punch_handler(Arc::new(move |addr| {
                let svc = svc.clone();
                tokio::spawn(async move {
                    svc.punch_hole(addr).await;
                });
            }))
            .await;
    }

    signal_client
        .connect()
        .await
        .context("connect signal server failed")?;
    signal_client
        .update_transfer_addr(settings.transfer_addr.clone())
        .await;

    if settings.start_delay_secs > 0 {
        info!("startup delay {}s", settings.start_delay_secs);
        sleep(Duration::from_secs(settings.start_delay_secs)).await;
    }

    match settings.role {
        Role::Initiator => run_initiator(&settings, &signal_client, &transfer_service, &data_dir).await?,
        Role::Receiver => run_receiver(&settings, &download_dir).await?,
    }

    Ok(())
}

async fn run_initiator(
    settings: &Settings,
    signal_client: &Arc<SignalClient>,
    transfer_service: &Arc<TransferService>,
    data_dir: &PathBuf,
) -> Result<()> {
    let peer_addr_str =
        request_punch_with_retry(signal_client, &settings.target_device_id, settings.wait_timeout_secs)
            .await?;
    let peer_addr: SocketAddr = peer_addr_str
        .parse()
        .with_context(|| format!("invalid peer addr from signal: {}", peer_addr_str))?;

    info!("initiator got peer addr: {}", peer_addr);
    transfer_service.punch_hole(peer_addr).await;
    sleep(Duration::from_secs(2)).await;

    let payload_path = data_dir.join(&settings.payload_name);
    tokio::fs::write(&payload_path, settings.payload_content.as_bytes()).await?;

    let mut last_err = None;
    for attempt in 1..=3 {
        info!("send attempt {}/3 to {}", attempt, peer_addr);
        match transfer_service.send_file(payload_path.clone(), peer_addr).await {
            Ok(file_id) => {
                info!("PASS: send_file succeeded, file_id={}", file_id);
                println!("PASS: double NAT punch + transfer succeeded, file_id={file_id}");
                return Ok(());
            }
            Err(e) => {
                warn!("send attempt {} failed: {}", attempt, e);
                last_err = Some(e);
                let _ = transfer_service.punch_hole(peer_addr).await;
                sleep(Duration::from_secs(2)).await;
            }
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("send failed with unknown error")))
}

async fn request_punch_with_retry(
    signal_client: &Arc<SignalClient>,
    target_device_id: &str,
    wait_timeout_secs: u64,
) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(wait_timeout_secs);
    let mut attempt = 0usize;
    loop {
        attempt += 1;
        match signal_client.request_punch(target_device_id.to_string()).await {
            Ok(peer_addr) => return Ok(peer_addr),
            Err(e) => {
                if Instant::now() >= deadline {
                    return Err(e).context("request_punch timed out waiting for target peer");
                }
                warn!(
                    "request_punch attempt {} failed: {}. retrying...",
                    attempt,
                    e
                );
                sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

async fn run_receiver(settings: &Settings, download_dir: &PathBuf) -> Result<()> {
    let expected_path = download_dir.join(&settings.payload_name);
    let expected = settings.payload_content.as_bytes();
    let deadline = Instant::now() + Duration::from_secs(settings.wait_timeout_secs);

    info!(
        "receiver waiting file: {} (timeout={}s)",
        expected_path.display(),
        settings.wait_timeout_secs
    );

    loop {
        if expected_path.exists() {
            let got = tokio::fs::read(&expected_path).await?;
            if got == expected {
                info!("PASS: receiver verified payload");
                println!(
                    "PASS: receiver got expected payload at {}",
                    expected_path.display()
                );
                return Ok(());
            }
            anyhow::bail!(
                "receiver payload mismatch at {}",
                expected_path.display()
            );
        }

        if Instant::now() >= deadline {
            anyhow::bail!(
                "receiver timeout, file not found: {}",
                expected_path.display()
            );
        }

        sleep(Duration::from_millis(400)).await;
    }
}
