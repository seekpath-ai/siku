use tauri::State;
use tracing::instrument;

use crate::core::models::{Paper, Tag};
use crate::AppState;

#[tauri::command]
#[instrument(skip(state))]
pub async fn tags_list(state: State<'_, AppState>) -> Result<Vec<Tag>, String> {
    crate::core::tag_service::list_tags(&state.db).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn tags_get(state: State<'_, AppState>, id: String) -> Result<Tag, String> {
    crate::core::tag_service::get_tag(&state.db, &id).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn tags_create(
    state: State<'_, AppState>,
    name: String,
    color: Option<String>,
) -> Result<Tag, String> {
    crate::core::tag_service::create_tag(&state.db, &name, color.as_deref()).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn tags_delete(state: State<'_, AppState>, id: String) -> Result<(), String> {
    crate::core::tag_service::delete_tag(&state.db, &id).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn tags_update(
    state: State<'_, AppState>,
    id: String,
    name: Option<String>,
    color: Option<String>,
) -> Result<Tag, String> {
    crate::core::tag_service::update_tag(&state.db, &id, name.as_deref(), color.as_deref()).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn tags_papers(state: State<'_, AppState>, paper_id: String) -> Result<Vec<Tag>, String> {
    crate::core::tag_service::get_paper_tags(&state.db, &paper_id).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn tags_add_to_paper(
    state: State<'_, AppState>,
    paper_id: String,
    tag_ids: Vec<String>,
) -> Result<(), String> {
    crate::core::tag_service::add_tags_to_paper(&state.db, &paper_id, &tag_ids).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn tags_remove_from_paper(
    state: State<'_, AppState>,
    paper_id: String,
    tag_ids: Vec<String>,
) -> Result<(), String> {
    crate::core::tag_service::remove_tags_from_paper(&state.db, &paper_id, &tag_ids).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn tags_list_papers(
    state: State<'_, AppState>,
    tag_id: String,
    sort_by: Option<String>,
    sort_order: Option<String>,
) -> Result<Vec<Paper>, String> {
    crate::core::tag_service::list_papers_by_tag(&state.db, &tag_id, sort_by.as_deref(), sort_order.as_deref()).await
}
