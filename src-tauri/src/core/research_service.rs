use sqlx::SqlitePool;
use tauri::Emitter;
use tracing::{info, instrument, warn};

use crate::core::models::ResearchTopic;
use crate::core::time::now_iso;

/// A newly inserted source queued for LLM relevance scoring.
struct ScoringCandidate {
    id: String,
    title: String,
    abstract_text: Option<String>,
}

/// Discover new sources for a topic from arXiv + Crossref + Semantic Scholar
/// and persist them. Returns the number of NEW sources added. Existing
/// sources (matched by arXiv id or DOI, across ALL topics) are skipped —
/// global dedup. New sources then get a batch LLM relevance score
/// (best-effort: any failure leaves scores NULL and is surfaced via a
/// `scoring_failed` progress event).
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
    let mut candidates: Vec<ScoringCandidate> = Vec::new();

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
                candidates.push(ScoringCandidate {
                    id: id.clone(),
                    title: paper.title.clone(),
                    abstract_text: Some(paper.abstract_text.clone()),
                });
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
                candidates.push(ScoringCandidate {
                    id: id.clone(),
                    title: title.clone(),
                    abstract_text: w.abstract_text.clone(),
                });
                emit_source(app, topic_id, id, "crossref", doi.clone(), title, authors_json, url, Some(doi), metadata, now.clone());
            }
        }
        Err(e) => warn!(topic_id, error = %e, "crossref search failed"),
    }

    // 3. Semantic Scholar (free graph API; one request per scan).
    emit("scholar", added);
    match crate::ai::scraping::semanticscholar::search(&query, limit, proxy.as_deref()).await {
        Ok(papers) => {
            for p in papers {
                let arxiv_id = p.arxiv_id.clone().unwrap_or_default();
                if source_exists(db, &arxiv_id, p.doi.as_deref()).await? {
                    continue;
                }
                let id = uuid::Uuid::new_v4().to_string();
                let authors_json = serde_json::to_string(&p.authors).unwrap_or_default();
                let metadata = serde_json::json!({
                    "s2_id": p.s2_id,
                    "arxiv_id": p.arxiv_id,
                    "year": p.year,
                    "citation_count": p.citation_count,
                    "pdf_url": p.pdf_url,
                })
                .to_string();
                // The import pipeline needs a real PDF: prefer the
                // open-access PDF, fall back to the landing page (import will
                // report a clear "not a PDF" error for paywalled papers).
                let url = p.pdf_url.clone().unwrap_or_else(|| p.url.clone());
                sqlx::query(
                    "INSERT INTO research_sources (id, topic_id, source_type, source_id, title, authors, url, doi, status, metadata, discovered_at)
                     VALUES (?, ?, 'scholar', ?, ?, ?, ?, ?, 'discovered', ?, ?)"
                )
                .bind(&id).bind(topic_id).bind(&p.s2_id).bind(&p.title)
                .bind(&authors_json).bind(&url).bind(&p.doi)
                .bind(&metadata).bind(&now)
                .execute(db).await.map_err(|e| format!("db: {e}"))?;
                added += 1;
                candidates.push(ScoringCandidate {
                    id: id.clone(),
                    title: p.title.clone(),
                    abstract_text: p.abstract_text.clone(),
                });
                emit_source(app, topic_id, id, "scholar", p.s2_id.clone(), p.title, authors_json, url, p.doi, metadata, now.clone());
            }
        }
        Err(e) => warn!(topic_id, error = %e, "semantic scholar search failed"),
    }

    // 4. LLM relevance scoring over just-inserted sources (best-effort).
    if !candidates.is_empty() {
        emit("scoring", added);
        if let Err(e) = score_candidates(db, &topic.name, &keywords, &candidates).await {
            warn!(topic_id, error = %e, "relevance scoring failed; sources left unscored");
            if let Some(a) = app {
                let _ = a.emit(
                    "research:discover_progress",
                    serde_json::json!({
                        "topic_id": topic_id,
                        "phase": "scoring_failed",
                        "found": added,
                        "error": e,
                    }),
                );
            }
        }
    }

    emit("done", added);
    info!(topic_id, added, "research discovery completed");
    Ok(added)
}

/// Batch-score new sources against the topic with the configured LLM.
/// One completion per scan (not per paper); an LLM failure is returned so the
/// caller can surface it, while per-row store failures are logged and skipped
/// — scores simply stay NULL and the UI shows the sources unscored.
async fn score_candidates(
    db: &SqlitePool,
    topic_name: &str,
    keywords: &[String],
    candidates: &[ScoringCandidate],
) -> Result<(), String> {
    let scored = score_candidates_llm(db, topic_name, keywords, candidates).await?;
    for (id, score, reason) in scored {
        if let Err(e) = sqlx::query(
            "UPDATE research_sources SET relevance_score = ?, relevance_reason = ? WHERE id = ?",
        )
        .bind(score)
        .bind(&reason)
        .bind(&id)
        .execute(db)
        .await
        {
            warn!(error = %e, "failed to store relevance score");
        }
    }
    Ok(())
}

/// The raw LLM call behind score_candidates. Returns (source_id, score, reason).
async fn score_candidates_llm(
    db: &SqlitePool,
    topic_name: &str,
    keywords: &[String],
    candidates: &[ScoringCandidate],
) -> Result<Vec<(String, i64, String)>, String> {
    let mut llm_config = crate::core::settings_service::load_llm_config(db).await?;
    if llm_config.api_key.is_empty() && llm_config.provider != crate::ai::llm::LlmProvider::Ollama {
        return Err("no LLM configured".into());
    }
    // 设置 → 领域智能体 的输出预算（只提高不降低）。
    let need = crate::core::task_prompts::apply_floor(
        llm_config.max_tokens,
        crate::core::task_prompts::budget_floor(db, "research").await,
    );
    llm_config.max_tokens = need;
    let client = crate::ai::llm::client::create_llm_client(&llm_config)
        .map_err(|e| format!("client: {e}"))?;

    let kw = if keywords.is_empty() { topic_name.to_string() } else { keywords.join("、") };
    let mut list = String::new();
    for (i, c) in candidates.iter().enumerate() {
        let abstract_preview: String = c
            .abstract_text
            .as_deref()
            .unwrap_or("")
            .chars()
            .take(300)
            .collect();
        list.push_str(&format!("{}. {}\n   摘要：{}\n", i + 1, c.title, abstract_preview));
    }
    let prompt = format!(
        "课题：{topic_name}\n关键词：{kw}\n\n候选文献：\n{list}\n\
         请逐条评估每篇文献与该课题的相关性，严格输出 JSON 数组，不要输出任何其它内容：\n\
         [{{\"i\":1,\"score\":7,\"reason\":\"一句话理由\"}}]\n\
         score 为 0-10 整数：9-10 直接命中课题核心；6-8 明显相关；3-5 边缘相关；0-2 基本无关。"
    );
    let messages = vec![
        crate::ai::llm::ChatMessage {
            reasoning_content: None,
            role: "system".into(),
            content: crate::core::task_prompts::effective(db, crate::core::task_prompts::RESEARCH_RELEVANCE).await,
            attachments: None, tool_calls: None, tool_call_id: None, name: None,
        },
        crate::ai::llm::ChatMessage {
            reasoning_content: None,
            role: "user".into(),
            content: prompt,
            attachments: None, tool_calls: None, tool_call_id: None, name: None,
        },
    ];
    let resp = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        client.chat_completion(&messages, &[]),
    )
    .await
    .map_err(|_| "scoring timeout".to_string())?
    .map_err(|e| format!("scoring: {e}"))?;

    // Tolerate prose around the JSON: take the outermost [ … ] span.
    let text = resp.content;
    let start = text.find('[').ok_or("no JSON array in scoring output")?;
    let end = text.rfind(']').ok_or("no JSON array in scoring output")?;
    #[derive(serde::Deserialize)]
    struct ScoreRow {
        i: usize,
        score: i64,
        #[serde(default)]
        reason: String,
    }
    let rows: Vec<ScoreRow> =
        serde_json::from_str(&text[start..=end]).map_err(|e| format!("parse: {e}"))?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            let c = candidates.get(r.i.checked_sub(1)?)?;
            Some((c.id.clone(), r.score.clamp(0, 10), r.reason))
        })
        .collect())
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
                    "relevance_score": serde_json::Value::Null,
                    "relevance_reason": serde_json::Value::Null,
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
///
/// When a scan adds sources, emits `research:auto_discovered`
/// { topic_id, topic_name, added } so the UI can toast + bump unread badges.
pub async fn run_auto_discovery(
    db: SqlitePool,
    app: tauri::AppHandle,
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
        let topics: Vec<(String, String)> = match sqlx::query_as(
            "SELECT id, name FROM research_topics WHERE status = 'active'"
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
        for (id, name) in &topics {
            match discover_for_topic(&db, Some(&app), id, None).await {
                Ok(added) if added > 0 => {
                    let _ = app.emit(
                        "research:auto_discovered",
                        serde_json::json!({ "topic_id": id, "topic_name": name, "added": added }),
                    );
                }
                Ok(_) => {}
                Err(e) => warn!(topic_id = %id, error = %e, "auto-discovery failed"),
            }
        }
    }
}

/// Best-effort LLM keyword expansion. Any failure returns an empty string so
/// the caller falls back to the raw topic keywords.
async fn expand_query(db: &SqlitePool, topic_name: &str, keywords: &[String]) -> Result<String, String> {
    let mut llm_config = crate::core::settings_service::load_llm_config(db).await?;
    if llm_config.api_key.is_empty() && llm_config.provider != crate::ai::llm::LlmProvider::Ollama {
        return Ok(String::new());
    }
    let need = crate::core::task_prompts::apply_floor(
        llm_config.max_tokens,
        crate::core::task_prompts::budget_floor(db, "research").await,
    );
    llm_config.max_tokens = need;
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
        crate::ai::llm::ChatMessage { reasoning_content: None,
            role: "system".into(),
            content: crate::core::task_prompts::effective(db, crate::core::task_prompts::RESEARCH_KEYWORDS).await,
            attachments: None, tool_calls: None, tool_call_id: None, name: None,
        },
        crate::ai::llm::ChatMessage { reasoning_content: None,
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
