use async_trait::async_trait;
use crate::ai::agent::tool_registry::{Tool, ToolParameter};

/// Build an FTS5 MATCH expression from a free-text query: each token becomes
/// a quoted prefix term (`"token"*`), joined by spaces (implicit AND).
fn fts_query(query: &str) -> String {
    query
        .split_whitespace()
        .map(|t| format!("\"{}\"*", t.trim_matches('"').trim_matches('"')))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Search papers in the user's library
pub struct PaperSearchTool {
    db: sqlx::SqlitePool,
}

impl PaperSearchTool {
    pub fn new(db: sqlx::SqlitePool) -> Self {
        Self { db }
    }
}

#[async_trait]
impl Tool for PaperSearchTool {
    fn name(&self) -> &str {
        "paper_search"
    }

    fn readonly(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Search papers in the user's library by title, author, keywords, or abstract content. Uses full-text search. Returns matching papers with metadata. Supports pagination via offset."
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![
            ToolParameter {
                name: "query".to_string(),
                param_type: "string".to_string(),
                description: "Search query — matches title, authors, keywords, and abstract".to_string(),
                required: true,
            },
            ToolParameter {
                name: "limit".to_string(),
                param_type: "integer".to_string(),
                description: "Maximum number of results to return (default 5, max 50)".to_string(),
                required: false,
            },
            ToolParameter {
                name: "offset".to_string(),
                param_type: "integer".to_string(),
                description: "Number of results to skip (default 0)".to_string(),
                required: false,
            },
        ]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let query = args["query"].as_str().unwrap_or("");
        let limit = args["limit"].as_u64().unwrap_or(5).min(50) as i64;
        let offset = args["offset"].as_u64().unwrap_or(0) as i64;

        let papers: Vec<crate::core::models::Paper> = if query.trim().is_empty() {
            Vec::new()
        } else {
            // Full-text search first; fall back to LIKE if FTS rejects the query.
            match sqlx::query_as(
                "SELECT p.* FROM papers_fts f JOIN papers p ON p.rowid = f.rowid \
                 WHERE papers_fts MATCH ? ORDER BY bm25(papers_fts) LIMIT ? OFFSET ?"
            )
            .bind(fts_query(query))
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.db)
            .await
            {
                Ok(rows) => rows,
                Err(_) => {
                    let pattern = format!("%{}%", query);
                    sqlx::query_as::<_, crate::core::models::Paper>(
                        "SELECT * FROM papers WHERE title LIKE ? OR authors LIKE ? OR keywords LIKE ? OR abstract LIKE ? \
                         ORDER BY imported_at DESC LIMIT ? OFFSET ?"
                    )
                    .bind(&pattern)
                    .bind(&pattern)
                    .bind(&pattern)
                    .bind(&pattern)
                    .bind(limit)
                    .bind(offset)
                    .fetch_all(&self.db)
                    .await
                    .map_err(|e| format!("database error: {e}"))?
                }
            }
        };

        if papers.is_empty() {
            return Ok("No papers found matching your query.".to_string());
        }

        let results: Vec<String> = papers
            .iter()
            .map(|p| {
                format!(
                    "- **{}** ({}) — {} — {}\n  id: {}",
                    p.title,
                    p.year.map(|y| y.to_string()).unwrap_or_else(|| "N/A".into()),
                    super::format_author_list(&p.authors),
                    p.abstract_text.as_deref().unwrap_or("No abstract"),
                    p.id,
                )
            })
            .collect();

        Ok(results.join("\n"))
    }
}
