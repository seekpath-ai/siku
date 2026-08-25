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

#[derive(Debug, Deserialize)]
struct TokenQuery {
    token: String,
}

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
}

impl AppState {
    fn new() -> Self {
        let db_path = std::env::var("RELAY_DB_PATH").unwrap_or_else(|_| ":memory:".to_string());
        Self {
            rooms: Mutex::new(HashMap::new()),
            db: db::Db::new(std::path::Path::new(&db_path)).expect("account db"),
            auth: auth::Auth::new(jwt_secret()),
            mailboxes: mailbox::Mailbox::new(),
        }
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

async fn signaling_handler(
    ws: WebSocketUpgrade,
    Query(query): Query<TokenQuery>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let claims = match decode_token(&query.token) {
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

                        if is_first_connection {
                            let pending = state
                                .mailboxes
                                .poll(&room_id, &join_device_id, Some(100));
                            if !pending.is_empty() {
                                let _ = tx.send(RelayServerMsg::MailboxBatch {
                                    payload: MailboxBatchPayload { messages: pending },
                                });
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
                        let Some(room) = joined_room.as_ref() else {
                            let _ = tx.send(RelayServerMsg::Error {
                                payload: ErrorPayload {
                                    code: "not_joined".to_string(),
                                    message: "send join before mailbox deposit".to_string(),
                                },
                            });
                            continue;
                        };
                        match state.mailboxes.deposit(
                            room,
                            &device_id,
                            &payload.to_device_id,
                            payload.ciphertext,
                            payload.nonce,
                            payload.ttl_seconds,
                        ) {
                            Ok(()) => info!(
                                from = %device_id,
                                to = %payload.to_device_id,
                                "mailbox deposit accepted"
                            ),
                            Err(e) => {
                                let _ = tx.send(RelayServerMsg::Error {
                                    payload: ErrorPayload {
                                        code: "bad_request".to_string(),
                                        message: e,
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
                        if !pending.is_empty() {
                            let _ = tx.send(RelayServerMsg::MailboxBatch {
                                payload: MailboxBatchPayload { messages: pending },
                            });
                        }
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
    use axum::routing::{patch, post};
    Router::new()
        .route("/v1/signaling", get(signaling_handler))
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/register", post(api_register))
        .route("/api/login", post(api_login))
        .route("/api/refresh", post(api_refresh))
        .route("/api/devices", get(api_list_devices))
        .route("/api/devices/:id", patch(api_rename_device).delete(api_remove_device))
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

    #[tokio::test]
    async fn two_peers_see_each_other() {
        use futures::{SinkExt, StreamExt};
        use tokio_tungstenite::{connect_async, tungstenite::Message};

        std::env::set_var("JWT_SECRET", "test-secret");
        let state = Arc::new(AppState::new());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app(state)).await.unwrap();
        });

        // Give the server a moment to start.
        tokio::time::sleep(Duration::from_millis(100)).await;

        let token_a = make_token("user-1", "device-a");
        let token_b = make_token("user-1", "device-b");

        let (mut a, _) = connect_async(format!("ws://127.0.0.1:{}/v1/signaling?token={}", port, token_a))
            .await
            .unwrap();
        let (mut b, _) = connect_async(format!("ws://127.0.0.1:{}/v1/signaling?token={}", port, token_b))
            .await
            .unwrap();

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
}
