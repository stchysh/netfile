use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct TransferProgress {
    pub file_id: String,
    pub file_name: String,
    pub total_size: u64,
    pub transferred: u64,
    pub total_chunks: u32,
    pub completed_chunks: u32,
    pub speed: f64,
    pub eta_secs: u64,
    pub elapsed_secs: u64,
    pub direction: String,
    pub status: String,
    pub paused: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip)]
    pub start_time: Instant,
    #[serde(skip)]
    pub last_update: Instant,
}

impl TransferProgress {
    pub fn new(file_id: String, file_name: String, total_size: u64, total_chunks: u32, direction: String) -> Self {
        let now = Instant::now();
        Self {
            file_id,
            file_name,
            total_size,
            transferred: 0,
            total_chunks,
            completed_chunks: 0,
            speed: 0.0,
            eta_secs: 0,
            elapsed_secs: 0,
            direction,
            status: "active".to_string(),
            paused: false,
            current_file: None,
            error: None,
            start_time: now,
            last_update: now,
        }
    }

    pub fn update(&mut self, chunk_size: u64) {
        self.transferred += chunk_size;
        self.completed_chunks += 1;
        self.last_update = Instant::now();
    }

    pub fn progress_percent(&self) -> f64 {
        if self.total_size == 0 {
            return 100.0;
        }
        (self.transferred as f64 / self.total_size as f64) * 100.0
    }

    pub fn speed_bps(&self) -> f64 {
        let elapsed = self.last_update.duration_since(self.start_time).as_secs_f64();
        if elapsed == 0.0 {
            return 0.0;
        }
        self.transferred as f64 / elapsed
    }

    pub fn speed_mbps(&self) -> f64 {
        self.speed_bps() / 1024.0 / 1024.0
    }

    pub fn eta(&self) -> Duration {
        let speed = self.speed_bps();
        if speed == 0.0 {
            return Duration::from_secs(0);
        }
        let remaining = self.total_size.saturating_sub(self.transferred);
        Duration::from_secs_f64(remaining as f64 / speed)
    }

    pub fn format_size(bytes: u64) -> String {
        const KB: u64 = 1024;
        const MB: u64 = KB * 1024;
        const GB: u64 = MB * 1024;

        if bytes >= GB {
            format!("{:.2} GB", bytes as f64 / GB as f64)
        } else if bytes >= MB {
            format!("{:.2} MB", bytes as f64 / MB as f64)
        } else if bytes >= KB {
            format!("{:.2} KB", bytes as f64 / KB as f64)
        } else {
            format!("{} B", bytes)
        }
    }

    pub fn format_duration(duration: Duration) -> String {
        let secs = duration.as_secs();
        if secs >= 3600 {
            format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
        } else if secs >= 60 {
            format!("{}m {}s", secs / 60, secs % 60)
        } else {
            format!("{}s", secs)
        }
    }

    pub fn display(&self) -> String {
        format!(
            "{}: {:.1}% ({}/{}) - {:.2} MB/s - ETA: {}",
            self.file_name,
            self.progress_percent(),
            Self::format_size(self.transferred),
            Self::format_size(self.total_size),
            self.speed_mbps(),
            Self::format_duration(self.eta())
        )
    }
}

pub struct ProgressTracker {
    progresses: Arc<RwLock<std::collections::HashMap<String, TransferProgress>>>,
}

impl ProgressTracker {
    pub fn new() -> Self {
        Self {
            progresses: Arc::new(RwLock::new(std::collections::HashMap::new())),
        }
    }

    pub async fn start_transfer(
        &self,
        file_id: String,
        file_name: String,
        total_size: u64,
        total_chunks: u32,
        direction: String,
    ) {
        let progress = TransferProgress::new(file_id.clone(), file_name, total_size, total_chunks, direction);
        self.progresses.write().await.insert(file_id, progress);
    }

    pub async fn register_queued(
        &self,
        file_id: String,
        file_name: String,
        total_size: u64,
        total_chunks: u32,
        direction: String,
    ) {
        let mut progress = TransferProgress::new(file_id.clone(), file_name, total_size, total_chunks, direction);
        progress.status = "queued".to_string();
        self.progresses.write().await.insert(file_id, progress);
    }

    pub async fn set_active(&self, file_id: &str) {
        if let Some(progress) = self.progresses.write().await.get_mut(file_id) {
            progress.status = "active".to_string();
            let now = Instant::now();
            progress.start_time = now;
            progress.last_update = now;
        }
    }

    pub async fn update_progress(&self, file_id: &str, chunk_size: u64) {
        if let Some(progress) = self.progresses.write().await.get_mut(file_id) {
            progress.update(chunk_size);
        }
    }

    pub async fn get_progress(&self, file_id: &str) -> Option<TransferProgress> {
        self.progresses.read().await.get(file_id).cloned()
    }

    pub async fn remove_progress(&self, file_id: &str) {
        self.progresses.write().await.remove(file_id);
    }

    pub async fn set_paused(&self, file_id: &str, paused: bool) {
        if let Some(progress) = self.progresses.write().await.get_mut(file_id) {
            progress.paused = paused;
        }
    }

    pub async fn set_current_file(&self, file_id: &str, current_file: String) {
        if let Some(progress) = self.progresses.write().await.get_mut(file_id) {
            progress.current_file = Some(current_file);
        }
    }

    pub async fn set_error(&self, file_id: &str, error: String) {
        if let Some(progress) = self.progresses.write().await.get_mut(file_id) {
            progress.status = "error".to_string();
            progress.error = Some(error);
        }
    }

    pub async fn register_pending_confirm(
        &self,
        file_id: String,
        file_name: String,
        file_size: u64,
    ) {
        let mut progress = TransferProgress::new(file_id.clone(), file_name, file_size, 0, "receive".to_string());
        progress.status = "pending_confirm".to_string();
        self.progresses.write().await.insert(file_id, progress);
    }

    pub async fn list_all(&self) -> Vec<TransferProgress> {
        let progresses = self.progresses.read().await;
        progresses.iter().filter_map(|(key, p)| {
            if key.starts_with("recv:") && p.status != "pending_confirm" && progresses.contains_key(&key["recv:".len()..]) {
                return None;
            }
            let mut p = p.clone();
            p.speed = p.speed_bps();
            p.eta_secs = p.eta().as_secs();
            p.elapsed_secs = p.start_time.elapsed().as_secs();
            Some(p)
        }).collect()
    }
}
