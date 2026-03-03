use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub instance: InstanceConfig,
    pub network: NetworkConfig,
    pub transfer: TransferConfig,
    pub security: SecurityConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceConfig {
    #[serde(default = "generate_instance_id")]
    pub instance_id: String,
    #[serde(default = "default_instance_name")]
    pub instance_name: String,
    #[serde(default = "default_device_name")]
    pub device_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    #[serde(default)]
    pub discovery_port: u16,
    #[serde(default)]
    pub transfer_port: u16,
    #[serde(default = "default_broadcast_interval")]
    pub broadcast_interval: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferConfig {
    #[serde(default = "default_chunk_size")]
    pub chunk_size: u32,
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: usize,
    #[serde(default)]
    pub enable_compression: bool,
    #[serde(default)]
    pub download_dir: String,
    #[serde(default)]
    pub speed_limit_mbps: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    #[serde(default = "default_require_auth")]
    pub require_auth: bool,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub allowed_devices: Vec<String>,
    #[serde(default)]
    pub enable_tls: bool,
}

fn generate_instance_id() -> String {
    Uuid::new_v4().to_string()
}

fn default_instance_name() -> String {
    "默认实例".to_string()
}

fn default_device_name() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "Unknown".to_string())
}

fn default_broadcast_interval() -> u64 {
    5
}

fn default_chunk_size() -> u32 {
    1048576
}

fn default_max_concurrent() -> usize {
    3
}

fn default_require_auth() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            instance: InstanceConfig::default(),
            network: NetworkConfig::default(),
            transfer: TransferConfig::default(),
            security: SecurityConfig::default(),
        }
    }
}

impl Default for InstanceConfig {
    fn default() -> Self {
        Self {
            instance_id: generate_instance_id(),
            instance_name: default_instance_name(),
            device_name: default_device_name(),
        }
    }
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            discovery_port: 0,
            transfer_port: 0,
            broadcast_interval: default_broadcast_interval(),
        }
    }
}

impl Default for TransferConfig {
    fn default() -> Self {
        Self {
            chunk_size: default_chunk_size(),
            max_concurrent: default_max_concurrent(),
            enable_compression: false,
            download_dir: String::new(),
            speed_limit_mbps: 0,
        }
    }
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            require_auth: default_require_auth(),
            password: String::new(),
            allowed_devices: Vec::new(),
            enable_tls: false,
        }
    }
}

impl Config {
    pub fn load(path: &PathBuf) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }

    pub fn save(&self, path: &PathBuf) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    pub fn default_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".netfile")
            .join("config.toml")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = Config::default();

        assert!(!config.instance.instance_id.is_empty());
        assert_eq!(config.instance.instance_name, "默认实例");
        assert!(!config.instance.device_name.is_empty());

        assert_eq!(config.network.discovery_port, 0);
        assert_eq!(config.network.transfer_port, 0);
        assert_eq!(config.network.broadcast_interval, 5);

        assert_eq!(config.transfer.chunk_size, 1048576);
        assert_eq!(config.transfer.max_concurrent, 3);
        assert!(!config.transfer.enable_compression);

        assert!(config.security.require_auth);
        assert!(config.security.password.is_empty());
        assert!(config.security.allowed_devices.is_empty());
        assert!(!config.security.enable_tls);
    }

    #[test]
    fn test_config_serialization() {
        let config = Config::default();
        let toml_str = toml::to_string(&config).unwrap();
        let deserialized: Config = toml::from_str(&toml_str).unwrap();

        assert_eq!(config.instance.instance_id, deserialized.instance.instance_id);
        assert_eq!(config.network.broadcast_interval, deserialized.network.broadcast_interval);
        assert_eq!(config.transfer.chunk_size, deserialized.transfer.chunk_size);
    }

    #[test]
    fn test_instance_id_generation() {
        let id1 = generate_instance_id();
        let id2 = generate_instance_id();

        assert_ne!(id1, id2);
        assert_eq!(id1.len(), 36);
    }

    #[test]
    fn test_default_values() {
        assert_eq!(default_chunk_size(), 1048576);
        assert_eq!(default_max_concurrent(), 3);
        assert_eq!(default_broadcast_interval(), 5);
        assert!(default_require_auth());
    }
}
