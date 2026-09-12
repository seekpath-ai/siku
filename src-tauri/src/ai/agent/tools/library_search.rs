use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::ai::agent::tool_registry::{Tool, ToolParameter};

/// Full-text (plus semantic, when a real embedding backend is configured) search
/// across the indexed papers.
///
/// This is the retrieval half of the RAG pipeline. Until now that half existed
/// only behind Tauri commands (`search_hybrid` / `search_rag_query`) which no UI
/// called, so the agent could only page through papers linearly with
/// `paper_read`. Results carry the paper title, page range and section path so
/// the model can cite them or jump into `paper_read` for more context.
pub struct LibrarySearchTool {
    db: SqlitePool,
}

impl LibrarySearchTool {
    pub fn new(db: SqlitePool) -> Self {
        Self { db }
    }
}

/// Per-hit and per-call character budgets. A hit is a ~512-token chunk
/// (2–3k characters); showing the head of each is enough to judge relevance,
/// and `paper_read` fetches the whole chunk when it matters.
const PER_HIT_CHARS_FALLBACK: usize = 700;
const PER_CALL_CHARS: usize = 12_000;

#[async_trait]
impl Tool for LibrarySearchTool {
    fn name(&self) -> &str {
        "search_library"
    }

    fn readonly(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Search the full text of every indexed paper in the library and return the most relevant \
         passages with their paper title, pages and section. Use this to find a specific fact, \
         method or claim across the library, then cite the passage or pull the surrounding text \
         with paper_read. Prefer it over reading a whole paper when you are looking for something \
         specific; use paper_read when you need the full argument."
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![
            ToolParameter {
                name: "query".into(),
                param_type: "string".into(),
                description: "What to look for, in the paper's own language (e.g. \"prehospital stroke assessment workflow\"). Keywords work better than long questions.".into(),
                required: true,
            },
            ToolParameter {
                name: "limit".into(),
                param_type: "integer".into(),
                description: "Maximum number of passages to return (default 8, max 30)".into(),
                required: false,
            },
            ToolParameter {
                name: "include_references".into(),
                param_type: "boolean".into(),
                description: "Also search the references/appendix of each paper (default false). Enable it when the question is about what a paper cites or about appendix details.".into(),
                required: false,
            },
        ]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let query = args["query"].as_str().unwrap_or("").trim();
        if query.is_empty() {
            return Err("query is required".into());
        }
        let limit = args["limit"].as_u64().unwrap_or(8).clamp(1, 30) as usize;
        let include_tail = args["include_references"].as_bool().unwrap_or(false);

        let hits = crate::ai::retriever::hybrid_search_with(&self.db, query, limit, include_tail)
            .await?;

        if hits.is_empty() {
            let total: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM chunks")
                .fetch_one(&self.db)
                .await
                .map_err(|e| format!("db error: {e}"))?;
            return Ok(if total.0 == 0 {
                format!(
                    "No passages matched \"{query}\". The library has no indexed text yet — \
                     import a paper or rebuild its index first."
                )
            } else {
                format!("No passages matched \"{query}\" (the library has {} indexed passages).", total.0)
            });
        }

        let per_hit = crate::core::settings_service::cached_settings()
            .rag_chunk_max_chars
            .max(1) as usize;
        let per_hit = if per_hit == 0 { PER_HIT_CHARS_FALLBACK } else { per_hit };

        let mut out = format!("{} passage(s) for \"{query}\":\n", hits.len());
        let mut spent = 0usize;
        for (i, hit) in hits.iter().enumerate() {
            let pages = match (hit.page_start, hit.page_end) {
                (Some(a), Some(b)) if a == b => format!("p.{a}"),
                (Some(a), Some(b)) => format!("p.{a}-{b}"),
                _ => "p.?".to_string(),
            };
            let section = hit
                .section_path
                .clone()
                .or_else(|| hit.section.clone())
                .map(|s| format!(" · {s}"))
                .unwrap_or_default();
            let kind = match (hit.is_tail, hit.block_type.as_str()) {
                (true, _) => " · references/appendix",
                (false, "caption") => " · caption",
                (false, "heading") => " · heading",
                _ => "",
            };

            let mut chars = hit.content.chars();
            let text: String = chars.by_ref().take(per_hit).collect();
            let truncated = if chars.next().is_some() { " …" } else { "" };
            let block = format!(
                "[{}] {} ({pages}{section}{kind}, relevance {:.2})\n{}{}\n\n",
                i + 1,
                hit.paper_title,
                hit.score,
                text,
                truncated
            );
            if spent + block.chars().count() > PER_CALL_CHARS && i > 0 {
                out.push_str(&format!(
                    "({} more passage(s) withheld — narrow the query or read them with paper_read)\n",
                    hits.len() - i
                ));
                break;
            }
            spent += block.chars().count();
            out.push_str(&block);
        }
        Ok(out)
    }
}
