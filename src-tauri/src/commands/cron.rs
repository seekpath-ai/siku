use tauri::State;
use tracing::instrument;

use crate::AppState;
use crate::core::models::{CronJob, CronJobInput};

/// cron_jobs 的完整列清单；调度器与命令共用它，避免漏列导致 FromRow 报错
pub(crate) const JOB_COLS: &str =
    "id, session_id, cron, prompt, recurring, enabled, last_fired, created_at, updated_at";

/// Schedule a prompt to be fired at a future time (5-field cron).
#[tauri::command]
#[instrument(skip(state))]
pub async fn cron_create(
    state: State<'_, AppState>,
    input: CronJobInput,
) -> Result<CronJob, String> {
    crate::core::cron_scheduler::validate(&input.cron)?;
    if input.prompt.trim().is_empty() {
        return Err("prompt required".to_string());
    }
    let exists: Option<(String,)> =
        sqlx::query_as("SELECT id FROM chat_sessions WHERE id = ?")
            .bind(&input.session_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| format!("db: {e}"))?;
    if exists.is_none() {
        return Err("session not found".to_string());
    }

    let id = uuid::Uuid::new_v4().to_string();
    let now = crate::core::time::now_iso();
    sqlx::query(
        "INSERT INTO cron_jobs (id, session_id, cron, prompt, recurring, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&input.session_id)
    .bind(&input.cron)
    .bind(&input.prompt)
    .bind(input.recurring.unwrap_or(true) as i32)
    .bind(&now)
    .bind(&now)
    .execute(&state.db)
    .await
    .map_err(|e| format!("db: {e}"))?;

    sqlx::query_as::<_, CronJob>(&format!(
        "SELECT {JOB_COLS} FROM cron_jobs WHERE id = ?"
    ))
    .bind(&id)
    .fetch_one(&state.db)
    .await
    .map_err(|e| format!("db: {e}"))
}

/// List scheduled cron jobs.
#[tauri::command]
#[instrument(skip(state))]
pub async fn cron_list(state: State<'_, AppState>) -> Result<Vec<CronJob>, String> {
    sqlx::query_as::<_, CronJob>(&format!(
        "SELECT {JOB_COLS} FROM cron_jobs ORDER BY created_at"
    ))
    .fetch_all(&state.db)
    .await
    .map_err(|e| format!("db: {e}"))
}

/// Cancel a cron job.
#[tauri::command]
#[instrument(skip(state))]
pub async fn cron_delete(state: State<'_, AppState>, id: String) -> Result<(), String> {
    sqlx::query("DELETE FROM cron_jobs WHERE id = ?")
        .bind(&id)
        .execute(&state.db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    Ok(())
}

/// 启用/禁用一个定时任务（不删除，任务中心里可以随时再打开）
#[tauri::command]
#[instrument(skip(state))]
pub async fn cron_set_enabled(
    state: State<'_, AppState>,
    id: String,
    enabled: bool,
) -> Result<(), String> {
    sqlx::query("UPDATE cron_jobs SET enabled = ?, updated_at = ? WHERE id = ?")
        .bind(enabled as i32)
        .bind(crate::core::time::now_iso())
        .bind(&id)
        .execute(&state.db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    Ok(())
}
