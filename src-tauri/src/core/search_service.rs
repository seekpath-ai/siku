use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tracing::instrument;

use crate::ai::retriever;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub chunk_id: String,
    pub paper_id: String,
    pub content: String,
    pub page_start: Option<i32>,
    pub page_end: Option<i32>,
    pub section: Option<String>,
    pub section_path: Option<String>,
    pub block_type: String,
    pub is_tail: bool,
    pub paper_title: String,
    pub score: f32,
    pub source: String,
}

/// Unified hybrid search across papers and chunks
#[instrument(skip(db))]
pub async fn search(
    db: &SqlitePool,
    query: &str,
    limit: usize,
) -> Result<Vec<SearchResult>, String> {
    let results = retriever::hybrid_search(db, query, limit).await?;
    Ok(results.into_iter().map(|r| SearchResult {
        chunk_id: r.chunk_id, paper_id: r.paper_id, content: r.content,
        page_start: r.page_start, page_end: r.page_end, section: r.section,
        section_path: r.section_path, block_type: r.block_type, is_tail: r.is_tail,
        paper_title: r.paper_title, score: r.score, source: r.source,
    }).collect())
}


