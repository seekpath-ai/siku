use async_trait::async_trait;
use sqlx::SqlitePool;
use crate::ai::agent::tool_registry::{Tool, ToolParameter};

pub struct KnowledgeQueryTool {
    db: SqlitePool,
}

impl KnowledgeQueryTool {
    pub fn new(db: SqlitePool) -> Self {
        Self { db }
    }
}

#[async_trait]
impl Tool for KnowledgeQueryTool {
    fn name(&self) -> &str { "knowledge_query" }

    fn readonly(&self) -> bool { true }

    fn description(&self) -> &str {
        "Search the user's knowledge base across all domains (research, learning, life, reading, notes) with full-text search. Finds items by title or content. Supports domain filter and pagination."
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![
            ToolParameter {
                name: "query".into(), param_type: "string".into(),
                description: "Search query for the knowledge base".into(), required: true,
            },
            ToolParameter {
                name: "domain".into(), param_type: "string".into(),
                description: "Optional domain filter: research, learning, life, reading, notes".into(), required: false,
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
        let query = args["query"].as_str().unwrap_or("");
        let domain = args["domain"].as_str();
        let limit = args["limit"].as_u64().unwrap_or(10).min(50) as i64;
        let offset = args["offset"].as_u64().unwrap_or(0) as i64;

        // Full-text search with optional domain filter; LIKE fallback. Each
        // branch also fetches the total match count for the 共 N 条 footer.
        let match_expr = query.split_whitespace()
            .map(|t| format!("\"{}\"*", t.trim_matches('"')))
            .collect::<Vec<_>>().join(" ");

        let (items, total): (Vec<(String, String, Option<String>, String)>, i64) = if query.trim().is_empty() {
            (Vec::new(), 0)
        } else if let Some(d) = domain {
            let sql =
                "SELECT ki.id, ki.title, ki.content, kd.name as domain_name \
                 FROM knowledge_items_fts f JOIN knowledge_items ki ON ki.rowid = f.rowid \
                 JOIN knowledge_domains kd ON ki.domain_id = kd.id \
                 WHERE knowledge_items_fts MATCH ? AND kd.domain_type = ? \
                 ORDER BY bm25(knowledge_items_fts) LIMIT ? OFFSET ?";
            match sqlx::query_as(sql).bind(&match_expr).bind(d).bind(limit).bind(offset).fetch_all(&self.db).await {
                Ok(rows) => {
                    let total: (i64,) = sqlx::query_as(
                        "SELECT count(*) FROM knowledge_items_fts f JOIN knowledge_items ki ON ki.rowid = f.rowid \
                         JOIN knowledge_domains kd ON ki.domain_id = kd.id \
                         WHERE knowledge_items_fts MATCH ? AND kd.domain_type = ?"
                    ).bind(&match_expr).bind(d).fetch_one(&self.db).await
                        .map_err(|e| format!("db error: {e}"))?;
                    (rows, total.0)
                }
                Err(_) => {
                    let pattern = format!("%{}%", query);
                    let rows = sqlx::query_as::<_, (String, String, Option<String>, String)>(
                        "SELECT ki.id, ki.title, ki.content, kd.name as domain_name \
                         FROM knowledge_items ki JOIN knowledge_domains kd ON ki.domain_id = kd.id \
                         WHERE (ki.title LIKE ? OR ki.content LIKE ?) AND kd.domain_type = ? \
                         ORDER BY ki.updated_at DESC LIMIT ? OFFSET ?"
                    ).bind(&pattern).bind(&pattern).bind(d).bind(limit).bind(offset).fetch_all(&self.db).await
                        .map_err(|e| format!("db error: {e}"))?;
                    let total: (i64,) = sqlx::query_as(
                        "SELECT count(*) FROM knowledge_items ki JOIN knowledge_domains kd ON ki.domain_id = kd.id \
                         WHERE (ki.title LIKE ? OR ki.content LIKE ?) AND kd.domain_type = ?"
                    ).bind(&pattern).bind(&pattern).bind(d).fetch_one(&self.db).await
                        .map_err(|e| format!("db error: {e}"))?;
                    (rows, total.0)
                }
            }
        } else {
            let sql =
                "SELECT ki.id, ki.title, ki.content, kd.name as domain_name \
                 FROM knowledge_items_fts f JOIN knowledge_items ki ON ki.rowid = f.rowid \
                 JOIN knowledge_domains kd ON ki.domain_id = kd.id \
                 WHERE knowledge_items_fts MATCH ? \
                 ORDER BY bm25(knowledge_items_fts) LIMIT ? OFFSET ?";
            match sqlx::query_as(sql).bind(&match_expr).bind(limit).bind(offset).fetch_all(&self.db).await {
                Ok(rows) => {
                    let total: (i64,) = sqlx::query_as(
                        "SELECT count(*) FROM knowledge_items_fts f JOIN knowledge_items ki ON ki.rowid = f.rowid \
                         WHERE knowledge_items_fts MATCH ?"
                    ).bind(&match_expr).fetch_one(&self.db).await
                        .map_err(|e| format!("db error: {e}"))?;
                    (rows, total.0)
                }
                Err(_) => {
                    let pattern = format!("%{}%", query);
                    let rows = sqlx::query_as::<_, (String, String, Option<String>, String)>(
                        "SELECT ki.id, ki.title, ki.content, kd.name as domain_name \
                         FROM knowledge_items ki JOIN knowledge_domains kd ON ki.domain_id = kd.id \
                         WHERE ki.title LIKE ? OR ki.content LIKE ? \
                         ORDER BY ki.updated_at DESC LIMIT ? OFFSET ?"
                    ).bind(&pattern).bind(&pattern).bind(limit).bind(offset).fetch_all(&self.db).await
                        .map_err(|e| format!("db error: {e}"))?;
                    let total: (i64,) = sqlx::query_as(
                        "SELECT count(*) FROM knowledge_items ki \
                         WHERE ki.title LIKE ? OR ki.content LIKE ?"
                    ).bind(&pattern).bind(&pattern).fetch_one(&self.db).await
                        .map_err(|e| format!("db error: {e}"))?;
                    (rows, total.0)
                }
            }
        };

        if items.is_empty() {
            return Ok("No knowledge items found matching your query.".to_string());
        }

        let preview_limit = crate::core::settings_service::cached_settings()
            .tool_knowledge_read_max_chars
            .max(1) as usize;
        let from = offset + 1;
        let to = offset + items.len() as i64;
        let mut out: Vec<String> = items.iter().map(|(id, title, content, domain_name)| {
            let full = content.as_deref().unwrap_or("");
            let mut preview = full.chars().take(preview_limit).collect::<String>();
            if full.chars().count() > preview_limit {
                preview.push_str("…[truncated]");
            }
            format!("- **{title}** [{domain_name}] (id: {id})\n  {preview}")
        }).collect();
        out.push(format!("共 {total} 条,显示第 {from}–{to} 条"));
        Ok(out.join("\n\n"))
    }
}
