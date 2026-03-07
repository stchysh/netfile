use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::SeekFrom;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: String,
    pub from_instance_id: String,
    pub from_instance_name: String,
    pub content: String,
    pub timestamp: u64,
    #[serde(default)]
    pub local_seq: u64,
    pub is_self: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationDelta {
    pub messages: Vec<ChatMessage>,
    pub next_cursor: u64,
    pub reset: bool,
}

pub struct MessageStore {
    data_dir: PathBuf,
    write_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    count_lock: Arc<Mutex<()>>,
}

impl MessageStore {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            write_locks: Arc::new(Mutex::new(HashMap::new())),
            count_lock: Arc::new(Mutex::new(())),
        }
    }

    fn conversation_path(&self, peer_instance_id: &str) -> PathBuf {
        self.data_dir.join("messages").join(format!("{}.jsonl", peer_instance_id))
    }

    fn legacy_conversation_path(&self, peer_instance_id: &str) -> PathBuf {
        self.data_dir.join("messages").join(format!("{}.json", peer_instance_id))
    }

    fn count_index_path(&self) -> PathBuf {
        self.data_dir.join("messages").join("counts.json")
    }

    async fn peer_lock(&self, peer_instance_id: &str) -> Arc<Mutex<()>> {
        let mut locks = self.write_locks.lock().await;
        locks.entry(peer_instance_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    async fn ensure_jsonl_conversation(&self, peer_instance_id: &str) -> Result<PathBuf> {
        let path = self.conversation_path(peer_instance_id);
        if path.exists() {
            return Ok(path);
        }

        let legacy_path = self.legacy_conversation_path(peer_instance_id);
        if !legacy_path.exists() {
            return Ok(path);
        }

        let legacy_content = tokio::fs::read_to_string(&legacy_path).await?;
        let messages: Vec<ChatMessage> = if legacy_content.trim().is_empty() {
            Vec::new()
        } else {
            serde_json::from_str(&legacy_content)?
        };
        let messages = Self::normalize_messages(messages);

        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let jsonl = Self::serialize_jsonl(&messages)?;
        tokio::fs::write(&path, jsonl).await?;
        self.set_count(peer_instance_id, messages.len()).await?;
        Ok(path)
    }

    fn serialize_jsonl(messages: &[ChatMessage]) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        for msg in messages {
            let line = serde_json::to_vec(msg)?;
            out.extend_from_slice(&line);
            out.push(b'\n');
        }
        Ok(out)
    }

    fn parse_jsonl(content: &str) -> Result<Vec<ChatMessage>> {
        let mut messages = Vec::new();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            messages.push(serde_json::from_str(line)?);
        }
        Ok(Self::normalize_messages(messages))
    }

    fn normalize_messages(mut messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
        for (idx, msg) in messages.iter_mut().enumerate() {
            if msg.local_seq == 0 {
                msg.local_seq = (idx + 1) as u64;
            }
        }
        messages
    }

    async fn current_count(&self, peer_instance_id: &str, path: &PathBuf) -> Result<usize> {
        let counts = self.read_count_index().await?;
        if let Some(count) = counts.get(peer_instance_id) {
            return Ok(*count);
        }

        if !path.exists() {
            return Ok(0);
        }

        let content = tokio::fs::read_to_string(path).await.unwrap_or_default();
        Ok(content.lines().filter(|line| !line.trim().is_empty()).count())
    }

    async fn read_count_index(&self) -> Result<HashMap<String, usize>> {
        let path = self.count_index_path();
        if !path.exists() {
            return Ok(HashMap::new());
        }
        let content = tokio::fs::read_to_string(path).await?;
        if content.trim().is_empty() {
            return Ok(HashMap::new());
        }
        Ok(serde_json::from_str(&content)?)
    }

    async fn write_count_index(&self, counts: &HashMap<String, usize>) -> Result<()> {
        let path = self.count_index_path();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(path, serde_json::to_vec(counts)?).await?;
        Ok(())
    }

    async fn set_count(&self, peer_instance_id: &str, count: usize) -> Result<()> {
        let _guard = self.count_lock.lock().await;
        let mut counts = self.read_count_index().await?;
        counts.insert(peer_instance_id.to_string(), count);
        self.write_count_index(&counts).await
    }

    async fn rebuild_count_index(&self) -> Result<HashMap<String, usize>> {
        let mut counts = HashMap::new();
        self.populate_missing_counts(&mut counts).await?;
        self.write_count_index(&counts).await?;
        Ok(counts)
    }

    async fn populate_missing_counts(&self, counts: &mut HashMap<String, usize>) -> Result<bool> {
        let mut changed = false;
        let messages_dir = self.data_dir.join("messages");
        let mut dir = match tokio::fs::read_dir(&messages_dir).await {
            Ok(d) => d,
            Err(_) => return Ok(changed),
        };
        while let Ok(Some(entry)) = dir.next_entry().await {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let peer_id = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) if !s.is_empty() => s.to_string(),
                _ => continue,
            };
            if counts.contains_key(&peer_id) {
                continue;
            }
            let content = tokio::fs::read_to_string(&path).await.unwrap_or_default();
            counts.insert(
                peer_id,
                content.lines().filter(|line| !line.trim().is_empty()).count(),
            );
            changed = true;
        }

        let mut dir = match tokio::fs::read_dir(&messages_dir).await {
            Ok(d) => d,
            Err(_) => {
                return Ok(changed);
            }
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
            if counts.contains_key(&peer_id) {
                continue;
            }
            if let Ok(data) = tokio::fs::read(&path).await {
                let count = serde_json::from_slice::<Vec<ChatMessage>>(&data)
                    .map(|v| v.len())
                    .unwrap_or(0);
                counts.insert(peer_id, count);
                changed = true;
            }
        }
        Ok(changed)
    }

    pub async fn save_message(&self, peer_instance_id: &str, msg: ChatMessage) -> Result<()> {
        let lock = self.peer_lock(peer_instance_id).await;
        let _guard = lock.lock().await;

        let path = self.ensure_jsonl_conversation(peer_instance_id).await?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let current_count = self.current_count(peer_instance_id, &path).await?;
        let mut msg = msg;
        if msg.local_seq == 0 {
            msg.local_seq = (current_count + 1) as u64;
        }

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        let line = serde_json::to_vec(&msg)?;
        file.write_all(&line).await?;
        file.write_all(b"\n").await?;
        file.flush().await?;
        let next_count = current_count.max(msg.local_seq as usize);
        self.set_count(peer_instance_id, next_count).await?;
        Ok(())
    }

    pub async fn load_conversation(&self, peer_instance_id: &str) -> Result<Vec<ChatMessage>> {
        let lock = self.peer_lock(peer_instance_id).await;
        let _guard = lock.lock().await;

        let path = self.ensure_jsonl_conversation(peer_instance_id).await?;
        if !path.exists() {
            return Ok(Vec::new());
        }
        let content = tokio::fs::read_to_string(&path).await?;
        Self::parse_jsonl(&content)
    }

    pub async fn load_conversation_delta(
        &self,
        peer_instance_id: &str,
        cursor: u64,
    ) -> Result<ConversationDelta> {
        let lock = self.peer_lock(peer_instance_id).await;
        let _guard = lock.lock().await;

        let path = self.ensure_jsonl_conversation(peer_instance_id).await?;
        if !path.exists() {
            return Ok(ConversationDelta {
                messages: Vec::new(),
                next_cursor: 0,
                reset: false,
            });
        }

        let mut file = tokio::fs::OpenOptions::new().read(true).open(&path).await?;
        let file_len = file.metadata().await?.len();
        let (start, reset) = if cursor > file_len {
            (0, true)
        } else {
            (cursor, false)
        };
        file.seek(SeekFrom::Start(start)).await?;

        let mut content = String::new();
        file.read_to_string(&mut content).await?;
        Ok(ConversationDelta {
            messages: Self::parse_jsonl(&content)?,
            next_cursor: file_len,
            reset,
        })
    }

    pub async fn get_all_counts(&self) -> HashMap<String, usize> {
        match self.read_count_index().await {
            Ok(mut counts) if !counts.is_empty() => {
                if self.populate_missing_counts(&mut counts).await.unwrap_or(false) {
                    let _ = self.write_count_index(&counts).await;
                }
                counts
            }
            _ => self.rebuild_count_index().await.unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ChatMessage, MessageStore};
    use anyhow::Result;
    use std::path::PathBuf;
    use uuid::Uuid;

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("netfile-message-store-{}", Uuid::new_v4()));
            Self { path }
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn sample_message(id: &str, ts: u64) -> ChatMessage {
        ChatMessage {
            id: id.to_string(),
            from_instance_id: "peer-a".to_string(),
            from_instance_name: "peer-a".to_string(),
            content: format!("msg-{id}"),
            timestamp: ts,
            local_seq: 0,
            is_self: false,
        }
    }

    #[tokio::test]
    async fn saves_and_loads_jsonl_messages() -> Result<()> {
        let dir = TestDir::new();
        let store = MessageStore::new(dir.path.clone());

        store.save_message("peer-a", sample_message("1", 100)).await?;
        store.save_message("peer-a", sample_message("2", 101)).await?;

        let path = dir.path.join("messages").join("peer-a.jsonl");
        let content = tokio::fs::read_to_string(&path).await?;
        assert_eq!(content.lines().count(), 2);

        let messages = store.load_conversation("peer-a").await?;
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].id, "1");
        assert_eq!(messages[1].id, "2");
        assert_eq!(messages[0].local_seq, 1);
        assert_eq!(messages[1].local_seq, 2);
        Ok(())
    }

    #[tokio::test]
    async fn migrates_legacy_json_to_jsonl() -> Result<()> {
        let dir = TestDir::new();
        let store = MessageStore::new(dir.path.clone());
        let messages_dir = dir.path.join("messages");
        tokio::fs::create_dir_all(&messages_dir).await?;

        let legacy_path = messages_dir.join("peer-b.json");
        let legacy_messages = vec![sample_message("1", 100), sample_message("2", 101)];
        tokio::fs::write(&legacy_path, serde_json::to_vec(&legacy_messages)?).await?;

        let loaded = store.load_conversation("peer-b").await?;
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].local_seq, 1);
        assert_eq!(loaded[1].local_seq, 2);

        let jsonl_path = messages_dir.join("peer-b.jsonl");
        assert!(jsonl_path.exists());

        store.save_message("peer-b", sample_message("3", 102)).await?;
        let reloaded = store.load_conversation("peer-b").await?;
        assert_eq!(reloaded.len(), 3);
        assert_eq!(reloaded[2].id, "3");
        assert_eq!(reloaded[2].local_seq, 3);
        Ok(())
    }

    #[tokio::test]
    async fn counts_support_jsonl_and_legacy_json() -> Result<()> {
        let dir = TestDir::new();
        let store = MessageStore::new(dir.path.clone());
        let messages_dir = dir.path.join("messages");
        tokio::fs::create_dir_all(&messages_dir).await?;

        store.save_message("peer-c", sample_message("1", 100)).await?;
        store.save_message("peer-c", sample_message("2", 101)).await?;

        let legacy_messages = vec![
            sample_message("3", 102),
            sample_message("4", 103),
            sample_message("5", 104),
        ];
        tokio::fs::write(
            messages_dir.join("peer-d.json"),
            serde_json::to_vec(&legacy_messages)?,
        )
        .await?;

        let counts = store.get_all_counts().await;
        assert_eq!(counts.get("peer-c"), Some(&2));
        assert_eq!(counts.get("peer-d"), Some(&3));
        Ok(())
    }

    #[tokio::test]
    async fn load_delta_reads_only_new_messages() -> Result<()> {
        let dir = TestDir::new();
        let store = MessageStore::new(dir.path.clone());

        store.save_message("peer-e", sample_message("1", 100)).await?;
        let initial = store.load_conversation_delta("peer-e", 0).await?;
        assert_eq!(initial.messages.len(), 1);
        assert!(initial.next_cursor > 0);
        assert!(!initial.reset);

        store.save_message("peer-e", sample_message("2", 101)).await?;
        let delta = store
            .load_conversation_delta("peer-e", initial.next_cursor)
            .await?;
        assert_eq!(delta.messages.len(), 1);
        assert_eq!(delta.messages[0].id, "2");
        assert!(!delta.reset);
        Ok(())
    }
}
