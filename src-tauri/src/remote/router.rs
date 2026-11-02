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
pub const REMOTE_PROMPT_ADDENDUM: &str = "你正通过即时通讯软件（微信/飞书）远程服务用户。\
回复必须是纯文本：不要输出 siku-reader:// 等应用内链接，不要输出本地文件路径，\
不要输出图片标记。引用文献或笔记时给出标题即可。回复控制在手机可阅读的长度，\
内容较多时先给结论，并提示用户可以追问细节。";

/// Read-only tool set for remote sessions. No bash/file writes: there is no
/// approval UI on the remote side.
const REMOTE_TOOLS_JSON: &str = r#"["paper_search","paper_read","search_library","note_read","knowledge_query","memory_read","memory_recall","web_search","web_fetch","task_list","task_output"]"#;

const DEBOUNCE_MS: u64 = 600;
const RATE_LIMIT_PER_MIN: usize = 10;
const SEEN_CAP: usize = 2048;
const SEEN_TTL_SECS: i64 = 24 * 3600;
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
pub struct InboundMessage {
    pub channel: &'static str,
    pub chat_id: String,
    pub kind: ChatKind,
    pub user_id: String,
    pub user_name: String,
    pub msg_id: String,
    pub text: String,
}

#[async_trait::async_trait]
pub trait ReplySink: Send + Sync {
    async fn send_text(&self, chat_id: &str, text: &str) -> Result<(), String>;
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
    meta: Option<InboundMessage>,
    sink: Arc<dyn ReplySink>,
}

static PENDING: Lazy<Mutex<HashMap<String, PendingBatch>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Entry point called by channel adapters for every inbound message.
pub async fn enqueue(app: AppHandle, msg: InboundMessage, sink: Arc<dyn ReplySink>) {
    let app_data_dir = app.state::<AppState>().app_data_dir.clone();
    if !msg.msg_id.is_empty() && seen_check_and_mark(&app_data_dir, &msg.msg_id).await {
        return;
    }

    let key = format!("{}:{}", msg.channel, msg.chat_id);
    let generation = {
        let mut map = PENDING.lock().await;
        let entry = map.entry(key.clone()).or_insert_with(|| PendingBatch {
            generation: 0,
            texts: Vec::new(),
            meta: None,
            sink: sink.clone(),
        });
        entry.generation += 1;
        entry.texts.push(msg.text.clone());
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
                    let meta = entry.meta.clone();
                    meta.map(|m| (m, texts, entry.sink.clone()))
                } else {
                    None
                }
            })
        };
        if let Some((meta, texts, sink)) = ready {
            process_batch(app, meta, texts, sink).await;
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
    sink: Arc<dyn ReplySink>,
) {
    let text = texts.join("\n");
    if text.trim().is_empty() {
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
    let session_id = match ensure_paired(&db, &app, &meta, &sink).await {
        Ok(Some(sid)) => sid,
        Ok(None) => return, // pairing notice already handled
        Err(e) => {
            tracing::error!(error = %e, "remote pairing check failed");
            return;
        }
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

    let channel_label = channel_label(meta.channel);
    crate::commands::pet::pet_report(
        &app,
        "remote",
        &format!("{channel_label}远程会话"),
        "running",
        Some(text.chars().take(50).collect::<String>()),
    );

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

    let result = crate::commands::agent::run_agent_turn(
        &state,
        &app,
        session_id.clone(),
        text.clone(),
        None,
    )
    .await;

    let reply = match result {
        Ok(()) => match wait_for_new_reply(&db, &session_id, baseline).await {
            Some(r) if !r.trim().is_empty() => r,
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

    let report_state = if reply.starts_with("处理失败") { "failed" } else { "done" };
    crate::commands::pet::pet_report(&app, "remote", &format!("{channel_label}远程会话"), report_state, None);

    if let Err(e) = sink.send_text(&meta.chat_id, &reply).await {
        tracing::error!(error = %e, "remote reply send failed");
    }
}

/// The agent persists its assistant message only when the turn ends, so a
/// new row past `baseline` is exactly "this turn finished" — including
/// failures, which are persisted with an ❌ marker. Polls until the timeout.
async fn wait_for_new_reply(db: &SqlitePool, session_id: &str, baseline: i64) -> Option<String> {
    let deadline = Instant::now() + Duration::from_secs(REPLY_TIMEOUT_SECS);
    loop {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let row = sqlx::query_scalar::<_, String>(
            "SELECT content FROM chat_messages WHERE session_id = ? AND role = 'assistant' AND rowid > ?
             ORDER BY rowid DESC LIMIT 1",
        )
        .bind(session_id)
        .bind(baseline)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();
        if row.is_some() {
            return row;
        }
        if Instant::now() >= deadline {
            tracing::warn!(session_id, "remote reply wait timed out");
            return None;
        }
    }
}

fn channel_label(channel: &str) -> String {
    match channel {
        "feishu" => "飞书".to_string(),
        "weixin" => "微信".to_string(),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------- pairing

/// Returns Ok(Some(session_id)) when the SENDER is paired; Ok(None) when not
/// approved yet (notice handled here). Pairing is per (channel, chat_id,
/// user_id): in a group every member pairs individually, while the backing
/// chat session is shared across the group's approved members.
async fn ensure_paired(
    db: &SqlitePool,
    app: &AppHandle,
    meta: &InboundMessage,
    sink: &Arc<dyn ReplySink>,
) -> Result<Option<String>, String> {
    let row = sqlx::query_as::<_, (String, i64, Option<String>)>(
        "SELECT user_name, approved, session_id FROM remote_chats
         WHERE channel = ? AND chat_id = ? AND user_id = ?",
    )
    .bind(meta.channel)
    .bind(&meta.chat_id)
    .bind(&meta.user_id)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("db error: {e}"))?;

    let now = crate::core::time::now_iso();
    let Some((user_name, approved, session_id)) = row else {
        // Resolve display names once at pairing time (feishu directory APIs);
        // failures fall back to raw ids without blocking pairing.
        let (_chat_name, resolved_user) = sink
            .resolve_names(&meta.chat_id, &meta.user_id, meta.kind)
            .await;
        let display_user = resolved_user
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| meta.user_name.clone());
        sqlx::query(
            "INSERT INTO remote_chats (channel, chat_id, user_id, user_name, session_id, approved, created_at, updated_at)
             VALUES (?, ?, ?, ?, NULL, 0, ?, ?)",
        )
        .bind(meta.channel)
        .bind(&meta.chat_id)
        .bind(&meta.user_id)
        .bind(&display_user)
        .bind(&now)
        .bind(&now)
        .execute(db)
        .await
        .map_err(|e| format!("db error: {e}"))?;
        let _ = app.emit("remote:pairing", ());
        let who = if display_user.is_empty() { "你" } else { &display_user };
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
        .bind(meta.channel)
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
            .bind(meta.channel)
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
        .bind(meta.channel)
        .bind(&meta.chat_id)
        .bind(&meta.user_id)
        .execute(db)
        .await;
    }

    if let Some(sid) = session_id.filter(|s| !s.is_empty()) {
        maybe_refresh_session_title(db, meta, sink, &sid).await;
        return Ok(Some(sid));
    }

    // The chat session is shared: reuse any approved member's session for
    // this (channel, chat_id) before creating a new one.
    let existing = sqlx::query_scalar::<_, String>(
        "SELECT session_id FROM remote_chats
         WHERE channel = ? AND chat_id = ? AND session_id IS NOT NULL AND session_id != '' LIMIT 1",
    )
    .bind(meta.channel)
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
        .bind(meta.channel)
        .bind(&meta.chat_id)
        .execute(db)
        .await
        .map_err(|e| format!("db error: {e}"))?;
    Ok(Some(sid))
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
    let label = channel_label(meta.channel);
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
        "channel": meta.channel,
        "chat_id": meta.chat_id,
    })
    .to_string();
    sqlx::query(
        // Same NULL convention as create_chat_session: legacy column DEFAULTs
        // must not leak in — NULL follows the global defaults.
        "INSERT INTO chat_sessions (id, title, mode, agent_mode, tools_enabled, system_prompt, context, paper_ids, max_tokens, max_loops, context_budget, created_at, updated_at)
         VALUES (?, ?, 'qa', 'chat', ?, NULL, ?, '[]', NULL, NULL, NULL, ?, ?)",
    )
    .bind(&id)
    .bind(&title)
    .bind(REMOTE_TOOLS_JSON)
    .bind(&context)
    .bind(&now)
    .bind(&now)
    .execute(db)
    .await
    .map_err(|e| format!("db error: {e}"))?;
    Ok(id)
}
