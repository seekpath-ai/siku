use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

/// A running background task (e.g. bash in background mode).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskInfo {
    pub id: String,
    pub description: String,
    /// running | completed | failed | stopped | timed_out
    pub status: String,
    pub exit_code: Option<i32>,
    pub output_path: Option<String>,
    pub created_at: String,
}

/// Internal handle: task metadata + cancellation flag.
pub struct TaskHandle {
    pub info: TaskInfo,
    pub cancel: Arc<AtomicBool>,
}

pub type TaskStore = Arc<Mutex<HashMap<String, TaskHandle>>>;

pub fn new_task_store() -> TaskStore {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Snapshot of all task infos, newest first.
pub async fn snapshot(store: &TaskStore) -> Vec<TaskInfo> {
    let map = store.lock().await;
    let mut list: Vec<TaskInfo> = map.values().map(|h| h.info.clone()).collect();
    list.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    list
}
