use sqlx::SqlitePool;
use tracing::instrument;
use crate::core::models::{Annotation, AnnotationInput};
use crate::core::time;

#[instrument(skip(db))]
pub async fn list_by_paper(db: &SqlitePool, paper_id: &str) -> Result<Vec<Annotation>, String> {
    sqlx::query_as::<_, Annotation>(
        "SELECT id, paper_id, page, type, rect, color, text, note, tags, translation, created_at, updated_at
         FROM annotations WHERE paper_id = ? ORDER BY page, created_at"
    )
    .bind(paper_id)
    .fetch_all(db)
    .await
    .map_err(|e| format!("failed to list annotations: {e}"))
}

#[instrument(skip(db))]
pub async fn create(db: &SqlitePool, input: &AnnotationInput) -> Result<Annotation, String> {
    let id = input.id.clone().unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let now = time::now_iso();
    let rect = serde_json::to_string(&input.rect).map_err(|e| format!("rect json: {e}"))?;
    let tags = serde_json::to_string(&input.tags).unwrap_or_else(|_| "[]".into());

    sqlx::query(
        "INSERT INTO annotations (id, paper_id, page, type, rect, color, text, note, tags, created_at, updated_at)
         VALUES (?, ?, ?, 'snippet', ?, '#ffeb3b', ?, ?, ?, ?, ?)"
    )
    .bind(&id)
    .bind(&input.paper_id)
    .bind(input.page)
    .bind(&rect)
    .bind(&input.text)
    .bind(&input.note)
    .bind(&tags)
    .bind(&now)
    .bind(&now)
    .execute(db)
    .await
    .map_err(|e| format!("failed to create annotation: {e}"))?;

    get_by_id(db, &id).await
}

#[instrument(skip(db))]
pub async fn get_by_id(db: &SqlitePool, id: &str) -> Result<Annotation, String> {
    sqlx::query_as::<_, Annotation>(
        "SELECT id, paper_id, page, type, rect, color, text, note, tags, translation, created_at, updated_at
         FROM annotations WHERE id = ?"
    )
    .bind(id)
    .fetch_one(db)
    .await
    .map_err(|e| format!("annotation not found: {e}"))
}

#[instrument(skip(db))]
pub async fn update_note(db: &SqlitePool, id: &str, note: &str) -> Result<Annotation, String> {
    let now = time::now_iso();
    sqlx::query("UPDATE annotations SET note = ?, updated_at = ? WHERE id = ?")
        .bind(note)
        .bind(&now)
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| format!("failed to update note: {e}"))?;
    get_by_id(db, id).await
}

#[instrument(skip(db))]
pub async fn update_tags(db: &SqlitePool, id: &str, tags: &[String]) -> Result<Annotation, String> {
    let now = time::now_iso();
    let tags_json = serde_json::to_string(tags).map_err(|e| format!("tags json: {e}"))?;
    sqlx::query("UPDATE annotations SET tags = ?, updated_at = ? WHERE id = ?")
        .bind(&tags_json)
        .bind(&now)
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| format!("failed to update tags: {e}"))?;
    get_by_id(db, id).await
}

#[instrument(skip(db))]
pub async fn update_translation(db: &SqlitePool, id: &str, translation: &str) -> Result<Annotation, String> {
    let now = time::now_iso();
    sqlx::query("UPDATE annotations SET translation = ?, updated_at = ? WHERE id = ?")
        .bind(translation)
        .bind(&now)
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| format!("failed to update translation: {e}"))?;
    get_by_id(db, id).await
}

#[instrument(skip(db))]
pub async fn delete(db: &SqlitePool, id: &str) -> Result<(), String> {
    sqlx::query("DELETE FROM annotations WHERE id = ?")
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| format!("failed to delete annotation: {e}"))?;
    Ok(())
}

#[instrument(skip(db))]
pub async fn clear_paper(db: &SqlitePool, paper_id: &str) -> Result<(), String> {
    sqlx::query("DELETE FROM annotations WHERE paper_id = ?")
        .bind(paper_id)
        .execute(db)
        .await
        .map_err(|e| format!("failed to clear annotations: {e}"))?;
    Ok(())
}
