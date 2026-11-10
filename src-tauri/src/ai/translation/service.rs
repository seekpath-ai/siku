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
        ChatMessage { reasoning_content: None,
            role: "system".to_string(),
            content: TRANSLATION_SYSTEM_PROMPT.to_string(),
            attachments: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        },
        ChatMessage { reasoning_content: None,
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

    // 空译文不得写入缓存：思考模型的推理 token 与正文共享 max_tokens，耗尽时
    // content 为空（finish_reason="length"）。半截/空结果入缓存会毒化后续命中。
    if resp.content.trim().is_empty() {
        return Err(format!(
            "translation produced no content (finish_reason={})：可能是思考模型的推理耗尽了输出额度，请调高该模型的 max_tokens 后重试",
            resp.finish_reason.as_deref().unwrap_or("未知"),
        ));
    }

    // Cache the result
    let _ = cache::store(db, text, target, &resp.content, &model).await;

    Ok(resp.content)
}

/// Stream-translate text. Events are forwarded to the frontend as
/// `translation:event` payloads tagged with `request_id`:
///
/// - `{ "type": "delta",     "content": "<incremental text>" }` while generating
/// - `{ "type": "reasoning", "content": "<thinking>" }` while the model reasons
///   (activity signal for the frontend idle guard; not displayable output)
/// - `{ "type": "done",      "content": "<full translation>" }` on success
/// - `{ "type": "error",     "content": "<message>" }` on failure
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

    // Forward deltas to the frontend while accumulating the full text.
    // Reasoning events are forwarded too (never accumulated): a thinking
    // model can spend minutes reasoning before the first translated token,
    // and the frontend's idle guard needs the activity signal to not
    // mistake that silence for a hung stream.
    let app_clone = app_handle.clone();
    let rid = request_id.to_string();
    let accumulator = tokio::spawn(async move {
        let mut full = String::new();
        let mut finish: Option<String> = None;
        while let Some(event) = rx.recv().await {
            // finish 事件只记录不转发：结束原因在下方统一判定。
            if event.event_type == "finish" {
                finish = event.content;
                continue;
            }
            let kind = match event.event_type.as_str() {
                "delta" => "delta",
                "reasoning" => "reasoning",
                _ => continue,
            };
            if let Some(content) = event.content {
                if kind == "delta" {
                    full.push_str(&content);
                }
                let _ = app_clone.emit(
                    "translation:event",
                    serde_json::json!({
                        "request_id": rid,
                        "type": kind,
                        "content": content,
                    }),
                );
            }
        }
        (full, finish)
    });

    if let Err(e) = client.chat_completion_stream(&messages, &[], tx).await {
        let _ = app_handle.emit(
            "translation:event",
            serde_json::json!({ "request_id": request_id, "type": "error", "content": e }),
        );
        return Err(format!("translation stream failed: {e}"));
    }

    let (full, finish) = accumulator.await.unwrap_or_default();
    // 输出被额度截断（finish_reason="length"）：半截译文不能缓存也不能当结果。
    if finish.as_deref() == Some("length") {
        let msg = "译文被输出额度截断：模型的 max_tokens 已用尽（思考模型的推理也占用该额度），请调高该模型的 max_tokens 后重试";
        let _ = app_handle.emit(
            "translation:event",
            serde_json::json!({
                "request_id": request_id,
                "type": "error",
                "content": msg,
            }),
        );
        return Err(msg.to_string());
    }
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

/// Look up cached translations for a batch of texts without calling the LLM.
/// Used by the reader's compare pane to warm already-translated paragraphs.
pub async fn lookup_cached_batch(
    db: &SqlitePool,
    texts: &[String],
    target_lang: Option<&str>,
) -> Result<Vec<Option<String>>, String> {
    let target = target_lang.unwrap_or("zh");
    let llm_config = crate::core::settings_service::load_llm_config(db).await?;
    let mut out = Vec::with_capacity(texts.len());
    for text in texts {
        out.push(cache::lookup(db, text, target, &llm_config.model).await?);
    }
    Ok(out)
}

/// Batch-translate texts with bounded concurrency, emitting one event per
/// item as `translation:batch-event` payloads tagged with `request_id`:
///
/// - `{ "type": "item",       "index": n, "content": "<translation>" }`
/// - `{ "type": "item-error", "index": n, "content": "<message>" }`
/// - `{ "type": "done", "translated": n, "failed": m, "cancelled": bool }`
///
/// Cache hits are resolved synchronously before any LLM call and emit an
/// `item` event immediately. A failed item never aborts the batch.
#[instrument(skip(db, app_handle, texts, cancel))]
pub async fn translate_texts_batch(
    app_handle: &tauri::AppHandle,
    db: &SqlitePool,
    texts: Vec<String>,
    target_lang: Option<&str>,
    request_id: &str,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<(), String> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let target = target_lang.unwrap_or("zh").to_string();
    let source = "auto";

    let llm_config = crate::core::settings_service::load_llm_config(db).await?;
    if llm_config.api_key.is_empty() && llm_config.provider != llm::LlmProvider::Ollama {
        return Err("API key not configured. Please set it in Settings.".to_string());
    }
    let model = llm_config.model.clone();
    let client: Arc<dyn llm::LlmClient> = Arc::from(
        llm::client::create_llm_client(&llm_config)
            .map_err(|e| format!("failed to create LLM client: {e}"))?,
    );

    let emit = |payload: serde_json::Value| {
        let mut p = payload;
        p["request_id"] = serde_json::Value::String(request_id.to_string());
        let _ = app_handle.emit("translation:batch-event", p);
    };

    let translated = Arc::new(AtomicUsize::new(0));
    let failed = Arc::new(AtomicUsize::new(0));
    let sem = Arc::new(tokio::sync::Semaphore::new(2));
    let mut set = tokio::task::JoinSet::new();

    for (index, text) in texts.into_iter().enumerate() {
        if cancel.is_cancelled() {
            break;
        }
        // Empty paragraphs need no call at all.
        if text.trim().is_empty() {
            emit(serde_json::json!({ "type": "item", "index": index, "content": "" }));
            continue;
        }
        // Cache hits resolve inline — no task, no LLM call.
        match cache::lookup(db, &text, &target, &model).await {
            Ok(Some(cached)) => {
                translated.fetch_add(1, Ordering::Relaxed);
                emit(serde_json::json!({ "type": "item", "index": index, "content": cached }));
                continue;
            }
            Ok(None) => {}
            Err(e) => {
                failed.fetch_add(1, Ordering::Relaxed);
                emit(serde_json::json!({ "type": "item-error", "index": index, "content": e }));
                continue;
            }
        }
        // Backpressure: wait for a slot before spawning the next task.
        let permit = match sem.clone().acquire_owned().await {
            Ok(p) => p,
            Err(_) => break,
        };
        let client = client.clone();
        let db = db.clone();
        let target = target.clone();
        let model = model.clone();
        let cancel = cancel.clone();
        let translated = translated.clone();
        let failed = failed.clone();
        let app_handle = app_handle.clone();
        let rid = request_id.to_string();
        set.spawn(async move {
            let _permit = permit;
            let emit = |payload: serde_json::Value| {
                let mut p = payload;
                p["request_id"] = serde_json::Value::String(rid);
                let _ = app_handle.emit("translation:batch-event", p);
            };
            if cancel.is_cancelled() {
                return;
            }
            let messages = build_translation_messages(&text, source, &target);
            let result = tokio::select! {
                _ = cancel.cancelled() => return,
                r = client.chat_completion(&messages, &[]) => r,
            };
            match result {
                Ok(resp)
                    if !resp.content.trim().is_empty()
                        && resp.finish_reason.as_deref() != Some("length") =>
                {
                    let _ = cache::store(&db, &text, &target, &resp.content, &model).await;
                    translated.fetch_add(1, Ordering::Relaxed);
                    emit(serde_json::json!({ "type": "item", "index": index, "content": resp.content }));
                }
                Ok(resp) => {
                    failed.fetch_add(1, Ordering::Relaxed);
                    emit(serde_json::json!({
                        "type": "item-error", "index": index,
                        "content": format!("译文为空或被输出额度截断（finish_reason={}），请调高模型 max_tokens 后重试",
                            resp.finish_reason.as_deref().unwrap_or("未知")),
                    }));
                }
                Err(e) => {
                    failed.fetch_add(1, Ordering::Relaxed);
                    emit(serde_json::json!({ "type": "item-error", "index": index, "content": e }));
                }
            }
        });
    }

    // Drain: on cancel, in-flight tasks observe the token via select! above.
    while set.join_next().await.is_some() {}

    emit(serde_json::json!({
        "type": "done",
        "translated": translated.load(Ordering::Relaxed),
        "failed": failed.load(Ordering::Relaxed),
        "cancelled": cancel.is_cancelled(),
    }));
    Ok(())
}
