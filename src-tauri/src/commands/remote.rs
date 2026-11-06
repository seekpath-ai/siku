//! Remote IM channel management commands (settings UI).

use tauri::{AppHandle, Emitter, State};
use tracing::instrument;

use crate::remote::ChannelStatusInfo;
use crate::AppState;

#[tauri::command]
#[instrument(skip(state))]
pub async fn remote_status(state: State<'_, AppState>) -> Result<Vec<ChannelStatusInfo>, String> {
    Ok(state.remote.status().await)
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn remote_feishu_bots(
    state: State<'_, AppState>,
) -> Result<Vec<crate::core::models::FeishuBotConfig>, String> {
    Ok(crate::core::settings_service::load_remote_config(&state.db)
        .await?
        .feishu_bots)
}

/// Upsert a feishu bot (keyed by app_id), then re-evaluate channels.
#[tauri::command]
#[instrument(skip(state, app))]
pub async fn remote_feishu_bot_save(
    state: State<'_, AppState>,
    app: AppHandle,
    name: String,
    app_id: String,
    app_secret: String,
    enabled: bool,
) -> Result<(), String> {
    let app_id = app_id.trim().to_string();
    if app_id.is_empty() {
        return Err("App ID 不能为空".to_string());
    }
    let mut remote = crate::core::settings_service::load_remote_config(&state.db).await?;
    let bots = &mut remote.feishu_bots;
    match bots.iter_mut().find(|b| b.app_id == app_id) {
        Some(bot) => {
            bot.name = name.trim().to_string();
            bot.enabled = enabled;
            if !app_secret.trim().is_empty() {
                bot.app_secret = app_secret.trim().to_string();
            }
        }
        None => {
            if app_secret.trim().is_empty() {
                return Err("App Secret 不能为空".to_string());
            }
            bots.push(crate::core::models::FeishuBotConfig {
                name: name.trim().to_string(),
                enabled,
                app_id,
                app_secret: app_secret.trim().to_string(),
            });
        }
    }
    crate::core::settings_service::save_remote_config(&state.db, &remote).await?;
    state.remote.sync_config(&state.db, &app).await;
    Ok(())
}

#[tauri::command]
#[instrument(skip(state, app))]
pub async fn remote_feishu_bot_delete(
    state: State<'_, AppState>,
    app: AppHandle,
    app_id: String,
) -> Result<(), String> {
    let mut remote = crate::core::settings_service::load_remote_config(&state.db).await?;
    remote.feishu_bots.retain(|b| b.app_id != app_id);
    crate::core::settings_service::save_remote_config(&state.db, &remote).await?;
    state.remote.sync_config(&state.db, &app).await;
    Ok(())
}

#[derive(serde::Serialize)]
pub struct WeixinConfigInfo {
    pub enabled: bool,
    pub ilink_user_id: String,
    pub ilink_bot_id: String,
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn remote_weixin_config(state: State<'_, AppState>) -> Result<WeixinConfigInfo, String> {
    let cfg = crate::core::settings_service::load_remote_config(&state.db)
        .await?
        .weixin;
    Ok(WeixinConfigInfo {
        enabled: cfg.enabled,
        ilink_user_id: cfg.ilink_user_id,
        ilink_bot_id: cfg.ilink_bot_id,
    })
}

/// Start the QR login flow. The QR image and progress arrive via the
/// `remote:weixin-qr` event.
#[tauri::command]
#[instrument(skip(state, app))]
pub async fn remote_weixin_login_start(
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<(), String> {
    if state.remote.start_weixin_login(&app).await {
        Ok(())
    } else {
        Err("已有进行中的扫码登录".to_string())
    }
}

/// Submit the digit code WeChat shows when the server asks for verification.
#[tauri::command]
#[instrument(skip(state))]
pub async fn remote_weixin_login_verify(
    state: State<'_, AppState>,
    code: String,
) -> Result<(), String> {
    if state.remote.weixin_login_verify(code).await {
        Ok(())
    } else {
        Err("当前没有进行中的扫码登录".to_string())
    }
}

#[tauri::command]
#[instrument(skip(state, app))]
pub async fn remote_weixin_logout(state: State<'_, AppState>, app: AppHandle) -> Result<(), String> {
    state.remote.cancel_weixin_login().await;
    let mut remote = crate::core::settings_service::load_remote_config(&state.db).await?;
    remote.weixin = crate::core::models::WeixinChannelConfig::default();
    crate::core::settings_service::save_remote_config(&state.db, &remote).await?;
    // Drop the roaming cursor/context tokens too — no device should resume
    // the dead session.
    let _ = crate::core::settings_service::save_weixin_state(&state.db, &Default::default()).await;
    state.remote.sync_config(&state.db, &app).await;
    Ok(())
}

/// Manual takeover: make this device the preferred remote-connection
/// holder (synced record honored while this device is online).
#[tauri::command]
#[instrument(skip(state, app))]
pub async fn remote_takeover(state: State<'_, AppState>, app: AppHandle) -> Result<(), String> {
    state.remote.takeover(&state.db, &app).await
}

#[derive(serde::Serialize, sqlx::FromRow)]
pub struct PairingRow {
    pub channel: String,
    pub chat_id: String,
    pub user_id: String,
    pub user_name: String,
    pub approved: i64,
    pub grant_level: String,
    pub created_at: String,
    pub updated_at: String,
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn remote_pairings(state: State<'_, AppState>) -> Result<Vec<PairingRow>, String> {
    sqlx::query_as::<_, PairingRow>(
        "SELECT channel, chat_id, user_id, user_name, approved, grant_level, created_at, updated_at
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
    user_id: String,
    approved: bool,
) -> Result<(), String> {
    sqlx::query(
        "UPDATE remote_chats SET approved = ?, updated_at = ? WHERE channel = ? AND chat_id = ? AND user_id = ?",
    )
    .bind(approved as i64)
    .bind(crate::core::time::now_iso())
    .bind(&channel)
    .bind(&chat_id)
    .bind(&user_id)
    .execute(&state.db)
    .await
    .map_err(|e| format!("db error: {e}"))?;
    let _ = app.emit("remote:pairing", ());
    Ok(())
}

/// Change a paired member's grant: "full" = all tools, "readonly" = the
/// per-turn read-only set. Takes effect on the member's next message.
#[tauri::command]
#[instrument(skip(state, app))]
pub async fn remote_pairing_set_grant(
    state: State<'_, AppState>,
    app: AppHandle,
    channel: String,
    chat_id: String,
    user_id: String,
    grant: String,
) -> Result<(), String> {
    let grant = match grant.as_str() {
        "full" | "readonly" => grant,
        _ => return Err("无效的授权级别".to_string()),
    };
    sqlx::query(
        "UPDATE remote_chats SET grant_level = ?, updated_at = ? WHERE channel = ? AND chat_id = ? AND user_id = ?",
    )
    .bind(&grant)
    .bind(crate::core::time::now_iso())
    .bind(&channel)
    .bind(&chat_id)
    .bind(&user_id)
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
    user_id: String,
) -> Result<(), String> {
    sqlx::query("DELETE FROM remote_chats WHERE channel = ? AND chat_id = ? AND user_id = ?")
        .bind(&channel)
        .bind(&chat_id)
        .bind(&user_id)
        .execute(&state.db)
        .await
        .map_err(|e| format!("db error: {e}"))?;
    let _ = app.emit("remote:pairing", ());
    Ok(())
}

// ---------------------------------------------------------------- group info & files (feishu)

fn feishu_cfg(
    remote: &crate::core::models::RemoteSettings,
    channel: &str,
) -> Result<crate::core::models::FeishuBotConfig, String> {
    let bots = &remote.feishu_bots;
    // Instance channel "feishu:<app_id>" picks the exact bot; legacy bare
    // "feishu" (pre-multi-bot sessions) resolves to the only configured bot.
    let bot = match channel.strip_prefix("feishu:") {
        Some(app_id) => bots.iter().find(|b| b.app_id == app_id),
        None if bots.len() == 1 => bots.first(),
        None => None,
    };
    let bot = bot.ok_or_else(|| "飞书机器人配置不存在（可能已删除）".to_string())?;
    if bot.app_id.is_empty() || bot.app_secret.is_empty() {
        return Err("飞书渠道未配置 App ID / App Secret".to_string());
    }
    Ok(bot.clone())
}

#[derive(serde::Serialize)]
pub struct RemoteChatMember {
    pub id: String,
    pub name: String,
    pub is_owner: bool,
}

#[derive(serde::Serialize)]
pub struct RemoteChatInfo {
    pub name: String,
    pub description: String,
    pub member_count: i64,
    pub members: Vec<RemoteChatMember>,
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn remote_chat_info(
    state: State<'_, AppState>,
    channel: String,
    chat_id: String,
) -> Result<RemoteChatInfo, String> {
    if crate::remote::router::channel_kind(&channel) != "feishu" {
        return Err("仅飞书会话支持查看群信息".to_string());
    }
    let cfg = feishu_cfg(&crate::core::settings_service::load_remote_config(&state.db).await?, &channel)?;
    let (info, members) = crate::remote::feishu::fetch_chat_info(&cfg, &chat_id).await?;
    Ok(RemoteChatInfo {
        name: info.name,
        description: info.description,
        member_count: info.member_count,
        members: members
            .into_iter()
            .map(|m| RemoteChatMember {
                is_owner: !info.owner_id.is_empty() && m.open_id == info.owner_id,
                id: m.open_id,
                name: m.name,
            })
            .collect(),
    })
}

#[derive(serde::Serialize)]
pub struct RemoteChatFile {
    pub message_id: String,
    pub file_key: String,
    pub kind: String,
    pub name: String,
    pub size_bytes: i64,
    pub sender_id: String,
    pub create_time_ms: String,
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn remote_chat_files(
    state: State<'_, AppState>,
    channel: String,
    chat_id: String,
) -> Result<Vec<RemoteChatFile>, String> {
    if crate::remote::router::channel_kind(&channel) != "feishu" {
        return Err("仅飞书会话支持查看群文件".to_string());
    }
    let cfg = feishu_cfg(&crate::core::settings_service::load_remote_config(&state.db).await?, &channel)?;
    let files = crate::remote::feishu::fetch_chat_files(&cfg, &chat_id).await?;
    Ok(files
        .into_iter()
        .map(|f| RemoteChatFile {
            message_id: f.message_id,
            file_key: f.file_key,
            kind: f.kind,
            name: f.name,
            size_bytes: f.size_bytes,
            sender_id: f.sender_id,
            create_time_ms: f.create_time_ms,
        })
        .collect())
}

/// Download a chat file message attachment to a user-chosen path.
#[tauri::command]
#[instrument(skip(state))]
pub async fn remote_chat_file_save(
    state: State<'_, AppState>,
    channel: String,
    message_id: String,
    file_key: String,
    kind: String,
    save_path: String,
) -> Result<(), String> {
    if crate::remote::router::channel_kind(&channel) != "feishu" {
        return Err("仅飞书会话支持下载群文件".to_string());
    }
    let cfg = feishu_cfg(&crate::core::settings_service::load_remote_config(&state.db).await?, &channel)?;
    let (bytes, _mime) = crate::remote::feishu::download_resource(&cfg, &message_id, &file_key, &kind).await?;
    std::fs::write(&save_path, &bytes).map_err(|e| format!("写入文件失败: {e}"))
}

/// Download a chat file and import it into the library (PDF).
#[tauri::command]
#[instrument(skip(state))]
pub async fn remote_chat_file_import(
    state: State<'_, AppState>,
    channel: String,
    message_id: String,
    file_key: String,
    kind: String,
    file_name: String,
) -> Result<crate::core::models::Paper, String> {
    if crate::remote::router::channel_kind(&channel) != "feishu" {
        return Err("仅飞书会话支持导入群文件".to_string());
    }
    let cfg = feishu_cfg(&crate::core::settings_service::load_remote_config(&state.db).await?, &channel)?;
    let (bytes, _mime) = crate::remote::feishu::download_resource(&cfg, &message_id, &file_key, &kind).await?;
    let tmp_dir = state.app_data_dir.join("remote_downloads");
    std::fs::create_dir_all(&tmp_dir).map_err(|e| format!("创建下载目录失败: {e}"))?;
    let tmp = tmp_dir.join(format!("{}_{}", uuid::Uuid::new_v4(), file_name));
    std::fs::write(&tmp, &bytes).map_err(|e| format!("写入临时文件失败: {e}"))?;
    let result = crate::core::paper_service::import_paper(&state.db, &state.app_data_dir, &tmp)
        .await
        .map_err(|e| e.to_string());
    let _ = std::fs::remove_file(&tmp);
    result
}

/// Read the account-wide outbound-file switch.
#[tauri::command]
#[instrument(skip(state))]
pub async fn remote_outbound_files_enabled(state: State<'_, AppState>) -> Result<bool, String> {
    Ok(crate::core::settings_service::load_remote_config(&state.db)
        .await?
        .allow_outbound_files)
}

/// Toggle the account-wide outbound-file switch (synced with the channel
/// config, so every device of the account follows it).
#[tauri::command]
#[instrument(skip(state))]
pub async fn remote_outbound_files_set(
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<(), String> {
    let mut cfg = crate::core::settings_service::load_remote_config(&state.db).await?;
    cfg.allow_outbound_files = enabled;
    crate::core::settings_service::save_remote_config(&state.db, &cfg).await
}

/// P0 smoke test for outbound WeChat media (docs/remote-channels.md §7.9):
/// send one local file over the stored binding, bypassing the agent. Used to
/// verify `getuploadurl` + CDN upload against a real account before the
/// agent-side tool is trusted with it.
#[tauri::command]
#[instrument(skip(state, caption))]
pub async fn remote_weixin_send_file(
    state: State<'_, AppState>,
    file_path: String,
    chat_id: Option<String>,
    caption: Option<String>,
) -> Result<String, String> {
    let remote = crate::core::settings_service::load_remote_config(&state.db).await?;
    // No explicit chat: use the most recently active approved WeChat chat, so
    // the user does not have to paste an opaque iLink id.
    let chat_id = match chat_id.filter(|c| !c.trim().is_empty()) {
        Some(c) => c,
        None => sqlx::query_scalar::<_, String>(
            "SELECT chat_id FROM remote_chats
             WHERE channel = 'weixin' AND approved = 1 AND chat_id != ''
             ORDER BY updated_at DESC LIMIT 1",
        )
        .fetch_optional(&state.db)
        .await
        .map_err(|e| format!("db error: {e}"))?
        .ok_or_else(|| "没有已配对的微信会话，请先在微信里给机器人发一条消息并完成配对".to_string())?,
    };
    crate::remote::weixin::send_file_now(
        &state.db,
        &remote.weixin,
        &chat_id,
        &file_path,
        caption,
    )
    .await
}

/// Re-send one attachment that failed earlier, without re-running the whole
/// turn (the card's job is "send this file again", not "rethink the answer").
///
/// Mirrors the delivery path: same channel sender, same row-bookkeeping, same
/// `chat:message_updated` event so the open panel flips to 已发送.
#[tauri::command]
#[instrument(skip(state, app))]
pub async fn remote_attachment_retry(
    state: State<'_, AppState>,
    app: AppHandle,
    session_id: String,
    message_id: String,
    attachment_index: usize,
) -> Result<(), String> {
    // 1. The session must still be a private remote chat: retrying inside a
    //    group would broadcast the file exactly like the original send would.
    let context: Option<String> =
        sqlx::query_scalar("SELECT context FROM chat_sessions WHERE id = ?")
            .bind(&session_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| format!("db error: {e}"))?;
    let ctx: serde_json::Value = context
        .as_deref()
        .and_then(|c| serde_json::from_str(c).ok())
        .ok_or_else(|| "不是远程会话，无法重发".to_string())?;
    if ctx.get("remote").and_then(|r| r.as_bool()) != Some(true) {
        return Err("不是远程会话，无法重发".to_string());
    }
    if ctx.get("kind").and_then(|k| k.as_str()) != Some("private") {
        return Err("群聊不支持发送文件（会广播给所有成员）".to_string());
    }
    let channel = ctx
        .get("channel")
        .and_then(|c| c.as_str())
        .ok_or_else(|| "会话缺少渠道信息".to_string())?
        .to_string();
    let chat_id = ctx
        .get("chat_id")
        .and_then(|c| c.as_str())
        .ok_or_else(|| "会话缺少 chat_id".to_string())?
        .to_string();

    // 2. Rebuild the attachment from the stored ledger entry.
    let attachments: Option<String> = sqlx::query_scalar(
        "SELECT attachments FROM chat_messages WHERE id = ? AND session_id = ?",
    )
    .bind(&message_id)
    .bind(&session_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| format!("db error: {e}"))?;
    let current = attachments
        .filter(|a| !a.trim().is_empty() && a.trim() != "null")
        .ok_or_else(|| "该消息没有附件".to_string())?;
    let file = crate::remote::router::outbound_file_at(&current, attachment_index)
        .ok_or_else(|| "附件不存在（索引可能已过期）".to_string())?;
    if !file.path.exists() {
        return Err("文件已不在本机，无法重发".to_string());
    }

    let remote = crate::core::settings_service::load_remote_config(&state.db).await?;
    if !remote.allow_outbound_files {
        return Err("管理员已关闭「允许远程回传文件」".to_string());
    }

    // 3. Send through the same channel-level sender the turn used.
    match crate::remote::router::channel_kind(&channel) {
        "weixin" => {
            crate::remote::weixin::send_file_now(
                &state.db,
                &remote.weixin,
                &chat_id,
                &file.path.to_string_lossy(),
                None,
            )
            .await
            .map(|_| ())
        }
        "feishu" => {
            let cfg = feishu_cfg(&remote, &channel)?;
            crate::remote::feishu::send_file(&cfg, &chat_id, &file).await
        }
        other => Err(format!("未知渠道: {other}")),
    }?;

    // 4. Freeze the success into the ledger and tell the open panel.
    if let Some(patched) =
        crate::remote::router::patch_attachment_status(&current, attachment_index, "ok", None)
    {
        sqlx::query("UPDATE chat_messages SET attachments = ? WHERE id = ?")
            .bind(&patched)
            .bind(&message_id)
            .execute(&state.db)
            .await
            .map_err(|e| format!("db error: {e}"))?;
        let _ = app.emit(
            "chat:message_updated",
            serde_json::json!({
                "session_id": session_id,
                "message_id": message_id,
                "attachments": patched,
            }),
        );
    }
    Ok(())
}
