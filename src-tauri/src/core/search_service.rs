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
    pub paper_title: String,
    pub score: f32,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeSearchResult {
    pub id: String,
    pub domain_id: String,
    pub domain_name: String,
    pub title: String,
    pub content: String,
    pub content_type: String,
    pub score: f32,
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
        paper_title: r.paper_title, score: r.score, source: r.source,
    }).collect())
}

/// Search knowledge items using FTS5 (with LIKE fallback)
#[instrument(skip(db))]
pub async fn search_knowledge(
    db: &SqlitePool,
    query: &str,
    domain_id: Option<&str>,
    limit: usize,
) -> Result<Vec<KnowledgeSearchResult>, String> {
    let pattern = format!("%{}%", query);
    let fts_query = query.split_whitespace().map(|w| format!("{}*", w)).collect::<Vec<_>>().join(" OR ");

    let rows: Vec<(String, String, String, String, String, String)> = if domain_id.is_some() {
        sqlx::query_as(
            "SELECT ki.id, ki.domain_id, kd.name, ki.title, COALESCE(ki.content,''), ki.content_type \
             FROM knowledge_items ki JOIN knowledge_domains kd ON ki.domain_id = kd.id \
             WHERE ki.domain_id = ? AND (ki.title LIKE ? OR ki.content LIKE ?) \
             ORDER BY ki.updated_at DESC LIMIT ?"
        ).bind(domain_id.unwrap()).bind(&pattern).bind(&pattern).bind(limit as i64)
         .fetch_all(db).await.map_err(|e| format!("db: {e}"))?
    } else {
        // Try FTS5 + LIKE
        sqlx::query_as(
            "SELECT ki.id, ki.domain_id, kd.name, ki.title, COALESCE(ki.content,''), ki.content_type \
             FROM knowledge_items ki JOIN knowledge_domains kd ON ki.domain_id = kd.id \
             LEFT JOIN knowledge_items_fts fts ON ki.rowid = fts.rowid \
             WHERE knowledge_items_fts MATCH ? OR ki.title LIKE ? OR ki.content LIKE ? \
             ORDER BY ki.updated_at DESC LIMIT ?"
        ).bind(&fts_query).bind(&pattern).bind(&pattern).bind(limit as i64)
         .fetch_all(db).await.map_err(|e| format!("db: {e}"))?
    };

    Ok(rows.into_iter().map(|(id, domain_id, domain_name, title, content, content_type)| {
        KnowledgeSearchResult { id, domain_id, domain_name, title, content, content_type, score: 0.7 }
    }).collect())
}


