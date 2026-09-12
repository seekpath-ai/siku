//! Retrieval evaluation harness: a golden set of "question → page" pairs run
//! through the *real* pipeline (PDF → extraction → chunking → FTS index →
//! hybrid retrieval), reporting recall@k, MRR and nDCG@k.
//!
//! Why: extraction has `pdf::corpus_regression`, retrieval had nothing — every
//! ranking change so far could only be judged by eye.
//!
//! Run it (skips with a notice when the inputs are absent):
//!
//! ```text
//! SIKU_EVAL_CORPUS=/home/me/pdfs SIKU_EVAL_GOLDEN=/home/me/golden.json \
//!   cargo test --lib ai::retrieval_eval -- --nocapture --test-threads=1
//! ```
//!
//! Golden set format (JSON array):
//! ```json
//! [
//!   { "query": "stage-local recovery", "paper": "demo2", "pages": [6, 7] },
//!   { "query": "这篇论文用了什么方法", "paper": "zh-paper", "pages": [2],
//!     "cross_lingual": true }
//! ]
//! ```
//! * `paper` matches the PDF file stem or the indexed paper title (substring).
//! * `pages` is the set of pages that count as a correct answer.
//! * `cross_lingual` marks a query whose language differs from the document's;
//!   keyword search cannot serve those (only embeddings can), so they are
//!   reported separately instead of dragging the headline numbers down.

use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct GoldenItem {
    query: String,
    paper: String,
    pages: Vec<i32>,
    #[serde(default)]
    cross_lingual: bool,
}

/// Lowercased words of length >= 2, deduplicated.
fn unique_words(text: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '-')
        .filter(|w| w.chars().count() >= 2)
        .map(|w| w.to_string())
        .filter(|w| seen.insert(w.clone()))
        .collect()
}

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var(key).ok().map(PathBuf::from).filter(|p| p.exists())
}

/// Build an in-memory database from the PDFs in `dir` using the production
/// pipeline: `extract_text` → `chunk_pages` → the same chunk rows the app writes.
async fn build_db(corpus: &Path) -> Result<sqlx::SqlitePool, String> {
    use crate::pdf::chunker::{chunk_pages, ChunkConfig};
    use sqlx::sqlite::SqlitePoolOptions;

    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .map_err(|e| e.to_string())?;
    sqlx::raw_sql(include_str!("../../schema_init.sql"))
        .execute(&pool)
        .await
        .map_err(|e| format!("schema: {e}"))?;

    for entry in std::fs::read_dir(corpus).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("pdf") {
            continue;
        }
        let stem = path.file_stem().unwrap().to_string_lossy().to_string();
        let pages = crate::pdf::extractor::extract_text(&path).map_err(|e| e.to_string())?;
        if pages.is_empty() {
            continue;
        }
        let paper_id = format!("paper-{stem}");
        sqlx::query(
            "INSERT INTO papers (id, title, created_at, updated_at) VALUES (?, ?, 't', 't')",
        )
        .bind(&paper_id)
        .bind(&stem)
        .execute(&pool)
        .await
        .map_err(|e| e.to_string())?;

        for chunk in chunk_pages(&pages, &ChunkConfig::default()) {
            sqlx::query(
                "INSERT INTO chunks (id, paper_id, content, search_text, page_start, page_end,
                                     section, section_path, block_type, is_tail, chunk_index,
                                     token_count, created_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 't')",
            )
            .bind(format!("{paper_id}-c{}", chunk.chunk_index))
            .bind(&paper_id)
            .bind(&chunk.content)
            .bind(crate::ai::query::bigram_index_text(&chunk.content))
            .bind(chunk.page_start)
            .bind(chunk.page_end)
            .bind(&chunk.section)
            .bind(&chunk.section_path)
            .bind(chunk.block_type.as_str())
            .bind(if chunk.is_tail { 1 } else { 0 })
            .bind(chunk.chunk_index)
            .bind(chunk.token_count)
            .execute(&pool)
            .await
            .map_err(|e| e.to_string())?;
        }
    }
    Ok(pool)
}

/// A hit is correct when it comes from the expected paper and its page range
/// overlaps the expected pages.
fn is_relevant(hit: &crate::ai::retriever::SearchResult, item: &GoldenItem) -> bool {
    let paper_ok = hit.paper_id.contains(&item.paper) || hit.paper_title.contains(&item.paper);
    if !paper_ok {
        return false;
    }
    let (Some(start), Some(end)) = (hit.page_start, hit.page_end) else { return false };
    item.pages.iter().any(|p| *p >= start && *p <= end)
}

fn dcg(gains: &[f64]) -> f64 {
    gains
        .iter()
        .enumerate()
        .map(|(i, g)| g / ((i as f64 + 2.0).log2()))
        .sum()
}

struct Metrics {
    recall: f64,
    mrr: f64,
    ndcg: f64,
    misses: Vec<String>,
}

fn evaluate(hits_per_query: &[Vec<crate::ai::retriever::SearchResult>], items: &[GoldenItem]) -> Metrics {
    let (mut hit_count, mut rr_sum, mut ndcg_sum) = (0usize, 0.0f64, 0.0f64);
    let mut misses = Vec::new();
    for (hits, item) in hits_per_query.iter().zip(items) {
        let gains: Vec<f64> = hits.iter().map(|h| if is_relevant(h, item) { 1.0 } else { 0.0 }).collect();
        if gains.iter().any(|g| *g > 0.0) {
            hit_count += 1;
        } else {
            misses.push(format!("{} [{}]", item.query, item.paper));
        }
        let first = gains.iter().position(|g| *g > 0.0);
        rr_sum += first.map(|i| 1.0 / (i as f64 + 1.0)).unwrap_or(0.0);
        // Ideal ranking = the relevant chunks that were actually retrieved, at
        // the top (the collection-wide relevant count is not knowable here, and
        // using `item.pages.len()` would let nDCG exceed 1 whenever one page
        // spans several chunks).
        let relevant = gains.iter().filter(|g| **g > 0.0).count().max(1);
        let ideal = dcg(&vec![1.0; relevant.min(hits.len()).max(1)]);
        ndcg_sum += if ideal > 0.0 { (dcg(&gains) / ideal).min(1.0) } else { 0.0 };
    }
    let n = items.len().max(1) as f64;
    Metrics {
        recall: hit_count as f64 / n,
        mrr: rr_sum / n,
        ndcg: ndcg_sum / n,
        misses,
    }
}

#[tokio::test]
async fn retrieval_golden_set() {
    let Some(corpus) = env_path("SIKU_EVAL_CORPUS") else {
        println!(
            "[retrieval_eval] SKIPPED: set SIKU_EVAL_CORPUS (a directory of PDFs) and \
             SIKU_EVAL_GOLDEN (a JSON golden set) to run this harness"
        );
        return;
    };

    // Bootstrap mode: propose candidate golden items from the indexed text, so
    // every query word is guaranteed to exist in the index (curate afterwards).
    // NB: the output path does not exist yet, so it must not go through
    // `env_path` (which only returns paths that are already there).
    if let Ok(out) = std::env::var("SIKU_EVAL_GENERATE") {
        let out = PathBuf::from(out);
        let db = build_db(&corpus).await.expect("build index");
        let rows: Vec<(String, String, i32, i32, String)> = sqlx::query_as(
            "SELECT paper_id, content, chunk_index, COALESCE(page_start, 0), block_type
             FROM chunks WHERE is_tail = 0 ORDER BY paper_id, chunk_index",
        )
        .fetch_all(&db)
        .await
        .unwrap();
        let mut doc_freq: std::collections::HashMap<String, usize> = Default::default();
        for (_, content, _, _, _) in &rows {
            for w in unique_words(content) {
                *doc_freq.entry(w).or_default() += 1;
            }
        }
        let mut candidates = Vec::new();
        for (paper, content, _, page, _) in &rows {
            if *page <= 1 {
                continue; // first pages are title/abstract noise
            }
            let mut words: Vec<String> = unique_words(content)
                .into_iter()
                .filter(|w| doc_freq.get(w).copied().unwrap_or(0) <= 2 && w.len() >= 6)
                .collect();
            if words.len() < 3 {
                continue;
            }
            words.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
            candidates.push(serde_json::json!({
                "query": words[..3].join(" "),
                "paper": paper,
                "pages": [page],
            }));
        }
        std::fs::write(&out, serde_json::to_string_pretty(&candidates).unwrap()).unwrap();
        println!("wrote {} candidate items to {}", candidates.len(), out.display());
        return;
    }

    let Some(golden) = env_path("SIKU_EVAL_GOLDEN") else {
        println!("[retrieval_eval] SKIPPED: SIKU_EVAL_GOLDEN is not set");
        return;
    };
    let items: Vec<GoldenItem> =
        serde_json::from_str(&std::fs::read_to_string(&golden).expect("golden set"))
            .expect("golden set JSON");
    let db = build_db(&corpus).await.expect("build index");
    let chunks: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM chunks")
        .fetch_one(&db)
        .await
        .unwrap();
    println!("index: {} chunks from {}", chunks.0, corpus.display());

    let k = 10usize;
    let mut same_lang: Vec<Vec<crate::ai::retriever::SearchResult>> = Vec::new();
    let mut same_items: Vec<&GoldenItem> = Vec::new();
    let mut cross: Vec<Vec<crate::ai::retriever::SearchResult>> = Vec::new();
    let mut cross_items: Vec<&GoldenItem> = Vec::new();

    for item in items.iter() {
        let hits = crate::ai::retriever::hybrid_search_with(&db, &item.query, k, false)
            .await
            .expect("search");
        if item.cross_lingual {
            cross.push(hits);
            cross_items.push(item);
        } else {
            same_lang.push(hits);
            same_items.push(item);
        }
    }

    let owned: Vec<GoldenItem> = same_items
        .iter()
        .map(|i| GoldenItem { query: i.query.clone(), paper: i.paper.clone(), pages: i.pages.clone(), cross_lingual: false })
        .collect();
    let m = evaluate(&same_lang, &owned);
    println!(
        "\n同语种 {} 条 → recall@{k}={:.2} MRR={:.2} nDCG@{k}={:.2}",
        owned.len(),
        m.recall,
        m.mrr,
        m.ndcg
    );
    if !m.misses.is_empty() {
        println!("未命中 {} 条:", m.misses.len());
        for miss in m.misses.iter().take(20) {
            println!("   {miss}");
        }
    }

    if !cross_items.is_empty() {
        let owned_cross: Vec<GoldenItem> = cross_items
            .iter()
            .map(|i| GoldenItem { query: i.query.clone(), paper: i.paper.clone(), pages: i.pages.clone(), cross_lingual: true })
            .collect();
        let mc = evaluate(&cross, &owned_cross);
        println!(
            "\n跨语种 {} 条（关键词检索无法服务，需 embeddings）→ recall@{k}={:.2} MRR={:.2}",
            owned_cross.len(),
            mc.recall,
            mc.mrr
        );
    }

    assert!(!owned.is_empty(), "golden set has no same-language items");
    assert!(
        m.recall >= 0.6,
        "recall@{} dropped to {:.2} — ranking regression?\nmisses: {:?}",
        k,
        m.recall,
        m.misses
    );
}
