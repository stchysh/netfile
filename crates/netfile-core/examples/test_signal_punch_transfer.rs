use anyhow::{Context, Result};
use netfile_core::{MessageStore, SignalClient, TransferService};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::sleep;

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

    let signal_addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:37200".to_string());

    println!("=== Punch Transfer Simulation (Single Host) ===");
    println!("Signal server: {}", signal_addr);
    println!("Mode: two peers on one machine (flow verification, not real dual-NAT)");

    let ts_millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let root = std::env::temp_dir().join(format!("netfile_punch_sim_{}", ts_millis));
    let a_data = root.join("a_data");
    let b_data = root.join("b_data");
    let a_download = root.join("a_download");
    let b_download = root.join("b_download");
    tokio::fs::create_dir_all(&a_data).await?;
    tokio::fs::create_dir_all(&b_data).await?;
    tokio::fs::create_dir_all(&a_download).await?;
    tokio::fs::create_dir_all(&b_download).await?;

    let peer_a = Arc::new(
        TransferService::new_with_compression(0, 2, 1024 * 1024, a_data.clone(), a_download, false, 0)
            .await
            .context("create transfer service A failed")?,
    );
    let peer_b = Arc::new(
        TransferService::new_with_compression(0, 2, 1024 * 1024, b_data.clone(), b_download.clone(), false, 0)
            .await
            .context("create transfer service B failed")?,
    );

    {
        let svc = peer_a.clone();
        tokio::spawn(async move { svc.start().await });
    }
    {
        let svc = peer_b.clone();
        tokio::spawn(async move { svc.start().await });
    }

    sleep(Duration::from_millis(800)).await;

    let a_addr = format!("127.0.0.1:{}", peer_a.local_port());
    let b_addr = format!("127.0.0.1:{}", peer_b.local_port());
    println!("Peer A transfer addr: {}", a_addr);
    println!("Peer B transfer addr: {}", b_addr);

    let a_store = Arc::new(MessageStore::new(a_data.clone()));
    let b_store = Arc::new(MessageStore::new(b_data.clone()));

    let sc_a = SignalClient::new(
        "peer-a".to_string(),
        "peer-a".to_string(),
        a_addr.clone(),
        signal_addr.clone(),
        peer_a.local_port(),
        a_store,
    );
    let sc_b = SignalClient::new(
        "peer-b".to_string(),
        "peer-b".to_string(),
        b_addr.clone(),
        signal_addr.clone(),
        peer_b.local_port(),
        b_store,
    );

    sc_a.update_nat_type("cone".to_string()).await;
    sc_b.update_nat_type("cone".to_string()).await;

    sc_a.connect().await.context("signal connect A failed")?;
    sc_b.connect().await.context("signal connect B failed")?;
    println!("Both peers connected to signal server");

    {
        let svc = peer_a.clone();
        sc_a
            .set_punch_handler(Arc::new(move |addr| {
                let svc = svc.clone();
                tokio::spawn(async move { svc.punch_hole(addr).await });
            }))
            .await;
    }
    {
        let svc = peer_b.clone();
        sc_b
            .set_punch_handler(Arc::new(move |addr| {
                let svc = svc.clone();
                tokio::spawn(async move { svc.punch_hole(addr).await });
            }))
            .await;
    }

    sc_a.update_transfer_addr(a_addr.clone()).await;
    sc_b.update_transfer_addr(b_addr.clone()).await;

    let code = sc_a.generate_invite().await.context("generate invite failed")?;
    sc_b
        .accept_invite(code)
        .await
        .context("accept invite failed")?;
    println!("Peers paired as friends");

    sleep(Duration::from_millis(500)).await;

    let peer_b_addr: std::net::SocketAddr = sc_a
        .request_punch("peer-b".to_string())
        .await
        .context("request_punch A->B failed")?
        .parse()
        .context("invalid peer addr from signal")?;
    println!("A got punch coordinate for B: {}", peer_b_addr);

    peer_a.punch_hole(peer_b_addr).await;
    sleep(Duration::from_secs(2)).await;

    let payload = root.join("payload_punch_test.txt");
    tokio::fs::write(&payload, b"hello-from-peer-a-via-punch").await?;

    let file_id = peer_a
        .send_file(payload.clone(), peer_b_addr)
        .await
        .context("send file via punched addr failed")?;
    println!("Transfer done, file_id={}", file_id);

    let recv_path = b_download.join(
        payload
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("payload_punch_test.txt"),
    );

    for _ in 0..20 {
        if recv_path.exists() {
            break;
        }
        sleep(Duration::from_millis(300)).await;
    }

    if !recv_path.exists() {
        anyhow::bail!("receiver file not found: {}", recv_path.display());
    }

    let sent = tokio::fs::read(&payload).await?;
    let recv = tokio::fs::read(&recv_path).await?;
    if sent != recv {
        anyhow::bail!("file content mismatch");
    }

    println!("PASS: received file verified at {}", recv_path.display());
    println!("Temp test dir: {}", root.display());
    println!("=== Done ===");
    Ok(())
}
