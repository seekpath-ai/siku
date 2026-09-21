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
    /// 归属的会话 id（哪个 agent 会话启动了它），未知/旧数据为 None
    #[serde(default)]
    pub session_id: Option<String>,
    pub created_at: String,
    /// Log file size / last-write time, filled at snapshot time so the UI can
    /// tell "running but quiet" apart from "actually producing output".
    #[serde(default)]
    pub log_bytes: Option<u64>,
    #[serde(default)]
    pub log_modified: Option<String>,
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

/// Snapshot of all task infos, newest first. Log file size / mtime are
/// stat'ed live here (the stored handles only carry static metadata).
pub async fn snapshot(store: &TaskStore) -> Vec<TaskInfo> {
    let map = store.lock().await;
    let mut list: Vec<TaskInfo> = map.values().map(|h| h.info.clone()).collect();
    for t in &mut list {
        if let Some(path) = &t.output_path {
            if let Ok(meta) = std::fs::metadata(path) {
                t.log_bytes = Some(meta.len());
                t.log_modified = meta.modified().ok().map(|st| {
                    let dt: chrono::DateTime<chrono::Local> = st.into();
                    dt.to_rfc3339()
                });
            }
        }
    }
    list.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    list
}
