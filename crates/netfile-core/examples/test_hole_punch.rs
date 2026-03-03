use netfile_core::UdpHolePuncher;
use std::net::SocketAddr;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        println!("Usage:");
        println!("  Server mode: {} server <port>", args[0]);
        println!("  Client mode: {} client <local_port> <peer_ip:port>", args[0]);
        return Ok(());
    }

    match args[1].as_str() {
        "server" => {
            let port: u16 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
            run_server(port).await?;
        }
        "client" => {
            let local_port: u16 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
            let peer_addr: SocketAddr = args
                .get(3)
                .and_then(|s| s.parse().ok())
                .expect("Invalid peer address");
            run_client(local_port, peer_addr).await?;
        }
        _ => {
            println!("Invalid mode. Use 'server' or 'client'");
        }
    }

    Ok(())
}

async fn run_server(port: u16) -> anyhow::Result<()> {
    println!("Starting UDP hole punch server...\n");

    let puncher = UdpHolePuncher::new(port).await?;
    let local_addr = puncher.local_addr()?;

    println!("✓ Server listening on: {}", local_addr);
    println!("  Waiting for punch requests...\n");

    let listen_handle = tokio::spawn(async move {
        if let Err(e) = puncher.listen_for_punches().await {
            eprintln!("Listen error: {}", e);
        }
    });

    listen_handle.await?;
    Ok(())
}

async fn run_client(local_port: u16, peer_addr: SocketAddr) -> anyhow::Result<()> {
    println!("Starting UDP hole punch client...\n");

    let puncher = UdpHolePuncher::new(local_port).await?;
    let local_addr = puncher.local_addr()?;

    println!("✓ Client bound to: {}", local_addr);
    println!("  Target peer: {}\n", peer_addr);

    println!("Attempting to punch hole...");
    let response = puncher.punch_hole(peer_addr).await?;

    if response.success {
        println!("✓ Hole punch successful!");
        println!("  Local address: {}", response.local_addr);
        println!("  Peer address: {}", response.peer_addr);

        println!("\nSending test message...");
        puncher.send_data(peer_addr, b"Hello from client!").await?;
        println!("✓ Test message sent");

        println!("\nWaiting for response...");
        match tokio::time::timeout(
            tokio::time::Duration::from_secs(5),
            puncher.recv_data(),
        )
        .await
        {
            Ok(Ok((addr, data))) => {
                println!("✓ Received response from {}: {:?}", addr, String::from_utf8_lossy(&data));
            }
            Ok(Err(e)) => {
                println!("✗ Error receiving response: {}", e);
            }
            Err(_) => {
                println!("✗ Timeout waiting for response");
            }
        }
    } else {
        println!("✗ Hole punch failed");
    }

    println!("\n✅ Test completed!");
    Ok(())
}
