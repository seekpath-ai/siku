//! DB-backed conversation history for agent turns.
//!
//! `chat_messages` + `agent_steps` are the synced source of truth for what the
//! model sees as history. The per-session JSONL file under the memory dir is
//! only a local cache/backup (kept for the memory-dir setting and debugging):
//! on a second device that file never contained the synced turns, so
//! continuing a synced conversation used to start from scratch.
//!
//! The records mirrored here are the ones the engine consumes: user turns and
//! final assistant turns, with the assistant reasoning reconstructed from the
//! ReAct steps linked to that message (the JSONL used to store it inline).

use std::collections::HashMap;

use sqlx::SqlitePool;

/// Everything here is replayed into the next turn's input context; an
/// unbounded thinking transcript would eat the budget for no benefit.
const REASONING_MEMORY_CAP: usize = 8_000;
/// Same manifest cap as the old JSONL implementation.
const OLDER_MANIFEST_MAX: usize = 30;

#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub role: String,
    pub content: String,
    pub reasoning_content: Option<String>,
    pub attachments: Option<String>,
    pub created_at: String,
    pub origin_device_id: String,
    pub origin_device_name: String,
    pub origin_working_dir: String,
}

pub struct LoadedHistory {
    pub entries: Vec<HistoryEntry>,
    /// "older rounds not loaded" manifest, ready to append to the prompt.
    pub older_note: Option<String>,
}

/// Concatenate per-step reasoning the same way the old JSONL assistant record
/// did (blank-line separated, capped from the END — the tail is what the model
/// was thinking just before it answered).
pub fn concat_reasoning<'a>(parts: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let mut out = String::new();
    for part in parts {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(part);
    }
    if out.trim().is_empty() {
        return None;
    }
    if out.chars().count() > REASONING_MEMORY_CAP {
        let tail: String = out
            .chars()
            .skip(out.chars().count() - REASONING_MEMORY_CAP)
            .collect();
        return Some(format!("…（较早的思考已省略）\n{tail}"));
    }
    Some(out)
}

/// Load the last `max_rounds` turns (user + final assistant messages) for a
/// session, plus the manifest of older user messages that fell out of the
/// window. `max_rounds == 0` still loads one record, matching the old
/// `MemoryStore::load_recent` behaviour.
pub async fn load(
    db: &SqlitePool,
    session_id: &str,
    max_rounds: usize,
    recall_available: bool,
) -> Result<LoadedHistory, String> {
    // `chat_messages` stores user + final assistant only; a "round" is those
    // two rows, same as the JSONL's 2 lines per round.
    let limit = max_rounds.saturating_mul(2).max(1) as i64;

    let mut rows = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            Option<String>,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        ),
    >(
        "SELECT id, role, content, attachments, created_at, reasoning_content, \
                origin_device_id, origin_device_name, origin_working_dir \
         FROM chat_messages \
         WHERE session_id = ? AND role IN ('user', 'assistant') \
         ORDER BY created_at DESC, id DESC LIMIT ?",
    )
    .bind(session_id)
    .bind(limit)
    .fetch_all(db)
    .await
    .map_err(|e| format!("db error: {e}"))?;
    rows.reverse();

    // Assistant reasoning lives in agent_steps (the message row keeps it only
    // for rows written before the steps split). Batch the lookup so a long
    // session doesn't issue one query per message.
    let assistant_ids: Vec<String> = rows
        .iter()
        .filter(|r| r.1 == "assistant")
        .map(|r| r.0.clone())
        .collect();
    let mut step_reasoning: HashMap<String, Vec<String>> = HashMap::new();
    if !assistant_ids.is_empty() {
        let placeholders = vec!["?"; assistant_ids.len()].join(",");
        let sql = format!(
            "SELECT message_id, reasoning_content FROM agent_steps \
             WHERE message_id IN ({placeholders}) ORDER BY step_index ASC, id ASC"
        );
        let mut q = sqlx::query_as::<_, (Option<String>, Option<String>)>(&sql);
        for id in &assistant_ids {
            q = q.bind(id);
        }
        for (message_id, reasoning) in q
            .fetch_all(db)
            .await
            .map_err(|e| format!("db error: {e}"))?
        {
            let (Some(message_id), Some(reasoning)) = (message_id, reasoning) else {
                continue;
            };
            if reasoning.trim().is_empty() {
                continue;
            }
            step_reasoning
                .entry(message_id)
                .or_default()
                .push(reasoning);
        }
    }

    let mut entries = Vec::with_capacity(rows.len());
    for (id, role, content, attachments, created_at, reasoning, dev_id, dev_name, dev_dir) in rows {
        let reasoning_content = reasoning.filter(|r| !r.trim().is_empty()).or_else(|| {
            step_reasoning
                .get(&id)
                .and_then(|parts| concat_reasoning(parts.iter().map(String::as_str)))
        });
        entries.push(HistoryEntry {
            role,
            content,
            reasoning_content,
            attachments,
            created_at,
            origin_device_id: dev_id.unwrap_or_default(),
            origin_device_name: dev_name.unwrap_or_default(),
            origin_working_dir: dev_dir.unwrap_or_default(),
        });
    }

    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM chat_messages \
         WHERE session_id = ? AND role IN ('user', 'assistant')",
    )
    .bind(session_id)
    .fetch_one(db)
    .await
    .map_err(|e| format!("db error: {e}"))?;

    let older_note = if (total as usize) > entries.len() {
        let oldest = entries
            .first()
            .map(|e| e.created_at.clone())
            .unwrap_or_default();
        let older_users = sqlx::query_as::<_, (String, String)>(
            "SELECT content, created_at FROM chat_messages \
             WHERE session_id = ? AND role = 'user' AND created_at < ? \
             ORDER BY created_at ASC, id ASC",
        )
        .bind(session_id)
        .bind(&oldest)
        .fetch_all(db)
        .await
        .map_err(|e| format!("db error: {e}"))?;
        build_older_note(&older_users, recall_available)
    } else {
        None
    };

    Ok(LoadedHistory {
        entries,
        older_note,
    })
}

/// Note injected into the system prompt when the loaded window contains turns
/// produced on a different device. It names both machines and their (then)
/// working directories so the model does not treat the earlier machine's
/// absolute paths or installed software as local truth.
pub fn device_switch_note(
    entries: &[HistoryEntry],
    current_device_id: &str,
    current_device_name: &str,
    current_working_dir: &str,
) -> Option<String> {
    let foreign: Vec<&HistoryEntry> = entries
        .iter()
        .filter(|e| !e.origin_device_id.is_empty() && e.origin_device_id != current_device_id)
        .collect();
    let latest = foreign.last()?;
    let rounds = foreign.iter().filter(|e| e.role == "user").count().max(1);
    let name = if latest.origin_device_name.trim().is_empty() {
        "另一台设备".to_string()
    } else {
        latest.origin_device_name.clone()
    };
    let dir = if latest.origin_working_dir.trim().is_empty() {
        "未记录".to_string()
    } else {
        latest.origin_working_dir.clone()
    };
    let local_dir = if current_working_dir.trim().is_empty() {
        "未记录".to_string()
    } else {
        current_working_dir.to_string()
    };
    Some(format!(
        "# 设备切换\n此会话最近有 {rounds} 轮来自设备「{name}」（当时工作目录：{dir}），本机为「{current_device_name}」（本机工作目录：{local_dir}）。历史中的绝对路径、软件环境和已安装工具属于当时那台设备，在本机可能不存在；涉及文件操作前请先确认路径，或向用户询问本机位置，不要假设历史路径可直接使用。"
    ))
}

fn build_older_note(users: &[(String, String)], recall_available: bool) -> Option<String> {
    if users.is_empty() {
        return None;
    }
    let first = date_of(&users[0].1);
    let last = date_of(&users[users.len() - 1].1);
    let mut note = format!(
        "# 更早的对话（未加载进上下文）\n本会话还有 {} 轮更早的对话（{} ~ {}）未加载。以下是每条用户消息的首行，仅供定位：\n",
        users.len(),
        first,
        last
    );
    let shown = if users.len() > OLDER_MANIFEST_MAX {
        &users[users.len() - OLDER_MANIFEST_MAX..]
    } else {
        users
    };
    for (content, ts) in shown {
        let first_line: String = content
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(60)
            .collect();
        note.push_str(&format!("- [{}] {}\n", short_time(ts), first_line));
    }
    if users.len() > OLDER_MANIFEST_MAX {
        note.push_str(&format!(
            "（更早的 {} 轮从略）\n",
            users.len() - OLDER_MANIFEST_MAX
        ));
    }
    if recall_available {
        note.push_str("需要完整内容或当时的工具调用详情时，用 memory_recall 工具按关键词（query）或时间范围（from/to）查询。");
    } else {
        note.push_str("（历史检索工具 memory_recall 未启用，无法回查这些内容。）");
    }
    Some(note)
}

fn date_of(ts: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|_| ts.get(..10).unwrap_or("").to_string())
}

fn short_time(ts: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|d| d.format("%m-%d %H:%M").to_string())
        .unwrap_or_else(|_| {
            ts.get(5..16)
                .map(|s| s.replace('T', " "))
                .unwrap_or_default()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pool() -> SqlitePool {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query(
            "CREATE TABLE chat_messages (
                id TEXT PRIMARY KEY NOT NULL,
                session_id TEXT NOT NULL DEFAULT '',
                role TEXT NOT NULL DEFAULT '',
                content TEXT NOT NULL DEFAULT '',
                reasoning_content TEXT,
                attachments TEXT,
                origin_device_id TEXT NOT NULL DEFAULT '',
                origin_device_name TEXT NOT NULL DEFAULT '',
                origin_working_dir TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL DEFAULT ''
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE agent_steps (
                id TEXT PRIMARY KEY NOT NULL,
                session_id TEXT NOT NULL DEFAULT '',
                message_id TEXT,
                step_index INTEGER NOT NULL DEFAULT 0,
                reasoning_content TEXT,
                tool_calls TEXT,
                narration TEXT,
                created_at TEXT NOT NULL DEFAULT ''
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    async fn insert_message(
        pool: &SqlitePool,
        id: &str,
        role: &str,
        content: &str,
        created_at: &str,
        device: &str,
        dir: &str,
    ) {
        sqlx::query(
            "INSERT INTO chat_messages (id, session_id, role, content, created_at, origin_device_id, origin_device_name, origin_working_dir)
             VALUES (?, 's1', ?, ?, ?, ?, '办公机', ?)",
        )
        .bind(id)
        .bind(role)
        .bind(content)
        .bind(created_at)
        .bind(device)
        .bind(dir)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn loads_recent_turns_and_reconstructs_reasoning() {
        let db = pool().await;
        insert_message(
            &db,
            "u1",
            "user",
            "第一问",
            "2026-11-01T10:00:00.000Z",
            "dev-a",
            "D:/proj",
        )
        .await;
        insert_message(
            &db,
            "a1",
            "assistant",
            "第一答",
            "2026-11-01T10:01:00.000Z",
            "dev-a",
            "D:/proj",
        )
        .await;
        insert_message(
            &db,
            "u2",
            "user",
            "第二问",
            "2026-11-02T10:00:00.000Z",
            "dev-a",
            "D:/proj",
        )
        .await;
        insert_message(
            &db,
            "a2",
            "assistant",
            "第二答",
            "2026-11-02T10:01:00.000Z",
            "dev-a",
            "D:/proj",
        )
        .await;
        for (i, reasoning) in ["先想", "再想"].iter().enumerate() {
            sqlx::query(
                "INSERT INTO agent_steps (id, session_id, message_id, step_index, reasoning_content)
                 VALUES (?, 's1', 'a2', ?, ?)",
            )
            .bind(format!("s{i}"))
            .bind(i as i32)
            .bind(reasoning)
            .execute(&db)
            .await
            .unwrap();
        }

        let loaded = load(&db, "s1", 1, true).await.unwrap();
        // 1 round => last 2 rows (u2, a2)
        assert_eq!(loaded.entries.len(), 2);
        assert_eq!(loaded.entries[0].content, "第二问");
        assert_eq!(loaded.entries[1].content, "第二答");
        // Origin metadata rides along so the prompt can warn about a device
        // switch with different paths.
        assert_eq!(loaded.entries[0].origin_device_id, "dev-a");
        assert_eq!(loaded.entries[0].origin_device_name, "办公机");
        assert_eq!(loaded.entries[0].origin_working_dir, "D:/proj");
        assert_eq!(
            loaded.entries[1].reasoning_content.as_deref(),
            Some("先想\n\n再想")
        );
        let note = loaded.older_note.expect("older manifest");
        assert!(note.contains("1 轮更早"), "note: {note}");
        assert!(note.contains("第一问"), "note: {note}");
    }

    #[tokio::test]
    async fn no_manifest_when_everything_fits() {
        let db = pool().await;
        insert_message(
            &db,
            "u1",
            "user",
            "唯一一问",
            "2026-11-01T10:00:00.000Z",
            "",
            "",
        )
        .await;
        insert_message(
            &db,
            "a1",
            "assistant",
            "唯一一答",
            "2026-11-01T10:01:00.000Z",
            "",
            "",
        )
        .await;

        let loaded = load(&db, "s1", 10, false).await.unwrap();
        assert_eq!(loaded.entries.len(), 2);
        assert!(loaded.older_note.is_none());
        // Origins stay empty for rows written before the source-device columns.
        assert!(loaded.entries[0].origin_device_id.is_empty());
    }

    #[test]
    fn device_switch_note_names_both_machines() {
        let entries = vec![HistoryEntry {
            role: "user".into(),
            content: "写文件".into(),
            reasoning_content: None,
            attachments: None,
            created_at: "2026-11-01T10:00:00.000Z".into(),
            origin_device_id: "dev-a".into(),
            origin_device_name: "办公机".into(),
            origin_working_dir: "D:/proj".into(),
        }];
        let note = device_switch_note(&entries, "dev-b", "家用机", "/home/me/proj")
            .expect("note for foreign origin");
        assert!(note.contains("办公机"), "{note}");
        assert!(note.contains("D:/proj"), "{note}");
        assert!(note.contains("家用机"), "{note}");
        assert!(note.contains("/home/me/proj"), "{note}");
    }

    #[test]
    fn no_device_switch_note_for_local_turns() {
        let entries = vec![HistoryEntry {
            role: "user".into(),
            content: "本机".into(),
            reasoning_content: None,
            attachments: None,
            created_at: "2026-11-01T10:00:00.000Z".into(),
            origin_device_id: "dev-b".into(),
            origin_device_name: "家用机".into(),
            origin_working_dir: "/home/me/proj".into(),
        }];
        assert!(device_switch_note(&entries, "dev-b", "家用机", "").is_none());
    }
}
