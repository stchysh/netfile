use crate::Config;
use anyhow::Result;
use sha2::{Sha256, Digest};

pub struct AuthManager {
    config: Config,
}

impl AuthManager {
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    pub fn is_device_allowed(&self, device_id: &str) -> bool {
        if !self.config.security.require_auth {
            return true;
        }

        if self.config.security.allowed_devices.is_empty() {
            return true;
        }

        self.config.security.allowed_devices.contains(&device_id.to_string())
    }

    pub fn verify_password(&self, password: &str) -> bool {
        if self.config.security.password.is_empty() {
            return true;
        }

        let hash = Self::hash_password(password);
        hash == self.config.security.password
    }

    pub fn hash_password(password: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(password.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    pub fn add_allowed_device(&mut self, device_id: String) {
        if !self.config.security.allowed_devices.contains(&device_id) {
            self.config.security.allowed_devices.push(device_id);
        }
    }

    pub fn remove_allowed_device(&mut self, device_id: &str) {
        self.config.security.allowed_devices.retain(|id| id != device_id);
    }

    pub fn list_allowed_devices(&self) -> &[String] {
        &self.config.security.allowed_devices
    }

    pub fn save_config(&self, path: &std::path::Path) -> Result<()> {
        let path_buf = path.to_path_buf();
        self.config.save(&path_buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_password() {
        let password = "test_password";
        let hash1 = AuthManager::hash_password(password);
        let hash2 = AuthManager::hash_password(password);

        assert_eq!(hash1, hash2);
        assert_eq!(hash1.len(), 64);
    }

    #[test]
    fn test_verify_password() {
        let password = "my_secret";
        let hash = AuthManager::hash_password(password);

        let mut config = Config::default();
        config.security.password = hash;

        let auth_manager = AuthManager::new(config);

        assert!(auth_manager.verify_password(password));
        assert!(!auth_manager.verify_password("wrong_password"));
    }

    #[test]
    fn test_device_authorization() {
        let config = Config::default();
        let mut auth_manager = AuthManager::new(config);

        let device_id = "device-123";

        assert!(auth_manager.is_device_allowed(device_id));

        auth_manager.add_allowed_device(device_id.to_string());
        assert!(auth_manager.is_device_allowed(device_id));

        auth_manager.remove_allowed_device(device_id);
        assert!(auth_manager.is_device_allowed(device_id));
    }

    #[test]
    fn test_list_allowed_devices() {
        let config = Config::default();
        let mut auth_manager = AuthManager::new(config);

        auth_manager.add_allowed_device("device1".to_string());
        auth_manager.add_allowed_device("device2".to_string());
        auth_manager.add_allowed_device("device3".to_string());

        let devices = auth_manager.list_allowed_devices();
        assert_eq!(devices.len(), 3);
        assert!(devices.contains(&"device1".to_string()));
        assert!(devices.contains(&"device2".to_string()));
        assert!(devices.contains(&"device3".to_string()));
    }
}
