use tauri::State;
use tracing::instrument;
use crate::AppState;
use crate::core::models::Note;

#[tauri::command]
#[instrument(skip(state))]
pub async fn notes_create(
    state: State<'_, AppState>, title: String, content: String,
    paper_id: Option<String>, parent_id: Option<String>, is_folder: Option<bool>,
) -> Result<Note, String> {
    // The system library folder name is reserved for the paper-mapped tree.
    if is_folder.unwrap_or(false)
        && title.trim() == crate::core::note_service::SYSTEM_LIBRARY_NAME
    {
        return Err(format!(
            "「{}」为系统保留名称，不能创建同名目录",
            crate::core::note_service::SYSTEM_LIBRARY_NAME
        ));
    }
    let vault_id = crate::core::vault_service::get_current_vault_id(&state.db).await?;
    crate::core::note_service::create_note(&state.db, &title, &content, paper_id.as_deref(), parent_id.as_deref(), &vault_id, is_folder.unwrap_or(false)).await
}

/// Create a note under the paper's collection folder tree (Zotero-style).
#[tauri::command]
#[instrument(skip(state))]
pub async fn note_create_under_paper(
    state: State<'_, AppState>,
    paper_id: String,
    title: String,
    content: String,
) -> Result<Note, String> {
    let vault_id = crate::core::vault_service::get_current_vault_id(&state.db).await?;
    crate::core::note_service::create_note_under_paper(&state.db, &paper_id, &title, &content, &vault_id).await
}

/// Append an excerpt to the paper's excerpt note (titled with the paper's
/// title), creating it on first use. Subsequent excerpts merge-append.
#[tauri::command]
#[instrument(skip(state))]
pub async fn note_add_excerpt(
    state: State<'_, AppState>,
    paper_id: String,
    content: String,
) -> Result<Note, String> {
    let vault_id = crate::core::vault_service::get_current_vault_id(&state.db).await?;
    crate::core::note_service::add_excerpt_to_paper(&state.db, &paper_id, &content, &vault_id).await
}

/// Merge a standalone note into the paper's excerpt note, then delete it.
#[tauri::command]
#[instrument(skip(state))]
pub async fn note_merge_into_excerpt(
    state: State<'_, AppState>,
    note_id: String,
    paper_id: String,
) -> Result<Note, String> {
    let vault_id = crate::core::vault_service::get_current_vault_id(&state.db).await?;
    crate::core::note_service::merge_note_into_paper_note(&state.db, &note_id, &paper_id, &vault_id).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn notes_get(state: State<'_, AppState>, id: String) -> Result<Note, String> {
    crate::core::note_service::get_note(&state.db, &id).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn notes_update(
    state: State<'_, AppState>, id: String, title: Option<String>,
    content: Option<String>, paper_id: Option<String>, aliases: Option<String>, is_favorite: Option<i32>,
) -> Result<Note, String> {
    crate::core::note_service::update_note(&state.db, &id, title.as_deref(), content.as_deref(), paper_id.as_deref(), aliases.as_deref(), is_favorite).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn notes_delete(state: State<'_, AppState>, id: String) -> Result<(), String> {
    crate::core::note_service::delete_note(&state.db, &id).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn notes_list(
    state: State<'_, AppState>, paper_id: Option<String>,
    search: Option<String>, parent_id: Option<String>,
) -> Result<Vec<Note>, String> {
    crate::core::note_service::list_notes(&state.db, paper_id.as_deref(), search.as_deref(), parent_id.as_deref()).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn notes_list_all(state: State<'_, AppState>) -> Result<Vec<Note>, String> {
    let vault_id = crate::core::vault_service::get_current_vault_id(&state.db).await?;
    crate::core::note_service::list_all_notes(&state.db, &vault_id).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn notes_move(
    state: State<'_, AppState>,
    id: String,
    parent_id: Option<String>,
    sort_order: Option<i32>,
) -> Result<Note, String> {
    crate::core::note_service::move_note(&state.db, &id, parent_id.as_deref(), sort_order).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn notes_get_backlinks(
    state: State<'_, AppState>, note_id: String,
) -> Result<Vec<serde_json::Value>, String> {
    let vault_id = crate::core::vault_service::get_current_vault_id(&state.db).await?;
    let backlinks = crate::core::note_service::get_backlinks(&state.db, &note_id, &vault_id).await?;
    Ok(backlinks.into_iter().map(|(note, ctx)| serde_json::json!({
        "id": note.id, "title": note.title, "context": ctx,
        "created_at": note.created_at,
    })).collect())
}

/// Full-text search notes (ranked results with snippets).
#[tauri::command]
#[instrument(skip(state))]
pub async fn notes_search(
    state: State<'_, AppState>,
    query: String,
    limit: Option<i64>,
) -> Result<Vec<serde_json::Value>, String> {
    let vault_id = crate::core::vault_service::get_current_vault_id(&state.db).await?;
    crate::core::note_service::search_notes(&state.db, &query, limit.unwrap_or(30).clamp(1, 100), &vault_id).await
}

/// List version snapshots for a note (newest first).
#[tauri::command]
#[instrument(skip(state))]
pub async fn note_versions_list(
    state: State<'_, AppState>,
    note_id: String,
) -> Result<Vec<crate::core::models::NoteVersion>, String> {
    sqlx::query_as::<_, crate::core::models::NoteVersion>(
        "SELECT * FROM note_versions WHERE note_id = ? ORDER BY created_at DESC, rowid DESC"
    )
    .bind(note_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| format!("db: {e}"))
}

/// Restore a note from a version snapshot (title + content). The current
/// version is snapshotted first so the restore can itself be undone.
#[tauri::command]
#[instrument(skip(state))]
pub async fn note_version_restore(
    state: State<'_, AppState>,
    version_id: String,
) -> Result<Note, String> {
    let v = sqlx::query_as::<_, crate::core::models::NoteVersion>("SELECT * FROM note_versions WHERE id = ?")
        .bind(version_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| format!("db: {e}"))?
        .ok_or("version not found")?;

    // Snapshot the current content before overwriting (undo protection).
    let current: Option<(String, String)> = sqlx::query_as(
        "SELECT title, content FROM notes WHERE id = ?"
    )
    .bind(&v.note_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| format!("db: {e}"))?;
    if let Some((t, c)) = current {
        let now = crate::core::time::now_iso();
        sqlx::query(
            "INSERT INTO note_versions (id, note_id, title, content, edited_by, created_at) VALUES (?, ?, ?, ?, 'restore', ?)"
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(&v.note_id)
        .bind(&t)
        .bind(&c)
        .bind(&now)
        .execute(&state.db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    }

    crate::core::note_service::update_note(&state.db, &v.note_id, Some(&v.title), Some(&v.content), None, None, None).await
}
