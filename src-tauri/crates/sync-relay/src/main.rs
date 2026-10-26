use axum::{
    extract::{Query, State, WebSocketUpgrade},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Router,
};
use axum::extract::ws::{Message, WebSocket};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{info, warn};

mod auth;
mod db;
mod mailbox;
mod pay;

// ── Configuration ──────────────────────────────────────────────────────────

fn jwt_secret() -> String {
    std::env::var("JWT_SECRET").unwrap_or_else(|_| "siku-dev-secret-change-me".to_string())
}

fn listen_addr() -> String {
    let host = std::env::var("RELAY_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = std::env::var("RELAY_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080u16);
    format!("{}:{}", host, port)
}

pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn heartbeat_interval() -> Duration {
    let secs = std::env::var("HEARTBEAT_INTERVAL_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30u64);
    Duration::from_secs(secs)
}

fn heartbeat_timeout() -> Duration {
    let secs = std::env::var("HEARTBEAT_TIMEOUT_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60u64);
    Duration::from_secs(secs)
}

/// Admin API token. When unset, all /api/admin/* endpoints return 404.
fn admin_token() -> Option<String> {
    std::env::var("RELAY_ADMIN_TOKEN")
        .ok()
        .filter(|t| !t.is_empty())
}

/// Default per-account mailbox quota (1 GiB) for users without an active
/// custom quota.
fn default_quota_bytes() -> i64 {
    std::env::var("RELAY_DEFAULT_QUOTA_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1 << 30)
}

/// Out-of-band payment instructions shown to the user when they create an
/// upgrade order (e.g. "Alipay 158xxxx, note your order id").
fn payment_info() -> String {
    std::env::var("RELAY_PAYMENT_INFO").unwrap_or_default()
}

// ── Storage plans ──────────────────────────────────────────────────────────
//
// Pricing is hardcoded here and served via GET /api/plans. Yearly = 10 ×
// monthly; a paid period lasts 30 days (month) or 365 days (year).

#[derive(Clone, Serialize)]
struct Plan {
    id: &'static str,
    name: &'static str,
    quota_bytes: i64,
    monthly_cny: f64,
    yearly_cny: f64,
}

const PLANS: &[Plan] = &[
    Plan { id: "free", name: "Free", quota_bytes: 1 << 30, monthly_cny: 0.0, yearly_cny: 0.0 },
    Plan { id: "plus", name: "Plus", quota_bytes: 10 << 30, monthly_cny: 6.0, yearly_cny: 60.0 },
    Plan { id: "pro", name: "Pro", quota_bytes: 50 << 30, monthly_cny: 15.0, yearly_cny: 150.0 },
    Plan { id: "max", name: "Max", quota_bytes: 200 << 30, monthly_cny: 30.0, yearly_cny: 300.0 },
];

fn find_plan(id: &str) -> Option<&'static Plan> {
    PLANS.iter().find(|p| p.id == id)
}

/// Best-effort label for the plan a quota corresponds to; "custom" when it
/// matches no plan (admin-adjusted quota).
fn plan_id_for_quota(quota_bytes: i64) -> &'static str {
    PLANS
        .iter()
        .find(|p| p.quota_bytes == quota_bytes)
        .map(|p| p.id)
        .unwrap_or("custom")
}

// ── Shared protocol types (mirror of the Siku client) ──────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RelayClientMsg {
    Join { payload: JoinPayload },
    Signal { payload: SignalPayload },
    Relay { payload: RelayPayload },
    MailboxDeposit { payload: MailboxDepositPayload },
    MailboxPoll { payload: MailboxPollPayload },
    MailboxAck { payload: MailboxAckPayload },
    /// Reply to the server's periodic `Ping`; keeps the connection alive so an
    /// idle device is not dropped by the receive timeout.
    Pong,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MailboxDepositPayload {
    to_device_id: String,
    ciphertext: String,
    nonce: String,
    #[serde(default)]
    ttl_seconds: Option<u64>,
    /// Client correlation id: echoed back in `MailboxDepositAck` so the sender
    /// only advances its sync cursor after the message is durably stored.
    /// Legacy clients omit it; the relay then generates its own id and sends
    /// an ack the client ignores.
    #[serde(default)]
    message_id: Option<String>,
    /// Opaque content key for blob payloads (`HMAC(sync_key, blob_hash)` — the
    /// relay cannot correlate it with any known file). A second deposit with
    /// the same key in this room is acknowledged without double-storing, so
    /// two devices pushing the same file don't double-spend the quota.
    /// Legacy clients omit it and are never deduped.
    #[serde(default)]
    dedup_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MailboxPollPayload {
    #[serde(default)]
    max_count: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MailboxAckPayload {
    message_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MailboxMessage {
    id: String,
    from_device_id: String,
    ciphertext: String,
    nonce: String,
    /// True for account-level archive messages (shared by every device of the
    /// account); false for per-device messages.
    #[serde(default)]
    account_level: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MailboxBatchPayload {
    messages: Vec<MailboxMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RelayPayload {
    to_device_id: String,
    ciphertext: String,
    ttl_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JoinPayload {
    room_id: String,
    /// PoC pairing: allow the guest to override the token's device identity.
    #[serde(default)]
    device_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SignalPayload {
    to_device_id: String,
    data: SignalData,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
enum SignalData {
    #[serde(alias = "offer")]
    Offer { sdp: String },
    #[serde(alias = "answer")]
    Answer { sdp: String },
    #[serde(alias = "ice")]
    Ice {
        candidate: String,
        sdp_mid: Option<String>,
        sdp_mline_index: Option<u16>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RelayServerMsg {
    PeerOnline { payload: PeerPayload },
    PeerOffline { payload: PeerPayload },
    Presence { payload: PresencePayload },
    Signal { payload: RelaySignalPayload },
    Relay { payload: serde_json::Value },
    Ping,
    Error { payload: ErrorPayload },
    MailboxBatch { payload: MailboxBatchPayload },
    /// Acknowledges a `MailboxDeposit` after the message is durably stored
    /// (or rejected). `ok=false` means the deposit was NOT stored — the
    /// sender must retry / queue it rather than advance its cursor.
    MailboxDepositAck { payload: MailboxDepositAckPayload },
    /// Capability handshake sent right after a successful Join. Lets the
    /// client detect a legacy relay that never acks mailbox deposits instead
    /// of timing out on every deposit. Old clients ignore unknown message
    /// types, so this is safe to send to them.
    ServerHello { payload: ServerHelloPayload },
}

/// Relay protocol versions: 1 = legacy (no mailbox deposit acks), 2 = durable
/// mailbox with `MailboxDepositAck`.
const RELAY_PROTOCOL_VERSION: u32 = 2;

/// Byte budget for one `MailboxBatch` frame. The websocket has no explicit
/// max_message_size (tungstenite's 64MB default applies); a single frame
/// carrying the whole poll result once exceeded that on accounts with large
/// snapshot backlogs, killing delivery. Payload size is approximated by
/// ciphertext + nonce lengths, which dominate the serialized frame.
const MAILBOX_BATCH_MAX_BYTES: usize = 8 * 1024 * 1024;

/// Group mailbox messages into batches whose cumulative payload stays within
/// `max_bytes`, preserving order. A single message larger than the budget
/// gets its own frame — it must still be deliverable (a ~10MB snapshot is
/// ~16MB base64, well under the 64MB websocket cap).
fn chunk_mailbox_messages(messages: Vec<MailboxMessage>, max_bytes: usize) -> Vec<Vec<MailboxMessage>> {
    let mut batches: Vec<Vec<MailboxMessage>> = Vec::new();
    let mut current: Vec<MailboxMessage> = Vec::new();
    let mut current_bytes = 0usize;
    for m in messages {
        let size = m.ciphertext.len() + m.nonce.len();
        if !current.is_empty() && current_bytes + size > max_bytes {
            batches.push(std::mem::take(&mut current));
            current_bytes = 0;
        }
        current_bytes += size;
        current.push(m);
    }
    if !current.is_empty() {
        batches.push(current);
    }
    batches
}

/// Send a poll result as one or more byte-budgeted `MailboxBatch` frames.
/// An empty result is still answered with an empty batch: without a frame the
/// client cannot distinguish "no new messages" from "no answer yet", and the
/// empty batch is the relay's explicit "you are up to date" signal. Old
/// clients treat an empty batch as a no-op, so this is additive.
fn send_mailbox_batches(tx: &mpsc::UnboundedSender<RelayServerMsg>, messages: Vec<MailboxMessage>) {
    let batches = chunk_mailbox_messages(messages, MAILBOX_BATCH_MAX_BYTES);
    if batches.is_empty() {
        let _ = tx.send(RelayServerMsg::MailboxBatch {
            payload: MailboxBatchPayload { messages: Vec::new() },
        });
        return;
    }
    for batch in batches {
        let _ = tx.send(RelayServerMsg::MailboxBatch {
            payload: MailboxBatchPayload { messages: batch },
        });
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ServerHelloPayload {
    protocol: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MailboxDepositAckPayload {
    /// The stored message id (the client's `message_id` when provided).
    id: String,
    ok: bool,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PeerPayload {
    device_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PresencePayload {
    device_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RelaySignalPayload {
    from_device_id: String,
    to_device_id: String,
    data: SignalData,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ErrorPayload {
    code: String,
    message: String,
}

// ── JWT authentication ─────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    sub: String,       // user_id / authorized room_id
    device_id: String,
    exp: usize,
}

fn decode_token(token: &str) -> anyhow::Result<Claims> {
    use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
    let mut validation = Validation::new(Algorithm::HS256);
    validation.validate_aud = false;
    let secret = jwt_secret();
    let token_data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )?;
    Ok(token_data.claims)
}

// ── Room state ─────────────────────────────────────────────────────────────

type DeviceId = String;
type RoomId = String;

struct DeviceConn {
    tx: mpsc::UnboundedSender<RelayServerMsg>,
}

struct Room {
    /// device_id -> conn_seq -> connection. A device holds several live
    /// WebSocket connections at once (auto-sync discovery, per-session
    /// signaling, mailbox transports all join the same room under the same
    /// device id), so one connection closing must not mark the whole device
    /// offline while its other connections are still up.
    devices: HashMap<DeviceId, HashMap<u64, DeviceConn>>,
}

struct AppState {
    rooms: Mutex<HashMap<RoomId, Room>>,
    db: db::Db,
    auth: auth::Auth,
    mailboxes: mailbox::Mailbox,
    admin_token: Option<String>,
    default_quota: i64,
    payment_info: String,
    /// 支付宝配置（None = 未配置，在线支付整体降级为线下转账流程）。
    alipay: Option<pay::AlipayConfig>,
    /// 公网可达的 relay 地址（RELAY_PUBLIC_BASE），用于拼 notify/return URL。
    alipay_public_base: Option<String>,
}

impl AppState {
    fn new() -> Self {
        let db_path = std::env::var("RELAY_DB_PATH").unwrap_or_else(|_| ":memory:".to_string());
        // Mailbox persistence: separate SQLite file next to the account db.
        // ":memory:" account db → ":memory:" mailbox (ephemeral, tests/dev).
        let mailbox_db_path = std::env::var("RELAY_MAILBOX_DB_PATH").unwrap_or_else(|_| {
            if db_path == ":memory:" {
                ":memory:".to_string()
            } else {
                format!("{db_path}.mailbox.sqlite")
            }
        });
        let alipay = pay::AlipayConfig::from_env();
        let alipay_public_base = std::env::var("RELAY_PUBLIC_BASE")
            .ok()
            .filter(|v| !v.is_empty());
        if alipay.is_some() && alipay_public_base.is_none() {
            warn!("RELAY_PUBLIC_BASE 未配置，支付宝在线支付不可用（下单不返回 pay_url）");
        }
        Self {
            rooms: Mutex::new(HashMap::new()),
            db: db::Db::new(std::path::Path::new(&db_path)).expect("account db"),
            auth: auth::Auth::new(jwt_secret()),
            mailboxes: mailbox::Mailbox::open(std::path::Path::new(&mailbox_db_path))
                .expect("mailbox db"),
            admin_token: admin_token(),
            default_quota: default_quota_bytes(),
            payment_info: payment_info(),
            alipay,
            alipay_public_base,
        }
    }

    /// 该订单的在线支付页 URL：仅当订单仍 pending、选了支付宝渠道且在线支付
    /// 配置完整时可用。
    fn order_pay_url(&self, order: &db::Order) -> Option<String> {
        if order.status != db::OrderStatus::Pending || order.channel.as_deref() != Some("alipay") {
            return None;
        }
        let base = self.alipay_public_base.as_ref()?;
        self.alipay.as_ref()?;
        Some(format!("{}/pay/{}", base.trim_end_matches('/'), order.id))
    }

    /// The quota currently in force for a user: their custom quota while it
    /// has not expired (None expiry = permanent, admin-granted), otherwise
    /// the relay default. Computed on use — no expiry sweeper needed, and a
    /// lapsed subscription only rejects NEW writes (existing rows age out
    /// via TTL or ack).
    fn effective_quota(&self, user_id: &str) -> (i64, Option<String>) {
        if let Some((Some(quota), expires_at)) = self.db.get_user_quota(user_id) {
            let active = match &expires_at {
                None => true,
                Some(ts) => chrono::DateTime::parse_from_rfc3339(ts)
                    .map(|t| t > chrono::Utc::now())
                    .unwrap_or(false),
            };
            if active {
                return (quota, expires_at);
            }
        }
        (self.default_quota, None)
    }

    fn is_online(&self, device_id: &str) -> bool {
        let rooms = self.rooms.lock().unwrap();
        rooms.values().any(|room| room.devices.contains_key(device_id))
    }

    fn join(
        &self,
        room_id: &str,
        device_id: &str,
        conn_seq: u64,
        tx: mpsc::UnboundedSender<RelayServerMsg>,
    ) {
        let mut rooms = self.rooms.lock().unwrap();
        let room = rooms.entry(room_id.to_string()).or_insert_with(|| Room {
            devices: HashMap::new(),
        });
        room.devices
            .entry(device_id.to_string())
            .or_default()
            .insert(conn_seq, DeviceConn { tx });
    }

    fn leave(&self, room_id: &str, device_id: &str, conn_seq: u64) {
        let mut rooms = self.rooms.lock().unwrap();
        if let Some(room) = rooms.get_mut(room_id) {
            let removed = room
                .devices
                .get_mut(device_id)
                .map(|conns| conns.remove(&conn_seq).is_some())
                .unwrap_or(false);
            if removed {
                // Only forget the device when its LAST connection is gone;
                // other live transports must keep it marked online.
                let empty_device = room
                    .devices
                    .get(device_id)
                    .map(|conns| conns.is_empty())
                    .unwrap_or(false);
                if empty_device {
                    room.devices.remove(device_id);
                    if room.devices.is_empty() {
                        rooms.remove(room_id);
                    }
                }
            }
        }
    }

    /// Monotonic counter shared by all connections; a reconnect for the same
    /// device gets a fresh sequence so its cleanup cannot hurt newer conns.
    fn next_conn_seq() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(1);
        SEQ.fetch_add(1, Ordering::Relaxed)
    }

    fn device_ids_in_room(&self, room_id: &str) -> Vec<String> {
        let rooms = self.rooms.lock().unwrap();
        rooms
            .get(room_id)
            .map(|room| room.devices.keys().cloned().collect())
            .unwrap_or_default()
    }

    fn send_to(&self, room_id: &str, device_id: &str, msg: RelayServerMsg) -> bool {
        let rooms = self.rooms.lock().unwrap();
        if let Some(room) = rooms.get(room_id) {
            if let Some(conns) = room.devices.get(device_id) {
                let mut delivered = false;
                for conn in conns.values() {
                    if conn.tx.send(msg.clone()).is_ok() {
                        delivered = true;
                    }
                }
                return delivered;
            }
        }
        false
    }

    fn broadcast(&self, room_id: &str, except: Option<&str>, msg: RelayServerMsg) {
        let rooms = self.rooms.lock().unwrap();
        if let Some(room) = rooms.get(room_id) {
            for (id, conns) in &room.devices {
                if except == Some(id.as_str()) {
                    continue;
                }
                for conn in conns.values() {
                    let _ = conn.tx.send(msg.clone());
                }
            }
        }
    }
}

// ── HTTP / WebSocket handlers ──────────────────────────────────────────────

/// Extract the device JWT from the request. Newer clients send it in the
/// `Authorization: Bearer <token>` header (never logged by proxies); the
/// legacy `?token=` query string is still accepted for older clients.
fn extract_token(headers: &axum::http::HeaderMap, query: &HashMap<String, String>) -> Option<String> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.trim().to_string())
        .or_else(|| query.get("token").cloned())
}

async fn signaling_handler(
    ws: WebSocketUpgrade,
    Query(query): Query<HashMap<String, String>>,
    headers: axum::http::HeaderMap,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let Some(token) = extract_token(&headers, &query) else {
        warn!("websocket connection without token");
        return (StatusCode::UNAUTHORIZED, "missing token").into_response();
    };
    let claims = match decode_token(&token) {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "invalid token");
            return (StatusCode::UNAUTHORIZED, "invalid token").into_response();
        }
    };
    // Only registered, non-revoked devices may connect, even with a
    // still-valid JWT. `revoked_at` only exists on legacy rows (device
    // removal replaced revocation); unknown devices are rejected outright —
    // otherwise a removed device could keep syncing until its JWT expired.
    match state.db.get_device(&claims.device_id) {
        Some(d) if d.revoked_at.is_none() => {
            state.db.touch_device(&claims.device_id);
        }
        Some(_) => {
            warn!(device_id = %claims.device_id, "connection rejected: device revoked");
            return (StatusCode::UNAUTHORIZED, "device revoked").into_response();
        }
        None => {
            warn!(device_id = %claims.device_id, "connection rejected: unknown device");
            return (StatusCode::UNAUTHORIZED, "unknown device").into_response();
        }
    }
    info!(device_id = %claims.device_id, room = %claims.sub, "websocket connected");
    ws.on_upgrade(move |socket| handle_socket(socket, state, claims))
}

async fn handle_socket(socket: WebSocket, state: Arc<AppState>, claims: Claims) {
    let (mut sender, mut receiver) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<RelayServerMsg>();

    let room_id = claims.sub;
    let device_id = claims.device_id;
    let mut joined_room: Option<String> = None;
    let conn_seq = AppState::next_conn_seq();

    // Forward server -> client messages over the WebSocket.
    let forward_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let json = match serde_json::to_string(&msg) {
                Ok(j) => j,
                Err(e) => {
                    warn!(error = %e, "failed to serialize server msg");
                    continue;
                }
            };
            if sender.send(Message::Text(json)).await.is_err() {
                break;
            }
        }
    });

    // Heartbeat: send Ping periodically.
    let tx_heartbeat = tx.clone();
    let heartbeat_task = tokio::spawn(async move {
        let interval = heartbeat_interval();
        loop {
            tokio::time::sleep(interval).await;
            if tx_heartbeat.send(RelayServerMsg::Ping).is_err() {
                break;
            }
        }
    });

    // Read client messages.
    let result = loop {
        let timeout = tokio::time::timeout(heartbeat_timeout(), receiver.next());
        match timeout.await {
            Ok(Some(Ok(Message::Text(text)))) => {
                let client_msg = match serde_json::from_str::<RelayClientMsg>(&text) {
                    Ok(m) => m,
                    Err(e) => {
                        warn!(error = %e, text = %text, "failed to parse client msg");
                        let _ = tx.send(RelayServerMsg::Error {
                            payload: ErrorPayload {
                                code: "bad_request".to_string(),
                                message: format!("parse error: {}", e),
                            },
                        });
                        continue;
                    }
                };

                match client_msg {
                    RelayClientMsg::Join { payload } => {
                        // In PoC the token's subject authorizes exactly one room.
                        if payload.room_id != room_id {
                            let _ = tx.send(RelayServerMsg::Error {
                                payload: ErrorPayload {
                                    code: "forbidden".to_string(),
                                    message: "room_id does not match token".to_string(),
                                },
                            });
                            continue;
                        }

                        // PoC pairing: the guest may join under the identity
                        // assigned in the pairing payload. The account phase
                        // removes this override (each device has its own token).
                        let join_device_id =
                            payload.device_id.clone().unwrap_or_else(|| device_id.clone());

                        // Leave previous room if any.
                        if let Some(prev) = joined_room.take() {
                            state.mailboxes.remove_device(&prev, &device_id);
                            state.leave(&prev, &device_id, conn_seq);
                            state.broadcast(
                                &prev,
                                Some(&device_id),
                                RelayServerMsg::PeerOffline {
                                    payload: PeerPayload {
                                        device_id: device_id.clone(),
                                    },
                                },
                            );
                            let ids = state.device_ids_in_room(&prev);
                            state.broadcast(
                                &prev,
                                Some(&device_id),
                                RelayServerMsg::Presence {
                                    payload: PresencePayload { device_ids: ids },
                                },
                            );
                        }

                        // Deliver pending encrypted mailbox messages — but only
                        // on the device's FIRST live connection, so a second
                        // transport (e.g. a mailbox listener) does not receive
                        // the same batch again.
                        let is_first_connection = {
                            let rooms = state.rooms.lock().unwrap();
                            rooms
                                .get(&room_id)
                                .map(|r| !r.devices.contains_key(&join_device_id))
                                .unwrap_or(true)
                        };

                        state.join(&room_id, &join_device_id, conn_seq, tx.clone());
                        state.mailboxes.ensure_device(&room_id, &join_device_id);
                        joined_room = Some(room_id.clone());

                        // Capability handshake, sent before anything else so
                        // the client can tell a modern relay (mailbox deposit
                        // acks) from a legacy one. Clients that predate this
                        // message ignore the unknown type.
                        let _ = tx.send(RelayServerMsg::ServerHello {
                            payload: ServerHelloPayload {
                                protocol: RELAY_PROTOCOL_VERSION,
                            },
                        });

                        if is_first_connection {
                            let pending = state
                                .mailboxes
                                .poll(&room_id, &join_device_id, Some(100));
                            let had_pending = !pending.is_empty();
                            // Always answer, even with an empty batch: the
                            // empty batch is the client's "in sync with the
                            // cloud archive" signal.
                            send_mailbox_batches(&tx, pending);
                            if had_pending {
                                info!(device_id = %join_device_id, "delivered pending mailbox batch");
                            }
                        }

                        // Tell the new device who is already here.
                        let present = state.device_ids_in_room(&room_id);
                        let others: Vec<String> = present
                            .iter()
                            .filter(|id| *id != &join_device_id)
                            .cloned()
                            .collect();
                        let _ = tx.send(RelayServerMsg::Presence {
                            payload: PresencePayload {
                                device_ids: others.clone(),
                            },
                        });
                        for other_id in &others {
                            let _ = tx.send(RelayServerMsg::PeerOnline {
                                payload: PeerPayload {
                                    device_id: other_id.clone(),
                                },
                            });
                        }

                        // Notify others.
                        state.broadcast(
                            &room_id,
                            Some(&join_device_id),
                            RelayServerMsg::PeerOnline {
                                payload: PeerPayload {
                                    device_id: join_device_id.clone(),
                                },
                            },
                        );

                        info!(device_id = %join_device_id, room = %room_id, "joined room");
                    }
                    RelayClientMsg::Signal { payload } => {
                        let Some(room) = joined_room.as_ref() else {
                            let _ = tx.send(RelayServerMsg::Error {
                                payload: ErrorPayload {
                                    code: "not_joined".to_string(),
                                    message: "send join before signaling".to_string(),
                                },
                            });
                            continue;
                        };
                        let delivered = state.send_to(
                            room,
                            &payload.to_device_id,
                            RelayServerMsg::Signal {
                                payload: RelaySignalPayload {
                                    from_device_id: device_id.clone(),
                                    to_device_id: payload.to_device_id.clone(),
                                    data: payload.data,
                                },
                            },
                        );
                        if !delivered {
                            info!(
                                from = %device_id,
                                to = %payload.to_device_id,
                                "signal dropped: target offline"
                            );
                        }
                    }
                    RelayClientMsg::Relay { payload } => {
                        let Some(room) = joined_room.as_ref() else {
                            let _ = tx.send(RelayServerMsg::Error {
                                payload: ErrorPayload {
                                    code: "not_joined".to_string(),
                                    message: "send join before relay".to_string(),
                                },
                            });
                            continue;
                        };
                        let relay_payload = serde_json::json!({
                            "from_device_id": device_id.clone(),
                            "to_device_id": payload.to_device_id.clone(),
                            "ciphertext": payload.ciphertext,
                            "ttl_seconds": payload.ttl_seconds,
                        });
                        let delivered = state.send_to(
                            room,
                            &payload.to_device_id,
                            RelayServerMsg::Relay {
                                payload: relay_payload,
                            },
                        );
                        if !delivered {
                            info!(
                                from = %device_id,
                                to = %payload.to_device_id,
                                "relay dropped: target offline"
                            );
                        }
                    }
                    RelayClientMsg::MailboxDeposit { payload } => {
                        let MailboxDepositPayload {
                            to_device_id,
                            ciphertext,
                            nonce,
                            ttl_seconds,
                            message_id,
                            dedup_key,
                        } = payload;
                        let ack_id = message_id.clone().unwrap_or_default();
                        let Some(room) = joined_room.as_ref() else {
                            let _ = tx.send(RelayServerMsg::MailboxDepositAck {
                                payload: MailboxDepositAckPayload {
                                    id: ack_id,
                                    ok: false,
                                    error: Some("send join before mailbox deposit".to_string()),
                                },
                            });
                            continue;
                        };
                        let (quota_bytes, _) = state.effective_quota(room);
                        match state.mailboxes.deposit(
                            room,
                            &device_id,
                            &to_device_id,
                            ciphertext,
                            nonce,
                            ttl_seconds,
                            message_id,
                            quota_bytes,
                            dedup_key,
                        ) {
                            Ok(id) => {
                                info!(
                                    from = %device_id,
                                    to = %to_device_id,
                                    message_id = %id,
                                    "mailbox deposit accepted (durable)"
                                );
                                let _ = tx.send(RelayServerMsg::MailboxDepositAck {
                                    payload: MailboxDepositAckPayload {
                                        id,
                                        ok: true,
                                        error: None,
                                    },
                                });
                            }
                            Err(e) => {
                                if e == "quota_exceeded" {
                                    let usage_bytes = state.mailboxes.usage_bytes(room);
                                    warn!(
                                        from = %device_id,
                                        room = %room,
                                        usage_bytes,
                                        quota_bytes,
                                        "mailbox deposit rejected: quota exceeded"
                                    );
                                } else {
                                    warn!(from = %device_id, to = %to_device_id, error = %e, "mailbox deposit rejected");
                                }
                                let _ = tx.send(RelayServerMsg::MailboxDepositAck {
                                    payload: MailboxDepositAckPayload {
                                        id: ack_id,
                                        ok: false,
                                        error: Some(e),
                                    },
                                });
                            }
                        }
                    }
                    RelayClientMsg::MailboxPoll { payload } => {
                        let Some(room) = joined_room.as_ref() else {
                            let _ = tx.send(RelayServerMsg::Error {
                                payload: ErrorPayload {
                                    code: "not_joined".to_string(),
                                    message: "send join before mailbox poll".to_string(),
                                },
                            });
                            continue;
                        };
                        let pending = state.mailboxes.poll(room, &device_id, payload.max_count);
                        send_mailbox_batches(&tx, pending);
                    }
                    RelayClientMsg::MailboxAck { payload } => {
                        let Some(room) = joined_room.as_ref() else {
                            continue;
                        };
                        state.mailboxes.ack(room, &device_id, &payload.message_ids);
                    }
                    RelayClientMsg::Pong => {
                        // Heartbeat reply; nothing to do — any received message
                        // already refreshes the receive timeout.
                    }
                }
            }
            Ok(Some(Ok(Message::Close(_)))) => {
                // The peer is closing the connection gracefully. Break out
                // immediately and clean up presence — continuing here would
                // leave the device marked online until the heartbeat timeout
                // (or forever if the peer's socket lingers).
                break;
            }
            Ok(Some(Ok(_))) => continue,
            Ok(Some(Err(e))) => {
                warn!(error = %e, "websocket error");
                break;
            }
            Ok(None) => break,
            Err(_) => {
                warn!(device_id = %device_id, "heartbeat timeout");
                break;
            }
        }
    };

    // Cleanup on disconnect. CRITICAL ORDERING: leave the room FIRST so the
    // DeviceConn's tx sender is dropped — `forward_task` awaits `rx.recv()`,
    // which only returns None once its sender is gone. Awaiting forward_task
    // before leave() deadlocks: the device stays in the room (marked online
    // forever, "left room" never logged) even though its socket is closed.
    if let Some(room) = joined_room {
        state.leave(&room, &device_id, conn_seq);
        state.broadcast(
            &room,
            None,
            RelayServerMsg::PeerOffline {
                payload: PeerPayload {
                    device_id: device_id.clone(),
                },
            },
        );
        let ids = state.device_ids_in_room(&room);
        state.broadcast(
            &room,
            None,
            RelayServerMsg::Presence {
                payload: PresencePayload { device_ids: ids },
            },
        );
        info!(device_id = %device_id, room = %room, "left room");
    }

    heartbeat_task.abort();
    let _ = forward_task.await;

    let _ = result;
}

// ── Application builder (also used by tests) ───────────────────────────────

fn app(state: Arc<AppState>) -> Router {
    use axum::routing::{patch, post, put};
    Router::new()
        .route("/v1/signaling", get(signaling_handler))
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/register", post(api_register))
        .route("/api/login", post(api_login))
        .route("/api/refresh", post(api_refresh))
        .route("/api/devices", get(api_list_devices))
        .route("/api/devices/:id", patch(api_rename_device).delete(api_remove_device))
        .route("/api/plans", get(api_plans))
        .route("/api/storage", get(api_storage))
        .route(
            "/api/storage/orders",
            post(api_create_storage_order).get(api_list_my_storage_orders),
        )
        .route("/api/storage/orders/:id/status", get(api_storage_order_status))
        .route("/pay/:order_id", get(pay_page))
        .route("/api/pay/alipay/notify", post(api_alipay_notify))
        .route("/api/admin/users", get(api_admin_list_users))
        .route("/api/admin/users/:id/quota", put(api_admin_set_quota))
        .route("/api/admin/orders", get(api_admin_list_orders))
        .route("/api/admin/orders/:id/confirm", post(api_admin_confirm_order))
        .route("/api/admin/orders/:id/reject", post(api_admin_reject_order))
        .with_state(state)
}

// ── Account HTTP API ───────────────────────────────────────────────────────

#[derive(Deserialize)]
struct RegisterRequest {
    email: String,
    password: String,
}

#[derive(Serialize)]
struct RegisterResponse {
    user_id: String,
}

async fn api_register(
    State(state): State<Arc<AppState>>,
    axum::Json(req): axum::Json<RegisterRequest>,
) -> Result<axum::Json<RegisterResponse>, (StatusCode, String)> {
    let user_id = state
        .auth
        .register(&state.db, &req.email.trim(), &req.password)
        .map_err(|e| (StatusCode::CONFLICT, e.to_string()))?;
    info!(user_id = %user_id, "account registered");
    Ok(axum::Json(RegisterResponse { user_id }))
}

#[derive(Deserialize)]
struct LoginRequest {
    email: String,
    password: String,
    device_id: String,
    device_name: String,
}

#[derive(Serialize)]
struct LoginResponse {
    access_token: String,
    refresh_token: String,
    user_id: String,
    /// Account-level sync key (base64), same for every device of the account.
    sync_key: String,
}

async fn api_login(
    State(state): State<Arc<AppState>>,
    axum::Json(req): axum::Json<LoginRequest>,
) -> Result<axum::Json<LoginResponse>, (StatusCode, String)> {
    let user_id = state
        .auth
        .login(&state.db, &req.email.trim(), &req.password)
        .map_err(|e| (StatusCode::UNAUTHORIZED, e.to_string()))?;
    let device_id = req.device_id.trim();
    let _ = state
        .db
        .register_device(&user_id, device_id, &req.device_name.trim());
    let token = state
        .auth
        .issue_device_token(&user_id, device_id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let refresh_token = state.auth.issue_refresh_token();
    let _ = state
        .db
        .set_device_refresh_token(device_id, &refresh_token);
    let sync_key = state
        .auth
        .sync_key(&state.db, &user_id)
        .unwrap_or_default();
    info!(user_id = %user_id, device_id = %device_id, "device logged in");
    Ok(axum::Json(LoginResponse {
        access_token: token,
        refresh_token,
        user_id,
        sync_key,
    }))
}

#[derive(Deserialize)]
struct RefreshRequest {
    refresh_token: String,
}

#[derive(Serialize)]
struct RefreshResponse {
    access_token: String,
    refresh_token: String,
    user_id: String,
}

async fn api_refresh(
    State(state): State<Arc<AppState>>,
    axum::Json(req): axum::Json<RefreshRequest>,
) -> Result<axum::Json<RefreshResponse>, (StatusCode, String)> {
    let device = state
        .auth
        .validate_refresh_token(&state.db, &req.refresh_token)
        .ok_or((StatusCode::UNAUTHORIZED, "invalid refresh token".to_string()))?;
    if device.revoked_at.is_some() {
        return Err((StatusCode::UNAUTHORIZED, "device revoked".to_string()));
    }
    let token = state
        .auth
        .issue_device_token(&device.user_id, &device.id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    // Rotate the refresh token on every use.
    let new_refresh_token = state.auth.issue_refresh_token();
    let _ = state
        .db
        .set_device_refresh_token(&device.id, &new_refresh_token);
    Ok(axum::Json(RefreshResponse {
        access_token: token,
        refresh_token: new_refresh_token,
        user_id: device.user_id,
    }))
}

fn bearer_claims(
    state: &AppState,
    headers: &axum::http::HeaderMap,
) -> Result<auth::Claims, (StatusCode, String)> {
    let header = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or((StatusCode::UNAUTHORIZED, "missing authorization".to_string()))?;
    let token = header.strip_prefix("Bearer ").unwrap_or(header);
    let claims = state
        .auth
        .validate(token)
        .map_err(|e| (StatusCode::UNAUTHORIZED, format!("invalid token: {e}")))?;
    // Same device gate as the websocket connect path: with long-lived tokens
    // the device row is the only access control, so it must apply to every
    // authenticated endpoint, not just the socket.
    match state.db.get_device(&claims.device_id) {
        Some(d) if d.revoked_at.is_none() => Ok(claims),
        Some(_) => Err((StatusCode::UNAUTHORIZED, "device revoked".to_string())),
        None => Err((StatusCode::UNAUTHORIZED, "unknown device".to_string())),
    }
}

#[derive(Serialize)]
struct DeviceRow {
    device_id: String,
    name: String,
    online: bool,
    last_seen_at: Option<String>,
}

async fn api_list_devices(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> Result<axum::Json<Vec<DeviceRow>>, (StatusCode, String)> {
    let claims = bearer_claims(&state, &headers)?;
    let devices = state.db.list_devices(&claims.sub);
    Ok(axum::Json(
        devices
            .into_iter()
            .map(|d| DeviceRow {
                device_id: d.id.clone(),
                name: d.name,
                online: state.is_online(&d.id),
                last_seen_at: None,
            })
            .collect(),
    ))
}

async fn api_rename_device(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::Json(req): axum::Json<serde_json::Value>,
) -> Result<StatusCode, (StatusCode, String)> {
    let claims = bearer_claims(&state, &headers)?;
    let name = req
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    state.db.rename_device(&claims.sub, &id, &name);
    Ok(StatusCode::OK)
}

async fn api_remove_device(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let claims = bearer_claims(&state, &headers)?;
    let ok = state.db.delete_device(&claims.sub, &id);
    if !ok {
        return Err((StatusCode::NOT_FOUND, "device not found".to_string()));
    }
    info!(device_id = %id, "device removed");
    Ok(StatusCode::OK)
}

// ── Storage quota / subscription HTTP API ──────────────────────────────────

/// Admin endpoints stay hidden (404) unless RELAY_ADMIN_TOKEN is configured;
/// with a token set, a missing or wrong bearer gets 401.
fn require_admin(
    state: &AppState,
    headers: &axum::http::HeaderMap,
) -> Result<(), (StatusCode, String)> {
    let Some(expected) = &state.admin_token else {
        return Err((StatusCode::NOT_FOUND, "not found".to_string()));
    };
    let token = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    match token {
        Some(t) if t == expected => Ok(()),
        _ => Err((StatusCode::UNAUTHORIZED, "invalid admin token".to_string())),
    }
}

async fn api_plans(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> Result<axum::Json<Vec<Plan>>, (StatusCode, String)> {
    bearer_claims(&state, &headers)?;
    Ok(axum::Json(PLANS.to_vec()))
}

#[derive(Serialize)]
struct StorageResponse {
    used_bytes: i64,
    quota_bytes: i64,
    plan_id: String,
    expires_at: Option<String>,
}

async fn api_storage(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> Result<axum::Json<StorageResponse>, (StatusCode, String)> {
    let claims = bearer_claims(&state, &headers)?;
    let (quota_bytes, expires_at) = state.effective_quota(&claims.sub);
    Ok(axum::Json(StorageResponse {
        used_bytes: state.mailboxes.usage_bytes(&claims.sub),
        quota_bytes,
        plan_id: plan_id_for_quota(quota_bytes).to_string(),
        expires_at,
    }))
}

#[derive(Deserialize)]
struct CreateOrderRequest {
    plan_id: String,
    /// "month" (30 days) or "year" (365 days).
    period: String,
    /// 支付渠道：仅识别 "alipay"；缺省/其他值 = 线下转账。
    #[serde(default)]
    channel: Option<String>,
}

#[derive(Serialize)]
struct CreateOrderResponse {
    order_id: String,
    plan_id: String,
    quota_bytes: i64,
    duration_days: u32,
    amount_cny: f64,
    status: db::OrderStatus,
    /// Out-of-band payment instructions (RELAY_PAYMENT_INFO) for the client
    /// to display alongside the order id.
    payment_info: String,
    /// 在线支付页 URL（支付宝已配置且 channel=alipay 时），客户端跳浏览器打开。
    pay_url: Option<String>,
}

fn order_response(order: &db::Order, state: &AppState) -> CreateOrderResponse {
    CreateOrderResponse {
        order_id: order.id.clone(),
        plan_id: order.plan_id.clone(),
        quota_bytes: order.quota_bytes,
        duration_days: order.duration_days,
        amount_cny: order.amount_cny,
        status: order.status,
        payment_info: state.payment_info.clone(),
        pay_url: state.order_pay_url(order),
    }
}

async fn api_create_storage_order(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    axum::Json(req): axum::Json<CreateOrderRequest>,
) -> Result<axum::Json<CreateOrderResponse>, (StatusCode, String)> {
    let claims = bearer_claims(&state, &headers)?;
    let plan = find_plan(&req.plan_id)
        .filter(|p| p.quota_bytes > state.default_quota)
        .ok_or((StatusCode::BAD_REQUEST, "unknown plan".to_string()))?;
    let (duration_days, amount_cny) = match req.period.as_str() {
        "month" => (30, plan.monthly_cny),
        "year" => (365, plan.yearly_cny),
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                "period must be \"month\" or \"year\"".to_string(),
            ))
        }
    };
    // 支付渠道归一化：只识别 "alipay"；relay 未配置支付宝时即使有渠道请求
    // 也不下发 pay_url（order_pay_url 自然为 None），前端回退到线下转账说明。
    let channel = req.channel.as_deref().filter(|c| *c == "alipay");
    // Idempotent only for the SAME plan+period: a repeated submission returns
    // the existing pending order instead of piling up duplicates. A pending
    // order for a DIFFERENT plan/period is the user changing their mind —
    // cancel the stale one and create the new selection.
    if let Some(existing) = state.db.pending_order_for_user(&claims.sub) {
        if existing.plan_id == plan.id && existing.duration_days == duration_days {
            if let Some(c) = channel {
                let _ = state.db.set_order_channel(&existing.id, c);
            }
            let existing = state.db.get_order(&existing.id).unwrap_or(existing);
            return Ok(axum::Json(order_response(&existing, &state)));
        }
        let _ = state.db.update_order_status(
            &existing.id,
            db::OrderStatus::Cancelled,
            None,
            Some("superseded by a new order".to_string()),
        );
        info!(user_id = %claims.sub, order_id = %existing.id, "stale pending order cancelled (plan changed)");
    }
    let order = db::Order {
        id: uuid::Uuid::new_v4().to_string(),
        user_id: claims.sub.clone(),
        plan_id: plan.id.to_string(),
        quota_bytes: plan.quota_bytes,
        duration_days,
        amount_cny,
        status: db::OrderStatus::Pending,
        created_at: now_iso(),
        paid_at: None,
        admin_note: None,
        channel: channel.map(|c| c.to_string()),
        trade_no: None,
    };
    state
        .db
        .create_order(order.clone())
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    info!(user_id = %claims.sub, order_id = %order.id, plan = %plan.id, "storage order created");
    Ok(axum::Json(order_response(&order, &state)))
}

/// 用户侧订单列表的响应行（GET /api/storage/orders）。与 db::Order 分离：
/// payment_info 来自运行时配置（RELAY_PAYMENT_INFO），不入库存储，且只在
/// 订单仍为 pending 时下发——历史订单不再携带收款信息。
#[derive(Serialize)]
struct OrderRow {
    id: String,
    user_id: String,
    plan_id: String,
    quota_bytes: i64,
    duration_days: u32,
    amount_cny: f64,
    status: db::OrderStatus,
    created_at: String,
    paid_at: Option<String>,
    admin_note: Option<String>,
    payment_info: String,
    /// 在线支付页 URL：pending 且选了支付宝渠道时下发，客户端可再次跳浏览器。
    pay_url: Option<String>,
}

async fn api_list_my_storage_orders(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> Result<axum::Json<Vec<OrderRow>>, (StatusCode, String)> {
    let claims = bearer_claims(&state, &headers)?;
    let orders = state
        .db
        .list_orders(None)
        .into_iter()
        .filter(|o| o.user_id == claims.sub)
        .map(|o| OrderRow {
            payment_info: if o.status == db::OrderStatus::Pending {
                state.payment_info.clone()
            } else {
                String::new()
            },
            pay_url: state.order_pay_url(&o),
            id: o.id,
            user_id: o.user_id,
            plan_id: o.plan_id,
            quota_bytes: o.quota_bytes,
            duration_days: o.duration_days,
            amount_cny: o.amount_cny,
            status: o.status,
            created_at: o.created_at,
            paid_at: o.paid_at,
            admin_note: o.admin_note,
        })
        .collect();
    Ok(axum::Json(orders))
}

/// 订单状态查询：带有效 bearer 且是本人才返回完整订单行；无鉴权时凭订单 id
///（UUID 能力 URL）只返回 {"status": "..."}，供支付页轮询。
async fn api_storage_order_status(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<axum::Json<serde_json::Value>, (StatusCode, String)> {
    let order = state
        .db
        .get_order(&id)
        .ok_or((StatusCode::NOT_FOUND, "order not found".to_string()))?;
    let authed_owner = bearer_claims(&state, &headers)
        .ok()
        .filter(|c| c.sub == order.user_id)
        .is_some();
    if authed_owner {
        let row = OrderRow {
            payment_info: if order.status == db::OrderStatus::Pending {
                state.payment_info.clone()
            } else {
                String::new()
            },
            pay_url: state.order_pay_url(&order),
            id: order.id,
            user_id: order.user_id,
            plan_id: order.plan_id,
            quota_bytes: order.quota_bytes,
            duration_days: order.duration_days,
            amount_cny: order.amount_cny,
            status: order.status,
            created_at: order.created_at,
            paid_at: order.paid_at,
            admin_note: order.admin_note,
        };
        return Ok(axum::Json(serde_json::to_value(row).unwrap()));
    }
    Ok(axum::Json(serde_json::json!({ "status": order.status })))
}

#[derive(Serialize)]
struct AdminUserRow {
    user_id: String,
    email: String,
    quota_bytes: i64,
    used_bytes: i64,
    expires_at: Option<String>,
    device_count: usize,
}

async fn api_admin_list_users(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> Result<axum::Json<Vec<AdminUserRow>>, (StatusCode, String)> {
    require_admin(&state, &headers)?;
    let users = state
        .db
        .list_users()
        .into_iter()
        .map(|u| {
            let (quota_bytes, expires_at) = state.effective_quota(&u.id);
            AdminUserRow {
                used_bytes: state.mailboxes.usage_bytes(&u.id),
                device_count: state.db.list_devices(&u.id).len(),
                user_id: u.id,
                email: u.email,
                quota_bytes,
                expires_at,
            }
        })
        .collect();
    Ok(axum::Json(users))
}

#[derive(Deserialize)]
struct SetQuotaRequest {
    quota_bytes: i64,
    /// RFC3339; omitted = permanent (gifts, test accounts).
    #[serde(default)]
    expires_at: Option<String>,
}

async fn api_admin_set_quota(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::Json(req): axum::Json<SetQuotaRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    require_admin(&state, &headers)?;
    state
        .db
        .set_user_quota(&id, Some(req.quota_bytes), req.expires_at.clone())
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;
    info!(user_id = %id, quota_bytes = req.quota_bytes, expires_at = ?req.expires_at, "admin set user quota");
    Ok(StatusCode::OK)
}

#[derive(Deserialize)]
struct AdminOrdersQuery {
    status: Option<String>,
}

async fn api_admin_list_orders(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Query(query): Query<AdminOrdersQuery>,
) -> Result<axum::Json<Vec<db::Order>>, (StatusCode, String)> {
    require_admin(&state, &headers)?;
    let status = match query.status.as_deref() {
        None => None,
        Some("pending") => Some(db::OrderStatus::Pending),
        Some("paid") => Some(db::OrderStatus::Paid),
        Some("rejected") => Some(db::OrderStatus::Rejected),
        Some("cancelled") => Some(db::OrderStatus::Cancelled),
        Some(_) => return Err((StatusCode::BAD_REQUEST, "unknown status".to_string())),
    };
    Ok(axum::Json(state.db.list_orders(status)))
}

/// 订单履约：激活配额并置为已支付。当前订阅未过期则新周期叠加到到期日
///（续期），否则从现在开始。管理员确认与支付宝回调共用。
///
/// 整个「检查未支付 → 叠加配额 → 置为已支付」在 db 层同一把锁内完成：
/// 并发的重复回调/管理员确认里只有第一个真正履约，其余得到 AlreadyPaid，
/// 配额绝不会重复叠加。
fn activate_order(
    state: &AppState,
    order_id: &str,
    trade_no: Option<String>,
    note: Option<String>,
) -> Result<db::Activation, (StatusCode, String)> {
    state
        .db
        .activate_order(order_id, trade_no, note)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// Confirm payment: activate the order's quota. When the user's current
/// subscription is still live the new period stacks onto its expiry
/// (renewal), otherwise it starts from now.
async fn api_admin_confirm_order(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<axum::Json<db::Order>, (StatusCode, String)> {
    require_admin(&state, &headers)?;
    let order = state
        .db
        .get_order(&id)
        .ok_or((StatusCode::NOT_FOUND, "order not found".to_string()))?;
    if order.status != db::OrderStatus::Pending {
        return Err((StatusCode::CONFLICT, "order is not pending".to_string()));
    }
    // 线下转账只应履约 pending 订单；与支付回调并发时由 db 层原子履约兜底，
    // 已被回调抢先支付的订单在这里得到 AlreadyPaid，按冲突处理。
    match activate_order(&state, &id, None, None)? {
        db::Activation::Fulfilled(order) => {
            info!(order_id = %id, user_id = %order.user_id, quota_bytes = order.quota_bytes, "storage order confirmed");
            Ok(axum::Json(order))
        }
        db::Activation::AlreadyPaid(_) => {
            Err((StatusCode::CONFLICT, "order is not pending".to_string()))
        }
    }
}

#[derive(Deserialize)]
struct RejectOrderRequest {
    #[serde(default)]
    note: Option<String>,
}

async fn api_admin_reject_order(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    body: Option<axum::Json<RejectOrderRequest>>,
) -> Result<axum::Json<db::Order>, (StatusCode, String)> {
    require_admin(&state, &headers)?;
    let order = state
        .db
        .get_order(&id)
        .ok_or((StatusCode::NOT_FOUND, "order not found".to_string()))?;
    if order.status != db::OrderStatus::Pending {
        return Err((StatusCode::CONFLICT, "order is not pending".to_string()));
    }
    let note = body.and_then(|b| b.0.note);
    let order = state
        .db
        .update_order_status(&id, db::OrderStatus::Rejected, None, note)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    info!(order_id = %id, user_id = %order.user_id, "storage order rejected");
    Ok(axum::Json(order))
}

// ── 支付宝在线支付（收银台页 + 异步通知）────────────────────────────────────

/// 在线支付订单的支付窗口（分钟），与支付宝 timeout_express 一致。
const ORDER_PAY_TIMEOUT_MINUTES: i64 = 30;

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// 账号打码展示：u***@x.com。
fn mask_email(email: &str) -> String {
    match email.split_once('@') {
        Some((local, domain)) => match local.chars().next() {
            Some(first) => format!("{first}***@{domain}"),
            None => format!("***@{domain}"),
        },
        None => "***".to_string(),
    }
}

fn pay_page_html(title: &str, body: &str, extra_head: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title} - 思库云端存储</title>
{extra_head}
<style>
body {{ margin: 0; min-height: 100vh; display: flex; align-items: center; justify-content: center;
  background: #f5f6f8; font-family: -apple-system, "PingFang SC", "Microsoft YaHei", sans-serif; color: #1f2329; }}
.card {{ background: #fff; border-radius: 12px; padding: 32px; max-width: 420px; width: calc(100% - 48px);
  box-shadow: 0 4px 24px rgba(0,0,0,.08); text-align: center; }}
h1 {{ font-size: 18px; margin: 0 0 16px; }}
.meta {{ font-size: 13px; color: #646a73; line-height: 1.8; }}
.meta .amount {{ color: #1f2329; font-size: 20px; font-weight: 600; }}
.hint {{ font-size: 12px; color: #8f959e; margin-top: 16px; line-height: 1.7; }}
a.btn {{ display: inline-block; margin-top: 16px; padding: 10px 24px; background: #1677ff; color: #fff;
  border-radius: 8px; text-decoration: none; font-size: 14px; }}
</style>
</head>
<body><div class="card">{body}</div></body>
</html>"#
    )
}

/// 支付页轮询脚本：每 3s 查一次订单状态，变为 paid 就显示成功。
fn pay_page_poll_script(order_id: &str) -> String {
    format!(
        r#"<script>
setInterval(function () {{
  fetch("/api/storage/orders/{order_id}/status").then(function (r) {{ return r.json(); }}).then(function (d) {{
    if (d.status === "paid") {{
      document.querySelector(".card").innerHTML =
        '<h1>支付成功</h1><div class="meta">扩容已生效，可回到思库查看。</div>';
    }}
  }}).catch(function () {{}});
}}, 3000);
</script>"#
    )
}

/// GET /pay/:order_id — 公开的订单支付页（订单 id 是 UUID 能力 URL）。
/// pending 且支付宝已配置 → 自动提交表单跳收银台；pending 超 30 分钟 → 惰性
/// 置为 Cancelled；其余状态展示对应提示页。
async fn pay_page(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(order_id): axum::extract::Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> axum::response::Response {
    let Some(order) = state.db.get_order(&order_id) else {
        return (
            StatusCode::NOT_FOUND,
            axum::response::Html(pay_page_html(
                "订单不存在",
                "<h1>订单不存在</h1><div class=\"meta\">请检查链接是否完整。</div>",
                "",
            )),
        )
            .into_response();
    };
    let email = state
        .db
        .get_user(&order.user_id)
        .map(|u| mask_email(&u.email))
        .unwrap_or_else(|| "***".to_string());
    let meta = format!(
        "<div class=\"meta\">账号：{}<br>套餐：思库云端存储扩容-{}<br><span class=\"amount\">¥{:.2}</span></div>",
        html_escape(&email),
        html_escape(&order.plan_id),
        order.amount_cny,
    );

    // 惰性超时关闭：pending 超过支付窗口即取消（notify 仍可履约，见回调）。
    if order.status == db::OrderStatus::Pending {
        let expired = chrono::DateTime::parse_from_rfc3339(&order.created_at)
            .map(|t| t + chrono::Duration::minutes(ORDER_PAY_TIMEOUT_MINUTES) < chrono::Utc::now())
            .unwrap_or(false);
        if expired {
            let _ = state.db.update_order_status(
                &order.id,
                db::OrderStatus::Cancelled,
                None,
                Some("支付超时自动关闭".to_string()),
            );
            info!(order_id = %order.id, "pending order auto-cancelled on pay page visit (timeout)");
            let body = format!(
                "<h1>订单已关闭</h1>{meta}<div class=\"hint\">订单已超时关闭，请回到思库重新下单。</div>"
            );
            return axum::response::Html(pay_page_html("订单已关闭", &body, "")).into_response();
        }
    }

    match order.status {
        db::OrderStatus::Pending => {
            let (Some(cfg), Some(base)) = (&state.alipay, &state.alipay_public_base) else {
                // 未配置在线支付：引导回线下转账流程。
                let body = format!(
                    "<h1>待支付</h1>{meta}<div class=\"hint\">该订单需线下转账支付，请回到思库查看付款方式，转账时备注订单号。</div>{}",
                    pay_page_poll_script(&order.id),
                );
                return axum::response::Html(pay_page_html("待支付", &body, "")).into_response();
            };
            // 支付完成后支付宝会把浏览器重定向回 return_url（本页）并带上
            // out_trade_no 等查询参数；此刻异步 notify 可能还没到（订单仍是
            // pending）。绝不能再次跳收银台，渲染确认页轮询等回调履约。
            if query.contains_key("out_trade_no") {
                let body = format!(
                    "<h1>支付结果确认中…</h1>{meta}<div class=\"hint\">正在等待支付结果，通常只需几秒钟。</div>{}",
                    pay_page_poll_script(&order.id),
                );
                return axum::response::Html(pay_page_html("支付确认中", &body, ""))
                    .into_response();
            }
            // GET 重定向拼接（支付宝官方推荐）：所有参数进 URL 查询串，字符集
            // 无歧义。POST 表单提交时支付宝对 body 的字符集处理与签名所用
            // UTF-8 不一致，中文 subject 会触发 invalid-signature。
            let url = html_escape(&pay::page_pay_url(
                &order.id,
                &order.plan_id,
                order.amount_cny,
                cfg,
                base,
            ));
            let body = format!(
                "<h1>正在跳转到支付宝收银台…</h1>{meta}\
                 <a id=\"go\" class=\"btn\" href=\"{url}\">如果没有自动跳转，点击这里支付</a>\
                 <div class=\"hint\">请在 30 分钟内完成支付，超时订单将自动关闭。</div>\
                 <script>document.getElementById(\"go\").click();</script>"
            );
            axum::response::Html(pay_page_html("去支付", &body, "")).into_response()
        }
        db::OrderStatus::Paid => {
            let body = format!("<h1>支付成功</h1>{meta}<div class=\"hint\">扩容已生效，可回到思库查看。</div>");
            axum::response::Html(pay_page_html("支付成功", &body, "")).into_response()
        }
        db::OrderStatus::Cancelled | db::OrderStatus::Rejected => {
            let body = format!("<h1>订单已关闭</h1>{meta}<div class=\"hint\">如有疑问请回到思库重新下单或联系管理员。</div>");
            axum::response::Html(pay_page_html("订单已关闭", &body, "")).into_response()
        }
    }
}

/// POST /api/pay/alipay/notify — 支付宝异步通知（form-urlencoded）。
/// 任何校验失败都返回非 200 + "fail"（支付宝会按策略重发）；处理成功必须
/// 返回字面量 "success"。
async fn api_alipay_notify(
    State(state): State<Arc<AppState>>,
    axum::Form(params): axum::Form<HashMap<String, String>>,
) -> (StatusCode, &'static str) {
    let fail = (StatusCode::BAD_REQUEST, "fail");
    let Some(cfg) = &state.alipay else {
        warn!("alipay notify received but alipay is not configured");
        return fail;
    };
    let sorted: std::collections::BTreeMap<String, String> = params.iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let sign = params.get("sign").cloned().unwrap_or_default();
    if params.get("sign_type").map(|s| s.as_str()) != Some("RSA2")
        || !pay::rsa2_verify(&sorted, &sign, &cfg.alipay_public_key)
    {
        warn!("alipay notify signature verification failed");
        return fail;
    }
    if params.get("app_id").map(|s| s.as_str()) != Some(cfg.app_id.as_str()) {
        warn!("alipay notify app_id mismatch");
        return fail;
    }
    // 只处理终态；WAIT_BUYER_PAY 等中间态直接确认收到，不再处理业务。
    let trade_status = params.get("trade_status").map(|s| s.as_str()).unwrap_or("");
    if trade_status != "TRADE_SUCCESS" && trade_status != "TRADE_FINISHED" {
        return (StatusCode::OK, "success");
    }
    let Some(out_trade_no) = params.get("out_trade_no") else {
        return fail;
    };
    let Some(order) = state.db.get_order(out_trade_no) else {
        warn!(out_trade_no = %out_trade_no, "alipay notify for unknown order");
        return fail;
    };
    // 金额按分比对，避免 "60" / "60.00" 这类格式差异造成误判。
    let expected_cents = (order.amount_cny * 100.0).round() as i64;
    let got_cents = params
        .get("total_amount")
        .and_then(|s| s.parse::<f64>().ok())
        .map(|v| (v * 100.0).round() as i64);
    if got_cents != Some(expected_cents) {
        warn!(order_id = %order.id, expected_cents, "alipay notify amount mismatch");
        return fail;
    }
    let trade_no = params.get("trade_no").cloned();
    // 已取消/被拒绝的订单收到回调也必须履约：用户真金白银付了款，绝不允许
    // 收钱不给货；记 admin_note 便于事后审计。履约是原子的：并发/重复回调
    // 只有第一个生效，其余走 AlreadyPaid 幂等确认，配额不会重复叠加。
    let note = match order.status {
        db::OrderStatus::Cancelled => Some("逾期支付自动履约".to_string()),
        db::OrderStatus::Rejected => Some("已拒绝订单收到支付回调，自动履约".to_string()),
        _ => None,
    };
    match activate_order(&state, out_trade_no, trade_no.clone(), note) {
        Ok(db::Activation::Fulfilled(order)) => {
            info!(order_id = %order.id, user_id = %order.user_id, trade_no = ?trade_no, "storage order paid via alipay notify");
            (StatusCode::OK, "success")
        }
        Ok(db::Activation::AlreadyPaid(_)) => (StatusCode::OK, "success"),
        Err((_, e)) => {
            warn!(order_id = %order.id, error = %e, "alipay notify fulfillment failed");
            fail
        }
    }
}



// ── Entry point ────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let state = Arc::new(AppState::new());
    let addr = listen_addr();
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    info!(addr = %addr, "siku-sync-relay listening");
    axum::serve(listener, app(state)).await.unwrap();
}

// ── Integration tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn mailbox_msg(id: &str, ciphertext_len: usize) -> MailboxMessage {
        MailboxMessage {
            id: id.to_string(),
            from_device_id: "dev".to_string(),
            ciphertext: "x".repeat(ciphertext_len),
            nonce: "n".to_string(),
            account_level: false,
        }
    }

    /// Mixed-size messages are grouped so each frame's cumulative
    /// ciphertext+nonce stays within the budget, preserving order.
    #[test]
    fn mailbox_batch_chunking_groups_by_byte_budget() {
        // Sizes (ciphertext+nonce): 41, 41, 41, 11 — budget 90.
        let msgs = vec![
            mailbox_msg("a", 40),
            mailbox_msg("b", 40),
            mailbox_msg("c", 40),
            mailbox_msg("d", 10),
        ];
        let batches = chunk_mailbox_messages(msgs, 90);
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(batches[1].iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["c", "d"]);
    }

    /// A message larger than the budget still gets delivered: it occupies its
    /// own frame and does not merge with neighbours.
    #[test]
    fn mailbox_batch_chunking_oversized_message_gets_own_frame() {
        let msgs = vec![
            mailbox_msg("small-1", 10),
            mailbox_msg("huge", 1024),
            mailbox_msg("small-2", 10),
        ];
        let batches = chunk_mailbox_messages(msgs, 90);
        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0][0].id, "small-1");
        assert_eq!(batches[1].len(), 1);
        assert_eq!(batches[1][0].id, "huge");
        assert_eq!(batches[2][0].id, "small-2");
    }

    #[test]
    fn mailbox_batch_chunking_empty_input() {
        assert!(chunk_mailbox_messages(Vec::new(), 90).is_empty());
    }

    fn make_token(sub: &str, device_id: &str) -> String {
        use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
        let exp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as usize
            + 3600;
        let claims = Claims {
            sub: sub.to_string(),
            device_id: device_id.to_string(),
            exp,
        };
        encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(jwt_secret().as_bytes()),
        )
        .unwrap()
    }

    /// The signaling handler rejects unknown devices (401), so WebSocket
    /// integration tests must register the user + devices in the account db
    /// first — mirroring a real login flow.
    fn seed_account(state: &AppState) {
        state
            .db
            .create_user("user-1", "test@example.com", "hash", "sync-key")
            .unwrap();
        state.db.register_device("user-1", "device-a", "A").unwrap();
        state.db.register_device("user-1", "device-b", "B").unwrap();
    }

    #[tokio::test]
    async fn two_peers_see_each_other() {
        use futures::{SinkExt, StreamExt};
        use tokio_tungstenite::connect_async;
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        use tokio_tungstenite::tungstenite::Message;

        std::env::set_var("JWT_SECRET", "test-secret");
        let state = Arc::new(AppState::new());
        seed_account(&state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app(state)).await.unwrap();
        });

        // Give the server a moment to start.
        tokio::time::sleep(Duration::from_millis(100)).await;

        let token_a = make_token("user-1", "device-a");
        let token_b = make_token("user-1", "device-b");

        // Newer clients send the token in the Authorization header (never in
        // the URL query string, which proxies log).
        async fn connect_with_header(
            port: u16,
            token: &str,
        ) -> (
            tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
            axum::http::Response<Option<Vec<u8>>>,
        ) {
            let mut request = format!("ws://127.0.0.1:{port}/v1/signaling")
                .into_client_request()
                .unwrap();
            request.headers_mut().insert(
                axum::http::header::AUTHORIZATION,
                axum::http::HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
            );
            connect_async(request).await.unwrap()
        }
        let (mut a, _) = connect_with_header(port, &token_a).await;
        let (mut b, _) = connect_with_header(port, &token_b).await;

        // Both join the same room.
        a.send(Message::Text(
            r#"{"type":"join","payload":{"room_id":"user-1"}}"#.into(),
        ))
        .await
        .unwrap();
        b.send(Message::Text(
            r#"{"type":"join","payload":{"room_id":"user-1"}}"#.into(),
        ))
        .await
        .unwrap();

        // Collect messages until each side sees the other's PeerOnline.
        let timeout = Duration::from_secs(5);
        let a_seen = tokio::time::timeout(timeout, async {
            while let Some(Ok(Message::Text(text))) = a.next().await {
                if text.contains("peer_online") && text.contains("device-b") {
                    return true;
                }
            }
            false
        })
        .await
        .unwrap();
        let b_seen = tokio::time::timeout(timeout, async {
            while let Some(Ok(Message::Text(text))) = b.next().await {
                if text.contains("peer_online") && text.contains("device-a") {
                    return true;
                }
            }
            false
        })
        .await
        .unwrap();

        assert!(a_seen, "device-a should see device-b online");
        assert!(b_seen, "device-b should see device-a online");
    }

    /// Regression: a graceful WebSocket close (client sends a Close frame but
    /// its socket lingers) must mark the device offline immediately. The old
    /// handler did `continue` on Close, waiting for the socket to die — a
    /// client that closes the WS cleanly without dropping the TCP socket (or
    /// whose FIN is delayed) stayed "online" until the heartbeat timeout.
    #[tokio::test]
    async fn graceful_close_marks_device_offline() {
        use futures::{SinkExt, StreamExt};
        use tokio_tungstenite::{connect_async, tungstenite::Message};

        std::env::set_var("JWT_SECRET", "test-secret");
        let state = Arc::new(AppState::new());
        seed_account(&state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server_state = state.clone();
        tokio::spawn(async move {
            axum::serve(listener, app(server_state)).await.unwrap();
        });
        tokio::time::sleep(Duration::from_millis(100)).await;

        let token_a = make_token("user-1", "device-a");
        let token_b = make_token("user-1", "device-b");

        let (mut a, _) = connect_async(format!("ws://127.0.0.1:{}/v1/signaling?token={}", port, token_a))
            .await
            .unwrap();
        let (mut b, _) = connect_async(format!("ws://127.0.0.1:{}/v1/signaling?token={}", port, token_b))
            .await
            .unwrap();
        a.send(Message::Text(
            r#"{"type":"join","payload":{"room_id":"user-1"}}"#.into(),
        ))
        .await
        .unwrap();
        b.send(Message::Text(
            r#"{"type":"join","payload":{"room_id":"user-1"}}"#.into(),
        ))
        .await
        .unwrap();

        // Let the server register both devices.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(state.is_online("device-a"), "A should be online after join");

        // B should observe A's PeerOnline.
        let b_saw_online = tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(Ok(Message::Text(text))) = b.next().await {
                if text.contains("peer_online") && text.contains("device-a") {
                    return true;
                }
            }
            false
        })
        .await
        .unwrap_or(false);
        assert!(b_saw_online, "B should see A online");

        // A closes the WebSocket gracefully (Close frame, socket stays open).
        a.close(None).await.unwrap();
        drop(a); // socket lingers only until drop; Close frame already sent

        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            !state.is_online("device-a"),
            "A must be offline right after its Close frame, not after the heartbeat timeout"
        );

        // B must receive PeerOffline promptly.
        let b_saw_offline = tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(Ok(Message::Text(text))) = b.next().await {
                if text.contains("peer_offline") && text.contains("device-a") {
                    return true;
                }
            }
            false
        })
        .await
        .unwrap_or(false);
        assert!(b_saw_offline, "B should see A go offline");
    }

    /// Regression: a device holds several live connections (auto-sync
    /// discovery, per-session signaling, mailbox transport). Closing one of
    /// them must NOT mark the device offline while others are still up — the
    /// previous single-connection room map made every session teardown drop
    /// the whole device from presence, so peers saw an online device as
    /// offline.
    #[test]
    fn device_stays_online_until_last_connection_leaves() {
        std::env::set_var("JWT_SECRET", "test-secret");
        let state = Arc::new(AppState::new());
        let (tx1, _rx1) = mpsc::unbounded_channel::<RelayServerMsg>();
        let (tx2, _rx2) = mpsc::unbounded_channel::<RelayServerMsg>();
        let (tx3, _rx3) = mpsc::unbounded_channel::<RelayServerMsg>();

        state.join("user-1", "device-a", 1, tx1);
        assert!(state.is_online("device-a"));
        // A second transport joins the same device.
        state.join("user-1", "device-a", 2, tx2);
        state.join("user-1", "device-a", 3, tx3);
        assert!(state.is_online("device-a"));

        // One connection closes — the device stays online.
        state.leave("user-1", "device-a", 1);
        assert!(state.is_online("device-a"));
        state.leave("user-1", "device-a", 2);
        assert!(state.is_online("device-a"));

        // The LAST connection closes — now the device is offline.
        state.leave("user-1", "device-a", 3);
        assert!(!state.is_online("device-a"));
        assert!(state.device_ids_in_room("user-1").is_empty());
    }

    /// MailboxDeposit over the wire is acked with ok=true and echoes the
    /// client's message_id once the message is durably stored.
    #[tokio::test]
    async fn mailbox_deposit_ack_ok_over_websocket() {
        use futures::{SinkExt, StreamExt};
        use tokio_tungstenite::connect_async;
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        use tokio_tungstenite::tungstenite::Message;

        std::env::set_var("JWT_SECRET", "test-secret");
        let state = Arc::new(AppState::new());
        seed_account(&state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app(state)).await.unwrap();
        });
        tokio::time::sleep(Duration::from_millis(100)).await;

        async fn connect(
            port: u16,
            token: &str,
        ) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
        {
            let mut request = format!("ws://127.0.0.1:{port}/v1/signaling")
                .into_client_request()
                .unwrap();
            request.headers_mut().insert(
                axum::http::header::AUTHORIZATION,
                axum::http::HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
            );
            let (ws, _) = connect_async(request).await.unwrap();
            ws
        }

        let token_a = make_token("user-1", "device-a");
        let token_b = make_token("user-1", "device-b");
        let mut a = connect(port, &token_a).await;
        let mut b = connect(port, &token_b).await;

        // Both join the room — a per-device deposit requires the target to be
        // registered (joined).
        a.send(Message::Text(
            r#"{"type":"join","payload":{"room_id":"user-1"}}"#.into(),
        ))
        .await
        .unwrap();
        b.send(Message::Text(
            r#"{"type":"join","payload":{"room_id":"user-1"}}"#.into(),
        ))
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;

        a.send(Message::Text(
            r#"{"type":"mailbox_deposit","payload":{"to_device_id":"device-b","ciphertext":"cipher-1","nonce":"nonce-1","message_id":"m-1"}}"#.into(),
        ))
        .await
        .unwrap();

        let ack = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(Ok(Message::Text(text))) = a.next().await {
                if text.contains("mailbox_deposit_ack") {
                    return text;
                }
            }
            String::new()
        })
        .await
        .unwrap();
        assert!(ack.contains(r#""ok":true"#), "deposit should be acked ok, got: {ack}");
        assert!(ack.contains(r#""id":"m-1""#), "ack must echo the client message_id, got: {ack}");
    }

    /// MailboxDeposit is rejected (ok=false) when the sender never joined the
    /// room, and when the target device is not in the room.
    #[tokio::test]
    async fn mailbox_deposit_ack_rejected_over_websocket() {
        use futures::{SinkExt, StreamExt};
        use tokio_tungstenite::connect_async;
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        use tokio_tungstenite::tungstenite::Message;

        std::env::set_var("JWT_SECRET", "test-secret");
        let state = Arc::new(AppState::new());
        seed_account(&state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app(state)).await.unwrap();
        });
        tokio::time::sleep(Duration::from_millis(100)).await;

        async fn connect(
            port: u16,
            token: &str,
        ) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
        {
            let mut request = format!("ws://127.0.0.1:{port}/v1/signaling")
                .into_client_request()
                .unwrap();
            request.headers_mut().insert(
                axum::http::header::AUTHORIZATION,
                axum::http::HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
            );
            let (ws, _) = connect_async(request).await.unwrap();
            ws
        }

        let token_a = make_token("user-1", "device-a");
        let mut a = connect(port, &token_a).await;

        // 1) Deposit before joining → rejected.
        a.send(Message::Text(
            r#"{"type":"mailbox_deposit","payload":{"to_device_id":"device-b","ciphertext":"c","nonce":"n","message_id":"m-not-joined"}}"#.into(),
        ))
        .await
        .unwrap();
        let ack = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(Ok(Message::Text(text))) = a.next().await {
                if text.contains("mailbox_deposit_ack") {
                    return text;
                }
            }
            String::new()
        })
        .await
        .unwrap();
        assert!(ack.contains(r#""ok":false"#), "deposit before join must be rejected, got: {ack}");
        assert!(ack.contains(r#""id":"m-not-joined""#), "got: {ack}");

        // 2) Joined, but the target device is not in the room → rejected.
        a.send(Message::Text(
            r#"{"type":"join","payload":{"room_id":"user-1"}}"#.into(),
        ))
        .await
        .unwrap();
        a.send(Message::Text(
            r#"{"type":"mailbox_deposit","payload":{"to_device_id":"ghost-device","ciphertext":"c","nonce":"n","message_id":"m-ghost"}}"#.into(),
        ))
        .await
        .unwrap();
        let ack = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(Ok(Message::Text(text))) = a.next().await {
                if text.contains("mailbox_deposit_ack") && text.contains("m-ghost") {
                    return text;
                }
            }
            String::new()
        })
        .await
        .unwrap();
        assert!(ack.contains(r#""ok":false"#), "deposit to unknown device must be rejected, got: {ack}");
        assert!(ack.contains("not in room"), "got: {ack}");
    }

    /// A successful Join is answered by a `ServerHello` capability handshake
    /// carrying the relay's protocol version (>= 2 = mailbox deposit acks).
    #[tokio::test]
    async fn join_receives_server_hello() {
        use futures::{SinkExt, StreamExt};
        use tokio_tungstenite::connect_async;
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        use tokio_tungstenite::tungstenite::Message;

        std::env::set_var("JWT_SECRET", "test-secret");
        let state = Arc::new(AppState::new());
        seed_account(&state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app(state)).await.unwrap();
        });
        tokio::time::sleep(Duration::from_millis(100)).await;

        let token_a = make_token("user-1", "device-a");
        let mut request = format!("ws://127.0.0.1:{port}/v1/signaling")
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            axum::http::header::AUTHORIZATION,
            axum::http::HeaderValue::from_str(&format!("Bearer {token_a}")).unwrap(),
        );
        let (mut a, _) = connect_async(request).await.unwrap();

        a.send(Message::Text(
            r#"{"type":"join","payload":{"room_id":"user-1"}}"#.into(),
        ))
        .await
        .unwrap();

        let hello = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(Ok(Message::Text(text))) = a.next().await {
                if text.contains("server_hello") {
                    return text;
                }
            }
            String::new()
        })
        .await
        .unwrap();
        assert!(
            hello.contains(r#""protocol":2"#),
            "join must be answered by server_hello with protocol 2, got: {hello}"
        );
    }

    /// An empty mailbox must still be answered: the Join-time catch-up and an
    /// explicit MailboxPoll both send a `MailboxBatch` with `messages: []`, so
    /// the client can tell "no new messages" apart from "no answer yet".
    #[tokio::test]
    async fn empty_mailbox_poll_returns_empty_batch() {
        use futures::{SinkExt, StreamExt};
        use tokio_tungstenite::connect_async;
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        use tokio_tungstenite::tungstenite::Message;

        std::env::set_var("JWT_SECRET", "test-secret");
        let state = Arc::new(AppState::new());
        seed_account(&state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app(state)).await.unwrap();
        });
        tokio::time::sleep(Duration::from_millis(100)).await;

        let token_a = make_token("user-1", "device-a");
        let mut request = format!("ws://127.0.0.1:{port}/v1/signaling")
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            axum::http::header::AUTHORIZATION,
            axum::http::HeaderValue::from_str(&format!("Bearer {token_a}")).unwrap(),
        );
        let (mut a, _) = connect_async(request).await.unwrap();

        a.send(Message::Text(
            r#"{"type":"join","payload":{"room_id":"user-1"}}"#.into(),
        ))
        .await
        .unwrap();

        // First connection with an empty mailbox: the Join-time catch-up is an
        // empty batch, not silence.
        let join_batch = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(Ok(Message::Text(text))) = a.next().await {
                if text.contains("mailbox_batch") {
                    return text;
                }
            }
            String::new()
        })
        .await
        .unwrap();
        assert!(
            join_batch.contains(r#""messages":[]"#),
            "join with an empty mailbox must deliver an empty batch, got: {join_batch}"
        );

        // An explicit poll of the still-empty mailbox is answered too.
        a.send(Message::Text(
            r#"{"type":"mailbox_poll","payload":{"max_count":100}}"#.into(),
        ))
        .await
        .unwrap();
        let poll_batch = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(Ok(Message::Text(text))) = a.next().await {
                if text.contains("mailbox_batch") {
                    return text;
                }
            }
            String::new()
        })
        .await
        .unwrap();
        assert!(
            poll_batch.contains(r#""messages":[]"#),
            "empty mailbox poll must return an empty batch, got: {poll_batch}"
        );
    }

    // ── Storage quota / order API ──────────────────────────────────────────

    /// Minimal HTTP client over the in-process router (no TCP listener).
    async fn http_json(
        state: Arc<AppState>,
        method: &str,
        uri: &str,
        token: Option<&str>,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, serde_json::Value) {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt as _;
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(t) = token {
            builder = builder.header("Authorization", format!("Bearer {t}"));
        }
        let req = match body {
            Some(b) => builder
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_vec(&b).unwrap()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        let resp = app(state).oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            // Error responses are plain text (StatusCode, String); keep them
            // as a string so status assertions can still run.
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&bytes).into_owned()))
        };
        (status, json)
    }

    #[tokio::test]
    async fn storage_endpoint_reports_usage_and_default_quota() {
        std::env::set_var("JWT_SECRET", "test-secret");
        let state = Arc::new(AppState::new());
        seed_account(&state);
        let token = state.auth.issue_device_token("user-1", "device-a").unwrap();

        // 5 (ciphertext) + 1 (nonce) bytes in the account archive.
        state
            .mailboxes
            .deposit(
                "user-1",
                "device-a",
                mailbox::ACCOUNT_LEVEL_TARGET,
                "aaaaa".into(),
                "n".into(),
                None,
                None,
                1 << 30,
                None,
            )
            .unwrap();

        let (status, json) = http_json(state.clone(), "GET", "/api/storage", Some(&token), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["used_bytes"], 6);
        assert_eq!(json["quota_bytes"], 1i64 << 30);
        assert_eq!(json["plan_id"], "free");
        assert!(json["expires_at"].is_null());

        // The plan table is served to logged-in users.
        let (status, json) = http_json(state.clone(), "GET", "/api/plans", Some(&token), None).await;
        assert_eq!(status, StatusCode::OK);
        let plans = json.as_array().unwrap();
        assert_eq!(plans.len(), 4);
        assert!(plans
            .iter()
            .any(|p| p["id"] == "plus" && p["quota_bytes"] == 10i64 << 30));

        // Unauthenticated requests are rejected.
        let (status, _) = http_json(state.clone(), "GET", "/api/storage", None, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn bearer_endpoints_reject_removed_devices_and_tokens_are_long_lived() {
        std::env::set_var("JWT_SECRET", "test-secret");
        let state = Arc::new(AppState::new());
        seed_account(&state);
        let token = state.auth.issue_device_token("user-1", "device-a").unwrap();

        // Zotero-style: the token itself is effectively permanent (~100y)…
        let claims = decode_token(&token).unwrap();
        assert!(
            claims.exp as i64 - chrono::Utc::now().timestamp() > 10 * 365 * 24 * 3600,
            "device token should be long-lived; revocation, not expiry, is the control"
        );
        // …and a registered device's token works on HTTP APIs.
        let (status, _) = http_json(state.clone(), "GET", "/api/devices", Some(&token), None).await;
        assert_eq!(status, StatusCode::OK);

        // Once the device is removed, the still-valid token must stop working
        // everywhere, not just on the websocket connect path.
        assert!(state.db.delete_device("user-1", "device-a"));
        let (status, _) = http_json(state.clone(), "GET", "/api/devices", Some(&token), None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = http_json(state.clone(), "GET", "/api/storage", Some(&token), None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn storage_order_creation_is_idempotent() {
        std::env::set_var("JWT_SECRET", "test-secret");
        let mut state = AppState::new();
        state.payment_info = "pay to alipay 158xxxx, note order id".to_string();
        let state = Arc::new(state);
        seed_account(&state);
        let token = state.auth.issue_device_token("user-1", "device-a").unwrap();

        let body = serde_json::json!({"plan_id": "plus", "period": "month"});
        let (status, first) =
            http_json(state.clone(), "POST", "/api/storage/orders", Some(&token), Some(body.clone())).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(first["amount_cny"], 6.0);
        assert_eq!(first["status"], "pending");
        assert_eq!(first["duration_days"], 30);
        assert_eq!(first["payment_info"], "pay to alipay 158xxxx, note order id");

        // A second application returns the same pending order, not a new one.
        let (status, second) =
            http_json(state.clone(), "POST", "/api/storage/orders", Some(&token), Some(body)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(second["order_id"], first["order_id"]);

        // The user sees exactly one order; a pending order carries the
        // payment instructions so the client can re-show them at any time.
        let (status, orders) =
            http_json(state.clone(), "GET", "/api/storage/orders", Some(&token), None).await;
        assert_eq!(status, StatusCode::OK);
        let orders = orders.as_array().unwrap();
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0]["id"], first["order_id"]);
        assert_eq!(orders[0]["payment_info"], "pay to alipay 158xxxx, note order id");

        // Applying for a DIFFERENT plan/period cancels the stale pending
        // order and returns a fresh one reflecting the new selection.
        let (status, third) = http_json(
            state.clone(),
            "POST",
            "/api/storage/orders",
            Some(&token),
            Some(serde_json::json!({"plan_id": "pro", "period": "year"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_ne!(third["order_id"], first["order_id"]);
        assert_eq!(third["plan_id"], "pro");
        assert_eq!(third["amount_cny"], 150.0);
        let (_, orders) =
            http_json(state.clone(), "GET", "/api/storage/orders", Some(&token), None).await;
        let orders = orders.as_array().unwrap();
        assert_eq!(orders.len(), 2);
        let old = orders.iter().find(|o| o["id"] == first["order_id"]).unwrap();
        assert_eq!(old["status"], "cancelled");
        // Non-pending orders never expose the payment instructions.
        assert_eq!(old["payment_info"], "");
        let current = orders.iter().find(|o| o["id"] == third["order_id"]).unwrap();
        assert_eq!(current["payment_info"], "pay to alipay 158xxxx, note order id");

        // Unknown plan / bad period are rejected.
        let (status, _) = http_json(
            state.clone(),
            "POST",
            "/api/storage/orders",
            Some(&token),
            Some(serde_json::json!({"plan_id": "nope", "period": "month"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = http_json(
            state.clone(),
            "POST",
            "/api/storage/orders",
            Some(&token),
            Some(serde_json::json!({"plan_id": "pro", "period": "decade"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn admin_confirm_activates_quota_stacks_renewals_and_expiry_falls_back() {
        std::env::set_var("JWT_SECRET", "test-secret");
        let mut state = AppState::new();
        state.admin_token = Some("test-admin-token".to_string());
        state.payment_info = "pay to alipay 158xxxx, note order id".to_string();
        let state = Arc::new(state);
        seed_account(&state);
        let token = state.auth.issue_device_token("user-1", "device-a").unwrap();
        let order_body = serde_json::json!({"plan_id": "plus", "period": "month"});

        let (status, order) =
            http_json(state.clone(), "POST", "/api/storage/orders", Some(&token), Some(order_body.clone())).await;
        assert_eq!(status, StatusCode::OK);
        let order_id = order["order_id"].as_str().unwrap().to_string();

        // The admin sees the pending order.
        let (status, orders) = http_json(
            state.clone(),
            "GET",
            "/api/admin/orders?status=pending",
            Some("test-admin-token"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(orders.as_array().unwrap().len(), 1);

        // Confirm activates the quota with an expiry ~30 days out.
        let (status, confirmed) = http_json(
            state.clone(),
            "POST",
            &format!("/api/admin/orders/{order_id}/confirm"),
            Some("test-admin-token"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(confirmed["status"], "paid");
        let (status, storage) = http_json(state.clone(), "GET", "/api/storage", Some(&token), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(storage["quota_bytes"], 10i64 << 30);
        assert_eq!(storage["plan_id"], "plus");
        let expiry = chrono::DateTime::parse_from_rfc3339(storage["expires_at"].as_str().unwrap()).unwrap();
        let days = (expiry.timestamp() - chrono::Utc::now().timestamp()) / 86400;
        assert!((29..=30).contains(&days), "expiry should be ~30 days out, got {days}");

        // Once paid, the order no longer exposes payment instructions.
        let (status, orders) =
            http_json(state.clone(), "GET", "/api/storage/orders", Some(&token), None).await;
        assert_eq!(status, StatusCode::OK);
        let orders = orders.as_array().unwrap();
        let paid = orders.iter().find(|o| o["id"] == order_id).unwrap();
        assert_eq!(paid["status"], "paid");
        assert_eq!(paid["payment_info"], "");

        // Re-confirming a paid order is a conflict.
        let (status, _) = http_json(
            state.clone(),
            "POST",
            &format!("/api/admin/orders/{order_id}/confirm"),
            Some("test-admin-token"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);

        // Renewing while the subscription is live stacks onto the old expiry.
        let (_, order2) =
            http_json(state.clone(), "POST", "/api/storage/orders", Some(&token), Some(order_body)).await;
        let order2_id = order2["order_id"].as_str().unwrap().to_string();
        assert_ne!(order2_id, order_id);
        let (status, _) = http_json(
            state.clone(),
            "POST",
            &format!("/api/admin/orders/{order2_id}/confirm"),
            Some("test-admin-token"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (_, storage2) = http_json(state.clone(), "GET", "/api/storage", Some(&token), None).await;
        let expiry2 =
            chrono::DateTime::parse_from_rfc3339(storage2["expires_at"].as_str().unwrap()).unwrap();
        let stacked = (expiry2.timestamp() - expiry.timestamp()) / 86400;
        assert!((29..=30).contains(&stacked), "renewal must stack ~30 days, got {stacked}");

        // Once the subscription lapses the effective quota falls back to the
        // default (existing data is kept; only new writes are rejected).
        state
            .db
            .set_user_quota("user-1", Some(10i64 << 30), Some("2020-01-01T00:00:00Z".to_string()))
            .unwrap();
        let (status, storage3) = http_json(state.clone(), "GET", "/api/storage", Some(&token), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(storage3["quota_bytes"], 1i64 << 30);
        assert_eq!(storage3["plan_id"], "free");
    }

    #[tokio::test]
    async fn admin_endpoints_require_configured_token() {
        std::env::set_var("JWT_SECRET", "test-secret");
        // No RELAY_ADMIN_TOKEN configured → admin routes are hidden (404).
        let state = Arc::new(AppState::new());
        let (status, _) = http_json(state.clone(), "GET", "/api/admin/users", Some("anything"), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // Configured: missing or wrong bearer → 401; correct → 200.
        let mut state = AppState::new();
        state.admin_token = Some("test-admin-token".to_string());
        let state = Arc::new(state);
        let (status, _) = http_json(state.clone(), "GET", "/api/admin/users", None, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = http_json(state.clone(), "GET", "/api/admin/users", Some("wrong"), None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, users) =
            http_json(state.clone(), "GET", "/api/admin/users", Some("test-admin-token"), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(users.as_array().unwrap().len(), 0);
    }

    // ── 支付宝在线支付 ─────────────────────────────────────────────────────

    /// POST form-urlencoded via the in-process router; returns status + body text.
    async fn http_form(
        state: Arc<AppState>,
        uri: &str,
        params: &std::collections::BTreeMap<String, String>,
    ) -> (StatusCode, String) {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt as _;
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(params.iter())
            .finish();
        let req = Request::builder()
            .method("POST")
            .uri(uri)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap();
        let resp = app(state).oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    /// GET 纯文本/HTML via the in-process router.
    async fn http_get_text(state: Arc<AppState>, uri: &str) -> (StatusCode, String) {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt as _;
        let req = Request::builder().method("GET").uri(uri).body(Body::empty()).unwrap();
        let resp = app(state).oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    /// 测试用配置：商户密钥对（下单签名）与"支付宝"密钥对（回调验签）分开生成，
    /// 贴近真实双方各持一把私钥的模型。返回 (relay 侧配置, 模拟支付宝持有的私钥)。
    fn test_pay() -> (pay::AlipayConfig, rsa::RsaPrivateKey) {
        let mut rng = rand::thread_rng();
        let merchant_priv = rsa::RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let alipay_priv = rsa::RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let cfg = pay::AlipayConfig {
            app_id: "test-app-id".to_string(),
            private_key: merchant_priv,
            alipay_public_key: rsa::RsaPublicKey::from(&alipay_priv),
            gateway: "https://openapi.alipay.com/gateway.do".to_string(),
        };
        (cfg, alipay_priv)
    }

    fn alipay_state(cfg: pay::AlipayConfig) -> Arc<AppState> {
        let mut state = AppState::new();
        state.alipay = Some(cfg);
        state.alipay_public_base = Some("https://relay.example.com".to_string());
        Arc::new(state)
    }

    /// 构造并签名一份 notify 表单（用模拟支付宝的私钥签）。
    fn signed_notify(
        alipay_private_key: &rsa::RsaPrivateKey,
        out_trade_no: &str,
        total_amount: &str,
        app_id: &str,
    ) -> std::collections::BTreeMap<String, String> {
        let mut params = std::collections::BTreeMap::new();
        params.insert("app_id".to_string(), app_id.to_string());
        params.insert("out_trade_no".to_string(), out_trade_no.to_string());
        params.insert("total_amount".to_string(), total_amount.to_string());
        params.insert("trade_no".to_string(), "2026102000123456".to_string());
        params.insert("trade_status".to_string(), "TRADE_SUCCESS".to_string());
        params.insert("sign_type".to_string(), "RSA2".to_string());
        params.insert("gmt_payment".to_string(), "2026-10-20 12:00:00".to_string());
        let sign = pay::rsa2_sign(&params, alipay_private_key);
        params.insert("sign".to_string(), sign);
        params
    }

    #[test]
    fn rsa2_sign_verify_roundtrip() {
        let mut rng = rand::thread_rng();
        let priv_key = rsa::RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let pub_key = rsa::RsaPublicKey::from(&priv_key);
        let mut params = std::collections::BTreeMap::new();
        params.insert("b".to_string(), "2".to_string());
        params.insert("a".to_string(), "值一".to_string());
        params.insert("empty".to_string(), String::new());
        params.insert("sign".to_string(), "should-be-excluded".to_string());
        let sign = pay::rsa2_sign(&params, &priv_key);
        assert!(pay::rsa2_verify(&params, &sign, &pub_key));
        // 篡改任一参数后验签必须失败。
        let mut tampered = params.clone();
        tampered.insert("a".to_string(), "值二".to_string());
        assert!(!pay::rsa2_verify(&tampered, &sign, &pub_key));
        // 换一把公钥验签也必须失败。
        let other = rsa::RsaPrivateKey::new(&mut rng, 2048).unwrap();
        assert!(!pay::rsa2_verify(&params, &sign, &rsa::RsaPublicKey::from(&other)));
    }

    #[test]
    fn private_key_accepts_pem_and_bare_base64_pkcs8_and_pkcs1() {
        use rsa::pkcs1::EncodeRsaPrivateKey;
        use rsa::pkcs8::EncodePrivateKey;
        use rsa::pkcs8::LineEnding;
        let mut rng = rand::thread_rng();
        let key = rsa::RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let pkcs8_pem = key.to_pkcs8_pem(LineEnding::LF).unwrap();
        assert!(pay::parse_private_key(&pkcs8_pem).is_ok());
        let pkcs1_pem = key.to_pkcs1_pem(LineEnding::LF).unwrap();
        assert!(pay::parse_private_key(&pkcs1_pem).is_ok());
        // 去头尾的裸 base64（支付宝密钥工具复制的常见形态）。
        let bare: String = pkcs8_pem
            .lines()
            .filter(|l| !l.contains("BEGIN") && !l.contains("END"))
            .collect();
        assert!(pay::parse_private_key(&bare).is_ok());
        let bare_pkcs1: String = pkcs1_pem
            .lines()
            .filter(|l| !l.contains("BEGIN") && !l.contains("END"))
            .collect();
        assert!(pay::parse_private_key(&bare_pkcs1).is_ok());
        assert!(pay::parse_private_key("not-a-key").is_err());
    }

    #[tokio::test]
    async fn alipay_notify_happy_path_and_repeat_is_idempotent() {
        std::env::set_var("JWT_SECRET", "test-secret");
        let (cfg, alipay_priv) = test_pay();
        let state = alipay_state(cfg);
        seed_account(&state);
        let token = state.auth.issue_device_token("user-1", "device-a").unwrap();

        // 下单（channel=alipay）→ 响应带 pay_url。
        let (status, order) = http_json(
            state.clone(),
            "POST",
            "/api/storage/orders",
            Some(&token),
            Some(serde_json::json!({"plan_id": "plus", "period": "month", "channel": "alipay"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let order_id = order["order_id"].as_str().unwrap().to_string();
        assert_eq!(
            order["pay_url"].as_str().unwrap(),
            format!("https://relay.example.com/pay/{order_id}")
        );

        // 模拟支付宝回调 → success，配额激活。
        let params = signed_notify(&alipay_priv, &order_id, "6.00", "test-app-id");
        let (status, body) = http_form(state.clone(), "/api/pay/alipay/notify", &params).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "success");
        let paid = state.db.get_order(&order_id).unwrap();
        assert_eq!(paid.status, db::OrderStatus::Paid);
        assert_eq!(paid.trade_no.as_deref(), Some("2026102000123456"));
        let (quota, expires_at) = state.db.get_user_quota("user-1").unwrap();
        assert_eq!(quota, Some(10i64 << 30));
        let first_expiry = expires_at.unwrap();

        // 重复回调幂等：仍 success，配额不重复叠加（到期时间不变）。
        let (status, body) = http_form(state.clone(), "/api/pay/alipay/notify", &params).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "success");
        let (_, expires_at2) = state.db.get_user_quota("user-1").unwrap();
        assert_eq!(expires_at2, Some(first_expiry));
    }

    #[tokio::test]
    async fn alipay_notify_rejects_tampered_requests() {
        std::env::set_var("JWT_SECRET", "test-secret");
        let (cfg, alipay_priv) = test_pay();
        let merchant_priv = cfg.private_key.clone();
        let state = alipay_state(cfg);
        seed_account(&state);
        let token = state.auth.issue_device_token("user-1", "device-a").unwrap();
        let (_, order) = http_json(
            state.clone(),
            "POST",
            "/api/storage/orders",
            Some(&token),
            Some(serde_json::json!({"plan_id": "plus", "period": "month", "channel": "alipay"})),
        )
        .await;
        let order_id = order["order_id"].as_str().unwrap().to_string();

        // 1) 错签名（用商户私钥而非"支付宝"私钥签）→ fail。
        let mut params = std::collections::BTreeMap::new();
        params.insert("app_id".to_string(), "test-app-id".to_string());
        params.insert("out_trade_no".to_string(), order_id.clone());
        params.insert("total_amount".to_string(), "6.00".to_string());
        params.insert("trade_status".to_string(), "TRADE_SUCCESS".to_string());
        params.insert("sign_type".to_string(), "RSA2".to_string());
        let bad_sign = pay::rsa2_sign(&params, &merchant_priv);
        params.insert("sign".to_string(), bad_sign);
        let (status, body) = http_form(state.clone(), "/api/pay/alipay/notify", &params).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body, "fail");

        // 2) 签名正确但金额不对 → fail，订单保持 pending。
        let params = signed_notify(&alipay_priv, &order_id, "5.99", "test-app-id");
        let (status, body) = http_form(state.clone(), "/api/pay/alipay/notify", &params).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body, "fail");
        assert_eq!(state.db.get_order(&order_id).unwrap().status, db::OrderStatus::Pending);

        // 3) 签名正确但 app_id 不匹配 → fail。
        let params = signed_notify(&alipay_priv, &order_id, "6.00", "another-app");
        let (status, body) = http_form(state.clone(), "/api/pay/alipay/notify", &params).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body, "fail");
        assert_eq!(state.db.get_order(&order_id).unwrap().status, db::OrderStatus::Pending);

        // 4) 未知订单号 → fail。
        let params = signed_notify(&alipay_priv, "no-such-order", "6.00", "test-app-id");
        let (status, body) = http_form(state.clone(), "/api/pay/alipay/notify", &params).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body, "fail");
    }

    /// 资金安全：已取消的订单收到回调也必须履约（用户已付款），置回 Paid 并
    /// 记 admin_note。
    #[tokio::test]
    async fn alipay_notify_fulfills_cancelled_order() {
        std::env::set_var("JWT_SECRET", "test-secret");
        let (cfg, alipay_priv) = test_pay();
        let state = alipay_state(cfg);
        seed_account(&state);
        let token = state.auth.issue_device_token("user-1", "device-a").unwrap();
        let (_, order) = http_json(
            state.clone(),
            "POST",
            "/api/storage/orders",
            Some(&token),
            Some(serde_json::json!({"plan_id": "pro", "period": "year", "channel": "alipay"})),
        )
        .await;
        let order_id = order["order_id"].as_str().unwrap().to_string();
        state
            .db
            .update_order_status(
                &order_id,
                db::OrderStatus::Cancelled,
                None,
                Some("支付超时自动关闭".to_string()),
            )
            .unwrap();

        let params = signed_notify(&alipay_priv, &order_id, "150.00", "test-app-id");
        let (status, body) = http_form(state.clone(), "/api/pay/alipay/notify", &params).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "success");
        let order = state.db.get_order(&order_id).unwrap();
        assert_eq!(order.status, db::OrderStatus::Paid);
        assert_eq!(order.admin_note.as_deref(), Some("逾期支付自动履约"));
        let (quota, _) = state.db.get_user_quota("user-1").unwrap();
        assert_eq!(quota, Some(50i64 << 30));
    }

    /// 支付页三种状态渲染 + 未知订单 404 + 无鉴权状态轮询。
    #[tokio::test]
    async fn pay_page_renders_each_order_state() {
        std::env::set_var("JWT_SECRET", "test-secret");
        let (cfg, _alipay_priv) = test_pay();
        let state = alipay_state(cfg);
        seed_account(&state);
        let token = state.auth.issue_device_token("user-1", "device-a").unwrap();
        let (_, order) = http_json(
            state.clone(),
            "POST",
            "/api/storage/orders",
            Some(&token),
            Some(serde_json::json!({"plan_id": "plus", "period": "month", "channel": "alipay"})),
        )
        .await;
        let order_id = order["order_id"].as_str().unwrap().to_string();

        // pending：自动 GET 跳收银台链接；账号打码。
        let (status, html) = http_get_text(state.clone(), &format!("/pay/{order_id}")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains(r#"id="go" class="btn" href="https://openapi.alipay.com/gateway.do?app_id="#));
        assert!(html.contains("alipay.trade.page.pay"));
        assert!(html.contains("FAST_INSTANT_TRADE_PAY"));
        assert!(html.contains("t***@example.com"));
        assert!(!html.contains("test@example.com"));

        // 从支付宝 return_url 回来（带 out_trade_no 查询参数）时订单可能还是
        // pending：必须渲染确认页轮询，而不是再次跳收银台。
        let (status, html) =
            http_get_text(state.clone(), &format!("/pay/{order_id}?out_trade_no={order_id}&trade_no=t1")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("支付结果确认中"));
        assert!(html.contains(&format!("/api/storage/orders/{order_id}/status")));

        // 无鉴权的状态接口只返回 status；本人带 token 返回完整订单行。
        let (status, json) = http_json(
            state.clone(),
            "GET",
            &format!("/api/storage/orders/{order_id}/status"),
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json, serde_json::json!({"status": "pending"}));
        let (status, json) = http_json(
            state.clone(),
            "GET",
            &format!("/api/storage/orders/{order_id}/status"),
            Some(&token),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["id"], order_id);
        assert!(json["pay_url"].is_string());

        // paid：成功页。
        state
            .db
            .activate_order(&order_id, None, None)
            .unwrap();
        let (status, html) = http_get_text(state.clone(), &format!("/pay/{order_id}")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("支付成功"));

        // cancelled：已关闭页。
        state
            .db
            .update_order_status(&order_id, db::OrderStatus::Cancelled, None, None)
            .unwrap();
        let (status, html) = http_get_text(state.clone(), &format!("/pay/{order_id}")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("订单已关闭"));

        // 未知订单 → 404。
        let (status, _) = http_get_text(state.clone(), "/pay/no-such-order").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    /// pending 超过 30 分钟的订单访问支付页时惰性置为 Cancelled 并提示重新下单。
    #[tokio::test]
    async fn pay_page_lazily_cancels_expired_pending_order() {
        std::env::set_var("JWT_SECRET", "test-secret");
        let (cfg, _alipay_priv) = test_pay();
        let state = alipay_state(cfg);
        seed_account(&state);
        let old = (chrono::Utc::now() - chrono::Duration::minutes(31))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let order = db::Order {
            id: uuid::Uuid::new_v4().to_string(),
            user_id: "user-1".to_string(),
            plan_id: "plus".to_string(),
            quota_bytes: 10i64 << 30,
            duration_days: 30,
            amount_cny: 6.0,
            status: db::OrderStatus::Pending,
            created_at: old,
            paid_at: None,
            admin_note: None,
            channel: Some("alipay".to_string()),
            trade_no: None,
        };
        let order_id = order.id.clone();
        state.db.create_order(order).unwrap();

        let (status, html) = http_get_text(state.clone(), &format!("/pay/{order_id}")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("订单已超时关闭"));
        assert_eq!(
            state.db.get_order(&order_id).unwrap().status,
            db::OrderStatus::Cancelled
        );
    }

    /// 未配置支付宝时：下单不返回 pay_url，行为与纯线下流程一致。
    #[tokio::test]
    async fn create_order_without_alipay_config_has_no_pay_url() {
        std::env::set_var("JWT_SECRET", "test-secret");
        let state = Arc::new(AppState::new());
        seed_account(&state);
        let token = state.auth.issue_device_token("user-1", "device-a").unwrap();
        let (status, order) = http_json(
            state.clone(),
            "POST",
            "/api/storage/orders",
            Some(&token),
            Some(serde_json::json!({"plan_id": "plus", "period": "month", "channel": "alipay"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(order["pay_url"].is_null());
        // 支付页仍可用，提示走线下转账。
        let order_id = order["order_id"].as_str().unwrap();
        let (status, html) = http_get_text(state.clone(), &format!("/pay/{order_id}")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("线下转账"));
    }

    /// 回归：alipay.trade.page.pay 是页面跳转类请求，sign_type 必须参与签名
    ///（网关验签字符串包含 sign_type，与 API 调用/异步通知的规则不同）。
    /// 2026-10-20 生产实测：排除 sign_type 被网关判 invalid-signature。
    #[test]
    fn page_pay_sign_includes_sign_type() {
        let (cfg, _alipay_priv) = test_pay();
        let params = pay::page_pay_params(
            "order-1",
            "plus",
            6.0,
            &cfg,
            "https://relay.seekpath.com.cn",
        );
        let sign = params.get("sign").unwrap();
        let merchant_pub = rsa::RsaPublicKey::from(&cfg.private_key);
        // 含 sign_type 的验签串必须验证通过。
        let mut with_sign_type = params.clone();
        with_sign_type.remove("sign");
        assert!(
            pay::rsa2_verify_page_redirect(&with_sign_type, sign, &merchant_pub),
            "page.pay sign must verify over the canonical string INCLUDING sign_type"
        );
        // 而 API 规则（排除 sign_type）下必须验证失败，防止回退。
        assert!(
            !pay::rsa2_verify(&with_sign_type, sign, &merchant_pub),
            "page.pay sign must NOT verify when sign_type is excluded"
        );
    }

    /// 幂等标配：同一订单并发/重复履约只有第一次生效——第二次得到    /// AlreadyPaid，配额到期时间不再叠加（回调重发 + 管理员同时确认的场景）。
    #[tokio::test]
    async fn double_fulfillment_is_idempotent() {
        std::env::set_var("JWT_SECRET", "test-secret");
        let state = Arc::new(AppState::new());
        seed_account(&state);
        let order = db::Order {
            id: uuid::Uuid::new_v4().to_string(),
            user_id: "user-1".to_string(),
            plan_id: "plus".to_string(),
            quota_bytes: 10i64 << 30,
            duration_days: 30,
            amount_cny: 6.0,
            status: db::OrderStatus::Pending,
            created_at: now_iso(),
            paid_at: None,
            admin_note: None,
            channel: Some("alipay".to_string()),
            trade_no: None,
        };
        let order_id = order.id.clone();
        state.db.create_order(order).unwrap();

        let first = activate_order(&state, &order_id, Some("t1".to_string()), None).unwrap();
        assert!(matches!(first, db::Activation::Fulfilled(_)));
        let expiry1 = state.db.get_user_quota("user-1").unwrap().1.unwrap();

        // 第二次履约（重复回调）：AlreadyPaid，到期时间与交易号都不变。
        let second = activate_order(&state, &order_id, Some("t2".to_string()), None).unwrap();
        assert!(matches!(second, db::Activation::AlreadyPaid(_)));
        let expiry2 = state.db.get_user_quota("user-1").unwrap().1.unwrap();
        assert_eq!(expiry1, expiry2);
        assert_eq!(
            state.db.get_order(&order_id).unwrap().trade_no.as_deref(),
            Some("t1")
        );
    }
}
