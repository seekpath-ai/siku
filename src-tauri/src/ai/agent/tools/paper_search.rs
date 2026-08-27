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

/// Resolve a collection/tag name filter into the set of matching paper ids.
/// Names match case-insensitively; an unknown name yields an empty set
/// (which then short-circuits the search to "no results").
async fn paper_ids_for_filter(
    db: &sqlx::SqlitePool,
    collection: Option<&str>,
    tag: Option<&str>,
) -> Result<Vec<String>, String> {
    let mut sets: Vec<std::collections::HashSet<String>> = Vec::new();
    if let Some(name) = collection {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT pc.paper_id FROM paper_collections pc \
             JOIN collections c ON c.id = pc.collection_id \
             WHERE c.name = ? COLLATE NOCASE",
        )
        .bind(name)
        .fetch_all(db)
        .await
        .map_err(|e| format!("database error: {e}"))?;
        sets.push(rows.into_iter().map(|(id,)| id).collect());
    }
    if let Some(name) = tag {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT pt.paper_id FROM paper_tags pt \
             JOIN tags t ON t.id = pt.tag_id \
             WHERE t.name = ? COLLATE NOCASE",
        )
        .bind(name)
        .fetch_all(db)
        .await
        .map_err(|e| format!("database error: {e}"))?;
        sets.push(rows.into_iter().map(|(id,)| id).collect());
    }
    match sets.len() {
        0 => Ok(Vec::new()),
        1 => Ok(sets.pop().unwrap().into_iter().collect()),
        _ => {
            let (first, rest) = sets.split_first().unwrap();
            Ok(first
                .iter()
                .filter(|id| rest.iter().all(|s| s.contains(*id)))
                .cloned()
                .collect())
        }
    }
}

/// `AND p.id IN (...)` clause for the filtered id set (empty when no filter).
fn id_filter_clause(ids: &[String]) -> String {
    if ids.is_empty() {
        return String::new();
    }
    let list = ids
        .iter()
        .map(|id| format!("'{}'", id.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(", ");
    format!(" AND p.id IN ({list})")
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
        "Search papers in the user's library by title, author, keywords, or abstract content. \
         Omit the query to list the most recently imported papers. Optional collection/tag \
         filters (by name) narrow the scope. Returns the total count plus a page of results \
         with metadata; use offset to paginate."
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![
            ToolParameter {
                name: "query".to_string(),
                param_type: "string".to_string(),
                description: "Search query — matches title, authors, keywords, and abstract. Omit/empty to list recent papers ordered by import date.".to_string(),
                required: false,
            },
            ToolParameter {
                name: "collection".to_string(),
                param_type: "string".to_string(),
                description: "Optional collection name — only papers in this collection.".to_string(),
                required: false,
            },
            ToolParameter {
                name: "tag".to_string(),
                param_type: "string".to_string(),
                description: "Optional tag name — only papers carrying this tag.".to_string(),
                required: false,
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
        let query = args["query"].as_str().unwrap_or("").trim().to_string();
        let limit = args["limit"].as_u64().unwrap_or(5).min(50) as i64;
        let offset = args["offset"].as_u64().unwrap_or(0) as i64;
        let collection = args["collection"].as_str().map(str::trim).filter(|s| !s.is_empty());
        let tag = args["tag"].as_str().map(str::trim).filter(|s| !s.is_empty());

        // Resolve name filters to a paper-id set first so every query path
        // (FTS / LIKE / list) shares one `IN (...)` clause. Filters present
        // but matching nothing short-circuit to an empty result.
        let filter_active = collection.is_some() || tag.is_some();
        let filter_ids = paper_ids_for_filter(&self.db, collection, tag).await?;
        if filter_active && filter_ids.is_empty() {
            return Ok("没有匹配的论文（收藏夹/标签筛选无结果）。".to_string());
        }
        let id_clause = id_filter_clause(&filter_ids);

        let (papers, total): (Vec<crate::core::models::Paper>, i64) = if query.is_empty() {
            // List path: most recently imported first.
            let list_sql = format!(
                "SELECT p.* FROM papers p WHERE 1=1{id_clause} \
                 ORDER BY p.imported_at DESC LIMIT ? OFFSET ?"
            );
            let count_sql = format!("SELECT count(*) FROM papers p WHERE 1=1{id_clause}");
            let papers = sqlx::query_as::<_, crate::core::models::Paper>(&list_sql)
                .bind(limit)
                .bind(offset)
                .fetch_all(&self.db)
                .await
                .map_err(|e| format!("database error: {e}"))?;
            let total: (i64,) = sqlx::query_as(&count_sql)
                .fetch_one(&self.db)
                .await
                .map_err(|e| format!("database error: {e}"))?;
            (papers, total.0)
        } else {
            // FTS path, falling back to LIKE when FTS rejects the query.
            let fts_sql = format!(
                "SELECT p.* FROM papers_fts f JOIN papers p ON p.rowid = f.rowid \
                 WHERE papers_fts MATCH ?{id_clause} \
                 ORDER BY bm25(papers_fts) LIMIT ? OFFSET ?"
            );
            let fts_count_sql = format!(
                "SELECT count(*) FROM papers_fts f JOIN papers p ON p.rowid = f.rowid \
                 WHERE papers_fts MATCH ?{id_clause}"
            );
            match sqlx::query_as::<_, crate::core::models::Paper>(&fts_sql)
                .bind(fts_query(&query))
                .bind(limit)
                .bind(offset)
                .fetch_all(&self.db)
                .await
            {
                Ok(rows) => {
                    let total: (i64,) = sqlx::query_as(&fts_count_sql)
                        .bind(fts_query(&query))
                        .fetch_one(&self.db)
                        .await
                        .map_err(|e| format!("database error: {e}"))?;
                    (rows, total.0)
                }
                Err(_) => {
                    let pattern = format!("%{query}%");
                    let like_sql = format!(
                        "SELECT p.* FROM papers p \
                         WHERE (p.title LIKE ? OR p.authors LIKE ? OR p.keywords LIKE ? OR p.abstract LIKE ?){id_clause} \
                         ORDER BY p.imported_at DESC LIMIT ? OFFSET ?"
                    );
                    let like_count_sql = format!(
                        "SELECT count(*) FROM papers p \
                         WHERE (p.title LIKE ? OR p.authors LIKE ? OR p.keywords LIKE ? OR p.abstract LIKE ?){id_clause}"
                    );
                    let papers = sqlx::query_as::<_, crate::core::models::Paper>(&like_sql)
                        .bind(&pattern)
                        .bind(&pattern)
                        .bind(&pattern)
                        .bind(&pattern)
                        .bind(limit)
                        .bind(offset)
                        .fetch_all(&self.db)
                        .await
                        .map_err(|e| format!("database error: {e}"))?;
                    let total: (i64,) = sqlx::query_as(&like_count_sql)
                        .bind(&pattern)
                        .bind(&pattern)
                        .bind(&pattern)
                        .bind(&pattern)
                        .fetch_one(&self.db)
                        .await
                        .map_err(|e| format!("database error: {e}"))?;
                    (papers, total.0)
                }
            }
        };

        if papers.is_empty() {
            return Ok("没有找到匹配的论文。".to_string());
        }

        let from = offset + 1;
        let to = offset + papers.len() as i64;
        let mut out = vec![format!("共 {total} 篇，显示第 {from}–{to} 篇")];
        out.extend(papers.iter().map(|p| {
            format!(
                "- **{}** ({}) — {} — {}\n  id: {}",
                p.title,
                p.year.map(|y| y.to_string()).unwrap_or_else(|| "N/A".into()),
                super::format_author_list(&p.authors),
                p.abstract_text.as_deref().unwrap_or("无摘要"),
                p.id,
            )
        }));
        Ok(out.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::{tests::connect_with_crsqlite, SCHEMA_INIT_SQL};

    async fn test_db() -> sqlx::SqlitePool {
        let dir = std::env::temp_dir().join(format!(
            "siku-paper-search-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = connect_with_crsqlite(&dir.join("test.db")).await.unwrap();
        sqlx::query(SCHEMA_INIT_SQL).execute(&db).await.unwrap();

        for (id, title, imported_at) in [
            ("p1", "Attention Is All You Need", "2026-01-01T00:00:00Z"),
            ("p2", "BERT: Pre-training of Deep Bidirectional Transformers", "2026-01-02T00:00:00Z"),
            ("p3", "Language Models are Few-Shot Learners", "2026-01-03T00:00:00Z"),
        ] {
            sqlx::query(
                "INSERT INTO papers (id, title, created_at, updated_at, imported_at) \
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(id)
            .bind(title)
            .bind(imported_at)
            .bind(imported_at)
            .bind(imported_at)
            .execute(&db)
            .await
            .unwrap();
        }
        sqlx::query("INSERT INTO collections (id, name) VALUES ('c1', 'ML')")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO paper_collections (paper_id, collection_id) VALUES ('p1', 'c1')")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO tags (id, name) VALUES ('t1', 'nlp')")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO paper_tags (paper_id, tag_id) VALUES ('p2', 't1')")
            .execute(&db)
            .await
            .unwrap();
        db
    }

    #[tokio::test]
    async fn empty_query_lists_recent_first_with_total() {
        let db = test_db().await;
        let tool = PaperSearchTool::new(db.clone());
        let out = tool.execute(serde_json::json!({"limit": 2})).await.unwrap();
        assert!(
            out.starts_with("共 3 篇，显示第 1–2 篇"),
            "total + page info expected: {out}"
        );
        let gpt = out.find("Few-Shot").unwrap();
        let bert = out.find("BERT").unwrap();
        assert!(gpt < bert, "most recently imported must come first: {out}");
        assert!(!out.contains("Attention"), "limit 2 must cut off the oldest: {out}");
        sqlx::query("SELECT crsql_finalize()").execute(&db).await.unwrap();
    }

    #[tokio::test]
    async fn collection_and_tag_filters_narrow_results() {
        let db = test_db().await;
        let tool = PaperSearchTool::new(db.clone());

        let out = tool.execute(serde_json::json!({"collection": "ML"})).await.unwrap();
        assert!(out.contains("Attention") && !out.contains("BERT"), "collection filter: {out}");

        let out = tool.execute(serde_json::json!({"tag": "NLP"})).await.unwrap();
        assert!(out.contains("BERT") && !out.contains("Attention"), "tag filter (case-insensitive): {out}");

        // Filters combine with a text query.
        let out = tool
            .execute(serde_json::json!({"query": "BERT", "collection": "ML"}))
            .await
            .unwrap();
        assert!(out.contains("没有找到匹配") || out.contains("没有匹配"), "query+filter mismatch: {out}");

        // Unknown collection short-circuits.
        let out = tool.execute(serde_json::json!({"collection": "不存在"})).await.unwrap();
        assert!(out.contains("没有匹配"), "unknown collection: {out}");

        sqlx::query("SELECT crsql_finalize()").execute(&db).await.unwrap();
    }
}
