use std::sync::atomic::Ordering;
use tauri::State;
use tracing::instrument;

use crate::AppState;
use crate::core::tasks::TaskInfo;

/// 读取任务输出日志的上限（字符数）。按字符截断，避免按字节切坏 UTF-8。
const OUTPUT_MAX_CHARS: usize = 64 * 1024;

/// 任务中心：所有后台任务的快照（新任务在前）。
#[tauri::command]
#[instrument(skip(state))]
pub async fn task_snapshot(state: State<'_, AppState>) -> Result<Vec<TaskInfo>, String> {
    // tauri 要求带引用的 async 命令必须返回 Result；快照本身不会失败
    Ok(crate::core::tasks::snapshot(&state.tasks).await)
}

/// 任务中心：读取某个后台任务的输出日志（最多 64K 字符）。
#[tauri::command]
#[instrument(skip(state))]
pub async fn task_output(state: State<'_, AppState>, id: String) -> Result<serde_json::Value, String> {
    let info = {
        let map = state.tasks.lock().await;
        map.get(&id)
            .map(|h| h.info.clone())
            .ok_or_else(|| format!("task not found: {id}"))?
    };

    let (mut content, mut truncated) = (String::new(), false);
    if let Some(path) = &info.output_path {
        match std::fs::read_to_string(path) {
            Ok(raw) => {
                truncated = raw.chars().count() > OUTPUT_MAX_CHARS;
                content = raw.chars().take(OUTPUT_MAX_CHARS).collect();
            }
            // 日志读不到（比如文件已被清理）不算致命错误，把原因放进 content
            Err(e) => content = format!("(log read error: {e})"),
        }
    }

    Ok(serde_json::json!({
        "status": info.status,
        "exit_code": info.exit_code,
        "content": content,
        "truncated": truncated,
    }))
}

/// 任务中心：停止一个后台任务（置 cancel 标志，watcher 循环会真正 kill 进程）。
#[tauri::command]
#[instrument(skip(state))]
pub async fn task_stop(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let map = state.tasks.lock().await;
    match map.get(&id) {
        Some(handle) => {
            handle.cancel.store(true, Ordering::SeqCst);
            Ok(())
        }
        None => Err(format!("task not found: {id}")),
    }
}
