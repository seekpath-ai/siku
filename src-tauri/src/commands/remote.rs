//! Remote IM channel management commands (settings UI).

use tauri::{AppHandle, Emitter, State};
use tracing::instrument;

use crate::core::models::FeishuChannelConfig;
use crate::remote::ChannelStatusInfo;
use crate::AppState;

#[tauri::command]
#[instrument(skip(state))]
pub async fn remote_status(state: State<'_, AppState>) -> Result<Vec<ChannelStatusInfo>, String> {
    Ok(state.remote.status().await)
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn remote_feishu_config(state: State<'_, AppState>) -> Result<FeishuChannelConfig, String> {
    Ok(crate::core::settings_service::load_device_settings(&state.db)
        .await?
        .remote
        .feishu)
}

#[tauri::command]
#[instrument(skip(state, app))]
pub async fn remote_feishu_save(
    state: State<'_, AppState>,
    app: AppHandle,
    app_id: String,
    app_secret: String,
    enabled: bool,
) -> Result<(), String> {
    let mut settings = crate::core::settings_service::load_device_settings(&state.db).await?;
    settings.remote.feishu = FeishuChannelConfig {
        enabled,
        app_id: app_id.trim().to_string(),
        app_secret: app_secret.trim().to_string(),
    };
    crate::core::settings_service::save_device_settings(&state.db, &settings).await?;
    state.remote.sync_config(&state.db, &app).await;
    Ok(())
}

#[derive(serde::Serialize, sqlx::FromRow)]
pub struct PairingRow {
    pub channel: String,
    pub chat_id: String,
    pub user_id: String,
    pub user_name: String,
    pub approved: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn remote_pairings(state: State<'_, AppState>) -> Result<Vec<PairingRow>, String> {
    sqlx::query_as::<_, PairingRow>(
        "SELECT channel, chat_id, user_id, user_name, approved, created_at, updated_at
         FROM remote_chats ORDER BY approved ASC, updated_at DESC",
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| format!("db error: {e}"))
}

#[tauri::command]
#[instrument(skip(state, app))]
pub async fn remote_pairing_set(
    state: State<'_, AppState>,
    app: AppHandle,
    channel: String,
    chat_id: String,
    approved: bool,
) -> Result<(), String> {
    sqlx::query(
        "UPDATE remote_chats SET approved = ?, updated_at = ? WHERE channel = ? AND chat_id = ?",
    )
    .bind(approved as i64)
    .bind(crate::core::time::now_iso())
    .bind(&channel)
    .bind(&chat_id)
    .execute(&state.db)
    .await
    .map_err(|e| format!("db error: {e}"))?;
    let _ = app.emit("remote:pairing", ());
    Ok(())
}

#[tauri::command]
#[instrument(skip(state, app))]
pub async fn remote_pairing_delete(
    state: State<'_, AppState>,
    app: AppHandle,
    channel: String,
    chat_id: String,
) -> Result<(), String> {
    sqlx::query("DELETE FROM remote_chats WHERE channel = ? AND chat_id = ?")
        .bind(&channel)
        .bind(&chat_id)
        .execute(&state.db)
        .await
        .map_err(|e| format!("db error: {e}"))?;
    let _ = app.emit("remote:pairing", ());
    Ok(())
}
