use sqlx::SqlitePool;
use tracing::instrument;

use crate::core::models::{Project, ProjectInput};
use crate::core::time::now_iso;

const PROJECT_COLS: &str = "id, name, path, created_at, updated_at";

#[instrument(skip(db))]
pub async fn list(db: &SqlitePool) -> Result<Vec<Project>, String> {
    sqlx::query_as::<_, Project>(&format!(
        "SELECT {PROJECT_COLS} FROM projects ORDER BY created_at"
    ))
    .fetch_all(db)
    .await
    .map_err(|e| format!("db error: {e}"))
}

#[instrument(skip(db))]
pub async fn get_by_id(db: &SqlitePool, id: &str) -> Result<Option<Project>, String> {
    sqlx::query_as::<_, Project>(&format!(
        "SELECT {PROJECT_COLS} FROM projects WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("db error: {e}"))
}

/// Path of a project, if it exists.
pub async fn get_path(db: &SqlitePool, id: &str) -> Result<Option<String>, String> {
    Ok(get_by_id(db, id).await?.map(|p| p.path))
}

fn folder_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| path.to_string())
}

#[instrument(skip(db))]
pub async fn create(db: &SqlitePool, input: ProjectInput) -> Result<Project, String> {
    let path = input.path.unwrap_or_default();
    let path = path.trim().to_string();
    if path.is_empty() {
        return Err("project path required".to_string());
    }
    if !std::path::Path::new(&path).is_dir() {
        return Err(format!("not a directory: {path}"));
    }

    let id = uuid::Uuid::new_v4().to_string();
    let now = now_iso();
    let name = input
        .name
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| folder_name(&path));

    sqlx::query(
        "INSERT INTO projects (id, name, path, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&name)
    .bind(&path)
    .bind(&now)
    .bind(&now)
    .execute(db)
    .await
    .map_err(|e| format!("db error: {e}"))?;

    get_by_id(db, &id)
        .await?
        .ok_or_else(|| "project not found".to_string())
}

#[instrument(skip(db))]
pub async fn update(db: &SqlitePool, id: &str, input: ProjectInput) -> Result<Project, String> {
    let now = now_iso();
    if let Some(name) = input.name.filter(|n| !n.trim().is_empty()) {
        sqlx::query("UPDATE projects SET name = ?, updated_at = ? WHERE id = ?")
            .bind(&name)
            .bind(&now)
            .bind(id)
            .execute(db)
            .await
            .map_err(|e| format!("db error: {e}"))?;
    } else {
        sqlx::query("UPDATE projects SET updated_at = ? WHERE id = ?")
            .bind(&now)
            .bind(id)
            .execute(db)
            .await
            .map_err(|e| format!("db error: {e}"))?;
    }

    get_by_id(db, id)
        .await?
        .ok_or_else(|| format!("project not found: {id}"))
}

#[instrument(skip(db))]
pub async fn delete(db: &SqlitePool, id: &str) -> Result<(), String> {
    // Sessions lose their project reference instead of being deleted.
    sqlx::query("UPDATE chat_sessions SET project_id = NULL WHERE project_id = ?")
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| format!("db error: {e}"))?;
    sqlx::query("DELETE FROM projects WHERE id = ?")
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| format!("db error: {e}"))?;
    Ok(())
}

/// Ensure at least one project exists (the app data dir as the default) and
/// backfill legacy sessions without a project into it. Called on startup.
#[instrument(skip(db))]
pub async fn ensure_default_project(
    db: &SqlitePool,
    app_data_dir: &std::path::Path,
) -> Result<Project, String> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM projects")
        .fetch_one(db)
        .await
        .map_err(|e| format!("db error: {e}"))?;

    if count == 0 {
        let path = app_data_dir.to_string_lossy().to_string();
        let name = folder_name(&path);
        let id = uuid::Uuid::new_v4().to_string();
        let now = now_iso();
        if let Err(e) = sqlx::query(
            "INSERT INTO projects (id, name, path, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(&name)
        .bind(&path)
        .bind(&now)
        .bind(&now)
        .execute(db)
        .await
        {
            // The projects table only has 5 columns; a mismatch like
            // "expected 57 values, got 53" means a trigger (or stale CRR
            // trigger) is firing on INSERT and doing an unqualified
            // `INSERT INTO other_table VALUES (...)` with the wrong count.
            let schema: Option<(String,)> = sqlx::query_as(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'projects'",
            )
            .fetch_optional(db)
            .await
            .unwrap_or_default();
            let triggers: Vec<(String, String)> = sqlx::query_as(
                "SELECT name, sql FROM sqlite_master WHERE type = 'trigger' AND tbl_name = 'projects'",
            )
            .fetch_all(db)
            .await
            .unwrap_or_default();

            let mut msg = format!("db error: {e}");
            if let Some((sql,)) = schema {
                msg.push_str(&format!("\nprojects schema: {sql}"));
            }
            for (name, sql) in triggers {
                msg.push_str(&format!("\nprojects trigger `{name}`: {sql}"));
            }
            return Err(msg);
        }
    } else {
        // Self-heal: the default project points at the app data dir. If the
        // stored path no longer exists but still looks like an app-data
        // default (same folder name as the live one — e.g. the dir was
        // cleaned, or the DB was moved to another machine), repoint it.
        // User-chosen paths are never touched: a temporarily unplugged disk
        // must not clobber a real project.
        let first: Option<(String, String)> =
            sqlx::query_as("SELECT id, path FROM projects ORDER BY created_at LIMIT 1")
                .fetch_optional(db)
                .await
                .map_err(|e| format!("db error: {e}"))?;
        if let Some((pid, path)) = first {
            let looks_like_app_data = match (
                std::path::Path::new(&path).file_name(),
                app_data_dir.file_name(),
            ) {
                (Some(a), Some(b)) => a == b,
                _ => false,
            };
            if looks_like_app_data && !std::path::Path::new(&path).is_dir() {
                let new_path = app_data_dir.to_string_lossy().to_string();
                sqlx::query("UPDATE projects SET path = ?, updated_at = ? WHERE id = ?")
                    .bind(&new_path)
                    .bind(now_iso())
                    .bind(&pid)
                    .execute(db)
                    .await
                    .map_err(|e| format!("db error: {e}"))?;
            }
        }
    }

    // Backfill legacy sessions into the first project.
    sqlx::query(
        "UPDATE chat_sessions SET project_id = (SELECT id FROM projects ORDER BY created_at LIMIT 1) \
         WHERE project_id IS NULL",
    )
    .execute(db)
    .await
    .map_err(|e| format!("db error: {e}"))?;

    sqlx::query_as::<_, Project>(&format!(
        "SELECT {PROJECT_COLS} FROM projects ORDER BY created_at LIMIT 1"
    ))
    .fetch_one(db)
    .await
    .map_err(|e| format!("db error: {e}"))
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::{tests::connect_with_crsqlite, SCHEMA_INIT_SQL};

    async fn fresh_db(dir: &std::path::Path) -> SqlitePool {
        let db = connect_with_crsqlite(&dir.join("t.db")).await.unwrap();
        sqlx::query(SCHEMA_INIT_SQL).execute(&db).await.unwrap();
        db
    }

    /// The default project points at the app data dir; when that stored path
    /// goes stale (dir cleaned, DB moved to another machine) it is repointed
    /// at the live app data dir on startup.
    #[tokio::test]
    async fn heals_stale_default_project_path() {
        let dir = tempfile::tempdir().unwrap();
        let live_app_dir = dir.path().join("live").join("com.siku.reader");
        std::fs::create_dir_all(&live_app_dir).unwrap();
        let stale = dir.path().join("old").join("com.siku.reader"); // never created

        let db = fresh_db(dir.path()).await;
        sqlx::query(
            "INSERT INTO projects (id, name, path, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
        )
        .bind("p1")
        .bind("com.siku.reader")
        .bind(stale.to_str().unwrap())
        .bind("2026-01-01T00:00:00Z")
        .bind("2026-01-01T00:00:00Z")
        .execute(&db)
        .await
        .unwrap();

        ensure_default_project(&db, &live_app_dir).await.unwrap();
        let (path,): (String,) = sqlx::query_as("SELECT path FROM projects WHERE id = 'p1'")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(path, live_app_dir.to_string_lossy());
        db.close().await;
    }

    /// A user-chosen project path that is missing (e.g. an unplugged disk)
    /// must NOT be rewritten — only app-data-shaped defaults are healed.
    #[tokio::test]
    async fn keeps_user_project_even_if_missing() {
        let dir = tempfile::tempdir().unwrap();
        let live_app_dir = dir.path().join("live").join("com.siku.reader");
        std::fs::create_dir_all(&live_app_dir).unwrap();
        let user_path = dir.path().join("unplugged-disk").join("my-vault"); // never created

        let db = fresh_db(dir.path()).await;
        sqlx::query(
            "INSERT INTO projects (id, name, path, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
        )
        .bind("p1")
        .bind("my-vault")
        .bind(user_path.to_str().unwrap())
        .bind("2026-01-01T00:00:00Z")
        .bind("2026-01-01T00:00:00Z")
        .execute(&db)
        .await
        .unwrap();

        ensure_default_project(&db, &live_app_dir).await.unwrap();
        let (path,): (String,) = sqlx::query_as("SELECT path FROM projects WHERE id = 'p1'")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(path, user_path.to_str().unwrap());
        db.close().await;
    }
}
