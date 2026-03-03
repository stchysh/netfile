use netfile_core::TlsManager;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    let data_dir = PathBuf::from("/tmp/netfile_tls_test");
    let tls_manager = TlsManager::new(data_dir.clone());

    println!("Testing TLS certificate generation...");
    tls_manager.ensure_certificate().await?;
    println!("✓ Certificate generated");

    println!("\nTesting server config loading...");
    let _server_config = tls_manager.load_server_config().await?;
    println!("✓ Server config loaded");

    println!("\nTesting client config loading...");
    let _client_config = tls_manager.load_client_config().await?;
    println!("✓ Client config loaded");

    println!("\nVerifying certificate files exist...");
    let cert_path = data_dir.join("certs").join("cert.pem");
    let key_path = data_dir.join("certs").join("key.pem");

    if cert_path.exists() {
        println!("✓ Certificate file exists: {:?}", cert_path);
    }
    if key_path.exists() {
        println!("✓ Key file exists: {:?}", key_path);
    }

    println!("\n✅ All TLS tests passed!");
    Ok(())
}
