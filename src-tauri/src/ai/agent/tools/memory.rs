//! memory_read / memory_write: agent access to the session's long-term
//! memory document (`agent_memories` — per-session, synced across devices,
//! versioned via note_versions snapshots so overwrites are recoverable).
//!
//! The same document is shown in the chat input's brain-button modal and
//! injected into the system prompt when active, so agent writes take effect
//! on the next turn automatically.

use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::ai::agent::tool_registry::{Tool, ToolParameter};
use crate::core::agent_memory_service;

/// Read the session's long-term memory. Auto-approved (read-only).
pub struct MemoryReadTool {
    db: SqlitePool,
    session_id: String,
}

impl MemoryReadTool {
    pub fn new(db: SqlitePool, session_id: String) -> Self {
        Self { db, session_id }
    }
}

#[async_trait]
impl Tool for MemoryReadTool {
    fn name(&self) -> &str {
        "memory_read"
    }
    fn description(&self) -> &str {
        "Read this conversation's long-term memory (a persistent markdown document that survives across turns and devices). Use it to recall user preferences, decisions and experience recorded earlier."
    }
    fn parameters(&self) -> Vec<ToolParameter> {
        vec![]
    }
    fn readonly(&self) -> bool {
        true
    }

    async fn execute(&self, _args: serde_json::Value) -> Result<String, String> {
        match agent_memory_service::get(&self.db, &self.session_id).await {
            Ok(Some(m)) if !m.content.trim().is_empty() => {
                let state = if m.active { "active" } else { "inactive (not injected into prompts)" };
                Ok(format!("Long-term memory ({state}):\n\n{}", m.content))
            }
            Ok(_) => Ok("Long-term memory is empty.".to_string()),
            Err(e) => Err(format!("memory read failed: {e}")),
        }
    }
}

/// Write the session's long-term memory: append a paragraph (default) or
/// overwrite the whole document. Overwrite snapshots the previous content
/// first, so it is recoverable from the memory modal's version history.
pub struct MemoryWriteTool {
    db: SqlitePool,
    session_id: String,
}

impl MemoryWriteTool {
    pub fn new(db: SqlitePool, session_id: String) -> Self {
        Self { db, session_id }
    }
}

#[async_trait]
impl Tool for MemoryWriteTool {
    fn name(&self) -> &str {
        "memory_write"
    }
    fn description(&self) -> &str {
        "Write to this conversation's long-term memory. mode=append (default) adds a paragraph — use it to record experience, user preferences and decisions worth keeping; mode=overwrite replaces the whole document (the previous content is snapshotted and recoverable). Keep entries short and factual."
    }
    fn parameters(&self) -> Vec<ToolParameter> {
        vec![
            ToolParameter {
                name: "content".into(),
                param_type: "string".into(),
                description: "Markdown content to append (or the full new document in overwrite mode)".into(),
                required: true,
            },
            ToolParameter {
                name: "mode".into(),
                param_type: "string".into(),
                description: "append (default) | overwrite".into(),
                required: false,
            },
        ]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let content = args["content"].as_str().unwrap_or("").trim();
        if content.is_empty() {
            return Err("content is empty".to_string());
        }
        let mode = args["mode"].as_str().unwrap_or("append");
        match mode {
            "append" => {
                agent_memory_service::append(&self.db, &self.session_id, content)
                    .await
                    .map_err(|e| format!("memory append failed: {e}"))?;
                Ok("已追加到长期记忆。".to_string())
            }
            "overwrite" => {
                agent_memory_service::set_content(&self.db, &self.session_id, content)
                    .await
                    .map_err(|e| format!("memory overwrite failed: {e}"))?;
                Ok("长期记忆已重写（旧内容已快照，可在记忆面板的历史版本中恢复）。".to_string())
            }
            other => Err(format!("unknown mode: {other} (expected append|overwrite)")),
        }
    }
}

/// memory_recall: search this session's RAW conversation history
/// (chat_messages, plus agent_steps tool-call details on demand).
///
/// This is the escape hatch for the context window: rounds that fell out of
/// the loaded memory window (max_memory_rounds) or were dropped by the token
/// budget are lossless in the database, and this tool is how the agent gets
/// them back. Unlike memory_read (the curated long-term memory document),
/// recall returns verbatim history.
pub struct MemoryRecallTool {
    db: SqlitePool,
    session_id: String,
}

impl MemoryRecallTool {
    pub fn new(db: SqlitePool, session_id: String) -> Self {
        Self { db, session_id }
    }
}

/// Per-message content budget in the recall output.
const RECALL_CONTENT_CAP: usize = 1200;
/// Per-tool-call result preview budget.
const RECALL_STEP_RESULT_CAP: usize = 300;
/// Whole-output budget; beyond it the agent must narrow the query.
const RECALL_TOTAL_CAP: usize = 8000;

fn cap_chars(s: &str, cap: usize) -> String {
    if s.chars().count() <= cap {
        return s.to_string();
    }
    let mut out: String = s.chars().take(cap).collect();
    out.push_str("…[截断]");
    out
}

#[async_trait]
impl Tool for MemoryRecallTool {
    fn name(&self) -> &str {
        "memory_recall"
    }
    fn description(&self) -> &str {
        "Search this conversation's raw history (verbatim past messages, optionally with the tool calls made then). Use it when earlier turns are no longer in your context — e.g. the system prompt says older rounds were not loaded — or when you need details of a previous discussion or tool result. Different from memory_read, which reads the curated long-term memory document."
    }
    fn parameters(&self) -> Vec<ToolParameter> {
        vec![
            ToolParameter {
                name: "query".into(),
                param_type: "string".into(),
                description: "Keywords, space-separated (AND match, case-insensitive). Omit to browse by time range alone.".into(),
                required: false,
            },
            ToolParameter {
                name: "role".into(),
                param_type: "string".into(),
                description: "user | assistant; omit for both".into(),
                required: false,
            },
            ToolParameter {
                name: "from".into(),
                param_type: "string".into(),
                description: "Lower time bound, e.g. 2026-10-08 or 2026-10-08T14:00".into(),
                required: false,
            },
            ToolParameter {
                name: "to".into(),
                param_type: "string".into(),
                description: "Upper time bound (a bare date covers that whole day)".into(),
                required: false,
            },
            ToolParameter {
                name: "include_steps".into(),
                param_type: "boolean".into(),
                description: "Also show the tool calls (name/arguments/result preview) of each matched assistant turn. Default false.".into(),
                required: false,
            },
            ToolParameter {
                name: "limit".into(),
                param_type: "number".into(),
                description: "Max messages to return, newest first (default 20, max 50)".into(),
                required: false,
            },
        ]
    }
    fn readonly(&self) -> bool {
        true
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let keywords: Vec<String> = args["query"]
            .as_str()
            .unwrap_or("")
            .split_whitespace()
            .map(|k| k.to_lowercase())
            .collect();
        let role = args["role"]
            .as_str()
            .filter(|r| matches!(*r, "user" | "assistant"));
        let from = args["from"].as_str().filter(|s| !s.trim().is_empty());
        // A bare date as the upper bound must cover the whole day — string
        // comparison would otherwise exclude that day's timestamps.
        let to_owned;
        let to = match args["to"].as_str().filter(|s| !s.trim().is_empty()) {
            Some(t) if t.len() == 10 => {
                to_owned = format!("{t}T23:59:59");
                Some(to_owned.as_str())
            }
            other => other,
        };
        let include_steps = args["include_steps"].as_bool().unwrap_or(false);
        let limit = args["limit"].as_i64().unwrap_or(20).clamp(1, 50);

        // LIKE-escape %, _ and the escape char itself.
        let escaped: Vec<String> = keywords
            .iter()
            .map(|k| k.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"))
            .collect();

        let mut sql = String::from(
            "SELECT id, role, content, created_at FROM chat_messages WHERE session_id = ?",
        );
        if role.is_some() {
            sql.push_str(" AND role = ?");
        }
        if from.is_some() {
            sql.push_str(" AND created_at >= ?");
        }
        if to.is_some() {
            sql.push_str(" AND created_at <= ?");
        }
        for _ in &escaped {
            sql.push_str(" AND LOWER(content) LIKE ? ESCAPE '\\'");
        }
        sql.push_str(" ORDER BY created_at DESC LIMIT ?");

        let mut q = sqlx::query_as::<_, (String, String, String, String)>(&sql)
            .bind(&self.session_id);
        if let Some(r) = role {
            q = q.bind(r);
        }
        if let Some(f) = from {
            q = q.bind(f);
        }
        if let Some(t) = to {
            q = q.bind(t);
        }
        for k in &escaped {
            q = q.bind(format!("%{k}%"));
        }
        q = q.bind(limit);

        let mut rows = q
            .fetch_all(&self.db)
            .await
            .map_err(|e| format!("db: {e}"))?;
        // Present oldest-first within the page: reading order matches how the
        // conversation unfolded, which is what the model is trying to recall.
        rows.reverse();

        if rows.is_empty() {
            return Ok("没有匹配的历史消息。可放宽关键词或时间范围后重试。".to_string());
        }

        let mut out = format!("匹配到 {} 条历史消息（按时间正序）：\n", rows.len());
        for (id, r, content, created_at) in &rows {
            let when = created_at.replace('T', " ");
            let when = when.get(..16).unwrap_or(&when);
            out.push_str(&format!("\n--- [{when}] [{r}] (msg {id})\n"));
            out.push_str(&cap_chars(content, RECALL_CONTENT_CAP));
            out.push('\n');

            if include_steps && r == "assistant" {
                let steps: Vec<(i32, Option<String>)> = sqlx::query_as(
                    "SELECT step_index, tool_calls FROM agent_steps WHERE message_id = ? ORDER BY step_index",
                )
                .bind(id)
                .fetch_all(&self.db)
                .await
                .unwrap_or_default();
                for (step_index, tool_calls) in steps {
                    let Some(tc) = tool_calls else { continue };
                    let Ok(calls) =
                        serde_json::from_str::<Vec<crate::ai::agent::engine::ToolCallRecord>>(&tc)
                    else {
                        continue;
                    };
                    for call in calls {
                        out.push_str(&format!(
                            "    [step {step_index}] {} {} → {}: {}\n",
                            call.name,
                            cap_chars(&call.arguments.to_string(), 120),
                            call.status,
                            cap_chars(&call.result, RECALL_STEP_RESULT_CAP),
                        ));
                    }
                }
            }

            if out.len() > RECALL_TOTAL_CAP {
                out.push_str("\n…（输出已达上限，请加关键词或缩小时间范围）\n");
                return Ok(out);
            }
        }
        Ok(out)
    }
}
