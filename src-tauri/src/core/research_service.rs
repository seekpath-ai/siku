use sqlx::SqlitePool;
use tauri::Emitter;
use tracing::{info, instrument, warn};

use crate::core::models::ResearchTopic;
use crate::core::time::now_iso;

/// Discover new sources for a topic from arXiv + Crossref and persist them.
/// Returns the number of NEW sources added. Existing sources (matched by
/// arXiv id or DOI, across ALL topics) are skipped — global dedup.
///
/// When `app` is provided, progress and per-source events are emitted for
/// incremental display:
///   research:discover_progress { topic_id, phase, found }
///   research:discovered        { topic_id, source }
#[instrument(skip(db, app))]
pub async fn discover_for_topic(
    db: &SqlitePool,
    app: Option<&tauri::AppHandle>,
    topic_id: &str,
    max_results: Option<u32>,
) -> Result<usize, String> {
    let emit = |phase: &str, found: usize| {
        if let Some(a) = app {
            let _ = a.emit(
                "research:discover_progress",
                serde_json::json!({ "topic_id": topic_id, "phase": phase, "found": found }),
            );
        }
    };
    let topic = sqlx::query_as::<_, ResearchTopic>(
        "SELECT * FROM research_topics WHERE id = ?"
    )
    .bind(topic_id)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("db: {e}"))?
    .ok_or_else(|| "topic not found".to_string())?;

    let keywords: Vec<String> = serde_json::from_str(&topic.keywords).unwrap_or_default();
    let mut query = if keywords.is_empty() {
        topic.name.clone()
    } else {
        keywords.join(" ")
    };

    // Best-effort LLM keyword expansion; falls back to the raw query on any error.
    if let Ok(expanded) = expand_query(db, &topic.name, &keywords).await {
        if !expanded.trim().is_empty() {
            query = expanded;
        }
    }

    let proxy = crate::core::settings_service::get_setting(db, "llm.proxy").await.ok().flatten();
    // Per-scan limit: explicit arg wins, otherwise the user setting (default 10).
    let setting_limit = crate::core::settings_service::cached_settings()
        .research_discover_max_results
        .max(1) as u32;
    let limit = max_results.unwrap_or(setting_limit).min(30);

    let mut added = 0usize;
    let now = now_iso();

    // 1. arXiv
    emit("arxiv", 0);
    match crate::ai::scraping::arxiv::search(&query, limit, proxy.as_deref()).await {
        Ok(papers) => {
            for paper in papers {
                if source_exists(db, &paper.arxiv_id, paper.doi.as_deref()).await? {
                    continue;
                }
                let id = uuid::Uuid::new_v4().to_string();
                let authors_json = serde_json::to_string(&paper.authors).unwrap_or_default();
                let metadata = serde_json::json!({
                    "arxiv_id": paper.arxiv_id,
                    "categories": paper.categories,
                    "pdf_url": paper.pdf_url,
                    "published": paper.published,
                })
                .to_string();
                sqlx::query(
                    "INSERT INTO research_sources (id, topic_id, source_type, source_id, title, authors, url, doi, status, metadata, discovered_at)
                     VALUES (?, ?, 'arxiv', ?, ?, ?, ?, ?, 'discovered', ?, ?)"
                )
                .bind(&id).bind(topic_id).bind(&paper.arxiv_id).bind(&paper.title)
                .bind(&authors_json).bind(&paper.pdf_url).bind(&paper.doi)
                .bind(&metadata).bind(&now)
                .execute(db).await.map_err(|e| format!("db: {e}"))?;
                added += 1;
                emit_source(app, topic_id, id, "arxiv", paper.arxiv_id.clone(), paper.title, authors_json, paper.pdf_url, paper.doi, metadata, now.clone());
            }
        }
        Err(e) => warn!(topic_id, error = %e, "arxiv search failed"),
    }

    // 2. Crossref
    emit("crossref", added);
    match crate::ai::scraping::metadata::search_works(&query, limit, proxy.as_deref()).await {
        Ok(works) => {
            for w in works {
                let doi = w.doi.clone().unwrap_or_default();
                if doi.is_empty() { continue; }
                if source_exists(db, "", Some(&doi)).await? {
                    continue;
                }
                let id = uuid::Uuid::new_v4().to_string();
                let authors_json = serde_json::to_string(&w.authors.unwrap_or_default()).unwrap_or_default();
                let metadata = serde_json::json!({
                    "journal": w.journal,
                    "citation_count": w.citation_count,
                    "year": w.year,
                })
                .to_string();
                let title = w.title.unwrap_or_else(|| "未命名文献".into());
                let url = format!("https://doi.org/{doi}");
                sqlx::query(
                    "INSERT INTO research_sources (id, topic_id, source_type, source_id, title, authors, url, doi, status, metadata, discovered_at)
                     VALUES (?, ?, 'crossref', ?, ?, ?, ?, ?, 'discovered', ?, ?)"
                )
                .bind(&id).bind(topic_id).bind(&doi).bind(&title)
                .bind(&authors_json).bind(&url).bind(&doi)
                .bind(&metadata).bind(&now)
                .execute(db).await.map_err(|e| format!("db: {e}"))?;
                added += 1;
                emit_source(app, topic_id, id, "crossref", doi.clone(), title, authors_json, url, Some(doi), metadata, now.clone());
            }
        }
        Err(e) => warn!(topic_id, error = %e, "crossref search failed"),
    }

    emit("done", added);
    info!(topic_id, added, "research discovery completed");
    Ok(added)
}

/// Emit a single discovered source to the frontend for incremental display.
fn emit_source(
    app: Option<&tauri::AppHandle>,
    topic_id: &str,
    id: String,
    source_type: &str,
    source_id: String,
    title: String,
    authors: String,
    url: String,
    doi: Option<String>,
    metadata: String,
    discovered_at: String,
) {
    if let Some(a) = app {
        let _ = a.emit(
            "research:discovered",
            serde_json::json!({
                "topic_id": topic_id,
                "source": {
                    "id": id,
                    "topic_id": topic_id,
                    "source_type": source_type,
                    "source_id": source_id,
                    "title": title,
                    "authors": authors,
                    "url": url,
                    "doi": doi,
                    "status": "discovered",
                    "metadata": metadata,
                    "discovered_at": discovered_at,
                    "processed_at": serde_json::Value::Null,
                }
            }),
        );
    }
}

/// Global dedup: a source already present in ANY topic (by arXiv id or DOI)
/// is skipped.
async fn source_exists(db: &SqlitePool, arxiv_id: &str, doi: Option<&str>) -> Result<bool, String> {
    let id_exists: Option<(String,)> = if arxiv_id.is_empty() {
        None
    } else {
        sqlx::query_as("SELECT id FROM research_sources WHERE source_id = ? LIMIT 1")
            .bind(arxiv_id)
            .fetch_optional(db)
            .await
            .map_err(|e| format!("db: {e}"))?
    };
    if id_exists.is_some() {
        return Ok(true);
    }
    if let Some(d) = doi.filter(|d| !d.is_empty()) {
        let doi_exists: Option<(String,)> = sqlx::query_as(
            "SELECT id FROM research_sources WHERE doi = ? AND doi != '' LIMIT 1"
        )
        .bind(d)
        .fetch_optional(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
        if doi_exists.is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Background auto-discovery: periodically scan all ACTIVE topics for new
/// sources. The interval and per-scan limit come from app settings
/// (`research_auto_discover_interval_hours`, `research_discover_max_results`)
/// and are re-read every cycle, so changes take effect on the next scan
/// without restarting. Interval 0 disables auto-discovery.
pub async fn run_auto_discovery(
    db: SqlitePool,
    mut shutdown: tokio::sync::broadcast::Receiver<()>,
) {
    loop {
        let interval_hours = crate::core::settings_service::cached_settings()
            .research_auto_discover_interval_hours
            .max(0) as u64;
        if interval_hours == 0 {
            // Disabled: sleep briefly and re-check so a later enable applies.
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(300)) => {}
                _ = shutdown.recv() => {
                    tracing::info!("research auto-discovery received shutdown signal");
                    break;
                }
            }
            continue;
        }
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(interval_hours * 3600)) => {}
            _ = shutdown.recv() => {
                tracing::info!("research auto-discovery received shutdown signal");
                break;
            }
        }
        let topics: Vec<String> = match sqlx::query_scalar(
            "SELECT id FROM research_topics WHERE status = 'active'"
        )
        .fetch_all(&db)
        .await
        {
            Ok(t) => t,
            Err(e) => {
                warn!(error = %e, "auto-discovery db error");
                continue;
            }
        };
        for id in &topics {
            if let Err(e) = discover_for_topic(&db, None, id, None).await {
                warn!(topic_id = %id, error = %e, "auto-discovery failed");
            }
        }
    }
}

/// Best-effort LLM keyword expansion. Any failure returns an empty string so
/// the caller falls back to the raw topic keywords.
async fn expand_query(db: &SqlitePool, topic_name: &str, keywords: &[String]) -> Result<String, String> {
    let llm_config = crate::core::settings_service::load_llm_config(db).await?;
    if llm_config.api_key.is_empty() && llm_config.provider != crate::ai::llm::LlmProvider::Ollama {
        return Ok(String::new());
    }
    let client = match crate::ai::llm::client::create_llm_client(&llm_config) {
        Ok(c) => c,
        Err(_) => return Ok(String::new()),
    };
    let kw = if keywords.is_empty() {
        topic_name.to_string()
    } else {
        keywords.join("、")
    };
    let prompt = format!(
        "课题：{topic_name}\n关键词：{kw}\n\
         请给出 3~5 个用于文献检索的英文关键词/短语，用逗号分隔，不要编号，不要解释。"
    );
    let messages = vec![
        crate::ai::llm::ChatMessage {
            role: "system".into(),
            content: "你是文献检索助手，只输出检索关键词。".into(),
            attachments: None, tool_calls: None, tool_call_id: None, name: None,
        },
        crate::ai::llm::ChatMessage {
            role: "user".into(),
            content: prompt,
            attachments: None, tool_calls: None, tool_call_id: None, name: None,
        },
    ];
    let resp = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        client.chat_completion(&messages, &[]),
    )
    .await
    .map_err(|_| "keyword expansion timeout".to_string())?
    .map_err(|e| format!("keyword expansion: {e}"))?;
    Ok(resp.content.trim().to_string())
}
