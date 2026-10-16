//! AI 综述生成 commands: outline generation + pipeline lifecycle.

use tauri::{AppHandle, State};

use crate::core::review_gen::{self, ReviewSection};
use crate::AppState;

/// Step 1: design the section outline for the selected paper set. The dialog
/// lets the user edit it before calling review_generate_start.
#[tauri::command]
pub async fn review_generate_outline(
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
