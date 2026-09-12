use sqlx::SqlitePool;
use tracing::instrument;

use crate::ai::embedder;

/// A search result with relevance score
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub chunk_id: String,
    pub paper_id: String,
    /// Position of the chunk inside the paper (used to group and to dedupe
    /// overlapping neighbours).
    pub chunk_index: i32,
    pub content: String,
    pub page_start: Option<i32>,
    pub page_end: Option<i32>,
    pub section: Option<String>,
    /// Ancestor chain of the section ("A > B > C"), when the index has it.
    pub section_path: Option<String>,
    /// "prose" | "heading" | "caption" | "reference".
    pub block_type: String,
    pub is_tail: bool,
    /// Retrieved as context for a neighbouring hit rather than as a hit itself.
    pub is_neighbor: bool,
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
///
/// Three legs can contribute, each with the query form it can actually serve:
///  * ASCII terms → the trigram index (`chunks_fts`), prefix-matched;
///  * CJK terms → the bigram index (`chunks_fts_bi`), where two-character words
///    are addressable at all;
///  * the vector index, when a real embedding backend is configured.
pub async fn hybrid_search_with(
    db: &SqlitePool,
    query: &str,
    top_k: usize,
    include_tail: bool,
) -> Result<Vec<SearchResult>, String> {
    let terms = crate::ai::query::analyze_query(query);
    let limit = top_k * 2;

    let mut legs: Vec<Vec<SearchResult>> = Vec::new();
    if let Some(expr) = crate::ai::query::trigram_match_expr(&terms.ascii) {
        legs.push(
            keyword_search(db, "chunks_fts", "content", &expr, limit, include_tail)
                .await
                .unwrap_or_default(),
        );
    }
    if let Some(expr) = crate::ai::query::bigram_match_expr(&terms.cjk) {
        legs.push(
            keyword_search(db, "chunks_fts_bi", "search_text", &expr, limit, include_tail)
                .await
                .unwrap_or_default(),
        );
    }
    if vector_leg_enabled() {
        legs.push(vector_search(db, query, limit, include_tail).await.unwrap_or_default());
    }

    Ok(rrf_fuse(&legs, top_k))
}

/// Whether the vector leg may contribute results.
///
/// True only for a real embedding backend ("api" with a model configured, or a
/// future local ONNX backend). See `embedder::generate_embeddings_for_paper`.
pub fn vector_leg_enabled() -> bool {
    let settings = crate::core::settings_service::cached_settings();
    settings.embedding_backend == "api" && !settings.embedding_base_url.trim().is_empty()
}

/// Keyword search on one of the chunk FTS indexes.
///
/// `table`/`column` are compile-time constants at every call site; `query`
/// holds the MATCH expression built by `ai::query` for the tokenizer in use
/// (trigram for ASCII, unicode61 bigrams for CJK).
async fn keyword_search(
    db: &SqlitePool,
    table: &str,
    column: &str,
    query: &str,
    limit: usize,
    include_tail: bool,
) -> Result<Vec<SearchResult>, String> {
    let sql = format!(
        "SELECT c.id, c.content, c.paper_id, c.chunk_index, c.page_start, c.page_end, c.section,
                c.section_path, c.block_type, c.is_tail, p.title, rank
         FROM {table} fts
         JOIN chunks c ON fts.rowid = c.rowid
         JOIN papers p ON c.paper_id = p.id
         WHERE {table} MATCH ? AND (? = 1 OR c.is_tail = 0)
         ORDER BY rank
         LIMIT ?"
    );
    let rows = sqlx::query_as::<_, (String, String, String, i32, Option<i32>, Option<i32>, Option<String>, Option<String>, String, i64, String, f64)>(
        &sql,
    )
    .bind(query)
    .bind(if include_tail { 1 } else { 0 })
    .bind(limit as i64)
    .fetch_all(db)
    .await
    .map_err(|e| format!("fts5 {column}: {e}"))?;

    Ok(rows
        .into_iter()
        .map(|(id, content, paper_id, chunk_index, ps, pe, section, path, block, is_tail, title, rank)| SearchResult {
            chunk_id: id,
            paper_id,
            chunk_index,
            content,
            page_start: ps,
            page_end: pe,
            section,
            section_path: path,
            block_type: block,
            is_tail: is_tail != 0,
            is_neighbor: false,
            paper_title: title,
            score: 1.0 / (1.0 + rank as f32),
            source: "fts5".into(),
        })
        .collect())
}

/// One row of the embedding table, with everything a hit needs.
struct CachedVector {
    chunk_id: String,
    vector: Vec<f32>,
    content: String,
    paper_id: String,
    chunk_index: i32,
    page_start: Option<i32>,
    page_end: Option<i32>,
    section: Option<String>,
    section_path: Option<String>,
    block_type: String,
    is_tail: bool,
    paper_title: String,
}

/// In-memory copy of the embedding table.
///
/// Every query used to select *all* vector blobs and deserialize them — 13 MB
/// for a 6.5k-chunk library, 130 MB per query at 65k. The cache is keyed on the
/// table row counts, which change on import / re-index / delete.
struct VectorCache {
    model: String,
    stamp: (i64, i64),
    entries: Vec<CachedVector>,
}

/// Above this many vectors the cache would cost more memory than the scans it
/// saves, so scoring falls back to reading the table (and an ANN index becomes
/// the right answer — there is none in this build).
const MAX_CACHED_VECTORS: usize = 50_000;

static VECTOR_CACHE: std::sync::OnceLock<std::sync::Mutex<Option<VectorCache>>> =
    std::sync::OnceLock::new();

/// Cosine floor: below this a vector neighbour is noise rather than a weak
/// match, and unlike an RRF rank a cosine *is* a similarity, so the floor
/// belongs here.
const MIN_COSINE: f32 = 0.25;

async fn vector_cache_stamp(db: &SqlitePool) -> Result<(i64, i64), String> {
    let embeddings: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM embeddings")
        .fetch_one(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    let chunks: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM chunks")
        .fetch_one(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    Ok((embeddings.0, chunks.0))
}

/// Vector similarity search on stored embeddings.
///
/// Only vectors produced by the *active* model are compared (a stale row lives
/// in a different space and often a different dimension; `cosine_similarity`
/// would silently truncate to the shorter vector). See
/// `embedder::generate_embeddings_for_paper`, which re-embeds stale rows.
async fn vector_search(
    db: &SqlitePool,
    query: &str,
    limit: usize,
    include_tail: bool,
) -> Result<Vec<SearchResult>, String> {
    let query_vec = embedder::embed_query(db, query).await;
    let model = embedder::embedding_model_label();
    let stamp = vector_cache_stamp(db).await?;

    let cache = VECTOR_CACHE.get_or_init(|| std::sync::Mutex::new(None));
    {
        let guard = cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(cached) = guard.as_ref() {
            if cached.model == model && cached.stamp == stamp {
                return Ok(score_vectors(&cached.entries, &query_vec, include_tail, limit));
            }
        }
    }

    let rows = sqlx::query_as::<_, (String, Vec<u8>, i32, String, String, i32, Option<i32>, Option<i32>, Option<String>, Option<String>, String, i64, String)>(
        "SELECT e.chunk_id, e.vector, e.dimensions, c.content, c.paper_id, c.chunk_index,
                c.page_start, c.page_end, c.section, c.section_path, c.block_type, c.is_tail, p.title
         FROM embeddings e
         JOIN chunks c ON e.chunk_id = c.id
         JOIN papers p ON c.paper_id = p.id
         WHERE e.model = ?",
    )
    .bind(&model)
    .fetch_all(db)
    .await
    .map_err(|e| format!("vector: {e}"))?;

    let entries: Vec<CachedVector> = rows
        .into_iter()
        .filter(|(_, _, dims, _, _, _, _, _, _, _, _, _, _)| *dims as usize == query_vec.len())
        .map(
            |(chunk_id, blob, _dims, content, paper_id, chunk_index, ps, pe, section, path, block, is_tail, title)| {
                CachedVector {
                    chunk_id,
                    vector: embedder::blob_to_vector(&blob),
                    content,
                    paper_id,
                    chunk_index,
                    page_start: ps,
                    page_end: pe,
                    section,
                    section_path: path,
                    block_type: block,
                    is_tail: is_tail != 0,
                    paper_title: title,
                }
            },
        )
        .collect();

    if entries.len() <= MAX_CACHED_VECTORS {
        let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some(VectorCache { model, stamp, entries });
        let cached = guard.as_ref().expect("just stored");
        return Ok(score_vectors(&cached.entries, &query_vec, include_tail, limit));
    }
    tracing::debug!(
        vectors = entries.len(),
        "embedding table too large to cache; scoring straight from the table"
    );
    Ok(score_vectors(&entries, &query_vec, include_tail, limit))
}

/// Score cached vectors against a query vector. Synchronous on purpose: it runs
/// while the cache lock is held, so it must not await.
fn score_vectors(
    entries: &[CachedVector],
    query_vec: &[f32],
    include_tail: bool,
    limit: usize,
) -> Vec<SearchResult> {
    let mut scored: Vec<(SearchResult, f32)> = entries
        .iter()
        .filter(|e| include_tail || !e.is_tail)
        .map(|e| {
            let sim = embedder::cosine_similarity(query_vec, &e.vector);
            (
                SearchResult {
                    chunk_id: e.chunk_id.clone(),
                    paper_id: e.paper_id.clone(),
                    chunk_index: e.chunk_index,
                    content: e.content.clone(),
                    page_start: e.page_start,
                    page_end: e.page_end,
                    section: e.section.clone(),
                    section_path: e.section_path.clone(),
                    block_type: e.block_type.clone(),
                    is_tail: e.is_tail,
                    is_neighbor: false,
                    paper_title: e.paper_title.clone(),
                    score: sim,
                    source: "vector".into(),
                },
                sim,
            )
        })
        .filter(|(_, sim)| *sim >= MIN_COSINE)
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit);
    scored.into_iter().map(|(r, _)| r).collect()
}

/// Reciprocal Rank Fusion over any number of ranked lists.
fn rrf_fuse(legs: &[Vec<SearchResult>], top_k: usize) -> Vec<SearchResult> {
    use std::collections::HashMap;

    let k: f32 = 60.0;
    let mut scores: HashMap<String, (f32, &SearchResult)> = HashMap::new();

    for leg in legs {
        for (rank, result) in leg.iter().enumerate() {
            let rrf = 1.0 / (k + (rank + 1) as f32);
            scores
                .entry(result.chunk_id.clone())
                .and_modify(|(s, _)| *s += rrf)
                .or_insert((rrf, result));
        }
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

/// Append the chunks immediately before and after each hit.
///
/// A ~512-token chunk boundary regularly cuts a sentence or a table row in half;
/// giving the model the neighbouring chunk restores the context that retrieval
/// alone loses. Neighbours are marked `is_neighbor`, scored below their anchor,
/// and never displace a real hit.
pub async fn expand_with_neighbors(
    db: &SqlitePool,
    results: &mut Vec<SearchResult>,
    include_tail: bool,
) -> Result<(), String> {
    if results.is_empty() {
        return Ok(());
    }
    let known: std::collections::HashSet<String> =
        results.iter().map(|r| r.chunk_id.clone()).collect();
    let mut extra: Vec<SearchResult> = Vec::new();
    for hit in results.iter() {
        for delta in [-1i32, 1i32] {
            let row = sqlx::query_as::<_, (String, String, String, i32, Option<i32>, Option<i32>, Option<String>, Option<String>, String, i64, String)>(
                "SELECT c.id, c.content, c.paper_id, c.chunk_index, c.page_start, c.page_end,
                        c.section, c.section_path, c.block_type, c.is_tail, p.title
                 FROM chunks c JOIN papers p ON c.paper_id = p.id
                 WHERE c.paper_id = ? AND c.chunk_index = ? AND (? = 1 OR c.is_tail = 0)",
            )
            .bind(&hit.paper_id)
            .bind(hit.chunk_index + delta)
            .bind(if include_tail { 1 } else { 0 })
            .fetch_optional(db)
            .await
            .map_err(|e| format!("db error: {e}"))?;
            if let Some((id, content, paper_id, chunk_index, ps, pe, section, path, block, is_tail, title)) = row {
                if known.contains(&id) || extra.iter().any(|e| e.chunk_id == id) {
                    continue;
                }
                extra.push(SearchResult {
                    chunk_id: id,
                    paper_id,
                    chunk_index,
                    content,
                    page_start: ps,
                    page_end: pe,
                    section,
                    section_path: path,
                    block_type: block,
                    is_tail: is_tail != 0,
                    is_neighbor: true,
                    paper_title: title,
                    score: hit.score * 0.5,
                    source: "neighbor".into(),
                });
            }
        }
    }
    results.extend(extra);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    /// A miniature library built from the real schema: two Chinese chunks, one
    /// English chunk and one references chunk.
    async fn test_db() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        sqlx::raw_sql(include_str!("../../schema_init.sql"))
            .execute(&pool)
            .await
            .expect("schema");
        sqlx::query(
            "INSERT INTO papers (id, title, created_at, updated_at) VALUES ('p1', '中文测试文献', 't', 't')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let rows: [(&str, i32, &str, i64); 4] = [
            ("c0", 0, "本文提出了一种基于注意力的语义分割方法，在多个数据集上取得最优结果。", 0),
            ("c1", 1, "实验部分使用公开数据集评测，评价指标为 mIoU。", 0),
            ("c2", 2, "The proposed method uses a transformer encoder for segmentation.", 0),
            ("c3", 3, "Kothari, R. U. Cincinnati prehospital stroke scale, 1999.", 1),
        ];
        for (id, index, content, tail) in rows {
            sqlx::query(
                "INSERT INTO chunks (id, paper_id, content, search_text, block_type, is_tail, chunk_index, created_at)
                 VALUES (?, 'p1', ?, ?, ?, ?, ?, 't')",
            )
            .bind(id)
            .bind(content)
            .bind(crate::ai::query::bigram_index_text(content))
            .bind(if tail == 1 { "reference" } else { "prose" })
            .bind(tail)
            .bind(index)
            .execute(&pool)
            .await
            .unwrap();
        }
        pool
    }

    #[tokio::test]
    async fn chinese_natural_question_hits_the_right_chunk() {
        let db = test_db().await;
        // The acceptance case: this returned ZERO hits before the bigram index.
        let hits = hybrid_search_with(&db, "这篇论文用了什么方法", 10, false)
            .await
            .unwrap();
        assert!(!hits.is_empty(), "中文自然语言问句必须命中");
        assert_eq!(hits[0].chunk_id, "c0", "应命中含「方法」的块，实际 {:?}",
            hits.iter().map(|h| h.chunk_id.as_str()).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn two_char_chinese_word_is_searchable() {
        let db = test_db().await;
        // 2 characters — impossible for the trigram index, and dropped entirely
        // by the old ">= 3 chars" filter.
        let hits = hybrid_search_with(&db, "方法", 10, false).await.unwrap();
        assert_eq!(hits.first().map(|h| h.chunk_id.as_str()), Some("c0"), "{:?}",
            hits.iter().map(|h| h.chunk_id.as_str()).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn english_still_searches_the_trigram_index() {
        let db = test_db().await;
        let hits = hybrid_search_with(&db, "transformer encoder", 10, false).await.unwrap();
        assert_eq!(hits.first().map(|h| h.chunk_id.as_str()), Some("c2"), "{:?}",
            hits.iter().map(|h| h.chunk_id.as_str()).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn references_tail_is_hidden_unless_requested() {
        let db = test_db().await;
        let hidden = hybrid_search_with(&db, "Cincinnati prehospital scale", 10, false).await.unwrap();
        assert!(hidden.is_empty(), "默认不返回参考文献块: {:?}",
            hidden.iter().map(|h| h.chunk_id.as_str()).collect::<Vec<_>>());
        let shown = hybrid_search_with(&db, "Cincinnati prehospital scale", 10, true).await.unwrap();
        assert_eq!(shown.first().map(|h| h.chunk_id.as_str()), Some("c3"));
    }

    #[tokio::test]
    async fn neighbours_are_appended_and_marked() {
        let db = test_db().await;
        let mut hits = hybrid_search_with(&db, "方法", 10, false).await.unwrap();
        expand_with_neighbors(&db, &mut hits, false).await.unwrap();
        let neighbour = hits.iter().find(|h| h.is_neighbor).expect("相邻块应被补齐");
        assert_eq!(neighbour.chunk_id, "c1");
        assert!(neighbour.score < hits[0].score);
    }

    #[tokio::test]
    async fn empty_query_returns_nothing() {
        let db = test_db().await;
        let hits = hybrid_search_with(&db, "   ", 10, false).await.unwrap();
        assert!(hits.is_empty());
    }
}
