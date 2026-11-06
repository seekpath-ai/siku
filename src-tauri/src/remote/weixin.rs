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
            .set_channel_status("weixin", false, "连接中…".to_string(), &app)
            .await;
        match run_monitor(db, &app, &manager, cfg, cancel).await {
            Ok(()) => {
                manager
                    .set_channel_status("weixin", false, "已停止".to_string(), &app)
                    .await
            }
            Err(e) => {
                tracing::error!(error = %e, "weixin channel stopped with error");
                manager
                    .set_channel_status("weixin", false, format!("错误：{e}"), &app)
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
        typing: Arc::new(std::sync::Mutex::new(TypingCache::default())),
        typing_tasks: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
    });
    // The long-poll cursor lives in the synced `remote_weixin_state` row so
    // a standby device taking over resumes where the previous holder left
    // off — no replayed backlog, no missed messages.
    let mut buf = crate::core::settings_service::load_weixin_state(&db)
        .await
        .map(|s| s.get_updates_buf)
        .unwrap_or_default();
    let mut checkpoint = BufCheckpoint::new(&db);
    let mut connected_once = false;

    // Startup grace on takeover: the previous holder's fresher cursor may
    // still be syncing in — polling immediately with our stale local cursor
    // makes the server replay backlog the other device already answered.
    // Give sync a short window to deliver the row. Skipped for a fresh
    // binding (an empty cursor starts from "now", no backlog to replay).
    if !buf.is_empty() {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return Ok(()),
                _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {}
            }
            if adopt_synced_cursor(&db, &mut buf, None).await
                || std::time::Instant::now() >= deadline
            {
                break;
            }
        }
    }

    loop {
        // A fresher cursor may also arrive mid-session (handoff while this
        // device was already polling on a stale one): adopt before polling.
        adopt_synced_cursor(&db, &mut buf, checkpoint.last_persisted()).await;
        let poll = get_updates(&http, &base_url, &cfg.bot_token, &buf);
        let resp = tokio::select! {
            _ = cancel.cancelled() => {
                checkpoint.flush().await;
                return Ok(());
            }
            r = poll => r,
        };
        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                if cancel.is_cancelled() {
                    checkpoint.flush().await;
                    return Ok(());
                }
                tracing::warn!(error = %e, "weixin getupdates failed, retrying in 5s");
                tokio::select! {
                    _ = cancel.cancelled() => {
                        checkpoint.flush().await;
                        return Ok(());
                    }
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
                    .set_channel_status("weixin", 
                        false,
                        "连接已失效，1 小时后自动重试；也可在设置中重新扫码".to_string(),
                        app,
                    )
                    .await;
                tokio::select! {
                    _ = cancel.cancelled() => {
                        checkpoint.flush().await;
                        return Ok(());
                    }
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
                _ = cancel.cancelled() => {
                    checkpoint.flush().await;
                    return Ok(());
                }
                _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
            }
            continue;
        }

        if !connected_once {
            connected_once = true;
            manager
                .set_channel_status("weixin", true, "已连接".to_string(), app)
                .await;
        }

        if let Some(new_buf) = resp.get("get_updates_buf").and_then(|v| v.as_str()) {
            if !new_buf.is_empty() && new_buf != buf {
                buf = new_buf.to_string();
                checkpoint.mark(&buf).await;
            }
        }

        if let Some(msgs) = resp.get("msgs").and_then(|m| m.as_array()) {
            for msg in msgs {
                handle_inbound(&db, app, &sink, msg).await;
            }
        }
    }
}

/// Throttled checkpoint for the long-poll cursor: writing the synced
/// settings row on every poll would spam the CRDT log during active chats,
/// so writes are batched to one per interval and flushed on shutdown.
struct BufCheckpoint<'a> {
    db: &'a sqlx::SqlitePool,
    last: std::time::Instant,
    dirty: Option<String>,
    /// Last cursor value this device persisted. Re-reading the synced row
    /// must distinguish our own writes (ignore) from a previous holder's
    /// (adopt) — cursors are opaque, so this is the only available signal.
    last_persisted: Option<String>,
}

impl BufCheckpoint<'_> {
    const INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);

    fn new(db: &sqlx::SqlitePool) -> BufCheckpoint<'_> {
        BufCheckpoint {
            db,
            last: std::time::Instant::now() - Self::INTERVAL,
            dirty: None,
            last_persisted: None,
        }
    }

    fn last_persisted(&self) -> Option<&str> {
        self.last_persisted.as_deref()
    }

    async fn mark(&mut self, buf: &str) {
        self.dirty = Some(buf.to_string());
        if self.last.elapsed() >= Self::INTERVAL {
            self.flush().await;
        }
    }

    async fn flush(&mut self) {
        if let Some(buf) = self.dirty.take() {
            persist_updates_buf(self.db, &buf).await;
            self.last_persisted = Some(buf);
            self.last = std::time::Instant::now();
        }
    }
}

/// Adopt a cursor that the synced `remote_weixin_state` row holds and that
/// differs from both our in-memory cursor and the last value we persisted
/// ourselves: under single-active arbitration such a value could only have
/// been written by the previous connection holder, so it is fresher.
async fn adopt_synced_cursor(
    db: &sqlx::SqlitePool,
    buf: &mut String,
    last_persisted: Option<&str>,
) -> bool {
    let remote = crate::core::settings_service::load_weixin_state(db)
        .await
        .map(|s| s.get_updates_buf)
        .unwrap_or_default();
    if !remote.is_empty() && remote != *buf && Some(remote.as_str()) != last_persisted {
        tracing::info!("weixin: adopted fresher cursor synced from the previous holder");
        *buf = remote;
        return true;
    }
    false
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
    let mut images: Vec<crate::ai::llm::ImageAttachment> = Vec::new();
    let mut media: Option<crate::remote::router::MediaRef> = None;
    let mut voice_without_text = false;
    if let Some(items) = msg.get("item_list").and_then(|v| v.as_array()) {
        for item in items {
            match item.get("type").and_then(|v| v.as_i64()) {
                Some(1) => {
                    if let Some(t) = item.pointer("/text_item/text").and_then(|v| v.as_str()) {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(t);
                    }
                }
                // VOICE: WeChat transcribes server-side; the result rides in
                // voice_item.text — the same field the official plugin
                // reads. No silk decoding needed for the common case.
                Some(3) => {
                    match item.pointer("/voice_item/text").and_then(|v| v.as_str()) {
                        Some(t) if !t.is_empty() => {
                            if !text.is_empty() {
                                text.push('\n');
                            }
                            text.push_str(t);
                        }
                        _ => voice_without_text = true,
                    }
                }
                // IMAGE: download + AES-128-ECB decrypt, feed the vision turn.
                Some(2) => {
                    let media_ref = item.get("image_item").and_then(|i| i.get("media"));
                    let aeskey = item
                        .pointer("/image_item/aeskey")
                        .and_then(|v| v.as_str());
                    if let Some(m) = media_ref {
                        match cdn_download(m, aeskey).await {
                            Ok(bytes) => images.push(crate::ai::llm::ImageAttachment {
                                mime: sniff_image_mime(&bytes).to_string(),
                                base64: base64::Engine::encode(
                                    &base64::engine::general_purpose::STANDARD,
                                    &bytes,
                                ),
                                name: None,
                            }),
                            Err(e) => {
                                tracing::warn!(error = %e, "weixin image download failed");
                            }
                        }
                    }
                }
                // FILE: download + decrypt, stash under remote_downloads;
                // the router auto-imports PDFs into the library (the saved
                // path rides in MediaRef.file_key) and answers other types
                // with manual-handling guidance.
                Some(4) => {
                    let media_ref = item.get("file_item").and_then(|i| i.get("media"));
                    let name = item
                        .pointer("/file_item/file_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("文件")
                        .to_string();
                    if let Some(m) = media_ref {
                        match cdn_download(m, None).await {
                            Ok(bytes) => {
                                let dir = app.state::<crate::AppState>().app_data_dir.join("remote_downloads");
                                let mut saved_path = String::new();
                                if std::fs::create_dir_all(&dir).is_ok() {
                                    let path = dir.join(format!("{}_{}", uuid::Uuid::new_v4(), name));
                                    if let Err(e) = std::fs::write(&path, &bytes) {
                                        tracing::warn!(error = %e, "weixin file save failed");
                                    } else {
                                        saved_path = path.to_string_lossy().to_string();
                                    }
                                }
                                media = Some(crate::remote::router::MediaRef {
                                    kind: "file".to_string(),
                                    name: name.clone(),
                                    message_id: String::new(),
                                    file_key: saved_path,
                                    extra_image_keys: Vec::new(),
                                });
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "weixin file download failed");
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let text = text.trim().to_string();
    if text.is_empty() && images.is_empty() && media.is_none() {
        if voice_without_text {
            let _ = sink
                .send_text(from, "收到了你的语音，但这条没有转文字结果，我这边无法识别，麻烦再发一次文字～")
                .await;
        }
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

    // Sender-side timestamp for the router's stale-backlog guard. The iLink
    // message shape is not a formal spec, so probe both known spellings and
    // normalize seconds to milliseconds; absent field = guard off.
    let as_i64 = |v: &serde_json::Value| v.as_i64().or_else(|| v.as_str()?.parse().ok());
    let sent_at_ms = msg
        .get("create_time_ms")
        .and_then(as_i64)
        .or_else(|| msg.get("create_time").and_then(as_i64))
        .map(|t| if t < 1_000_000_000_000 { t * 1000 } else { t });

    router::enqueue(
        app.clone(),
        InboundMessage {
            channel: "weixin".to_string(),
            chat_id: from.to_string(),
            kind: ChatKind::Private,
            user_id: from.to_string(),
            user_name: String::new(),
            msg_id,
            sent_at_ms,
            text,
            images,
            media,
        },
        sink.clone(),
    )
    .await;
}

async fn persist_updates_buf(db: &sqlx::SqlitePool, buf: &str) {
    if let Ok(mut state) = crate::core::settings_service::load_weixin_state(db).await {
        if state.get_updates_buf == buf {
            return;
        }
        state.get_updates_buf = buf.to_string();
        let _ = crate::core::settings_service::save_weixin_state(db, &state).await;
    }
}

async fn save_context_token(db: &sqlx::SqlitePool, chat_id: &str, token: &str) {
    if let Ok(mut state) = crate::core::settings_service::load_weixin_state(db).await {
        let map = &mut state.context_tokens;
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
        let _ = crate::core::settings_service::save_weixin_state(db, &state).await;
    }
}

// ---------------------------------------------------------------- replies

struct WeixinSink {
    http: reqwest::Client,
    base_url: String,
    token: String,
    db: sqlx::SqlitePool,
    /// Per-chat typing tickets (in memory only — see `TypingCache`).
    typing: Arc<std::sync::Mutex<TypingCache>>,
    /// Running typing heartbeats, one per chat, aborted when the turn settles.
    typing_tasks:
        Arc<std::sync::Mutex<std::collections::HashMap<String, tokio::task::JoinHandle<()>>>>,
}

#[async_trait::async_trait]
impl ReplySink for WeixinSink {
    async fn send_text(&self, chat_id: &str, text: &str) -> Result<(), String> {
        send_text_raw(&self.http, &self.base_url, &self.token, &self.db, chat_id, text).await
    }

    async fn send_media(
        &self,
        chat_id: &str,
        file: &crate::remote::router::OutboundFile,
    ) -> Result<(), String> {
        send_file(&self.http, &self.base_url, &self.token, &self.db, chat_id, file).await
    }

    async fn resolve_file_media(
        &self,
        media: &crate::remote::router::MediaRef,
    ) -> Result<std::path::PathBuf, String> {
        // Inbound files are written to remote_downloads at receive time;
        // the path rides in file_key.
        if media.file_key.is_empty() {
            return Err("文件保存失败，请重发一次".to_string());
        }
        Ok(std::path::PathBuf::from(&media.file_key))
    }

    /// WeChat has no message-editing surface, so "the bot is working" is
    /// carried by the native typing indicator instead of chat bubbles: fetch
    /// the per-user ticket (cached), set typing immediately, then heartbeat it
    /// every 5s until `progress_finish` cancels it.
    ///
    /// Every failure path is silent on purpose — a bot the server issues no
    /// ticket for must behave exactly like a bot with no typing support, and
    /// never fall back to the old text notices (docs/remote-channels.md §9.4).
    async fn progress_start(
        &self,
        chat_id: &str,
        _preview: &str,
        _session_id: &str,
        _prompt: &str,
        _kind: crate::remote::router::ChatKind,
    ) -> Result<(), String> {
        // Turns are serialized per chat, so any handle here is stale.
        if let Ok(mut tasks) = self.typing_tasks.lock() {
            if let Some(old) = tasks.remove(chat_id) {
                old.abort();
            }
        }
        let http = self.http.clone();
        let base_url = self.base_url.clone();
        let token = self.token.clone();
        let db = self.db.clone();
        let cache = self.typing.clone();
        let chat = chat_id.to_string();
        let handle = tokio::spawn(async move {
            let now = std::time::Instant::now();
            let cached = cache.lock().ok().and_then(|c| c.lookup(&chat, now));
            let ticket = match cached {
                Some(t) => t,
                None => match fetch_typing_ticket(&http, &base_url, &token, &db, &chat).await {
                    Ok(t) => {
                        if let Ok(mut c) = cache.lock() {
                            c.store(&chat, &t, now);
                        }
                        t
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, "typing unavailable: no ticket");
                        return;
                    }
                },
            };
            if ticket.is_empty() {
                tracing::debug!("typing unavailable: empty ticket");
                return;
            }
            let started = std::time::Instant::now();
            loop {
                if send_typing(&http, &base_url, &token, &chat, &ticket, 1)
                    .await
                    .is_err()
                {
                    return;
                }
                if started.elapsed().as_secs() >= TYPING_MAX_SECS {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_secs(TYPING_HEARTBEAT_SECS)).await;
            }
        });
        if let Ok(mut tasks) = self.typing_tasks.lock() {
            tasks.insert(chat_id.to_string(), handle);
        }
        Ok(())
    }

    async fn progress_finish(
        &self,
        _chat_id: &str,
        _reply: &str,
        _ok: bool,
        _progress: &crate::remote::router::RemoteProgress,
    ) -> Result<bool, String> {
        // Typing intentionally keeps running here: the reply text and any
        // outbound file upload still have to happen, and that upload is
        // exactly when the phone needs the "still working" signal. The router
        // calls `progress_settle` when everything is out.
        // WeChat can't edit messages: the reply always goes out as text.
        Ok(false)
    }

    async fn progress_settle(&self, chat_id: &str) {
        if let Ok(mut tasks) = self.typing_tasks.lock() {
            if let Some(handle) = tasks.remove(chat_id) {
                handle.abort();
            }
        }
        let now = std::time::Instant::now();
        let ticket = self.typing.lock().ok().and_then(|c| c.lookup(chat_id, now));
        if let Some(t) = ticket.filter(|t| !t.is_empty()) {
            // Stop the indicator explicitly; if it already expired in the
            // client this is a no-op.
            if let Err(e) =
                send_typing(&self.http, &self.base_url, &self.token, chat_id, &t, 2).await
            {
                tracing::debug!(error = %e, "typing cancel failed");
            }
        }
    }
}

async fn send_text_raw(
    http: &reqwest::Client,
    base_url: &str,
    token: &str,
    db: &sqlx::SqlitePool,
    chat_id: &str,
    text: &str,
) -> Result<(), String> {
    let context_token = crate::core::settings_service::load_weixin_state(db)
        .await
        .ok()
        .and_then(|s| s.context_tokens.get(chat_id).cloned());
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
        // The official endpoint really is "sendmessages" — no trailing
        // s; a 404 here previously swallowed every reply.
        let url = format!(
            "{}/ilink/bot/sendmessage",
            base_url.trim_end_matches('/')
        );
        let resp = http
            .post(&url)
            .headers(common_headers(Some(token)))
            .json(&serde_json::json!({ "msg": msg, "base_info": base_info() }))
            .timeout(std::time::Duration::from_secs(API_TIMEOUT_SECS))
            .send()
            .await
            .map_err(|e| format!("微信发送失败: {e}"))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| format!("微信发送响应读取失败: {e}"))?;
        if !status.is_success() {
            return Err(format!(
                "微信发送失败: HTTP {status} {}",
                body.chars().take(200).collect::<String>()
            ));
        }
        // A 2xx with an empty/non-JSON body has been observed on delivered
        // messages; strict .json() parsing used to fail those sends even
        // though the text reached WeChat.
        let parsed: serde_json::Value = match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(_) => {
                tracing::warn!(
                    body = %body.chars().take(200).collect::<String>(),
                    "weixin sendmessage: non-JSON 2xx body, treating as delivered"
                );
                continue;
            }
        };
        let ret = parsed.get("ret").and_then(|v| v.as_i64()).unwrap_or(0);
        if ret != 0 {
            let errmsg = parsed.get("errmsg").and_then(|v| v.as_str()).unwrap_or("");
            return Err(format!("微信发送失败: ret={ret} {errmsg}"));
        }
    }
    Ok(())
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
                let mut remote =
                    crate::core::settings_service::load_remote_config(&db).await?;
                remote.weixin = WeixinChannelConfig {
                    enabled: true,
                    bot_token,
                    ilink_bot_id,
                    ilink_user_id,
                    baseurl,
                    ..Default::default()
                };
                crate::core::settings_service::save_remote_config(&db, &remote).await?;
                // Fresh binding: the old long-poll cursor and context tokens
                // belong to the replaced session.
                let _ = crate::core::settings_service::save_weixin_state(
                    &db,
                    &Default::default(),
                )
                .await;
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

// ---------------------------------------------------------------- CDN media
// Inbound images/files arrive as CDNMedia references: encrypted bytes on the
// WeChat CDN, AES-128-ECB key in the message. Mirrors the official plugin's
// pic-decrypt.ts.

const CDN_BASE_URL: &str = "https://novac2c.cdn.weixin.qq.com/c2c";

/// encrypt_query_param is base64-ish; encode only what a query value must.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Key material arrives in two encodings (per the official plugin):
/// base64(raw 16 bytes) — images; base64(32-char hex) — file/voice/video.
fn parse_aes_key(b64: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    let decoded = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    if decoded.len() == 16 {
        return Some(decoded);
    }
    if decoded.len() == 32 {
        if let Ok(s) = std::str::from_utf8(&decoded) {
            if let Ok(raw) = hex::decode(s) {
                if raw.len() == 16 {
                    return Some(raw);
                }
            }
        }
    }
    None
}

/// ECB with manual PKCS7 unpad, block-by-block over raw AES-128 (avoids the
/// `ecb` crate's mode trait plumbing for a 16-byte-block scheme).
fn aes_ecb_decrypt(key: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    use aes::cipher::{BlockDecrypt, KeyInit};
    if data.is_empty() || data.len() % 16 != 0 || key.len() != 16 {
        return None;
    }
    let cipher = aes::Aes128::new(key.into());
    let mut out = data.to_vec();
    for chunk in out.chunks_exact_mut(16) {
        cipher.decrypt_block(aes::cipher::generic_array::GenericArray::from_mut_slice(chunk));
    }
    let pad = *out.last()? as usize;
    if pad == 0 || pad > 16 || pad > out.len() {
        return None;
    }
    if out.iter().skip(out.len() - pad).any(|&b| b as usize != pad) {
        return None;
    }
    out.truncate(out.len() - pad);
    Some(out)
}

fn sniff_image_mime(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        "image/png"
    } else if bytes.starts_with(b"GIF8") {
        "image/gif"
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        "image/webp"
    } else {
        "image/jpeg"
    }
}

/// Download (and when keyed, AES-128-ECB decrypt) a CDN media reference.
/// `full_url` wins; otherwise the URL is built from encrypt_query_param.
async fn cdn_download(
    media: &serde_json::Value,
    aeskey_hex: Option<&str>,
) -> Result<Vec<u8>, String> {
    let url = match media.get("full_url").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        Some(u) => u.to_string(),
        None => {
            let eqp = media
                .get("encrypt_query_param")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .ok_or("CDN 媒体缺少 full_url/encrypt_query_param")?;
            format!("{CDN_BASE_URL}/download?encrypted_query_param={}", percent_encode(eqp))
        }
    };
    let resp = reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("CDN 下载失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("CDN 下载失败: HTTP {}", resp.status()));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("CDN 读取失败: {e}"))?
        .to_vec();
    // image_item.aeskey (raw hex) is preferred over media.aes_key (base64).
    let key = aeskey_hex
        .and_then(|h| hex::decode(h).ok())
        .filter(|k| k.len() == 16)
        .or_else(|| media.get("aes_key").and_then(|v| v.as_str()).and_then(parse_aes_key));
    match key {
        Some(k) => aes_ecb_decrypt(&k, &bytes).ok_or_else(|| "CDN 媒体解密失败".to_string()),
        None => Ok(bytes),
    }
}

// ---------------------------------------------------------- outbound media
// Mirrors the official plugin's `src/cdn/upload.ts` + `src/messaging/send-media.ts`:
//   getuploadurl → POST ciphertext to the CDN → sendmessage with a media item.
// Only this file knows the wire details; the router/delivery contract lives in
// router.rs (docs/remote-channels.md §7.2).

/// CDN upload attempts for server-side (5xx/transport) errors. 4xx aborts
/// immediately — retrying a rejected request never helps.
const CDN_UPLOAD_RETRIES: usize = 3;
/// Hard cap on one outbound file; the plugin's default `maxSingleMediaBytes`.
pub const MAX_OUTBOUND_BYTES: u64 = 25 * 1024 * 1024;

fn md5_hex(bytes: &[u8]) -> String {
    use md5::{Digest, Md5};
    let mut hasher = Md5::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// PKCS7 ciphertext length for AES-128-ECB: always pads, so a block-aligned
/// plaintext grows by a full block (mirrors the plugin's `aesEcbPaddedSize`).
fn aes_ecb_padded_len(plaintext_len: usize) -> usize {
    (plaintext_len / 16 + 1) * 16
}

fn aes_ecb_encrypt(key: &[u8], data: &[u8]) -> Result<Vec<u8>, String> {
    use aes::cipher::{BlockEncrypt, KeyInit};
    if key.len() != 16 {
        return Err("AES key must be 16 bytes".to_string());
    }
    let cipher = aes::Aes128::new(key.into());
    let pad = 16 - (data.len() % 16);
    let mut buf = Vec::with_capacity(data.len() + pad);
    buf.extend_from_slice(data);
    buf.extend(std::iter::repeat(pad as u8).take(pad));
    for chunk in buf.chunks_exact_mut(16) {
        cipher.encrypt_block(aes::cipher::generic_array::GenericArray::from_mut_slice(chunk));
    }
    Ok(buf)
}

fn rand_hex(n: usize) -> String {
    use rand::RngCore;
    let mut buf = vec![0u8; n];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

/// Extension → MIME, limited to what WeChat renders sensibly. Unknown types
/// fall back to octet-stream and go out as a plain file attachment.
pub(crate) fn outbound_mime(name: &str) -> &'static str {
    let ext = std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "pdf" => "application/pdf",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "txt" => "text/plain",
        "md" => "text/markdown",
        "csv" => "text/csv",
        "json" => "application/json",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        _ => "application/octet-stream",
    }
}

/// proto: UploadMediaType — 1 image, 2 video, 3 file, 4 voice.
fn upload_media_type(mime: &str) -> i64 {
    if mime.starts_with("image/") {
        1
    } else if mime.starts_with("video/") {
        2
    } else {
        3
    }
}

/// Ask the server for an upload slot, returning `(upload_full_url, upload_param)`
/// — either may be present; the caller prefers the full URL.
async fn get_upload_url(
    http: &reqwest::Client,
    base_url: &str,
    token: &str,
    filekey: &str,
    media_type: i64,
    to_user_id: &str,
    rawsize: u64,
    rawfilemd5: &str,
    filesize: u64,
    aeskey_hex: &str,
) -> Result<(Option<String>, Option<String>), String> {
    let url = format!(
        "{}/ilink/bot/getuploadurl",
        base_url.trim_end_matches('/')
    );
    let body = serde_json::json!({
        "filekey": filekey,
        "media_type": media_type,
        "to_user_id": to_user_id,
        "rawsize": rawsize,
        "rawfilemd5": rawfilemd5,
        "filesize": filesize,
        // No thumbnail: matches the official plugin (`no_need_thumb: true`).
        "no_need_thumb": true,
        "aeskey": aeskey_hex,
        "base_info": base_info(),
    });
    let resp = http
        .post(&url)
        .headers(common_headers(Some(token)))
        .json(&body)
        .timeout(std::time::Duration::from_secs(API_TIMEOUT_SECS))
        .send()
        .await
        .map_err(|e| format!("微信上传凭据请求失败: {e}"))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("微信上传凭据响应读取失败: {e}"))?;
    if !status.is_success() {
        return Err(format!(
            "微信上传凭据请求失败: HTTP {status} {}",
            text.chars().take(200).collect::<String>()
        ));
    }
    let v: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("微信上传凭据响应解析失败: {e}"))?;
    let ret = v.get("ret").and_then(|r| r.as_i64()).unwrap_or(0);
    if ret != 0 {
        let errmsg = v.get("errmsg").and_then(|m| m.as_str()).unwrap_or("");
        return Err(format!("微信上传凭据获取失败: ret={ret} {errmsg}"));
    }
    let full = v
        .get("upload_full_url")
        .and_then(|u| u.as_str())
        .filter(|u| !u.is_empty())
        .map(|u| u.to_string());
    let param = v
        .get("upload_param")
        .and_then(|u| u.as_str())
        .filter(|u| !u.is_empty())
        .map(|u| u.to_string());
    if full.is_none() && param.is_none() {
        return Err("微信未返回上传地址（可能需要更新协议版本）".to_string());
    }
    Ok((full, param))
}

/// POST the ciphertext to the CDN; the `x-encrypted-param` response header is
/// the download parameter that goes into the message item.
async fn cdn_upload(
    http: &reqwest::Client,
    url: &str,
    ciphertext: &[u8],
) -> Result<String, String> {
    let mut last_err = String::new();
    for attempt in 1..=CDN_UPLOAD_RETRIES {
        match http
            .post(url)
            .header("Content-Type", "application/octet-stream")
            .body(ciphertext.to_vec())
            .timeout(std::time::Duration::from_secs(120))
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    let param = resp
                        .headers()
                        .get("x-encrypted-param")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                        .to_string();
                    if !param.is_empty() {
                        return Ok(param);
                    }
                    last_err = "CDN 上传响应缺少 x-encrypted-param".to_string();
                } else {
                    let header_msg = resp
                        .headers()
                        .get("x-error-message")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                        .to_string();
                    last_err = format!("CDN 上传失败: HTTP {status} {header_msg}");
                    // Client errors are terminal: the slot/key is wrong.
                    if status.is_client_error() {
                        return Err(last_err);
                    }
                }
            }
            Err(e) => last_err = format!("CDN 上传失败: {e}"),
        }
        if attempt < CDN_UPLOAD_RETRIES {
            tokio::time::sleep(std::time::Duration::from_millis(500 * attempt as u64)).await;
        }
    }
    Err(last_err)
}

/// Upload one local file and send it to a chat as an image / video / file
/// message. Shared by `ReplySink::send_media` (agent-initiated) and the P0
/// manual smoke-test command.
pub(crate) async fn send_file(
    http: &reqwest::Client,
    base_url: &str,
    token: &str,
    db: &sqlx::SqlitePool,
    chat_id: &str,
    file: &crate::remote::router::OutboundFile,
) -> Result<(), String> {
    let bytes = tokio::fs::read(&file.path)
        .await
        .map_err(|e| format!("读取文件失败（{}）: {e}", file.name))?;
    if bytes.is_empty() {
        return Err("文件为空，未发送".to_string());
    }
    if bytes.len() as u64 > MAX_OUTBOUND_BYTES {
        return Err(format!(
            "文件过大（{}MB，上限 {}MB）",
            bytes.len() / (1024 * 1024),
            MAX_OUTBOUND_BYTES / (1024 * 1024)
        ));
    }

    let rawsize = bytes.len() as u64;
    let rawfilemd5 = md5_hex(&bytes);
    let filesize = aes_ecb_padded_len(bytes.len()) as u64;
    let filekey = rand_hex(16);
    let aeskey = rand_hex(16);
    let media_type = upload_media_type(&file.mime);

    let (full, param) = get_upload_url(
        http, base_url, token, &filekey, media_type, chat_id, rawsize, &rawfilemd5, filesize,
        &aeskey,
    )
    .await?;
    let upload_url = match full {
        Some(u) => u,
        None => {
            let p = param.ok_or_else(|| "微信未返回上传地址".to_string())?;
            format!(
                "{CDN_BASE_URL}/upload?encrypted_query_param={}&filekey={}",
                percent_encode(&p),
                percent_encode(&filekey)
            )
        }
    };

    // The key travels as base64 of its hex spelling (that is what the client
    // expects); our own inbound parser accepts the same encoding.
    let aes_key_b64 = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(aeskey.as_bytes())
    };
    let ciphertext = aes_ecb_encrypt(&hex::decode(&aeskey).map_err(|e| e.to_string())?, &bytes)?;
    let download_param = cdn_upload(http, &upload_url, &ciphertext).await?;

    let media = serde_json::json!({
        "encrypt_query_param": download_param,
        "aes_key": aes_key_b64,
        "encrypt_type": 1,
    });
    let item = match media_type {
        1 => serde_json::json!({
            "type": 2,
            "image_item": { "media": media, "mid_size": filesize },
        }),
        2 => serde_json::json!({
            "type": 5,
            "video_item": { "media": media, "video_size": filesize },
        }),
        _ => serde_json::json!({
            "type": 4,
            // `len` is the PLAINTEXT size, as a string (protocol quirk).
            "file_item": { "media": media, "file_name": file.name, "len": rawsize.to_string() },
        }),
    };

    // Caption first, then the media item — one item per request, like the
    // official plugin (WeChat has no combined text+attachment message).
    if let Some(caption) = file.caption.as_deref().filter(|c| !c.trim().is_empty()) {
        send_text_raw(http, base_url, token, db, chat_id, caption).await?;
    }
    send_message_item(http, base_url, token, db, chat_id, item).await
}

/// POST one `item_list` entry to `sendmessage`.
async fn send_message_item(
    http: &reqwest::Client,
    base_url: &str,
    token: &str,
    db: &sqlx::SqlitePool,
    chat_id: &str,
    item: serde_json::Value,
) -> Result<(), String> {
    let context_token = crate::core::settings_service::load_weixin_state(db)
        .await
        .ok()
        .and_then(|s| s.context_tokens.get(chat_id).cloned());
    let mut msg = serde_json::json!({
        "from_user_id": "",
        "to_user_id": chat_id,
        "client_id": uuid::Uuid::new_v4().to_string(),
        "message_type": 2,   // BOT
        "message_state": 2,  // FINISH
        "item_list": [item],
    });
    if let Some(t) = &context_token {
        msg["context_token"] = serde_json::Value::String(t.clone());
    }
    let url = format!(
        "{}/ilink/bot/sendmessage",
        base_url.trim_end_matches('/')
    );
    let resp = http
        .post(&url)
        .headers(common_headers(Some(token)))
        .json(&serde_json::json!({ "msg": msg, "base_info": base_info() }))
        .timeout(std::time::Duration::from_secs(API_TIMEOUT_SECS))
        .send()
        .await
        .map_err(|e| format!("微信发送失败: {e}"))?;
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| format!("微信发送响应读取失败: {e}"))?;
    if !status.is_success() {
        return Err(format!(
            "微信发送失败: HTTP {status} {}",
            body.chars().take(200).collect::<String>()
        ));
    }
    let parsed: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        // A 2xx with an empty/non-JSON body has been observed on delivered
        // messages; treat it as delivered (same tolerance as send_text_raw).
        Err(_) => return Ok(()),
    };
    let ret = parsed.get("ret").and_then(|v| v.as_i64()).unwrap_or(0);
    if ret != 0 {
        let errmsg = parsed.get("errmsg").and_then(|v| v.as_str()).unwrap_or("");
        return Err(format!("微信发送失败: ret={ret} {errmsg}"));
    }
    Ok(())
}

/// P0 smoke test: send one local file over the stored binding without going
/// through an agent turn. Used by the `remote_weixin_send_file` command so the
/// protocol can be verified on a real device before the tool exists.
pub(crate) async fn send_file_now(
    db: &sqlx::SqlitePool,
    cfg: &crate::core::models::WeixinChannelConfig,
    chat_id: &str,
    file_path: &str,
    caption: Option<String>,
) -> Result<String, String> {
    if cfg.bot_token.is_empty() {
        return Err("微信渠道未登录，请先在设置中扫码".to_string());
    }
    let base_url = if cfg.baseurl.is_empty() {
        FIXED_BASE_URL.to_string()
    } else {
        cfg.baseurl.clone()
    };
    let path = std::path::PathBuf::from(file_path);
    let meta = std::fs::metadata(&path).map_err(|e| format!("文件不可读: {e}"))?;
    if !meta.is_file() {
        return Err("不是文件".to_string());
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "文件".to_string());
    let file = crate::remote::router::OutboundFile {
        mime: outbound_mime(&name).to_string(),
        sha256: String::new(),
        size: meta.len(),
        caption,
        name: name.clone(),
        path,
    };
    send_file(&reqwest::Client::new(), &base_url, &cfg.bot_token, db, chat_id, &file).await?;
    Ok(format!("已发送 {name}（{} 字节）", meta.len()))
}

// ------------------------------------------------------------- typing state
// WeChat's only zero-noise "the bot is working" signal: POST /ilink/bot/sendtyping
// with a per-user `typing_ticket` from /ilink/bot/getconfig. The client clears
// the indicator after a few seconds, so it must be re-sent while the turn runs
// (the official plugin heartbeats it every 5s). Mirrors docs/remote-channels.md §9.

/// Heartbeat cadence for the typing indicator.
const TYPING_HEARTBEAT_SECS: u64 = 5;
/// How long a fetched ticket is reused before asking again. The plugin
/// refreshes randomly within 24h; 12h bounded is plenty and costs one cheap
/// request per chat per half-day.
const TYPING_TICKET_TTL_SECS: u64 = 12 * 3600;
/// Cap on cached tickets (bounded like the context tokens).
const TYPING_TICKET_CAP: usize = 200;
/// Hard stop for a heartbeat whose settle never arrives (turn timeout is
/// 600s; this is the same order of magnitude and just prevents an orphan).
const TYPING_MAX_SECS: u64 = 900;

/// Per-chat typing tickets. In-memory on purpose: a ticket is worthless to
/// another device and re-fetching after a restart is one request, so it must
/// not add writes to the synced state row.
#[derive(Default)]
struct TypingCache {
    tickets: std::collections::HashMap<String, (String, std::time::Instant)>,
}

impl TypingCache {
    /// A non-empty ticket younger than the TTL, if any. An empty cached ticket
    /// means "this bot gets no typing support" — also a valid cache hit, so a
    /// server that never issues tickets is not asked on every turn.
    fn lookup(&self, chat_id: &str, now: std::time::Instant) -> Option<String> {
        let (ticket, at) = self.tickets.get(chat_id)?;
        if now.duration_since(*at).as_secs() >= TYPING_TICKET_TTL_SECS {
            return None;
        }
        Some(ticket.clone())
    }

    fn store(&mut self, chat_id: &str, ticket: &str, now: std::time::Instant) {
        if self.tickets.len() >= TYPING_TICKET_CAP && !self.tickets.contains_key(chat_id) {
            if let Some(k) = self.tickets.keys().next().cloned() {
                self.tickets.remove(&k);
            }
        }
        self.tickets
            .insert(chat_id.to_string(), (ticket.to_string(), now));
    }
}

/// Fetch the typing ticket for one user (`ilink/bot/getconfig`).
async fn fetch_typing_ticket(
    http: &reqwest::Client,
    base_url: &str,
    token: &str,
    db: &sqlx::SqlitePool,
    chat_id: &str,
) -> Result<String, String> {
    let context_token = crate::core::settings_service::load_weixin_state(db)
        .await
        .ok()
        .and_then(|s| s.context_tokens.get(chat_id).cloned());
    let mut body = serde_json::json!({
        "ilink_user_id": chat_id,
        "base_info": base_info(),
    });
    if let Some(t) = context_token.filter(|t| !t.is_empty()) {
        body["context_token"] = serde_json::Value::String(t);
    }
    let url = format!("{}/ilink/bot/getconfig", base_url.trim_end_matches('/'));
    let resp = http
        .post(&url)
        .headers(common_headers(Some(token)))
        .json(&body)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("getconfig 请求失败: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| format!("getconfig 响应读取失败: {e}"))?;
    if !status.is_success() {
        return Err(format!("getconfig 失败: HTTP {status}"));
    }
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("getconfig 响应解析失败: {e}"))?;
    let ret = v.get("ret").and_then(|r| r.as_i64()).unwrap_or(0);
    if ret != 0 {
        let errmsg = v.get("errmsg").and_then(|m| m.as_str()).unwrap_or("");
        return Err(format!("getconfig ret={ret} {errmsg}"));
    }
    Ok(v
        .get("typing_ticket")
        .and_then(|t| t.as_str())
        .unwrap_or_default()
        .to_string())
}

/// Set or clear the typing indicator (`ilink/bot/sendtyping`).
/// `status`: 1 = typing, 2 = cancel.
async fn send_typing(
    http: &reqwest::Client,
    base_url: &str,
    token: &str,
    chat_id: &str,
    ticket: &str,
    status: i64,
) -> Result<(), String> {
    let url = format!("{}/ilink/bot/sendtyping", base_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "ilink_user_id": chat_id,
        "typing_ticket": ticket,
        "status": status,
        "base_info": base_info(),
    });
    let resp = http
        .post(&url)
        .headers(common_headers(Some(token)))
        .json(&body)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("sendtyping 请求失败: {e}"))?;
    let status_code = resp.status();
    if !status_code.is_success() {
        return Err(format!("sendtyping 失败: HTTP {status_code}"));
    }
    let text = resp
        .text()
        .await
        .map_err(|e| format!("sendtyping 响应读取失败: {e}"))?;
    // An empty body is a delivered typing state (same tolerance as sendmessage).
    if text.trim().is_empty() {
        return Ok(());
    }
    let v: serde_json::Value = serde_json::from_str(&text)
        .map_err(|_| format!("sendtyping 响应解析失败: {}", text.chars().take(120).collect::<String>()))?;
    let ret = v.get("ret").and_then(|r| r.as_i64()).unwrap_or(0);
    if ret != 0 {
        let errmsg = v.get("errmsg").and_then(|m| m.as_str()).unwrap_or("");
        return Err(format!("sendtyping ret={ret} {errmsg}"));
    }
    Ok(())
}

#[cfg(test)]
mod typing_tests {
    use super::{TypingCache, TYPING_TICKET_TTL_SECS};
    use std::time::{Duration, Instant};

    #[test]
    fn cache_hits_reuse_the_ticket_until_the_ttl_expires() {
        let mut cache = TypingCache::default();
        let now = Instant::now();
        cache.store("u1", "ticket-a", now);
        assert_eq!(cache.lookup("u1", now).as_deref(), Some("ticket-a"));

        let almost = now + Duration::from_secs(TYPING_TICKET_TTL_SECS - 1);
        assert_eq!(cache.lookup("u1", almost).as_deref(), Some("ticket-a"));

        let expired = now + Duration::from_secs(TYPING_TICKET_TTL_SECS);
        assert_eq!(cache.lookup("u1", expired), None);
        assert_eq!(cache.lookup("other", now), None);
    }

    /// A bot the server issues no ticket for caches an empty value: the cache
    /// still answers, so every turn does not re-ask (and typing stays off).
    #[test]
    fn empty_ticket_is_cached_as_unsupported() {
        let mut cache = TypingCache::default();
        let now = Instant::now();
        cache.store("u1", "", now);
        assert_eq!(cache.lookup("u1", now).as_deref(), Some(""));
    }
}
