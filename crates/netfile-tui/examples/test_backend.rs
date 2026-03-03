use netfile_core::{Config, DiscoveryService, TransferService};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("Testing TUI interface...\n");

    let config = Config::default();

    let data_dir = PathBuf::from("/tmp/netfile_tui_test");
    let download_dir = PathBuf::from("/tmp/netfile_tui_download");

    tokio::fs::create_dir_all(&data_dir).await?;
    tokio::fs::create_dir_all(&download_dir).await?;

    let transfer_service = Arc::new(
        TransferService::new_with_compression(
            0,
            config.transfer.max_concurrent,
            config.transfer.chunk_size,
            data_dir,
            download_dir,
            config.transfer.enable_compression,
        )
        .await?,
    );

    let transfer_port = transfer_service.local_port();
    println!("✓ Transfer service started on port {}", transfer_port);

    let discovery_service = Arc::new(
        DiscoveryService::new(
            0,
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
    println!("✓ Discovery service started on port {}", discovery_port);

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

    println!("\nServices running for 5 seconds...");
    tokio::time::sleep(Duration::from_secs(5)).await;

    let devices = discovery_service.get_devices().await;
    println!("\n✓ Found {} devices", devices.len());

    let transfers = transfer_service.progress_tracker().list_all().await;
    println!("✓ Active transfers: {}", transfers.len());

    println!("\n✅ TUI backend test completed!");

    Ok(())
}
