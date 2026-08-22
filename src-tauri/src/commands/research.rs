use tauri::State;
use tracing::instrument;

use crate::AppState;
use crate::core::models::{ResearchTopic, ResearchSource};
use crate::core::time;

/// Create a research topic
#[tauri::command]
#[instrument(skip(state))]
pub async fn research_create_topic(
    state: State<'_, AppState>,
    name: String,
    description: Option<String>,
    keywords: Vec<String>,
) -> Result<ResearchTopic, String> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = time::now_iso();
    let keywords_json = serde_json::to_string(&keywords).map_err(|e| format!("json: {e}"))?;

    sqlx::query(
        "INSERT INTO research_topics (id, name, description, keywords, status, created_at, updated_at)
         VALUES (?, ?, ?, ?, 'active', ?, ?)"
    )
    .bind(&id).bind(&name).bind(&description).bind(&keywords_json).bind(&now).bind(&now)
    .execute(&state.db).await.map_err(|e| format!("db error: {e}"))?;

    let topic = sqlx::query_as::<_, ResearchTopic>(
        "SELECT * FROM research_topics WHERE id = ?"
    ).bind(&id).fetch_one(&state.db).await.map_err(|e| format!("db: {e}"))?;

    Ok(topic)
}

/// List all research topics
#[tauri::command]
#[instrument(skip(state))]
pub async fn research_list_topics(
    state: State<'_, AppState>,
) -> Result<Vec<ResearchTopic>, String> {
    sqlx::query_as::<_, ResearchTopic>(
        "SELECT * FROM research_topics ORDER BY updated_at DESC"
    )
    .fetch_all(&state.db).await.map_err(|e| format!("db: {e}"))
}

/// Update topic status
#[tauri::command]
#[instrument(skip(state))]
pub async fn research_update_topic(
    state: State<'_, AppState>,
    topic_id: String,
    status: Option<String>,
    description: Option<String>,
) -> Result<(), String> {
    let now = time::now_iso();
    if let Some(s) = status {
        sqlx::query("UPDATE research_topics SET status = ?, updated_at = ? WHERE id = ?")
            .bind(&s).bind(&now).bind(&topic_id)
            .execute(&state.db).await.map_err(|e| format!("db: {e}"))?;
    }
    if let Some(d) = description {
        sqlx::query("UPDATE research_topics SET description = ?, updated_at = ? WHERE id = ?")
            .bind(&d).bind(&now).bind(&topic_id)
            .execute(&state.db).await.map_err(|e| format!("db: {e}"))?;
    }
    Ok(())
}

/// Discover papers for a topic from arXiv + Crossref (LLM keyword expansion,
/// global dedup). Progress and per-source events are streamed to the frontend
/// during discovery; the final return is the full source list.
#[tauri::command]
#[instrument(skip(state, app_handle))]
pub async fn research_discover_sources(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
    topic_id: String,
    max_results: Option<u32>,
) -> Result<Vec<ResearchSource>, String> {
    crate::core::research_service::discover_for_topic(&state.db, Some(&app_handle), &topic_id, max_results).await?;
    research_list_sources(state, topic_id, None, None, None).await
}

/// Update a research source's status (e.g. mark as read).
#[tauri::command]
#[instrument(skip(state))]
pub async fn research_update_source_status(
    state: State<'_, AppState>,
    source_id: String,
    status: String,
) -> Result<(), String> {
    let now = time::now_iso();
    sqlx::query("UPDATE research_sources SET status = ?, processed_at = ? WHERE id = ?")
        .bind(&status)
        .bind(&now)
        .bind(&source_id)
        .execute(&state.db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    Ok(())
}

/// List sources for a topic, newest first, with pagination
/// (default page size 50).
#[tauri::command]
#[instrument(skip(state))]
pub async fn research_list_sources(
    state: State<'_, AppState>,
    topic_id: String,
    status: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<ResearchSource>, String> {
    let limit = limit.unwrap_or(50).clamp(1, 200);
    let offset = offset.unwrap_or(0).max(0);

    let mut q = String::from("SELECT * FROM research_sources WHERE topic_id = ?");
    if let Some(ref s) = status {
        q.push_str(" AND status = ?");
    }
    q.push_str(" ORDER BY discovered_at DESC LIMIT ? OFFSET ?");

    let mut query = sqlx::query_as::<_, ResearchSource>(&q).bind(&topic_id);
    if let Some(s) = &status {
        query = query.bind(s);
    }
    query = query.bind(limit).bind(offset);

    query.fetch_all(&state.db).await.map_err(|e| format!("db: {e}"))
}

/// Import a discovered source as a paper (download PDF and run import pipeline)
#[tauri::command]
#[instrument(skip(state))]
pub async fn research_import_source(
    state: State<'_, AppState>,
    source_id: String,
) -> Result<serde_json::Value, String> {
    let source = sqlx::query_as::<_, crate::core::models::ResearchSource>(
        "SELECT * FROM research_sources WHERE id = ?"
    ).bind(&source_id).fetch_optional(&state.db).await
        .map_err(|e| format!("db: {e}"))?
        .ok_or("source not found")?;

    let pdf_url = source.url.as_deref().unwrap_or("");
    if pdf_url.is_empty() {
        return Err("No PDF URL for this source".into());
    }

    // Download PDF to temp location
    let temp_dir = std::env::temp_dir().join("siku_imports");
    tokio::fs::create_dir_all(&temp_dir).await.map_err(|e| format!("mkdir: {e}"))?;
    let file_name = format!("{}.pdf", source.source_id.as_deref().unwrap_or("paper"));
    let temp_path = temp_dir.join(&file_name);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .user_agent("Siku/0.1")
        .build().map_err(|e| format!("client: {e}"))?;

    let resp = client.get(pdf_url).send().await.map_err(|e| format!("download: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("download failed: HTTP {}", resp.status()));
    }

    let bytes = resp.bytes().await.map_err(|e| format!("read: {e}"))?;

    // Validate PDF magic bytes
    if bytes.len() < 5 || &bytes[0..5] != b"%PDF-" {
        return Err(format!("Downloaded file is not a valid PDF (starts with: {:?})", &bytes[..std::cmp::min(20, bytes.len())]));
    }

    tokio::fs::write(&temp_path, &bytes).await.map_err(|e| format!("write: {e}"))?;

    // Run import pipeline
    let paper = crate::core::paper_service::import_paper(
        &state.db, &state.app_data_dir, &temp_path,
    ).await.map_err(|e| format!("import: {e}"))?;

    // Update source status
    let now = time::now_iso();
    sqlx::query("UPDATE research_sources SET status = 'imported', processed_at = ? WHERE id = ?")
        .bind(&now).bind(&source_id).execute(&state.db).await.map_err(|e| format!("db: {e}"))?;

    // Clean up temp file
    let _ = tokio::fs::remove_file(&temp_path).await;

    Ok(serde_json::json!({
        "paper_id": paper.id,
        "title": paper.title,
        "source_id": source_id,
        "status": "imported",
    }))
}

/// Delete a research topic and its sources
#[tauri::command]
#[instrument(skip(state))]
pub async fn research_delete_topic(
    state: State<'_, AppState>,
    topic_id: String,
) -> Result<(), String> {
    sqlx::query("DELETE FROM research_sources WHERE topic_id = ?").bind(&topic_id)
        .execute(&state.db).await.map_err(|e| format!("db: {e}"))?;
    sqlx::query("DELETE FROM research_topics WHERE id = ?").bind(&topic_id)
        .execute(&state.db).await.map_err(|e| format!("db: {e}"))?;
    Ok(())
}
