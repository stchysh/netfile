use anyhow::Result;
use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{info, warn};

pub const ALPN: &[u8] = b"netfile/1";

pub struct IrohManager {
    endpoint: Endpoint,
}

impl IrohManager {
    pub async fn new(data_dir: PathBuf) -> Result<Arc<Self>> {
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

        let endpoint = Endpoint::builder()
            .secret_key(secret_key)
            .alpns(vec![ALPN.to_vec()])
            .bind()
            .await?;

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
