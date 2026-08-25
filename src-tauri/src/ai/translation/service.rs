use sqlx::SqlitePool;
use tauri::Emitter;
use tracing::instrument;

use crate::ai::llm::{self, ChatMessage};
use crate::ai::translation::cache;

/// Default system prompt for translation
const TRANSLATION_SYSTEM_PROMPT: &str = "\
You are a professional academic translator. Translate the following text accurately and fluently. \
Preserve technical terms, citations, and formatting. \
If the text contains mathematical formulas, preserve them exactly. \
Output ONLY the translation, no explanations or notes.";

/// Build the translation prompt messages
fn build_translation_messages(text: &str, source: &str, target: &str) -> Vec<ChatMessage> {
    let user_prompt = if source == "auto" {
        format!("Translate the following text to {target}:\n\n{text}", target = lang_name(target))
    } else {
        format!(
            "Translate the following text from {source} to {target}:\n\n{text}",
            source = lang_name(source),
            target = lang_name(target)
        )
    };

    vec![
        ChatMessage {
            role: "system".to_string(),
            content: TRANSLATION_SYSTEM_PROMPT.to_string(),
            attachments: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        },
        ChatMessage {
            role: "user".to_string(),
            content: user_prompt,
            attachments: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        },
    ]
}

/// Translate text, using cache if available
#[instrument(skip(db))]
pub async fn translate_text(
    db: &SqlitePool,
    text: &str,
    source_lang: Option<&str>,
    target_lang: Option<&str>,
) -> Result<String, String> {
    let target = target_lang.unwrap_or("zh");
    let source = source_lang.unwrap_or("auto");

    // Load LLM config
    let llm_config = crate::core::settings_service::load_llm_config(db).await?;
    let model = llm_config.model.clone();

    if llm_config.api_key.is_empty() && llm_config.provider != llm::LlmProvider::Ollama {
        return Err("API key not configured. Please set it in Settings.".to_string());
    }

    // Check cache first
    if let Some(cached) = cache::lookup(db, text, target, &model).await? {
        tracing::info!(target, "translation cache hit");
        return Ok(cached);
    }

    // Call LLM
    let client = llm::client::create_llm_client(&llm_config)
        .map_err(|e| format!("failed to create LLM client: {e}"))?;

    let messages = build_translation_messages(text, source, target);
    let resp = client
        .chat_completion(&messages, &[])
        .await
        .map_err(|e| format!("translation failed: {e}"))?;

    // Cache the result
    let _ = cache::store(db, text, target, &resp.content, &model).await;

    Ok(resp.content)
}

/// Stream-translate text. Deltas are forwarded to the frontend as
/// `translation:event` payloads tagged with `request_id` for live display:
///
/// - `{ "type": "delta", "content": "<incremental text>" }` while generating
/// - `{ "type": "done",  "content": "<full translation>" }` on success
/// - `{ "type": "error", "content": "<message>" }` on failure
///
/// The full translation is ALSO returned as the command result, so the
/// frontend never depends on event delivery order for correctness.
///
/// Cache hits emit a single `done` event and return the cached text.
#[instrument(skip(db, app_handle))]
pub async fn translate_text_stream(
    app_handle: &tauri::AppHandle,
    db: &SqlitePool,
    text: &str,
    source_lang: Option<&str>,
    target_lang: Option<&str>,
    request_id: &str,
) -> Result<String, String> {
    let target = target_lang.unwrap_or("zh");
    let source = source_lang.unwrap_or("auto");

    // Load LLM config
    let llm_config = crate::core::settings_service::load_llm_config(db).await?;
    let model = llm_config.model.clone();

    if llm_config.api_key.is_empty() && llm_config.provider != llm::LlmProvider::Ollama {
        return Err("API key not configured. Please set it in Settings.".to_string());
    }

    // Check cache first
    if let Some(cached) = cache::lookup(db, text, target, &model).await? {
        tracing::info!(target, "translation cache hit (stream)");
        let _ = app_handle.emit(
            "translation:event",
            serde_json::json!({ "request_id": request_id, "type": "done", "content": cached }),
        );
        return Ok(cached);
    }

    // Stream LLM response
    let client = llm::client::create_llm_client(&llm_config)
        .map_err(|e| format!("failed to create LLM client: {e}"))?;

    let messages = build_translation_messages(text, source, target);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<llm::StreamEvent>();

    // Forward deltas to the frontend while accumulating the full text
    let app_clone = app_handle.clone();
    let rid = request_id.to_string();
    let accumulator = tokio::spawn(async move {
        let mut full = String::new();
        while let Some(event) = rx.recv().await {
            if event.event_type == "delta" {
                if let Some(content) = event.content {
                    full.push_str(&content);
                    let _ = app_clone.emit(
                        "translation:event",
                        serde_json::json!({
                            "request_id": rid,
                            "type": "delta",
                            "content": content,
                        }),
                    );
                }
            }
        }
        full
    });

    if let Err(e) = client.chat_completion_stream(&messages, &[], tx).await {
        let _ = app_handle.emit(
            "translation:event",
            serde_json::json!({ "request_id": request_id, "type": "error", "content": e }),
        );
        return Err(format!("translation stream failed: {e}"));
    }

    let full = accumulator.await.unwrap_or_default();
    if full.is_empty() {
        let _ = app_handle.emit(
            "translation:event",
            serde_json::json!({
                "request_id": request_id,
                "type": "error",
                "content": "translation produced no content",
            }),
        );
        return Err("translation produced no content".to_string());
    }

    // Cache the result
    let _ = cache::store(db, text, target, &full, &model).await;

    let _ = app_handle.emit(
        "translation:event",
        serde_json::json!({ "request_id": request_id, "type": "done", "content": full }),
    );
    Ok(full)
}

fn lang_name(code: &str) -> &str {
    match code {
        "zh" | "zh-CN" | "zh-TW" => "Chinese (中文)",
        "en" => "English",
        "ja" => "Japanese",
        "ko" => "Korean",
        "fr" => "French",
        "de" => "German",
        _ => code,
    }
}
