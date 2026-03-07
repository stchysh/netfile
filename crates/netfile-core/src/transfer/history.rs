use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferRecord {
    pub id: String,
    pub file_name: String,
    pub file_size: u64,
    pub direction: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub timestamp: u64,
    pub elapsed_secs: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub save_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transfer_method: Option<String>,
}

pub struct HistoryStore {
    path: PathBuf,
    lock: Arc<Mutex<()>>,
}

const MAX_RECORDS: usize = 200;

impl HistoryStore {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            path: data_dir.join("transfer_history.json"),
            lock: Arc::new(Mutex::new(())),
        }
    }

    pub async fn add_record(&self, record: TransferRecord) -> Result<()> {
        let _guard = self.lock.lock().await;
        let mut records = self.read_records().await;
        records.insert(0, record);
        records.truncate(MAX_RECORDS);
        self.write_records(&records).await
    }

    pub async fn load_history(&self) -> Vec<TransferRecord> {
        self.read_records().await
    }

    pub async fn clear_history(&self) -> Result<()> {
        let _guard = self.lock.lock().await;
        self.write_records(&[]).await
    }

    async fn read_records(&self) -> Vec<TransferRecord> {
        match tokio::fs::read(&self.path).await {
            Ok(data) => serde_json::from_slice(&data).unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    async fn write_records(&self, records: &[TransferRecord]) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&self.path, serde_json::to_vec(records)?).await?;
        Ok(())
    }
}
