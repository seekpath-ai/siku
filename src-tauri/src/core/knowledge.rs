use sqlx::SqlitePool;
use crate::core::models::{KnowledgeDomain, KnowledgeItem};

pub async fn get_item(db: &SqlitePool, id: &str) -> Result<KnowledgeItem, String> {
    sqlx::query_as::<_, KnowledgeItem>("SELECT * FROM knowledge_items WHERE id = ?")
        .bind(id).fetch_optional(db).await.map_err(|e| format!("db: {e}"))?
        .ok_or_else(|| format!("item not found: {id}"))
}

pub async fn delete_item(db: &SqlitePool, id: &str) -> Result<(), String> {
    sqlx::query("DELETE FROM knowledge_items WHERE id = ?").bind(id)
        .execute(db).await.map_err(|e| format!("db: {e}"))?;
    Ok(())
}

/// Clean up orphan knowledge_items when a paper is deleted
pub async fn remove_by_source(db: &SqlitePool, source_type: &str, source_id: &str) -> Result<(), String> {
    sqlx::query("DELETE FROM knowledge_items WHERE source_type = ? AND source_id = ?")
        .bind(source_type).bind(source_id)
        .execute(db).await.map_err(|e| format!("db: {e}"))?;
    Ok(())
}
