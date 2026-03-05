use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: String,
    pub from_instance_id: String,
    pub from_instance_name: String,
    pub content: String,
    pub timestamp: u64,
    pub is_self: bool,
}

pub struct MessageStore {
    data_dir: PathBuf,
}

impl MessageStore {
    pub fn new(data_dir: PathBuf) -> Self {
        Self { data_dir }
    }

    fn conversation_path(&self, peer_instance_id: &str) -> PathBuf {
        self.data_dir.join("messages").join(format!("{}.json", peer_instance_id))
    }

    pub async fn save_message(&self, peer_instance_id: &str, msg: ChatMessage) -> Result<()> {
        let path = self.conversation_path(peer_instance_id);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let mut messages = self.load_conversation(peer_instance_id).await.unwrap_or_default();
        messages.push(msg);
        let content = serde_json::to_string(&messages)?;
        tokio::fs::write(&path, content).await?;
        Ok(())
    }

    pub async fn load_conversation(&self, peer_instance_id: &str) -> Result<Vec<ChatMessage>> {
        let path = self.conversation_path(peer_instance_id);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let content = tokio::fs::read_to_string(&path).await?;
        let messages: Vec<ChatMessage> = serde_json::from_str(&content)?;
        Ok(messages)
    }

    pub async fn get_all_counts(&self) -> HashMap<String, usize> {
        let mut result = HashMap::new();
        let messages_dir = self.data_dir.join("messages");
        let mut dir = match tokio::fs::read_dir(&messages_dir).await {
            Ok(d) => d,
            Err(_) => return result,
        };
        while let Ok(Some(entry)) = dir.next_entry().await {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let peer_id = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) if !s.is_empty() => s.to_string(),
                _ => continue,
            };
            if let Ok(data) = tokio::fs::read(&path).await {
                let count = serde_json::from_slice::<Vec<ChatMessage>>(&data)
                    .map(|v| v.len())
                    .unwrap_or(0);
                result.insert(peer_id, count);
            }
        }
        result
    }
}
