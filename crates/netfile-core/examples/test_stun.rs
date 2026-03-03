use netfile_core::StunClient;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    println!("Testing STUN client...\n");

    let client = StunClient::new();

    println!("Attempting to get public IP address...");
    match client.get_public_address().await {
        Ok(addr) => {
            println!("✓ Public address: {}", addr);
            println!("  - IP: {}", addr.ip());
            println!("  - Port: {}", addr.port());
        }
        Err(e) => {
            println!("✗ Failed to get public address: {}", e);
        }
    }

    println!("\nDetecting NAT type...");
    match client.detect_nat_type().await {
        Ok(nat_type) => {
            println!("✓ NAT type: {:?}", nat_type);
        }
        Err(e) => {
            println!("✗ Failed to detect NAT type: {}", e);
        }
    }

    println!("\n✅ STUN test completed!");
    Ok(())
}
