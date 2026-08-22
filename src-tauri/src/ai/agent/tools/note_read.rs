use async_trait::async_trait;
use sqlx::SqlitePool;
use crate::ai::agent::tool_registry::{Tool, ToolParameter};

pub struct NoteReadTool {
    db: SqlitePool,
}

impl NoteReadTool {
    pub fn new(db: SqlitePool) -> Self { Self { db } }
}

#[async_trait]
impl Tool for NoteReadTool {
    fn name(&self) -> &str { "note_read" }

    fn readonly(&self) -> bool { true }

    fn description(&self) -> &str {
        "Read a note by its ID or search notes by title/content (full-text). If note_id is provided, returns that note. Otherwise searches all notes."
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![
            ToolParameter {
                name: "note_id".into(), param_type: "string".into(),
                description: "Optional UUID of a specific note to read".into(), required: false,
            },
            ToolParameter {
                name: "search".into(), param_type: "string".into(),
                description: "Search term to find notes (if note_id not provided)".into(), required: false,
            },
            ToolParameter {
                name: "limit".into(), param_type: "integer".into(),
                description: "Maximum results (default 10, max 50)".into(), required: false,
            },
            ToolParameter {
                name: "offset".into(), param_type: "integer".into(),
                description: "Number of results to skip (default 0)".into(), required: false,
            },
        ]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        if let Some(id) = args["note_id"].as_str() {
            let note = sqlx::query_as::<_, crate::core::models::Note>(
                "SELECT * FROM notes WHERE id = ?"
            ).bind(id).fetch_optional(&self.db).await
                .map_err(|e| format!("db error: {e}"))?
                .ok_or("note not found")?;

            return Ok(format!("**{}**\n\n{}", note.title, note.content));
        }

        // Search (full-text, LIKE fallback)
        let search = args["search"].as_str().unwrap_or("");
        let limit = args["limit"].as_u64().unwrap_or(10).min(50) as i64;
        let offset = args["offset"].as_u64().unwrap_or(0) as i64;

        let notes: Vec<(String, String, String)> = if search.trim().is_empty() {
            sqlx::query_as(
                "SELECT id, title, content_plain FROM notes ORDER BY updated_at DESC LIMIT ? OFFSET ?"
            ).bind(limit).bind(offset).fetch_all(&self.db).await
                .map_err(|e| format!("db error: {e}"))?
        } else {
            let match_expr = search.split_whitespace()
                .map(|t| format!("\"{}\"*", t.trim_matches('"')))
                .collect::<Vec<_>>().join(" ");
            match sqlx::query_as(
                "SELECT n.id, n.title, n.content_plain FROM notes_fts f JOIN notes n ON n.rowid = f.rowid \
                 WHERE notes_fts MATCH ? ORDER BY bm25(notes_fts) LIMIT ? OFFSET ?"
            ).bind(match_expr).bind(limit).bind(offset).fetch_all(&self.db).await {
                Ok(rows) => rows,
                Err(_) => {
                    let pattern = format!("%{}%", search);
                    sqlx::query_as(
                        "SELECT id, title, content_plain FROM notes WHERE title LIKE ? OR content_plain LIKE ? ORDER BY updated_at DESC LIMIT ? OFFSET ?"
                    ).bind(&pattern).bind(&pattern).bind(limit).bind(offset).fetch_all(&self.db).await
                        .map_err(|e| format!("db error: {e}"))?
                }
            }
        };

        if notes.is_empty() {
            return Ok("No notes found.".to_string());
        }

        let preview_limit = crate::core::settings_service::cached_settings()
            .tool_note_read_max_chars
            .max(1) as usize;
        Ok(notes.iter().map(|(id, title, content)| {
            format!("- **{}** (id: {})\n  {}", title, id, content.chars().take(preview_limit).collect::<String>())
        }).collect::<Vec<_>>().join("\n\n"))
    }
}
