//! Inbound routing for remote IM channels (feishu/weixin): dedup, pairing,
//! rate limiting, debounce batching, session mapping, agent turn, reply.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
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

const DEBOUNCE_MS: u64 = 600;
const RATE_LIMIT_PER_MIN: usize = 10;
const SEEN_CAP: usize = 2048;
const SEEN_TTL_SECS: i64 = 24 * 3600;
/// Cap on the synced remote_msg_ids dedup table; pruning runs only above it
/// (deletes are sync traffic, so the table is allowed to just sit below).
const REMOTE_MSG_IDS_CAP: i64 = 5000;
/// Stale-backlog guard: server-redelivered messages older than this are
/// dropped even when every dedup layer missed them — nobody expects an
/// answer to a message sent hours ago.
const STALE_MESSAGE_MS: i64 = 2 * 3600 * 1000;
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

#[derive(Clone)]
pub struct MediaRef {
    /// "image" | "file" (images are additionally materialized into `images`).
    pub kind: String,
    pub name: String,
    /// Channel-specific attachment locators (feishu message_id + file_key).
    pub message_id: String,
    pub file_key: String,
    /// Extra image keys of a feishu post message (file_key holds the first).
    pub extra_image_keys: Vec<String>,
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
    /// Downloaded image attachments ready for the agent turn.
    pub images: Vec<crate::ai::llm::ImageAttachment>,
    pub media: Option<MediaRef>,
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

struct PendingBatch {
    generation: u64,
    texts: Vec<String>,
    images: Vec<crate::ai::llm::ImageAttachment>,
    meta: Option<InboundMessage>,
    sink: Arc<dyn ReplySink>,
}

static PENDING: Lazy<Mutex<HashMap<String, PendingBatch>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Entry point called by channel adapters for every inbound message.
pub async fn enqueue(app: AppHandle, msg: InboundMessage, sink: Arc<dyn ReplySink>) {
    let app_data_dir = app.state::<AppState>().app_data_dir.clone();
    if !msg.msg_id.is_empty() {
        if seen_check_and_mark(&app_data_dir, &msg.msg_id).await {
            return;
        }
        let db = app.state::<AppState>().db.clone();
        if db_seen_check_and_mark(&db, &msg.channel, &msg.msg_id).await {
            return;
        }
    }
    // Stale-backlog guard: when a standby device takes over with a stale
    // long-poll cursor, the server redelivers hours-old messages. Dedup
    // catches what the predecessor recorded; this catches the rest.
    if let Some(sent_ms) = msg.sent_at_ms {
        let age_ms = chrono::Utc::now().timestamp_millis() - sent_ms;
        if age_ms > STALE_MESSAGE_MS {
            tracing::info!(
                channel = %msg.channel, msg_id = %msg.msg_id, age_ms,
                "dropping stale remote message"
            );
            return;
        }
    }

    let key = format!("{}:{}", msg.channel, msg.chat_id);
    let generation = {
        let mut map = PENDING.lock().await;
        let entry = map.entry(key.clone()).or_insert_with(|| PendingBatch {
            generation: 0,
            texts: Vec::new(),
            images: Vec::new(),
            meta: None,
            sink: sink.clone(),
        });
        entry.generation += 1;
        entry.texts.push(msg.text.clone());
        entry.images.extend(msg.images.clone());
        entry.meta = Some(msg);
        entry.sink = sink;
        entry.generation
    };

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(DEBOUNCE_MS)).await;
        let ready = {
            let mut map = PENDING.lock().await;
            map.get_mut(&key).and_then(|entry| {
                if entry.generation == generation {
                    let texts = std::mem::take(&mut entry.texts);
                    let images = std::mem::take(&mut entry.images);
                    let meta = entry.meta.clone();
                    meta.map(|m| (m, texts, images, entry.sink.clone()))
                } else {
                    None
                }
            })
        };
        if let Some((meta, texts, images, sink)) = ready {
            process_batch(app, meta, texts, images, sink).await;
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

async fn process_batch(
    app: AppHandle,
    meta: InboundMessage,
    texts: Vec<String>,
    images: Vec<crate::ai::llm::ImageAttachment>,
    sink: Arc<dyn ReplySink>,
) {
    let mut text = texts.join("\n");
    if text.trim().is_empty() && !images.is_empty() {
        text = "[图片]".to_string();
    }
    if text.trim().is_empty() && images.is_empty() && meta.media.is_none() {
        return;
    }
    let state = app.state::<AppState>();
    let db = state.db.clone();

    if rate_limited(&format!("{}:{}", meta.channel, meta.user_id)).await {
        let _ = sink
            .send_text(&meta.chat_id, "消息太频繁了，请稍后再试。")
            .await;
        return;
    }

    // Pairing stays outside the queue: a stranger gets the pairing notice
    // immediately instead of waiting behind a running turn.
    let (session_id, grant) = match ensure_paired(&db, &app, &meta, &sink).await {
        Ok(Some(pair)) => pair,
        Ok(None) => return, // pairing notice already handled
        Err(e) => {
            tracing::error!(error = %e, "remote pairing check failed");
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

    // File messages: save the attachment, extract its text with the same
    // document pipeline chat attachments use, and hand both path and content
    // to a normal agent turn. No auto-import — the agent decides (e.g.
    // paper_import when the user asks to add a PDF to the library).
    if let Some(media) = &meta.media {
        if media.kind == "file" {
            match sink.resolve_file_media(media).await {
                Ok(path) => {
                    // Keep every inbound file under remote_downloads so the
                    // agent can come back to it in later turns (feishu
                    // resolves into a volatile temp dir).
                    let downloads = state.app_data_dir.join("remote_downloads");
                    let path = if path.starts_with(&downloads) {
                        path
                    } else {
                        let target =
                            downloads.join(format!("{}_{}", uuid::Uuid::new_v4(), media.name));
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
                    let user_note = text.trim().to_string();
                    let body = match extracted {
                        Ok(content) => format!("文件内容提取如下：\n{content}"),
                        Err(e) => format!(
                            "文件内容提取失败（{e}）。可建议用户在思库应用内处理；\
                             若用户要求入库且是文献 PDF，可用 paper_import 导入该路径。"
                        ),
                    };
                    text = if user_note.is_empty() {
                        format!("（用户发来文件《{}》，已保存到本机：{}。{body}）", media.name, path.display())
                    } else {
                        format!("{user_note}\n\n（附件《{}》已保存到本机：{}。{body}）", media.name, path.display())
                    };
                }
                Err(_) => {
                    let _ = sink
                        .send_text(&meta.chat_id, &file_fallback_text(&meta, &media.name))
                        .await;
                    return;
                }
            }
        }
    }

    let channel_label = channel_label(&meta.channel);
    let preview: String = text.chars().take(50).collect();
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
        .progress_start(&meta.chat_id, &preview, &session_id, &text, meta.kind)
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
    let result = crate::commands::agent::run_agent_turn(
        &state,
        &app,
        session_id.clone(),
        text.clone(),
        if images.is_empty() { None } else { Some(images) },
        tools_override,
        Some(outbound.clone()),
    )
    .await;

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
        db
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
