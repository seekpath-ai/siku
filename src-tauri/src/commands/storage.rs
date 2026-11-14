use tauri::State;

use crate::core::storage_service::{audit_storage, StorageAudit};
use crate::AppState;

/// Read-only local storage audit. Never deletes or moves files.
#[tauri::command]
pub async fn storage_audit(state: State<'_, AppState>) -> Result<StorageAudit, String> {
    audit_storage(&state.db, &state.app_data_dir).await
}
