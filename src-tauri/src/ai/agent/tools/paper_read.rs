use async_trait::async_trait;
use regex::Regex;
use sqlx::SqlitePool;
use std::sync::OnceLock;
use crate::ai::agent::tool_registry::{Tool, ToolParameter};

pub struct PaperReadTool {
    db: SqlitePool,
}

impl PaperReadTool {
    pub fn new(db: SqlitePool) -> Self { Self { db } }
}

/// Find the chunk where the references/appendix tail begins.
///
/// PDF text extraction reflows the layout — headings do NOT sit on their own
/// lines (verified against the real chunk store), so this matches an inline,
/// case-sensitive heading (`References`, incl. the "R EFERENCES" small-caps
/// extraction artifact) that is IMMEDIATELY followed by the first reference
/// entry: `[1]` (numeric style), `Adlakha, …` or `Anthropic. 2024` (author-year
/// style). Prose mentions ("meme references", "see Appendix B") and TOC lines
/// ("References ......... 59") never match because of the entry requirement.
/// Validated 7/7 papers against the production chunk store.
fn detect_body_end(chunks: &[(i32, Option<i32>, String)]) -> Option<i32> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        const NAME: &str = r"(?:[A-Z][A-Za-z\-']+\s+){0,3}[A-Z][A-Za-z\-']+";
        Regex::new(&format!(
            r"(?:\d{{1,2}}\s+)?(?:References|Bibliography|R EFERENCES|参考文献)\s*(?:\[1\]|{NAME},|{NAME}\.\s*(?:19|20)\d{{2}})"
        ))
        .expect("tail heading regex")
    });
    // Skip chunk 0 (title page) — a false positive there would hide the body.
    for (idx, _, content) in chunks {
        if *idx > 0 && re.is_match(content) {
            return Some(*idx);
        }
    }
    None
}

#[async_trait]
impl Tool for PaperReadTool {
    fn name(&self) -> &str { "paper_read" }

    fn readonly(&self) -> bool { true }

    fn description(&self) -> &str {
        "Get a paper's metadata, abstract and paginated text chunks. \
         Raw chunks target ~512 tokens, which is roughly 500–4000 characters depending on the \
         language and whether a long paragraph/sentence could not be split. Every call counts \
         against your context budget, which is also capped per call. The response marks where \
         the body ends — chunks after that (references/appendix) are excluded by default. Use \
         offset/limit to read only the parts relevant to the question, and set max_chars high \
         enough to avoid truncating the chunks you need."
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
                description: "Per-chunk character cap. Default comes from the app settings (typically 500), which truncates most chunks to short previews — fine for locating content. Raw chunks can reach several thousand characters when a long paragraph or sentence cannot be split; set this high enough to avoid truncation.".into(),
                required: false,
            },
            ToolParameter {
                name: "include_tail".into(),
                param_type: "boolean".into(),
                description: "Chunks after the body end (references/appendix) are excluded by default; set true to include them (default false)".into(),
                required: false,
            },
        ]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let paper_id = args["paper_id"].as_str().ok_or("paper_id required")?;
        let include_chunks = args["include_chunks"].as_bool().unwrap_or(false);
        let include_tail = args["include_tail"].as_bool().unwrap_or(false);
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

        // Scan chunk contents once for the body-end boundary.
        let all_chunks: Vec<(i32, Option<i32>, String)> = sqlx::query_as(
            "SELECT chunk_index, page_start, content FROM chunks WHERE paper_id = ? ORDER BY chunk_index"
        )
        .bind(paper_id)
        .fetch_all(&self.db)
        .await
        .map_err(|e| format!("db error: {e}"))?;
        let body_end = detect_body_end(&all_chunks);

        // Reading boundary: the model navigates by this instead of fetching
        // everything.
        result.push_str(&format!("\n\nChunks: {total} (paginate with offset/limit)"));
        if let Some(end) = body_end {
            let end_page = all_chunks.iter()
                .find(|(idx, _, _)| *idx == end)
                .and_then(|(_, ps, _)| *ps);
            result.push_str(&format!(
                "\nBody ends at chunk {end}{}. Chunks {end}-{total_minus_1} are references/appendix — excluded from reads by default; pass include_tail=true if you really need them.",
                end_page.map(|p| format!(" (p.{p})")).unwrap_or_default(),
                total_minus_1 = total - 1,
            ));
        }

        if !include_chunks {
            return Ok(result);
        }

        // Chunks past the body end are unreachable unless explicitly asked for.
        let readable_end = match body_end {
            Some(end) if !include_tail => end as i64,
            _ => total,
        };

        if offset >= readable_end {
            if readable_end < total {
                result.push_str(&format!(
                    "\n\n--- Text Chunks ---\noffset {offset} is inside the excluded tail (references/appendix, chunks {readable_end}-{}) — pass include_tail=true to read them",
                    total - 1,
                ));
            } else {
                result.push_str(&format!(
                    "\n\n--- Text Chunks (0 of {total}) ---\noffset {offset} is past the end — this paper has {total} chunks in total",
                ));
            }
            return Ok(result);
        }

        let limit = limit.min(readable_end - offset);
        let chunks: Vec<(String, i32, Option<i32>, Option<i32>)> = sqlx::query_as(
            "SELECT content, chunk_index, page_start, page_end FROM chunks WHERE paper_id = ? ORDER BY chunk_index LIMIT ? OFFSET ?"
        )
        .bind(paper_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.db)
        .await
        .map_err(|e| format!("db error: {e}"))?;

        let from = offset + 1;
        let to = offset + chunks.len() as i64;
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
        for (content, idx, ps, pe) in &chunks {
            // Truncate over-long chunks, but say so — otherwise the agent
            // may quote a partial chunk as if it were complete.
            let mut chars = content.chars();
            let text: String = chars.by_ref().take(chunk_limit).collect();
            let marker = if chars.next().is_some() { " … [truncated]" } else { "" };
            let text_len = text.chars().count();
            if emitted > 0 && spent + text_len > total_budget {
                result.push_str(&format!(
                    "(output budget exhausted — {} more chunk(s) in this range; call again with offset {idx})\n",
                    chunks.len() - emitted,
                ));
                break;
            }
            result.push_str(&format!(
                "[Chunk {} (p.{}-{})] {}{}\n\n",
                idx,
                page(ps),
                page(pe),
                text,
                marker,
            ));
            spent += text_len;
            emitted += 1;
        }
        if emitted == chunks.len() {
            if to < readable_end {
                result.push_str(&format!("(more chunks available — call again with offset {to})\n"));
            } else if readable_end < total {
                result.push_str(&format!(
                    "(end of body — chunks {readable_end}-{} are references/appendix, excluded by default)\n",
                    total - 1,
                ));
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunks(contents: &[&str]) -> Vec<(i32, Option<i32>, String)> {
        contents
            .iter()
            .enumerate()
            .map(|(i, c)| (i as i32, Some(i as i32 + 1), c.to_string()))
            .collect()
    }

    #[test]
    fn detects_numeric_style_references() {
        let c = chunks(&[
            "body text",
            "supported in part by NSF CNS-2145295. References [1] Chaos mesh: A powerful chaos engineering platform",
            "[2] Claude Code by Anthropic",
        ]);
        assert_eq!(detect_body_end(&c), Some(1));
    }

    #[test]
    fn detects_author_year_style_references() {
        let c = chunks(&[
            "body",
            "References Vaibhav Adlakha, Parishad BehnamGhader, Xing Han Lu",
            "tail",
        ]);
        assert_eq!(detect_body_end(&c), Some(1));
    }

    #[test]
    fn detects_year_after_name_style() {
        let c = chunks(&[
            "body",
            "correspondence to: panlu@stanford.edu. References Anthropic. 2024. Claude 3.5 haiku",
            "tail",
        ]);
        assert_eq!(detect_body_end(&c), Some(1));
    }

    #[test]
    fn detects_smallcaps_extraction_variant() {
        let c = chunks(&[
            "body",
            "of AIOps tasks. R EFERENCES Josh Achiam, Steven Adler, Sandhini Agarwal",
            "tail",
        ]);
        assert_eq!(detect_body_end(&c), Some(1));
    }

    #[test]
    fn rejects_prose_and_toc() {
        let c = chunks(&[
            "Contents: References ......... 59",
            "find reliable sources or references that explain the conversion process",
            "we refer the reader to Appendix A for details",
        ]);
        assert_eq!(detect_body_end(&c), None);
    }

    #[test]
    fn skips_title_chunk() {
        let c = chunks(&["References [1] bogus match on the title chunk", "real body"]);
        assert_eq!(detect_body_end(&c), None);
    }
}

