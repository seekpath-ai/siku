use sqlx::SqlitePool;
use tracing::instrument;
use uuid::Uuid;

use crate::core::models::{Paper, Tag};
use crate::core::time::now_iso;

/// List all tags ordered by name, each with its paper count.
#[instrument(skip(db))]
pub async fn list_tags(db: &SqlitePool) -> Result<Vec<Tag>, String> {
    sqlx::query_as::<_, Tag>(
        "SELECT t.id, t.name, t.color, t.parent_id, t.created_at, \
         (SELECT COUNT(*) FROM paper_tags pt WHERE pt.tag_id = t.id) AS paper_count \
         FROM tags t ORDER BY t.name"
    )
    .fetch_all(db)
    .await
    .map_err(|e| format!("db: {e}"))
}

/// Create a new tag.
#[instrument(skip(db))]
pub async fn create_tag(
    db: &SqlitePool,
    name: &str,
    color: Option<&str>,
) -> Result<Tag, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("标签名不能为空".to_string());
    }
    // No UNIQUE(name) constraint anymore (CRDT-incompatible) — check here.
    let conflict: Option<(String,)> = sqlx::query_as("SELECT id FROM tags WHERE name = ?")
        .bind(trimmed)
        .fetch_optional(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    if conflict.is_some() {
        return Err(format!("已存在同名标签「{trimmed}」"));
    }
    let id = Uuid::new_v4().to_string();
    let now = now_iso();
    let color = color.unwrap_or("#3b82f6");

    sqlx::query(
        "INSERT INTO tags (id, name, color, created_at) VALUES (?, ?, ?, ?)"
    )
    .bind(&id)
    .bind(trimmed)
    .bind(color)
    .bind(&now)
    .execute(db)
    .await
    .map_err(|e| format!("db: {e}"))?;

    get_tag(db, &id).await
}

/// Get a single tag by ID.
#[instrument(skip(db))]
pub async fn get_tag(db: &SqlitePool, id: &str) -> Result<Tag, String> {
    sqlx::query_as::<_, Tag>(
        "SELECT t.id, t.name, t.color, t.parent_id, t.created_at, \
         (SELECT COUNT(*) FROM paper_tags pt WHERE pt.tag_id = t.id) AS paper_count \
         FROM tags t WHERE t.id = ?"
    )
    .bind(id)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("db: {e}"))?
    .ok_or_else(|| format!("tag not found: {id}"))
}

/// Update a tag's name and/or color.
#[instrument(skip(db))]
pub async fn update_tag(
    db: &SqlitePool,
    id: &str,
    name: Option<&str>,
    color: Option<&str>,
) -> Result<Tag, String> {
    if let Some(n) = name {
        if n.trim().is_empty() {
            return Err("标签名不能为空".to_string());
        }
        let conflict: Option<(String,)> = sqlx::query_as("SELECT id FROM tags WHERE name = ? AND id != ?")
            .bind(n.trim())
            .bind(id)
            .fetch_optional(db)
            .await
            .map_err(|e| format!("db: {e}"))?;
        if conflict.is_some() {
            return Err(format!("已存在同名标签「{}」", n.trim()));
        }
        sqlx::query("UPDATE tags SET name = ? WHERE id = ?")
            .bind(n.trim())
            .bind(id)
            .execute(db)
            .await
            .map_err(|e| format!("db: {e}"))?;
    }
    if let Some(c) = color {
        sqlx::query("UPDATE tags SET color = ? WHERE id = ?")
            .bind(c)
            .bind(id)
            .execute(db)
            .await
            .map_err(|e| format!("db: {e}"))?;
    }
    get_tag(db, id).await
}

/// Delete a tag and all its paper associations.
#[instrument(skip(db))]
pub async fn delete_tag(db: &SqlitePool, id: &str) -> Result<(), String> {
    sqlx::query("DELETE FROM paper_tags WHERE tag_id = ?")
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;

    // No ON DELETE SET NULL anymore (CRR tables may not have checked FKs) —
    // orphan child tags explicitly so the change propagates via CRDT.
    sqlx::query("UPDATE tags SET parent_id = NULL WHERE parent_id = ?")
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;

    sqlx::query("DELETE FROM tags WHERE id = ?")
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;

    Ok(())
}

/// Get tags associated with a paper.
#[instrument(skip(db))]
pub async fn get_paper_tags(db: &SqlitePool, paper_id: &str) -> Result<Vec<Tag>, String> {
    sqlx::query_as::<_, Tag>(
        "SELECT t.id, t.name, t.color, t.parent_id, t.created_at, \
         (SELECT COUNT(*) FROM paper_tags pt WHERE pt.tag_id = t.id) AS paper_count \
         FROM tags t INNER JOIN paper_tags pt ON pt.tag_id = t.id \
         WHERE pt.paper_id = ? ORDER BY t.name"
    )
    .bind(paper_id)
    .fetch_all(db)
    .await
    .map_err(|e| format!("db: {e}"))
}

/// Add tags to a paper. Tags that don't exist are created on the fly.
#[instrument(skip(db))]
pub async fn add_tags_to_paper(
    db: &SqlitePool,
    paper_id: &str,
    tag_ids: &[String],
) -> Result<(), String> {
    for tag_id in tag_ids {
        sqlx::query(
            "INSERT OR IGNORE INTO paper_tags (paper_id, tag_id) VALUES (?, ?)"
        )
        .bind(paper_id)
        .bind(tag_id)
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    }
    Ok(())
}

/// Remove tags from a paper.
#[instrument(skip(db))]
pub async fn remove_tags_from_paper(
    db: &SqlitePool,
    paper_id: &str,
    tag_ids: &[String],
) -> Result<(), String> {
    for tag_id in tag_ids {
        sqlx::query("DELETE FROM paper_tags WHERE paper_id = ? AND tag_id = ?")
            .bind(paper_id)
            .bind(tag_id)
            .execute(db)
            .await
            .map_err(|e| format!("db: {e}"))?;
    }
    Ok(())
}

/// List papers that have a specific tag.
#[instrument(skip(db))]
pub async fn list_papers_by_tag(
    db: &SqlitePool,
    tag_id: &str,
    sort_by: Option<&str>,
    sort_order: Option<&str>,
) -> Result<Vec<Paper>, String> {
    let sort_by = match sort_by {
        Some("title") => "title",
        Some("year") => "year",
        Some("imported_at") => "imported_at",
        _ => "imported_at",
    };
    let sort_order = match sort_order {
        Some("asc") => "ASC",
        _ => "DESC",
    };

    let sql = format!(
        "SELECT p.* FROM papers p \
         INNER JOIN paper_tags pt ON pt.paper_id = p.id WHERE pt.tag_id = ? \
         ORDER BY p.{sort_by} {sort_order}"
    );

    sqlx::query_as::<_, Paper>(&sql)
        .bind(tag_id)
        .fetch_all(db)
        .await
        .map_err(|e| format!("db: {e}"))
}
