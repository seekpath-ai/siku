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

/// Look up cached translations for a batch of texts (no LLM calls).
#[tauri::command]
#[instrument(skip(state, texts))]
pub async fn translation_lookup_cached(
    state: State<'_, AppState>,
    texts: Vec<String>,
    target_lang: Option<String>,
) -> Result<Vec<Option<String>>, String> {
    crate::ai::translation::service::lookup_cached_batch(
        &state.db,
        &texts,
        target_lang.as_deref(),
    )
    .await
}

/// Batch-translate texts (reader full-text translation). Progress is emitted
/// as `translation:batch-event` payloads tagged with `request_id`; cancel via
/// `translation_cancel_batch`. Resolves when the batch finishes (or is
/// cancelled); per-item failures are reported as events, not command errors.
#[tauri::command]
#[instrument(skip(state, app_handle, texts))]
pub async fn translation_translate_batch(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    texts: Vec<String>,
    target_lang: Option<String>,
    request_id: String,
) -> Result<(), String> {
    let key = format!("translation-batch:{request_id}");
    let token = tokio_util::sync::CancellationToken::new();
    {
        let mut tokens = state.cancel_tokens.lock().await;
        tokens.insert(key.clone(), token.clone());
    }
    let result = crate::ai::translation::service::translate_texts_batch(
        &app_handle,
        &state.db,
        texts,
        target_lang.as_deref(),
        &request_id,
        token,
    )
    .await;
    state.cancel_tokens.lock().await.remove(&key);
    result
}

/// Cancel an in-flight batch translation.
#[tauri::command]
#[instrument(skip(state))]
pub async fn translation_cancel_batch(
    state: State<'_, AppState>,
    request_id: String,
) -> Result<(), String> {
    let key = format!("translation-batch:{request_id}");
    let tokens = state.cancel_tokens.lock().await;
    if let Some(token) = tokens.get(&key) {
        token.cancel();
    }
    Ok(())
}
