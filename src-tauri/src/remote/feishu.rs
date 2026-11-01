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
use tauri::AppHandle;

use crate::core::models::FeishuChannelConfig;
use crate::remote::router::{self, ChatKind, InboundMessage, ReplySink};
use crate::remote::RemoteManager;

/// Spawn the supervisor task for the feishu channel. Status transitions are
/// reported through the manager (and onward to the settings UI).
pub fn spawn(
    db: sqlx::SqlitePool,
    app: AppHandle,
    manager: RemoteManager,
    cfg: FeishuChannelConfig,
    cancel: tokio_util::sync::CancellationToken,
) {
    tokio::spawn(async move {
        manager
            .set_feishu_status(false, "连接中…".to_string(), &app)
            .await;
        match run(db, &app, &manager, &cfg, cancel).await {
            Ok(()) => {
                manager
                    .set_feishu_status(false, "已停止".to_string(), &app)
                    .await
            }
            Err(e) => {
                tracing::error!(error = %e, "feishu channel stopped with error");
                manager
                    .set_feishu_status(false, format!("错误：{e}"), &app)
                    .await
            }
        }
    });
}

async fn run(
    _db: sqlx::SqlitePool,
    app: &AppHandle,
    manager: &RemoteManager,
    cfg: &FeishuChannelConfig,
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
    let sink: Arc<dyn ReplySink> = Arc::new(FeishuSink { client: http_client });

    let dispatcher = EventDispatcher::new(EventDispatcherConfig::new(), logger);
    dispatcher
        .register_handler(Box::new(FeishuMessageHandler {
            app: app.clone(),
            sink: sink.clone(),
            bot_open_id: bot.open_id.clone(),
        }))
        .await;

    let stream = StreamClient::builder(config)
        .event_dispatcher(dispatcher)
        .build()
        .map_err(|e| format!("feishu stream init failed: {e}"))?;

    manager
        .set_feishu_status(true, format!("已连接（{}）", bot.app_name), app)
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

/// tenant_access_token → bot/v3/info. Two tiny REST calls; the SDK keeps its
/// token handling internal, so we do this directly.
async fn fetch_bot_info(cfg: &FeishuChannelConfig) -> Result<BotInfo, String> {
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
    let token = token_resp
        .get("tenant_access_token")
        .and_then(|t| t.as_str())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| {
            format!(
                "飞书鉴权失败（请检查 App ID / App Secret）：{}",
                token_resp
            )
        })?
        .to_string();

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
}

impl EventHandler for FeishuMessageHandler {
    fn event_type(&self) -> &str {
        "im.message.receive_v1"
    }

    fn handle(&self, event: Event) -> Pin<Box<dyn Future<Output = EventHandlerResult> + Send + '_>> {
        Box::pin(async move {
            let Some(payload) = event.event else {
                return Ok(None);
            };
            if let Some(inbound) = parse_inbound(&payload, &self.bot_open_id) {
                router::enqueue(self.app.clone(), inbound, self.sink.clone()).await;
            } else if is_non_text_user_message(&payload) {
                let chat_id = payload
                    .pointer("/message/chat_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if !chat_id.is_empty() {
                    let _ = self
                        .sink
                        .send_text(chat_id, "目前只支持文字消息，图片和文件请在思库应用内处理。")
                        .await;
                }
            }
            Ok(None)
        })
    }
}

/// Text messages from humans that pass the group-mention policy become
/// inbound messages; everything else returns None.
fn parse_inbound(payload: &serde_json::Value, bot_open_id: &str) -> Option<InboundMessage> {
    let sender_type = payload.pointer("/sender/sender_type")?.as_str()?;
    if sender_type != "user" {
        return None; // ignore other bots
    }
    let message = payload.get("message")?;
    if message.get("msg_type")?.as_str()? != "text" {
        return None;
    }
    let chat_type = message.get("chat_type")?.as_str()?;
    let kind = match chat_type {
        "p2p" => ChatKind::Private,
        _ => ChatKind::Group,
    };

    // Mention keys look like "@_user_1" inside the text; strip them all.
    let mut mentioned_keys: Vec<String> = Vec::new();
    let mut bot_mentioned = false;
    if let Some(list) = message.get("mentions").and_then(|m| m.as_array()) {
        for m in list {
            if let Some(key) = m.get("key").and_then(|k| k.as_str()) {
                mentioned_keys.push(key.to_string());
            }
            if m.pointer("/id/open_id").and_then(|v| v.as_str()) == Some(bot_open_id)
                && !bot_open_id.is_empty()
            {
                bot_mentioned = true;
            }
        }
    }
    if kind == ChatKind::Group && !bot_mentioned {
        return None;
    }

    let content = message.get("content")?.as_str()?;
    let mut text = serde_json::from_str::<serde_json::Value>(content)
        .ok()?
        .get("text")?
        .as_str()?
        .to_string();
    for key in &mentioned_keys {
        text = text.replace(key.as_str(), "");
    }
    let text = text.trim().to_string();
    if text.is_empty() {
        return None;
    }

    Some(InboundMessage {
        channel: "feishu",
        chat_id: message.get("chat_id")?.as_str()?.to_string(),
        kind,
        user_id: payload
            .pointer("/sender/sender_id/open_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        user_name: String::new(),
        msg_id: message
            .get("msg_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        text,
    })
}

fn is_non_text_user_message(payload: &serde_json::Value) -> bool {
    payload.pointer("/sender/sender_type").and_then(|v| v.as_str()) == Some("user")
        && payload.pointer("/message/msg_type").and_then(|v| v.as_str()) != Some("text")
}

// ---------------------------------------------------------------- replies

struct FeishuSink {
    client: feishu_sdk::Client,
}

#[async_trait::async_trait]
impl ReplySink for FeishuSink {
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
