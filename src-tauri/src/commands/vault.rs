use tauri::State;
use tracing::instrument;

use crate::AppState;
use crate::core::models::Vault;
use crate::core::vault_service;

#[tauri::command]
#[instrument(skip(state))]
pub async fn vault_list(state: State<'_, AppState>) -> Result<Vec<Vault>, String> {
    vault_service::list_vaults(&state.db).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn vault_current(state: State<'_, AppState>) -> Result<Vault, String> {
    let id = vault_service::get_current_vault_id(&state.db).await?;
    vault_service::get_vault(&state.db, &id).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn vault_create(state: State<'_, AppState>, name: String) -> Result<Vault, String> {
    vault_service::create_vault(&state.db, &name).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn vault_rename(state: State<'_, AppState>, id: String, name: String) -> Result<Vault, String> {
    vault_service::rename_vault(&state.db, &id, &name).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn vault_delete(state: State<'_, AppState>, id: String) -> Result<(), String> {
    vault_service::delete_vault(&state.db, &id).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn vault_set_current(state: State<'_, AppState>, id: String) -> Result<Vault, String> {
    vault_service::set_current_vault_id(&state.db, &id).await?;
    vault_service::get_vault(&state.db, &id).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn vault_export(state: State<'_, AppState>, id: String, target_dir: String) -> Result<usize, String> {
    vault_service::export_vault(&state.db, &id, &target_dir).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn vault_import(
    state: State<'_, AppState>,
    id: String,
    source_dir: String,
) -> Result<serde_json::Value, String> {
    vault_service::import_vault(&state.db, &id, &source_dir).await
}
