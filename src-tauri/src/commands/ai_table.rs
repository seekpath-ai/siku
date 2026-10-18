//! AI 表格列 commands: batch extraction lifecycle + cell/preset CRUD.

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tauri::{AppHandle, State};

use crate::core::ai_table::{self, TableColumn, TableCell};
use crate::AppState;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TablePreset {
    pub id: String,
    pub name: String,
    pub columns: Vec<TableColumn>,
    pub updated_at: String,
}

/// Start a batch extraction run (spawned; progress on
/// `library:ai-table-progress`). One run at a time. `provider_id` overrides
/// the default (starred) model when set; `detail` scales how verbose each cell
/// is (brief | normal | rich); `depth` how hard it looks for evidence; `verify`
/// whether doubtful cells get cross-checked (off | doubtful | all); `force`
/// re-extracts cells that already have a value (manual edits are still kept).
#[tauri::command]
pub async fn ai_table_extract_start(
    app: AppHandle,
    state: State<'_, AppState>,
    paper_ids: Vec<String>,
    columns: Vec<TableColumn>,
    provider_id: Option<String>,
    detail: Option<String>,
    depth: Option<String>,
    verify: Option<String>,
    force: Option<bool>,
) -> Result<(), String> {
    if paper_ids.is_empty() {
        return Err("未选择文献".to_string());
    }
    if columns.is_empty() {
        return Err("请先添加至少一列".to_string());
    }
    let db = state.db.clone();
    let detail = crate::core::ai_table::Detail::parse(detail.as_deref());
    let depth = crate::core::ai_table::Depth::parse(depth.as_deref());
    let verify = crate::core::ai_table::Verify::parse(verify.as_deref());
    let force = force.unwrap_or(false);
    tauri::async_runtime::spawn(async move {
        ai_table::run_extraction(app, db, paper_ids, columns, provider_id, detail, depth, verify, force)
            .await;
    });
    Ok(())
}

/// Search terms currently stored for these columns (cache only — never calls the
/// model), so the column editor can show and override them.
#[tauri::command]
pub async fn ai_table_terms_get(
    state: State<'_, AppState>,
    column_keys: Vec<String>,
) -> Result<Vec<crate::core::query_expand::ColumnTerms>, String> {
    Ok(crate::core::query_expand::terms_of(&state.db, &column_keys).await)
}

/// Hand-write a column's search terms; an empty list clears the override and
/// returns the column to automatic behaviour.
#[tauri::command]
pub async fn ai_table_terms_set(
    state: State<'_, AppState>,
    column_key: String,
    terms: Vec<String>,
) -> Result<Vec<crate::core::query_expand::ColumnTerms>, String> {
    crate::core::query_expand::set_terms(&state.db, &column_key, &terms).await?;
    Ok(crate::core::query_expand::terms_of(&state.db, &[column_key]).await)
}

/// Recompute a column's search terms now (one model call when the built-in
/// mapping has nothing for it).
#[tauri::command]
pub async fn ai_table_terms_regenerate(
    state: State<'_, AppState>,
    column_key: String,
    prompt: Option<String>,
) -> Result<Vec<String>, String> {
    let llm = crate::core::llm_provider_service::load_llm_config_for(&state.db, None)
        .await
        .ok()
        .and_then(|cfg| crate::ai::llm::client::create_llm_client(&cfg).ok());
    let prompt = prompt.unwrap_or_default();
    let terms = crate::core::query_expand::regenerate(
        &state.db,
        llm.as_ref().map(|c| c.as_ref() as &dyn crate::ai::llm::LlmClient),
        &column_key,
        &prompt,
    )
    .await?;
    Ok(terms)
}

/// The passages that backed one cell — the audit trail behind a value.
#[tauri::command]
pub async fn ai_table_cell_evidence(
    state: State<'_, AppState>,
    paper_id: String,
    column_key: String,
) -> Result<Vec<crate::core::ai_table::EvidenceRef>, String> {
    ai_table::cell_evidence(&state.db, &paper_id, &column_key).await
}

/// Built-in column presets (read-only starting points, see
/// `core::ai_table_builtin`). Not stored in `ai_table_presets`: picking one only
/// fills the column editor.
#[tauri::command]
pub async fn ai_table_builtin_presets() -> Result<Vec<crate::core::ai_table_builtin::BuiltinPreset>, String> {
    Ok(crate::core::ai_table_builtin::builtins())
}

#[tauri::command]
pub async fn ai_table_extract_cancel() -> Result<(), String> {
    ai_table::request_cancel();
    Ok(())
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractStatus {
    pub running: bool,
    pub progress: Option<crate::core::ai_table::ExtractProgress>,
}

/// Whether a run is in flight, plus its last beat. The dialog asks on open so
/// closing it (or a webview reload) never strands a run without a viewer.
#[tauri::command]
pub async fn ai_table_extract_status() -> Result<ExtractStatus, String> {
    let (running, progress) = ai_table::status();
    Ok(ExtractStatus { running, progress })
}

#[tauri::command]
pub async fn ai_table_cells_list(
    state: State<'_, AppState>,
    paper_ids: Vec<String>,
    column_keys: Vec<String>,
) -> Result<Vec<TableCell>, String> {
    ai_table::list_cells(&state.db, &paper_ids, &column_keys).await
}

#[tauri::command]
pub async fn ai_table_cell_set(
    state: State<'_, AppState>,
    paper_id: String,
    column_key: String,
    value: String,
) -> Result<(), String> {
    ai_table::set_cell(&state.db, &paper_id, &column_key, &value).await
}

#[tauri::command]
pub async fn ai_table_presets_list(state: State<'_, AppState>) -> Result<Vec<TablePreset>, String> {
    list_presets(&state.db).await
}

async fn list_presets(db: &SqlitePool) -> Result<Vec<TablePreset>, String> {
    let rows = sqlx::query_as::<_, (String, String, String, String)>(
        "SELECT id, name, columns_json, updated_at FROM ai_table_presets ORDER BY updated_at DESC",
    )
    .fetch_all(db)
    .await
    .map_err(|e| format!("db: {e}"))?;
    Ok(rows
        .into_iter()
        .map(|(id, name, columns_json, updated_at)| TablePreset {
            id,
            name,
            columns: serde_json::from_str(&columns_json).unwrap_or_default(),
            updated_at,
        })
        .collect())
}

/// Insert or update a preset (id empty = create). Returns the saved preset.
#[tauri::command]
pub async fn ai_table_preset_save(
    state: State<'_, AppState>,
    id: Option<String>,
    name: String,
    columns: Vec<TableColumn>,
) -> Result<TablePreset, String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("模板名不能为空".to_string());
    }
    let now = crate::core::time::now_iso();
    let id = id.filter(|s| !s.is_empty()).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let columns_json = serde_json::to_string(&columns).map_err(|e| format!("json: {e}"))?;
    sqlx::query(
        "INSERT INTO ai_table_presets (id, name, columns_json, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT (id) DO UPDATE SET name = excluded.name,
           columns_json = excluded.columns_json, updated_at = excluded.updated_at",
    )
    .bind(&id)
    .bind(&name)
    .bind(&columns_json)
    .bind(&now)
    .bind(&now)
    .execute(&state.db)
    .await
    .map_err(|e| format!("db: {e}"))?;
    Ok(TablePreset { id, name, columns, updated_at: now })
}

#[tauri::command]
pub async fn ai_table_preset_delete(state: State<'_, AppState>, id: String) -> Result<(), String> {
    sqlx::query("DELETE FROM ai_table_presets WHERE id = ?")
        .bind(&id)
        .execute(&state.db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaperBrief {
    pub id: String,
    pub title: String,
    pub year: Option<i64>,
}

/// id → title/year for the grid rows, in input order (one round trip for the
/// whole paper set instead of one invoke per paper).
#[tauri::command]
pub async fn ai_table_papers_info(
    state: State<'_, AppState>,
    paper_ids: Vec<String>,
) -> Result<Vec<PaperBrief>, String> {
    if paper_ids.is_empty() {
        return Ok(Vec::new());
    }
    let ph = paper_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!("SELECT id, title, year FROM papers WHERE id IN ({ph}) AND deleted_at IS NULL");
    let mut q = sqlx::query_as::<_, (String, String, Option<i64>)>(&sql);
    for id in &paper_ids {
        q = q.bind(id);
    }
    let rows = q.fetch_all(&state.db).await.map_err(|e| format!("db: {e}"))?;
    let mut map: std::collections::HashMap<String, PaperBrief> = rows
        .into_iter()
        .map(|(id, title, year)| (id.clone(), PaperBrief { id, title, year }))
        .collect();
    Ok(paper_ids.into_iter().filter_map(|id| map.remove(&id)).collect())
}
