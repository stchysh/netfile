use anyhow::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex;

use super::history::TransferRecord;

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_md5: Option<String>,
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

/// Compute SHA-256 of a file, returned as lowercase hex string.
/// Returns error for directories or unreadable files.
pub async fn compute_file_sha256(path: &std::path::Path) -> Result<String> {
    if path.is_dir() {
        return Err(anyhow::anyhow!("path is a directory"));
    }
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 4 * 1024 * 1024];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
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

    /// Upsert entry with MD5-based deduplication.
    /// If entry.file_md5 is Some and another entry with the same MD5 exists,
    /// merge tags into that entry instead of adding a duplicate.
    pub async fn upsert_entry(&self, entry: ShareEntry) -> Result<()> {
        let _guard = self.lock.lock().await;
        let mut entries = self.read_entries().await;

        // MD5 dedup: if this entry has a known hash, check for an existing entry with the same hash
        if let Some(ref hash) = entry.file_md5 {
            if let Some(existing) = entries.iter_mut().find(|e| {
                e.record_id != entry.record_id && e.file_md5.as_deref() == Some(hash.as_str())
            }) {
                // Merge tags into existing entry (deduplicated)
                for tag in &entry.tags {
                    if !existing.tags.contains(tag) {
                        existing.tags.push(tag.clone());
                    }
                }
                return self.write_entries(&entries).await;
            }
        }

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

    /// Set MD5 for an entry. If another entry already has the same MD5,
    /// merge the current entry's tags into that one and remove this duplicate.
    pub async fn update_md5(&self, record_id: &str, hash: String) -> Result<()> {
        let _guard = self.lock.lock().await;
        let mut entries = self.read_entries().await;

        let current_tags: Vec<String> = entries
            .iter()
            .find(|e| e.record_id == record_id)
            .map(|e| e.tags.clone())
            .unwrap_or_default();

        // Check if another entry already has this hash
        let dup_idx = entries.iter().position(|e| {
            e.record_id != record_id && e.file_md5.as_deref() == Some(hash.as_str())
        });

        if let Some(idx) = dup_idx {
            // Merge current entry's tags into the existing duplicate
            for tag in &current_tags {
                if !entries[idx].tags.contains(tag) {
                    entries[idx].tags.push(tag.clone());
                }
            }
            // Mark this entry as excluded rather than removing it, so the
            // transfer history can still show its share meta section
            if let Some(e) = entries.iter_mut().find(|e| e.record_id == record_id) {
                e.file_md5 = Some(hash);
                e.excluded = true;
            }
        } else {
            // No duplicate — just update the hash
            if let Some(e) = entries.iter_mut().find(|e| e.record_id == record_id) {
                e.file_md5 = Some(hash);
            }
        }

        self.write_entries(&entries).await
    }

    pub async fn clear_all(&self) -> Result<()> {
        let _guard = self.lock.lock().await;
        self.write_entries(&[]).await
    }

    pub async fn remove_entry(&self, record_id: &str) -> Result<()> {
        let _guard = self.lock.lock().await;
        let mut entries = self.read_entries().await;
        entries.retain(|e| e.record_id != record_id);
        self.write_entries(&entries).await
    }

    /// Synchronise share entries against the current transfer history.
    ///
    /// - Removes entries whose record_id is absent from `records` (deleted history) or
    ///   whose file no longer exists on disk.
    /// - Inserts new entries for completed receive records that are not yet in the store.
    pub async fn sync_from_history(
        &self,
        records: &[TransferRecord],
        instance_name: &str,
    ) -> Result<()> {
        let _guard = self.lock.lock().await;
        let mut entries = self.read_entries().await;

        // Build set of valid record IDs (completed receives only)
        let valid_ids: std::collections::HashSet<&str> = records
            .iter()
            .filter(|r| r.direction == "receive" && r.status == "completed")
            .map(|r| r.id.as_str())
            .collect();

        // Remove stale entries: record gone from history OR file no longer on disk
        entries.retain(|e| {
            if !valid_ids.contains(e.record_id.as_str()) {
                return false;
            }
            std::path::Path::new(&e.save_path).exists()
        });

        // Add new entries for history records not yet in the store
        let existing_ids: std::collections::HashSet<String> =
            entries.iter().map(|e| e.record_id.clone()).collect();

        let mut new_entries: Vec<ShareEntry> = Vec::new();
        for record in records {
            if record.direction != "receive" || record.status != "completed" {
                continue;
            }
            if existing_ids.contains(&record.id) {
                continue;
            }
            let save_path = match &record.save_path {
                Some(p) => p.clone(),
                None => continue,
            };
            if !std::path::Path::new(&save_path).exists() {
                continue;
            }
            new_entries.push(ShareEntry {
                record_id: record.id.clone(),
                file_name: record.file_name.clone(),
                file_size: record.file_size,
                save_path,
                file_md5: None,
                tags: vec![instance_name.to_string()],
                remark: String::new(),
                excluded: false,
                download_count: 0,
                timestamp: record.timestamp,
            });
        }
        // Prepend new entries (most recent first)
        new_entries.extend(entries);
        let entries = new_entries;

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
