//! WeChat ClawBot channel over the official iLink protocol: QR login +
//! outbound long-poll `getupdates` + REST `sendmessages`. Protocol mirrors
//! Tencent's `@tencent-weixin/openclaw-weixin` plugin (see
//! docs/remote-channels.md); it is not a formally stabilized API, so all
//! version constants live here and auth failures degrade to "re-scan".

use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager};
use tokio_util::sync::CancellationToken;

use crate::core::models::WeixinChannelConfig;
use crate::remote::router::{self, ChatKind, InboundMessage, ReplySink};
use crate::remote::RemoteManager;

const FIXED_BASE_URL: &str = "https://ilinkai.weixin.qq.com";
const BOT_TYPE: &str = "3";
/// Mirrors the plugin's package.json `ilink_appid` field.
const ILINK_APP_ID: &str = "bot";
/// Plugin version we mirror in base_info / client-version header.
const MIRROR_VERSION: &str = "2.4.10";
const LONG_POLL_TIMEOUT_SECS: u64 = 35;
const API_TIMEOUT_SECS: u64 = 15;
/// Server-side cooldown after errcode -14 (stale token), per the plugin's
/// session-guard.
const STALE_TOKEN_ERRCODE: i64 = -14;
const STALE_BACKOFF_SECS: u64 = 3600;
/// Text chunk size for outbound messages.
const SEND_CHUNK: usize = 2000;
/// Cap on persisted per-conversation context tokens.
const CONTEXT_TOKEN_CAP: usize = 200;

fn client_version_u32() -> u32 {
    let mut parts = MIRROR_VERSION.split('.').filter_map(|p| p.parse::<u32>().ok());
    let major = parts.next().unwrap_or(0) & 0xff;
    let minor = parts.next().unwrap_or(0) & 0xff;
    let patch = parts.next().unwrap_or(0) & 0xff;
    (major << 16) | (minor << 8) | patch
}

fn base_info() -> serde_json::Value {
    serde_json::json!({
        "channel_version": MIRROR_VERSION,
        "bot_agent": format!("siku/{}", env!("CARGO_PKG_VERSION")),
    })
}

fn random_wechat_uin() -> String {
    use base64::Engine;
    let v: u32 = rand::random();
    base64::engine::general_purpose::STANDARD.encode(v.to_string())
}

fn common_headers(token: Option<&str>) -> reqwest::header::HeaderMap {
    use reqwest::header::{HeaderMap, HeaderValue};
    let mut h = HeaderMap::new();
    h.insert("iLink-App-Id", HeaderValue::from_static(ILINK_APP_ID));
    h.insert(
        "iLink-App-ClientVersion",
        HeaderValue::from(client_version_u32()),
    );
    if let Some(t) = token.filter(|t| !t.is_empty()) {
        h.insert("Content-Type", HeaderValue::from_static("application/json"));
        h.insert("AuthorizationType", HeaderValue::from_static("ilink_bot_token"));
        if let Ok(v) = HeaderValue::from_str(&format!("Bearer {t}")) {
            h.insert("Authorization", v);
        }
        if let Ok(v) = HeaderValue::from_str(&random_wechat_uin()) {
            h.insert("X-WECHAT-UIN", v);
        }
    }
    h
}

// ---------------------------------------------------------------- monitor

pub fn spawn(
    db: sqlx::SqlitePool,
    app: AppHandle,
    manager: RemoteManager,
    cfg: WeixinChannelConfig,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        manager
            .set_weixin_status(false, "连接中…".to_string(), &app)
            .await;
        match run_monitor(db, &app, &manager, cfg, cancel).await {
            Ok(()) => {
                manager
                    .set_weixin_status(false, "已停止".to_string(), &app)
                    .await
            }
            Err(e) => {
                tracing::error!(error = %e, "weixin channel stopped with error");
                manager
                    .set_weixin_status(false, format!("错误：{e}"), &app)
                    .await
            }
        }
    });
}

async fn run_monitor(
    db: sqlx::SqlitePool,
    app: &AppHandle,
    manager: &RemoteManager,
    cfg: WeixinChannelConfig,
    cancel: CancellationToken,
) -> Result<(), String> {
    let http = reqwest::Client::new();
    let base_url = if cfg.baseurl.is_empty() {
        FIXED_BASE_URL.to_string()
    } else {
        cfg.baseurl.clone()
    };
    let sink: Arc<dyn ReplySink> = Arc::new(WeixinSink {
        http: http.clone(),
        base_url: base_url.clone(),
        token: cfg.bot_token.clone(),
        db: db.clone(),
    });
    let mut buf = cfg.get_updates_buf.clone();
    let mut connected_once = false;

    loop {
        let poll = get_updates(&http, &base_url, &cfg.bot_token, &buf);
        let resp = tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            r = poll => r,
        };
        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                if cancel.is_cancelled() {
                    return Ok(());
                }
                tracing::warn!(error = %e, "weixin getupdates failed, retrying in 5s");
                tokio::select! {
                    _ = cancel.cancelled() => return Ok(()),
                    _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
                }
                continue;
            }
        };

        let ret = resp.get("ret").and_then(|v| v.as_i64()).unwrap_or(0);
        if ret != 0 {
            let errcode = resp.get("errcode").and_then(|v| v.as_i64()).unwrap_or(0);
            if errcode == STALE_TOKEN_ERRCODE {
                manager
                    .set_weixin_status(
                        false,
                        "连接已失效，1 小时后自动重试；也可在设置中重新扫码".to_string(),
                        app,
                    )
                    .await;
                tokio::select! {
                    _ = cancel.cancelled() => return Ok(()),
                    _ = tokio::time::sleep(std::time::Duration::from_secs(STALE_BACKOFF_SECS)) => {}
                }
                continue;
            }
            let errmsg = resp
                .get("errmsg")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            tracing::warn!(ret, errmsg, "weixin getupdates error, retrying in 5s");
            tokio::select! {
                _ = cancel.cancelled() => return Ok(()),
                _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
            }
            continue;
        }

        if !connected_once {
            connected_once = true;
            manager
                .set_weixin_status(true, "已连接".to_string(), app)
                .await;
        }

        if let Some(new_buf) = resp.get("get_updates_buf").and_then(|v| v.as_str()) {
            if !new_buf.is_empty() && new_buf != buf {
                buf = new_buf.to_string();
                persist_updates_buf(&db, &buf).await;
            }
        }

        if let Some(msgs) = resp.get("msgs").and_then(|m| m.as_array()) {
            for msg in msgs {
                handle_inbound(&db, app, &sink, msg).await;
            }
        }
    }
}

async fn get_updates(
    http: &reqwest::Client,
    base_url: &str,
    token: &str,
    buf: &str,
) -> Result<serde_json::Value, String> {
    let url = format!("{}/ilink/bot/getupdates", base_url.trim_end_matches('/'));
    let resp = http
        .post(&url)
        .headers(common_headers(Some(token)))
        .json(&serde_json::json!({
            "get_updates_buf": buf,
            "base_info": base_info(),
        }))
        .timeout(std::time::Duration::from_secs(LONG_POLL_TIMEOUT_SECS))
        .send()
        .await;
    match resp {
        Ok(r) => r
            .json::<serde_json::Value>()
            .await
            .map_err(|e| format!("getupdates 响应解析失败: {e}")),
        Err(e) if e.is_timeout() => Ok(serde_json::json!({"ret": 0, "msgs": []})),
        Err(e) => Err(format!("getupdates 请求失败: {e}")),
    }
}

async fn handle_inbound(
    db: &sqlx::SqlitePool,
    app: &AppHandle,
    sink: &Arc<dyn ReplySink>,
    msg: &serde_json::Value,
) {
    // 1 = USER; skip our own BOT echoes and system messages.
    if msg.get("message_type").and_then(|v| v.as_i64()) != Some(1) {
        return;
    }
    let from = msg
        .get("from_user_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if from.is_empty() {
        return;
    }
    // Per-conversation token required by the send API; learn it from inbound.
    if let Some(token) = msg.get("context_token").and_then(|v| v.as_str()) {
        if !token.is_empty() {
            save_context_token(db, from, token).await;
        }
    }
    let mut text = String::new();
    if let Some(items) = msg.get("item_list").and_then(|v| v.as_array()) {
        for item in items {
            if item.get("type").and_then(|v| v.as_i64()) == Some(1) {
                if let Some(t) = item.pointer("/text_item/text").and_then(|v| v.as_str()) {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(t);
                }
            }
        }
    }
    let text = text.trim().to_string();
    if text.is_empty() {
        return;
    }
    let msg_id = msg
        .get("message_id")
        .map(|v| match v {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .filter(|s| !s.is_empty() && s != "0")
        .or_else(|| {
            msg.get("client_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_default();

    router::enqueue(
        app.clone(),
        InboundMessage {
            channel: "weixin",
            chat_id: from.to_string(),
            kind: ChatKind::Private,
            user_id: from.to_string(),
            user_name: String::new(),
            msg_id,
            text,
        },
        sink.clone(),
    )
    .await;
}

async fn persist_updates_buf(db: &sqlx::SqlitePool, buf: &str) {
    if let Ok(mut settings) = crate::core::settings_service::load_device_settings(db).await {
        settings.remote.weixin.get_updates_buf = buf.to_string();
        let _ = crate::core::settings_service::save_device_settings(db, &settings).await;
    }
}

async fn save_context_token(db: &sqlx::SqlitePool, chat_id: &str, token: &str) {
    if let Ok(mut settings) = crate::core::settings_service::load_device_settings(db).await {
        let map = &mut settings.remote.weixin.context_tokens;
        if map.get(chat_id) == Some(&token.to_string()) {
            return;
        }
        map.insert(chat_id.to_string(), token.to_string());
        while map.len() > CONTEXT_TOKEN_CAP {
            if let Some(k) = map.keys().next().cloned() {
                map.remove(&k);
            } else {
                break;
            }
        }
        let _ = crate::core::settings_service::save_device_settings(db, &settings).await;
    }
}

// ---------------------------------------------------------------- replies

struct WeixinSink {
    http: reqwest::Client,
    base_url: String,
    token: String,
    db: sqlx::SqlitePool,
}

#[async_trait::async_trait]
impl ReplySink for WeixinSink {
    async fn send_text(&self, chat_id: &str, text: &str) -> Result<(), String> {
        let context_token = crate::core::settings_service::load_device_settings(&self.db)
            .await
            .ok()
            .and_then(|s| s.remote.weixin.context_tokens.get(chat_id).cloned());
        for chunk in chunk_text(text, SEND_CHUNK) {
            let mut msg = serde_json::json!({
                "from_user_id": "",
                "to_user_id": chat_id,
                "client_id": uuid::Uuid::new_v4().to_string(),
                "message_type": 2, // BOT
                "message_state": 2, // FINISH
                "item_list": [{ "type": 1, "text_item": { "text": chunk } }],
            });
            if let Some(t) = &context_token {
                msg["context_token"] = serde_json::Value::String(t.clone());
            }
            let url = format!(
                "{}/ilink/bot/sendmessages",
                self.base_url.trim_end_matches('/')
            );
            let resp = self
                .http
                .post(&url)
                .headers(common_headers(Some(&self.token)))
                .json(&serde_json::json!({ "msg": msg, "base_info": base_info() }))
                .timeout(std::time::Duration::from_secs(API_TIMEOUT_SECS))
                .send()
                .await
                .map_err(|e| format!("微信发送失败: {e}"))?;
            let body: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| format!("微信发送响应解析失败: {e}"))?;
            let ret = body.get("ret").and_then(|v| v.as_i64()).unwrap_or(0);
            if ret != 0 {
                let errmsg = body.get("errmsg").and_then(|v| v.as_str()).unwrap_or("");
                return Err(format!("微信发送失败: ret={ret} {errmsg}"));
            }
        }
        Ok(())
    }
}

fn chunk_text(text: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if current.len() + ch.len_utf8() > max && !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
        current.push(ch);
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

// ---------------------------------------------------------------- QR login

/// Login task state shared with the verify-code command.
pub struct LoginState {
    pub verify_code: tokio::sync::Mutex<Option<String>>,
}

/// QR payload pushed to the settings UI as `remote:weixin-qr`.
#[derive(Clone, serde::Serialize)]
pub struct QrEvent {
    /// QR content URL to render; None clears the display.
    pub url: Option<String>,
    /// Status line ("已扫描，等待确认…" / error) for the UI.
    pub message: Option<String>,
    /// Server asks for the digit code shown in WeChat.
    pub need_verify: bool,
}

fn emit_qr(app: &AppHandle, url: Option<String>, message: Option<String>, need_verify: bool) {
    let _ = app.emit("remote:weixin-qr", QrEvent { url, message, need_verify });
}

async fn fetch_qr(http: &reqwest::Client) -> Result<(String, String), String> {
    let url = format!("{FIXED_BASE_URL}/ilink/bot/get_bot_qrcode?bot_type={BOT_TYPE}");
    let resp: serde_json::Value = http
        .post(&url)
        .headers(common_headers(None))
        .json(&serde_json::json!({ "local_token_list": [] }))
        .timeout(std::time::Duration::from_secs(API_TIMEOUT_SECS))
        .send()
        .await
        .map_err(|e| format!("获取二维码失败: {e}"))?
        .json()
        .await
        .map_err(|e| format!("二维码响应解析失败: {e}"))?;
    let qrcode = resp
        .get("qrcode")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("获取二维码失败: {resp}"))?
        .to_string();
    let img = resp
        .get("qrcode_img_content")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    Ok((qrcode, img))
}

async fn poll_qr_status(
    http: &reqwest::Client,
    base_url: &str,
    qrcode: &str,
    verify_code: Option<&str>,
) -> serde_json::Value {
    let mut url = format!(
        "{}/ilink/bot/get_qrcode_status?qrcode={}",
        base_url.trim_end_matches('/'),
        urlencoding(qrcode)
    );
    if let Some(code) = verify_code {
        url.push_str(&format!("&verify_code={}", urlencoding(code)));
    }
    let resp = http
        .get(&url)
        .headers(common_headers(None))
        .timeout(std::time::Duration::from_secs(LONG_POLL_TIMEOUT_SECS))
        .send()
        .await;
    match resp {
        Ok(r) => r
            .json::<serde_json::Value>()
            .await
            .unwrap_or_else(|_| serde_json::json!({"status": "wait"})),
        Err(_) => serde_json::json!({"status": "wait"}),
    }
}

fn urlencoding(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

const MAX_QR_REFRESH: u32 = 3;
const LOGIN_TIMEOUT_SECS: u64 = 480;

/// Run the QR login flow in the background. QR images and status lines reach
/// the settings UI via `remote:weixin-qr`; on success the credentials are
/// persisted and `RemoteManager::sync_config` starts the monitor.
pub fn spawn_login(app: AppHandle, manager: RemoteManager, login: Arc<LoginState>) {
    tokio::spawn(async move {
        if let Err(e) = run_login(&app, &manager, &login).await {
            emit_qr(&app, None, Some(e), false);
        }
        manager.clear_weixin_login().await;
    });
}

async fn run_login(
    app: &AppHandle,
    manager: &RemoteManager,
    login: &Arc<LoginState>,
) -> Result<(), String> {
    let http = reqwest::Client::new();
    let db = app.state::<crate::AppState>().db.clone();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(LOGIN_TIMEOUT_SECS);

    let (mut qrcode, mut img) = fetch_qr(&http).await?;
    emit_qr(app, Some(img.clone()), Some("请用微信扫描二维码".to_string()), false);
    let mut base_url = FIXED_BASE_URL.to_string();
    let mut refreshes = 0u32;
    let mut scanned = false;

    while std::time::Instant::now() < deadline {
        let code = login.verify_code.lock().await.take();
        let status = poll_qr_status(&http, &base_url, &qrcode, code.as_deref()).await;
        match status.get("status").and_then(|v| v.as_str()).unwrap_or("wait") {
            "wait" => {}
            "scaned" => {
                if code.is_some() {
                    // Verify code accepted; already consumed via take().
                }
                if !scanned {
                    scanned = true;
                    emit_qr(app, None, Some("已扫描，请在微信中确认".to_string()), false);
                }
            }
            "need_verifycode" => {
                emit_qr(
                    app,
                    None,
                    Some("请输入微信中显示的数字验证码".to_string()),
                    true,
                );
            }
            "expired" | "verify_code_blocked" => {
                refreshes += 1;
                if refreshes > MAX_QR_REFRESH {
                    return Err("二维码多次失效，请重新发起连接".to_string());
                }
                let (q, i) = fetch_qr(&http).await?;
                qrcode = q;
                img = i;
                scanned = false;
                emit_qr(
                    app,
                    Some(img.clone()),
                    Some("二维码已刷新，请重新扫描".to_string()),
                    false,
                );
            }
            "binded_redirect" => {
                return Err("该微信已连接过此应用，无需重复连接".to_string());
            }
            "scaned_but_redirect" => {
                if let Some(host) = status.get("redirect_host").and_then(|v| v.as_str()) {
                    base_url = format!("https://{host}");
                }
            }
            "confirmed" => {
                let bot_token = status
                    .get("bot_token")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let ilink_bot_id = status
                    .get("ilink_bot_id")
                    .and_then(|v| v.as_str())
                    .ok_or("登录失败：服务器未返回 ilink_bot_id")?
                    .to_string();
                let ilink_user_id = status
                    .get("ilink_user_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let baseurl = status
                    .get("baseurl")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                if bot_token.is_empty() {
                    return Err("登录失败：服务器未返回 bot_token".to_string());
                }
                let mut settings =
                    crate::core::settings_service::load_device_settings(&db).await?;
                settings.remote.weixin = WeixinChannelConfig {
                    enabled: true,
                    bot_token,
                    ilink_bot_id,
                    ilink_user_id,
                    baseurl,
                    ..settings.remote.weixin
                };
                crate::core::settings_service::save_device_settings(&db, &settings).await?;
                emit_qr(app, None, Some("已连接微信".to_string()), false);
                manager.sync_config(&db, app).await;
                return Ok(());
            }
            other => {
                tracing::warn!(status = other, "weixin login: unknown qr status");
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    Err("登录超时，请重新发起".to_string())
}
