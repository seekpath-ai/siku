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
    /// Ancestor chain of the section ("A > B > C"), when the index has it.
    pub section_path: Option<String>,
    /// "prose" | "heading" | "caption" | "reference".
    pub block_type: String,
    pub is_tail: bool,
    pub paper_title: String,
    pub score: f32,
    pub source: String, // "fts5" | "vector" | "rrf"
}

/// Hybrid search: FTS5 keyword + vector similarity + RRF fusion.
///
/// `include_tail = false` (the default) drops the references/appendix chunks
/// that `chunk_pages` labels with `is_tail`; they answer "what does this paper
/// cite?" but otherwise crowd out body content.
#[instrument(skip(db))]
pub async fn hybrid_search(
    db: &SqlitePool,
    query: &str,
    top_k: usize,
) -> Result<Vec<SearchResult>, String> {
    hybrid_search_with(db, query, top_k, false).await
}

/// As [`hybrid_search`], with the references/appendix tail selectable.
pub async fn hybrid_search_with(
    db: &SqlitePool,
    query: &str,
    top_k: usize,
    include_tail: bool,
) -> Result<Vec<SearchResult>, String> {
    // 1. FTS5 keyword search
    let fts5_results = fts5_search(db, query, top_k * 2, include_tail).await.unwrap_or_default();

    // 2. Vector similarity search — only when a real embedding backend is
    //    configured. The built-in "hash" backend is a character-histogram
    //    placeholder whose cosine similarity is not semantic, so fusing it
    //    would add noise rather than recall.
    let vector_results = if vector_leg_enabled() {
        vector_search(db, query, top_k * 2, include_tail).await.unwrap_or_default()
    } else {
        Vec::new()
    };

    // 3. RRF fusion
    let fused = rrf_fuse(&fts5_results, &vector_results, top_k);

    Ok(fused)
}

/// Whether the vector leg may contribute results.
///
/// True only for a real embedding backend ("api" with a model configured, or a
/// future local ONNX backend). See `embedder::generate_embeddings_for_paper`.
pub fn vector_leg_enabled() -> bool {
    let settings = crate::core::settings_service::cached_settings();
    settings.embedding_backend == "api" && !settings.embedding_base_url.trim().is_empty()
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
    include_tail: bool,
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

    let rows = sqlx::query_as::<_, (String, String, String, Option<i32>, Option<i32>, Option<String>, Option<String>, String, i64, String, f64)>(
        "SELECT c.id, c.content, c.paper_id, c.page_start, c.page_end, c.section,
                c.section_path, c.block_type, c.is_tail, p.title, rank
         FROM chunks_fts fts
         JOIN chunks c ON fts.rowid = c.rowid
         JOIN papers p ON c.paper_id = p.id
         WHERE chunks_fts MATCH ? AND (? = 1 OR c.is_tail = 0)
         ORDER BY rank
         LIMIT ?"
    )
    .bind(&fts_query)
    .bind(if include_tail { 1 } else { 0 })
    .bind(limit as i64)
    .fetch_all(db)
    .await
    .map_err(|e| format!("fts5: {e}"))?;

    Ok(rows.into_iter().map(|(id, content, paper_id, ps, pe, section, path, block, is_tail, title, rank)| {
        SearchResult {
            chunk_id: id, paper_id, content, page_start: ps, page_end: pe,
            section, section_path: path, block_type: block, is_tail: is_tail != 0,
            paper_title: title,
            score: (1.0 / (1.0 + rank as f32)), source: "fts5".into(),
        }
    }).collect())
}

/// Vector similarity search on stored embeddings
async fn vector_search(
    db: &SqlitePool,
    query: &str,
    limit: usize,
    include_tail: bool,
) -> Result<Vec<SearchResult>, String> {
    let query_vec = embedder::embed_query(db, query).await;
    let model = embedder::embedding_model_label();

    // Only vectors produced by the ACTIVE model may be compared: a stale row
    // from another backend has a different space (and often a different
    // dimension), and `cosine_similarity` silently truncates to the shorter
    // vector, which would produce plausible-looking nonsense.
    //
    // Full scan for now — fine for a few thousand chunks, and honest about it:
    // there is no ANN index (no sqlite-vec in the build).
    let rows = sqlx::query_as::<_, (String, Vec<u8>, i32, String, String, Option<i32>, Option<i32>, Option<String>, Option<String>, String, i64, String)>(
        "SELECT e.chunk_id, e.vector, e.dimensions, c.content, c.paper_id, c.page_start,
                c.page_end, c.section, c.section_path, c.block_type, c.is_tail, p.title
         FROM embeddings e
         JOIN chunks c ON e.chunk_id = c.id
         JOIN papers p ON c.paper_id = p.id
         WHERE e.model = ? AND (? = 1 OR c.is_tail = 0)"
    )
    .bind(&model)
    .bind(if include_tail { 1 } else { 0 })
    .fetch_all(db)
    .await
    .map_err(|e| format!("vector: {e}"))?;

    let mut scored: Vec<(SearchResult, f32)> = rows
        .into_iter()
        .filter(|(_, _, dims, _, _, _, _, _, _, _, _, _)| *dims as usize == query_vec.len())
        .map(|(id, blob, _dims, content, paper_id, ps, pe, section, path, block, is_tail, title)| {
            let vec = embedder::blob_to_vector(&blob);
            let sim = embedder::cosine_similarity(&query_vec, &vec);
            (SearchResult {
                chunk_id: id, paper_id, content, page_start: ps, page_end: pe,
                section, section_path: path, block_type: block, is_tail: is_tail != 0,
                paper_title: title,
                score: sim, source: "vector".into(),
            }, sim)
        })
        .collect();

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
