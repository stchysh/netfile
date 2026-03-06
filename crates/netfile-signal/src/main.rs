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
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("debug")),
        )
        .init();
    let args = Args::parse();

    let state = server::ServerState::new();

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
