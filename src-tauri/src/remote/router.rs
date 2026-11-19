//! Inbound routing for remote IM channels (feishu/weixin): dedup, pairing,
//! rate limiting, debounce batching, session mapping, agent turn, reply.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Mutex;

use crate::AppState;

/// System-prompt addendum appended to remote sessions at runtime (see
/// `run_agent_turn`). Remote users read plain chat text: app-internal links
/// and local paths are meaningless to them.
pub const REMOTE_PROMPT_ADDENDUM: &str = "你正通过即时通讯软件（微信/飞书）远程服务用户，能力接近思库应用内的对话，但受会话类型与授权限制。\
回复必须是纯文本：不要输出 siku-reader:// 等应用内链接，不要输出本地文件路径，\
不要输出图片标记。引用文献或笔记时给出标题即可。回复控制在手机可阅读的长度，\
内容较多时先给结论，并提示用户可以追问细节。\
用户可能远程发来文件：消息中会附带文件的保存路径和已提取的内容，\
用户要求入库时可用 paper_import 导入（仅限文献 PDF），但回复中不要暴露路径。\
把本机文件发到聊天应用（send_file_to_user）只在「私聊 + 全部授权 + 管理员开启回传」时可用；\
群聊里不可用（会广播给所有成员）。工具返回不可用时，直接把原因告诉用户并给出替代方案，\
不要尝试用 bash 或其他工具拼凑。";

/// Read-only tool set applied per-turn for members paired with the
/// "readonly" grant (the default for group members). Group sessions are
/// shared across members, so the restriction travels with the turn, not the
/// session row.
const READONLY_TOOLS_JSON: &str = r#"["paper_search","paper_read","search_library","note_read","knowledge_query","memory_read","memory_recall","web_search","web_fetch","task_list","task_output"]"#;

/// Grant levels stored in remote_chats.grant_level.
pub const GRANT_FULL: &str = "full";
pub const GRANT_READONLY: &str = "readonly";

const DEBOUNCE_MS: u64 = 1_500;
/// Text-only messages wait for a possible attachment. On WeChat the user
/// often sends the question first and the image a few seconds later; without
/// this window the text turn starts before the image arrives.
const TEXT_DEBOUNCE_MS: u64 = 2_000;
/// Image/file-only batches wait a little longer for the user to type the
/// caption/question. Once text exists, the pair window is enough.
const MEDIA_DEBOUNCE_MS: u64 = 3_000;
/// A file transfer + its caption/question can take longer than an image.
/// Wait for the accompanying text before starting a file-only turn.
const FILE_DEBOUNCE_MS: u64 = 15_000;
/// A batch may not span arbitrarily distant messages. This also stops a
/// startup backlog (three old messages delivered together) from becoming one
/// giant user turn.
const BATCH_MAX_GAP_MS: i64 = 5_000;
/// During the startup window, a remote backlog can be redelivered before the
/// synced session watermark has caught up. Messages this old are replay, not
/// something the user just sent.
const STARTUP_REPLAY_WINDOW: Duration = Duration::from_secs(120);
const STARTUP_MAX_AGE_MS: i64 = 2 * 60 * 60 * 1000;
const RATE_LIMIT_PER_MIN: usize = 10;
const SEEN_CAP: usize = 2048;
const SEEN_TTL_SECS: i64 = 24 * 3600;
/// Cap on the synced remote_msg_ids dedup table; pruning runs only above it
/// (deletes are sync traffic, so the table is allowed to just sit below).
const REMOTE_MSG_IDS_CAP: i64 = 5000;
/// Stale-backlog guard: server-redelivered messages older than this are
/// dropped even when every dedup layer missed them. The per-chat session
/// watermark above is the primary defense; this is only the last resort.
const STALE_MESSAGE_MS: i64 = 30 * 60 * 1000;
/// How often an unapproved chat is reminded to pair.
const PAIRING_NOTICE_INTERVAL_SECS: i64 = 3600;
/// Max wait for a remote turn to persist its reply before giving up.
const REPLY_TIMEOUT_SECS: u64 = 600;

/// True when a chat_sessions.context marks the row as remote-originated.
pub fn is_remote_context(context: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(context)
        .map(|v| v.get("remote").and_then(|r| r.as_bool()).unwrap_or(false))
        .unwrap_or(false)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ChatKind {
    Private,
    Group,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    File,
}

impl MediaKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::File => "file",
        }
    }
}

/// Channel-specific locator for one inbound image/file. Downloading and
/// decrypting happen later, in the batch's resolve stage, so the debounce
/// window never depends on network timing.
#[derive(Clone, Debug)]
pub enum MediaLocator {
    Feishu {
        message_id: String,
        file_key: String,
        /// Extra image keys of a Feishu post message (`file_key` is the first).
        extra_image_keys: Vec<String>,
    },
    Weixin {
        /// The `image_item.media` / `file_item.media` object.
        media: serde_json::Value,
        /// `image_item.aeskey` (hex), when present.
        aeskey: Option<String>,
    },
}

#[derive(Clone, Debug)]
pub struct MediaRef {
    pub kind: MediaKind,
    pub name: String,
    pub locator: MediaLocator,
}

#[derive(Clone)]
pub struct InboundMessage {
    /// "weixin" or "feishu:<app_id>" — the bot instance id is encoded so
    /// several feishu apps (across tenants or within one) stay distinct in
    /// pairing keys, session contexts and rate-limit/lock keys.
    pub channel: String,
    pub chat_id: String,
    pub kind: ChatKind,
    pub user_id: String,
    pub user_name: String,
    pub msg_id: String,
    /// Sender-side timestamp (epoch ms) when the channel provides one.
    /// Drives the stale-backlog guard in `enqueue`; None disables the guard
    /// for that message (synthetic turns, channels without timestamps).
    pub sent_at_ms: Option<i64>,
    pub text: String,
    /// Raw image/file references. The batch resolves them after debounce, so
    /// a slow download can never split a caption from its image.
    pub media: Vec<MediaRef>,
}

/// A file the agent asked to send back to the remote user's chat app.
///
/// The tool only *registers* it during the turn; `run_agent_turn` serializes
/// the queue into the assistant row's `attachments`, and the router uploads
/// the files after the reply is sent. One ledger row is therefore the single
/// source of truth for both sides (docs/remote-channels.md §7).
#[derive(Clone)]
pub struct OutboundFile {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub mime: String,
    pub sha256: String,
    /// Optional one-line text sent before the file itself.
    pub caption: Option<String>,
}

/// Per-turn registration queue shared by the tool and `run_agent_turn`.
/// Created by the router (never a global map: cancelled/error paths would
/// leak entries and concurrent chats would collide).
pub type OutboundQueue = Arc<std::sync::Mutex<Vec<OutboundFile>>>;

pub fn new_outbound_queue() -> OutboundQueue {
    Arc::new(std::sync::Mutex::new(Vec::new()))
}

/// Serialize one outbound attachment in the shape the frontend parses
/// (`ChatAttachment` with `kind: "file"` — no base64, see §7.3).
fn attachment_entry(f: &OutboundFile, status: &str, error: Option<&str>) -> serde_json::Value {
    let mut v = serde_json::json!({
        "kind": "file",
        "name": &f.name,
        "size": f.size,
        "mime": &f.mime,
        "path": f.path.to_string_lossy(),
        "sha256": &f.sha256,
        "status": status,
    });
    if let Some(e) = error {
        v["error"] = serde_json::json!(e);
    }
    v
}

/// Attachments JSON written when the turn succeeds; delivery happens later.
pub fn pending_attachments_json(files: &[OutboundFile]) -> String {
    serde_json::Value::Array(files.iter().map(|f| attachment_entry(f, "pending", None)).collect())
        .to_string()
}

/// Attachments JSON for a turn that never delivered (cancelled / failed).
pub fn failed_attachments_json(files: &[OutboundFile], reason: &str) -> String {
    serde_json::Value::Array(
        files
            .iter()
            .map(|f| attachment_entry(f, "failed", Some(reason)))
            .collect(),
    )
    .to_string()
}

/// Attachments JSON after delivery: `None` error means it reached the user.
pub fn settled_attachments_json(items: &[(OutboundFile, Option<String>)]) -> String {
    serde_json::Value::Array(
        items
            .iter()
            .map(|(f, err)| match err {
                None => attachment_entry(f, "ok", None),
                Some(e) => attachment_entry(f, "failed", Some(e)),
            })
            .collect(),
    )
    .to_string()
}

/// Rebuild one outbound attachment from its stored JSON entry.
fn parse_outbound_file(v: &serde_json::Value) -> Option<OutboundFile> {
    if v.get("kind").and_then(|k| k.as_str()) != Some("file") {
        return None;
    }
    Some(OutboundFile {
        path: PathBuf::from(v.get("path")?.as_str()?),
        name: v.get("name")?.as_str()?.to_string(),
        size: v.get("size")?.as_u64()?,
        mime: v.get("mime").and_then(|m| m.as_str()).unwrap_or_default().to_string(),
        sha256: v.get("sha256").and_then(|s| s.as_str()).unwrap_or_default().to_string(),
        caption: None,
    })
}

/// Files persisted as `pending` — i.e. registered by a turn that is about to
/// deliver them. A cancelled/failed turn persists `failed` instead, so this
/// returns nothing and the router sends nothing.
pub fn pending_files_from_attachments(json: &str) -> Vec<OutboundFile> {
    let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    items
        .iter()
        .filter(|v| v.get("status").and_then(|s| s.as_str()) == Some("pending"))
        .filter_map(parse_outbound_file)
        .collect()
}

/// One attachment by its position in the stored array (the retry command
/// needs the same index the UI shows).
pub fn outbound_file_at(json: &str, index: usize) -> Option<OutboundFile> {
    let items: serde_json::Value = serde_json::from_str(json).ok()?;
    parse_outbound_file(items.as_array()?.get(index)?)
}

/// Rewrite one attachment's delivery state in the stored JSON. Used after a
/// manual retry succeeds. Returns `None` when the JSON or index is invalid.
pub fn patch_attachment_status(
    json: &str,
    index: usize,
    status: &str,
    error: Option<&str>,
) -> Option<String> {
    let mut value: serde_json::Value = serde_json::from_str(json).ok()?;
    let item = value.as_array_mut()?.get_mut(index)?;
    item["status"] = serde_json::json!(status);
    match error {
        Some(e) => item["error"] = serde_json::json!(e),
        None => {
            if let Some(obj) = item.as_object_mut() {
                obj.remove("error");
            }
        }
    }
    Some(value.to_string())
}

#[async_trait::async_trait]
pub trait ReplySink: Send + Sync {
    async fn send_text(&self, chat_id: &str, text: &str) -> Result<(), String>;
    /// Send one local file to the chat. Channels that cannot upload files yet
    /// (feishu, until P2) keep the default "unsupported" answer; the router
    /// then records the failure in the assistant row instead of losing it.
    async fn send_media(
        &self,
        _chat_id: &str,
        _file: &OutboundFile,
    ) -> Result<(), String> {
        Err("当前渠道不支持发送文件".to_string())
    }

    /// Best-effort display names for pairing rows and session titles:
    /// (chat/group name, sender name). Channels without a directory API
    /// (weixin iLink) return (None, None).
    async fn resolve_names(
        &self,
        _chat_id: &str,
        _user_id: &str,
        _kind: ChatKind,
    ) -> (Option<String>, Option<String>) {
        (None, None)
    }
    /// Download/decrypt an inbound image into vision attachments. Called once
    /// per batch and only after the debounce window has settled, so network
    /// latency cannot separate an image from its caption.
    async fn resolve_image_media(
        &self,
        _media: &MediaRef,
    ) -> Result<Vec<crate::ai::llm::ImageAttachment>, String> {
        Err("渠道不支持图片解析".to_string())
    }
    /// Materialize a file attachment to a local path for import. Channels
    /// that already store inbound files locally (weixin) return their path;
    /// feishu downloads on demand. Default: unsupported.
    async fn resolve_file_media(&self, _media: &MediaRef) -> Result<std::path::PathBuf, String> {
        Err("渠道不支持文件解析".to_string())
    }
    /// A remote turn started: channels with live UI (feishu card) create
    /// their progress surface; channels with delayed acks (weixin) arm them.
    /// session_id/prompt/kind are carried so card buttons can stop the turn
    /// or retry the same prompt without further context lookups.
    async fn progress_start(
        &self,
        _chat_id: &str,
        _preview: &str,
        _session_id: &str,
        _prompt: &str,
        _kind: ChatKind,
    ) -> Result<(), String> {
        Ok(())
    }
    /// Periodic progress snapshot (throttled by the router's driver).
    async fn progress_update(
        &self,
        _chat_id: &str,
        _progress: &RemoteProgress,
    ) -> Result<(), String> {
        Ok(())
    }
    /// Stop any transient "working" indicator started by `progress_start`.
    /// Called after the reply AND its outbound files went out: a slow upload
    /// must keep the typing indicator alive, so this cannot live in
    /// `progress_finish` (docs/remote-channels.md §7.7). Default: nothing to do.
    async fn progress_settle(&self, _chat_id: &str) {}

    /// The turn settled with its final reply. Return true when the channel
    /// rendered the reply into its own progress surface — the router then
    /// skips the plain-text send. Default false: plain text as usual.
    async fn progress_finish(
        &self,
        _chat_id: &str,
        _reply: &str,
        _ok: bool,
        _progress: &RemoteProgress,
    ) -> Result<bool, String> {
        Ok(false)
    }
}

/// One tool action line in a live progress surface.
#[derive(Clone)]
pub struct ProgressAction {
    pub id: String,
    pub label: String,
    pub done: bool,
    pub duration_ms: Option<i32>,
}

/// Aggregated in-flight turn state, snapshotted by the progress driver and
/// rendered by channels that support live UI.
#[derive(Clone, Default)]
pub struct RemoteProgress {
    pub actions: Vec<ProgressAction>,
    /// Accumulated streamed answer text (display-capped).
    pub answer_so_far: String,
    /// Accumulated reasoning text (display-capped).
    pub thinking_so_far: String,
    pub started: Option<Instant>,
    pub changed: bool,
    pub done: bool,
}

impl RemoteProgress {
    pub fn elapsed_secs(&self) -> u64 {
        self.started.map(|t| t.elapsed().as_secs()).unwrap_or(0)
    }

    fn append_capped(buf: &mut String, chunk: &str, cap: usize) {
        if buf.len() >= cap {
            return;
        }
        buf.push_str(chunk);
        if buf.len() > cap {
            // Truncate on a char boundary.
            let mut end = cap;
            while !buf.is_char_boundary(end) {
                end -= 1;
            }
            buf.truncate(end);
        }
    }
}

// ---------------------------------------------------------------- dedup

struct SeenStore {
    map: HashMap<String, i64>,
    path: PathBuf,
}

impl SeenStore {
    fn load(path: PathBuf) -> Self {
        let map = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<HashMap<String, i64>>(&s).ok())
            .unwrap_or_default();
        Self { map, path }
    }

    fn prune(&mut self, now: i64) {
        self.map.retain(|_, ts| now - *ts < SEEN_TTL_SECS);
        while self.map.len() > SEEN_CAP {
            if let Some(oldest) = self
                .map
                .iter()
                .min_by_key(|(_, ts)| *ts)
                .map(|(k, _)| k.clone())
            {
                self.map.remove(&oldest);
            } else {
                break;
            }
        }
    }

    async fn save(&self) {
        if let Ok(json) = serde_json::to_string(&self.map) {
            let tmp = self.path.with_extension("tmp");
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(&tmp, &self.path);
            }
        }
    }
}

static SEEN: Lazy<Mutex<Option<SeenStore>>> = Lazy::new(|| Mutex::new(None));

/// Returns true when the message id was already processed (duplicate).
#[allow(dead_code)]
async fn seen_check_and_mark(app_data_dir: &std::path::Path, id: &str) -> bool {
    let mut guard = SEEN.lock().await;
    let store = guard.get_or_insert_with(|| SeenStore::load(app_data_dir.join("remote_seen.json")));
    if store.map.contains_key(id) {
        return true;
    }
    let now = crate::core::time::now_iso();
    let ts = chrono::DateTime::parse_from_rfc3339(&now)
        .map(|d| d.timestamp())
        .unwrap_or(0);
    store.map.insert(id.to_string(), ts);
    store.prune(ts);
    store.save().await;
    false
}

/// Cross-device dedup backed by the synced `remote_msg_ids` table. The
/// per-device seen file above is empty on a standby device taking over the
/// channel connection, so without this the server-redelivered backlog its
/// predecessor already answered gets processed again. INSERT OR IGNORE +
/// rows_affected makes check-and-mark atomic on this device.
/// Returns true when the message id was already processed (duplicate).
async fn db_seen_check_and_mark(db: &SqlitePool, channel: &str, msg_id: &str) -> bool {
    let id = format!("{channel}:{msg_id}");
    let res = sqlx::query("INSERT OR IGNORE INTO remote_msg_ids (id, ts) VALUES (?, ?)")
        .bind(&id)
        .bind(crate::core::time::now_iso())
        .execute(db)
        .await;
    match res {
        Ok(done) => {
            if done.rows_affected() > 0 {
                prune_remote_msg_ids(db).await;
                false
            } else {
                true
            }
        }
        // Fail open: a dedup-store hiccup must not silence live messages.
        Err(e) => {
            tracing::warn!(error = %e, "remote msg_id dedup failed, processing anyway");
            false
        }
    }
}

/// The latest user-message timestamp already persisted for this remote chat.
/// This is the version-independent replay watermark: if the synced chat
/// history already contains a message from (or after) the inbound message's
/// send time, the inbound is an old redelivery and must not start a new turn.
async fn latest_remote_user_message_ms(
    db: &SqlitePool,
    channel: &str,
    chat_id: &str,
) -> Option<i64> {
    let session_id: Option<String> = sqlx::query_scalar(
        "SELECT session_id FROM remote_chats \
         WHERE channel = ? AND chat_id = ? AND session_id IS NOT NULL LIMIT 1",
    )
    .bind(channel)
    .bind(chat_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    let session_id = session_id.filter(|id| !id.is_empty())?;
    let created_at: Option<String> = sqlx::query_scalar(
        "SELECT created_at FROM chat_messages \
         WHERE session_id = ? AND role = 'user' ORDER BY created_at DESC LIMIT 1",
    )
    .bind(session_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    let created_at = created_at?;
    chrono::DateTime::parse_from_rfc3339(&created_at)
        .ok()
        .map(|timestamp| timestamp.timestamp_millis())
}

/// Result of comparing an inbound message with the chat's already-synced
/// latest user message. `Unknown` is the only case where the age guard is
/// allowed to run: `Newer` means the watermark positively identified a new
/// message, even if it is old because the device was offline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WatermarkVerdict {
    Replay,
    Newer,
    Unknown,
}

async fn watermark_verdict(db: &SqlitePool, msg: &InboundMessage) -> WatermarkVerdict {
    let Some(sent_at_ms) = msg.sent_at_ms else {
        return WatermarkVerdict::Unknown;
    };
    match latest_remote_user_message_ms(db, &msg.channel, &msg.chat_id).await {
        Some(latest) if sent_at_ms <= latest => WatermarkVerdict::Replay,
        Some(_) => WatermarkVerdict::Newer,
        None => WatermarkVerdict::Unknown,
    }
}

/// Whether the last-resort age guard applies. It must NOT run when the
/// watermark positively identified a newer message: a message sent while the
/// device was offline for an hour is still a new, unanswered message.
fn age_guard_exceeded(verdict: WatermarkVerdict, sent_at_ms: i64, now_ms: i64) -> bool {
    verdict == WatermarkVerdict::Unknown && now_ms - sent_at_ms > STALE_MESSAGE_MS
}

/// Content fingerprint used when the channel's message id is missing or
/// changes on redelivery. It deliberately includes the sender timestamp so a
/// user intentionally sending the same text twice at different times is not
/// collapsed, while the same message redelivered with a new/empty id is.
fn inbound_fingerprint(msg: &InboundMessage) -> Option<String> {
    let sent_at_ms = msg.sent_at_ms?;
    let mut hash = Sha256::new();
    for part in [
        msg.channel.as_str(),
        msg.chat_id.as_str(),
        msg.user_id.as_str(),
        msg.text.as_str(),
    ] {
        hash.update(part.as_bytes());
        hash.update([0u8]);
    }
    hash.update(sent_at_ms.to_le_bytes());
    for media in &msg.media {
        hash.update(media.kind.as_str().as_bytes());
        hash.update([0u8]);
        hash.update(media.name.as_bytes());
        hash.update([0u8]);
        match &media.locator {
            MediaLocator::Feishu {
                message_id,
                file_key,
                extra_image_keys,
            } => {
                hash.update(b"feishu");
                hash.update([0u8]);
                hash.update(message_id.as_bytes());
                hash.update([0u8]);
                hash.update(file_key.as_bytes());
                hash.update([0u8]);
                for key in extra_image_keys {
                    hash.update(key.as_bytes());
                    hash.update([0u8]);
                }
            }
            MediaLocator::Weixin { media, aeskey } => {
                hash.update(b"weixin");
                hash.update([0u8]);
                // The signed `full_url` changes between redeliveries; the
                // encrypted query param is the stable locator.
                let stable = media
                    .get("encrypt_query_param")
                    .and_then(|v| v.as_str())
                    .or_else(|| media.get("full_url").and_then(|v| v.as_str()))
                    .unwrap_or_default();
                hash.update(stable.as_bytes());
                hash.update([0u8]);
                if let Some(key) = media.get("aes_key").and_then(|v| v.as_str()) {
                    hash.update(key.as_bytes());
                    hash.update([0u8]);
                }
                if let Some(key) = aeskey {
                    hash.update(key.as_bytes());
                    hash.update([0u8]);
                }
            }
        }
    }
    Some(format!("{:x}", hash.finalize()))
}

/// Claim one inbound message for this device/turn. Returns false when the
/// ledger already has it running or done. A stale `running` claim (crashed
/// turn) is reclaimable after the TTL.
async fn claim_inbound(db: &SqlitePool, msg: &InboundMessage) -> Option<String> {
    if msg.msg_id.is_empty() {
        return Some(String::new());
    }
    let id = format!("{}:{}:{}:{}", msg.channel, msg.chat_id, msg.user_id, msg.msg_id);
    let now = crate::core::time::now_iso();
    let inserted = sqlx::query(
        "INSERT OR IGNORE INTO remote_inbound
         (id, channel, chat_id, user_id, msg_id, sent_at_ms, state, claimed_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, 'running', ?, ?)",
    )
    .bind(&id)
    .bind(&msg.channel)
    .bind(&msg.chat_id)
    .bind(&msg.user_id)
    .bind(&msg.msg_id)
    .bind(msg.sent_at_ms)
    .bind(&now)
    .bind(&now)
    .execute(db)
    .await
    .map(|r| r.rows_affected() == 1)
    .unwrap_or(false);
    if inserted {
        return Some(id);
    }

    let row: Option<(String, String)> =
        sqlx::query_as("SELECT state, updated_at FROM remote_inbound WHERE id = ?")
            .bind(&id)
            .fetch_optional(db)
            .await
            .ok()
            .flatten();
    let Some((state, updated_at)) = row else {
        return None;
    };
    let stale = chrono::DateTime::parse_from_rfc3339(&updated_at)
        .map(|t| (chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_seconds() > 900)
        .unwrap_or(true);
    if state == "done" || (state == "running" && !stale) {
        return None;
    }
    sqlx::query(
        "UPDATE remote_inbound SET state = 'running', claimed_at = ?, updated_at = ? WHERE id = ?",
    )
    .bind(&now)
    .bind(&now)
    .bind(&id)
    .execute(db)
    .await
    .ok()
    .map(|_| id)
}

fn turn_key_for(ledger_ids: &[String]) -> Option<String> {
    let mut ids: Vec<&str> = ledger_ids
        .iter()
        .map(String::as_str)
        .filter(|id| !id.is_empty())
        .collect();
    if ids.is_empty() {
        return None;
    }
    ids.sort_unstable();
    let mut hash = Sha256::new();
    for id in ids {
        hash.update(id.as_bytes());
        hash.update([0u8]);
    }
    Some(format!("{:x}", hash.finalize()))
}

async fn claim_turn(
    db: &SqlitePool,
    turn_key: &str,
    meta: &InboundMessage,
    ledger_ids: &[String],
) -> bool {
    let now = crate::core::time::now_iso();
    let msg_ids = ledger_ids
        .iter()
        .filter(|id| !id.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(",");
    let inserted = sqlx::query(
        "INSERT OR IGNORE INTO remote_turns
         (turn_key, channel, chat_id, msg_ids, state, claimed_at, updated_at)
         VALUES (?, ?, ?, ?, 'running', ?, ?)",
    )
    .bind(turn_key)
    .bind(&meta.channel)
    .bind(&meta.chat_id)
    .bind(&msg_ids)
    .bind(&now)
    .bind(&now)
    .execute(db)
    .await
    .map(|r| r.rows_affected() == 1)
    .unwrap_or(false);
    if inserted {
        for id in ledger_ids.iter().filter(|id| !id.is_empty()) {
            let _ = sqlx::query("UPDATE remote_inbound SET turn_key = ? WHERE id = ?")
                .bind(turn_key)
                .bind(id)
                .execute(db)
                .await;
        }
        return true;
    }
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT state, updated_at FROM remote_turns WHERE turn_key = ?")
            .bind(turn_key)
            .fetch_optional(db)
            .await
            .ok()
            .flatten();
    let Some((state, updated_at)) = row else {
        return false;
    };
    let stale = chrono::DateTime::parse_from_rfc3339(&updated_at)
        .map(|t| (chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_seconds() > 900)
        .unwrap_or(true);
    if state == "done" || (state == "running" && !stale) {
        return false;
    }
    sqlx::query(
        "UPDATE remote_turns SET state = 'running', claimed_at = ?, updated_at = ? WHERE turn_key = ?",
    )
    .bind(&now)
    .bind(&now)
    .bind(turn_key)
    .execute(db)
    .await
    .is_ok()
}

async fn finish_turn(db: &SqlitePool, turn_key: &str, state: &str) {
    let now = crate::core::time::now_iso();
    let _ = sqlx::query("UPDATE remote_turns SET state = ?, updated_at = ? WHERE turn_key = ?")
        .bind(state)
        .bind(&now)
        .bind(turn_key)
        .execute(db)
        .await;
}

async fn finish_inbound(db: &SqlitePool, ledger_ids: &[String], state: &str) {
    let now = crate::core::time::now_iso();
    for id in ledger_ids {
        if id.is_empty() {
            continue;
        }
        let _ = sqlx::query("UPDATE remote_inbound SET state = ?, updated_at = ? WHERE id = ?")
            .bind(state)
            .bind(&now)
            .bind(id)
            .execute(db)
            .await;
    }
}

/// Deletes are sync traffic, so pruning runs only when the table clearly
/// exceeds the cap, then removes the oldest overflow in one statement.
async fn prune_remote_msg_ids(db: &SqlitePool) {
    let count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM remote_msg_ids")
        .fetch_one(db)
        .await
        .unwrap_or(0);
    if count <= REMOTE_MSG_IDS_CAP {
        return;
    }
    let _ = sqlx::query(
        "DELETE FROM remote_msg_ids WHERE id IN (SELECT id FROM remote_msg_ids ORDER BY ts ASC LIMIT ?)",
    )
    .bind(count - REMOTE_MSG_IDS_CAP)
    .execute(db)
    .await;
}

// ---------------------------------------------------------------- rate limit

static RATE: Lazy<Mutex<HashMap<String, VecDeque<Instant>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

async fn rate_limited(user_key: &str) -> bool {
    let mut map = RATE.lock().await;
    let hits = map.entry(user_key.to_string()).or_default();
    let cutoff = Instant::now() - Duration::from_secs(60);
    while hits.front().is_some_and(|t| *t < cutoff) {
        hits.pop_front();
    }
    if hits.len() >= RATE_LIMIT_PER_MIN {
        return true;
    }
    hits.push_back(Instant::now());
    false
}

// ---------------------------------------------------------------- debounce

enum BatchPart {
    Text(String),
    Media(MediaRef),
}

struct PendingBatch {
    generation: u64,
    /// Original arrival order of text/media (a Feishu post is one Text plus
    /// its images, but separate WeChat messages interleave freely).
    parts: Vec<BatchPart>,
    /// `remote_inbound.id` rows claimed by this batch (empty for synthetic
    /// messages without a channel msg_id).
    ledger_ids: Vec<String>,
    meta: Option<InboundMessage>,
    last_sent_at: Option<i64>,
    sink: Arc<dyn ReplySink>,
}

/// Remove the accumulated payload from a batch. The caller either dispatches
/// it immediately (a timestamp gap) or schedules the normal debounce.
fn take_pending(
    entry: &mut PendingBatch,
) -> Option<(InboundMessage, Vec<BatchPart>, Vec<String>, Arc<dyn ReplySink>)> {
    if entry.parts.is_empty() {
        return None;
    }
    let parts = std::mem::take(&mut entry.parts);
    let ledger_ids = std::mem::take(&mut entry.ledger_ids);
    let meta = entry.meta.take();
    entry.last_sent_at = None;
    meta.map(|m| (m, parts, ledger_ids, entry.sink.clone()))
}

static PENDING: Lazy<Mutex<HashMap<String, PendingBatch>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

fn batch_gap_exceeded(current: Option<i64>, previous: Option<i64>) -> bool {
    current
        .zip(previous)
        .is_some_and(|(current, previous)| (current - previous).abs() > BATCH_MAX_GAP_MS)
}

fn startup_replay(age_ms: Option<i64>, elapsed: Duration) -> bool {
    age_ms.is_some_and(|age| age > STARTUP_MAX_AGE_MS && elapsed < STARTUP_REPLAY_WINDOW)
}

/// Quiet-window for one pending batch. Media-only waits for a possible
/// caption; any real text (with or without media) uses the normal window.
fn batch_delay(has_text: bool, has_image: bool, has_file: bool) -> u64 {
    if has_text && (has_image || has_file) {
        // Pair already assembled: dispatch shortly after the last part.
        DEBOUNCE_MS
    } else if has_file {
        FILE_DEBOUNCE_MS
    } else if has_image {
        MEDIA_DEBOUNCE_MS
    } else {
        // Text-only: leave a window for the image/file the user is sending.
        TEXT_DEBOUNCE_MS
    }
}

/// Entry point called by channel adapters for every inbound message.
pub async fn enqueue(app: AppHandle, msg: InboundMessage, sink: Arc<dyn ReplySink>) {
    let state = app.state::<AppState>();
    let db = state.db.clone();
    let role = state.remote.role_label().await;
    let cursor_len = if msg.channel == "weixin" {
        crate::core::settings_service::load_weixin_state(&db)
            .await
            .map(|state| state.get_updates_buf.chars().count())
            .unwrap_or(0)
    } else {
        0
    };
    let age_ms = msg
        .sent_at_ms
        .map(|sent| chrono::Utc::now().timestamp_millis() - sent);
    tracing::info!(
        channel = %msg.channel,
        chat_id = %msg.chat_id,
        msg_id = %msg.msg_id,
        sent_at_ms = ?msg.sent_at_ms,
        age_ms = ?age_ms,
        role = %role,
        cursor_len,
        relay_connected = crate::sync::engine::relay_connected(),
        initial_sync_ready = crate::sync::engine::initial_sync_ready(),
        media = msg.media.len(),
        "remote inbound message"
    );

    // Message-id dedup is the durable `remote_inbound` claim below. The old
    // `remote_msg_ids` insert happened before that claim, so a `failed` turn
    // could never be retried: the id was already marked seen. Keep
    // `remote_msg_ids` only for the no-msg-id fingerprint path.
    if msg.msg_id.is_empty() {
        if let Some(fingerprint) = inbound_fingerprint(&msg) {
            let key = format!("fp:{fingerprint}");
            if db_seen_check_and_mark(&db, &msg.channel, &key).await {
                tracing::info!(
                    channel = %msg.channel, msg_id = %msg.msg_id, chat_id = %msg.chat_id,
                    "dropping redelivered remote message (fingerprint)"
                );
                return;
            }
        }
    }

    // Session watermark: the chat history already has this message (or a
    // newer one). This is the primary guard against cursor rollback and
    // msg_id churn; it does not depend on either.
    let watermark = watermark_verdict(&db, &msg).await;
    if watermark == WatermarkVerdict::Replay {
        let latest = latest_remote_user_message_ms(&db, &msg.channel, &msg.chat_id).await;
        tracing::info!(
            channel = %msg.channel, msg_id = %msg.msg_id, chat_id = %msg.chat_id,
            sent_at_ms = ?msg.sent_at_ms, latest_user_ms = ?latest,
            "dropping replayed remote message (session watermark)"
        );
        return;
    }

    // Startup backlog: the account sync may still be applying changes, so a
    // stale watermark can label yesterday's messages as "Newer". During the
    // first two minutes, do not let a multi-hour-old backlog into a turn.
    if startup_replay(age_ms, state.started_at.elapsed()) {
        tracing::info!(
            channel = %msg.channel, msg_id = %msg.msg_id, age_ms = age_ms,
            "dropping startup replay backlog"
        );
        return;
    }

    // The age guard is ONLY a fallback for the state where the watermark
    // cannot answer. If the watermark positively identified a newer message,
    // process it even when it is old: the user sent it while every device was
    // offline and expects the answer.
    if let Some(sent_ms) = msg.sent_at_ms {
        let now_ms = chrono::Utc::now().timestamp_millis();
        if age_guard_exceeded(watermark, sent_ms, now_ms) {
            tracing::info!(
                channel = %msg.channel, msg_id = %msg.msg_id, age_ms = now_ms - sent_ms,
                role = %role, cursor_len,
                "dropping stale remote message (no session watermark)"
            );
            return;
        }
    }

    // Durable claim BEFORE the batch. A redelivered/crashed message id is
    // ignored even if the channel/watermark path missed it; the claim is
    // released as done/failed when the turn settles.
    let Some(ledger_id) = claim_inbound(&db, &msg).await else {
        tracing::info!(
            channel = %msg.channel, msg_id = %msg.msg_id,
            "dropping inbound already claimed by the durable ledger"
        );
        return;
    };

    // Include the sender: a group's A-image must not be merged into B's text.
    let key = format!("{}:{}:{}", msg.channel, msg.chat_id, msg.user_id);
    let app_for_flush = app.clone();
    let mut flushed = None;
    let mut generation_floor = 0u64;
    let (generation, delay) = {
        let mut map = PENDING.lock().await;
        // A redelivered backlog can arrive in one burst but carry timestamps
        // minutes/hours apart. Only apply the sender-timestamp split to old
        // (replayed) messages: a live file upload may carry a timestamp
        // several seconds older than the caption typed after it, and must
        // still merge with that caption.
        // Timestamp-gap splitting exists only to break up a startup replay
        // burst. WeChat can deliver one "send together" text+image action as
        // two messages with independent create_time values; after startup
        // those must never be split by their sender timestamps.
        let now_ms = chrono::Utc::now().timestamp_millis();
        let current_old = age_ms.is_some_and(|age| age > STALE_MESSAGE_MS);
        let previous_old = map
            .get(&key)
            .and_then(|entry| entry.last_sent_at)
            .is_some_and(|sent| now_ms - sent > STALE_MESSAGE_MS);
        let startup_backlog = state.started_at.elapsed() < STARTUP_REPLAY_WINDOW;
        let gap = startup_backlog
            && current_old
            && previous_old
            && batch_gap_exceeded(
                msg.sent_at_ms,
                map.get(&key).and_then(|entry| entry.last_sent_at),
            );
        if gap {
            if let Some(mut old) = map.remove(&key) {
                generation_floor = old.generation;
                flushed = take_pending(&mut old);
            }
        }
        let entry = map.entry(key.clone()).or_insert_with(|| PendingBatch {
            generation: 0,
            parts: Vec::new(),
            ledger_ids: Vec::new(),
            meta: None,
            last_sent_at: None,
            sink: sink.clone(),
        });
        // Never reuse the removed entry's generation: its timer is still in
        // flight and must not consume the new segment.
        entry.generation = entry.generation.max(generation_floor) + 1;
        entry.parts.push(BatchPart::Text(msg.text.clone()));
        for media in msg.media.clone() {
            entry.parts.push(BatchPart::Media(media));
        }
        if !ledger_id.is_empty() {
            entry.ledger_ids.push(ledger_id);
        }
        entry.meta = Some(msg);
        entry.last_sent_at = entry.meta.as_ref().and_then(|m| m.sent_at_ms);
        entry.sink = sink;
        // Media-only waits for a possible caption; text (with or without
        // media) uses the normal quiet window. Empty text entries from media
        // messages must not count as a caption.
        let has_text = entry.parts.iter().any(|part| {
            matches!(part, BatchPart::Text(text) if !text.trim().is_empty())
        });
        let has_image = entry.parts.iter().any(|part| {
            matches!(part, BatchPart::Media(MediaRef { kind: MediaKind::Image, .. }))
        });
        let has_file = entry.parts.iter().any(|part| {
            matches!(part, BatchPart::Media(MediaRef { kind: MediaKind::File, .. }))
        });
        (entry.generation, batch_delay(has_text, has_image, has_file))
    };

    if let Some((meta, parts, ledger_ids, sink)) = flushed {
        tokio::spawn(async move {
            process_batch(app_for_flush, meta, parts, ledger_ids, sink).await;
        });
    }

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(delay)).await;
        let ready = {
            let mut map = PENDING.lock().await;
            map.get_mut(&key).and_then(|entry| {
                if entry.generation == generation {
                    let parts = std::mem::take(&mut entry.parts);
                    let ledger_ids = std::mem::take(&mut entry.ledger_ids);
                    let meta = entry.meta.clone();
                    meta.map(|m| (m, parts, ledger_ids, entry.sink.clone()))
                } else {
                    None
                }
            })
        };
        if let Some((meta, parts, ledger_ids, sink)) = ready {
            process_batch(app, meta, parts, ledger_ids, sink).await;
        }
    });
}

// ---------------------------------------------------------------- processing

/// Per-chat serialization: tasks waiting on the lock ARE the message queue.
/// A second message mid-turn no longer gets "稍后再发" — it waits its turn
/// and is answered automatically when the running turn completes.
static CHAT_LOCKS: Lazy<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

async fn chat_lock(key: &str) -> Arc<tokio::sync::Mutex<()>> {
    CHAT_LOCKS
        .lock()
        .await
        .entry(key.to_string())
        .or_default()
        .clone()
}

fn push_block(target: &mut String, block: &str) {
    if block.trim().is_empty() {
        return;
    }
    if !target.trim().is_empty() {
        target.push_str("\n\n");
    }
    target.push_str(block.trim());
}

async fn process_batch(
    app: AppHandle,
    meta: InboundMessage,
    parts: Vec<BatchPart>,
    ledger_ids: Vec<String>,
    sink: Arc<dyn ReplySink>,
) {
    if parts.is_empty() {
        return;
    }
    let state = app.state::<AppState>();
    let db = state.db.clone();

    if rate_limited(&format!("{}:{}", meta.channel, meta.user_id)).await {
        let _ = sink
            .send_text(&meta.chat_id, "消息太频繁了，请稍后再试。")
            .await;
        finish_inbound(&db, &ledger_ids, "done").await;
        return;
    }

    // Pairing stays outside the queue: a stranger gets the pairing notice
    // immediately instead of waiting behind a running turn.
    let (session_id, grant) = match ensure_paired(&db, &app, &meta, &sink).await {
        Ok(Some(pair)) => pair,
        Ok(None) => {
            // Pairing notice already handled; the message is consumed.
            finish_inbound(&db, &ledger_ids, "done").await;
            return;
        }
        Err(e) => {
            tracing::error!(error = %e, "remote pairing check failed");
            finish_inbound(&db, &ledger_ids, "failed").await;
            return;
        }
    };
    // Read-only members (the default for group pairings) get a per-turn
    // read-only tool set; the shared group session itself keeps full tools.
    let tools_override: Option<Vec<String>> = if grant == GRANT_READONLY {
        serde_json::from_str(READONLY_TOOLS_JSON).ok()
    } else {
        None
    };

    // One turn per message set. The per-message ledger already claimed each
    // id; this closes the window where two batches with the same ids race.
    let turn_key = turn_key_for(&ledger_ids);
    if let Some(key) = &turn_key {
        if !claim_turn(&db, key, &meta, &ledger_ids).await {
            tracing::info!(turn_key = %key, "dropping already-claimed remote turn");
            finish_inbound(&db, &ledger_ids, "done").await;
            return;
        }
    }

    let lock = chat_lock(&format!("{}:{}", meta.channel, meta.chat_id)).await;
    let _guard = match lock.clone().try_lock_owned() {
        Ok(g) => g,
        Err(_) => {
            let _ = sink
                .send_text(&meta.chat_id, "收到，上一条还在处理——这条已排队，完成后自动回复你。")
                .await;
            lock.lock_owned().await
        }
    };

    // Resolve media AFTER the debounce window, in the original arrival order.
    // `agent_text` keeps the local path + extracted document body for the
    // model; `display_text` is the compact body persisted for the UI and
    // sync, so a machine-local path never appears in a user bubble.
    let mut images = Vec::new();
    let mut file_attachments: Vec<serde_json::Value> = Vec::new();
    let mut agent_text = String::new();
    let mut display_text = String::new();
    let mut prompt_text = String::new();
    let mut useful = false;
    let downloads = state.app_data_dir.join("remote_downloads");

    for part in parts {
        match part {
            BatchPart::Text(text) => {
                if !text.trim().is_empty() {
                    useful = true;
                    push_block(&mut agent_text, text.trim());
                    push_block(&mut display_text, text.trim());
                    push_block(&mut prompt_text, text.trim());
                }
            }
            BatchPart::Media(media) => match media.kind {
                MediaKind::Image => match sink.resolve_image_media(&media).await {
                    Ok(mut resolved) if !resolved.is_empty() => {
                        useful = true;
                        images.append(&mut resolved);
                    }
                    Ok(_) => {
                        let note = format!("（图片《{}》内容为空，已跳过）", media.name);
                        push_block(&mut agent_text, &note);
                        push_block(&mut prompt_text, &note);
                        push_block(&mut display_text, "（一张图片内容为空，已跳过）");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, image = %media.name, "remote image resolve failed");
                        let note = format!("（图片《{}》下载失败：{e}）", media.name);
                        push_block(&mut agent_text, &note);
                        push_block(&mut prompt_text, &note);
                        push_block(&mut display_text, "（一张图片下载失败，已跳过）");
                    }
                },
                MediaKind::File => match sink.resolve_file_media(&media).await {
                    Ok(path) => {
                        // Keep every inbound file under remote_downloads so the
                        // agent can come back to it in later turns (feishu
                        // resolves into a volatile temp dir).
                        let path = if path.starts_with(&downloads) {
                            path
                        } else {
                            let target = downloads
                                .join(format!("{}_{}", uuid::Uuid::new_v4(), media.name));
                            match std::fs::create_dir_all(&downloads)
                                .and_then(|_| std::fs::copy(&path, &target).map(|_| ()))
                            {
                                Ok(()) => {
                                    let _ = std::fs::remove_file(&path);
                                    target
                                }
                                Err(_) => path,
                            }
                        };
                        let cache_dir = downloads.join(".siku-cache");
                        let read_path = path.clone();
                        let extracted = tokio::task::spawn_blocking(move || {
                            crate::core::document_text::read_document_text(&read_path, &cache_dir)
                        })
                        .await
                        .map_err(|e| format!("task join: {e}"))
                        .and_then(|r| r);
                        let extracted_text = match &extracted {
                            Ok(content) => content.clone(),
                            Err(_) => String::new(),
                        };
                        let body = match &extracted {
                            Ok(content) => format!("文件内容提取如下：\n{content}"),
                            Err(e) => format!(
                                "文件内容提取失败（{e}）。可建议用户在思库应用内处理；\
                                 若用户要求入库且是文献 PDF，可用 paper_import 导入该路径。"
                            ),
                        };
                        // Content-addressed artifact: the synced message only
                        // carries a device-independent URI; the blob itself is
                        // transferred by the existing blob sync channel.
                        let mut artifact_uri = None;
                        if let Ok(bytes) = std::fs::read(&path) {
                            let ext = path
                                .extension()
                                .and_then(|e| e.to_str())
                                .unwrap_or("bin")
                                .to_ascii_lowercase();
                            let hash = crate::file_store::sha256_hex(&bytes);
                            if crate::file_store::write_blob(&state.app_data_dir, &bytes, &ext)
                                .is_ok()
                            {
                                let uri =
                                    crate::core::artifact_service::artifact_uri(&hash, &ext);
                                let _ = sqlx::query(
                                    "INSERT OR IGNORE INTO remote_artifacts
                                     (artifact_id, session_id, channel, chat_id, msg_id, name, mime,
                                      size, hash, ext, extracted_text, created_at)
                                     VALUES (?, ?, ?, ?, ?, ?, '', ?, ?, ?, ?, ?)",
                                )
                                .bind(&uri)
                                .bind(&session_id)
                                .bind(&meta.channel)
                                .bind(&meta.chat_id)
                                .bind(&meta.msg_id)
                                .bind(&media.name)
                                .bind(bytes.len() as i64)
                                .bind(&hash)
                                .bind(&ext)
                                .bind(&extracted_text)
                                .bind(crate::core::time::now_iso())
                                .execute(&db)
                                .await;
                                artifact_uri = Some(uri.clone());
                                file_attachments.push(serde_json::json!({
                                    "kind": "file",
                                    "name": media.name,
                                    "mime": crate::core::file_service::mime_guess(&media.name)
                                        .unwrap_or_else(|| format!("application/{ext}")),
                                    "size": bytes.len(),
                                    "artifact_id": uri,
                                    "sha256": hash,
                                    "status": "ok",
                                }));
                            }
                        }
                        let has_text = !agent_text.trim().is_empty();
                        let stored_note = match &artifact_uri {
                            Some(uri) => format!("（附件《{}》，引用：{uri}）", media.name),
                            None => format!("（附件《{}》）", media.name),
                        };
                        let prompt_note = match &artifact_uri {
                            Some(uri) => format!(
                                "（用户发来文件《{}》，已保存为 {uri}；本机路径：{}。{body}）",
                                media.name,
                                path.display()
                            ),
                            None => format!(
                                "（用户发来文件《{}》，已保存到本机：{}。{body}）",
                                media.name,
                                path.display()
                            ),
                        };
                        let prompt_note = if has_text {
                            prompt_note.replace("（用户发来文件", "（附件")
                        } else {
                            prompt_note
                        };
                        useful = true;
                        push_block(&mut agent_text, &stored_note);
                        push_block(&mut prompt_text, &prompt_note);
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, file = %media.name, "remote file download failed");
                        let note = format!("（{}：{e}）", file_fallback_text(&meta, &media.name));
                        push_block(&mut agent_text, &note);
                        push_block(&mut prompt_text, &note);
                        push_block(
                            &mut display_text,
                            &format!("（附件《{}》处理失败）", media.name),
                        );
                    }
                },
            },
        }
    }

    if !useful {
        let note = if display_text.trim().is_empty() {
            "没有可处理的媒体内容，请重发一次。".to_string()
        } else {
            display_text.trim().to_string()
        };
        let _ = sink.send_text(&meta.chat_id, &note).await;
        if let Some(key) = &turn_key {
            finish_turn(&db, key, "done").await;
        }
        finish_inbound(&db, &ledger_ids, "done").await;
        return;
    }
    if agent_text.trim().is_empty() && !images.is_empty() {
        agent_text = "[图片]".to_string();
        display_text = "[图片]".to_string();
        prompt_text = "[图片]".to_string();
    }

    let channel_label = channel_label(&meta.channel);
    let preview: String = display_text.chars().take(50).collect();
    crate::commands::pet::pet_report(
        &app,
        "remote",
        &format!("{channel_label}远程会话"),
        "running",
        Some(preview.clone()),
    );

    // Live progress surface (feishu card / weixin delayed ack): created
    // before the turn starts so early events are not missed.
    let _ = sink
        // `preview` is human-facing; `prompt` must stay the full agent prompt
        // so a Feishu card retry does not lose hidden file context.
        .progress_start(&meta.chat_id, &preview, &session_id, &prompt_text, meta.kind)
        .await;
    let driver = spawn_progress_driver(&app, &session_id, &meta.chat_id, sink.clone());

    // run_agent_turn spawns the turn and returns immediately, so remember
    // where the assistant history ends and poll for the row the turn appends
    // when it finishes. Grabbing the latest row right away would return the
    // PREVIOUS turn's reply.
    let baseline: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(rowid), 0) FROM chat_messages WHERE session_id = ? AND role = 'assistant'",
    )
    .bind(&session_id)
    .fetch_one(&db)
    .await
    .unwrap_or(0);

    // Files the agent registers during this turn land here; the engine writes
    // them into the assistant row and we upload them after the reply goes out.
    let outbound = new_outbound_queue();
    let result = crate::commands::agent::run_agent_turn_with_display(
        &state,
        &app,
        session_id.clone(),
        agent_text.clone(),
        Some(display_text.clone()),
        Some(prompt_text.clone()),
        if file_attachments.is_empty() { None } else { Some(file_attachments) },
        if images.is_empty() { None } else { Some(images) },
        tools_override,
        Some(outbound.clone()),
    )
    .await;
    let turn_state = if result.is_ok() { "done" } else { "failed" };
    if let Some(key) = &turn_key {
        finish_turn(&db, key, turn_state).await;
    }
    finish_inbound(&db, &ledger_ids, turn_state).await;

    let mut reply_row: Option<ReplyRow> = None;
    let reply = match result {
        Ok(_) => match wait_for_new_reply(&db, &session_id, baseline).await {
            Some(row) if !row.content.trim().is_empty() => {
                reply_row = Some(row);
                reply_row.as_ref().map(|r| r.content.clone()).unwrap_or_default()
            }
            _ => "（回复生成超时，请稍后在思库对话中查看，或追问一次）".to_string(),
        },
        Err(e) if e.contains("正在生成中") => {
            // The per-chat lock serializes remote turns, so reaching here
            // means the session is being driven from the siku UI itself.
            "这个会话正在思库应用内处理消息，请稍后再发。".to_string()
        }
        Err(e) => {
            tracing::error!(error = %e, "remote agent turn failed");
            format!("处理失败：{e}")
        }
    };

    let progress = driver.finish();
    let ok = !reply.starts_with("处理失败") && !reply.starts_with("（回复生成超时");
    // Channels with live UI render the reply into their progress surface;
    // everyone else gets the plain-text send.
    let handled = sink
        .progress_finish(&meta.chat_id, &reply, ok, &progress)
        .await
        .unwrap_or(false);

    let report_state = if ok { "done" } else { "failed" };
    crate::commands::pet::pet_report(&app, "remote", &format!("{channel_label}远程会话"), report_state, None);

    if !handled {
        if let Err(e) = sink.send_text(&meta.chat_id, &reply).await {
            tracing::error!(error = %e, "remote reply send failed");
        }
    }

    // Registered files go out after the text so the answer is read first
    // (§7.7). A cancelled/failed turn persisted them as `failed`, so nothing
    // is pending there and this is a no-op.
    if ok {
        if let Some(row) = reply_row.as_ref() {
            if let Some(atts) = row.attachments.as_deref() {
                let pending = pending_files_from_attachments(atts);
                if !pending.is_empty() {
                    deliver_outbound_files(
                        &app, &sink, &db, &session_id, &meta.chat_id, &row.id, pending,
                    )
                    .await;
                }
            }
        }
    }

    // Only now stop the "working" indicator: during a large upload it is the
    // single piece of feedback the phone has (§7.7).
    sink.progress_settle(&meta.chat_id).await;
}

/// The agent persists its assistant message only when the turn ends, so a
/// new row past `baseline` is exactly "this turn finished" — including
/// failures, which are persisted with an ❌ marker. Polls until the timeout.
/// The assistant row this turn wrote: id, content, attachments JSON. The id
/// is needed to freeze attachment delivery status after the files are sent
/// (docs/remote-channels.md §7.4), so it is selected alongside the content.
struct ReplyRow {
    id: String,
    content: String,
    attachments: Option<String>,
}

async fn wait_for_new_reply(db: &SqlitePool, session_id: &str, baseline: i64) -> Option<ReplyRow> {
    let deadline = Instant::now() + Duration::from_secs(REPLY_TIMEOUT_SECS);
    loop {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let row = sqlx::query_as::<_, (String, String, Option<String>)>(
            "SELECT id, content, attachments FROM chat_messages
             WHERE session_id = ? AND role = 'assistant' AND rowid > ?
             ORDER BY rowid DESC LIMIT 1",
        )
        .bind(session_id)
        .bind(baseline)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .map(|(id, content, attachments)| ReplyRow { id, content, attachments });
        if row.is_some() {
            return row;
        }
        if Instant::now() >= deadline {
            tracing::warn!(session_id, "remote reply wait timed out");
            return None;
        }
    }
}

/// Upload + send the files a successful turn registered, then freeze their
/// final status into the assistant row. Failures are recorded per file rather
/// than dropped: the siku conversation is the ledger (§7.4).
async fn deliver_outbound_files(
    app: &AppHandle,
    sink: &Arc<dyn ReplySink>,
    db: &SqlitePool,
    session_id: &str,
    chat_id: &str,
    row_id: &str,
    files: Vec<OutboundFile>,
) {
    let mut items: Vec<(OutboundFile, Option<String>)> = Vec::with_capacity(files.len());
    for file in files {
        let err = match sink.send_media(chat_id, &file).await {
            Ok(()) => None,
            Err(e) => {
                tracing::warn!(chat_id, name = %file.name, error = %e, "remote outbound file failed");
                Some(e)
            }
        };
        items.push((file, err));
    }
    let json = settled_attachments_json(&items);
    if let Err(e) = sqlx::query("UPDATE chat_messages SET attachments = ? WHERE id = ?")
        .bind(&json)
        .bind(row_id)
        .execute(db)
        .await
    {
        tracing::warn!(error = %e, "failed to persist outbound attachment status");
        return;
    }
    // The open chat panel reloaded its history when the turn ended — i.e.
    // BEFORE these uploads ran — so it still shows `pending`. Push the
    // settled attachments so the bubble flips to 已发送 without a manual
    // tab switch (or a full history reload).
    let _ = app.emit(
        "chat:message_updated",
        serde_json::json!({
            "session_id": session_id,
            "message_id": row_id,
            "attachments": json,
        }),
    );
}

// ---------------------------------------------------------------- progress

/// Progress tick interval: feishu card PATCHes are throttled to this pace.
const PROGRESS_TICK_MS: u64 = 2000;
const ANSWER_CAP: usize = 1500;
const THINKING_CAP: usize = 400;

/// Listens to the turn's "agent:event" stream, aggregates a RemoteProgress,
/// and pushes throttled snapshots to the sink. finish() tears everything
/// down and returns the final state for progress_finish.
struct ProgressDriver {
    state: Arc<std::sync::Mutex<RemoteProgress>>,
    task: tokio::task::JoinHandle<()>,
    listener_id: tauri::EventId,
    app: AppHandle,
}

impl ProgressDriver {
    fn finish(self) -> RemoteProgress {
        use tauri::Listener as _;
        self.app.unlisten(self.listener_id);
        self.task.abort();
        let mut st = self.state.lock().map(|s| s.clone()).unwrap_or_default();
        st.done = true;
        st
    }
}

fn spawn_progress_driver(
    app: &AppHandle,
    session_id: &str,
    chat_id: &str,
    sink: Arc<dyn ReplySink>,
) -> ProgressDriver {
    use tauri::Listener as _;
    let state = Arc::new(std::sync::Mutex::new(RemoteProgress {
        started: Some(Instant::now()),
        ..Default::default()
    }));
    let state_cb = state.clone();
    let sid = session_id.to_string();
    let listener_id = app.listen("agent:event", move |event| {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(event.payload()) else {
            return;
        };
        if v.get("session_id").and_then(|s| s.as_str()) != Some(sid.as_str()) {
            return;
        }
        let Ok(ev) = serde_json::from_value::<crate::ai::agent::engine::AgentEvent>(v) else {
            return;
        };
        let Ok(mut st) = state_cb.lock() else { return };
        use crate::ai::agent::engine::AgentEvent as E;
        match ev {
            E::Delta { content, .. } => {
                RemoteProgress::append_capped(&mut st.answer_so_far, &content, ANSWER_CAP)
            }
            E::Reasoning { content, .. } => {
                RemoteProgress::append_capped(&mut st.thinking_so_far, &content, THINKING_CAP)
            }
            E::ToolCall {
                tool_call_id,
                tool_name,
                tool_args,
                ..
            } => st.actions.push(ProgressAction {
                id: tool_call_id,
                label: tool_label(&tool_name, &tool_args),
                done: false,
                duration_ms: None,
            }),
            E::ToolResult {
                tool_call_id,
                duration_ms,
                ..
            } => {
                if let Some(a) = st.actions.iter_mut().rev().find(|a| a.id == tool_call_id) {
                    a.done = true;
                    a.duration_ms = duration_ms;
                }
            }
            E::Done { .. } | E::Cancelled { .. } | E::Error { .. } => st.done = true,
            _ => {}
        }
        st.changed = true;
    });
    let state_tick = state.clone();
    let chat = chat_id.to_string();
    let task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(PROGRESS_TICK_MS));
        // The first interval tick fires immediately; skip it so the first
        // snapshot goes out after a real tick.
        interval.tick().await;
        loop {
            interval.tick().await;
            let snapshot = {
                let Ok(mut st) = state_tick.lock() else { break };
                if st.done {
                    break;
                }
                if st.changed {
                    st.changed = false;
                    Some(st.clone())
                } else {
                    None
                }
            };
            if let Some(p) = snapshot {
                let _ = sink.progress_update(&chat, &p).await;
            }
        }
    });
    ProgressDriver {
        state,
        task,
        listener_id,
        app: app.clone(),
    }
}

/// Human-facing label for a tool call line in the live progress surface.
fn tool_label(tool: &str, args: &serde_json::Value) -> String {
    let pick = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| args.get(*k).and_then(|v| v.as_str()))
            .unwrap_or_default()
    };
    let with = |name: &str, detail: &str| {
        if detail.is_empty() {
            name.to_string()
        } else {
            let c: String = detail.chars().take(24).collect();
            format!("{name}「{}」", c)
        }
    };
    match tool {
        "paper_read" => with("阅读文献", pick(&["title", "paper_id", "id"])),
        "paper_search" | "search_library" => with("检索文献库", pick(&["query", "keyword"])),
        "note_read" => with("阅读笔记", pick(&["title", "note_id", "id"])),
        "knowledge_query" => with("查知识库", pick(&["query"])),
        "memory_read" | "memory_recall" => "回忆".to_string(),
        "web_search" => with("联网搜索", pick(&["query"])),
        "web_fetch" => "打开网页".to_string(),
        "task_list" | "task_output" => "查看任务".to_string(),
        other => other.to_string(),
    }
}

fn channel_label(channel: &str) -> String {
    match channel_kind(channel) {
        "feishu" => "飞书".to_string(),
        "weixin" => "微信".to_string(),
        other => other.to_string(),
    }
}

/// Fallback reply for file messages that cannot be auto-imported
/// (non-PDF, or import failure).
fn file_fallback_text(meta: &InboundMessage, name: &str) -> String {
    match channel_kind(&meta.channel) {
        "feishu" => format!(
            "已收到文件《{name}》。在思库对话列表右键本会话 →「查看群文件」，可将其导入文献库或另存到本地。"
        ),
        _ => format!(
            "已收到文件《{name}》，已保存到思库数据目录，请在思库应用内处理。"
        ),
    }
}

/// Channel family without the bot-instance suffix ("feishu:cli_x" → "feishu").
pub fn channel_kind(channel: &str) -> &str {
    channel.split(':').next().unwrap_or(channel)
}

// ---------------------------------------------------------------- pairing

/// Returns Ok(Some((session_id, grant_level))) when the SENDER is paired;
/// Ok(None) when not approved yet (notice handled here). Pairing is per
/// (channel, chat_id, user_id): in a group every member pairs individually,
/// while the backing chat session is shared across the group's approved
/// members. New pairings default to readonly in groups, full in private.
async fn ensure_paired(
    db: &SqlitePool,
    app: &AppHandle,
    meta: &InboundMessage,
    sink: &Arc<dyn ReplySink>,
) -> Result<Option<(String, String)>, String> {
    let row = sqlx::query_as::<_, (String, i64, Option<String>, String)>(
        "SELECT user_name, approved, session_id, grant_level FROM remote_chats
         WHERE channel = ? AND chat_id = ? AND user_id = ?",
    )
     .bind(&meta.channel)
    .bind(&meta.chat_id)
    .bind(&meta.user_id)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("db error: {e}"))?;

    let now = crate::core::time::now_iso();
    let Some((user_name, approved, session_id, grant_level)) = row else {
        // Resolve display names once at pairing time (feishu directory APIs);
        // failures fall back to raw ids without blocking pairing.
        let (_chat_name, resolved_user) = sink
            .resolve_names(&meta.chat_id, &meta.user_id, meta.kind)
            .await;
        let display_user = resolved_user
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| meta.user_name.clone());
        let default_grant = match meta.kind {
            ChatKind::Group => GRANT_READONLY,
            ChatKind::Private => GRANT_FULL,
        };
        sqlx::query(
            "INSERT INTO remote_chats (channel, chat_id, user_id, user_name, session_id, approved, grant_level, created_at, updated_at)
             VALUES (?, ?, ?, ?, NULL, 0, ?, ?, ?)",
        )
         .bind(&meta.channel)
        .bind(&meta.chat_id)
        .bind(&meta.user_id)
        .bind(&display_user)
        .bind(default_grant)
        .bind(&now)
        .bind(&now)
        .execute(db)
        .await
        .map_err(|e| format!("db error: {e}"))?;
        let _ = app.emit("remote:pairing", ());
        let who = if display_user.is_empty() { "飞友" } else { &display_user };
        let _ = sink
            .send_text(
                &meta.chat_id,
                &format!("{who} 你好，我是思库远程助手。请在思库「设置 → 远程接入」中确认配对后再开始对话。群内每位成员需分别确认。"),
            )
            .await;
        return Ok(None);
    };

    if approved == 0 {
        // Throttle the reminder: only re-notify when the row is stale.
        let stale = sqlx::query_scalar::<_, String>(
            "SELECT updated_at FROM remote_chats WHERE channel = ? AND chat_id = ? AND user_id = ?",
        )
         .bind(&meta.channel)
        .bind(&meta.chat_id)
        .bind(&meta.user_id)
        .fetch_one(db)
        .await
        .map_err(|e| format!("db error: {e}"))?;
        let stale_secs = chrono::DateTime::parse_from_rfc3339(&stale)
            .map(|t| (chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_seconds())
            .unwrap_or(i64::MAX);
        if stale_secs >= PAIRING_NOTICE_INTERVAL_SECS {
            let _ = sqlx::query(
                "UPDATE remote_chats SET updated_at = ?, user_name = ? WHERE channel = ? AND chat_id = ? AND user_id = ?",
            )
            .bind(&now)
            .bind(&meta.user_name)
             .bind(&meta.channel)
            .bind(&meta.chat_id)
            .bind(&meta.user_id)
            .execute(db)
            .await;
            let _ = sink
                .send_text(
                    &meta.chat_id,
                    "配对尚未确认。请在思库「设置 → 远程接入」中确认后再开始对话。",
                )
                .await;
        }
        return Ok(None);
    }

    // Keep the sender display name fresh.
    if !meta.user_name.is_empty() && user_name != meta.user_name {
        let _ = sqlx::query(
            "UPDATE remote_chats SET user_name = ?, updated_at = ? WHERE channel = ? AND chat_id = ? AND user_id = ?",
        )
        .bind(&meta.user_name)
        .bind(&now)
         .bind(&meta.channel)
        .bind(&meta.chat_id)
        .bind(&meta.user_id)
        .execute(db)
        .await;
    }

    if let Some(sid) = session_id.filter(|s| !s.is_empty()) {
        maybe_refresh_session_title(db, meta, sink, &sid).await;
        return Ok(Some((sid, grant_level)));
    }

    // The chat session is shared: reuse any approved member's session for
    // this (channel, chat_id) before creating a new one.
    let existing = sqlx::query_scalar::<_, String>(
        "SELECT session_id FROM remote_chats
         WHERE channel = ? AND chat_id = ? AND session_id IS NOT NULL AND session_id != '' LIMIT 1",
    )
     .bind(&meta.channel)
    .bind(&meta.chat_id)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("db error: {e}"))?;
    let sid = match existing {
        Some(s) => {
            maybe_refresh_session_title(db, meta, sink, &s).await;
            s
        }
        None => {
            let (chat_name, resolved_user) = sink
                .resolve_names(&meta.chat_id, &meta.user_id, meta.kind)
                .await;
            create_remote_session(db, meta, chat_name, resolved_user).await?
        }
    };
    // Stamp every row of the chat so later members skip the lookup.
    sqlx::query("UPDATE remote_chats SET session_id = ?, updated_at = ? WHERE channel = ? AND chat_id = ?")
        .bind(&sid)
        .bind(&now)
         .bind(&meta.channel)
        .bind(&meta.chat_id)
        .execute(db)
        .await
        .map_err(|e| format!("db error: {e}"))?;
    Ok(Some((sid, grant_level)))
}

/// Sessions created before directory-name resolution carry raw-id titles
/// like "飞书 · oc_xxx". Refresh once: the raw id in the title is the stale
/// marker, and a successful rename removes it so this never fires twice.
async fn maybe_refresh_session_title(
    db: &SqlitePool,
    meta: &InboundMessage,
    sink: &Arc<dyn ReplySink>,
    session_id: &str,
) {
    let title = sqlx::query_scalar::<_, String>("SELECT title FROM chat_sessions WHERE id = ?")
        .bind(session_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();
    let Some(title) = title else { return };
    if !title.contains(&meta.chat_id) {
        return;
    }
    let (chat_name, resolved_user) = sink
        .resolve_names(&meta.chat_id, &meta.user_id, meta.kind)
        .await;
    let new_title = remote_session_title(meta, chat_name, resolved_user);
    if new_title != title && !new_title.ends_with(&meta.chat_id) {
        let _ = sqlx::query("UPDATE chat_sessions SET title = ?, updated_at = ? WHERE id = ?")
            .bind(&new_title)
            .bind(crate::core::time::now_iso())
            .bind(session_id)
            .execute(db)
            .await;
    }
}

/// Remote session title: groups by chat name, private chats by counterpart
/// name; everything falls back to the raw chat id.
fn remote_session_title(
    meta: &InboundMessage,
    chat_name: Option<String>,
    resolved_user: Option<String>,
) -> String {
    let label = channel_label(&meta.channel);
    let who = match meta.kind {
        ChatKind::Group => chat_name
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| meta.chat_id.clone()),
        ChatKind::Private => resolved_user
            .filter(|n| !n.is_empty())
            .or_else(|| chat_name.filter(|n| !n.is_empty()))
            .or_else(|| {
                let n = meta.user_name.clone();
                (!n.is_empty()).then_some(n)
            })
            .unwrap_or_else(|| meta.chat_id.clone()),
    };
    format!("{label} · {who}")
}

async fn create_remote_session(
    db: &SqlitePool,
    meta: &InboundMessage,
    chat_name: Option<String>,
    resolved_user: Option<String>,
) -> Result<String, String> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = crate::core::time::now_iso();
    let title = remote_session_title(meta, chat_name, resolved_user);
    let context = serde_json::json!({
        "remote": true,
        "channel": &meta.channel,
        "chat_id": meta.chat_id,
        "kind": match meta.kind {
            ChatKind::Group => "group",
            ChatKind::Private => "private",
        },
    })
    .to_string();
    sqlx::query(
        // tools_enabled stores the JSON string "null": the column is NOT
        // NULL, and build_agent_config parses JSON null → None → the full
        // built-in tool set, same convention as UI-created chat sessions
        // (serde_json::to_string of Option<Vec<String>>). Remote access is
        // the user driving their own machine; approval is forced to Auto at
        // turn time (see build_agent_config): the remote side has no
        // approval UI.
        "INSERT INTO chat_sessions (id, title, mode, agent_mode, tools_enabled, system_prompt, context, paper_ids, max_tokens, max_loops, context_budget, created_at, updated_at)
         VALUES (?, ?, 'qa', 'chat', 'null', NULL, ?, '[]', NULL, NULL, NULL, ?, ?)",
    )
    .bind(&id)
    .bind(&title)
    .bind(&context)
    .bind(&now)
    .bind(&now)
    .execute(db)
    .await
    .map_err(|e| format!("db error: {e}"))?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_file(name: &str) -> OutboundFile {
        OutboundFile {
            path: PathBuf::from(format!("/tmp/{name}")),
            name: name.to_string(),
            size: 2048,
            mime: "application/pdf".to_string(),
            sha256: "abc".to_string(),
            caption: None,
        }
    }

    /// Only `pending` files are delivered; a cancelled turn's `failed` entries
    /// must never reach the channel (docs/remote-channels.md §7.4).
    #[test]
    fn only_pending_attachments_are_delivered() {
        let pending = pending_attachments_json(&[sample_file("a.pdf"), sample_file("b.pdf")]);
        let files = pending_files_from_attachments(&pending);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].name, "a.pdf");
        assert_eq!(files[0].size, 2048);
        assert_eq!(files[0].mime, "application/pdf");

        let failed = failed_attachments_json(&[sample_file("a.pdf")], "已取消");
        assert!(pending_files_from_attachments(&failed).is_empty());
    }

    /// A manual retry reads one attachment by index and flips it back to ok,
    /// dropping the stale error so the card stops offering "重试".
    #[test]
    fn attachment_retry_reads_by_index_and_clears_the_error() {
        let items = vec![
            (sample_file("ok.pdf"), None),
            (sample_file("bad.pdf"), Some("CDN 上传失败: HTTP 400".to_string())),
        ];
        let json = settled_attachments_json(&items);

        let f = outbound_file_at(&json, 1).expect("indexed file");
        assert_eq!(f.name, "bad.pdf");
        assert!(outbound_file_at(&json, 2).is_none());

        let patched = patch_attachment_status(&json, 1, "ok", None).unwrap();
        let v: serde_json::Value = serde_json::from_str(&patched).unwrap();
        assert_eq!(v[1]["status"], "ok");
        assert!(v[1].get("error").is_none(), "error must be cleared");
        assert_eq!(v[0]["status"], "ok", "other entries untouched");
        // Only files parse; a legacy inline image yields nothing.
        assert!(outbound_file_at(r#"[{"mime":"image/png","base64":"AA"}]"#, 0).is_none());
    }

    /// The settled ledger keeps per-file outcomes, and legacy user-side
    /// inline images (no `kind`) are ignored by the delivery parser.
    #[test]
    fn settled_attachments_record_per_file_status() {
        let items = vec![
            (sample_file("ok.pdf"), None),
            (sample_file("bad.pdf"), Some("CDN 上传失败: HTTP 400".to_string())),
        ];
        let json = settled_attachments_json(&items);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr[0]["status"], "ok");
        assert_eq!(arr[1]["status"], "failed");
        assert!(arr[1]["error"].as_str().unwrap().contains("CDN"));
        assert!(pending_files_from_attachments(&json).is_empty());

        let legacy = r#"[{"mime":"image/png","base64":"AAAA","name":"x.png"}]"#;
        assert!(pending_files_from_attachments(legacy).is_empty());
    }

    /// Single-connection in-memory pool: multiple connections would each get
    /// their own private ":memory:" database.
    async fn test_db() -> SqlitePool {
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE remote_msg_ids (id TEXT NOT NULL DEFAULT '', ts TEXT NOT NULL DEFAULT '', PRIMARY KEY (id))",
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE remote_chats (
                channel TEXT NOT NULL DEFAULT '',
                chat_id TEXT NOT NULL DEFAULT '',
                session_id TEXT
            )",
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE chat_messages (
                session_id TEXT NOT NULL DEFAULT '',
                role TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL DEFAULT ''
            )",
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE remote_inbound (
                id TEXT NOT NULL DEFAULT '',
                channel TEXT NOT NULL DEFAULT '',
                chat_id TEXT NOT NULL DEFAULT '',
                user_id TEXT NOT NULL DEFAULT '',
                msg_id TEXT NOT NULL DEFAULT '',
                sent_at_ms INTEGER,
                state TEXT NOT NULL DEFAULT 'pending',
                turn_key TEXT NOT NULL DEFAULT '',
                claimed_at TEXT NOT NULL DEFAULT '',
                updated_at TEXT NOT NULL DEFAULT '',
                PRIMARY KEY (id)
            )",
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE remote_turns (
                turn_key TEXT NOT NULL DEFAULT '',
                channel TEXT NOT NULL DEFAULT '',
                chat_id TEXT NOT NULL DEFAULT '',
                msg_ids TEXT NOT NULL DEFAULT '',
                state TEXT NOT NULL DEFAULT 'running',
                claimed_at TEXT NOT NULL DEFAULT '',
                updated_at TEXT NOT NULL DEFAULT '',
                PRIMARY KEY (turn_key)
            )",
        )
        .execute(&db)
        .await
        .unwrap();
        db
    }

    fn sample_inbound(msg_id: &str, sent_at_ms: Option<i64>, text: &str) -> InboundMessage {
        InboundMessage {
            channel: "weixin".to_string(),
            chat_id: "chat-1".to_string(),
            kind: ChatKind::Private,
            user_id: "user-1".to_string(),
            user_name: String::new(),
            msg_id: msg_id.to_string(),
            sent_at_ms,
            text: text.to_string(),
            media: Vec::new(),
        }
    }

    #[test]
    fn media_only_waits_for_a_caption_but_text_does_not() {
        assert_eq!(batch_delay(false, false, false), TEXT_DEBOUNCE_MS);
        assert_eq!(batch_delay(true, false, false), TEXT_DEBOUNCE_MS);
        assert_eq!(batch_delay(true, true, true), DEBOUNCE_MS);
        assert_eq!(batch_delay(false, true, false), MEDIA_DEBOUNCE_MS);
        assert_eq!(batch_delay(false, false, true), FILE_DEBOUNCE_MS);
        assert_eq!(batch_delay(false, true, true), FILE_DEBOUNCE_MS);
    }

    #[test]
    fn batch_splits_on_sender_timestamp_gap() {
        assert!(!batch_gap_exceeded(Some(1_000), Some(1_060)));
        assert!(!batch_gap_exceeded(Some(1_060), Some(1_000)));
        assert!(batch_gap_exceeded(Some(1_000), Some(361_000)));
        assert!(!batch_gap_exceeded(None, Some(1_000)));
    }

    #[test]
    fn startup_backlog_is_dropped_only_during_the_guard_window() {
        let old = Some(STARTUP_MAX_AGE_MS + 1);
        let fresh = Some(1_000);
        assert!(startup_replay(old, Duration::from_secs(1)));
        assert!(!startup_replay(old, STARTUP_REPLAY_WINDOW));
        assert!(!startup_replay(fresh, Duration::from_secs(1)));
        assert!(!startup_replay(None, Duration::from_secs(1)));
    }

    #[test]
    fn fingerprint_includes_media_locator() {
        let mut msg = sample_inbound("m1", Some(1_700_000_000_000), "");
        msg.media.push(MediaRef {
            kind: MediaKind::Image,
            name: "图片".into(),
            locator: MediaLocator::Feishu {
                message_id: "m1".into(),
                file_key: "img-1".into(),
                extra_image_keys: vec![],
            },
        });
        let first = inbound_fingerprint(&msg).expect("fingerprint");
        if let MediaLocator::Feishu { file_key, .. } = &mut msg.media[0].locator {
            *file_key = "img-2".into();
        }
        let second = inbound_fingerprint(&msg).expect("fingerprint");
        assert_ne!(first, second);
    }

    #[tokio::test]
    async fn inbound_ledger_claims_once_and_releases_done() {
        let db = test_db().await;
        let msg = sample_inbound("ledger-1", Some(1), "hi");
        let first = claim_inbound(&db, &msg).await.expect("first claim");
        assert!(!first.is_empty());
        assert!(claim_inbound(&db, &msg).await.is_none());
        finish_inbound(&db, &[first], "done").await;
        assert!(claim_inbound(&db, &msg).await.is_none());
    }

    #[tokio::test]
    async fn turn_key_is_order_independent_and_claims_once() {
        let ids = vec!["a".to_string(), "b".to_string()];
        let reversed = vec!["b".to_string(), "a".to_string()];
        let key = turn_key_for(&ids).expect("key");
        assert_eq!(Some(key.clone()), turn_key_for(&reversed));
        let db = test_db().await;
        let msg = sample_inbound("turn-1", Some(1), "hi");
        assert!(claim_turn(&db, &key, &msg, &ids).await);
        assert!(!claim_turn(&db, &key, &msg, &ids).await);
        finish_turn(&db, &key, "done").await;
        assert!(!claim_turn(&db, &key, &msg, &ids).await);
    }

    #[tokio::test]
    async fn db_seen_marks_once_per_channel_msg_id() {
        let db = test_db().await;
        assert!(!db_seen_check_and_mark(&db, "weixin", "m1").await);
        assert!(db_seen_check_and_mark(&db, "weixin", "m1").await);
        // Same msg_id on another channel instance is a distinct message.
        assert!(!db_seen_check_and_mark(&db, "feishu:app1", "m1").await);
        assert!(db_seen_check_and_mark(&db, "feishu:app1", "m1").await);
    }

    #[tokio::test]
    async fn session_watermark_drops_old_but_not_new_messages() {
        let db = test_db().await;
        sqlx::query("INSERT INTO remote_chats (channel, chat_id, session_id) VALUES ('weixin', 'chat-1', 's1')")
            .execute(&db)
            .await
            .unwrap();
        let original = "2026-11-13T09:18:01.637Z";
        sqlx::query("INSERT INTO chat_messages (session_id, role, created_at) VALUES ('s1', 'user', ?)")
            .bind(original)
            .execute(&db)
            .await
            .unwrap();
        let latest = chrono::DateTime::parse_from_rfc3339(original)
            .unwrap()
            .timestamp_millis();

        for sent_at in [Some(latest), Some(latest - 1000)] {
            let msg = sample_inbound("replay", sent_at, "same old message");
            assert_eq!(
                watermark_verdict(&db, &msg).await,
                WatermarkVerdict::Replay,
                "sent_at={sent_at:?} should be at/before the watermark"
            );
        }
        let fresh = sample_inbound("fresh", Some(latest + 1), "new message");
        assert_eq!(
            watermark_verdict(&db, &fresh).await,
            WatermarkVerdict::Newer
        );
        let no_timestamp = sample_inbound("no-ts", None, "unknown age");
        assert_eq!(
            watermark_verdict(&db, &no_timestamp).await,
            WatermarkVerdict::Unknown
        );
        let other_chat = InboundMessage {
            chat_id: "chat-unmapped".to_string(),
            ..sample_inbound("other", Some(latest), "other chat")
        };
        assert_eq!(
            watermark_verdict(&db, &other_chat).await,
            WatermarkVerdict::Unknown
        );
    }

    #[test]
    fn age_guard_only_applies_when_watermark_is_unknown() {
        let now = 1_700_000_000_000;
        let two_hours_ago = now - 2 * 60 * 60 * 1000;
        // Positively identified as a newer message: process even when old.
        assert!(!age_guard_exceeded(
            WatermarkVerdict::Newer,
            two_hours_ago,
            now
        ));
        // Replay is dropped by the watermark, not the age guard.
        assert!(!age_guard_exceeded(
            WatermarkVerdict::Replay,
            two_hours_ago,
            now
        ));
        // Unknown: age guard is the only thing left.
        assert!(age_guard_exceeded(
            WatermarkVerdict::Unknown,
            now - 31 * 60 * 1000,
            now
        ));
        assert!(!age_guard_exceeded(
            WatermarkVerdict::Unknown,
            now - 29 * 60 * 1000,
            now
        ));
    }

    #[tokio::test]
    async fn fingerprint_dedups_redelivery_without_msg_id() {
        let db = test_db().await;
        let msg = sample_inbound("", Some(1_700_000_000_000), "hello");
        let fingerprint = inbound_fingerprint(&msg).unwrap();
        let key = format!("fp:{fingerprint}");
        assert!(!db_seen_check_and_mark(&db, "weixin", &key).await);
        assert!(db_seen_check_and_mark(&db, "weixin", &key).await);

        // Same text sent at a different time is a new message, not a replay.
        let later = sample_inbound("", Some(1_700_000_001_000), "hello");
        assert_ne!(inbound_fingerprint(&later).unwrap(), fingerprint);
    }

    #[tokio::test]
    async fn prune_remote_msg_ids_keeps_newest() {
        let db = test_db().await;
        for i in 0..(REMOTE_MSG_IDS_CAP + 10) {
            sqlx::query("INSERT INTO remote_msg_ids (id, ts) VALUES (?, ?)")
                .bind(format!("weixin:m{i}"))
                // Zero-padded so lexicographic order matches insertion order.
                .bind(format!("2026-01-01T00:{:05}", i))
                .execute(&db)
                .await
                .unwrap();
        }
        prune_remote_msg_ids(&db).await;
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM remote_msg_ids")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(count, REMOTE_MSG_IDS_CAP);
        // The oldest rows were pruned (re-inserted here), the newest survive.
        assert!(!db_seen_check_and_mark(&db, "weixin", "m0").await);
        assert!(db_seen_check_and_mark(&db, "weixin", &format!("m{}", REMOTE_MSG_IDS_CAP + 9)).await);
    }
}
