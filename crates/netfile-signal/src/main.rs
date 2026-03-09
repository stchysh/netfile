mod protocol;
mod server;
mod stun_server;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "0.0.0.0")]
    host: String,
    #[arg(long, default_value_t = 37200)]
    port: u16,
    #[arg(long)]
    relay_port: Option<u16>,
    #[arg(long)]
    stun_port: Option<u16>,
    #[arg(long)]
    stun_public_ip: Option<String>,
    #[arg(long)]
    iroh_relay_url: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("debug")),
        )
        .init();
    let args = Args::parse();

    let relay_addr = if let Some(relay_port) = args.relay_port {
        let addr = format!("{}:{}", args.host, relay_port);
        let relay_listener = TcpListener::bind(&addr).await?;
        tracing::info!("Relay listener started on {}", addr);

        let waiting: Arc<Mutex<HashMap<String, tokio::net::TcpStream>>> =
            Arc::new(Mutex::new(HashMap::new()));

        tokio::spawn(async move {
            loop {
                match relay_listener.accept().await {
                    Ok((stream, peer)) => {
                        tracing::debug!("Relay connection from {}", peer);
                        let waiting = waiting.clone();
                        tokio::spawn(async move {
                            handle_relay_connection(stream, waiting).await;
                        });
                    }
                    Err(e) => {
                        tracing::error!("Relay accept error: {}", e);
                    }
                }
            }
        });
        Some(addr)
    } else {
        None
    };

    let stun_addr = if let Some(stun_port) = args.stun_port {
        let bind_addr: SocketAddr = format!("0.0.0.0:{}", stun_port).parse()?;
        tokio::spawn(stun_server::run_stun_server(bind_addr));
        let public_ip = args.stun_public_ip.unwrap_or_else(|| args.host.clone());
        let addr_str = format!("{}:{}", public_ip, stun_port);
        tracing::info!("STUN service addr announced as {}", addr_str);
        Some(addr_str)
    } else {
        None
    };

    let state = server::ServerState::new_full(relay_addr, stun_addr, args.iroh_relay_url);

    {
        let state = state.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(300));
            loop {
                interval.tick().await;
                state.cleanup_expired_invites().await;
            }
        });
    }

    let addr = format!("{}:{}", args.host, args.port);
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("Signal server listening on {}", addr);

    loop {
        let (stream, peer) = listener.accept().await?;
        tracing::debug!("New connection from {}", peer);
        let state = state.clone();
        tokio::spawn(async move {
            server::handle_connection(state, stream).await;
        });
    }
}

async fn handle_relay_connection(
    mut stream: tokio::net::TcpStream,
    waiting: Arc<Mutex<HashMap<String, tokio::net::TcpStream>>>,
) {
    let mut len_buf = [0u8; 4];
    if stream.read_exact(&mut len_buf).await.is_err() {
        return;
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 || len > 256 {
        return;
    }
    let mut key_buf = vec![0u8; len];
    if stream.read_exact(&mut key_buf).await.is_err() {
        return;
    }
    let session_key = match String::from_utf8(key_buf) {
        Ok(k) => k,
        Err(_) => return,
    };

    let mut map = waiting.lock().await;
    if let Some(peer) = map.remove(&session_key) {
        drop(map);
        tracing::debug!("Relay session {} paired, piping", session_key);
        let key = session_key.clone();
        let (mut r1, mut w1) = stream.into_split();
        let (mut r2, mut w2) = peer.into_split();
        let t1 = tokio::spawn(async move { tokio::io::copy(&mut r1, &mut w2).await });
        let t2 = tokio::spawn(async move { tokio::io::copy(&mut r2, &mut w1).await });
        let _ = tokio::join!(t1, t2);
        tracing::debug!("Relay session {} ended", key);
    } else {
        map.insert(session_key, stream);
    }
}
