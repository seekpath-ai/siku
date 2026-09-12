use async_trait::async_trait;
use sqlx::SqlitePool;
use crate::ai::agent::tool_registry::{Tool, ToolParameter};

pub struct PaperReadTool {
    db: SqlitePool,
}

impl PaperReadTool {
    pub fn new(db: SqlitePool) -> Self {
        Self { db }
    }
}
#[async_trait]
impl Tool for PaperReadTool {
    fn name(&self) -> &str { "paper_read" }

    fn readonly(&self) -> bool { true }

    fn description(&self) -> &str {
        "Get a paper's metadata, abstract and paginated text chunks. \
         Raw chunks target ~512 tokens (2–3k characters for English prose). Each chunk is \
         returned whole by default; every call also counts against a per-call character budget. \
         Chunks carry their section path and kind (prose/heading/caption/reference), and the \
         references/appendix tail is labelled rather than hidden. Use offset/limit to read only \
         the parts relevant to the question."
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![
            ToolParameter {
                name: "paper_id".into(),
                param_type: "string".into(),
                description: "The UUID of the paper to read".into(),
                required: true,
            },
            ToolParameter {
                name: "include_chunks".into(),
                param_type: "boolean".into(),
                description: "Whether to include the paper's text chunks (default false)".into(),
                required: false,
            },
            ToolParameter {
                name: "offset".into(),
                param_type: "integer".into(),
                description: "Chunk index to start from (default 0)".into(),
                required: false,
            },
            ToolParameter {
                name: "limit".into(),
                param_type: "integer".into(),
                description: "Number of chunks to return (default 20, max 50)".into(),
                required: false,
            },
            ToolParameter {
                name: "max_chars".into(),
                param_type: "integer".into(),
                description: "Per-chunk character cap. Defaults to the app setting (2500 ≈ a whole chunk), so chunks normally arrive untruncated. Lower it to scan many chunks cheaply; raise it for unusually long chunks.".into(),
                required: false,
            },
        ]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let paper_id = args["paper_id"].as_str().ok_or("paper_id required")?;
        let include_chunks = args["include_chunks"].as_bool().unwrap_or(false);
        let offset = args["offset"].as_i64().unwrap_or(0).max(0);
        let limit = args["limit"].as_i64().unwrap_or(20).clamp(1, 50);

        // Only the columns actually displayed — papers has 40+ columns
        // including large bibtex/file blobs.
        let paper: Option<(String, String, Option<i32>, Option<String>, Option<String>, Option<i32>, Option<String>)> = sqlx::query_as(
            "SELECT title, authors, year, journal, doi, page_count, abstract FROM papers WHERE id = ?"
        )
        .bind(paper_id)
        .fetch_optional(&self.db)
        .await
        .map_err(|e| format!("db error: {e}"))?;
        let (title, authors, year, journal, doi, page_count, abstract_text) =
            paper.ok_or_else(|| format!("paper not found: {paper_id}"))?;

        let mut result = format!(
            "**{}**\nAuthors: {}\nYear: {}\nJournal: {}\nDOI: {}\nPages: {}\n\nAbstract: {}",
            title,
            super::format_author_list(&authors),
            year.map(|y| y.to_string()).unwrap_or_else(|| "N/A".into()),
            journal.as_deref().unwrap_or("N/A"),
            doi.as_deref().unwrap_or("N/A"),
            page_count.map(|n| n.to_string()).unwrap_or_else(|| "N/A".into()),
            abstract_text.as_deref().unwrap_or("No abstract"),
        );

        let total: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM chunks WHERE paper_id = ?")
            .bind(paper_id)
            .fetch_one(&self.db)
            .await
            .map_err(|e| format!("db error: {e}"))?;
        let total = total.0;

        if total == 0 {
            if include_chunks {
                result.push_str(
                    "\n\n--- Text Chunks ---\n(no chunks — the PDF may not be indexed yet, or it has no text layer)",
                );
            }
            return Ok(result);
        }

        // Structure metadata (section path, block type, references tail) is
        // computed while indexing, so the boundary is read from the index rather
        // than re-derived on every call. The tail is LABELLED, never hidden: the
        // previous behaviour truncated pagination at the boundary, so one wrong
        // boundary made the rest of a paper unreachable.
        let rows: Vec<(i32, String, Option<i32>, Option<i32>, i64)> = sqlx::query_as(
            "SELECT chunk_index, content, page_start, page_end, is_tail \
             FROM chunks WHERE paper_id = ? ORDER BY chunk_index",
        )
        .bind(paper_id)
        .fetch_all(&self.db)
        .await
        .map_err(|e| format!("db error: {e}"))?;

        let body_end = rows
            .iter()
            .find(|(_, _, _, _, is_tail)| *is_tail != 0)
            .map(|(idx, _, _, _, _)| *idx)
            .or_else(|| {
                // Index built before the structure migration: derive it here.
                crate::pdf::chunker::detect_body_end(
                    rows.iter().map(|(i, c, _, _, _)| (*i, c.as_str())),
                )
            });
        let end_page = body_end.and_then(|end| {
            rows.iter()
                .find(|(i, _, _, _, _)| *i == end)
                .and_then(|(_, _, ps, _, _)| *ps)
        });

        // Navigation hint: the model pages by this instead of fetching everything.
        result.push_str(&format!("\n\nChunks: {total} (paginate with offset/limit)"));
        if let Some(end) = body_end {
            result.push_str(&format!(
                "\nReferences/appendix start at chunk {end}{}; those chunks are labelled in the output and can be read normally.",
                end_page.map(|p| format!(" (p.{p})")).unwrap_or_default(),
            ));
        }

        if !include_chunks {
            return Ok(result);
        }

        if offset >= total {
            result.push_str(&format!(
                "\n\n--- Text Chunks (0 of {total}) ---\noffset {offset} is past the end — this paper has {total} chunks in total",
            ));
            return Ok(result);
        }

        let limit = limit.min(total - offset);
        let selected: Vec<&(i32, String, Option<i32>, Option<i32>, i64)> =
            rows.iter().skip(offset as usize).take(limit as usize).collect();

        let from = offset + 1;
        let to = offset + selected.len() as i64;
        result.push_str(&format!("\n\n--- Text Chunks ({from}-{to} of {total}) ---\n"));

        let chunk_limit = args["max_chars"].as_i64()
            .filter(|v| *v > 0)
            .map(|v| v as usize)
            .unwrap_or_else(|| {
                crate::core::settings_service::cached_settings()
                    .tool_paper_read_max_chars
                    .max(1) as usize
            });
        // Per-CALL total budget: bounds the output no matter what limit and
        // max_chars the model picks.
        let total_budget = crate::core::settings_service::cached_settings()
            .tool_paper_read_total_max_chars
            .max(1) as usize;

        let page = |v: &Option<i32>| v.map(|n| n.to_string()).unwrap_or_else(|| "?".into());
        let mut spent = 0usize;
        let mut emitted = 0usize;
        for row in &selected {
            let (idx, content, ps, pe, is_tail) = (&row.0, &row.1, &row.2, &row.3, row.4);
            // Truncate over-long chunks, but say so — otherwise the agent
            // may quote a partial chunk as if it were complete.
            let mut chars = content.chars();
            let text: String = chars.by_ref().take(chunk_limit).collect();
            let marker = if chars.next().is_some() { " … [truncated]" } else { "" };
            let text_len = text.chars().count();
            if emitted > 0 && spent + text_len > total_budget {
                result.push_str(&format!(
                    "(output budget exhausted — spent {spent} of {total_budget} chars this call; \
                     {} more chunk(s) in this range; call again with offset {idx})\n",
                    selected.len() - emitted,
                ));
                break;
            }
            let kind = if is_tail != 0 { ", references/appendix" } else { "" };
            result.push_str(&format!(
                "[Chunk {} (p.{}-{}{kind})] {}{}\n\n",
                idx,
                page(ps),
                page(pe),
                text,
                marker,
            ));
            spent += text_len;
            emitted += 1;
        }
        if emitted == selected.len() && to < total {
            result.push_str(&format!("(more chunks available — call again with offset {to})\n"));
        }
        Ok(result)
    }
}
