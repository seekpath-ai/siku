//! Per-agent long-term memory (1:1 with chat_sessions).
//!
//! The memory is a single Markdown document curated by the user via the
//! brain button in the chat input. When active it is injected into the
//! agent's system prompt on every turn; when "forgotten" (active = 0) the
//! content is kept but not injected. The table is a CRR so memories sync
//! across devices.
//!
//! `append` is reserved for a future agent-writable `update_memory` tool:
//! the storage layer does not care who writes, wiring the tool up later is
//! just a matter of calling it.

use anyhow::{Context, Result};
use sqlx::SqlitePool;

/// Minimum interval between automatic version snapshots, so a burst of
/// debounced autosaves collapses into a single version row.
const SNAPSHOT_INTERVAL_SECS: i64 = 60;

/// Snapshot the current memory content into `note_versions` (the note
/// version-history table, reused here with the memory/session id as
/// `note_id`) before an overwrite. Skipped when there is nothing worth
/// keeping (first write, empty or unchanged content) and throttled to one
/// snapshot per SNAPSHOT_INTERVAL unless `force` (used by restore, which
/// promises the current content stays recoverable).
async fn snapshot_previous(
    db: &SqlitePool,
    session_id: &str,
    new_content: &str,
    force: bool,
) -> Result<()> {
    let prev: Option<(String,)> = sqlx::query_as("SELECT content FROM agent_memories WHERE id = ?")
        .bind(session_id)
        .fetch_optional(db)
        .await
        .context("load previous memory for snapshot")?;
    let Some((old,)) = prev else { return Ok(()) };
    if old.trim().is_empty() || old == new_content {
        return Ok(());
    }
    if !force {
        let threshold = (chrono::Utc::now() - chrono::Duration::seconds(SNAPSHOT_INTERVAL_SECS))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let recent: Option<(String,)> = sqlx::query_as(
            "SELECT created_at FROM note_versions WHERE note_id = ? AND created_at > ? LIMIT 1",
        )
        .bind(session_id)
        .bind(&threshold)
        .fetch_optional(db)
        .await
        .context("check recent memory snapshot")?;
        if recent.is_some() {
            return Ok(());
        }
    }
    let now = crate::core::time::now_iso();
    // Match the note restore semantics: a snapshot taken as restore
    // protection is marked 'restore' (the dialog shows an undo icon).
    let edited_by = if force { "restore" } else { "user" };
    sqlx::query(
        "INSERT INTO note_versions (id, note_id, title, content, edited_by, created_at) \
         VALUES (?, ?, '长期记忆', ?, ?, ?)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(session_id)
    .bind(&old)
    .bind(edited_by)
    .bind(&now)
    .execute(db)
    .await
    .context("snapshot agent memory version")?;
    Ok(())
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AgentMemory {
    pub id: String,
    pub content: String,
    pub active: bool,
    pub updated_at: String,
}

/// Load the memory row for a session, or None when never written.
pub async fn get(db: &SqlitePool, session_id: &str) -> Result<Option<AgentMemory>> {
    let row: Option<(String, String, i64, String)> = sqlx::query_as(
        "SELECT id, content, active, updated_at FROM agent_memories WHERE id = ?",
    )
    .bind(session_id)
    .fetch_optional(db)
    .await
    .context("load agent memory")?;
    Ok(row.map(|(id, content, active, updated_at)| AgentMemory {
        id,
        content,
        active: active != 0,
        updated_at,
    }))
}

/// Upsert the memory content for a session (active flag preserved).
pub async fn set_content(db: &SqlitePool, session_id: &str, content: &str) -> Result<()> {
    snapshot_previous(db, session_id, content, false).await?;
    let now = crate::core::time::now_iso();
    sqlx::query(
        "INSERT INTO agent_memories (id, content, active, created_at, updated_at) \
         VALUES (?, ?, 1, ?, ?) \
         ON CONFLICT(id) DO UPDATE SET content = excluded.content, updated_at = excluded.updated_at",
    )
    .bind(session_id)
    .bind(content)
    .bind(&now)
    .bind(&now)
    .execute(db)
    .await
    .context("save agent memory")?;
    Ok(())
}

/// Upsert the active flag for a session (content preserved).
pub async fn set_active(db: &SqlitePool, session_id: &str, active: bool) -> Result<()> {
    let now = crate::core::time::now_iso();
    sqlx::query(
        "INSERT INTO agent_memories (id, content, active, created_at, updated_at) \
         VALUES (?, '', ?, ?, ?) \
         ON CONFLICT(id) DO UPDATE SET active = excluded.active, updated_at = excluded.updated_at",
    )
    .bind(session_id)
    .bind(active as i64)
    .bind(&now)
    .bind(&now)
    .execute(db)
    .await
    .context("set agent memory active flag")?;
    Ok(())
}

/// Append a paragraph to the memory (reserved for a future agent tool).
#[allow(dead_code)]
pub async fn append(db: &SqlitePool, session_id: &str, text: &str) -> Result<()> {
    let current = get(db, session_id).await?;
    let content = match current {
        Some(m) if !m.content.is_empty() => format!("{}\n\n{}", m.content, text),
        _ => text.to_string(),
    };
    set_content(db, session_id, &content).await
}

/// Content to inject into the system prompt: only when active and non-empty.
pub async fn active_content(db: &SqlitePool, session_id: &str) -> Result<Option<String>> {
    Ok(match get(db, session_id).await? {
        Some(m) if m.active && !m.content.trim().is_empty() => Some(m.content),
        _ => None,
    })
}

/// Restore the memory from a version snapshot (listed via note_versions_list
/// with the session id). The current content is force-snapshotted first so
/// the restore itself can be undone.
pub async fn restore(db: &SqlitePool, session_id: &str, version_id: &str) -> Result<()> {
    let version: Option<(String, String)> =
        sqlx::query_as("SELECT note_id, content FROM note_versions WHERE id = ?")
            .bind(version_id)
            .fetch_optional(db)
            .await
            .context("load memory version")?;
    let Some((note_id, content)) = version else {
        anyhow::bail!("version not found");
    };
    anyhow::ensure!(note_id == session_id, "version does not belong to this memory");
    // Bypass set_content so the pre-restore snapshot ignores the throttle.
    snapshot_previous(db, session_id, &content, true).await?;
    set_content(db, session_id, &content).await
}

/// Cascade for session deletion (CRR tables declare no checked FKs).
pub async fn delete_for_session(db: &SqlitePool, session_id: &str) -> Result<()> {
    sqlx::query("DELETE FROM agent_memories WHERE id = ?")
        .bind(session_id)
        .execute(db)
        .await
        .context("delete agent memory")?;
    sqlx::query("DELETE FROM note_versions WHERE note_id = ?")
        .bind(session_id)
        .execute(db)
        .await
        .context("delete agent memory versions")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn test_db() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE agent_memories (\
             id TEXT PRIMARY KEY NOT NULL, content TEXT NOT NULL DEFAULT '', \
             active INTEGER NOT NULL DEFAULT 1, \
             created_at TEXT NOT NULL DEFAULT '', updated_at TEXT NOT NULL DEFAULT '')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE note_versions (\
             id TEXT PRIMARY KEY NOT NULL, note_id TEXT NOT NULL, \
             title TEXT NOT NULL DEFAULT '', content TEXT NOT NULL DEFAULT '', \
             edited_by TEXT NOT NULL DEFAULT 'user', created_at TEXT NOT NULL DEFAULT '')",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    async fn version_rows(db: &SqlitePool, session_id: &str) -> Vec<(String, String)> {
        sqlx::query_as(
            "SELECT content, edited_by FROM note_versions WHERE note_id = ? \
             ORDER BY created_at DESC, rowid DESC",
        )
        .bind(session_id)
        .fetch_all(db)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn crud_and_active_content() {
        let db = test_db().await;
        assert!(get(&db, "s1").await.unwrap().is_none());
        assert!(active_content(&db, "s1").await.unwrap().is_none());

        set_content(&db, "s1", "用户偏好简洁回答").await.unwrap();
        let m = get(&db, "s1").await.unwrap().unwrap();
        assert_eq!(m.content, "用户偏好简洁回答");
        assert!(m.active, "new memories default to active");
        assert_eq!(
            active_content(&db, "s1").await.unwrap().as_deref(),
            Some("用户偏好简洁回答")
        );

        // Content update preserves the active flag.
        set_active(&db, "s1", false).await.unwrap();
        set_content(&db, "s1", "更新后的记忆").await.unwrap();
        let m = get(&db, "s1").await.unwrap().unwrap();
        assert_eq!(m.content, "更新后的记忆");
        assert!(!m.active, "content update must preserve the active flag");
        assert!(active_content(&db, "s1").await.unwrap().is_none());

        // set_active on a missing session creates an empty row.
        set_active(&db, "s2", true).await.unwrap();
        let m = get(&db, "s2").await.unwrap().unwrap();
        assert_eq!(m.content, "");
        assert!(active_content(&db, "s2").await.unwrap().is_none());

        // append accumulates paragraphs.
        append(&db, "s1", "新增经验").await.unwrap();
        let m = get(&db, "s1").await.unwrap().unwrap();
        assert_eq!(m.content, "更新后的记忆\n\n新增经验");

        delete_for_session(&db, "s1").await.unwrap();
        assert!(get(&db, "s1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn updates_snapshot_previous_content_throttled() {
        let db = test_db().await;
        // First write has nothing to snapshot.
        set_content(&db, "s1", "v1").await.unwrap();
        assert!(version_rows(&db, "s1").await.is_empty());

        // Second write snapshots "v1".
        set_content(&db, "s1", "v2").await.unwrap();
        let rows = version_rows(&db, "s1").await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "v1");
        assert_eq!(rows[0].1, "user");

        // Immediate further writes are throttled (autosave burst collapse).
        set_content(&db, "s1", "v3").await.unwrap();
        set_content(&db, "s1", "v4").await.unwrap();
        assert_eq!(version_rows(&db, "s1").await.len(), 1);

        // Unchanged content never snapshots.
        set_content(&db, "s1", "v4").await.unwrap();
        assert_eq!(version_rows(&db, "s1").await.len(), 1);
    }

    #[tokio::test]
    async fn restore_force_snapshots_current_and_applies_version() {
        let db = test_db().await;
        set_content(&db, "s1", "v1").await.unwrap();
        set_content(&db, "s1", "v2").await.unwrap();
        let (version_id,): (String,) = sqlx::query_as(
            "SELECT id FROM note_versions WHERE note_id = 's1' AND content = 'v1'",
        )
        .fetch_one(&db)
        .await
        .unwrap();

        restore(&db, "s1", &version_id).await.unwrap();
        let m = get(&db, "s1").await.unwrap().unwrap();
        assert_eq!(m.content, "v1");
        // The pre-restore content is kept as a forced 'restore' snapshot,
        // despite the throttle window.
        let rows = version_rows(&db, "s1").await;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "v2");
        assert_eq!(rows[0].1, "restore");

        // Versions of another memory are rejected.
        set_content(&db, "s2", "other").await.unwrap();
        assert!(restore(&db, "s2", &version_id).await.is_err());

        // Session deletion cascades to versions.
        delete_for_session(&db, "s1").await.unwrap();
        assert!(version_rows(&db, "s1").await.is_empty());
    }
}
