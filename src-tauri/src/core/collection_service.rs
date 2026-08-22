use sqlx::SqlitePool;
use tracing::instrument;
use uuid::Uuid;

use crate::core::models::Collection;
use crate::core::time::now_iso;

/// List all collections as a flat list ordered by parent and sort_order.
#[instrument(skip(db))]
pub async fn list_collections(db: &SqlitePool) -> Result<Vec<Collection>, String> {
    sqlx::query_as::<_, Collection>(
        "SELECT id, name, parent_id, sort_order, created_at FROM collections ORDER BY parent_id NULLS FIRST, sort_order, name"
    )
    .fetch_all(db)
    .await
    .map_err(|e| format!("db: {e}"))
}

/// Create a new collection. Returns the created collection.
#[instrument(skip(db))]
pub async fn create_collection(
    db: &SqlitePool,
    name: &str,
    parent_id: Option<&str>,
) -> Result<Collection, String> {
    let id = Uuid::new_v4().to_string();
    let now = now_iso();

    // Compute sort_order: place at the end of siblings
    let sort_order: i32 = sqlx::query_scalar::<_, i32>(
        "SELECT COALESCE(MAX(sort_order), 0) + 1 FROM collections WHERE parent_id IS ?"
    )
    .bind(parent_id)
    .fetch_one(db)
    .await
    .map_err(|e| format!("db: {e}"))?;

    sqlx::query(
        "INSERT INTO collections (id, name, parent_id, sort_order, created_at) VALUES (?, ?, ?, ?, ?)"
    )
    .bind(&id)
    .bind(name)
    .bind(parent_id)
    .bind(sort_order)
    .bind(&now)
    .execute(db)
    .await
    .map_err(|e| format!("db: {e}"))?;

    get_collection(db, &id).await
}

/// Get a single collection by ID.
#[instrument(skip(db))]
pub async fn get_collection(db: &SqlitePool, id: &str) -> Result<Collection, String> {
    sqlx::query_as::<_, Collection>(
        "SELECT id, name, parent_id, sort_order, created_at FROM collections WHERE id = ?"
    )
    .bind(id)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("db: {e}"))?
    .ok_or_else(|| format!("collection not found: {id}"))
}

/// Update collection name or parent.
#[instrument(skip(db))]
pub async fn update_collection(
    db: &SqlitePool,
    id: &str,
    name: Option<&str>,
    parent_id: Option<Option<&str>>,
) -> Result<Collection, String> {
    if let Some(n) = name {
        sqlx::query("UPDATE collections SET name = ? WHERE id = ?")
            .bind(n)
            .bind(id)
            .execute(db)
            .await
            .map_err(|e| format!("db: {e}"))?;
        // Sync: rename the mapped folder note(s).
        sqlx::query("UPDATE notes SET title = ?, updated_at = ? WHERE source_collection_id = ? AND is_folder = 1")
            .bind(n)
            .bind(crate::core::time::now_iso())
            .bind(id)
            .execute(db)
            .await
            .map_err(|e| format!("db: {e}"))?;
    }

    if let Some(pid) = parent_id {
        // Prevent setting self or descendants as parent would require tree validation;
        // kept simple here: just block self-parent.
        if pid == Some(id) {
            return Err("collection cannot be its own parent".to_string());
        }
        sqlx::query("UPDATE collections SET parent_id = ? WHERE id = ?")
            .bind(pid)
            .bind(id)
            .execute(db)
            .await
            .map_err(|e| format!("db: {e}"))?;

        // Sync: move the mapped folder note(s) under the parent collection's
        // folder, falling back to the system "我的图书馆" root.
        let folders: Vec<(String, String)> = sqlx::query_as(
            "SELECT id, vault_id FROM notes WHERE source_collection_id = ? AND is_folder = 1"
        )
        .bind(id)
        .fetch_all(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
        let parent_folder: Option<String> = match pid {
            Some(p) => {
                let f: Option<(String,)> = sqlx::query_as(
                    "SELECT id FROM notes WHERE source_collection_id = ? AND is_folder = 1 LIMIT 1"
                )
                .bind(p)
                .fetch_optional(db)
                .await
                .map_err(|e| format!("db: {e}"))?;
                f.map(|(x,)| x)
            }
            None => None,
        };
        for (folder_id, vault_id) in folders {
            let target = match &parent_folder {
                Some(pf) => Some(pf.clone()),
                None => Some(crate::core::note_service::ensure_system_library_folder(db, &vault_id).await?),
            };
            sqlx::query("UPDATE notes SET parent_id = ?, updated_at = ? WHERE id = ?")
                .bind(target)
                .bind(crate::core::time::now_iso())
                .bind(&folder_id)
                .execute(db)
                .await
                .map_err(|e| format!("db: {e}"))?;
        }
    }

    get_collection(db, id).await
}

/// Delete a collection. Papers in the collection are not deleted (only link rows).
/// Also deletes all descendant collections iteratively.
#[instrument(skip(db))]
pub async fn delete_collection(db: &SqlitePool, id: &str) -> Result<(), String> {
    // Collect all descendant collection IDs using a queue.
    let mut to_delete = vec![id.to_string()];
    let mut i = 0;
    while i < to_delete.len() {
        let current = &to_delete[i];
        let children: Vec<(String,)> =
            sqlx::query_as("SELECT id FROM collections WHERE parent_id = ?")
                .bind(current)
                .fetch_all(db)
                .await
                .map_err(|e| format!("db: {e}"))?;
        for (child_id,) in children {
            to_delete.push(child_id);
        }
        i += 1;
    }

    // Delete from deepest to shallowest so foreign keys don't block.
    for collection_id in to_delete.into_iter().rev() {
        sqlx::query("DELETE FROM paper_collections WHERE collection_id = ?")
            .bind(&collection_id)
            .execute(db)
            .await
            .map_err(|e| format!("db: {e}"))?;
        sqlx::query("DELETE FROM collections WHERE id = ?")
            .bind(&collection_id)
            .execute(db)
            .await
            .map_err(|e| format!("db: {e}"))?;
        // Sync: unlink mapped folder notes (folders stay, notes keep content).
        sqlx::query("UPDATE notes SET source_collection_id = NULL, updated_at = ? WHERE source_collection_id = ?")
            .bind(crate::core::time::now_iso())
            .bind(&collection_id)
            .execute(db)
            .await
            .map_err(|e| format!("db: {e}"))?;
    }

    Ok(())
}

/// Add papers to a collection. Ignores duplicates.
#[instrument(skip(db))]
pub async fn add_papers_to_collection(
    db: &SqlitePool,
    collection_id: &str,
    paper_ids: &[String],
) -> Result<(), String> {
    for paper_id in paper_ids {
        sqlx::query(
            "INSERT OR IGNORE INTO paper_collections (paper_id, collection_id) VALUES (?, ?)"
        )
        .bind(paper_id)
        .bind(collection_id)
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    }
    Ok(())
}

/// Remove papers from a collection.
#[instrument(skip(db))]
pub async fn remove_papers_from_collection(
    db: &SqlitePool,
    collection_id: &str,
    paper_ids: &[String],
) -> Result<(), String> {
    for paper_id in paper_ids {
        sqlx::query(
            "DELETE FROM paper_collections WHERE paper_id = ? AND collection_id = ?"
        )
        .bind(paper_id)
        .bind(collection_id)
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    }
    Ok(())
}
