use tauri::State;
use tracing::instrument;

use crate::core::llm_provider_service::{self, LlmProviderInput};
use crate::core::models::LlmProvider;
use crate::AppState;

#[tauri::command]
#[instrument(skip(state))]
pub async fn llm_provider_list(state: State<'_, AppState>) -> Result<Vec<LlmProvider>, String> {
    llm_provider_service::list_providers(&state.db).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn llm_provider_get(state: State<'_, AppState>, id: String) -> Result<LlmProvider, String> {
    llm_provider_service::get_provider(&state.db, &id).await
}

#[tauri::command]
#[instrument(skip(state, input))]
pub async fn llm_provider_create(
    state: State<'_, AppState>,
    input: LlmProviderInput,
) -> Result<LlmProvider, String> {
    llm_provider_service::create_provider(&state.db, input).await
}

#[tauri::command]
#[instrument(skip(state, input))]
pub async fn llm_provider_update(
    state: State<'_, AppState>,
    id: String,
    input: LlmProviderInput,
) -> Result<LlmProvider, String> {
    llm_provider_service::update_provider(&state.db, &id, input).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn llm_provider_delete(state: State<'_, AppState>, id: String) -> Result<(), String> {
    llm_provider_service::delete_provider(&state.db, &id).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn llm_provider_set_default(
    state: State<'_, AppState>,
    id: String,
) -> Result<LlmProvider, String> {
    llm_provider_service::set_default_provider(&state.db, &id).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn llm_provider_validate(
    state: State<'_, AppState>,
    id: String,
) -> Result<bool, String> {
    let block = llm_provider_service::resolve_block(&state.db, &id).await?;
    let config = block.to_llm_config();
    crate::core::settings_service::validate_llm_config(&config).await
}
