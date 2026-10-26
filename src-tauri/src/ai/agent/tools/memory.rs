//! memory_read / memory_write: agent access to the session's long-term
//! memory documents. Two scopes (see agent_memory_service):
//! - global (`agent_memories` — synced across devices),
//! - device (`agent_memories_local` — this machine only, never synced).
//! Both are versioned via note_versions snapshots so overwrites are
//! recoverable.
//!
//! The same documents are shown in the chat input's brain-button modal
//! (「全局 / 本机」tabs) and injected into the system prompt when active, so
//! agent writes take effect on the next turn automatically.

use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::ai::agent::tool_registry::{Tool, ToolParameter};
use crate::core::agent_memory_service::{self, MemoryScope};

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
        "Read this conversation's long-term memory (persistent markdown documents that survive across turns). There are two scopes: global memory syncs across devices; device memory holds facts about this machine only. Use them to recall user preferences, decisions, experience and environment facts recorded earlier."
    }
    fn parameters(&self) -> Vec<ToolParameter> {
        vec![]
    }
    fn readonly(&self) -> bool {
        true
    }

    async fn execute(&self, _args: serde_json::Value) -> Result<String, String> {
        let mut out = String::new();
        for (scope, label) in [
            (MemoryScope::Global, "Global memory (synced across devices)"),
            (MemoryScope::Local, "Device memory (this machine only)"),
        ] {
            match agent_memory_service::get(&self.db, scope, &self.session_id).await {
                Ok(Some(m)) if !m.content.trim().is_empty() => {
                    let state = if m.active { "active" } else { "inactive (not injected into prompts)" };
                    out.push_str(&format!("## {label} — {state}\n\n{}\n\n", m.content));
                }
                Ok(_) => out.push_str(&format!("## {label}\n\n(empty)\n\n")),
                Err(e) => return Err(format!("memory read failed: {e}")),
            }
        }
        Ok(out.trim_end().to_string())
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
        "Write to this conversation's long-term memory. mode=append (default) adds a paragraph — appends are automatically prefixed with a `## YYYY-MM-DD` date heading, so do NOT add your own date heading; mode=overwrite replaces the whole document (the previous content is snapshotted and recoverable). scope=global (default) syncs across devices — use it for user preferences, project conventions, domain knowledge and experience; scope=device stays on this machine — use it for facts about the local environment ONLY (OS, installed software and versions, absolute paths, hardware). Never record machine-specific facts in the global scope: they would be injected on other devices where they are false. Keep entries short and factual."
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
            ToolParameter {
                name: "scope".into(),
                param_type: "string".into(),
                description: "global (default, syncs across devices) | device (this machine only)".into(),
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
        let scope = MemoryScope::from_param(args["scope"].as_str());
        let scope_label = match scope {
            MemoryScope::Global => "全局记忆（跨设备同步）",
            MemoryScope::Local => "本机记忆（仅此设备）",
        };
        match mode {
            "append" => {
                // Stamp each appended paragraph with a date heading so every
                // entry stays traceable (the document itself only carries a
                // whole-doc updated_at). Content that already starts with a
                // heading is left alone — e.g. the 「打标为经验」 path writes
                // its own `## 经验（date）` header.
                let dated = if content.starts_with('#') {
                    content.to_string()
                } else {
                    let now = crate::core::time::now_iso();
                    let date = now.get(..10).unwrap_or(&now);
                    format!("## {date}\n\n{content}")
                };
                agent_memory_service::append(&self.db, scope, &self.session_id, &dated)
                    .await
                    .map_err(|e| format!("memory append failed: {e}"))?;
                Ok(format!("已追加到{scope_label}。"))
            }
            "overwrite" => {
                agent_memory_service::set_content(&self.db, scope, &self.session_id, content)
                    .await
                    .map_err(|e| format!("memory overwrite failed: {e}"))?;
                Ok(format!("{scope_label}已重写（旧内容已快照，可在记忆面板的历史版本中恢复）。"))
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
        "Search this conversation's raw history (verbatim past messages, optionally with the tool calls made then). Use it when earlier turns are no longer in your context — e.g. the system prompt says older rounds were not loaded — or when you need details of a previous discussion or tool result. Matching is keyword-based, so when a query finds nothing, retry with synonyms / translations / abbreviations instead of giving up: pass query as an ARRAY of alternative phrasings (AND within each, OR across) to cover several directions in one call, e.g. [\"transformer\", \"attention 注意力\"]. Different from memory_read, which reads the curated long-term memory document."
    }
    fn parameters(&self) -> Vec<ToolParameter> {
        vec![
            ToolParameter {
                name: "query".into(),
                param_type: "string".into(),
                description: "Keywords, space-separated (AND match, case-insensitive). For synonym expansion pass an array of alternative phrasings instead — words AND within each element, elements OR. Omit to browse by time range alone.".into(),
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
        // query accepts a string ("a b" = a AND b) or an array of strings for
        // synonym expansion in one call: words AND within each group, groups
        // OR — e.g. ["transformer", "注意力机制", "attention"].
        let groups: Vec<Vec<String>> = match &args["query"] {
            serde_json::Value::String(s) => {
                let g: Vec<String> = s.split_whitespace().map(|k| k.to_lowercase()).collect();
                if g.is_empty() { Vec::new() } else { vec![g] }
            }
            serde_json::Value::Array(arr) => arr
                .iter()
                .filter_map(|v| v.as_str())
                .map(|g| g.split_whitespace().map(|k| k.to_lowercase()).collect::<Vec<_>>())
                .filter(|g| !g.is_empty())
                .collect(),
            _ => Vec::new(),
        };
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
        let escaped: Vec<Vec<String>> = groups
            .iter()
            .map(|g| {
                g.iter()
                    .map(|k| k.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"))
                    .collect()
            })
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
        if !escaped.is_empty() {
            sql.push_str(" AND (");
            for (i, g) in escaped.iter().enumerate() {
                if i > 0 {
                    sql.push_str(" OR ");
                }
                sql.push('(');
                for (j, _) in g.iter().enumerate() {
                    if j > 0 {
                        sql.push_str(" AND ");
                    }
                    sql.push_str("LOWER(content) LIKE ? ESCAPE '\\'");
                }
                sql.push(')');
            }
            sql.push(')');
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
        for g in &escaped {
            for k in g {
                q = q.bind(format!("%{k}%"));
            }
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
            return Ok("没有匹配的历史消息。换同义词/中英文/缩写（query 传数组，组间为 OR）或放宽时间范围后重试。".to_string());
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
