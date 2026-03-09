use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareEntry {
    pub record_id: String,
    pub file_name: String,
    pub file_size: u64,
    pub save_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_md5: Option<String>,
    pub tags: Vec<String>,
    pub remark: String,
    pub excluded: bool,
    pub download_count: u32,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookmarkEntry {
    pub id: String,
    pub file_id: String,
    pub file_name: String,
    pub file_size: u64,
    pub tags: Vec<String>,
    pub remark: String,
    pub source_instance_id: String,
    pub source_instance_name: String,
    pub source_transfer_addr: String,
    pub require_confirm: bool,
    pub bookmarked_at: u64,
}

pub struct ShareStore {
    path: PathBuf,
    lock: Arc<Mutex<()>>,
}

pub struct BookmarkStore {
    path: PathBuf,
    lock: Arc<Mutex<()>>,
}

impl ShareStore {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            path: data_dir.join("share_entries.json"),
            lock: Arc::new(Mutex::new(())),
        }
    }

    pub async fn load_entries(&self) -> Vec<ShareEntry> {
        self.read_entries().await
    }

    pub async fn upsert_entry(&self, entry: ShareEntry) -> Result<()> {
        let _guard = self.lock.lock().await;
        let mut entries = self.read_entries().await;
        if let Some(pos) = entries.iter().position(|e| e.record_id == entry.record_id) {
            entries[pos] = entry;
        } else {
            entries.insert(0, entry);
        }
        self.write_entries(&entries).await
    }

    pub async fn set_excluded(&self, record_id: &str, excluded: bool) -> Result<()> {
        let _guard = self.lock.lock().await;
        let mut entries = self.read_entries().await;
        if let Some(e) = entries.iter_mut().find(|e| e.record_id == record_id) {
            e.excluded = excluded;
        }
        self.write_entries(&entries).await
    }

    pub async fn update_tags(&self, record_id: &str, tags: Vec<String>) -> Result<()> {
        let _guard = self.lock.lock().await;
        let mut entries = self.read_entries().await;
        if let Some(e) = entries.iter_mut().find(|e| e.record_id == record_id) {
            e.tags = tags;
        }
        self.write_entries(&entries).await
    }

    pub async fn update_remark(&self, record_id: &str, remark: String) -> Result<()> {
        let _guard = self.lock.lock().await;
        let mut entries = self.read_entries().await;
        if let Some(e) = entries.iter_mut().find(|e| e.record_id == record_id) {
            e.remark = remark;
        }
        self.write_entries(&entries).await
    }

    pub async fn get_shared_entries(&self) -> Vec<ShareEntry> {
        self.read_entries().await.into_iter().filter(|e| !e.excluded).collect()
    }

    pub async fn increment_download_count(&self, record_id: &str) -> Result<()> {
        let _guard = self.lock.lock().await;
        let mut entries = self.read_entries().await;
        if let Some(e) = entries.iter_mut().find(|e| e.record_id == record_id) {
            e.download_count = e.download_count.saturating_add(1);
        }
        self.write_entries(&entries).await
    }

    pub async fn update_md5(&self, record_id: &str, md5: String) -> Result<()> {
        let _guard = self.lock.lock().await;
        let mut entries = self.read_entries().await;
        if let Some(e) = entries.iter_mut().find(|e| e.record_id == record_id) {
            e.file_md5 = Some(md5);
        }
        self.write_entries(&entries).await
    }

    async fn read_entries(&self) -> Vec<ShareEntry> {
        match tokio::fs::read(&self.path).await {
            Ok(data) => serde_json::from_slice(&data).unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    async fn write_entries(&self, entries: &[ShareEntry]) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&self.path, serde_json::to_vec(entries)?).await?;
        Ok(())
    }
}

impl BookmarkStore {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            path: data_dir.join("share_bookmarks.json"),
            lock: Arc::new(Mutex::new(())),
        }
    }

    pub async fn load_bookmarks(&self) -> Vec<BookmarkEntry> {
        self.read_bookmarks().await
    }

    pub async fn add_bookmark(&self, entry: BookmarkEntry) -> Result<()> {
        let _guard = self.lock.lock().await;
        let mut bookmarks = self.read_bookmarks().await;
        bookmarks.insert(0, entry);
        self.write_bookmarks(&bookmarks).await
    }

    pub async fn remove_bookmark(&self, id: &str) -> Result<()> {
        let _guard = self.lock.lock().await;
        let mut bookmarks = self.read_bookmarks().await;
        bookmarks.retain(|b| b.id != id);
        self.write_bookmarks(&bookmarks).await
    }

    pub async fn get_bookmarks(&self) -> Vec<BookmarkEntry> {
        self.read_bookmarks().await
    }

    async fn read_bookmarks(&self) -> Vec<BookmarkEntry> {
        match tokio::fs::read(&self.path).await {
            Ok(data) => serde_json::from_slice(&data).unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    async fn write_bookmarks(&self, bookmarks: &[BookmarkEntry]) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&self.path, serde_json::to_vec(bookmarks)?).await?;
        Ok(())
    }
}
