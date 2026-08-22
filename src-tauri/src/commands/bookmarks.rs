use tauri::State;
use tracing::instrument;

use crate::AppState;
use crate::core::models::{Bookmark, BookmarkInput};

#[tauri::command]
#[instrument(skip(state))]
pub async fn bookmarks_list(state: State<'_, AppState>) -> Result<Vec<Bookmark>, String> {
    crate::core::bookmark_service::list_bookmarks(&state.db).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn bookmarks_create(
    state: State<'_, AppState>,
    input: BookmarkInput,
) -> Result<Bookmark, String> {
    crate::core::bookmark_service::create_bookmark(&state.db, input).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn bookmarks_delete(state: State<'_, AppState>, id: String) -> Result<(), String> {
    crate::core::bookmark_service::delete_bookmark(&state.db, &id).await
}
