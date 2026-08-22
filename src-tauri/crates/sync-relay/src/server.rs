use crate::auth::{Auth, Claims};
use crate::room::{Device, DeviceTx, RoomManager};
use crate::types::{ClientMessage, ErrorPayload, JoinPayload, ServerMessage};
use axum::{
    extract::{Query, State, WebSocketUpgrade},
    response::IntoResponse,
    routing::get,
    Router,
};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

#[derive(Clone)]
pub struct AppState {
    pub rooms: RoomManager,
    pub auth: Auth,
    pub config: Arc<ServerConfig>,
}

#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub heartbeat_interval: Duration,
    #[allow(dead_code)]
    pub heartbeat_timeout: Duration,
    pub max_relay_queue_per_device: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval: Duration::from_secs(30),
            heartbeat_timeout: Duration::from_secs(90),
            max_relay_queue_per_device: 100,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct SignalingParams {
    token: String,
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/v1/signaling", get(signaling_handler))
        .route("/health", get(health_handler))
        .with_state(state)
}

async fn health_handler() -> impl IntoResponse {
    "ok"
}

async fn signaling_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(params): Query<SignalingParams>,
) -> impl IntoResponse {
    let claims = match state.auth.validate(&params.token) {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "auth failed");
            return (axum::http::StatusCode::UNAUTHORIZED, "unauthorized").into_response();
        }
    };

    ws.on_upgrade(move |socket| handle_socket(socket, state, claims))
}

async fn handle_socket(
    socket: axum::extract::ws::WebSocket,
    state: AppState,
    claims: Claims,
) {
    let (mut sender, mut receiver) = socket.split();
    let (tx, mut rx): (DeviceTx, mpsc::UnboundedReceiver<ServerMessage>) = mpsc::unbounded_channel();

    let device_id = claims.device_id.clone();
    let user_id = claims.sub.clone();

    // Forward server messages to the WebSocket client.
    let forward_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let json = match serde_json::to_string(&msg) {
                Ok(j) => j,
                Err(e) => {
                    error!(error = %e, "failed to serialize server message");
                    continue;
                }
            };
            if sender.send(axum::extract::ws::Message::Text(json)).await.is_err() {
                break;
            }
        }
    });

    // Main receive loop.
    let mut heartbeat = tokio::time::interval_at(
        tokio::time::Instant::now() + state.config.heartbeat_interval,
        state.config.heartbeat_interval,
    );
    let mut joined_room: Option<String> = None;

    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                if joined_room.is_some() {
                    state.rooms.heartbeat(&device_id).await;
                }
                // Send a periodic ping to the client through the server→client channel.
                let _ = tx.send(ServerMessage::Ping);
            }
            Some(Ok(msg)) = receiver.next() => {
                match msg {
                    axum::extract::ws::Message::Text(text) => {
                        match serde_json::from_str::<ClientMessage>(&text) {
                            Ok(client_msg) => {
                                if let Err(e) = handle_client_message(
                                    &state,
                                    &claims,
                                    &mut joined_room,
                                    client_msg,
                                    &tx,
                                ).await {
                                    warn!(error = %e, "client message handling failed");
                                    let _ = tx.send(ServerMessage::Error {
                                        payload: ErrorPayload {
                                            code: "bad_request".to_string(),
                                            message: e.to_string(),
                                        },
                                    });
                                }
                            }
                            Err(e) => {
                                warn!(error = %e, text = %text, "invalid client message");
                                let _ = tx.send(ServerMessage::Error {
                                    payload: ErrorPayload {
                                        code: "invalid_message".to_string(),
                                        message: e.to_string(),
                                    },
                                });
                            }
                        }
                    }
                    axum::extract::ws::Message::Close(_) => break,
                    _ => {}
                }
            }
            else => break,
        }
    }

    // Cleanup on disconnect.
    if joined_room.is_some() {
        state.rooms.leave(&device_id).await;
    }
    forward_task.abort();
    info!(device = %device_id, user = %user_id, "connection closed");
}

async fn handle_client_message(
    state: &AppState,
    claims: &Claims,
    joined_room: &mut Option<String>,
    msg: ClientMessage,
    _tx: &DeviceTx,
) -> anyhow::Result<()> {
    match msg {
        ClientMessage::Join { payload: JoinPayload { room_id } } => {
            if joined_room.is_some() {
                anyhow::bail!("already joined a room");
            }
            let device = Device {
                device_id: claims.device_id.clone(),
                user_id: claims.sub.clone(),
                room_id: room_id.clone(),
                tx: _tx.clone(),
                last_seen: std::time::Instant::now(),
            };
            state.rooms.join(room_id.clone(), device).await;
            *joined_room = Some(room_id);
            Ok(())
        }
        ClientMessage::Signal { mut payload } => {
            if joined_room.is_none() {
                anyhow::bail!("not joined");
            }
            payload.from_device_id = Some(claims.device_id.clone());
            state.rooms.send_signal(payload).await
        }
        ClientMessage::Relay { mut payload } => {
            if joined_room.is_none() {
                anyhow::bail!("not joined");
            }
            payload.from_device_id = Some(claims.device_id.clone());
            state
                .rooms
                .send_relay(payload, state.config.max_relay_queue_per_device)
                .await
        }
        ClientMessage::Presence => {
            if joined_room.is_none() {
                anyhow::bail!("not joined");
            }
            // Heartbeat is already handled by the interval; explicit presence just refreshes.
            state.rooms.heartbeat(&claims.device_id).await;
            Ok(())
        }
        ClientMessage::Pong => {
            state.rooms.heartbeat(&claims.device_id).await;
            Ok(())
        }
    }
}
