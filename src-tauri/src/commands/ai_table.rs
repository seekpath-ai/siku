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
/// `library:ai-table-progress`). One run at a time.
#[tauri::command]
pub async fn ai_table_extract_start(
    app: AppHandle,
    state: State<'_, AppState>,
    paper_ids: Vec<String>,
    columns: Vec<TableColumn>,
) -> Result<(), String> {
    if paper_ids.is_empty() {
        return Err("未选择文献".to_string());
    }
    if columns.is_empty() {
        return Err("请先添加至少一列".to_string());
    }
    let db = state.db.clone();
    tauri::async_runtime::spawn(async move {
        ai_table::run_extraction(app, db, paper_ids, columns).await;
    });
    Ok(())
}

#[tauri::command]
pub async fn ai_table_extract_cancel() -> Result<(), String> {
    ai_table::request_cancel();
    Ok(())
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
