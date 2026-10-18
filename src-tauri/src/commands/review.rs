use tauri::{AppHandle, State};

use crate::core::review_gen;
use crate::core::review_store::{self, ReviewPaperCard, ReviewSection, ReviewSnapshot};
use crate::AppState;

#[tauri::command]
pub async fn review_create(
    state: State<'_, AppState>,
    paper_ids: Vec<String>,
    question: String,
    scope: String,
    language: String,
    target_words: u32,
    provider_id: Option<String>,
) -> Result<ReviewSnapshot, String> {
    review_gen::create(
        &state.db,
        &paper_ids,
        &question,
        &scope,
        &language,
        target_words,
        provider_id.as_deref(),
    )
    .await
}

#[tauri::command]
pub async fn review_get(state: State<'_, AppState>, run_id: String) -> Result<ReviewSnapshot, String> {
    review_gen::get(&state.db, &run_id).await
}

#[tauri::command]
pub async fn review_read_start(
    app: AppHandle,
    state: State<'_, AppState>,
    run_id: String,
) -> Result<(), String> {
    review_store::load_state(&state.db, &run_id).await?;
    let db = state.db.clone();
    tauri::async_runtime::spawn(async move {
        review_gen::run_reading(app, db, run_id).await;
    });
    Ok(())
}

#[tauri::command]
pub async fn review_read_retry(
    app: AppHandle,
    state: State<'_, AppState>,
    run_id: String,
    paper_id: String,
) -> Result<(), String> {
    review_gen::prepare_read_retry(&state.db, &run_id, &paper_id).await?;
    let db = state.db.clone();
    tauri::async_runtime::spawn(async move {
        review_gen::run_reading(app, db, run_id).await;
    });
    Ok(())
}

#[tauri::command]
pub async fn review_card_update(
    state: State<'_, AppState>,
    run_id: String,
    paper_id: String,
    card: ReviewPaperCard,
) -> Result<ReviewSnapshot, String> {
    review_gen::update_card(&state.db, &run_id, &paper_id, card).await
}

#[tauri::command]
pub async fn review_generate_outline(
    app: AppHandle,
    state: State<'_, AppState>,
    run_id: String,
) -> Result<Vec<ReviewSection>, String> {
    review_gen::generate_outline(&app, &state.db, &run_id).await
}

#[tauri::command]
pub async fn review_generate_start(
    app: AppHandle,
    state: State<'_, AppState>,
    run_id: String,
    outline: Vec<ReviewSection>,
) -> Result<(), String> {
    review_gen::prepare_generation(&state.db, &run_id, &outline).await?;
    let db = state.db.clone();
    tauri::async_runtime::spawn(async move {
        review_gen::run_generation(app, db, run_id).await;
    });
    Ok(())
}

#[tauri::command]
pub async fn review_generate_retry_section(
    app: AppHandle,
    state: State<'_, AppState>,
    run_id: String,
    section_index: usize,
) -> Result<(), String> {
    review_gen::prepare_section_retry(&state.db, &run_id, section_index).await?;
    let db = state.db.clone();
    tauri::async_runtime::spawn(async move {
        review_gen::run_generation(app, db, run_id).await;
    });
    Ok(())
}

#[tauri::command]
pub async fn review_generate_cancel(state: State<'_, AppState>, run_id: String) -> Result<(), String> {
    review_gen::request_cancel(&state.db, &run_id).await
}

#[tauri::command]
pub async fn review_generate_save(
    app: AppHandle,
    state: State<'_, AppState>,
    run_id: String,
) -> Result<String, String> {
    review_gen::save_review(&app, &state.db, &run_id).await
}
