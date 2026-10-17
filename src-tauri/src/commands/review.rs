//! AI 综述生成 commands: outline generation + pipeline lifecycle.

use tauri::{AppHandle, State};

use crate::core::review_gen::{self, ReviewSection};
use crate::AppState;

/// Step 1: design the section outline for the selected paper set. The dialog
/// lets the user edit it before calling review_generate_start.
#[tauri::command]
pub async fn review_generate_outline(
    app: AppHandle,
    state: State<'_, AppState>,
    paper_ids: Vec<String>,
    question: String,
    scope: String,
    language: String,
    target_words: u32,
    provider_id: Option<String>,
) -> Result<Vec<ReviewSection>, String> {
    if paper_ids.is_empty() {
        return Err("未选择文献".to_string());
    }
    if question.trim().is_empty() {
        return Err("请填写研究问题".to_string());
    }
    review_gen::generate_outline(
        &app,
        &state.db,
        &paper_ids,
        question.trim(),
        scope.trim(),
        &language,
        target_words,
        provider_id.as_deref(),
    )
    .await
}

/// Step 2: run the section-by-section pipeline (spawned; progress on
/// `library:review-progress`; the terminal event carries the new note id).
#[tauri::command]
pub async fn review_generate_start(
    app: AppHandle,
    state: State<'_, AppState>,
    paper_ids: Vec<String>,
    question: String,
    language: String,
    target_words: u32,
    outline: Vec<ReviewSection>,
    provider_id: Option<String>,
) -> Result<(), String> {
    if paper_ids.is_empty() {
        return Err("未选择文献".to_string());
    }
    if outline.is_empty() {
        return Err("大纲为空".to_string());
    }
    let db = state.db.clone();
    tauri::async_runtime::spawn(async move {
        review_gen::run_review(app, db, paper_ids, question, language, target_words, outline, provider_id).await;
    });
    Ok(())
}

#[tauri::command]
pub async fn review_generate_cancel() -> Result<(), String> {
    review_gen::request_cancel();
    Ok(())
}

/// Retry ONE failed section (1-based index) with a doubled output budget.
/// Spawned: a section call can take minutes, and progress arrives on
/// `library:review-progress` like every other beat.
#[tauri::command]
pub async fn review_generate_retry_section(
    app: AppHandle,
    state: State<'_, AppState>,
    section_index: usize,
) -> Result<(), String> {
    let db = state.db.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = review_gen::retry_section(app, db, section_index).await {
            // The per-section failure event was already emitted by the core;
            // this is just the tracing trail.
            tracing::warn!(section_index, error = %e, "review section retry did not succeed");
        }
    });
    Ok(())
}

/// Save the note from whatever sections exist (placeholders for the missing
/// ones). Async because it writes a note, but fast enough to await.
#[tauri::command]
pub async fn review_generate_save(app: AppHandle, state: State<'_, AppState>) -> Result<String, String> {
    review_gen::save_review(app, state.db.clone()).await
}
