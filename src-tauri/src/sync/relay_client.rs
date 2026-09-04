use crate::sync::types::{
    MailboxDepositAckPayload, MailboxDepositPayload, RelayClientMsg, RelayServerMsg,
};
use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{info, warn};

/// Outcome of a `MailboxDeposit` sent through [`RelayClient::deposit_await_ack`].
#[derive(Debug, Clone)]
pub enum AckError {
    /// The message could not be handed to the WebSocket at all. Also used for
    /// the fail-fast rejection when the relay is known to be a legacy build
    /// that never acks deposits ("relay too old: ...").
    SendFailed(String),
    /// The relay explicitly rejected the deposit (e.g. per-device target not
    /// in the room). The message was NOT stored — retry or queue it.
    Rejected(String),
    /// No ack arrived within the timeout. The deposit may or may not have been
    /// stored — treat as unconfirmed and re-queue (idempotent retry via the
    /// same message_id).
    TimedOut,
}

/// How long after sending `Join` we wait for the relay's `ServerHello`
/// capability handshake before concluding the relay is a legacy build
/// (protocol 1, no mailbox deposit acks).
const SERVER_HELLO_TIMEOUT: Duration = Duration::from_secs(2);

/// Floor for the deposit-ack wait, scaled by payload size. The relay can only
/// ack after it has received the WHOLE frame, and a large frame takes
/// non-negligible time just to cross the wire — production: a 13.4MB mailbox
/// deposit at ~4.5MB/s needs ~3s of pure transmission, so a fixed 3s timeout
/// aborted every large deposit and made the outbox retransmit the full payload
/// on every tick. Scales as `max(3s, 2s + ciphertext_bytes / 2MB/s)`, capped
/// at 60s.
fn payload_scaled_ack_timeout(ciphertext_b64_len: usize) -> Duration {
    // The payload ciphertext is base64 (4:3); the decoded byte count is what
    // costs bandwidth.
    let payload_bytes = (ciphertext_b64_len as u64).saturating_mul(3) / 4;
    let transfer_secs = payload_bytes / (2 * 1024 * 1024);
    Duration::from_secs(2)
        .saturating_add(Duration::from_secs(transfer_secs))
        .clamp(Duration::from_secs(3), Duration::from_secs(60))
}

/// Detected relay capability, learned from the `ServerHello` handshake (or
/// its absence within `SERVER_HELLO_TIMEOUT` of joining).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RelayProtocol {
    /// No Join sent yet on this connection.
    Unknown,
    /// Join sent; waiting for `ServerHello`.
    AwaitingHello,
    /// Handshake received with protocol >= 2: mailbox deposit acks work.
    V2OrLater,
    /// No handshake in time, or protocol < 2: a legacy relay that never acks
    /// deposits — `deposit_await_ack` fails fast instead of timing out.
    Legacy,
}

impl std::fmt::Display for AckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AckError::SendFailed(e) => write!(f, "mailbox deposit send failed: {e}"),
            AckError::Rejected(e) => write!(f, "mailbox deposit rejected: {e}"),
            AckError::TimedOut => write!(f, "mailbox deposit ack timed out"),
        }
    }
}

#[derive(Clone)]
pub struct RelayClient {
    tx: mpsc::UnboundedSender<RelayClientMsg>,
    rx: Arc<Mutex<mpsc::UnboundedReceiver<RelayServerMsg>>>,
    /// Background tasks driving the WebSocket; aborted by `shutdown()`.
    tasks: Arc<Vec<tokio::task::JoinHandle<()>>>,
    /// In-flight mailbox deposits awaiting their `MailboxDepositAck`, keyed by
    /// the client message_id. Completed (or timed-out) entries are removed.
    pending_acks: Arc<std::sync::Mutex<HashMap<String, oneshot::Sender<Result<(), String>>>>>,
    /// Relay capability learned from the `ServerHello` handshake.
    protocol: Arc<std::sync::Mutex<RelayProtocol>>,
}

/// Route one `MailboxDepositAck` to the waiting sender (if any). Acks with no
/// waiter (already timed out) are dropped.
fn route_ack(
    pending: &Arc<std::sync::Mutex<HashMap<String, oneshot::Sender<Result<(), String>>>>>,
    payload: MailboxDepositAckPayload,
) {
    if let Some(tx) = pending.lock().unwrap().remove(&payload.id) {
        let result = if payload.ok {
            Ok(())
        } else {
            Err(payload
                .error
                .unwrap_or_else(|| "mailbox deposit rejected by relay".to_string()))
        };
        let _ = tx.send(result);
    }
}

impl RelayClient {
    pub async fn connect(relay_url: &str, token: &str) -> Result<Self> {
        // The JWT goes in the `Authorization` header, NOT the URL query
        // string: query strings are logged by proxies and reverse proxies
        // (access logs, Caddy/nginx) and leak the credential. The relay still
        // accepts the legacy `?token=` form for older clients.
        let mut request = relay_url
            .to_string()
            .into_client_request()
            .context("build relay request")?;
        request.headers_mut().insert(
            http::header::AUTHORIZATION,
            http::HeaderValue::from_str(&format!("Bearer {token}"))
                .context("invalid token for header")?,
        );
        let (ws_stream, _) = connect_async(request)
            .await
            .context("connect to sync relay")?;
        let (mut ws_tx, mut ws_rx) = ws_stream.split();

        let (client_tx, mut client_rx) = mpsc::unbounded_channel::<RelayClientMsg>();
        let (server_tx, server_rx) = mpsc::unbounded_channel::<RelayServerMsg>();

        let forward_task = tokio::spawn(async move {
            while let Some(msg) = client_rx.recv().await {
                let json = serde_json::to_string(&msg).unwrap_or_default();
                if ws_tx.send(WsMessage::Text(json)).await.is_err() {
                    break;
                }
            }
        });

        let pong_tx = client_tx.clone();
        let pending = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let recv_pending = pending.clone();
        let protocol = Arc::new(std::sync::Mutex::new(RelayProtocol::Unknown));
        let recv_protocol = protocol.clone();
        let recv_task = tokio::spawn(async move {
            while let Some(Ok(ws_msg)) = ws_rx.next().await {
                if let WsMessage::Text(text) = ws_msg {
                    match serde_json::from_str::<RelayServerMsg>(&text) {
                        Ok(RelayServerMsg::Ping) => {
                            // Answer the relay's heartbeat so an idle
                            // connection is not dropped by its receive
                            // timeout (which would silently mark this device
                            // offline). Best-effort; routed through the same
                            // outgoing queue as every other client message.
                            let _ = pong_tx.send(RelayClientMsg::Pong);
                        }
                        Ok(RelayServerMsg::ServerHello { payload }) => {
                            // Capability handshake: record it and consume the
                            // message (other consumers would only warn about
                            // an unexpected type).
                            let mut proto = recv_protocol.lock().unwrap();
                            if payload.protocol >= 2 {
                                *proto = RelayProtocol::V2OrLater;
                            } else {
                                *proto = RelayProtocol::Legacy;
                                warn!(
                                    protocol = payload.protocol,
                                    "relay 版本过旧，mailbox 确认不可用，请升级 relay"
                                );
                            }
                        }
                        Ok(RelayServerMsg::MailboxDepositAck { payload }) => {
                            // Route to the deposit's waiter instead of the
                            // shared stream, so ack-awaiting senders never
                            // race other readers of `recv()`.
                            route_ack(&recv_pending, payload);
                        }
                        Ok(msg) => {
                            if server_tx.send(msg).is_err() {
                                break;
                            }
                        }
                        // Unknown message types from a NEWER relay land here
                        // and are ignored, keeping old clients forward-
                        // compatible.
                        Err(e) => warn!(error = %e, text = %text, "failed to parse relay msg"),
                    }
                }
            }
        });

        info!("connected to sync relay");
        Ok(Self {
            tx: client_tx,
            rx: Arc::new(Mutex::new(server_rx)),
            tasks: Arc::new(vec![forward_task, recv_task]),
            pending_acks: pending,
            protocol,
        })
    }

    pub fn send(&self, msg: RelayClientMsg) -> Result<()> {
        if matches!(msg, RelayClientMsg::Join { .. }) {
            self.start_hello_watchdog();
        }
        self.tx
            .send(msg)
            .map_err(|_| anyhow::anyhow!("relay send failed"))
    }

    /// After a Join, the relay answers with `ServerHello`. If none arrives
    /// within `SERVER_HELLO_TIMEOUT` the relay is a legacy build that never
    /// acks mailbox deposits — record that so `deposit_await_ack` can fail
    /// fast instead of burning its full timeout on every deposit.
    fn start_hello_watchdog(&self) {
        {
            let mut proto = self.protocol.lock().unwrap();
            if *proto != RelayProtocol::Unknown {
                return;
            }
            *proto = RelayProtocol::AwaitingHello;
        }
        let protocol = self.protocol.clone();
        tokio::spawn(async move {
            tokio::time::sleep(SERVER_HELLO_TIMEOUT).await;
            let mut proto = protocol.lock().unwrap();
            if *proto == RelayProtocol::AwaitingHello {
                *proto = RelayProtocol::Legacy;
                warn!("relay 版本过旧，mailbox 确认不可用，请升级 relay");
            }
        });
    }

    /// Send a mailbox deposit and wait for the relay's `MailboxDepositAck`.
    /// On success the message is durably stored; on rejection or timeout the
    /// caller must retry (same message_id = idempotent) or queue it locally.
    /// This is what lets the sync engine advance its cursor ONLY after a
    /// confirmed deposit.
    pub async fn deposit_await_ack(
        &self,
        mut payload: MailboxDepositPayload,
        timeout: Duration,
    ) -> std::result::Result<(), AckError> {
        // Legacy relay (no ServerHello, or protocol < 2): it never acks
        // deposits, so fail fast instead of blocking the full timeout on
        // every message. The caller's outbox fallback keeps the data safe.
        if *self.protocol.lock().unwrap() == RelayProtocol::Legacy {
            return Err(AckError::SendFailed(
                "relay too old: mailbox deposit acks unsupported, upgrade the relay".to_string(),
            ));
        }
        let message_id = payload
            .message_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        payload.message_id = Some(message_id.clone());

        // The caller-supplied timeout is only a floor: scale the wait with the
        // payload size so a multi-MB frame gets enough time to reach the relay
        // (which acks only after receiving the whole frame) before the deposit
        // is declared unconfirmed.
        let timeout = timeout.max(payload_scaled_ack_timeout(payload.ciphertext.len()));

        let (ack_tx, ack_rx) = oneshot::channel();
        self.pending_acks
            .lock()
            .unwrap()
            .insert(message_id.clone(), ack_tx);
        if let Err(e) = self.send(RelayClientMsg::MailboxDeposit { payload }) {
            self.pending_acks.lock().unwrap().remove(&message_id);
            return Err(AckError::SendFailed(e.to_string()));
        }
        match tokio::time::timeout(timeout, ack_rx).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(rejected))) => Err(AckError::Rejected(rejected)),
            Ok(Err(_)) => Err(AckError::SendFailed("ack channel closed".to_string())),
            Err(_) => {
                // Late acks for this id are dropped by route_ack.
                self.pending_acks.lock().unwrap().remove(&message_id);
                Err(AckError::TimedOut)
            }
        }
    }

    /// Route an incoming deposit ack to its waiter. Exposed for tests that
    /// drive a fake relay through [`RelayClient::new_for_test`]; the real
    /// recv task calls the same routing internally.
    #[cfg(test)]
    pub fn route_ack(&self, payload: MailboxDepositAckPayload) {
        route_ack(&self.pending_acks, payload);
    }

    /// Close the underlying WebSocket immediately: abort the forward/receive
    /// tasks (dropping their WebSocket halves) and drop the receive-channel
    /// sender, so any caller blocked in `recv()` unblocks with `None`.
    /// Idempotent and safe to call multiple times.
    pub fn shutdown(&self) {
        info!("relay client shutting down ({} tasks aborted)", self.tasks.len());
        for task in self.tasks.iter() {
            task.abort();
        }
    }

    /// Whether two handles refer to the same underlying connection (identity
    /// comparison, since `RelayClient` is cheaply cloneable).
    pub fn is_same_connection(&self, other: &RelayClient) -> bool {
        Arc::ptr_eq(&self.rx, &other.rx)
    }

    pub async fn recv(&self) -> Option<RelayServerMsg> {
        self.rx.lock().await.recv().await
    }

    /// Test-only constructor that feeds outgoing messages into the supplied
    /// channel instead of a real WebSocket. Used by sync-engine tests to verify
    /// mailbox deposits without a live relay; acks are delivered by calling
    /// [`RelayClient::route_ack`].
    #[cfg(test)]
    pub fn new_for_test(tx: mpsc::UnboundedSender<RelayClientMsg>) -> Self {
        let (_server_tx, server_rx) = mpsc::unbounded_channel();
        Self {
            tx,
            rx: Arc::new(Mutex::new(server_rx)),
            tasks: Arc::new(vec![]),
            pending_acks: Arc::new(std::sync::Mutex::new(HashMap::new())),
            protocol: Arc::new(std::sync::Mutex::new(RelayProtocol::Unknown)),
        }
    }

    /// Test-only: pretend the relay handshake resolved as a legacy build, so
    /// `deposit_await_ack` takes the fail-fast path.
    #[cfg(test)]
    pub fn force_legacy_relay(&self) {
        *self.protocol.lock().unwrap() = RelayProtocol::Legacy;
    }

    pub async fn expect_signal(&self) -> Result<crate::sync::types::RelaySignalPayload> {
        loop {
            match self.recv().await {
                Some(RelayServerMsg::Signal { payload }) => return Ok(payload),
                Some(RelayServerMsg::Ping) => continue,
                Some(RelayServerMsg::Error { payload }) => {
                    anyhow::bail!("relay error {}: {}", payload.code, payload.message);
                }
                Some(other) => {
                    warn!(msg = ?other, "unexpected relay msg while waiting for signal");
                }
                None => anyhow::bail!("relay closed"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{SinkExt, StreamExt};
    use std::time::Duration;
    use tokio::net::TcpListener;
    use tokio::time::timeout;

    /// Spawn a minimal WebSocket "server" that accepts one connection and
    /// reports whether the client's side of the socket was closed (i.e. the
    /// read stream returned None/Err) — i.e. whether `RelayClient::shutdown`
    /// actually tears the connection down at the TCP level.
    #[tokio::test]
    async fn shutdown_closes_websocket() -> anyhow::Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;

        let server = tokio::spawn(async move {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => return false,
            };
            let mut ws = match tokio_tungstenite::accept_async(stream).await {
                Ok(w) => w,
                Err(_) => return false,
            };
            // Drain until the peer closes the connection.
            loop {
                match ws.next().await {
                    Some(Ok(_)) => continue,
                    _ => return true, // stream ended / errored = connection closed
                }
            }
        });

        let client =
            RelayClient::connect(&format!("ws://{addr}/v1/signaling"), "test-token").await?;
        // Send something so the server is actively reading.
        client
            .send(crate::sync::types::RelayClientMsg::Join {
                payload: crate::sync::types::JoinPayload {
                    room_id: "room-1".to_string(),
                    device_id: Some("device-1".to_string()),
                },
            })
            .ok();

        client.shutdown();

        let closed = timeout(Duration::from_secs(5), server)
            .await
            .context("server did not observe the connection closing in time")?
            .map_err(|e| anyhow::anyhow!("server task failed: {e}"))?;
        assert!(closed, "shutdown() must close the WebSocket at the TCP level");
        Ok(())
    }

    /// `deposit_await_ack` resolves only when the relay acks the deposit:
    /// accepted → Ok, rejected → Err(Rejected), no ack → Err(TimedOut).
    #[tokio::test]
    async fn deposit_await_ack_correlates_acks() -> anyhow::Result<()> {
        use crate::sync::types::{MailboxDepositAckPayload, MailboxDepositPayload};

        // Accepted.
        {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RelayClientMsg>();
            let client = RelayClient::new_for_test(tx);
            let ack_client = client.clone();
            let responder = tokio::spawn(async move {
                let msg = rx.recv().await.unwrap();
                let RelayClientMsg::MailboxDeposit { payload } = msg else {
                    panic!("expected deposit");
                };
                ack_client.route_ack(MailboxDepositAckPayload {
                    id: payload.message_id.unwrap(),
                    ok: true,
                    error: None,
                });
            });
            let result = client
                .deposit_await_ack(
                    MailboxDepositPayload {
                        to_device_id: "dev-b".to_string(),
                        ciphertext: "ct".to_string(),
                        nonce: "nn".to_string(),
                        ttl_seconds: None,
                        message_id: None,
                    },
                    Duration::from_secs(2),
                )
                .await;
            assert!(result.is_ok(), "accepted deposit must resolve Ok");
            responder.await?;
        }

        // Rejected.
        {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RelayClientMsg>();
            let client = RelayClient::new_for_test(tx);
            let ack_client = client.clone();
            let responder = tokio::spawn(async move {
                let msg = rx.recv().await.unwrap();
                let RelayClientMsg::MailboxDeposit { payload } = msg else {
                    panic!("expected deposit");
                };
                ack_client.route_ack(MailboxDepositAckPayload {
                    id: payload.message_id.unwrap(),
                    ok: false,
                    error: Some("target device not in room".to_string()),
                });
            });
            let result = client
                .deposit_await_ack(
                    MailboxDepositPayload {
                        to_device_id: "ghost".to_string(),
                        ciphertext: "ct".to_string(),
                        nonce: "nn".to_string(),
                        ttl_seconds: None,
                        message_id: None,
                    },
                    Duration::from_secs(2),
                )
                .await;
            assert!(matches!(result, Err(AckError::Rejected(_))), "rejection must surface");
            responder.await?;
        }

        // Timeout: no ack arrives.
        {
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<RelayClientMsg>();
            let client = RelayClient::new_for_test(tx);
            let result = client
                .deposit_await_ack(
                    MailboxDepositPayload {
                        to_device_id: "dev-b".to_string(),
                        ciphertext: "ct".to_string(),
                        nonce: "nn".to_string(),
                        ttl_seconds: None,
                        message_id: None,
                    },
                    Duration::from_millis(150),
                )
                .await;
            assert!(matches!(result, Err(AckError::TimedOut)), "missing ack must time out");
        }

        Ok(())
    }

    /// The ack-timeout floor scales with payload size: small deposits keep
    /// the 3s floor, a ~13.4MB deposit (the production incident) gets ~8s,
    /// and huge payloads are capped at 60s.
    #[test]
    fn ack_timeout_scales_with_payload() {
        // Tiny payload → 3s floor.
        assert_eq!(payload_scaled_ack_timeout(8), Duration::from_secs(3));
        // Boundary: 2MB of ciphertext → 2s + 1s = 3s (still the floor).
        let two_mb_b64 = (2 * 1024 * 1024) * 4 / 3;
        assert_eq!(payload_scaled_ack_timeout(two_mb_b64), Duration::from_secs(3));
        // 13.4MB ciphertext → 2s + ~6s = ~8s.
        let thirteen_mb_b64 = (13_400_000u64 * 4 / 3) as usize;
        let t = payload_scaled_ack_timeout(thirteen_mb_b64);
        assert!(t >= Duration::from_secs(7) && t <= Duration::from_secs(9), "got {t:?}");
        // Huge payload → capped at 60s.
        let huge_b64 = (200 * 1024 * 1024) * 4 / 3;
        assert_eq!(payload_scaled_ack_timeout(huge_b64), Duration::from_secs(60));
    }

    /// Once the relay is known to be legacy (no/unsupported ServerHello),
    /// `deposit_await_ack` fails fast with a clear "relay too old" error
    /// instead of burning the full ack timeout on every deposit.
    #[tokio::test]
    async fn deposit_fast_fails_once_relay_known_legacy() {
        use crate::sync::types::MailboxDepositPayload;

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<RelayClientMsg>();
        let client = RelayClient::new_for_test(tx);
        client.force_legacy_relay();

        let start = std::time::Instant::now();
        let result = client
            .deposit_await_ack(
                MailboxDepositPayload {
                    to_device_id: "dev-b".to_string(),
                    ciphertext: "ct".to_string(),
                    nonce: "nn".to_string(),
                    ttl_seconds: None,
                    message_id: None,
                },
                Duration::from_secs(3),
            )
            .await;
        assert!(
            matches!(result, Err(AckError::SendFailed(ref e)) if e.contains("relay too old")),
            "legacy relay must fail fast with a clear error, got: {result:?}"
        );
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "fail-fast must not wait for the ack timeout"
        );
    }

    /// Against a relay that never sends ServerHello (a pre-handshake build),
    /// the client marks the relay legacy once the hello window has passed and
    /// deposits then fail fast instead of waiting for acks that never come.
    #[tokio::test]
    async fn relay_without_server_hello_fails_fast() -> anyhow::Result<()> {
        use crate::sync::types::MailboxDepositPayload;

        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;

        // Fake legacy relay: accepts the socket and drains messages, but
        // never sends ServerHello (nor any ack).
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(_)) = ws.next().await {}
        });

        let client =
            RelayClient::connect(&format!("ws://{addr}/v1/signaling"), "test-token").await?;
        client.send(crate::sync::types::RelayClientMsg::Join {
            payload: crate::sync::types::JoinPayload {
                room_id: "room-1".to_string(),
                device_id: Some("device-1".to_string()),
            },
        })?;

        // Let the ServerHello watchdog window pass.
        tokio::time::sleep(SERVER_HELLO_TIMEOUT + Duration::from_millis(300)).await;

        let start = std::time::Instant::now();
        let result = client
            .deposit_await_ack(
                MailboxDepositPayload {
                    to_device_id: "dev-b".to_string(),
                    ciphertext: "ct".to_string(),
                    nonce: "nn".to_string(),
                    ttl_seconds: None,
                    message_id: None,
                },
                Duration::from_secs(3),
            )
            .await;
        assert!(
            matches!(result, Err(AckError::SendFailed(ref e)) if e.contains("relay too old")),
            "deposit on a legacy relay must fail fast, got: {result:?}"
        );
        assert!(start.elapsed() < Duration::from_millis(500));

        client.shutdown();
        server.abort();
        Ok(())
    }
}
