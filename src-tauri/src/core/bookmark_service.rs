use sqlx::SqlitePool;
use crate::core::models::{Bookmark, BookmarkInput};
use crate::core::time;

pub async fn list_bookmarks(db: &SqlitePool) -> Result<Vec<Bookmark>, String> {
    sqlx::query_as::<_, Bookmark>(
        "SELECT id, title, route, params_json, created_at FROM bookmarks ORDER BY created_at DESC"
    )
    .fetch_all(db)
    .await
    .map_err(|e| format!("db: {e}"))
}

pub async fn create_bookmark(db: &SqlitePool, input: BookmarkInput) -> Result<Bookmark, String> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = time::now_iso();
    let params_json = input.params_json.unwrap_or_else(|| "{}".to_string());

    sqlx::query(
        "INSERT INTO bookmarks (id, title, route, params_json, created_at) VALUES (?, ?, ?, ?, ?)"
    )
    .bind(&id)
    .bind(&input.title)
    .bind(&input.route)
    .bind(&params_json)
    .bind(&now)
    .execute(db)
    .await
    .map_err(|e| format!("db: {e}"))?;

    sqlx::query_as::<_, Bookmark>(
        "SELECT id, title, route, params_json, created_at FROM bookmarks WHERE id = ?"
    )
    .bind(&id)
    .fetch_one(db)
    .await
    .map_err(|e| format!("db: {e}"))
}

pub async fn delete_bookmark(db: &SqlitePool, id: &str) -> Result<(), String> {
    sqlx::query("DELETE FROM bookmarks WHERE id = ?")
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    Ok(())
}
