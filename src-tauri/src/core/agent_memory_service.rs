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

/// Cascade for session deletion (CRR tables declare no checked FKs).
pub async fn delete_for_session(db: &SqlitePool, session_id: &str) -> Result<()> {
    sqlx::query("DELETE FROM agent_memories WHERE id = ?")
        .bind(session_id)
        .execute(db)
        .await
        .context("delete agent memory")?;
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
        pool
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
}
