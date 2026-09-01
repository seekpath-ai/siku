//! Client-side encrypted mailbox: deposit/poll/ack over the relay, with a
//! background listener that delivers `MailboxBatch` messages to a callback.

use crate::sync::relay_client::RelayClient;
use crate::sync::types::{
    JoinPayload, MailboxAckPayload, MailboxBatchPayload, MailboxDepositPayload,
    MailboxMessage, RelayClientMsg, RelayServerMsg,
};
use anyhow::{Context, Result};
use tracing::info;

const DEFAULT_TTL_SECONDS: u64 = 7 * 24 * 3600;

pub struct MailboxClient {
    relay: RelayClient,
    tasks: std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl MailboxClient {
    /// Connect to the relay and join the account room under `device_id`.
    pub async fn connect(
        relay_url: &str,
        token: &str,
        room_id: &str,
        device_id: &str,
    ) -> Result<Self> {
        let relay = RelayClient::connect(relay_url, token).await?;
        relay.send(RelayClientMsg::Join {
            payload: JoinPayload {
                room_id: room_id.to_string(),
                device_id: Some(device_id.to_string()),
            },
        })?;
        info!(room = %room_id, device = %device_id, "mailbox client connected");
        Ok(Self {
            relay,
            tasks: std::sync::Mutex::new(Vec::new()),
        })
    }

    /// Register a handler for incoming mailbox batches, plus a handler for
    /// relay `Error` messages (surfaced so the engine can record them in its
    /// status). The handlers run on a background task; they must be cheap
    /// (spawn heavy work themselves).
    pub fn on_batch<F, E>(&self, handler: F, on_error: E)
    where
        F: Fn(Vec<MailboxMessage>) + Send + Sync + 'static,
        E: Fn(String) + Send + Sync + 'static,
    {
        self.spawn_listener(move |msg| match msg {
            RelayServerMsg::MailboxBatch { payload: MailboxBatchPayload { messages } } => {
                handler(messages);
            }
            RelayServerMsg::Error { payload } => {
                on_error(format!("{}: {}", payload.code, payload.message));
            }
            _ => {}
        });
    }

    fn spawn_listener<F>(&self, handler: F)
    where
        F: Fn(RelayServerMsg) + Send + Sync + 'static,
    {
        let relay = self.relay.clone();
        let task = tokio::spawn(async move {
            loop {
                match relay.recv().await {
                    Some(msg) => handler(msg),
                    None => {
                        info!("mailbox relay closed");
                        break;
                    }
                }
            }
        });
        if let Ok(mut tasks) = self.tasks.lock() {
            tasks.push(task);
        }
    }

    /// Test-only constructor wrapping a supplied relay (typically a
    /// [`RelayClient::new_for_test`] driven by a fake relay).
    #[cfg(test)]
    pub fn new_for_test(relay: RelayClient) -> Self {
        Self {
            relay,
            tasks: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Encrypt `plaintext` with the account sync key and deposit it for
    /// `to_device_id`.
    pub async fn deposit_encrypted(
        &self,
        to_device_id: &str,
        ciphertext: Vec<u8>,
        nonce: [u8; crate::sync::crypto::NONCE_LEN],
        ttl_seconds: Option<u64>,
    ) -> Result<()> {
        use base64::Engine;
        self.relay
            .send(RelayClientMsg::MailboxDeposit {
                payload: MailboxDepositPayload {
                    to_device_id: to_device_id.to_string(),
                    ciphertext: base64::engine::general_purpose::STANDARD.encode(ciphertext),
                    nonce: base64::engine::general_purpose::STANDARD.encode(nonce),
                    ttl_seconds: ttl_seconds.or(Some(DEFAULT_TTL_SECONDS)),
                    message_id: None,
                },
            })
            .context("send mailbox deposit")
    }

    /// Like [`Self::deposit_encrypted`] but waits for the relay's
    /// `MailboxDepositAck`: returns Ok only when the message is durably
    /// stored. Used by the sync engine to advance its cursor only after a
    /// confirmed deposit (see `sync::engine::sync_over_mailbox`). Pass a
    /// fixed `message_id` when the deposit may be retried (outbox), so the
    /// relay dedupes the retransmits; `None` generates a fresh one.
    pub async fn deposit_encrypted_await_ack(
        &self,
        to_device_id: &str,
        ciphertext: Vec<u8>,
        nonce: [u8; crate::sync::crypto::NONCE_LEN],
        ttl_seconds: Option<u64>,
        message_id: Option<String>,
        timeout: std::time::Duration,
    ) -> std::result::Result<(), crate::sync::relay_client::AckError> {
        use base64::Engine;
        self.relay
            .deposit_await_ack(
                MailboxDepositPayload {
                    to_device_id: to_device_id.to_string(),
                    ciphertext: base64::engine::general_purpose::STANDARD.encode(ciphertext),
                    nonce: base64::engine::general_purpose::STANDARD.encode(nonce),
                    ttl_seconds: ttl_seconds.or(Some(DEFAULT_TTL_SECONDS)),
                    message_id,
                },
                timeout,
            )
            .await
    }

    /// Acknowledge processed message ids so the relay can drop them.
    pub async fn ack(&self, message_ids: Vec<String>) -> Result<()> {
        self.relay
            .send(RelayClientMsg::MailboxAck {
                payload: MailboxAckPayload { message_ids },
            })
            .context("send mailbox ack")
    }

    /// The underlying relay connection (used for one-shot deposits and acks
    /// without owning a separate client).
    pub fn relay(&self) -> &RelayClient {
        &self.relay
    }

    /// Stop every background listener task and close the underlying relay
    /// WebSocket, so the device stops appearing online the moment a session
    /// (or the account) is torn down. Dropping the client afterwards releases
    /// the remaining references.
    pub fn shutdown(&self) {
        if let Ok(mut tasks) = self.tasks.lock() {
            for task in tasks.drain(..) {
                task.abort();
            }
        }
        self.relay.shutdown();
        info!("mailbox client shut down");
    }
}

/// Load the account sync key (base64) from device-local settings (never from
/// the synced `settings` table — the key must not leave the device via sync).
/// Returns None when no account/pairing has happened yet.
pub async fn load_sync_key(db: &sqlx::SqlitePool) -> Option<[u8; crate::sync::crypto::SYNC_KEY_LEN]> {
    use base64::Engine;
    let raw = crate::core::settings_service::get_device_setting(
        db,
        crate::sync::onboarding::ACCOUNT_SYNC_KEY_SETTING,
    )
    .await
    .ok()?
    .filter(|v| !v.is_empty())?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(raw)
        .ok()?;
    bytes.try_into().ok()
}
