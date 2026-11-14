use tauri::State;

use crate::file_store;
use crate::core::storage_service::{
    audit_storage, empty_trash, gc_storage, StorageAudit, StorageGcResult, StorageTrashResult,
};
use crate::AppState;

/// Read-only local storage audit. Never deletes or moves files.
#[tauri::command]
pub async fn storage_audit(state: State<'_, AppState>) -> Result<StorageAudit, String> {
    audit_storage(&state.db, &state.app_data_dir).await
}

/// Conservative GC: move old, locally-unreferenced blobs into `blobs/.trash`.
/// `dry_run` returns the selection without touching the filesystem.
#[tauri::command]
pub async fn storage_gc(
    state: State<'_, AppState>,
    dry_run: bool,
    older_than_days: u64,
) -> Result<StorageGcResult, String> {
    gc_storage(&state.db, &state.app_data_dir, older_than_days, dry_run).await
}

/// Ensure `blobs/.trash` exists and reveal it. Frontend must not concatenate
/// the path itself: Windows Explorer treats a mixed `\` + `/` path as an
/// invalid location and falls back to Documents.
#[tauri::command]
pub fn storage_open_trash(state: State<'_, AppState>) -> Result<(), String> {
    let trash = file_store::blob_dir(&state.app_data_dir).join(".trash");
    std::fs::create_dir_all(&trash).map_err(|e| format!("创建回收站目录失败：{e}"))?;
    crate::commands::system::reveal_in_file_manager(trash.to_string_lossy().into_owned())
}

/// Permanently empty `blobs/.trash`. The UI must confirm this separately.
#[tauri::command]
pub async fn storage_empty_trash(
    state: State<'_, AppState>,
) -> Result<StorageTrashResult, String> {
    let dir = state.app_data_dir.clone();
    tokio::task::spawn_blocking(move || empty_trash(&dir))
        .await
        .map_err(|e| format!("trash cleanup task failed: {e}"))?
}
