//! Feishu (Lark) channel: outbound WebSocket long connection via the
//! community `feishu-sdk` crate. No public webhook endpoint is required.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use feishu_sdk::core::{new_logger, Config, LogLevel, RequestOptions};
use feishu_sdk::event::{
    Event, EventDispatcher, EventDispatcherConfig, EventHandler, EventHandlerResult,
};
use feishu_sdk::ws::StreamClient;
use tauri::{AppHandle, Manager as _};

use crate::core::models::FeishuBotConfig;
use crate::remote::router::{self, ChatKind, InboundMessage, ReplySink};
use crate::remote::RemoteManager;

/// Spawn the supervisor task for the feishu channel. Status transitions are
/// reported through the manager (and onward to the settings UI).
pub fn spawn(
    db: sqlx::SqlitePool,
    app: AppHandle,
    manager: RemoteManager,
    cfg: FeishuBotConfig,
    cancel: tokio_util::sync::CancellationToken,
) {
    tokio::spawn(async move {
        let status_key = format!("feishu:{}", cfg.app_id);
        manager
            .set_channel_status(&status_key, false, "连接中…".to_string(), &app)
            .await;
        match run(db, &app, &manager, &cfg, cancel).await {
            Ok(()) => {
                manager
                    .set_channel_status(&status_key, false, "已停止".to_string(), &app)
                    .await
            }
            Err(e) => {
                tracing::error!(error = %e, app_id = %cfg.app_id, "feishu channel stopped with error");
                manager
                    .set_channel_status(&status_key, false, format!("错误：{e}"), &app)
                    .await
            }
        }
    });
}

async fn run(
    _db: sqlx::SqlitePool,
    app: &AppHandle,
    manager: &RemoteManager,
    cfg: &FeishuBotConfig,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<(), String> {
    // Validate credentials and learn the bot identity (needed to detect
    // @mentions in groups and to label sessions).
    let bot = fetch_bot_info(cfg).await?;

    let config = Config::builder(cfg.app_id.clone(), cfg.app_secret.clone())
        .log_level(LogLevel::Warn)
        .build();
    let logger = new_logger(LogLevel::Warn);

    let http_client = feishu_sdk::Client::builder(config.clone())
        .build()
        .map_err(|e| format!("feishu client init failed: {e}"))?;
    let cards: CardRegistry = Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
    let sink: Arc<dyn ReplySink> = Arc::new(FeishuSink {
        client: http_client,
        cfg: cfg.clone(),
        cards: cards.clone(),
    });

    let dispatcher = EventDispatcher::new(EventDispatcherConfig::new(), logger);
    dispatcher
        .register_handler(Box::new(FeishuMessageHandler {
            app: app.clone(),
            sink: sink.clone(),
            bot_open_id: bot.open_id.clone(),
            cfg: cfg.clone(),
            instance_channel: format!("feishu:{}", cfg.app_id),
        }))
        .await;
    // Card button callbacks (停止/重试/思考过程). Requires the
    // "卡片回传交互 card.action.trigger" event subscription in the app
    // console; without it the buttons simply do nothing.
    dispatcher
        .register_handler(Box::new(FeishuCardActionHandler {
            app: app.clone(),
            cfg: cfg.clone(),
            cards,
            sink: sink.clone(),
            instance_channel: format!("feishu:{}", cfg.app_id),
        }))
        .await;

    let stream = StreamClient::builder(config)
        .event_dispatcher(dispatcher)
        .build()
        .map_err(|e| format!("feishu stream init failed: {e}"))?;

    manager
        .set_channel_status(
            &format!("feishu:{}", cfg.app_id),
            true,
            format!("已连接（{}）", bot.app_name),
            app,
        )
        .await;

    let mut handle = stream.spawn();
    tokio::select! {
        _ = cancel.cancelled() => {
            handle.abort();
            Ok(())
        }
        result = &mut handle => {
            match result {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(format!("连接中断：{e}")),
                Err(e) => Err(format!("任务异常：{e}")),
            }
        }
    }
}

struct BotInfo {
    open_id: String,
    app_name: String,
}

/// tenant_access_token, split out of fetch_bot_info so name resolution can
/// reuse it.
pub(crate) async fn tenant_token(cfg: &FeishuBotConfig) -> Result<String, String> {
    let http = reqwest::Client::new();
    let token_resp: serde_json::Value = http
        .post("https://open.feishu.cn/open-apis/auth/v3/tenant_access_token/internal")
        .json(&serde_json::json!({
            "app_id": cfg.app_id,
            "app_secret": cfg.app_secret,
        }))
        .send()
        .await
        .map_err(|e| format!("飞书令牌请求失败: {e}"))?
        .json()
        .await
        .map_err(|e| format!("飞书令牌响应解析失败: {e}"))?;
    token_resp
        .get("tenant_access_token")
        .and_then(|t| t.as_str())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .ok_or_else(|| format!("飞书鉴权失败（请检查 App ID / App Secret）：{}", token_resp))
}

/// tenant_access_token → bot/v3/info. Two tiny REST calls; the SDK keeps its
/// token handling internal, so we do this directly.
async fn fetch_bot_info(cfg: &FeishuBotConfig) -> Result<BotInfo, String> {
    let token = tenant_token(cfg).await?;
    let http = reqwest::Client::new();
    let info: serde_json::Value = http
        .get("https://open.feishu.cn/open-apis/bot/v3/info")
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("飞书机器人信息请求失败: {e}"))?
        .json()
        .await
        .map_err(|e| format!("飞书机器人信息解析失败: {e}"))?;
    let open_id = info
        .pointer("/bot/open_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let app_name = info
        .pointer("/bot/app_name")
        .and_then(|v| v.as_str())
        .unwrap_or("飞书机器人")
        .to_string();
    Ok(BotInfo { open_id, app_name })
}

// ---------------------------------------------------------------- messages

struct FeishuMessageHandler {
    app: AppHandle,
    sink: Arc<dyn ReplySink>,
    bot_open_id: String,
    cfg: FeishuBotConfig,
    /// "feishu:<app_id>" — instance-scoped channel key for routing/pairing.
    instance_channel: String,
}

impl EventHandler for FeishuMessageHandler {
    fn event_type(&self) -> &str {
        "im.message.receive_v1"
    }

    fn handle(&self, event: Event) -> Pin<Box<dyn Future<Output = EventHandlerResult> + Send + '_>> {
        Box::pin(async move {
            let Some(payload) = event.event else {
                tracing::warn!("feishu event without payload");
                return Ok(None);
            };
            tracing::info!(
                chat_type = payload.pointer("/message/chat_type").and_then(|v| v.as_str()).unwrap_or("?"),
                message_type = payload.pointer("/message/message_type").and_then(|v| v.as_str()).unwrap_or("?"),
                sender_type = payload.pointer("/sender/sender_type").and_then(|v| v.as_str()).unwrap_or("?"),
                "feishu message event"
            );
            match parse_inbound(&payload, &self.bot_open_id, &self.instance_channel) {
                Ok(Some(mut inbound)) => {
                    // Image messages: fetch the attachment now so the agent
                    // turn gets real pixels. Files stay as references — the
                    // router answers them with guidance instead.
                    let is_image = inbound
                        .media
                        .as_ref()
                        .map(|m| m.kind == "image")
                        .unwrap_or(false);
                    if is_image {
                        let m = inbound.media.clone().unwrap();
                        let mut keys = vec![m.file_key.clone()];
                        keys.extend(m.extra_image_keys.iter().cloned());
                        let mut download_failed = false;
                        for key in &keys {
                            match download_resource(&self.cfg, &m.message_id, key, "image").await {
                                Ok((bytes, mime)) => {
                                    inbound.images.push(crate::ai::llm::ImageAttachment {
                                        mime: if mime.starts_with("image/") { mime } else { "image/jpeg".to_string() },
                                        base64: base64::Engine::encode(
                                            &base64::engine::general_purpose::STANDARD,
                                            &bytes,
                                        ),
                                        name: None,
                                    });
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, "feishu image download failed");
                                    download_failed = true;
                                    break;
                                }
                            }
                        }
                        if download_failed {
                            let _ = self
                                .sink
                                .send_text(&inbound.chat_id, "图片下载失败，请重发一次。")
                                .await;
                            return Ok(None);
                        }
                    }
                    router::enqueue(self.app.clone(), inbound, self.sink.clone()).await;
                }
                Ok(None) => {
                    // The "text only" notice must honor the same group-mention
                    // policy as real replies — otherwise every sticker/image in
                    // a group gets an unprompted bot reply.
                    if is_non_text_user_message(&payload) && bot_engaged(&payload, &self.bot_open_id) {
                        let chat_id = payload
                            .pointer("/message/chat_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default();
                        if !chat_id.is_empty() {
                            let _ = self
                                .sink
                                .send_text(chat_id, "暂不支持这类消息：目前支持文字、图片和图文混排；文件请在思库对话列表右键本会话 →「查看群文件」处理。")
                                .await;
                        }
                    }
                }
                Err(reason) => {
                    tracing::info!(reason, "feishu message dropped");
                }
            }
            Ok(None)
        })
    }
}

/// Text messages from humans that pass the group-mention policy become
/// inbound messages. Err(reason) marks a deliberate drop for logging.
fn parse_inbound(
    payload: &serde_json::Value,
    bot_open_id: &str,
    instance_channel: &str,
) -> Result<Option<InboundMessage>, &'static str> {
    let sender_type = payload
        .pointer("/sender/sender_type")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if sender_type != "user" {
        return Err("非人类发送者（机器人消息），忽略");
    }
    let message = payload.get("message").ok_or("负载缺少 message 字段")?;
    let msg_type = message.get("message_type").and_then(|v| v.as_str()).unwrap_or_default();
    if msg_type != "text" && msg_type != "image" && msg_type != "file" && msg_type != "post" {
        return Ok(None); // other types: handled by the caller with a notice
    }
    let kind = match message.get("chat_type").and_then(|v| v.as_str()) {
        Some("p2p") => ChatKind::Private,
        _ => ChatKind::Group,
    };

    // Mention keys look like "@_user_1" inside the text; strip them all.
    let mut mentioned_keys: Vec<String> = Vec::new();
    if let Some(list) = message.get("mentions").and_then(|m| m.as_array()) {
        for m in list {
            if let Some(key) = m.get("key").and_then(|k| k.as_str()) {
                mentioned_keys.push(key.to_string());
            }
        }
    }
    if kind == ChatKind::Group && !bot_engaged(payload, bot_open_id) {
        return Err("群聊消息未 @ 机器人，按策略忽略");
    }

    let msg_id = message
        .get("message_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let chat_id = message
        .get("chat_id")
        .and_then(|v| v.as_str())
        .ok_or("消息缺少 chat_id")?;
    let user_id = payload
        .pointer("/sender/sender_id/open_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    // Rich-text (post) messages: paragraphs of tagged segments. Text/link
    // segments become the prompt text, at-segments keep "@name" for context
    // (the bot's own mention is dropped), img segments become image
    // attachments downloaded by the handler.
    if msg_type == "post" {
        let content = message
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or("消息缺少 content")?;
        let c: serde_json::Value =
            serde_json::from_str(content).map_err(|_| "content 不是合法的 JSON")?;
        let mut text = String::new();
        let mut image_keys: Vec<String> = Vec::new();
        if let Some(title) = c
            .get("title")
            .and_then(|t| t.as_str())
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            text.push_str(title);
            text.push('\n');
        }
        if let Some(paragraphs) = c.get("content").and_then(|c| c.as_array()) {
            for para in paragraphs.iter().filter_map(|p| p.as_array()) {
                for seg in para {
                    match seg.get("tag").and_then(|t| t.as_str()) {
                        Some("text") | Some("a") => {
                            if let Some(t) = seg.get("text").and_then(|t| t.as_str()) {
                                text.push_str(t);
                            }
                        }
                        Some("at") => {
                            let uid = seg
                                .get("user_id")
                                .and_then(|u| u.as_str())
                                .unwrap_or_default();
                            if uid != bot_open_id {
                                if let Some(name) =
                                    seg.get("user_name").and_then(|n| n.as_str())
                                {
                                    text.push('@');
                                    text.push_str(name);
                                    text.push(' ');
                                }
                            }
                        }
                        Some("img") => {
                            if let Some(k) = seg
                                .get("image_key")
                                .and_then(|k| k.as_str())
                                .filter(|k| !k.is_empty())
                            {
                                image_keys.push(k.to_string());
                            }
                        }
                        _ => {}
                    }
                }
                text.push('\n');
            }
        }
        let text = text.trim().to_string();
        if text.is_empty() && image_keys.is_empty() {
            return Err("post 消息无文本也无图片");
        }
        let (first_key, extra_keys) = match image_keys.split_first() {
            Some((first, rest)) => (first.clone(), rest.to_vec()),
            None => (String::new(), Vec::new()),
        };
        return Ok(Some(InboundMessage {
            channel: instance_channel.to_string(),
            chat_id: chat_id.to_string(),
            kind,
            user_id,
            user_name: String::new(),
            msg_id: msg_id.clone(),
            text,
            images: Vec::new(),
            media: if first_key.is_empty() {
                None
            } else {
                Some(crate::remote::router::MediaRef {
                    kind: "image".to_string(),
                    name: "图片".to_string(),
                    message_id: msg_id,
                    file_key: first_key,
                    extra_image_keys: extra_keys,
                })
            },
        }));
    }

    if msg_type != "text" {
        // Media messages carry no text; the file_key/image_key locates the
        // attachment for the handler to download (images) or for the group
        // files dialog to fetch later (files).
        let content = message
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or("消息缺少 content")?;
        let c: serde_json::Value = serde_json::from_str(content)
            .map_err(|_| "content 不是合法的 JSON")?;
        let (key, name) = if msg_type == "image" {
            (
                c.get("image_key").and_then(|k| k.as_str()).unwrap_or_default(),
                "图片".to_string(),
            )
        } else {
            (
                c.get("file_key").and_then(|k| k.as_str()).unwrap_or_default(),
                c.get("file_name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("文件")
                    .to_string(),
            )
        };
        if key.is_empty() {
            return Err("媒体消息缺少 file_key/image_key");
        }
        return Ok(Some(InboundMessage {
            channel: instance_channel.to_string(),
            chat_id: chat_id.to_string(),
            kind,
            user_id,
            user_name: String::new(),
            msg_id: msg_id.clone(),
            text: String::new(),
            images: Vec::new(),
            media: Some(crate::remote::router::MediaRef {
                kind: msg_type.to_string(),
                name,
                message_id: msg_id,
                file_key: key.to_string(),
                extra_image_keys: Vec::new(),
            }),
        }));
    }

    let content = message
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or("消息缺少 content")?;
    let mut text = serde_json::from_str::<serde_json::Value>(content)
        .ok()
        .and_then(|c| c.get("text").and_then(|t| t.as_str()).map(|s| s.to_string()))
        .ok_or("content 不是合法的 text 消息体")?;
    for key in &mentioned_keys {
        text = text.replace(key.as_str(), "");
    }
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("去除 @ 引用后文本为空");
    }

    Ok(Some(InboundMessage {
        channel: instance_channel.to_string(),
        chat_id: chat_id.to_string(),
        kind,
        user_id,
        user_name: String::new(),
        msg_id,
        text,
        images: Vec::new(),
        media: None,
    }))
}


/// Engagement policy shared by the reply path and the "text only" notice:
/// p2p chats always engage; group chats only when the bot is @-mentioned
/// (mention entries carry the target's open_id under id.open_id).
fn bot_engaged(payload: &serde_json::Value, bot_open_id: &str) -> bool {
    let Some(message) = payload.get("message") else {
        return false;
    };
    if message.get("chat_type").and_then(|v| v.as_str()) == Some("p2p") {
        return true;
    }
    !bot_open_id.is_empty()
        && message
            .get("mentions")
            .and_then(|m| m.as_array())
            .map(|list| {
                list.iter().any(|m| {
                    m.pointer("/id/open_id").and_then(|v| v.as_str()) == Some(bot_open_id)
                })
            })
            .unwrap_or(false)
}

fn is_non_text_user_message(payload: &serde_json::Value) -> bool {
    payload.pointer("/sender/sender_type").and_then(|v| v.as_str()) == Some("user")
        && payload.pointer("/message/message_type").and_then(|v| v.as_str()) != Some("text")
}

// ---------------------------------------------------------------- card actions
// Button callbacks from progress/final cards (停止生成 / 重试 / 思考过程).
// They arrive as card.action.trigger events over the same long connection.

struct FeishuCardActionHandler {
    app: AppHandle,
    cfg: FeishuBotConfig,
    cards: CardRegistry,
    sink: Arc<dyn ReplySink>,
    instance_channel: String,
}

impl EventHandler for FeishuCardActionHandler {
    fn event_type(&self) -> &str {
        "card.action.trigger"
    }

    fn handle(&self, event: Event) -> Pin<Box<dyn Future<Output = EventHandlerResult> + Send + '_>> {
        Box::pin(async move {
            let Some(payload) = event.event else {
                return Ok(None);
            };
            let action = payload
                .pointer("/action/value/remote")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let chat_id = payload
                .pointer("/context/open_chat_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let message_id = payload
                .pointer("/context/open_message_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let operator = payload
                .pointer("/operator/open_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if action.is_empty() || chat_id.is_empty() {
                return Ok(None);
            }
            let state = self
                .cards
                .lock()
                .ok()
                .and_then(|cards| cards.get(chat_id).cloned());
            let Some(state) = state else {
                return Ok(None);
            };
            // Ignore clicks on superseded cards from older turns.
            if !message_id.is_empty() && message_id != state.message_id {
                return Ok(None);
            }
            match action {
                "stop" => self.handle_stop(chat_id, state).await,
                "retry" => self.handle_retry(chat_id, operator, state).await,
                "toggle_thinking" => self.handle_toggle(chat_id).await,
                _ => {}
            }
            Ok(None)
        })
    }
}

impl FeishuCardActionHandler {
    /// Cancel the engine turn. The router's progress_finish renders the
    /// final stopped card once the cancelled reply row lands; we patch a
    /// "stopping" hint immediately so the button feels responsive.
    async fn handle_stop(&self, chat_id: &str, mut state: CardState) {
        if state.finished {
            return;
        }
        {
            let app_state = self.app.state::<crate::AppState>();
            let tokens = app_state.cancel_tokens.lock().await;
            if let Some(token) = tokens.get(&state.session_id) {
                token.cancel();
            }
        }
        state.stopped = true;
        if let Ok(mut cards) = self.cards.lock() {
            cards.insert(chat_id.to_string(), state.clone());
        }
        let hint = card_json(
            "⏹ 正在停止…",
            "orange",
            vec![md_div(&state.preview), note_el("思库远程助手")],
        );
        let _ = card_update(&self.cfg, &state.message_id, &hint).await;
    }

    /// Re-run the same prompt through the normal pipeline. The old card
    /// gets a retried marker; the new turn creates its own progress card.
    async fn handle_retry(&self, chat_id: &str, operator: &str, mut state: CardState) {
        if !state.finished {
            return;
        }
        state.footer = format!("{} · 已重试 ↻", state.footer);
        let _ = card_update(&self.cfg, &state.message_id, &final_card(&state)).await;
        let inbound = InboundMessage {
            channel: self.instance_channel.clone(),
            chat_id: chat_id.to_string(),
            kind: state.kind,
            user_id: operator.to_string(),
            user_name: String::new(),
            msg_id: String::new(), // synthetic click: no dedup entry
            text: state.prompt.clone(),
            images: Vec::new(),
            media: None,
        };
        router::enqueue(self.app.clone(), inbound, self.sink.clone()).await;
    }

    async fn handle_toggle(&self, chat_id: &str) {
        let (message_id, card) = {
            let Ok(mut cards) = self.cards.lock() else {
                return;
            };
            let Some(state) = cards.get_mut(chat_id) else {
                return;
            };
            state.thinking_expanded = !state.thinking_expanded;
            (state.message_id.clone(), final_card(state))
        };
        let _ = card_update(&self.cfg, &message_id, &card).await;
    }
}

// ---------------------------------------------------------------- replies

struct FeishuSink {
    client: feishu_sdk::Client,
    cfg: FeishuBotConfig,
    /// In-flight and settled progress cards: chat_id → card state. Shared
    /// with the card-action callback handler (stop/retry/thinking buttons).
    cards: CardRegistry,
}

type CardRegistry = Arc<std::sync::Mutex<std::collections::HashMap<String, CardState>>>;

#[derive(Clone)]
struct CardState {
    message_id: String,
    preview: String,
    /// Full prompt and routing context, so the retry button can re-run the
    /// turn without any further lookup.
    prompt: String,
    session_id: String,
    kind: ChatKind,
    /// Set by progress_finish when the turn settled.
    finished: bool,
    ok: bool,
    /// Card body (the reply, or the "分条发送" pointer for long replies).
    reply: String,
    thinking: String,
    footer: String,
    /// The user clicked 停止生成 before the turn settled: progress_finish
    /// renders the stopped header instead of the success one.
    stopped: bool,
    thinking_expanded: bool,
}

// ---------------------------------------------------------------- progress cards
// Interactive cards + message PATCH give feishu a live "working" surface:
// the card is created when the turn starts, patched as tool/reasoning
// events stream in, and finalized with the reply.

fn md_div(content: &str) -> serde_json::Value {
    serde_json::json!({"tag": "div", "text": {"tag": "lark_md", "content": content}})
}

fn note_el(text: &str) -> serde_json::Value {
    serde_json::json!({"tag": "note", "elements": [{"tag": "plain_text", "content": text}]})
}

fn hr_el() -> serde_json::Value {
    serde_json::json!({"tag": "hr"})
}

fn card_json(header: &str, template: &str, elements: Vec<serde_json::Value>) -> String {
    serde_json::json!({
        "config": { "wide_screen_mode": true },
        "header": { "template": template, "title": { "tag": "plain_text", "content": header } },
        "elements": elements,
    })
    .to_string()
}

/// Card action row. Each button carries {"remote": action} in its value —
/// the card.action.trigger callback dispatches on it.
fn buttons_el(buttons: &[(&str, &str, &str)]) -> serde_json::Value {
    serde_json::json!({
        "tag": "action",
        "actions": buttons.iter().map(|(text, kind, action)| serde_json::json!({
            "tag": "button",
            "text": { "tag": "plain_text", "content": text },
            "type": kind,
            "value": { "remote": action },
        })).collect::<Vec<_>>(),
    })
}

/// The settled card: success/stopped/failure header, reply body, optional
/// expanded thinking section, footer, retry (+thinking toggle) buttons.
fn final_card(state: &CardState) -> String {
    let (header, template) = if state.stopped {
        ("⏹ 已停止", "orange")
    } else if state.ok {
        ("✅ 已完成", "green")
    } else {
        ("❌ 处理失败", "red")
    };
    let mut els = vec![md_div(&state.reply), hr_el(), note_el(&state.footer)];
    let mut buttons: Vec<(&str, &str, &str)> = vec![("重试", "primary", "retry")];
    if !state.thinking.is_empty() {
        if state.thinking_expanded {
            els.push(md_div(&format!("**思考过程**\n{}", state.thinking)));
            buttons.push(("收起思考过程", "default", "toggle_thinking"));
        } else {
            buttons.push(("思考过程", "default", "toggle_thinking"));
        }
    }
    els.push(buttons_el(&buttons));
    card_json(header, template, els)
}

/// The in-progress card: prompt preview, tool action lines with live
/// checkmarks, latest thinking snippet, streaming answer preview, footer.
fn progress_card(preview: &str, p: &crate::remote::router::RemoteProgress) -> String {
    let mut els = vec![md_div(&format!("「{preview}」")), hr_el()];
    for a in &p.actions {
        let status = if a.done {
            match a.duration_ms {
                Some(d) => format!(" ✅ {:.1}s", d as f64 / 1000.0),
                None => " ✅".to_string(),
            }
        } else {
            " ⏳".to_string()
        };
        els.push(md_div(&format!("{}{status}", a.label)));
    }
    if !p.thinking_so_far.is_empty() && p.answer_so_far.is_empty() {
        let tail: String = p
            .thinking_so_far
            .chars()
            .rev()
            .take(120)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        els.push(md_div(&format!("🧠 {tail}")));
    }
    if !p.answer_so_far.is_empty() {
        els.push(hr_el());
        els.push(md_div(&p.answer_so_far));
    }
    els.push(note_el(&format!("已用时 {}s · 思库远程助手", p.elapsed_secs())));
    els.push(buttons_el(&[("停止生成", "danger", "stop")]));
    card_json("⏳ 思库正在处理", "blue", els)
}

async fn card_create(cfg: &FeishuBotConfig, chat_id: &str, content: &str) -> Result<String, String> {
    let token = tenant_token(cfg).await?;
    let v: serde_json::Value = reqwest::Client::new()
        .post("https://open.feishu.cn/open-apis/im/v1/messages?receive_id_type=chat_id")
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "receive_id": chat_id,
            "msg_type": "interactive",
            "content": content,
        }))
        .send()
        .await
        .map_err(|e| format!("飞书卡片发送失败: {e}"))?
        .json()
        .await
        .map_err(|e| format!("飞书卡片响应解析失败: {e}"))?;
    let data = feishu_data(&v, "发送卡片")?;
    data.get("message_id")
        .and_then(|m| m.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "飞书卡片响应缺少 message_id".to_string())
}

async fn card_update(cfg: &FeishuBotConfig, message_id: &str, content: &str) -> Result<(), String> {
    let token = tenant_token(cfg).await?;
    let v: serde_json::Value = reqwest::Client::new()
        .patch(format!(
            "https://open.feishu.cn/open-apis/im/v1/messages/{message_id}"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "content": content }))
        .send()
        .await
        .map_err(|e| format!("飞书卡片更新失败: {e}"))?
        .json()
        .await
        .map_err(|e| format!("飞书卡片更新响应解析失败: {e}"))?;
    feishu_data(&v, "更新卡片")?;
    Ok(())
}

/// Group name via im/v1/chats/{chat_id}; member name via
/// contact/v3/users/{open_id}. Both need extra scopes (im:chat:readonly,
/// contact:user.base:readonly) — failures degrade to None, never block.
async fn fetch_feishu_names(
    cfg: &FeishuBotConfig,
    chat_id: &str,
    user_id: &str,
    kind: ChatKind,
) -> (Option<String>, Option<String>) {
    let token = match tenant_token(cfg).await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "feishu name resolution: token failed");
            return (None, None);
        }
    };
    let http = reqwest::Client::new();

    let chat_name = if kind == ChatKind::Group {
        match http
            .get(format!("https://open.feishu.cn/open-apis/im/v1/chats/{chat_id}"))
            .bearer_auth(&token)
            .send()
            .await
            .and_then(|r| r.error_for_status().map_err(|e| e))
        {
            Ok(resp) => match resp.json::<serde_json::Value>().await {
                Ok(v) => {
                    let name = v
                        .pointer("/data/name")
                        .and_then(|n| n.as_str())
                        .filter(|n| !n.is_empty())
                        .map(|n| n.to_string());
                    if name.is_none() {
                        tracing::warn!(body = %v, "feishu chat info: no name (check im:chat scope)");
                    }
                    name
                }
                Err(e) => {
                    tracing::warn!(error = %e, "feishu chat info parse failed");
                    None
                }
            },
            Err(e) => {
                tracing::warn!(error = %e, "feishu chat info request failed");
                None
            }
        }
    } else {
        None
    };

    let user_name = if !user_id.is_empty() {
        match http
            .get(format!(
                "https://open.feishu.cn/open-apis/contact/v3/users/{user_id}?user_id_type=open_id"
            ))
            .bearer_auth(&token)
            .send()
            .await
            .and_then(|r| r.error_for_status().map_err(|e| e))
        {
            Ok(resp) => match resp.json::<serde_json::Value>().await {
                Ok(v) => {
                    let name = v
                        .pointer("/data/user/name")
                        .and_then(|n| n.as_str())
                        .filter(|n| !n.is_empty())
                        .map(|n| n.to_string());
                    if name.is_none() {
                        tracing::warn!(body = %v, "feishu user info: no name (check contact scope)");
                    }
                    name
                }
                Err(e) => {
                    tracing::warn!(error = %e, "feishu user info parse failed");
                    None
                }
            },
            Err(e) => {
                tracing::warn!(error = %e, "feishu user info request failed");
                None
            }
        }
    } else {
        None
    };

    (chat_name, user_name)
}

#[async_trait::async_trait]
impl ReplySink for FeishuSink {
    async fn resolve_names(
        &self,
        chat_id: &str,
        user_id: &str,
        kind: ChatKind,
    ) -> (Option<String>, Option<String>) {
        fetch_feishu_names(&self.cfg, chat_id, user_id, kind).await
    }

    async fn send_text(&self, chat_id: &str, text: &str) -> Result<(), String> {
        for chunk in chunk_text(text, 4000) {
            let query = feishu_sdk::api::SendMessageQuery {
                receive_id_type: Some("chat_id".to_string()),
            };
            let body = feishu_sdk::api::SendMessageBody {
                receive_id: chat_id.to_string(),
                msg_type: "text".to_string(),
                content: serde_json::json!({ "text": chunk }).to_string(),
                uuid: None,
            };
            let resp = self
                .client
                .im_v1_message()
                .send_typed(&query, &body, RequestOptions::default())
                .await
                .map_err(|e| format!("飞书发送失败: {e}"))?;
            if resp.code != 0 {
                return Err(format!("飞书发送失败: {} {}", resp.code, resp.msg));
            }
        }
        Ok(())
    }

    async fn progress_start(
        &self,
        chat_id: &str,
        preview: &str,
        session_id: &str,
        prompt: &str,
        kind: ChatKind,
    ) -> Result<(), String> {
        let card = progress_card(preview, &crate::remote::router::RemoteProgress::default());
        match card_create(&self.cfg, chat_id, &card).await {
            Ok(message_id) => {
                if let Ok(mut cards) = self.cards.lock() {
                    cards.insert(
                        chat_id.to_string(),
                        CardState {
                            message_id,
                            preview: preview.to_string(),
                            prompt: prompt.to_string(),
                            session_id: session_id.to_string(),
                            kind,
                            finished: false,
                            ok: false,
                            reply: String::new(),
                            thinking: String::new(),
                            footer: String::new(),
                            stopped: false,
                            thinking_expanded: false,
                        },
                    );
                }
            }
            Err(e) => {
                // No card → the router's plain-text fallback still works.
                tracing::warn!(error = %e, "feishu progress card create failed");
            }
        }
        Ok(())
    }

    async fn progress_update(
        &self,
        chat_id: &str,
        progress: &crate::remote::router::RemoteProgress,
    ) -> Result<(), String> {
        let state = self
            .cards
            .lock()
            .ok()
            .and_then(|cards| cards.get(chat_id).map(|c| (c.message_id.clone(), c.preview.clone())));
        let Some((message_id, preview)) = state else {
            return Ok(());
        };
        // The card is a nicety, never a blocker: update failures are logged
        // and the final answer still lands via progress_finish/send_text.
        if let Err(e) = card_update(&self.cfg, &message_id, &progress_card(&preview, progress)).await {
            tracing::warn!(error = %e, "feishu progress card update failed");
        }
        Ok(())
    }

    async fn progress_finish(
        &self,
        chat_id: &str,
        reply: &str,
        ok: bool,
        progress: &crate::remote::router::RemoteProgress,
    ) -> Result<bool, String> {
        // Long replies overflow the card body: finalize the card with a
        // pointer and let the router send the text chunks as usual.
        const FINAL_CARD_MAX: usize = 2500;
        let (body, handled) = if !ok {
            (reply.to_string(), true)
        } else if reply.len() <= FINAL_CARD_MAX {
            (reply.to_string(), true)
        } else {
            ("回复较长，分条发送：".to_string(), false)
        };
        let footer = format!(
            "用时 {}s · {} 次工具调用 · 思库远程助手",
            progress.elapsed_secs(),
            progress.actions.len()
        );
        let (message_id, card) = {
            let Ok(mut cards) = self.cards.lock() else {
                return Ok(false);
            };
            let Some(state) = cards.get_mut(chat_id) else {
                return Ok(false);
            };
            state.finished = true;
            state.ok = ok;
            state.reply = body;
            state.thinking = progress.thinking_so_far.clone();
            state.footer = footer;
            (state.message_id.clone(), final_card(state))
        };
        card_update(&self.cfg, &message_id, &card).await?;
        Ok(handled)
    }
}

/// Split on char boundaries so multi-part replies never cut a UTF-8 sequence.
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

// ---------------------------------------------------------------- group info & files
// REST helpers backing the "查看群信息 / 查看群文件" dialogs. They reuse the
// stored app credentials and need no live channel connection.

pub(crate) struct FeishuChatInfo {
    pub name: String,
    pub description: String,
    pub owner_id: String,
    pub member_count: i64,
}

pub(crate) struct FeishuMember {
    pub open_id: String,
    pub name: String,
}

pub(crate) struct FeishuChatFile {
    pub message_id: String,
    pub file_key: String,
    pub kind: String, // "file" | "image"
    pub name: String,
    pub size_bytes: i64,
    pub sender_id: String,
    pub create_time_ms: String,
}

fn feishu_data(v: &serde_json::Value, what: &str) -> Result<serde_json::Value, String> {
    let code = v.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
    if code != 0 {
        let msg = v.get("msg").and_then(|m| m.as_str()).unwrap_or("");
        return Err(format!("飞书{what}失败: code={code} {msg}（请确认权限已开通并已发布版本）"));
    }
    Ok(v.get("data").cloned().unwrap_or(serde_json::Value::Null))
}

pub(crate) async fn fetch_chat_info(
    cfg: &FeishuBotConfig,
    chat_id: &str,
) -> Result<(FeishuChatInfo, Vec<FeishuMember>), String> {
    let token = tenant_token(cfg).await?;
    let http = reqwest::Client::new();

    let v: serde_json::Value = http
        .get(format!("https://open.feishu.cn/open-apis/im/v1/chats/{chat_id}"))
        .bearer_auth(&token)
        .send()
        .await
        .map_err(|e| format!("飞书群信息请求失败: {e}"))?
        .json()
        .await
        .map_err(|e| format!("飞书群信息解析失败: {e}"))?;
    let data = feishu_data(&v, "获取群信息")?;
    let info = FeishuChatInfo {
        name: data.get("name").and_then(|n| n.as_str()).unwrap_or_default().to_string(),
        description: data
            .get("description")
            .and_then(|d| d.as_str())
            .unwrap_or_default()
            .to_string(),
        owner_id: data.get("owner_id").and_then(|o| o.as_str()).unwrap_or_default().to_string(),
        member_count: data
            .get("user_count")
            .and_then(|c| c.as_str().and_then(|s| s.parse().ok()).or_else(|| c.as_i64()))
            .unwrap_or(0),
    };

    let mut members = Vec::new();
    let mut page_token = String::new();
    for _ in 0..6 {
        let url = format!(
            "https://open.feishu.cn/open-apis/im/v1/chats/{chat_id}/members?member_id_type=open_id&page_size=100{page}"
        , page = if page_token.is_empty() { String::new() } else { format!("&page_token={page_token}") });
        let v: serde_json::Value = http
            .get(&url)
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| format!("飞书群成员请求失败: {e}"))?
            .json()
            .await
            .map_err(|e| format!("飞书群成员解析失败: {e}"))?;
        let data = feishu_data(&v, "获取群成员")?;
        if let Some(items) = data.get("items").and_then(|i| i.as_array()) {
            for m in items {
                members.push(FeishuMember {
                    open_id: m.get("member_id").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
                    name: m.get("name").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
                });
            }
        }
        let more = data.get("has_more").and_then(|h| h.as_bool()).unwrap_or(false);
        page_token = data
            .get("page_token")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string();
        if !more || page_token.is_empty() {
            break;
        }
    }
    Ok((info, members))
}

/// File/image messages in the chat, newest first (capped at ~200 entries).
pub(crate) async fn fetch_chat_files(
    cfg: &FeishuBotConfig,
    chat_id: &str,
) -> Result<Vec<FeishuChatFile>, String> {
    let token = tenant_token(cfg).await?;
    let http = reqwest::Client::new();
    let mut out = Vec::new();
    let mut page_token = String::new();
    for _ in 0..10 {
        let url = format!(
            "https://open.feishu.cn/open-apis/im/v1/messages?container_id_type=chat&container_id={chat_id}&sort_type=ByCreateTimeDesc&page_size=50{page}"
        , page = if page_token.is_empty() { String::new() } else { format!("&page_token={page_token}") });
        let v: serde_json::Value = http
            .get(&url)
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| format!("飞书消息列表请求失败: {e}"))?
            .json()
            .await
            .map_err(|e| format!("飞书消息列表解析失败: {e}"))?;
        let data = feishu_data(&v, "获取群文件")?;
        if let Some(items) = data.get("items").and_then(|i| i.as_array()) {
            for m in items {
                // REST API message objects differ from event payloads:
                // msg_type (not message_type), body.content (not content),
                // create_time may be a number (events use strings).
                if m.get("deleted").and_then(|d| d.as_bool()).unwrap_or(false) {
                    continue;
                }
                let msg_type = m
                    .get("msg_type")
                    .or_else(|| m.get("message_type"))
                    .and_then(|t| t.as_str())
                    .unwrap_or_default();
                if msg_type != "file" && msg_type != "image" {
                    continue;
                }
                let content = m
                    .pointer("/body/content")
                    .or_else(|| m.get("content"))
                    .and_then(|c| c.as_str())
                    .unwrap_or("{}");
                let c: serde_json::Value = serde_json::from_str(content).unwrap_or_default();
                let (key, name, size) = if msg_type == "file" {
                    (
                        c.get("file_key").and_then(|k| k.as_str()).unwrap_or_default().to_string(),
                        c.get("file_name").and_then(|n| n.as_str()).unwrap_or("文件").to_string(),
                        c.get("file_size").and_then(|s| s.as_i64()).unwrap_or(0),
                    )
                } else {
                    (
                        c.get("image_key").and_then(|k| k.as_str()).unwrap_or_default().to_string(),
                        format!("图片.{}", c.get("image_type").and_then(|t| t.as_str()).unwrap_or("png")),
                        0,
                    )
                };
                if key.is_empty() {
                    continue;
                }
                out.push(FeishuChatFile {
                    message_id: m.get("message_id").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
                    file_key: key,
                    kind: msg_type.to_string(),
                    name,
                    size_bytes: size,
                    sender_id: m.pointer("/sender/id").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
                    create_time_ms: m
                        .get("create_time")
                        .and_then(|x| x.as_str().map(|s| s.to_string()).or_else(|| x.as_i64().map(|n| n.to_string())))
                        .unwrap_or_default(),
                });
                if out.len() >= 200 {
                    return Ok(out);
                }
            }
        }
        let more = data.get("has_more").and_then(|h| h.as_bool()).unwrap_or(false);
        page_token = data
            .get("page_token")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string();
        if !more || page_token.is_empty() {
            break;
        }
    }
    Ok(out)
}

/// Download a message attachment (file or image); returns bytes + mime.
pub(crate) async fn download_resource(
    cfg: &FeishuBotConfig,
    message_id: &str,
    file_key: &str,
    kind: &str,
) -> Result<(Vec<u8>, String), String> {
    let token = tenant_token(cfg).await?;
    let resp = reqwest::Client::new()
        .get(format!(
            "https://open.feishu.cn/open-apis/im/v1/messages/{message_id}/resources/{file_key}?type={kind}"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .map_err(|e| format!("飞书文件下载失败: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("飞书文件下载失败: HTTP {status} {}", body.chars().take(200).collect::<String>()));
    }
    let mime = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(';').next().unwrap_or(s).trim().to_string())
        .unwrap_or_else(|| "application/octet-stream".to_string());
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("飞书文件读取失败: {e}"))?;
    Ok((bytes.to_vec(), mime))
}
