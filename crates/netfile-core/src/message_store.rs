use anyhow::Result;
use serde::{Deserialize, Serialize};
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
}
