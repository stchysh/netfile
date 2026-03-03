use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferState {
    pub file_id: String,
    pub file_path: PathBuf,
    pub file_size: u64,
    pub chunk_size: u32,
    pub total_chunks: u32,
    pub completed_chunks: HashSet<u32>,
    pub temp_file_path: PathBuf,
}

impl TransferState {
    pub fn new(
        file_id: String,
        file_path: PathBuf,
        file_size: u64,
        chunk_size: u32,
        temp_file_path: PathBuf,
    ) -> Self {
        let total_chunks = ((file_size + chunk_size as u64 - 1) / chunk_size as u64) as u32;
        Self {
            file_id,
            file_path,
            file_size,
            chunk_size,
            total_chunks,
            completed_chunks: HashSet::new(),
            temp_file_path,
        }
    }

    pub fn is_complete(&self) -> bool {
        self.completed_chunks.len() == self.total_chunks as usize
    }

    pub fn progress(&self) -> f64 {
        self.completed_chunks.len() as f64 / self.total_chunks as f64
    }

    pub fn save(&self, path: &PathBuf) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    pub fn load(path: &PathBuf) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let state: TransferState = serde_json::from_str(&content)?;
        Ok(state)
    }

    pub fn state_file_path(data_dir: &PathBuf, file_id: &str) -> PathBuf {
        data_dir.join("transfers").join(format!("{}.json", file_id))
    }
}
