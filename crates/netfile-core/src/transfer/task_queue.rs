use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferTask {
    pub task_id: String,
    pub file_id: String,
    pub file_name: String,
    pub file_size: u64,
    pub target_device: String,
    pub status: TaskStatus,
    pub progress: f64,
    pub error: Option<String>,
}

impl TransferTask {
    pub fn new(file_id: String, file_name: String, file_size: u64, target_device: String) -> Self {
        Self {
            task_id: Uuid::new_v4().to_string(),
            file_id,
            file_name,
            file_size,
            target_device,
            status: TaskStatus::Pending,
            progress: 0.0,
            error: None,
        }
    }
}

pub struct TaskQueue {
    tasks: Arc<RwLock<VecDeque<TransferTask>>>,
    max_concurrent: Arc<RwLock<usize>>,
}

impl TaskQueue {
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            tasks: Arc::new(RwLock::new(VecDeque::new())),
            max_concurrent: Arc::new(RwLock::new(max_concurrent)),
        }
    }

    pub async fn update_max_concurrent(&self, n: usize) {
        *self.max_concurrent.write().await = n;
    }

    pub async fn add_task(&self, task: TransferTask) {
        let mut tasks = self.tasks.write().await;
        tasks.push_back(task);
    }

    pub async fn get_next_pending(&self) -> Option<TransferTask> {
        let mut tasks = self.tasks.write().await;
        for task in tasks.iter_mut() {
            if task.status == TaskStatus::Pending {
                task.status = TaskStatus::InProgress;
                return Some(task.clone());
            }
        }
        None
    }

    pub async fn update_task_progress(&self, task_id: &str, progress: f64) {
        let mut tasks = self.tasks.write().await;
        if let Some(task) = tasks.iter_mut().find(|t| t.task_id == task_id) {
            task.progress = progress;
        }
    }

    pub async fn complete_task(&self, task_id: &str) {
        let mut tasks = self.tasks.write().await;
        if let Some(task) = tasks.iter_mut().find(|t| t.task_id == task_id) {
            task.status = TaskStatus::Completed;
            task.progress = 1.0;
        }
    }

    pub async fn fail_task(&self, task_id: &str, error: String) {
        let mut tasks = self.tasks.write().await;
        if let Some(task) = tasks.iter_mut().find(|t| t.task_id == task_id) {
            task.status = TaskStatus::Failed;
            task.error = Some(error);
        }
    }

    pub async fn cancel_task(&self, task_id: &str) {
        let mut tasks = self.tasks.write().await;
        if let Some(task) = tasks.iter_mut().find(|t| t.task_id == task_id) {
            task.status = TaskStatus::Cancelled;
        }
    }

    pub async fn list_tasks(&self) -> Vec<TransferTask> {
        let tasks = self.tasks.read().await;
        tasks.iter().cloned().collect()
    }

    pub async fn get_task(&self, task_id: &str) -> Option<TransferTask> {
        let tasks = self.tasks.read().await;
        tasks.iter().find(|t| t.task_id == task_id).cloned()
    }

    pub async fn active_count(&self) -> usize {
        let tasks = self.tasks.read().await;
        tasks
            .iter()
            .filter(|t| t.status == TaskStatus::InProgress)
            .count()
    }

    pub async fn can_start_new(&self) -> bool {
        self.active_count().await < *self.max_concurrent.read().await
    }
}
