use crate::sync::types::{RelayClientMsg, RelayServerMsg};
use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{info, warn};

#[derive(Clone)]
pub struct RelayClient {
    tx: mpsc::UnboundedSender<RelayClientMsg>,
    rx: Arc<Mutex<mpsc::UnboundedReceiver<RelayServerMsg>>>,
    /// Background tasks driving the WebSocket; aborted by `shutdown()`.
    tasks: Arc<Vec<tokio::task::JoinHandle<()>>>,
}

impl RelayClient {
    pub async fn connect(relay_url: &str, token: &str) -> Result<Self> {
        let url = format!("{}?token={}", relay_url, token);
        let (ws_stream, _) = connect_async(&url)
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
                        Ok(msg) => {
                            if server_tx.send(msg).is_err() {
                                break;
                            }
                        }
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
        })
    }

    pub fn send(&self, msg: RelayClientMsg) -> Result<()> {
        self.tx
            .send(msg)
            .map_err(|_| anyhow::anyhow!("relay send failed"))
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
    /// mailbox deposits without a live relay.
    #[cfg(test)]
    pub fn new_for_test(tx: mpsc::UnboundedSender<RelayClientMsg>) -> Self {
        let (_server_tx, server_rx) = mpsc::unbounded_channel();
        Self {
            tx,
            rx: Arc::new(Mutex::new(server_rx)),
            tasks: Arc::new(vec![]),
        }
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
}
