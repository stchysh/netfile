use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryMessage {
    pub device_id: String,
    pub instance_id: String,
    pub device_name: String,
    pub instance_name: String,
    pub version: String,
    pub port: u16,
    pub timestamp: u64,
}

impl DiscoveryMessage {
    pub fn new(
        device_id: String,
        instance_id: String,
        device_name: String,
        instance_name: String,
        port: u16,
    ) -> Self {
        Self {
            device_id,
            instance_id,
            device_name,
            instance_name,
            version: env!("CARGO_PKG_VERSION").to_string(),
            port,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        }
    }

    pub fn to_bytes(&self) -> anyhow::Result<Vec<u8>> {
        Ok(bincode::serialize(self)?)
    }

    pub fn from_bytes(data: &[u8]) -> anyhow::Result<Self> {
        Ok(bincode::deserialize(data)?)
    }
}
