use tauri::{AppHandle, State};
use tracing::instrument;

use crate::AppState;

/// Translate a single text segment (uses cache)
#[tauri::command]
#[instrument(skip(state))]
pub async fn translate_text(
    state: State<'_, AppState>,
    text: String,
    source_lang: Option<String>,
    target_lang: Option<String>,
) -> Result<String, String> {
    crate::ai::translation::service::translate_text(
        &state.db,
        &text,
        source_lang.as_deref(),
        target_lang.as_deref(),
    )
    .await
}

/// Stream-translate a single text segment (uses cache).
///
/// Deltas are emitted as `translation:event` payloads tagged with
/// `request_id` for live display; the full translation is returned as the
/// command result. See `translate_text_stream` in the service module.
#[tauri::command]
#[instrument(skip(state, app_handle))]
pub async fn translate_text_stream(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    text: String,
    source_lang: Option<String>,
    target_lang: Option<String>,
    request_id: String,
) -> Result<String, String> {
    crate::ai::translation::service::translate_text_stream(
        &app_handle,
        &state.db,
        &text,
        source_lang.as_deref(),
        target_lang.as_deref(),
        &request_id,
    )
    .await
}

/// Clear translation cache
#[tauri::command]
#[instrument(skip(state))]
pub async fn translation_clear_cache(
    state: State<'_, AppState>,
    model: Option<String>,
) -> Result<u64, String> {
    crate::ai::translation::cache::clear(&state.db, model.as_deref()).await
}
