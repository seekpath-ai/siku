use tauri::State;
use tracing::instrument;

use crate::AppState;

/// Resolve a device-independent `siku://artifact/<hash>.<ext>` reference to
/// this device's absolute blob path. Errors while the blob is still being
/// fetched from the peer ("产物尚未同步到本机").
#[tauri::command]
#[instrument(skip(state))]
pub async fn artifact_resolve(state: State<'_, AppState>, uri: String) -> Result<String, String> {
    crate::core::artifact_service::resolve_uri(&state.app_data_dir, &uri)
}
