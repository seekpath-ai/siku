use sqlx::SqlitePool;
use sha2::{Digest, Sha256};
use tracing::instrument;

use crate::core::time;

/// Look up a translation in the cache by source text hash
#[instrument(skip(db))]
pub async fn lookup(
    db: &SqlitePool,
    source_text: &str,
    target_lang: &str,
    model: &str,
) -> Result<Option<String>, String> {
    let source_hash = hash_source(source_text, target_lang);

    let result: Option<(String,)> = sqlx::query_as(
        "SELECT translation FROM translation_cache WHERE source_hash = ? AND model = ?"
    )
    .bind(&source_hash)
    .bind(model)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("db error: {e}"))?;

    Ok(result.map(|r| r.0))
}

/// Store a translation in the cache
#[instrument(skip(db))]
pub async fn store(
    db: &SqlitePool,
    source_text: &str,
    target_lang: &str,
    translation: &str,
    model: &str,
) -> Result<(), String> {
    let id = uuid::Uuid::new_v4().to_string();
    let source_hash = hash_source(source_text, target_lang);
    let now = time::now_iso();

    // Upsert — update if hash+model already exists
    sqlx::query(
        "INSERT INTO translation_cache (id, source_hash, translation, model, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(source_hash, model) DO UPDATE SET translation = excluded.translation, updated_at = excluded.updated_at"
    )
    .bind(&id)
    .bind(&source_hash)
    .bind(translation)
    .bind(model)
    .bind(&now)
    .bind(&now)
    .execute(db)
    .await
    .map_err(|e| format!("db error: {e}"))?;

    Ok(())
}

/// Clear expired or all cache entries
#[instrument(skip(db))]
pub async fn clear(db: &SqlitePool, model: Option<&str>) -> Result<u64, String> {
    let affected = if let Some(m) = model {
        sqlx::query("DELETE FROM translation_cache WHERE model = ?")
            .bind(m)
            .execute(db)
            .await
            .map_err(|e| format!("db error: {e}"))?
            .rows_affected()
    } else {
        sqlx::query("DELETE FROM translation_cache")
            .execute(db)
            .await
            .map_err(|e| format!("db error: {e}"))?
            .rows_affected()
    };

    Ok(affected)
}

/// Generate a SHA-256 hash for source text + target lang
fn hash_source(source_text: &str, target_lang: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(source_text.as_bytes());
    hasher.update(b"||");
    hasher.update(target_lang.as_bytes());
    format!("{:x}", hasher.finalize())
}
