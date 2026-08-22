use tauri::State;
use tracing::instrument;

use crate::AppState;
use crate::core::models::{KnowledgeDomain, KnowledgeItem, KnowledgeItemInput};
use crate::core::time;

/// List all knowledge domains (5 default domains)
#[tauri::command]
#[instrument(skip(state))]
pub async fn knowledge_list_domains(
    state: State<'_, AppState>,
) -> Result<Vec<KnowledgeDomain>, String> {
    let domains = sqlx::query_as::<_, KnowledgeDomain>(
        "SELECT id, name, domain_type, icon, color, sort_order, created_at, updated_at
         FROM knowledge_domains ORDER BY sort_order ASC"
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| format!("db error: {e}"))?;

    Ok(domains)
}

/// Create a knowledge item in a domain
#[tauri::command]
#[instrument(skip(state))]
pub async fn knowledge_create_item(
    state: State<'_, AppState>,
    domain_id: String,
    title: String,
    content: Option<String>,
    content_type: Option<String>,
    source_type: Option<String>,
    source_id: Option<String>,
    tags: Option<Vec<String>>,
) -> Result<KnowledgeItem, String> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = time::now_iso();
    let content_type = content_type.unwrap_or_else(|| "note".to_string());
    let tags_json = serde_json::to_string(&tags.unwrap_or_default())
        .map_err(|e| format!("json error: {e}"))?;

    sqlx::query(
        "INSERT INTO knowledge_items (id, domain_id, title, content_type, content, source_type, source_id, tags, metadata, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, '{}', ?, ?)"
    )
    .bind(&id)
    .bind(&domain_id)
    .bind(&title)
    .bind(&content_type)
    .bind(&content)
    .bind(&source_type)
    .bind(&source_id)
    .bind(&tags_json)
    .bind(&now)
    .bind(&now)
    .execute(&state.db)
    .await
    .map_err(|e| format!("db error: {e}"))?;

    let item = sqlx::query_as::<_, KnowledgeItem>(
        "SELECT id, domain_id, title, content_type, content, source_type, source_id, metadata, tags, created_at, updated_at
         FROM knowledge_items WHERE id = ?"
    )
    .bind(&id)
    .fetch_one(&state.db)
    .await
    .map_err(|e| format!("db error: {e}"))?;

    Ok(item)
}

/// List knowledge items, optionally filtered by domain
#[tauri::command]
#[instrument(skip(state))]
pub async fn knowledge_list_items(
    state: State<'_, AppState>,
    domain_id: Option<String>,
    search: Option<String>,
    tag: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<KnowledgeItem>, String> {
    let mut query = String::from(
        "SELECT id, domain_id, title, content_type, content, source_type, source_id, metadata, tags, created_at, updated_at FROM knowledge_items WHERE 1=1"
    );
    let mut params: Vec<String> = Vec::new();

    if let Some(ref did) = domain_id {
        query.push_str(" AND domain_id = ?");
        params.push(did.clone());
    }
    if let Some(ref s) = search {
        query.push_str(" AND (title LIKE ? OR content LIKE ?)");
        let pattern = format!("%{}%", s);
        params.push(pattern.clone());
        params.push(pattern);
    }
    if let Some(ref t) = tag {
        query.push_str(" AND tags LIKE ?");
        params.push(format!("%{}%", t));
    }

    query.push_str(" ORDER BY updated_at DESC");

    if let Some(l) = limit {
        query.push_str(&format!(" LIMIT {}", l));
    }
    if let Some(o) = offset {
        query.push_str(&format!(" OFFSET {}", o));
    }

    let mut q = sqlx::query_as::<_, KnowledgeItem>(&query);
    for p in &params {
        q = q.bind(p);
    }

    let items = q.fetch_all(&state.db).await.map_err(|e| format!("db error: {e}"))?;

    Ok(items)
}

/// Get a single knowledge item
#[tauri::command]
#[instrument(skip(state))]
pub async fn knowledge_get_item(
    state: State<'_, AppState>,
    id: String,
) -> Result<KnowledgeItem, String> {
    let item = sqlx::query_as::<_, KnowledgeItem>(
        "SELECT id, domain_id, title, content_type, content, source_type, source_id, metadata, tags, created_at, updated_at
         FROM knowledge_items WHERE id = ?"
    )
    .bind(&id)
    .fetch_one(&state.db)
    .await
    .map_err(|e| format!("db error: {e}"))?;

    Ok(item)
}

/// Update a knowledge item
#[tauri::command]
#[instrument(skip(state))]
pub async fn knowledge_update_item(
    state: State<'_, AppState>,
    id: String,
    title: Option<String>,
    content: Option<String>,
    domain_id: Option<String>,
    tags: Option<Vec<String>>,
) -> Result<KnowledgeItem, String> {
    let now = time::now_iso();
    if let Some(t) = &title {
        sqlx::query("UPDATE knowledge_items SET title = ?, updated_at = ? WHERE id = ?")
            .bind(t).bind(&now).bind(&id).execute(&state.db).await.map_err(|e| format!("db: {e}"))?;
    }
    if let Some(c) = &content {
        sqlx::query("UPDATE knowledge_items SET content = ?, updated_at = ? WHERE id = ?")
            .bind(c).bind(&now).bind(&id).execute(&state.db).await.map_err(|e| format!("db: {e}"))?;
    }
    if let Some(d) = &domain_id {
        sqlx::query("UPDATE knowledge_items SET domain_id = ?, updated_at = ? WHERE id = ?")
            .bind(d).bind(&now).bind(&id).execute(&state.db).await.map_err(|e| format!("db: {e}"))?;
    }
    if let Some(t) = &tags {
        let tj = serde_json::to_string(t).map_err(|e| format!("json: {e}"))?;
        sqlx::query("UPDATE knowledge_items SET tags = ?, updated_at = ? WHERE id = ?")
            .bind(&tj).bind(&now).bind(&id).execute(&state.db).await.map_err(|e| format!("db: {e}"))?;
    }
    crate::core::knowledge::get_item(&state.db, &id).await
}

/// Delete a knowledge item
#[tauri::command]
#[instrument(skip(state))]
pub async fn knowledge_delete_item(
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    crate::core::knowledge::delete_item(&state.db, &id).await
}

// ============================================================
// Domain CRUD
// ============================================================

#[tauri::command]
#[instrument(skip(state))]
pub async fn knowledge_create_domain(
    state: State<'_, AppState>,
    name: String,
    domain_type: String,
    icon: Option<String>,
    color: Option<String>,
) -> Result<KnowledgeDomain, String> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = time::now_iso();
    let color = color.unwrap_or_else(|| "#3b82f6".into());
    let max_sort: i32 = sqlx::query_scalar("SELECT COALESCE(MAX(sort_order), -1) FROM knowledge_domains")
        .fetch_one(&state.db).await.map_err(|e| format!("db: {e}"))?;

    sqlx::query(
        "INSERT INTO knowledge_domains (id, name, domain_type, icon, color, sort_order, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
    ).bind(&id).bind(&name).bind(&domain_type).bind(&icon).bind(&color).bind(max_sort + 1).bind(&now).bind(&now)
     .execute(&state.db).await.map_err(|e| format!("db: {e}"))?;

    sqlx::query_as::<_, KnowledgeDomain>("SELECT * FROM knowledge_domains WHERE id = ?")
        .bind(&id).fetch_one(&state.db).await.map_err(|e| format!("db: {e}"))
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn knowledge_update_domain(
    state: State<'_, AppState>,
    id: String,
    name: Option<String>,
    icon: Option<String>,
    color: Option<String>,
    sort_order: Option<i32>,
) -> Result<(), String> {
    let now = time::now_iso();
    if let Some(n) = &name { sqlx::query("UPDATE knowledge_domains SET name = ?, updated_at = ? WHERE id = ?").bind(n).bind(&now).bind(&id).execute(&state.db).await.map_err(|e| format!("db: {e}"))?; }
    if let Some(i) = &icon { sqlx::query("UPDATE knowledge_domains SET icon = ?, updated_at = ? WHERE id = ?").bind(i).bind(&now).bind(&id).execute(&state.db).await.map_err(|e| format!("db: {e}"))?; }
    if let Some(c) = &color { sqlx::query("UPDATE knowledge_domains SET color = ?, updated_at = ? WHERE id = ?").bind(c).bind(&now).bind(&id).execute(&state.db).await.map_err(|e| format!("db: {e}"))?; }
    if let Some(s) = sort_order { sqlx::query("UPDATE knowledge_domains SET sort_order = ?, updated_at = ? WHERE id = ?").bind(s).bind(&now).bind(&id).execute(&state.db).await.map_err(|e| format!("db: {e}"))?; }
    Ok(())
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn knowledge_delete_domain(
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    // Don't allow deleting the 5 default domains
    if id.starts_with("dom-") {
        return Err("Cannot delete default knowledge domains".into());
    }
    sqlx::query("DELETE FROM knowledge_items WHERE domain_id = ?").bind(&id).execute(&state.db).await.map_err(|e| format!("db: {e}"))?;
    sqlx::query("DELETE FROM knowledge_domains WHERE id = ?").bind(&id).execute(&state.db).await.map_err(|e| format!("db: {e}"))?;
    Ok(())
}
