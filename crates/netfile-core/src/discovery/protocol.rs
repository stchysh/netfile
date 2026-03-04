use serde::{Deserialize, Serialize};

const MSG_VERSION: u8 = 0x02;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryMessage {
    pub device_id: String,
    pub instance_id: String,
    pub device_name: String,
    pub instance_name: String,
    pub version: String,
    pub port: u16,
    pub timestamp: u64,
    pub public_transfer_addr: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyDiscoveryMessage {
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
            public_transfer_addr: None,
        }
    }

    pub fn to_bytes(&self) -> anyhow::Result<Vec<u8>> {
        let mut data = vec![MSG_VERSION];
        data.extend(bincode::serialize(self)?);
        Ok(data)
    }

    pub fn from_bytes(data: &[u8]) -> anyhow::Result<Self> {
        if data.first() == Some(&MSG_VERSION) && data.len() > 1 {
            Ok(bincode::deserialize(&data[1..])?)
        } else {
            let legacy: LegacyDiscoveryMessage = bincode::deserialize(data)?;
            Ok(Self {
                device_id: legacy.device_id,
                instance_id: legacy.instance_id,
                device_name: legacy.device_name,
                instance_name: legacy.instance_name,
                version: legacy.version,
                port: legacy.port,
                timestamp: legacy.timestamp,
                public_transfer_addr: None,
            })
        }
    }
}
