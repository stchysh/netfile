use anyhow::Result;
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMap, RelayMode, SecretKey};
use iroh::endpoint::{QuicTransportConfig, VarInt};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

pub const ALPN: &[u8] = b"netfile/1";

pub struct IrohManager {
    endpoint: Endpoint,
}

impl IrohManager {
    pub async fn new(data_dir: PathBuf, stream_window_mb: u32, relay_url: Option<String>) -> Result<Arc<Self>> {
        let key_dir = data_dir.join("iroh");
        tokio::fs::create_dir_all(&key_dir).await?;
        let key_path = key_dir.join("secret_key");

        let secret_key = if key_path.exists() {
            let bytes = tokio::fs::read(&key_path).await?;
            if bytes.len() == 32 {
                let arr: [u8; 32] = bytes.try_into().map_err(|_| anyhow::anyhow!("invalid key bytes"))?;
                SecretKey::from_bytes(&arr)
            } else {
                warn!("iroh secret key file invalid length, regenerating");
                let k = SecretKey::generate(&mut rand::rng());
                tokio::fs::write(&key_path, k.to_bytes()).await?;
                k
            }
        } else {
            let k = SecretKey::generate(&mut rand::rng());
            tokio::fs::write(&key_path, k.to_bytes()).await?;
            info!("generated new iroh secret key");
            k
        };

        let stream_window_bytes = stream_window_mb.max(8).min(256) as u64 * 1024 * 1024;
        let conn_window_bytes = stream_window_bytes * 2;
        info!("iroh QUIC window: stream={}MB conn={}MB", stream_window_bytes / 1024 / 1024, conn_window_bytes / 1024 / 1024);
        let transport_config = QuicTransportConfig::builder()
            .stream_receive_window(VarInt::try_from(stream_window_bytes).unwrap_or(VarInt::from_u32(32 * 1024 * 1024)))
            .receive_window(VarInt::try_from(conn_window_bytes).unwrap_or(VarInt::from_u32(64 * 1024 * 1024)))
            .send_window(conn_window_bytes)
            .max_idle_timeout(Some(VarInt::from_u32(120_000).into()))
            .keep_alive_interval(Duration::from_secs(15))
            .build();

        let builder = Endpoint::builder()
            .secret_key(secret_key)
            .alpns(vec![ALPN.to_vec()])
            .transport_config(transport_config);

        let builder = if let Some(url_str) = relay_url.filter(|s| !s.is_empty()) {
            match RelayMap::try_from_iter([url_str.as_str()]) {
                Ok(relay_map) => {
                    info!("iroh using custom relay: {}", url_str);
                    builder.relay_mode(RelayMode::Custom(relay_map))
                }
                Err(e) => {
                    warn!("Invalid iroh_relay_url {}: {}, using default relays", url_str, e);
                    builder
                }
            }
        } else {
            builder
        };

        let endpoint = builder.bind().await?;

        info!("iroh endpoint started, node_id={}", endpoint.id());

        Ok(Arc::new(Self { endpoint }))
    }

    pub async fn connect(&self, addr: EndpointAddr) -> Result<iroh::endpoint::Connection> {
        let conn = self.endpoint.connect(addr, ALPN).await?;
        Ok(conn)
    }

    pub async fn accept(&self) -> Option<iroh::endpoint::Incoming> {
        self.endpoint.accept().await
    }

    pub fn endpoint_addr(&self) -> EndpointAddr {
        self.endpoint.addr()
    }

    pub fn endpoint_id(&self) -> EndpointId {
        self.endpoint.id()
    }

    pub fn endpoint_ref(&self) -> &Endpoint {
        &self.endpoint
    }
}
