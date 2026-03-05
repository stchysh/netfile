mod protocol;
mod server;

use clap::Parser;
use tokio::net::TcpListener;

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "0.0.0.0")]
    host: String,
    #[arg(long, default_value_t = 37200)]
    port: u16,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();
    let addr = format!("{}:{}", args.host, args.port);
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("Signal server listening on {}", addr);
    let state = server::ServerState::new();
    loop {
        let (stream, peer) = listener.accept().await?;
        tracing::debug!("New connection from {}", peer);
        let state = state.clone();
        tokio::spawn(async move {
            server::handle_connection(state, stream).await;
        });
    }
}
