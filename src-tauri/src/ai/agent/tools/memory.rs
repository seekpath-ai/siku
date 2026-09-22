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
