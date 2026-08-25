use tauri::State;
use tracing::instrument;
use crate::AppState;
use crate::core::models::FileItem;

#[tauri::command]
#[instrument(skip(state))]
pub async fn files_list(state: State<'_, AppState>, vault_id: String) -> Result<Vec<FileItem>, String> {
    crate::core::file_item_service::list_files(&state.db, &vault_id).await
}

/// Import a local file into the vault's managed file list (copies content
/// into the blob store).
#[tauri::command]
#[instrument(skip(state))]
pub async fn files_import(
    state: State<'_, AppState>,
    vault_id: String,
    source_path: String,
    parent_id: Option<String>,
) -> Result<FileItem, String> {
    crate::core::file_item_service::import_file(
        &state.db,
        &state.app_data_dir,
        &vault_id,
        parent_id.as_deref(),
        &source_path,
    )
    .await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn files_move(
    state: State<'_, AppState>,
    id: String,
    parent_id: Option<String>,
    sort_order: Option<i32>,
) -> Result<FileItem, String> {
    crate::core::file_item_service::move_file(&state.db, &id, parent_id.as_deref(), sort_order).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn files_rename(
    state: State<'_, AppState>,
    id: String,
    name: String,
) -> Result<FileItem, String> {
    crate::core::file_item_service::rename_file(&state.db, &id, &name).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn files_delete(state: State<'_, AppState>, id: String) -> Result<(), String> {
    crate::core::file_item_service::delete_file(&state.db, &id).await
}

/// Open a managed file with the system default application.
#[tauri::command]
#[instrument(skip(state))]
pub async fn files_open(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let path = crate::core::file_item_service::resolve_file_path(&state.db, &state.app_data_dir, &id).await?;
    crate::core::file_service::open_in_system(&path.to_string_lossy())
}
