use sqlx::SqlitePool;
use tracing::instrument;

use crate::ai::embedder;

/// A search result with relevance score
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub chunk_id: String,
    pub paper_id: String,
    pub content: String,
    pub page_start: Option<i32>,
    pub page_end: Option<i32>,
    pub section: Option<String>,
    pub paper_title: String,
    pub score: f32,
    pub source: String, // "fts5" | "vector" | "rrf"
}

/// Hybrid search: FTS5 keyword + vector similarity + RRF fusion
#[instrument(skip(db))]
pub async fn hybrid_search(
    db: &SqlitePool,
    query: &str,
    top_k: usize,
) -> Result<Vec<SearchResult>, String> {
    // 1. FTS5 keyword search
    let fts5_results = fts5_search(db, query, top_k * 2).await.unwrap_or_default();

    // 2. Vector similarity search (if embeddings exist)
    let vector_results = vector_search(db, query, top_k * 2).await.unwrap_or_default();

    // 3. RRF fusion
    let fused = rrf_fuse(&fts5_results, &vector_results, top_k);

    Ok(fused)
}

/// FTS5 keyword search on chunks.
///
/// The index uses the trigram tokenizer (CJK-friendly). Trigram requires
/// terms of at least 3 characters; shorter tokens are skipped. When nothing
/// qualifies, an empty result set is returned and the vector leg carries
/// the query in the RRF fusion.
async fn fts5_search(
    db: &SqlitePool,
    query: &str,
    limit: usize,
) -> Result<Vec<SearchResult>, String> {
    let fts_query = query
        .split_whitespace()
        .filter(|w| w.chars().count() >= 3)
        .map(|w| format!("{}*", w))
        .collect::<Vec<_>>()
        .join(" OR ");

    if fts_query.is_empty() {
        return Ok(Vec::new());
    }

    let rows = sqlx::query_as::<_, (String, String, String, Option<i32>, Option<i32>, Option<String>, String, f64)>(
        "SELECT c.id, c.content, c.paper_id, c.page_start, c.page_end, c.section, p.title, rank
         FROM chunks_fts fts
         JOIN chunks c ON fts.rowid = c.rowid
         JOIN papers p ON c.paper_id = p.id
         WHERE chunks_fts MATCH ?
         ORDER BY rank
         LIMIT ?"
    )
    .bind(&fts_query)
    .bind(limit as i64)
    .fetch_all(db)
    .await
    .map_err(|e| format!("fts5: {e}"))?;

    Ok(rows.into_iter().map(|(id, content, paper_id, ps, pe, section, title, rank)| {
        SearchResult {
            chunk_id: id, paper_id, content, page_start: ps, page_end: pe,
            section, paper_title: title,
            score: (1.0 / (1.0 + rank as f32)), source: "fts5".into(),
        }
    }).collect())
}

/// Vector similarity search on stored embeddings
async fn vector_search(
    db: &SqlitePool,
    query: &str,
    limit: usize,
) -> Result<Vec<SearchResult>, String> {
    let query_vec = embedder::embed_query(db, query).await;

    // Get all embeddings (for small libraries; in production use sqlite-vec ANN)
    let rows = sqlx::query_as::<_, (String, Vec<u8>, String, String, Option<i32>, Option<i32>, Option<String>, String)>(
        "SELECT e.chunk_id, e.vector, c.content, c.paper_id, c.page_start, c.page_end, c.section, p.title
         FROM embeddings e
         JOIN chunks c ON e.chunk_id = c.id
         JOIN papers p ON c.paper_id = p.id"
    )
    .fetch_all(db)
    .await
    .map_err(|e| format!("vector: {e}"))?;

    let mut scored: Vec<(SearchResult, f32)> = rows.into_iter().map(|(id, blob, content, paper_id, ps, pe, section, title)| {
        let vec = embedder::blob_to_vector(&blob);
        let sim = embedder::cosine_similarity(&query_vec, &vec);
        (SearchResult {
            chunk_id: id, paper_id, content, page_start: ps, page_end: pe,
            section, paper_title: title,
            score: sim, source: "vector".into(),
        }, sim)
    }).collect();

    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit);

    Ok(scored.into_iter().map(|(r, _)| r).collect())
}

/// Reciprocal Rank Fusion — combines two ranked lists
fn rrf_fuse(
    fts5: &[SearchResult],
    vector: &[SearchResult],
    top_k: usize,
) -> Vec<SearchResult> {
    use std::collections::HashMap;

    let k: f32 = 60.0;
    let mut scores: HashMap<String, (f32, &SearchResult)> = HashMap::new();

    for (rank, result) in fts5.iter().enumerate() {
        let rrf = 1.0 / (k + (rank + 1) as f32);
        scores.entry(result.chunk_id.clone())
            .and_modify(|(s, _)| *s += rrf)
            .or_insert((rrf, result));
    }

    for (rank, result) in vector.iter().enumerate() {
        let rrf = 1.0 / (k + (rank + 1) as f32);
        scores.entry(result.chunk_id.clone())
            .and_modify(|(s, _)| *s += rrf)
            .or_insert((rrf, result));
    }

    let mut fused: Vec<(&SearchResult, f32)> = scores.values().map(|(s, r)| (*r, *s)).collect();
    fused.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    fused.truncate(top_k);

    fused.into_iter().map(|(r, s)| {
        let mut result = r.clone();
        result.score = s;
        result.source = "rrf".into();
        result
    }).collect()
}
