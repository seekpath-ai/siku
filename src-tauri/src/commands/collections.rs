use tauri::State;
use tracing::instrument;

use crate::core::models::Collection;
use crate::AppState;

#[tauri::command]
#[instrument(skip(state))]
pub async fn collections_list(state: State<'_, AppState>) -> Result<Vec<Collection>, String> {
    crate::core::collection_service::list_collections(&state.db).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn collections_get(state: State<'_, AppState>, id: String) -> Result<Collection, String> {
    crate::core::collection_service::get_collection(&state.db, &id).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn collections_create(
    state: State<'_, AppState>,
    name: String,
    parent_id: Option<String>,
) -> Result<Collection, String> {
    crate::core::collection_service::create_collection(&state.db, &name, parent_id.as_deref()).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn collections_update(
    state: State<'_, AppState>,
    id: String,
    name: Option<String>,
    parent_id: Option<Option<String>>,
) -> Result<Collection, String> {
    crate::core::collection_service::update_collection(&state.db, &id, name.as_deref(), parent_id.as_ref().map(|o| o.as_deref())).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn collections_delete(state: State<'_, AppState>, id: String) -> Result<(), String> {
    crate::core::collection_service::delete_collection(&state.db, &id).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn collections_add_papers(
    state: State<'_, AppState>,
    collection_id: String,
    paper_ids: Vec<String>,
) -> Result<(), String> {
    crate::core::collection_service::add_papers_to_collection(&state.db, &collection_id, &paper_ids).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn collections_remove_papers(
    state: State<'_, AppState>,
    collection_id: String,
    paper_ids: Vec<String>,
) -> Result<(), String> {
    crate::core::collection_service::remove_papers_from_collection(&state.db, &collection_id, &paper_ids).await
}

/// Collections that a paper belongs to (Zotero-style "collections" display).
#[tauri::command]
#[instrument(skip(state))]
pub async fn paper_get_collections(
    state: State<'_, AppState>,
    paper_id: String,
) -> Result<Vec<crate::core::models::Collection>, String> {
    sqlx::query_as::<_, crate::core::models::Collection>(
        "SELECT c.id, c.name, c.parent_id, c.sort_order, c.created_at \
         FROM paper_collections pc JOIN collections c ON c.id = pc.collection_id \
         WHERE pc.paper_id = ? ORDER BY c.name"
    )
    .bind(paper_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| format!("db: {e}"))
}
