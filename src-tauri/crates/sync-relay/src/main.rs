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
fn send_mailbox_batches(tx: &mpsc::UnboundedSender<RelayServerMsg>, messages: Vec<MailboxMessage>) {
    for batch in chunk_mailbox_messages(messages, MAILBOX_BATCH_MAX_BYTES) {
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
        Self {
            rooms: Mutex::new(HashMap::new()),
            db: db::Db::new(std::path::Path::new(&db_path)).expect("account db"),
            auth: auth::Auth::new(jwt_secret()),
            mailboxes: mailbox::Mailbox::open(std::path::Path::new(&mailbox_db_path))
                .expect("mailbox db"),
            admin_token: admin_token(),
            default_quota: default_quota_bytes(),
            payment_info: payment_info(),
        }
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
                            if !pending.is_empty() {
                                send_mailbox_batches(&tx, pending);
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
    Ok(claims)
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
    // Idempotent: an existing pending order is returned as-is so repeated
    // applications never pile up for the admin to review.
    if let Some(existing) = state.db.pending_order_for_user(&claims.sub) {
        return Ok(axum::Json(order_response(&existing, &state)));
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
    };
    state
        .db
        .create_order(order.clone())
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    info!(user_id = %claims.sub, order_id = %order.id, plan = %plan.id, "storage order created");
    Ok(axum::Json(order_response(&order, &state)))
}

async fn api_list_my_storage_orders(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> Result<axum::Json<Vec<db::Order>>, (StatusCode, String)> {
    let claims = bearer_claims(&state, &headers)?;
    let orders = state
        .db
        .list_orders(None)
        .into_iter()
        .filter(|o| o.user_id == claims.sub)
        .collect();
    Ok(axum::Json(orders))
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
    let now = chrono::Utc::now();
    let base = state
        .db
        .get_user_quota(&order.user_id)
        .and_then(|(_, exp)| exp)
        .and_then(|exp| chrono::DateTime::parse_from_rfc3339(&exp).ok())
        .map(|t| t.with_timezone(&chrono::Utc))
        .filter(|t| *t > now)
        .unwrap_or(now);
    let expires = base + chrono::Duration::days(order.duration_days as i64);
    let expires_at = expires.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    state
        .db
        .set_user_quota(&order.user_id, Some(order.quota_bytes), Some(expires_at))
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;
    let order = state
        .db
        .update_order_status(&id, db::OrderStatus::Paid, Some(now_iso()), None)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    info!(order_id = %id, user_id = %order.user_id, quota_bytes = order.quota_bytes, "storage order confirmed");
    Ok(axum::Json(order))
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

        // The user sees exactly one order.
        let (status, orders) =
            http_json(state.clone(), "GET", "/api/storage/orders", Some(&token), None).await;
        assert_eq!(status, StatusCode::OK);
        let orders = orders.as_array().unwrap();
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0]["id"], first["order_id"]);

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
}
