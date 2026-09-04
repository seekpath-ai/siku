use crate::sync::attachments::{
    blob_fits_mailbox, collect_missing_blob_hashes, read_blob_base64, write_blob_from_base64,
};
use crate::sync::crdt::{apply_changes, export_changes_since, export_own_changes_since, ChangesetMessage};
use crate::sync::mailbox_client::MailboxClient;
use crate::sync::types::MailboxMessage;
use crate::sync::webrtc_peer::SyncSession;
use anyhow::{Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

/// Snapshot of the current sync session status for the UI.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncStatus {
    pub connected: bool,
    pub peer_device_id: Option<String>,
    pub last_sync_at: Option<String>,
    pub last_error: Option<String>,
    /// Current transport: "p2p", "mailbox", or "none".
    #[serde(default = "default_transport")]
    pub transport: String,
    /// Number of changesets waiting in the local outbox.
    #[serde(default)]
    pub outbox_pending: i64,
    /// Cumulative row-changes successfully sent to the peer (incl. mailbox).
    #[serde(default)]
    pub pushed: i64,
    /// Cumulative row-changes received and applied from the peer (incl.
    /// mailbox and full snapshots).
    #[serde(default)]
    pub pulled: i64,
    /// Session kind: "lan" (local pairing) or "cloud" (account auto-sync).
    /// The UI shows it only on the matching tab — LAN and cloud sessions
    /// share one engine slot, so a cloud session must not light up the LAN
    /// tab and vice versa.
    #[serde(default)]
    pub kind: Option<String>,
}

fn default_transport() -> String {
    "none".to_string()
}

/// Wire message envelope exchanged over the sync DataChannel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum SyncMessage {
    #[serde(rename = "changeset")]
    Changeset(ChangesetMessage),
    #[serde(rename = "full_snapshot")]
    FullSnapshot { statements: Vec<String> },
    #[serde(rename = "chunk")]
    Chunk {
        id: String,
        index: usize,
        total: usize,
        data: String, // base64 slice
    },
    #[serde(rename = "pull")]
    Pull,
    #[serde(rename = "attachment_request")]
    AttachmentRequest { hashes: Vec<(String, String)> },
    #[serde(rename = "attachment_payload")]
    AttachmentPayload {
        hash: String,
        ext: String,
        data: String, // base64
    },
}

/// DataChannel messages above this size are split into chunks: WebRTC SCTP
/// rejects oversized datagrams (default max message size is 16KB, so keep a
/// conservative threshold — snapshots with hundreds of rows easily exceed it).
const MAX_WIRE_MSG: usize = 16_000;

/// Interval of the background incremental push loop (seconds). While a sync
/// session is alive, local changes are pushed to the peer on this cadence —
/// no manual "sync now" needed for continuous sync.
const SYNC_PUSH_INTERVAL_SECS: u64 = 15;

/// How long to wait for the relay's `MailboxDepositAck` before treating a
/// deposit as unconfirmed (queued to the outbox / retried). This is only the
/// floor: `deposit_await_ack` scales the wait with the payload size (large
/// frames take seconds just to transmit), capped at 60s.
const MAILBOX_ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// device_settings keys persisting sync progress per peer. Progress must be
/// keyed by peer: a global watermark would make a freshly reconnected peer
/// skip changes that were only delivered to a different peer.
const CURSOR_KEY_PREFIX: &str = "sync.cursor.sent.";
const SNAPSHOT_SENT_KEY_PREFIX: &str = "sync.snapshot.sent.";

/// device_settings key storing the last db_version successfully sent to a peer.
pub fn sent_cursor_key(peer_key: &str) -> String {
    format!("{CURSOR_KEY_PREFIX}{peer_key}")
}

/// device_settings key storing whether the full snapshot was already sent to a
/// peer (avoids re-sending the whole database on every reconnect).
pub fn snapshot_sent_key(peer_key: &str) -> String {
    format!("{SNAPSHOT_SENT_KEY_PREFIX}{peer_key}")
}

/// Load persisted sync progress for a peer: `(last sent db_version,
/// full_snapshot_sent)`. Unknown peers start from zero (full re-export).
pub async fn load_peer_progress(
    db: &sqlx::SqlitePool,
    peer_key: &str,
) -> (i64, bool) {
    let sent = crate::core::settings_service::get_device_setting(db, &sent_cursor_key(peer_key))
        .await
        .ok()
        .flatten()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    let snapshot_sent = crate::core::settings_service::get_device_setting(
        db,
        &snapshot_sent_key(peer_key),
    )
    .await
    .ok()
    .flatten()
    .map(|v| v == "1")
    .unwrap_or(false);
    (sent, snapshot_sent)
}

/// Reassembles chunked messages on the receiving side.
struct ChunkAssembler {
    parts: HashMap<String, (usize, Vec<Option<Vec<u8>>>)>,
}

impl ChunkAssembler {
    fn new() -> Self {
        Self {
            parts: HashMap::new(),
        }
    }

    /// Feed one chunk; returns the full message bytes when complete.
    fn push(&mut self, id: &str, index: usize, total: usize, data: &str) -> Option<Vec<u8>> {
        use base64::Engine as _;
        let entry = self
            .parts
            .entry(id.to_string())
            .or_insert_with(|| (total, vec![None; total]));
        if index >= total {
            return None;
        }
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(data)
            .unwrap_or_default();
        entry.1[index] = Some(decoded);
        if entry.1.iter().all(|p| p.is_some()) {
            let mut out = Vec::new();
            for part in &entry.1 {
                if let Some(p) = part {
                    out.extend_from_slice(p);
                }
            }
            self.parts.remove(id);
            return Some(out);
        }
        None
    }
}

/// Manages a single sync session with a peer: WebRTC DataChannel + CR-SQLite changesets + blobs.
pub struct SyncEngine {
    session: Arc<SyncSession>,
    db: SqlitePool,
    app_data_dir: PathBuf,
    last_sent_db_version: Mutex<i64>,
    last_applied_db_version: Mutex<i64>,
    status: Mutex<SyncStatus>,
    mailbox: Option<MailboxClient>,
    sync_key: Option<[u8; crate::sync::crypto::SYNC_KEY_LEN]>,
    full_snapshot_sent: Mutex<bool>,
    assembler: std::sync::Mutex<ChunkAssembler>,
    /// Set by `stop()`; message handlers spawned by `start()` bail out when
    /// set, so a "disconnected" engine stops applying peer data even though
    /// the callback closures still hold an Arc to this engine.
    stopped: std::sync::atomic::AtomicBool,
    /// Session kind: "lan" / "cloud" / "unknown" (see SyncStatus::kind).
    kind: String,
    /// Stable identity of the peer used to persist sync progress
    /// (device id, or "lan" when unknown).
    peer_key: String,
}

impl SyncEngine {
    pub fn new(
        session: Arc<SyncSession>,
        db: SqlitePool,
        app_data_dir: PathBuf,
        peer_device_id: Option<String>,
    ) -> Self {
        Self {
            session,
            db,
            app_data_dir,
            last_sent_db_version: Mutex::new(0),
            last_applied_db_version: Mutex::new(0),
            status: Mutex::new(SyncStatus {
                connected: true,
                peer_device_id,
                transport: "p2p".to_string(),
                ..Default::default()
            }),
            mailbox: None,
            sync_key: None,
            full_snapshot_sent: Mutex::new(false),
            assembler: std::sync::Mutex::new(ChunkAssembler::new()),
            stopped: std::sync::atomic::AtomicBool::new(false),
            kind: "unknown".to_string(),
            peer_key: String::new(),
        }
    }

    /// Tag this session as a LAN pairing or cloud auto-sync session.
    pub fn with_kind(mut self, kind: &str) -> Self {
        self.kind = kind.to_string();
        self
    }

    /// Set the stable peer identity used to persist sync progress. Falls back
    /// to the peer device id; callers with a LAN guest that carries no id may
    /// pass an explicit key.
    pub fn with_peer_key(mut self, peer_key: &str) -> Self {
        self.peer_key = peer_key.to_string();
        self
    }

    /// Restore persisted sync progress for the peer: the last db_version
    /// already sent to it and whether the full snapshot was delivered. New
    /// sessions then only send the delta instead of the whole history.
    pub fn with_peer_progress(mut self, sent_db_version: i64, snapshot_sent: bool) -> Self {
        *self.last_sent_db_version.get_mut() = sent_db_version;
        *self.full_snapshot_sent.get_mut() = snapshot_sent;
        self
    }

    /// Stop this engine: mark it stopped so in-flight handlers bail out,
    /// close the WebRTC connection and shut down the mailbox transport.
    /// Safe to call multiple times.
    pub async fn stop(&self) {
        self.stopped.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = self.session.close().await;
        if let Some(mb) = &self.mailbox {
            mb.shutdown();
        }
        self.status.lock().await.connected = false;
        info!("sync engine stopped");
    }

    /// Send a message, splitting it into chunks when it exceeds the SCTP
    /// datagram limit.
    async fn send_message(&self, msg: &SyncMessage) -> Result<()> {
        let json = serde_json::to_string(msg).context("serialize sync message")?;
        if json.len() <= MAX_WIRE_MSG {
            info!(bytes = json.len(), "sending sync message");
            return self.session.send_text(json).await.context("send sync message");
        }
        let id = uuid::Uuid::new_v4().to_string();
        let bytes = json.into_bytes();
        let total = bytes.len().div_ceil(MAX_WIRE_MSG);
        info!(bytes = bytes.len(), total, "splitting large sync message into chunks");
        for (index, part) in bytes.chunks(MAX_WIRE_MSG).enumerate() {
            use base64::Engine as _;
            let chunk = SyncMessage::Chunk {
                id: id.clone(),
                index,
                total,
                data: base64::engine::general_purpose::STANDARD.encode(part),
            };
            let chunk_json = serde_json::to_string(&chunk).context("serialize chunk")?;
            self.session
                .send_text(chunk_json)
                .await
                .with_context(|| format!("send chunk {index}/{total}"))?;
        }
        Ok(())
    }

    /// Attach an encrypted mailbox transport. When set, changes that cannot be
    /// sent over the DataChannel fall back to the mailbox.
    pub fn with_mailbox(mut self, mailbox: MailboxClient) -> Self {
        self.mailbox = Some(mailbox);
        self
    }

    /// Set the account sync key used to encrypt mailbox payloads.
    pub fn with_sync_key(mut self, key: [u8; crate::sync::crypto::SYNC_KEY_LEN]) -> Self {
        self.sync_key = Some(key);
        self
    }

    pub async fn status(&self) -> SyncStatus {
        let mut s = self.status.lock().await.clone();
        s.kind = Some(self.kind.clone());
        s
    }

    pub async fn mark_synced(&self) {
        self.status.lock().await.last_sync_at = Some(crate::core::time::now_iso());
    }

    /// Persist the sent watermark for this peer so a future session (or the
    /// offline mailbox path) can resume from here instead of re-exporting the
    /// whole history. Best-effort: failures only log.
    async fn persist_sent_cursor(&self) {
        if self.peer_key.is_empty() {
            return;
        }
        let v = *self.last_sent_db_version.lock().await;
        let _ = crate::core::settings_service::set_device_setting(
            &self.db,
            &sent_cursor_key(&self.peer_key),
            &v.to_string(),
        )
        .await;
    }

    async fn persist_snapshot_sent(&self) {
        if self.peer_key.is_empty() {
            return;
        }
        let _ = crate::core::settings_service::set_device_setting(
            &self.db,
            &snapshot_sent_key(&self.peer_key),
            "1",
        )
        .await;
    }

    pub async fn set_connected(&self, connected: bool) {
        self.status.lock().await.connected = connected;
    }

    pub async fn set_last_error(&self, error: Option<String>) {
        self.status.lock().await.last_error = error;
    }

    /// Send the current local changeset to the peer.
    pub async fn push(&self) -> Result<()> {
        let since = *self.last_sent_db_version.lock().await;
        // Incremental pushes export own-site rows only: foreign-site rows in
        // range are echoes of what we just received and must not bounce back.
        let msg = export_own_changes_since(&self.db, since).await?;
        if msg.changes.is_empty() {
            info!("no local changes to push");
            return Ok(());
        }
        let sent = msg.changes.len() as i64;
        let envelope = SyncMessage::Changeset(msg);
        self.send_message(&envelope).await.context("send changeset")?;
        *self.last_sent_db_version.lock().await = envelope.to_db_version().unwrap_or(since);
        self.persist_sent_cursor().await;
        self.status.lock().await.pushed += sent;
        self.mark_synced().await;
        info!(
            to_db_version = envelope.to_db_version().unwrap_or(since),
            "pushed changeset"
        );
        Ok(())
    }

    /// Request missing blobs after a changeset has been applied.
    ///
    /// Tries the live DataChannel first; if that fails and a mailbox transport
    /// is attached, deposits the request into the peer's mailbox so offline
    /// devices can fetch PDFs/attachments when they come back online.
    async fn request_missing_blobs(&self) -> Result<()> {
        let missing = collect_missing_blob_hashes(&self.db, &self.app_data_dir).await?;
        if missing.is_empty() {
            return Ok(());
        }
        info!(count = missing.len(), "requesting missing blobs");
        let msg = SyncMessage::AttachmentRequest { hashes: missing };
        if let Err(e) = self.send_message(&msg).await {
            if let Some(peer) = self.status.lock().await.peer_device_id.clone() {
                if self.mailbox.is_some() {
                    if let Err(e2) = self.deposit_message_to(&peer, &msg).await {
                        return Err(e2).with_context(|| {
                            format!("data channel failed ({e}) and mailbox deposit failed")
                        });
                    }
                    info!(to = %peer, "deposited attachment request to mailbox");
                    return Ok(());
                }
            }
            return Err(e).context("send attachment request");
        }
        Ok(())
    }

    /// Encrypt and deposit a sync message for a specific peer device.
    async fn deposit_message_to(
        &self,
        to_device_id: &str,
        msg: &SyncMessage,
    ) -> Result<()> {
        let Some(mailbox) = &self.mailbox else {
            anyhow::bail!("mailbox transport not available");
        };
        let Some(key) = &self.sync_key else {
            anyhow::bail!("sync key not available");
        };
        let json = serde_json::to_string(msg).context("serialize mailbox sync message")?;
        let (ciphertext, nonce) =
            crate::sync::crypto::encrypt_bytes(key, json.as_bytes()).map_err(anyhow::Error::msg)?;
        mailbox
            .deposit_encrypted(to_device_id, ciphertext, nonce, None)
            .await
            .context("deposit sync message to mailbox")
    }

    /// Receive and apply a changeset from the peer.
    pub async fn handle_changeset(&self, msg: ChangesetMessage) -> Result<()> {
        let applied_rows = apply_changes(&self.db, &msg).await? as i64;
        let mut applied = self.last_applied_db_version.lock().await;
        *applied = applied.max(msg.to_db_version);
        drop(applied);
        self.status.lock().await.pulled += applied_rows;
        self.mark_synced().await;
        // Notify the frontend only when rows actually changed; a changeset
        // fully filtered out by LWW/sync-scope must not trigger a reload.
        if applied_rows > 0 {
            crate::sync::emit_remote_applied(applied_rows);
        }
        self.request_missing_blobs().await?;
        Ok(())
    }

    /// Handle an attachment-related message from the peer.
    pub async fn handle_attachment_message(&self, msg: SyncMessage) -> Result<()> {
        match msg {
            SyncMessage::AttachmentRequest { hashes } => {
                info!(count = hashes.len(), "received blob request");
                for (hash, ext) in hashes {
                    match read_blob_base64(&self.app_data_dir, &hash, &ext) {
                        Ok(Some(data)) => {
                            // Route through send_message: real PDFs/blobs exceed
                            // the SCTP datagram limit and must be chunked.
                            let payload = SyncMessage::AttachmentPayload {
                                hash: hash.clone(),
                                ext: ext.clone(),
                                data,
                            };
                            if let Err(e) = self.send_message(&payload).await {
                                // DataChannel unavailable: try mailbox fallback
                                // when we know which peer asked. Oversized blobs
                                // are not mailbox-eligible (single-frame limit);
                                // they only sync while P2P is up.
                                if !blob_fits_mailbox(&self.app_data_dir, &hash, &ext) {
                                    warn!(
                                        error = %e,
                                        hash = %hash,
                                        "failed to send blob payload; too large for mailbox fallback"
                                    );
                                } else if let Some(peer) =
                                    self.status.lock().await.peer_device_id.clone()
                                {
                                    if let Err(e2) = self.deposit_message_to(&peer, &payload).await
                                    {
                                        warn!(
                                            error = %e,
                                            error2 = %e2,
                                            hash = %hash,
                                            "failed to send blob payload over data channel and mailbox"
                                        );
                                    } else {
                                        info!(to = %peer, hash = %hash, "deposited blob payload to mailbox");
                                    }
                                } else {
                                    warn!(error = %e, hash = %hash, "failed to send blob payload");
                                }
                            }
                        }
                        Ok(None) => warn!(hash = %hash, "peer requested blob we do not have"),
                        Err(e) => warn!(error = %e, hash = %hash, "failed to read blob"),
                    }
                }
            }
            SyncMessage::AttachmentPayload { hash, ext, data } => {
                if let Err(e) = write_blob_from_base64(&self.app_data_dir, &hash, &ext, &data) {
                    warn!(error = %e, hash = %hash, "failed to write received blob");
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// One-shot bidirectional sync: push then pull. Records the outcome in
    /// `last_error` so the UI can show the failure reason.
    pub async fn sync_once(&self) -> Result<()> {
        match self.sync_once_inner().await {
            Ok(()) => {
                self.status.lock().await.last_error = None;
                Ok(())
            }
            Err(e) => {
                self.status.lock().await.last_error = Some(e.to_string());
                Err(e)
            }
        }
    }

    async fn sync_once_inner(&self) -> Result<()> {
        info!("sync_once starting");
        // First time on this connection: send a full-history changeset
        // (everything in crsql_changes since db_version 0, tombstones
        // included) so pre-existing history reaches the peer. The receiver
        // runs it through the same cr-sqlite merge as incremental sync, so
        // rows deleted on either side stay deleted — plain INSERT snapshots
        // used to resurrect them. Idempotent, so re-sending is harmless.
        {
            let mut sent = self.full_snapshot_sent.lock().await;
            if !*sent {
                let msg = export_changes_since(&self.db, 0).await?;
                if !msg.changes.is_empty() {
                    let count = msg.changes.len();
                    self.send_message(&SyncMessage::Changeset(msg))
                        .await
                        .context("send full-history changeset")?;
                    info!(count, "sent full-history changeset");
                    self.persist_snapshot_sent().await;
                }
                *sent = true;
            }
        }
        self.push().await?;
        // Request peer changes by sending a lightweight pull request.
        let msg = SyncMessage::Pull;
        self.send_message(&msg).await.context("send pull request")?;
        self.mark_synced().await;
        info!("sync_once done");
        Ok(())
    }

    // ── Encrypted mailbox fallback ─────────────────────────────────────────

    /// Export local changes, encrypt them with the account sync key, and
    /// deposit them into the peer's mailbox. When the deposit fails (relay
    /// unreachable) the message is written to the local outbox for retry.
    pub async fn sync_over_mailbox(&self, to_device_id: &str) -> Result<()> {
        let Some(mailbox) = &self.mailbox else {
            anyhow::bail!("mailbox transport disabled (no session mailbox)");
        };
        let Some(key) = &self.sync_key else {
            anyhow::bail!("no sync key configured; mailbox sync unavailable");
        };

        let since = *self.last_sent_db_version.lock().await;
        // Own-site only: incremental mailbox delivery must not echo back
        // changes we just applied from the peer.
        let msg = export_own_changes_since(&self.db, since).await?;
        if msg.changes.is_empty() {
            return Ok(());
        }
        let sent = msg.changes.len() as i64;
        let envelope = SyncMessage::Changeset(msg);
        let json = serde_json::to_string(&envelope).context("serialize changeset")?;
        let (ciphertext, nonce) =
            crate::sync::crypto::encrypt_bytes(key, json.as_bytes()).map_err(anyhow::Error::msg)?;

        // The message_id is fixed BEFORE the first deposit attempt so an
        // outbox retry reuses it (relay-side dedupe makes the retry
        // idempotent even when the original deposit was stored but its ack
        // was lost).
        let message_id = uuid::Uuid::new_v4().to_string();
        match mailbox
            .deposit_encrypted_await_ack(to_device_id, ciphertext.clone(), nonce, None, Some(message_id.clone()), MAILBOX_ACK_TIMEOUT)
            .await
        {
            Ok(()) => {
                let v = envelope.to_db_version().unwrap_or(since);
                *self.last_sent_db_version.lock().await = v;
                self.persist_sent_cursor().await;
                self.status.lock().await.pushed += sent;
                self.mark_synced().await;
                info!(to = %to_device_id, to_db_version = v, "changeset deposited to mailbox (acked)");
                self.set_transport("mailbox").await;
            }
            Err(e) => {
                // Unconfirmed deposit: do NOT advance the cursor — queue the
                // changeset so it is retried instead of lost.
                warn!(error = %e, "mailbox deposit unacknowledged; writing to outbox");
                self.write_outbox(to_device_id, &ciphertext, &nonce, &message_id).await?;
                self.refresh_outbox_count().await;
            }
        }
        Ok(())
    }

    /// Handle a decrypted mailbox message: apply the changeset (and any
    /// attachment request/response), then ack.
    pub async fn handle_mailbox_message(&self, mb_msg: MailboxMessage) -> Result<()> {
        let Some(mailbox) = &self.mailbox else {
            return Ok(());
        };
        let Some(key) = &self.sync_key else {
            return Ok(());
        };
        use base64::Engine as _;
        let nonce = base64::engine::general_purpose::STANDARD
            .decode(&mb_msg.nonce)
            .context("decode nonce")?;
        let ciphertext = base64::engine::general_purpose::STANDARD
            .decode(&mb_msg.ciphertext)
            .context("decode ciphertext")?;
        let plaintext = crate::sync::crypto::decrypt_bytes(key, &nonce, &ciphertext)
            .map_err(anyhow::Error::msg)?;
        let envelope: SyncMessage = serde_json::from_slice(&plaintext).context("parse changeset")?;
        match envelope {
            SyncMessage::Changeset(cs) => {
                self.handle_changeset(cs).await?;
                info!(from = %mb_msg.from_device_id, "applied mailbox changeset");
                // The changeset may reference blobs (paper PDFs, note images)
                // that the mailbox path does not automatically carry. Request
                // them from the sender so they arrive as follow-up mailbox
                // messages.
                if let Err(e) = self.request_missing_blobs_from(&mb_msg.from_device_id).await {
                    warn!(error = %e, from = %mb_msg.from_device_id, "mailbox blob request failed");
                }
            }
            SyncMessage::Pull => {
                // Peer asked for our changes over the mailbox; reply by
                // depositing our changeset back.
                if let Err(e) = self.sync_over_mailbox(&mb_msg.from_device_id).await {
                    warn!(error = %e, "mailbox pull reply failed");
                }
            }
            SyncMessage::FullSnapshot { statements } => {
                // A peer shipped its whole database through the mailbox so
                // pre-CRR history (rows that never appear in crsql_changes)
                // reaches a device that has never had a P2P session.
                match crate::sync::crdt::apply_full_snapshot(&self.db, &statements).await {
                    Ok(applied) => {
                        let applied = applied as i64;
                        info!(count = applied, "applied mailbox full snapshot");
                        if applied > 0 {
                            self.status.lock().await.pulled += applied;
                            self.mark_synced().await;
                            crate::sync::emit_remote_applied(applied);
                        }
                        // Snapshot rows may reference blobs (paper PDFs, note
                        // images) the mailbox does not carry — request them
                        // from the sender, same as the changeset branch.
                        if let Err(e) =
                            self.request_missing_blobs_from(&mb_msg.from_device_id).await
                        {
                            warn!(error = %e, from = %mb_msg.from_device_id, "mailbox blob request after snapshot failed");
                        }
                    }
                    Err(e) => warn!(error = %e, "apply mailbox full snapshot failed"),
                }
            }
            attachment_msg @ (SyncMessage::AttachmentRequest { .. } | SyncMessage::AttachmentPayload { .. }) => {
                if let Err(e) = self.handle_attachment_message_for(&mb_msg.from_device_id, attachment_msg).await {
                    warn!(error = %e, from = %mb_msg.from_device_id, "mailbox attachment message failed");
                }
            }
            other => warn!(msg = ?other, "ignoring unsupported mailbox message"),
        }
        mailbox.ack(vec![mb_msg.id]).await.ok();
        self.set_transport("mailbox").await;
        Ok(())
    }

    /// Request missing blobs from a specific peer via mailbox.
    async fn request_missing_blobs_from(&self, peer_device_id: &str) -> Result<()> {
        let missing = collect_missing_blob_hashes(&self.db, &self.app_data_dir).await?;
        if missing.is_empty() {
            return Ok(());
        }
        info!(count = missing.len(), to = %peer_device_id, "requesting missing blobs over mailbox");
        let msg = SyncMessage::AttachmentRequest { hashes: missing };
        self.deposit_message_to(peer_device_id, &msg).await
    }

    /// Handle an attachment message that arrived over mailbox: answer requests
    /// by depositing payloads back to the sender, and write received payloads.
    async fn handle_attachment_message_for(
        &self,
        from_device_id: &str,
        msg: SyncMessage,
    ) -> Result<()> {
        match msg {
            SyncMessage::AttachmentRequest { hashes } => {
                info!(count = hashes.len(), from = %from_device_id, "received mailbox blob request");
                for (hash, ext) in hashes {
                    if !blob_fits_mailbox(&self.app_data_dir, &hash, &ext) {
                        warn!(hash = %hash, "blob exceeds mailbox size limit; only P2P will carry it");
                        continue;
                    }
                    match read_blob_base64(&self.app_data_dir, &hash, &ext) {
                        Ok(Some(data)) => {
                            let payload = SyncMessage::AttachmentPayload {
                                hash: hash.clone(),
                                ext: ext.clone(),
                                data,
                            };
                            if let Err(e) = self.deposit_message_to(from_device_id, &payload).await
                            {
                                warn!(error = %e, hash = %hash, "failed to deposit blob payload");
                            }
                        }
                        Ok(None) => warn!(hash = %hash, "peer requested blob we do not have"),
                        Err(e) => warn!(error = %e, hash = %hash, "failed to read blob"),
                    }
                }
            }
            SyncMessage::AttachmentPayload { hash, ext, data } => {
                write_blob_from_base64(&self.app_data_dir, &hash, &ext, &data)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Retry delivering queued outbox messages; drop ones that succeed.
    /// The outbox stores the ciphertext base64-encoded (see `write_outbox`);
    /// the flush validates and re-sends it as-is — feeding the base64 text
    /// through `deposit_encrypted` would double-encode it and the peer could
    /// never decrypt it.
    pub async fn flush_outbox(&self) -> Result<()> {
        let Some(mailbox) = &self.mailbox else {
            return Ok(());
        };
        flush_outbox_with(&self.db, mailbox.relay()).await
    }

    async fn write_outbox(
        &self,
        to_device_id: &str,
        ciphertext: &[u8],
        nonce: &[u8],
        message_id: &str,
    ) -> Result<()> {
        write_outbox_row(&self.db, to_device_id, ciphertext, nonce, message_id).await
    }

    async fn refresh_outbox_count(&self) {
        let count: Result<i64, _> = sqlx::query_scalar("SELECT count(*) FROM sync_outbox")
            .fetch_one(&self.db)
            .await;
        if let Ok(n) = count {
            self.status.lock().await.outbox_pending = n;
        }
    }

    async fn set_transport(&self, transport: &str) {
        self.status.lock().await.transport = transport.to_string();
    }    /// Start continuous sync in the background.
    pub fn start(self: Arc<Self>) {
        let engine = self.clone();
        // Mark the session disconnected when the peer closes the channel, so
        // the UI stops showing "connected" and manual syncs fail fast with a
        // meaningful state instead of a dead-channel send error.
        self.session.on_close({
            let engine = engine.clone();
            move || {
                let engine = engine.clone();
                tokio::spawn(async move {
                    engine.status.lock().await.connected = false;
                    info!("sync engine connection closed by peer");
                });
            }
        });
        self.session.on_message(move |text| {
            let engine = engine.clone();
            tokio::spawn(async move {
                if engine.stopped.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                match serde_json::from_str::<SyncMessage>(&text) {
                    Ok(SyncMessage::Pull) => {
                        if let Err(e) = engine.push().await {
                            warn!(error = %e, "push failed in response to pull");
                        }
                    }
                    Ok(SyncMessage::Chunk {
                        id,
                        index,
                        total,
                        data,
                    }) => {
                        let complete = {
                            let mut asm = engine.assembler.lock().unwrap();
                            asm.push(&id, index, total, &data)
                        };
                        if let Some(bytes) = complete {
                            match serde_json::from_slice::<SyncMessage>(&bytes) {
                                Ok(SyncMessage::Changeset(cs)) => {
                                    if let Err(e) = engine.handle_changeset(cs).await {
                                        warn!(error = %e, "apply chunked changeset failed");
                                    }
                                }
                                Ok(SyncMessage::FullSnapshot { statements }) => {
                                    match crate::sync::crdt::apply_full_snapshot(&engine.db, &statements).await {
                                        Ok(applied) => {
                                            let applied = applied as i64;
                                            if applied > 0 {
                                                engine.status.lock().await.pulled += applied;
                                                crate::sync::emit_remote_applied(applied);
                                            }
                                            // A snapshot can bring in rows (e.g. note
                                            // Markdown referencing `blobs/...`) without a
                                            // following changeset; request the referenced
                                            // files explicitly or they never arrive.
                                            if let Err(e) = engine.request_missing_blobs().await {
                                                warn!(error = %e, "request missing blobs after chunked snapshot failed");
                                            }
                                        }
                                        Err(e) => {
                                            warn!(error = %e, "apply chunked snapshot failed");
                                        }
                                    }
                                }
                                Ok(SyncMessage::Pull) => {
                                    if let Err(e) = engine.push().await {
                                        warn!(error = %e, "push failed for chunked pull");
                                    }
                                }
                                Ok(
                                    msg @ (SyncMessage::AttachmentRequest { .. }
                                    | SyncMessage::AttachmentPayload { .. }),
                                ) => {
                                    if let Err(e) = engine.handle_attachment_message(msg).await {
                                        warn!(error = %e, "handle chunked attachment message failed");
                                    }
                                }
                                Ok(other) => warn!(msg = ?other, "unexpected chunked message"),
                                Err(e) => warn!(error = %e, "parse chunked message failed"),
                            }
                        }
                    }
                    Ok(SyncMessage::Changeset(cs)) => {
                        if let Err(e) = engine.handle_changeset(cs).await {
                            warn!(error = %e, "apply changeset failed");
                        }
                    }
                    Ok(SyncMessage::FullSnapshot { statements }) => {
                        match crate::sync::crdt::apply_full_snapshot(&engine.db, &statements).await {
                            Ok(applied) => {
                                let applied = applied as i64;
                                if applied > 0 {
                                    engine.status.lock().await.pulled += applied;
                                    crate::sync::emit_remote_applied(applied);
                                }
                                // Request blob files referenced by the snapshot
                                // (note images, paper PDFs) — see above.
                                if let Err(e) = engine.request_missing_blobs().await {
                                    warn!(error = %e, "request missing blobs after snapshot failed");
                                }
                            }
                            Err(e) => {
                                warn!(error = %e, "apply full snapshot failed");
                            }
                        }
                    }
                    Ok(
                        msg @ (SyncMessage::AttachmentRequest { .. }
                        | SyncMessage::AttachmentPayload { .. }),
                    ) => {
                        if let Err(e) = engine.handle_attachment_message(msg).await {
                            warn!(error = %e, "handle attachment message failed");
                        }
                    }
                    Err(e) => warn!(error = %e, text = %text, "failed to parse sync message"),
                }
            });
        });

        let engine = self.clone();
        self.session.on_close(move || {
            let engine = engine.clone();
            tokio::spawn(async move {
                engine.set_connected(false).await;
                engine.set_transport("none").await;
                warn!("sync data channel closed");
            });
        });

        // Route encrypted mailbox batches into the same changeset pipeline;
        // relay Error messages surface in the engine status.
        if let Some(mailbox) = &self.mailbox {
            let engine = self.clone();
            let err_engine = self.clone();
            mailbox.on_batch(
                move |messages| {
                    let engine = engine.clone();
                    tokio::spawn(async move {
                        for msg in messages {
                            if let Err(e) = engine.handle_mailbox_message(msg).await {
                                warn!(error = %e, "handle mailbox message failed");
                            }
                        }
                    });
                },
                move |err| {
                    let engine = err_engine.clone();
                    tokio::spawn(async move {
                        warn!(error = %err, "mailbox relay error");
                        engine.set_last_error(Some(format!("relay: {err}"))).await;
                    });
                },
            );
        }

        // Continuous incremental sync: while the session is alive, push local
        // changes to the peer on a fixed cadence and drain the encrypted
        // outbox. `push` is a no-op when there is nothing new, so an idle
        // session costs one cheap query per tick.
        {
            let engine = self.clone();
            tokio::spawn(async move {
                let mut ticker = tokio::time::interval(std::time::Duration::from_secs(
                    SYNC_PUSH_INTERVAL_SECS,
                ));
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    ticker.tick().await;
                    if engine.stopped.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    if !engine.status().await.connected {
                        // Marked disconnected (channel died / peer closed).
                        // Keep probing: only a message that actually crosses
                        // the wire proves the channel recovered (e.g. after a
                        // transient send failure). `push()` with nothing new
                        // returns Ok without sending anything, so it must not
                        // be used as the probe — on a dead channel it would
                        // flip the status back to "已连接" (host stopped, but
                        // the guest shows connected again). A `Pull` request is
                        // tiny and always hits the wire.
                        let probe = SyncMessage::Pull;
                        if let Err(e) = engine.send_message(&probe).await {
                            warn!(error = %e, "push probe failed; still disconnected");
                            continue;
                        }
                        engine.set_connected(true).await;
                        engine.set_last_error(None).await;
                        continue;
                    }
                    if let Err(e) = engine.push().await {
                        // A real send failure means the peer is unreachable —
                        // surface it immediately so the UI does not keep
                        // showing "已连接" while syncs fail.
                        warn!(error = %e, "periodic push failed; marking session disconnected");
                        engine.set_connected(false).await;
                        engine.set_last_error(Some(e.to_string())).await;
                        continue;
                    }
                    if engine.mailbox.is_some() {
                        if let Err(e) = engine.flush_outbox().await {
                            warn!(error = %e, "periodic outbox flush failed");
                        }
                    }
                }
            });
        }
    }
}

/// Outbox rows are abandoned after this many delivery attempts — a message
/// that fails 50 times is poison (undeliverable), and retrying it forever
/// would let the outbox grow without bound while the relay is unreachable.
const MAX_OUTBOX_RETRIES: i64 = 50;

/// Process-level outbox-flush backoff. When a flush hits a transport-level
/// failure (ack timeout / send failure), the next flush is held off for a
/// growing window (10s, doubling on each consecutive failure, capped at 5
/// minutes) instead of retransmitting the whole backlog on every proxy tick —
/// production burned ~1GB of uplink re-sending a 13.4MB outbox every 10s
/// while the relay was congested. In-memory only: there is a single relay
/// connection per process and congestion is a property of the connection,
/// not of any outbox row. Reset as soon as any row is acked (or a flush
/// completes without transport failures).
static FLUSH_BACKOFF: std::sync::Mutex<Option<FlushBackoff>> = std::sync::Mutex::new(None);

struct FlushBackoff {
    next_flush_at: std::time::Instant,
    current: std::time::Duration,
}

const FLUSH_BACKOFF_INITIAL: std::time::Duration = std::time::Duration::from_secs(10);
const FLUSH_BACKOFF_MAX: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// Whether the outbox flush is currently held off by the backoff window.
fn flush_backoff_active() -> bool {
    let guard = FLUSH_BACKOFF.lock().unwrap();
    match &*guard {
        Some(b) => std::time::Instant::now() < b.next_flush_at,
        None => false,
    }
}

/// Record a transport-level flush failure: start the backoff, or double it on
/// consecutive failures (capped at FLUSH_BACKOFF_MAX).
fn flush_backoff_record_failure() {
    let mut guard = FLUSH_BACKOFF.lock().unwrap();
    let current = guard
        .as_ref()
        .map(|b| (b.current * 2).min(FLUSH_BACKOFF_MAX))
        .unwrap_or(FLUSH_BACKOFF_INITIAL);
    *guard = Some(FlushBackoff {
        next_flush_at: std::time::Instant::now() + current,
        current,
    });
}

/// Clear the backoff (any acknowledged row, or a flush without transport
/// failures, proves the connection is healthy again).
fn flush_backoff_reset() {
    *FLUSH_BACKOFF.lock().unwrap() = None;
}

/// Drop outbox rows that can never be delivered: poison messages (too many
/// retries) and expired ones (the relay keeps mailbox messages for the same
/// TTL anyway, so older payloads would be dropped on arrival).
async fn prune_outbox(db: &SqlitePool) -> Result<()> {
    let poisoned = sqlx::query("DELETE FROM sync_outbox WHERE retry_count >= ?")
        .bind(MAX_OUTBOX_RETRIES)
        .execute(db)
        .await
        .context("prune poisoned outbox rows")?
        .rows_affected();
    if poisoned > 0 {
        warn!(count = poisoned, "dropped poisoned outbox messages");
    }

    let rows: Vec<(String, i64, String)> =
        sqlx::query_as("SELECT id, ttl_seconds, created_at FROM sync_outbox")
            .fetch_all(db)
            .await
            .context("read outbox for expiry sweep")?;
    let now = chrono::Utc::now();
    for (id, ttl_seconds, created_at) in rows {
        let expired = chrono::DateTime::parse_from_rfc3339(&created_at)
            .map(|t| t.with_timezone(&chrono::Utc) + chrono::Duration::seconds(ttl_seconds) < now)
            .unwrap_or(false);
        if expired {
            sqlx::query("DELETE FROM sync_outbox WHERE id = ?")
                .bind(&id)
                .execute(db)
                .await
                .context("drop expired outbox row")?;
            info!(id = %id, "dropped expired outbox message");
        }
    }
    Ok(())
}

/// Deliver queued outbox messages through the given relay connection; drop
/// rows that are delivered, bump `retry_count` on explicit rejection. A dead
/// transport (send failure / ack timeout) does NOT count against the message
/// — it is not poison, just early; the row's TTL bounds how long it can
/// linger. Free function so the command layer can flush without a live
/// engine.
///
/// The stored ciphertext/nonce are already base64 in the exact form the
/// deposit payload expects; they are only validated here. Re-sending reuses
/// the stored `message_id`, so the relay dedupes retransmits of a deposit it
/// already stored (idempotent retry).
pub async fn flush_outbox_with(
    db: &SqlitePool,
    relay: &crate::sync::relay_client::RelayClient,
) -> Result<()> {
    // Backoff window after a transport-level failure: fast-return instead of
    // re-sending the whole backlog on every tick while the relay is down.
    if flush_backoff_active() {
        debug!("outbox flush skipped: transport backoff active");
        return Ok(());
    }
    prune_outbox(db).await?;
    let rows: Vec<(String, String, String, String, i64, Option<String>, i64)> = sqlx::query_as(
        "SELECT id, to_device_id, ciphertext, nonce, ttl_seconds, message_id, retry_count FROM sync_outbox ORDER BY created_at LIMIT 50",
    )
    .fetch_all(db)
    .await
    .context("read outbox")?;
    let mut any_acked = false;
    let mut transport_failed = false;
    for (id, to_device_id, ciphertext, nonce, ttl_seconds, message_id, _retries) in rows {
        if base64::engine::general_purpose::STANDARD.decode(&ciphertext).is_err()
            || base64::engine::general_purpose::STANDARD.decode(&nonce).is_err()
        {
            warn!(id = %id, "outbox row is not valid base64; dropping");
            sqlx::query("DELETE FROM sync_outbox WHERE id = ?")
                .bind(&id)
                .execute(db)
                .await?;
            continue;
        }
        // Rows written before the message_id column existed get a stable id
        // now, so later flushes retry with the SAME id.
        let message_id = match message_id {
            Some(m) => m,
            None => {
                let m = uuid::Uuid::new_v4().to_string();
                sqlx::query("UPDATE sync_outbox SET message_id = ? WHERE id = ?")
                    .bind(&m)
                    .bind(&id)
                    .execute(db)
                    .await?;
                m
            }
        };
        let ack_result = relay
            .deposit_await_ack(
                crate::sync::types::MailboxDepositPayload {
                    to_device_id: to_device_id.clone(),
                    ciphertext,
                    nonce,
                    ttl_seconds: Some(ttl_seconds.max(0) as u64),
                    message_id: Some(message_id),
                },
                MAILBOX_ACK_TIMEOUT,
            )
            .await;
        match ack_result {
            Ok(()) => {
                sqlx::query("DELETE FROM sync_outbox WHERE id = ?")
                    .bind(&id)
                    .execute(db)
                    .await?;
                any_acked = true;
                info!(id = %id, to = %to_device_id, "outbox message delivered (acked)");
            }
            Err(crate::sync::relay_client::AckError::Rejected(e)) => {
                // Explicitly rejected (e.g. per-device target not in the room):
                // keep the row for a later retry — the peer may join later.
                sqlx::query("UPDATE sync_outbox SET retry_count = retry_count + 1 WHERE id = ?")
                    .bind(&id)
                    .execute(db)
                    .await?;
                warn!(id = %id, error = %e, "outbox deposit rejected; will retry");
            }
            Err(e @ (crate::sync::relay_client::AckError::TimedOut
                | crate::sync::relay_client::AckError::SendFailed(_))) => {
                // Unconfirmed: the transport is likely down. Stop the flush —
                // every remaining row would only time out again, and stalling
                // here blocks the sync loop. The retry count is NOT bumped:
                // a transport outage does not make the message poison. The
                // process-level backoff below keeps the next flush from
                // re-sending the whole backlog on the very next tick.
                warn!(id = %id, error = %e, "outbox deposit unconfirmed; pausing flush");
                transport_failed = true;
                break;
            }
        }
    }
    if any_acked || !transport_failed {
        flush_backoff_reset();
    } else {
        flush_backoff_record_failure();
    }
    Ok(())
}

/// Encrypt and deposit local changes for `to_device_id` into its mailbox via
/// `relay`. Resumes from the peer's persisted cursor and advances it on
/// success; on a dead transport the changeset is queued into the outbox.
/// Used by the auto-sync proxy so changes reach a peer that is currently
/// offline — the relay stores them until the peer next connects.
pub async fn deliver_changes_mailbox(
    db: &SqlitePool,
    relay: &crate::sync::relay_client::RelayClient,
    key: &[u8; crate::sync::crypto::SYNC_KEY_LEN],
    to_device_id: &str,
    peer_key: &str,
) -> Result<i64> {
    use base64::Engine as _;
    let (since, _) = load_peer_progress(db, peer_key).await;
    // Backpressure: an undelivered outbox row for this target means a previous
    // deposit failed. Re-exporting the same (never-advanced) range and
    // queueing another copy on every tick would grow the outbox without bound
    // while the relay is unreachable — `flush_outbox_with` retries the queued
    // row instead.
    let pending: i64 = sqlx::query_scalar("SELECT count(*) FROM sync_outbox WHERE to_device_id = ?")
        .bind(to_device_id)
        .fetch_one(db)
        .await
        .context("check outbox backpressure")?;
    if pending > 0 {
        return Ok(since);
    }
    let changes = export_own_changes_since(db, since).await?;
    if changes.changes.is_empty() {
        return Ok(since);
    }
    let delivered_to = changes.to_db_version;
    let json = serde_json::to_string(&SyncMessage::Changeset(changes)).context("serialize")?;
    let (ciphertext, nonce) =
        crate::sync::crypto::encrypt_bytes(key, json.as_bytes()).map_err(anyhow::Error::msg)?;
    // The message_id is fixed BEFORE the deposit so an outbox retry reuses it
    // (relay-side dedupe makes the retry idempotent even when the original
    // deposit was stored but its ack was lost).
    let message_id = uuid::Uuid::new_v4().to_string();
    let payload = crate::sync::types::MailboxDepositPayload {
        to_device_id: to_device_id.to_string(),
        ciphertext: base64::engine::general_purpose::STANDARD.encode(&ciphertext),
        nonce: base64::engine::general_purpose::STANDARD.encode(&nonce),
        ttl_seconds: Some(7 * 24 * 3600),
        message_id: Some(message_id.clone()),
    };
    // Advance the cursor ONLY after the relay acknowledges the deposit is
    // durably stored. A rejected deposit (e.g. per-device target not in room)
    // or a missing ack (dead transport) queues the changeset for retry instead
    // of silently losing it behind an advanced watermark.
    match relay.deposit_await_ack(payload, MAILBOX_ACK_TIMEOUT).await {
        Ok(()) => {
            let _ = crate::core::settings_service::set_device_setting(
                db,
                &sent_cursor_key(peer_key),
                &delivered_to.to_string(),
            )
            .await;
            info!(to = %to_device_id, to_db_version = delivered_to, "changeset deposited to mailbox (offline, acked)");
            Ok(delivered_to)
        }
        Err(e) => {
            warn!(to = %to_device_id, error = %e, "mailbox deposit unacknowledged; queuing to outbox");
            write_outbox_row(db, to_device_id, &ciphertext, &nonce, &message_id).await?;
            Ok(since)
        }
    }
}

/// Serialized mailbox snapshot larger than this is skipped (P2P covers those
/// libraries); the relay has no frame limit, but a multi-MB single frame is
/// wasteful for what is only a history-fill safety net.
const MAX_MAILBOX_SNAPSHOT_BYTES: usize = 12 * 1024 * 1024;

/// Export the full change history (everything in `crsql_changes` since
/// db_version 0 — including tombstones, which CR-SQLite backfills for
/// pre-CRR rows at registration), encrypt it as a regular changeset, and
/// deposit it into the ACCOUNT-LEVEL archive so any device of the account —
/// including one that never had a P2P session — can pick up the complete
/// library and rows the archive may have pruned. The receiver applies it
/// through the same cr-sqlite merge as incremental sync (delete-wins
/// preserved), so refreshing the archive can no longer resurrect rows a peer
/// deleted. Deliberately does NOT touch `sync.cursor.sent.*`: the delta
/// cursor is managed exclusively by `deliver_changes_mailbox`.
pub async fn deliver_full_snapshot_mailbox(
    db: &SqlitePool,
    relay: &crate::sync::relay_client::RelayClient,
    key: &[u8; crate::sync::crypto::SYNC_KEY_LEN],
) -> Result<()> {
    use base64::Engine as _;
    let changes = export_changes_since(db, 0).await?;
    if changes.changes.is_empty() {
        return Ok(());
    }
    let json = serde_json::to_string(&SyncMessage::Changeset(changes))?;
    if json.len() > MAX_MAILBOX_SNAPSHOT_BYTES {
        warn!(
            bytes = json.len(),
            "full-history changeset too large for mailbox; skipping (P2P will carry it)"
        );
        return Ok(());
    }
    let (ciphertext, nonce) =
        crate::sync::crypto::encrypt_bytes(key, json.as_bytes()).map_err(anyhow::Error::msg)?;
    relay
        .send(crate::sync::types::RelayClientMsg::MailboxDeposit {
            payload: crate::sync::types::MailboxDepositPayload {
                to_device_id: String::new(), // account-level archive
                ciphertext: base64::engine::general_purpose::STANDARD.encode(&ciphertext),
                nonce: base64::engine::general_purpose::STANDARD.encode(&nonce),
                ttl_seconds: Some(7 * 24 * 3600),
                message_id: Some(uuid::Uuid::new_v4().to_string()),
            },
        })
        .context("deposit full-history changeset to account archive")?;
    info!(bytes = json.len(), "full-history changeset deposited to account archive");
    Ok(())
}

/// Decrypt one mailbox message into its wire envelope (no side effects).
pub async fn decrypt_mailbox_message(
    key: &[u8; crate::sync::crypto::SYNC_KEY_LEN],
    msg: &crate::sync::types::MailboxMessage,
) -> Result<SyncMessage> {
    use base64::Engine as _;
    let nonce = base64::engine::general_purpose::STANDARD
        .decode(&msg.nonce)
        .context("decode nonce")?;
    let ciphertext = base64::engine::general_purpose::STANDARD
        .decode(&msg.ciphertext)
        .context("decode ciphertext")?;
    let plaintext = crate::sync::crypto::decrypt_bytes(key, &nonce, &ciphertext)
        .map_err(anyhow::Error::msg)?;
    serde_json::from_slice(&plaintext).context("parse changeset")
}

/// Decrypt one mailbox message and apply its changeset locally. Returns the
/// parsed envelope so callers can react to `Pull` requests.
pub async fn decrypt_and_apply_mailbox_message(
    db: &SqlitePool,
    key: &[u8; crate::sync::crypto::SYNC_KEY_LEN],
    msg: &crate::sync::types::MailboxMessage,
) -> Result<SyncMessage> {
    let envelope = decrypt_mailbox_message(key, msg).await?;
    if let SyncMessage::Changeset(cs) = &envelope {
        apply_changes(db, cs).await?;
    }
    Ok(envelope)
}

/// device_settings key recording the highest db_version already applied from
/// account-level archive messages sent by a given device.
pub fn account_applied_cursor_key(from_device_id: &str) -> String {
    format!("sync.cursor.applied.account.{from_device_id}")
}

/// Encrypt and deposit a sync message for a specific peer through a relay
/// connection. Used by the mailbox-only path (no live P2P DataChannel).
async fn deposit_sync_message_to(
    relay: &crate::sync::relay_client::RelayClient,
    key: &[u8; crate::sync::crypto::SYNC_KEY_LEN],
    to_device_id: &str,
    msg: &SyncMessage,
) -> Result<()> {
    use base64::Engine as _;
    let json = serde_json::to_string(msg).context("serialize mailbox sync message")?;
    let (ciphertext, nonce) =
        crate::sync::crypto::encrypt_bytes(key, json.as_bytes()).map_err(anyhow::Error::msg)?;
    relay
        .send(crate::sync::types::RelayClientMsg::MailboxDeposit {
            payload: crate::sync::types::MailboxDepositPayload {
                to_device_id: to_device_id.to_string(),
                ciphertext: base64::engine::general_purpose::STANDARD.encode(&ciphertext),
                nonce: base64::engine::general_purpose::STANDARD.encode(&nonce),
                ttl_seconds: Some(7 * 24 * 3600),
                message_id: Some(uuid::Uuid::new_v4().to_string()),
            },
        })
        .context("deposit sync message to mailbox")
}

/// Decrypt and apply a batch of mailbox messages, then acknowledge them.
/// `Pull` requests are answered by depositing our changeset back through the
/// same relay connection. Account-level messages (the offline archive shared
/// by every device of the account) are filtered by a per-sender applied
/// cursor so repeated fetches are no-ops. Used by the auto-sync proxy so a
/// freshly logged-in device picks up changes its peers deposited while it was
/// offline — no P2P session required.
pub async fn handle_mailbox_batch(
    db: &SqlitePool,
    app_data_dir: &std::path::Path,
    key: &[u8; crate::sync::crypto::SYNC_KEY_LEN],
    relay: &crate::sync::relay_client::RelayClient,
    messages: Vec<crate::sync::types::MailboxMessage>,
) -> Result<()> {
    let mut ack_ids = Vec::with_capacity(messages.len());
    let mut applied_changes: i64 = 0;
    for mb_msg in messages {
        ack_ids.push(mb_msg.id.clone());
        let envelope = match decrypt_mailbox_message(key, &mb_msg).await {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, from = %mb_msg.from_device_id, "mailbox message decrypt failed");
                continue;
            }
        };
        match envelope {
            SyncMessage::Changeset(cs) => {
                if mb_msg.account_level {
                    // Account-level archive: skip what we already applied from
                    // this sender, then record the new watermark.
                    let cursor_key = account_applied_cursor_key(&mb_msg.from_device_id);
                    let applied: i64 =
                        crate::core::settings_service::get_device_setting(db, &cursor_key)
                            .await
                            .ok()
                            .flatten()
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(0);
                    if cs.to_db_version <= applied {
                        continue;
                    }
                    applied_changes += apply_changes(db, &cs).await? as i64;
                    let _ = crate::core::settings_service::set_device_setting(
                        db,
                        &cursor_key,
                        &cs.to_db_version.to_string(),
                    )
                    .await;
                    info!(from = %mb_msg.from_device_id, to_db_version = cs.to_db_version, "applied account-level mailbox changeset");
                } else {
                    applied_changes += apply_changes(db, &cs).await? as i64;
                    info!(from = %mb_msg.from_device_id, "applied per-device mailbox changeset");
                }
                // The mailbox path carries row changes, not blob files. Ask the
                // sender for any referenced blobs (paper PDFs, note images).
                if let Err(e) = request_missing_blobs_over_relay(
                    db,
                    app_data_dir,
                    key,
                    relay,
                    &mb_msg.from_device_id,
                )
                .await
                {
                    warn!(error = %e, from = %mb_msg.from_device_id, "mailbox blob request failed");
                }
            }
            SyncMessage::Pull => {
                // Peer asked for our changes over the mailbox; reply by
                // depositing our changeset back (per-device to the requester).
                if let Err(e) =
                    deliver_changes_mailbox(db, relay, key, &mb_msg.from_device_id, &mb_msg.from_device_id)
                        .await
                {
                    warn!(error = %e, "mailbox pull reply failed");
                }
            }
            SyncMessage::FullSnapshot { statements } => {
                // Full-snapshot delivery through the account archive: covers
                // pre-CRR history (rows that never appear in crsql_changes) so
                // a device that only ever syncs via the mailbox still receives
                // the complete library. INSERT OR IGNORE is idempotent.
                match crate::sync::crdt::apply_full_snapshot(db, &statements).await {
                    Ok(applied) => {
                        // Count only rows actually inserted — a re-applied
                        // archive snapshot IGNOREs almost every row, and
                        // counting it would spam a full frontend reload.
                        applied_changes += applied as i64;
                        info!(count = applied, "applied account-level full snapshot");
                        // Snapshot rows may reference blobs (paper PDFs, note
                        // images) the mailbox does not carry — ask the sender.
                        if let Err(e) = request_missing_blobs_over_relay(
                            db,
                            app_data_dir,
                            key,
                            relay,
                            &mb_msg.from_device_id,
                        )
                        .await
                        {
                            warn!(error = %e, from = %mb_msg.from_device_id, "mailbox blob request after snapshot failed");
                        }
                    }
                    Err(e) => warn!(error = %e, "apply account-level full snapshot failed"),
                }
            }
            attachment_msg @ (SyncMessage::AttachmentRequest { .. } | SyncMessage::AttachmentPayload { .. }) => {
                if let Err(e) = handle_mailbox_attachment_message(
                    app_data_dir,
                    key,
                    relay,
                    &mb_msg.from_device_id,
                    attachment_msg,
                )
                .await
                {
                    warn!(error = %e, from = %mb_msg.from_device_id, "mailbox attachment message failed");
                }
            }
            other => warn!(msg = ?other, "ignoring unsupported mailbox message"),
        }
    }
    if !ack_ids.is_empty() {
        let _ = relay.send(crate::sync::types::RelayClientMsg::MailboxAck {
            payload: crate::sync::types::MailboxAckPayload {
                message_ids: ack_ids,
            },
        });
    }
    // One notification per batch; the frontend debounces and reloads.
    if applied_changes > 0 {
        crate::sync::emit_remote_applied(applied_changes);
    }
    Ok(())
}

/// Request blobs referenced by synced rows from a specific peer via mailbox.
async fn request_missing_blobs_over_relay(
    db: &SqlitePool,
    app_data_dir: &std::path::Path,
    key: &[u8; crate::sync::crypto::SYNC_KEY_LEN],
    relay: &crate::sync::relay_client::RelayClient,
    peer_device_id: &str,
) -> Result<()> {
    let missing = collect_missing_blob_hashes(db, app_data_dir).await?;
    if missing.is_empty() {
        return Ok(());
    }
    info!(count = missing.len(), to = %peer_device_id, "requesting missing blobs over mailbox");
    let msg = SyncMessage::AttachmentRequest { hashes: missing };
    deposit_sync_message_to(relay, key, peer_device_id, &msg).await
}

/// Handle an attachment request/payload that arrived over the mailbox path.
async fn handle_mailbox_attachment_message(
    app_data_dir: &std::path::Path,
    key: &[u8; crate::sync::crypto::SYNC_KEY_LEN],
    relay: &crate::sync::relay_client::RelayClient,
    from_device_id: &str,
    msg: SyncMessage,
) -> Result<()> {
    match msg {
        SyncMessage::AttachmentRequest { hashes } => {
            info!(count = hashes.len(), from = %from_device_id, "received mailbox blob request");
            for (hash, ext) in hashes {
                if !blob_fits_mailbox(app_data_dir, &hash, &ext) {
                    warn!(hash = %hash, "blob exceeds mailbox size limit; only P2P will carry it");
                    continue;
                }
                match read_blob_base64(app_data_dir, &hash, &ext) {
                    Ok(Some(data)) => {
                        let payload = SyncMessage::AttachmentPayload {
                            hash: hash.clone(),
                            ext: ext.clone(),
                            data,
                        };
                        if let Err(e) =
                            deposit_sync_message_to(relay, key, from_device_id, &payload).await
                        {
                            warn!(error = %e, hash = %hash, "failed to deposit blob payload");
                        }
                    }
                    Ok(None) => warn!(hash = %hash, "peer requested blob we do not have"),
                    Err(e) => warn!(error = %e, hash = %hash, "failed to read blob"),
                }
            }
        }
        SyncMessage::AttachmentPayload { hash, ext, data } => {
            write_blob_from_base64(app_data_dir, &hash, &ext, &data)?;
        }
        _ => {}
    }
    Ok(())
}

/// Queue an encrypted changeset into the local outbox for later delivery.
/// `message_id` is the id of the original deposit attempt; the flush path
/// reuses it so the relay dedupes retransmits (idempotent retry).
/// Free function so the command layer (offline mailbox fallback) can enqueue
/// without a live engine.
pub async fn write_outbox_row(
    db: &SqlitePool,
    to_device_id: &str,
    ciphertext: &[u8],
    nonce: &[u8],
    message_id: &str,
) -> Result<()> {
    use base64::Engine as _;
    sqlx::query(
        "INSERT INTO sync_outbox (id, to_device_id, ciphertext, nonce, ttl_seconds, created_at, message_id)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(to_device_id)
    .bind(base64::engine::general_purpose::STANDARD.encode(ciphertext))
    .bind(base64::engine::general_purpose::STANDARD.encode(nonce))
    .bind(7 * 24 * 3600i64)
    .bind(crate::core::time::now_iso())
    .bind(message_id)
    .execute(db)
    .await
    .context("write outbox")?;
    Ok(())
}

impl SyncMessage {
    fn to_db_version(&self) -> Option<i64> {
        match self {
            SyncMessage::Changeset(cs) => Some(cs.to_db_version),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::{
        register_crr_tables, tests::connect_with_crsqlite, CORE_SYNC_TABLES, SCHEMA_INIT_SQL,
    };
    use anyhow::Context;
    use std::time::Duration;
    use tokio::sync::mpsc;
    use tokio::time::timeout;
    use webrtc::api::interceptor_registry::register_default_interceptors;
    use webrtc::api::media_engine::MediaEngine;
    use webrtc::api::setting_engine::SettingEngine;
    use webrtc::api::APIBuilder;
    use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
    use webrtc::ice_transport::ice_server::RTCIceServer;
    use webrtc::peer_connection::configuration::RTCConfiguration;
    use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
    use webrtc::peer_connection::RTCPeerConnection;

    /// Serializes tests that call `flush_outbox_with`: the flush backoff is
    /// process-global state, so a test that engages it must not overlap with
    /// other tests' flushes (they would fast-return and never deliver).
    static FLUSH_TEST_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    async fn create_test_db() -> anyhow::Result<(SqlitePool, PathBuf)> {
        let dir = std::env::temp_dir().join(format!(
            "siku-engine-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("test.db");
        let db = connect_with_crsqlite(&path).await?;
        sqlx::query(SCHEMA_INIT_SQL).execute(&db).await?;
        register_crr_tables(&db, CORE_SYNC_TABLES).await?;
        Ok((db, dir))
    }

    async fn create_peer_connection() -> anyhow::Result<Arc<RTCPeerConnection>> {
        let mut m = MediaEngine::default();
        m.register_default_codecs()?;
        let mut registry = webrtc::interceptor::registry::Registry::new();
        registry = register_default_interceptors(registry, &mut m)?;
        let api = APIBuilder::new()
            .with_media_engine(m)
            .with_interceptor_registry(registry)
            .with_setting_engine(SettingEngine::default())
            .build();
        let config = RTCConfiguration {
            ice_servers: vec![RTCIceServer {
                urls: vec!["stun:stun.l.google.com:19302".to_string()],
                ..Default::default()
            }],
            ..Default::default()
        };
        Ok(Arc::new(api.new_peer_connection(config).await?))
    }

    async fn connect_sessions() -> anyhow::Result<(SyncSession, SyncSession)> {
        let pc_a = create_peer_connection().await?;
        let pc_b = create_peer_connection().await?;

        let dc_a = pc_a
            .create_data_channel("siku-sync", None)
            .await
            .context("create data channel")?;

        let (dc_b_tx, mut dc_b_rx) =
            mpsc::unbounded_channel::<Arc<webrtc::data_channel::RTCDataChannel>>();
        pc_b.on_data_channel(Box::new(move |d| {
            let tx = dc_b_tx.clone();
            Box::pin(async move {
                let _ = tx.send(d);
            })
        }));

        // Exchange SDP.
        let offer = pc_a.create_offer(None).await.context("create offer")?;
        pc_a.set_local_description(offer.clone()).await?;
        pc_b.set_remote_description(RTCSessionDescription::offer(offer.sdp)?)
            .await?;
        let answer = pc_b.create_answer(None).await.context("create answer")?;
        pc_b.set_local_description(answer.clone()).await?;
        pc_a.set_remote_description(RTCSessionDescription::answer(answer.sdp)?)
            .await?;

        // Exchange ICE candidates.
        let (ice_a_tx, mut ice_a_rx) = mpsc::unbounded_channel::<RTCIceCandidateInit>();
        pc_a.on_ice_candidate(Box::new(move |c| {
            let tx = ice_a_tx.clone();
            Box::pin(async move {
                if let Some(c) = c {
                    if let Ok(init) = c.to_json() {
                        let _ = tx.send(init);
                    }
                }
            })
        }));
        let (ice_b_tx, mut ice_b_rx) = mpsc::unbounded_channel::<RTCIceCandidateInit>();
        pc_b.on_ice_candidate(Box::new(move |c| {
            let tx = ice_b_tx.clone();
            Box::pin(async move {
                if let Some(c) = c {
                    if let Ok(init) = c.to_json() {
                        let _ = tx.send(init);
                    }
                }
            })
        }));

        let pc_a2 = pc_a.clone();
        let pc_b2 = pc_b.clone();
        tokio::spawn(async move {
            while let Some(init) = ice_a_rx.recv().await {
                let _ = pc_b2.add_ice_candidate(init).await;
            }
        });
        tokio::spawn(async move {
            while let Some(init) = ice_b_rx.recv().await {
                let _ = pc_a2.add_ice_candidate(init).await;
            }
        });

        // Wait for data channel to open on A and be received on B.
        let (open_tx, mut open_rx) = mpsc::channel::<()>(1);
        dc_a.on_open(Box::new(move || {
            let _ = open_tx.try_send(());
            Box::pin(async {})
        }));

        timeout(Duration::from_secs(30), open_rx.recv())
            .await
            .context("data channel open timeout")?
            .context("open channel closed")?;

        let dc_b = timeout(Duration::from_secs(5), dc_b_rx.recv())
            .await
            .context("wait for data channel on B")?
            .context("dc channel closed")?;

        Ok((
            SyncSession { pc: pc_a, dc: dc_a },
            SyncSession { pc: pc_b, dc: dc_b },
        ))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sync_engine_exchanges_changes_over_datachannel() -> anyhow::Result<()> {
        let (sess_a, sess_b) = connect_sessions().await?;
        let (db_a, dir_a) = create_test_db().await?;
        let (db_b, dir_b) = create_test_db().await?;

        let engine_a = Arc::new(SyncEngine::new(
            Arc::new(sess_a),
            db_a.clone(),
            dir_a.clone(),
            None,
        ));
        let engine_b = Arc::new(SyncEngine::new(
            Arc::new(sess_b),
            db_b.clone(),
            dir_b.clone(),
            None,
        ));
        engine_a.clone().start();
        engine_b.clone().start();

        // Insert a paper and note on A.
        sqlx::query(
            "INSERT INTO papers (id, title, created_at, updated_at, imported_at) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind("p1")
        .bind("Paper One")
        .bind("2026-01-01T00:00:00Z")
        .bind("2026-01-01T00:00:00Z")
        .bind("2026-01-01T00:00:00Z")
        .execute(&db_a)
        .await?;
        sqlx::query(
            "INSERT INTO notes (id, vault_id, title, content, content_plain, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)"
        )
        .bind("n1")
        .bind(1i64)
        .bind("Note One")
        .bind("Hello **world**")
        .bind("Hello world")
        .bind("2026-01-01T00:00:00Z")
        .bind("2026-01-01T00:00:00Z")
        .execute(&db_a)
        .await?;

        engine_a.sync_once().await?;

        // Give async message handlers time to run.
        tokio::time::sleep(Duration::from_millis(800)).await;

        let paper_count: (i64,) = sqlx::query_as("SELECT count(*) FROM papers WHERE id = 'p1'")
            .fetch_one(&db_b)
            .await?;
        assert_eq!(paper_count.0, 1, "paper should be synced to B");
        let note_count: (i64,) = sqlx::query_as("SELECT count(*) FROM notes WHERE id = 'n1'")
            .fetch_one(&db_b)
            .await?;
        assert_eq!(note_count.0, 1, "note should be synced to B");

        sqlx::query("SELECT crsql_finalize()")
            .execute(&db_a)
            .await?;
        sqlx::query("SELECT crsql_finalize()")
            .execute(&db_b)
            .await?;
        db_a.close().await;
        db_b.close().await;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sync_engine_transfers_missing_blobs() -> anyhow::Result<()> {
        let (sess_a, sess_b) = connect_sessions().await?;
        let (db_a, dir_a) = create_test_db().await?;
        let (db_b, dir_b) = create_test_db().await?;

        let engine_a = Arc::new(SyncEngine::new(
            Arc::new(sess_a),
            db_a.clone(),
            dir_a.clone(),
            None,
        ));
        let engine_b = Arc::new(SyncEngine::new(
            Arc::new(sess_b),
            db_b.clone(),
            dir_b.clone(),
            None,
        ));
        engine_a.clone().start();
        engine_b.clone().start();

        // Create a blob on A and reference it from a paper.
        let blob_bytes = b"fake pdf content".to_vec();
        let rel_path = crate::file_store::write_blob(&dir_a, &blob_bytes, "pdf")?;
        let hash = crate::file_store::sha256_hex(&blob_bytes);

        sqlx::query(
            "INSERT INTO papers (id, title, file_path, created_at, updated_at, imported_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind("p1")
        .bind("Paper One")
        .bind(&rel_path)
        .bind("2026-01-01T00:00:00Z")
        .bind("2026-01-01T00:00:00Z")
        .bind("2026-01-01T00:00:00Z")
        .execute(&db_a)
        .await?;

        engine_a.sync_once().await?;

        // Wait for changeset + blob request + blob payload.
        tokio::time::sleep(Duration::from_millis(1500)).await;

        let paper_count: (i64,) = sqlx::query_as("SELECT count(*) FROM papers WHERE id = 'p1'")
            .fetch_one(&db_b)
            .await?;
        assert_eq!(paper_count.0, 1, "paper metadata should be synced to B");

        let blob_exists = crate::file_store::has_blob(&dir_b, &hash);
        assert!(blob_exists, "blob should be transferred to B");

        sqlx::query("SELECT crsql_finalize()")
            .execute(&db_a)
            .await?;
        sqlx::query("SELECT crsql_finalize()")
            .execute(&db_b)
            .await?;
        db_a.close().await;
        db_b.close().await;
        Ok(())
    }

    /// Real PDFs are hundreds of KB to several MB — far above the 16KB SCTP
    /// datagram limit, so blob payloads must be split into chunks and
    /// reassembled on the receiving side. The small-blob test above only
    /// exercises the direct-send path; this one proves the chunked path
    /// transfers a large blob byte-for-byte.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sync_engine_transfers_large_blob_in_chunks() -> anyhow::Result<()> {
        let (sess_a, sess_b) = connect_sessions().await?;
        let (db_a, dir_a) = create_test_db().await?;
        let (db_b, dir_b) = create_test_db().await?;

        let engine_a = Arc::new(SyncEngine::new(
            Arc::new(sess_a),
            db_a.clone(),
            dir_a.clone(),
            None,
        ));
        let engine_b = Arc::new(SyncEngine::new(
            Arc::new(sess_b),
            db_b.clone(),
            dir_b.clone(),
            None,
        ));
        engine_a.clone().start();
        engine_b.clone().start();

        // ~300KB pseudo-PDF: representative of a real paper, guaranteed to
        // exceed MAX_WIRE_MSG and exercise the chunker.
        let blob_bytes: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let rel_path = crate::file_store::write_blob(&dir_a, &blob_bytes, "pdf")?;
        let hash = crate::file_store::sha256_hex(&blob_bytes);
        assert!(blob_bytes.len() > super::MAX_WIRE_MSG, "test must exceed the chunk threshold");

        sqlx::query(
            "INSERT INTO papers (id, title, file_path, created_at, updated_at, imported_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind("p-big")
        .bind("Big Paper")
        .bind(&rel_path)
        .bind("2026-01-01T00:00:00Z")
        .bind("2026-01-01T00:00:00Z")
        .bind("2026-01-01T00:00:00Z")
        .execute(&db_a)
        .await?;

        engine_a.sync_once().await?;

        // Wait for changeset + blob request + chunked blob payloads.
        tokio::time::sleep(Duration::from_millis(3000)).await;

        let paper_count: (i64,) = sqlx::query_as("SELECT count(*) FROM papers WHERE id = 'p-big'")
            .fetch_one(&db_b)
            .await?;
        assert_eq!(paper_count.0, 1, "paper metadata should be synced to B");

        assert!(
            crate::file_store::has_blob(&dir_b, &hash),
            "large blob should be transferred to B"
        );
        let received = std::fs::read(crate::file_store::blob_path(&dir_b, &hash, "pdf"))?;
        assert_eq!(
            received,
            blob_bytes,
            "chunked transfer must reproduce the blob byte-for-byte"
        );

        sqlx::query("SELECT crsql_finalize()")
            .execute(&db_a)
            .await?;
        sqlx::query("SELECT crsql_finalize()")
            .execute(&db_b)
            .await?;
        db_a.close().await;
        db_b.close().await;
        Ok(())
    }

    /// Note images are embedded in note Markdown as `![...](blobs/<hash>.png)`
    /// and must be transferred along with the note, or synced devices show
    /// broken images.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sync_engine_transfers_note_image_blob() -> anyhow::Result<()> {
        let (sess_a, sess_b) = connect_sessions().await?;
        let (db_a, dir_a) = create_test_db().await?;
        let (db_b, dir_b) = create_test_db().await?;

        let engine_a = Arc::new(SyncEngine::new(
            Arc::new(sess_a),
            db_a.clone(),
            dir_a.clone(),
            None,
        ));
        let engine_b = Arc::new(SyncEngine::new(
            Arc::new(sess_b),
            db_b.clone(),
            dir_b.clone(),
            None,
        ));
        engine_a.clone().start();
        engine_b.clone().start();

        // A note on A references a pasted image stored in the blob store.
        let img_bytes: Vec<u8> = (0..40_000u32).map(|i| (i % 200) as u8).collect();
        let rel_path = crate::file_store::write_blob(&dir_a, &img_bytes, "png")?;
        let hash = crate::file_store::sha256_hex(&img_bytes);
        let content = format!("![Pasted image]({rel_path})");

        sqlx::query(
            "INSERT INTO notes (id, vault_id, title, content, content_plain, tags, aliases, created_at, updated_at) \
             VALUES ('n1', 1, 'note-with-img', ?, ?, '[]', '[]', ?, ?)",
        )
        .bind(&content)
        .bind(&content)
        .bind("2026-01-01T00:00:00Z")
        .bind("2026-01-01T00:00:00Z")
        .execute(&db_a)
        .await?;

        engine_a.sync_once().await?;

        // Wait for note changeset + blob request + blob payload.
        tokio::time::sleep(Duration::from_millis(2500)).await;

        let note_count: (i64,) = sqlx::query_as("SELECT count(*) FROM notes WHERE id = 'n1'")
            .fetch_one(&db_b)
            .await?;
        assert_eq!(note_count.0, 1, "note should be synced to B");

        assert!(
            crate::file_store::has_blob(&dir_b, &hash),
            "note image blob should be transferred to B"
        );
        let received = std::fs::read(crate::file_store::blob_path(&dir_b, &hash, "png"))?;
        assert_eq!(received, img_bytes, "note image must be byte-identical");

        sqlx::query("SELECT crsql_finalize()")
            .execute(&db_a)
            .await?;
        sqlx::query("SELECT crsql_finalize()")
            .execute(&db_b)
            .await?;
        db_a.close().await;
        db_b.close().await;
        Ok(())
    }

    /// Offline delivery: A encrypts a changeset into a mailbox message; B
    /// decrypts and applies it without any live P2P session. This is the core
    /// of the "edit on device A while B is offline, then B logs in and gets
    /// the changes" flow.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn mailbox_message_decrypt_and_apply() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "siku-mailbox-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;

        let db_a_path = dir.join("a.db");
        let db_b_path = dir.join("b.db");
        let db_a = crate::core::db::tests::connect_with_crsqlite(&db_a_path).await?;
        let db_b = crate::core::db::tests::connect_with_crsqlite(&db_b_path).await?;
        sqlx::query(crate::core::db::SCHEMA_INIT_SQL).execute(&db_a).await?;
        sqlx::query(crate::core::db::SCHEMA_INIT_SQL).execute(&db_b).await?;
        crate::core::db::register_crr_tables(&db_a, crate::core::db::CORE_SYNC_TABLES).await?;
        crate::core::db::register_crr_tables(&db_b, crate::core::db::CORE_SYNC_TABLES).await?;

        // A edits a note, then (offline) exports the changeset and encrypts it.
        sqlx::query(
            "INSERT INTO notes (id, vault_id, title, content, content_plain, tags, aliases, created_at, updated_at) \
             VALUES ('n-offline', 1, 'offline-note', 'body', 'body', '[]', '[]', ?, ?)",
        )
        .bind("2026-01-01T00:00:00Z")
        .bind("2026-01-01T00:00:00Z")
        .execute(&db_a)
        .await?;
        let changes = crate::sync::crdt::export_changes_since(&db_a, 0).await?;
        assert!(!changes.changes.is_empty(), "A must have changes to deliver");

        let key = crate::sync::crypto::generate_sync_key();
        let json = serde_json::to_string(&SyncMessage::Changeset(changes))?;
        let (ciphertext, nonce) = crate::sync::crypto::encrypt_bytes(&key, json.as_bytes())
            .map_err(anyhow::Error::msg)?;
        use base64::Engine as _;
        let msg = crate::sync::types::MailboxMessage {
            id: "m1".to_string(),
            from_device_id: "device-a".to_string(),
            ciphertext: base64::engine::general_purpose::STANDARD.encode(&ciphertext),
            nonce: base64::engine::general_purpose::STANDARD.encode(&nonce),
            account_level: false,
        };

        // B (no session, no peer) decrypts and applies the message.
        let envelope =
            decrypt_and_apply_mailbox_message(&db_b, &key, &msg).await?;
        assert!(
            matches!(envelope, SyncMessage::Changeset(_)),
            "envelope should be a changeset"
        );

        let note_count: (i64,) =
            sqlx::query_as("SELECT count(*) FROM notes WHERE id = 'n-offline'")
                .fetch_one(&db_b)
                .await?;
        assert_eq!(note_count.0, 1, "offline note must appear on B after mailbox apply");

        // Tampered ciphertext must be rejected.
        let mut tampered = msg.clone();
        let last = tampered.ciphertext.len() - 1;
        let mut bytes = tampered.ciphertext.into_bytes();
        bytes[last] = if bytes[last] == b'A' { b'B' } else { b'A' };
        tampered.ciphertext = String::from_utf8(bytes)?;
        assert!(
            decrypt_and_apply_mailbox_message(&db_b, &key, &tampered).await.is_err(),
            "tampered mailbox message must be rejected"
        );

        sqlx::query("SELECT crsql_finalize()").execute(&db_a).await?;
        sqlx::query("SELECT crsql_finalize()").execute(&db_b).await?;
        db_a.close().await;
        db_b.close().await;
        Ok(())
    }

    /// The mailbox-only path (no live P2P DataChannel) must still transfer
    /// referenced blobs. A sends a paper changeset, B applies it and requests
    /// the missing PDF, A deposits the blob payload back, and B writes it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mailbox_batch_transfers_missing_blob() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "siku-mailbox-blob-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;

        let db_a = crate::core::db::tests::connect_with_crsqlite(&dir.join("a.db")).await?;
        let db_b = crate::core::db::tests::connect_with_crsqlite(&dir.join("b.db")).await?;
        sqlx::query(crate::core::db::SCHEMA_INIT_SQL).execute(&db_a).await?;
        sqlx::query(crate::core::db::SCHEMA_INIT_SQL).execute(&db_b).await?;
        crate::core::db::register_crr_tables(&db_a, crate::core::db::CORE_SYNC_TABLES).await?;
        crate::core::db::register_crr_tables(&db_b, crate::core::db::CORE_SYNC_TABLES).await?;

        let dir_a = dir.join("a_blobs");
        let dir_b = dir.join("b_blobs");
        std::fs::create_dir_all(&dir_a)?;
        std::fs::create_dir_all(&dir_b)?;

        // A creates a paper whose PDF lives in A's blob store.
        let blob_bytes = b"fake pdf content for mailbox sync".to_vec();
        let rel_path = crate::file_store::write_blob(&dir_a, &blob_bytes, "pdf")?;
        let hash = crate::file_store::sha256_hex(&blob_bytes);
        sqlx::query(
            "INSERT INTO papers (id, title, file_path, created_at, updated_at, imported_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind("p1")
        .bind("Paper One")
        .bind(&rel_path)
        .bind("2026-01-01T00:00:00Z")
        .bind("2026-01-01T00:00:00Z")
        .bind("2026-01-01T00:00:00Z")
        .execute(&db_a)
        .await?;

        let key = crate::sync::crypto::generate_sync_key();
        use base64::Engine as _;

        // A sends the changeset to B as an encrypted mailbox message.
        let changes = crate::sync::crdt::export_changes_since(&db_a, 0).await?;
        let changeset_json = serde_json::to_string(&SyncMessage::Changeset(changes))?;
        let (ct, nonce) = crate::sync::crypto::encrypt_bytes(&key, changeset_json.as_bytes())
            .map_err(anyhow::Error::msg)?;
        let msg_to_b = crate::sync::types::MailboxMessage {
            id: "m1".to_string(),
            from_device_id: "device-a".to_string(),
            ciphertext: base64::engine::general_purpose::STANDARD.encode(&ct),
            nonce: base64::engine::general_purpose::STANDARD.encode(&nonce),
            account_level: false,
        };

        // B applies the changeset and should deposit an AttachmentRequest back to A.
        let (b_to_a_tx, mut b_to_a_rx) = mpsc::unbounded_channel::<crate::sync::types::RelayClientMsg>();
        let relay_b = crate::sync::relay_client::RelayClient::new_for_test(b_to_a_tx);
        handle_mailbox_batch(&db_b, &dir_b, &key, &relay_b, vec![msg_to_b]).await?;

        let paper_count: (i64,) =
            sqlx::query_as("SELECT count(*) FROM papers WHERE id = 'p1'")
                .fetch_one(&db_b)
                .await?;
        assert_eq!(paper_count.0, 1, "paper metadata must be synced to B");

        let deposit = b_to_a_rx
            .recv()
            .await
            .context("B did not send attachment request")?;
        let crate::sync::types::RelayClientMsg::MailboxDeposit { payload: req_payload } = deposit else {
            anyhow::bail!("expected mailbox deposit from B, got {:?}", deposit);
        };
        let req_ct = req_payload.ciphertext.clone();
        let req_nonce = req_payload.nonce.clone();
        let req_envelope = decrypt_mailbox_message(
            &key,
            &crate::sync::types::MailboxMessage {
                id: "req".to_string(),
                from_device_id: "device-b".to_string(),
                ciphertext: req_payload.ciphertext,
                nonce: req_payload.nonce,
                account_level: false,
            },
        )
        .await?;
        let SyncMessage::AttachmentRequest { hashes } = req_envelope else {
            anyhow::bail!("expected attachment request, got {:?}", req_envelope);
        };
        assert_eq!(hashes, vec![(hash.clone(), "pdf".to_string())]);

        // A receives the request and deposits the blob payload back to B.
        let (a_to_b_tx, mut a_to_b_rx) = mpsc::unbounded_channel::<crate::sync::types::RelayClientMsg>();
        let relay_a = crate::sync::relay_client::RelayClient::new_for_test(a_to_b_tx);
        handle_mailbox_batch(
            &db_a,
            &dir_a,
            &key,
            &relay_a,
            vec![crate::sync::types::MailboxMessage {
                id: "req".to_string(),
                from_device_id: "device-b".to_string(),
                ciphertext: req_ct,
                nonce: req_nonce,
                account_level: false,
            }],
        )
        .await?;

        let payload_deposit = a_to_b_rx
            .recv()
            .await
            .context("A did not send blob payload")?;
        let crate::sync::types::RelayClientMsg::MailboxDeposit { payload: payload_payload } = payload_deposit else {
            anyhow::bail!("expected mailbox deposit from A, got {:?}", payload_deposit);
        };

        // B receives the payload and writes the blob.
        handle_mailbox_batch(
            &db_b,
            &dir_b,
            &key,
            &relay_b,
            vec![crate::sync::types::MailboxMessage {
                id: "payload".to_string(),
                from_device_id: "device-a".to_string(),
                ciphertext: payload_payload.ciphertext,
                nonce: payload_payload.nonce,
                account_level: false,
            }],
        )
        .await?;

        assert!(
            crate::file_store::has_blob(&dir_b, &hash),
            "blob must be written on B after mailbox payload"
        );
        let received = std::fs::read(crate::file_store::blob_path(&dir_b, &hash, "pdf"))?;
        assert_eq!(received, blob_bytes, "mailbox-transferred blob must be byte-identical");

        sqlx::query("SELECT crsql_finalize()").execute(&db_a).await?;
        sqlx::query("SELECT crsql_finalize()").execute(&db_b).await?;
        db_a.close().await;
        db_b.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn prune_outbox_drops_expired_and_poisoned_rows() -> anyhow::Result<()> {
        let (db, _dir) = create_test_db().await?;
        let now = chrono::Utc::now();
        let old = (now - chrono::Duration::days(8))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let fresh = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

        // fresh row: kept
        sqlx::query(
            "INSERT INTO sync_outbox (id, to_device_id, ciphertext, nonce, ttl_seconds, created_at, retry_count)
             VALUES ('fresh', 'dev', '', '', 604800, ?, 0)",
        )
        .bind(&fresh)
        .execute(&db)
        .await?;
        // expired row: created 8 days ago with a 7-day TTL
        sqlx::query(
            "INSERT INTO sync_outbox (id, to_device_id, ciphertext, nonce, ttl_seconds, created_at, retry_count)
             VALUES ('expired', 'dev', '', '', 604800, ?, 0)",
        )
        .bind(&old)
        .execute(&db)
        .await?;
        // poisoned row: hit the retry ceiling
        sqlx::query(
            "INSERT INTO sync_outbox (id, to_device_id, ciphertext, nonce, ttl_seconds, created_at, retry_count)
             VALUES ('poisoned', 'dev', '', '', 604800, ?, ?)",
        )
        .bind(&fresh)
        .bind(MAX_OUTBOX_RETRIES)
        .execute(&db)
        .await?;

        prune_outbox(&db).await?;

        let remaining: Vec<(String,)> =
            sqlx::query_as("SELECT id FROM sync_outbox ORDER BY id")
                .fetch_all(&db)
                .await?;
        assert_eq!(
            remaining,
            vec![("fresh".to_string(),)],
            "only the fresh row must survive pruning"
        );

        sqlx::query("SELECT crsql_finalize()").execute(&db).await?;
        db.close().await;
        Ok(())
    }

    /// The account-level archive path (what the auto-sync proxy actually uses
    /// for offline delivery) must bring library rows — papers, annotations,
    /// collections, paper_collections, tags, paper_tags — onto a peer that was
    /// offline, and re-delivering the same archive message must be a no-op
    /// (per-sender applied cursor). Regression guard for "我的图书馆不能离线同步".
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn mailbox_account_archive_applies_library_tables_once() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "siku-mailbox-account-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;

        let db_a = crate::core::db::tests::connect_with_crsqlite(&dir.join("a.db")).await?;
        let db_b = crate::core::db::tests::connect_with_crsqlite(&dir.join("b.db")).await?;
        sqlx::query(crate::core::db::SCHEMA_INIT_SQL).execute(&db_a).await?;
        sqlx::query(crate::core::db::SCHEMA_INIT_SQL).execute(&db_b).await?;
        crate::core::db::register_crr_tables(&db_a, crate::core::db::CORE_SYNC_TABLES).await?;
        crate::core::db::register_crr_tables(&db_b, crate::core::db::CORE_SYNC_TABLES).await?;

        // A edits its library: a paper, an annotation, a collection and its
        // membership row, a tag and its paper tag.
        let now = "2026-01-01T00:00:00Z";
        sqlx::query(
            "INSERT INTO papers (id, title, authors, keywords, file_path, created_at, updated_at, imported_at) \
             VALUES ('p1', 'Paper One', '[]', '[]', 'blobs/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.pdf', ?, ?, ?)",
        )
        .bind(now).bind(now).bind(now)
        .execute(&db_a)
        .await?;
        sqlx::query(
            "INSERT INTO annotations (id, paper_id, page, type, rect, color, created_at, updated_at) \
             VALUES ('a1', 'p1', 3, 'highlight', '0,0,1,1', '#ffeb3b', ?, ?)",
        )
        .bind(now).bind(now)
        .execute(&db_a)
        .await?;
        sqlx::query(
            "INSERT INTO collections (id, name, created_at) VALUES ('c1', 'Reading List', ?)",
        )
        .bind(now)
        .execute(&db_a)
        .await?;
        sqlx::query("INSERT INTO paper_collections (paper_id, collection_id) VALUES ('p1', 'c1')")
            .execute(&db_a)
            .await?;
        sqlx::query(
            "INSERT INTO tags (id, name, created_at) VALUES ('t1', 'important', ?)",
        )
        .bind(now)
        .execute(&db_a)
        .await?;
        sqlx::query("INSERT INTO paper_tags (paper_id, tag_id) VALUES ('p1', 't1')")
            .execute(&db_a)
            .await?;

        let key = crate::sync::crypto::generate_sync_key();
        use base64::Engine as _;

        // A deposits its changes into the account archive (to_device_id "",
        // account_level = true) exactly like the auto-sync proxy does.
        let changes = crate::sync::crdt::export_changes_since(&db_a, 0).await?;
        assert!(!changes.changes.is_empty(), "A must have library changes");
        let json = serde_json::to_string(&SyncMessage::Changeset(changes))?;
        let (ct, nonce) = crate::sync::crypto::encrypt_bytes(&key, json.as_bytes())
            .map_err(anyhow::Error::msg)?;
        let msg = crate::sync::types::MailboxMessage {
            id: "acct-1".to_string(),
            from_device_id: "device-a".to_string(),
            ciphertext: base64::engine::general_purpose::STANDARD.encode(&ct),
            nonce: base64::engine::general_purpose::STANDARD.encode(&nonce),
            account_level: true,
        };

        // B (offline until now) connects and gets the archive batch.
        let (b_tx, mut b_rx) =
            tokio::sync::mpsc::unbounded_channel::<crate::sync::types::RelayClientMsg>();
        let relay_b = crate::sync::relay_client::RelayClient::new_for_test(b_tx);
        handle_mailbox_batch(&db_b, &dir, &key, &relay_b, vec![msg.clone()]).await?;

        for (table, id_col, id) in [
            ("papers", "id", "p1"),
            ("annotations", "id", "a1"),
            ("collections", "id", "c1"),
            ("paper_collections", "paper_id", "p1"),
            ("tags", "id", "t1"),
            ("paper_tags", "paper_id", "p1"),
        ] {
            let count: (i64,) = sqlx::query_as(&format!(
                "SELECT count(*) FROM {table} WHERE {id_col} = ?"
            ))
            .bind(id)
            .fetch_one(&db_b)
            .await?;
            assert_eq!(count.0, 1, "{table} row must reach B via the account archive");
        }

        // B acknowledges; the relay keeps account-level messages. Re-delivering
        // the same archive message must be a no-op (applied cursor), not a
        // duplicate.
        handle_mailbox_batch(&db_b, &dir, &key, &relay_b, vec![msg]).await?;
        for table in [
            "papers",
            "annotations",
            "collections",
            "paper_collections",
            "tags",
            "paper_tags",
        ] {
            let count: (i64,) = sqlx::query_as(&format!("SELECT count(*) FROM {table}"))
                .fetch_one(&db_b)
                .await?;
            assert_eq!(count.0, 1, "{table} must not get duplicates on re-delivery");
        }

        // B's reply stream may carry an AttachmentRequest for the referenced
        // PDF; drain whatever was queued (the request itself is validated by
        // the dedicated blob test above).
        while b_rx.try_recv().is_ok() {}

        sqlx::query("SELECT crsql_finalize()").execute(&db_a).await?;
        sqlx::query("SELECT crsql_finalize()").execute(&db_b).await?;
        db_a.close().await;
        db_b.close().await;
        Ok(())
    }

    /// A full snapshot deposited into the account archive must reach a peer
    /// that applies it via `handle_mailbox_batch`. This is the safety net for
    /// rows the DELTA path cannot deliver — e.g. changesets the relay archive
    /// pruned (7-day TTL / 2000-message cap) after the sender's cursor already
    /// advanced past them, or a device that never had a P2P session. It is
    /// also what keeps a "我的图书馆" folder and its notes from being missing
    /// on a device that only ever syncs over the mailbox.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn mailbox_full_snapshot_fills_peer_database() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "siku-mailbox-snapshot-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;

        let db_a = crate::core::db::tests::connect_with_crsqlite(&dir.join("a.db")).await?;
        let db_b = crate::core::db::tests::connect_with_crsqlite(&dir.join("b.db")).await?;
        sqlx::query(crate::core::db::SCHEMA_INIT_SQL).execute(&db_a).await?;
        sqlx::query(crate::core::db::SCHEMA_INIT_SQL).execute(&db_b).await?;
        crate::core::db::register_crr_tables(&db_a, crate::core::db::CORE_SYNC_TABLES).await?;
        crate::core::db::register_crr_tables(&db_b, crate::core::db::CORE_SYNC_TABLES).await?;

        // A's library (post-CRR; would also sync as deltas, but the snapshot
        // must carry it regardless).
        let now = "2026-01-01T00:00:00Z";
        sqlx::query(
            "INSERT INTO notes (id, vault_id, title, content, content_plain, tags, aliases, created_at, updated_at) \
             VALUES ('snap-note', 1, 'Snapshot Note', 'body', 'body', '[]', '[]', ?, ?)",
        )
        .bind(now)
        .bind(now)
        .execute(&db_a)
        .await?;
        sqlx::query(
            "INSERT INTO papers (id, title, authors, keywords, created_at, updated_at, imported_at) \
             VALUES ('snap-paper', 'Snapshot Paper', '[]', '[]', ?, ?, ?)",
        )
        .bind(now).bind(now).bind(now)
        .execute(&db_a)
        .await?;

        let key = crate::sync::crypto::generate_sync_key();

        // A deposits a full snapshot into the account archive.
        let (a_tx, mut a_rx) =
            tokio::sync::mpsc::unbounded_channel::<crate::sync::types::RelayClientMsg>();
        let relay_a = crate::sync::relay_client::RelayClient::new_for_test(a_tx);
        deliver_full_snapshot_mailbox(&db_a, &relay_a, &key).await?;
        let crate::sync::types::RelayClientMsg::MailboxDeposit { payload } = a_rx
            .recv()
            .await
            .context("A must deposit a snapshot into the account archive")?
        else {
            anyhow::bail!("expected mailbox deposit");
        };

        // B (offline device) applies the snapshot.
        let (b_tx, _b_rx) =
            tokio::sync::mpsc::unbounded_channel::<crate::sync::types::RelayClientMsg>();
        let relay_b = crate::sync::relay_client::RelayClient::new_for_test(b_tx);
        handle_mailbox_batch(
            &db_b,
            &dir,
            &key,
            &relay_b,
            vec![crate::sync::types::MailboxMessage {
                id: "snap-1".to_string(),
                from_device_id: "device-a".to_string(),
                ciphertext: payload.ciphertext,
                nonce: payload.nonce,
                account_level: true,
            }],
        )
        .await?;

        for (table, id) in [("notes", "snap-note"), ("papers", "snap-paper")] {
            let count: (i64,) = sqlx::query_as(&format!(
                "SELECT count(*) FROM {table} WHERE id = ?"
            ))
            .bind(id)
            .fetch_one(&db_b)
            .await?;
            assert_eq!(count.0, 1, "{table} row must reach B via the mailbox snapshot");
        }

        sqlx::query("SELECT crsql_finalize()").execute(&db_a).await?;
        sqlx::query("SELECT crsql_finalize()").execute(&db_b).await?;
        db_a.close().await;
        db_b.close().await;
        Ok(())
    }

    /// Multi-hop topology: A creates a note, A→B syncs, then B→C syncs.
    /// Changes received from A carry A's db_version numbers; when B re-exports
    /// them to C using B's own sent cursor, they must NOT be skipped because
    /// their db_version is lower than B's cursor. Regression guard for
    /// "note created on A never reaches C when B is the middleman".
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn changes_received_from_a_are_relayed_through_b_to_c() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "siku-relay-hop-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;

        let db_a = crate::core::db::tests::connect_with_crsqlite(&dir.join("a.db")).await?;
        let db_b = crate::core::db::tests::connect_with_crsqlite(&dir.join("b.db")).await?;
        let db_c = crate::core::db::tests::connect_with_crsqlite(&dir.join("c.db")).await?;
        for db in [&db_a, &db_b, &db_c] {
            sqlx::query(crate::core::db::SCHEMA_INIT_SQL).execute(db).await?;
            crate::core::db::register_crr_tables(db, crate::core::db::CORE_SYNC_TABLES).await?;
        }

        let now = "2026-01-01T00:00:00Z";
        // B creates a note and syncs with C FIRST, so B's sent cursor for C is
        // already past B's own early db_versions.
        sqlx::query(
            "INSERT INTO notes (id, vault_id, title, content, content_plain, tags, aliases, created_at, updated_at) \
             VALUES ('b-note', 1, 'B Note', 'b', 'b', '[]', '[]', ?, ?)",
        )
        .bind(now).bind(now)
        .execute(&db_b)
        .await?;
        let b_to_c_1 = crate::sync::crdt::export_changes_since(&db_b, 0).await?;
        assert!(!b_to_c_1.changes.is_empty());
        let mut cursor_b_for_c = b_to_c_1.to_db_version;
        crate::sync::crdt::apply_changes(&db_c, &b_to_c_1).await?;

        // A creates a note and syncs it to B (A's db_version numbering is
        // independent of B's, typically much lower).
        sqlx::query(
            "INSERT INTO notes (id, vault_id, title, content, content_plain, tags, aliases, created_at, updated_at) \
             VALUES ('a-note', 1, 'A Note', 'a', 'a', '[]', '[]', ?, ?)",
        )
        .bind(now).bind(now)
        .execute(&db_a)
        .await?;
        let a_to_b = crate::sync::crdt::export_changes_since(&db_a, 0).await?;
        eprintln!("A→B export rows={} to_db_version={}", a_to_b.changes.len(), a_to_b.to_db_version);
        crate::sync::crdt::apply_changes(&db_b, &a_to_b).await?;

        // Inspect B's crsql_changes after applying A's changes.
        let b_rows: Vec<(String, String, Option<String>, i64, Vec<u8>)> = sqlx::query_as(
            "SELECT \"table\", cid, CAST(val AS TEXT), db_version, site_id FROM crsql_changes ORDER BY db_version",
        )
        .fetch_all(&db_b)
        .await?;
        eprintln!(
            "B crsql_changes after apply ({} rows): {:?}",
            b_rows.len(),
            b_rows
                .iter()
                .map(|(t, cid, val, dv, _)| format!("{t}.{cid}={val:?}@dv{dv}"))
                .collect::<Vec<_>>()
        );
        let b_db_version: (i64,) =
            sqlx::query_as("SELECT crsql_db_version()").fetch_one(&db_b).await?;
        eprintln!("B crsql_db_version()={}", b_db_version.0);

        // B relays to C using its persisted cursor for C. cr-sqlite re-versions
        // applied foreign rows with B's local db_version (see inspection
        // above), so they are NOT skipped even though A's numbering was lower.
        let b_to_c_2 = crate::sync::crdt::export_changes_since(&db_b, cursor_b_for_c).await?;
        eprintln!(
            "B→C cursor={} second export rows={} (a-note title among them: {})",
            cursor_b_for_c,
            b_to_c_2.changes.len(),
            b_to_c_2
                .changes
                .iter()
                .any(|c| c.table == "notes" && c.val.as_deref() == Some("A Note"))
        );
        cursor_b_for_c = cursor_b_for_c.max(b_to_c_2.to_db_version);
        crate::sync::crdt::apply_changes(&db_c, &b_to_c_2).await?;

        let count: (i64,) =
            sqlx::query_as("SELECT count(*) FROM notes WHERE id = 'a-note'")
                .fetch_one(&db_c)
                .await?;
        assert_eq!(
            count.0, 1,
            "A's note must reach C through B, even though its db_version predates B's cursor for C"
        );

        // The other direction: C's changes relayed back through B to A.
        sqlx::query(
            "INSERT INTO notes (id, vault_id, title, content, content_plain, tags, aliases, created_at, updated_at) \
             VALUES ('c-note', 1, 'C Note', 'c', 'c', '[]', '[]', ?, ?)",
        )
        .bind(now).bind(now)
        .execute(&db_c)
        .await?;
        let c_to_b = crate::sync::crdt::export_changes_since(&db_c, 0).await?;
        crate::sync::crdt::apply_changes(&db_b, &c_to_b).await?;
        let b_to_a = crate::sync::crdt::export_changes_since(&db_b, 0).await?;
        crate::sync::crdt::apply_changes(&db_a, &b_to_a).await?;
        let count_c: (i64,) =
            sqlx::query_as("SELECT count(*) FROM notes WHERE id = 'c-note'")
                .fetch_one(&db_a)
                .await?;
        assert_eq!(count_c.0, 1, "C's note must reach A through B");

        for db in [&db_a, &db_b, &db_c] {
            sqlx::query("SELECT crsql_finalize()").execute(db).await?;
            db.close().await;
        }
        Ok(())
    }

    /// Notes search runs against the device-local FTS index (notes_fts),
    /// which is maintained by triggers on the notes table. A note that only
    /// arrives via sync must still be searchable — i.e. cr-sqlite's apply
    /// path must fire the FTS triggers like any normal write.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn synced_note_is_searchable_via_fts() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "siku-fts-sync-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;

        let db_a = crate::core::db::tests::connect_with_crsqlite(&dir.join("a.db")).await?;
        let db_b = crate::core::db::tests::connect_with_crsqlite(&dir.join("b.db")).await?;
        for db in [&db_a, &db_b] {
            sqlx::query(crate::core::db::SCHEMA_INIT_SQL).execute(db).await?;
            crate::core::db::register_crr_tables(db, crate::core::db::CORE_SYNC_TABLES).await?;
        }

        let now = "2026-01-01T00:00:00Z";
        sqlx::query(
            "INSERT INTO notes (id, vault_id, title, content, content_plain, tags, aliases, created_at, updated_at) \
             VALUES ('fts-note', 1, 'Neural Sync Paper', 'searchable keyword zzzq', 'searchable keyword zzzq', '[]', '[]', ?, ?)",
        )
        .bind(now).bind(now)
        .execute(&db_a)
        .await?;

        let changes = crate::sync::crdt::export_changes_since(&db_a, 0).await?;
        crate::sync::crdt::apply_changes(&db_b, &changes).await?;

        // The note row exists on B...
        let count: (i64,) = sqlx::query_as("SELECT count(*) FROM notes WHERE id = 'fts-note'")
            .fetch_one(&db_b)
            .await?;
        assert_eq!(count.0, 1, "note must sync to B");

        // ...and must be found by the FTS-based notes search.
        let fts_count: (i64,) = sqlx::query_as(
            "SELECT count(*) FROM notes_fts WHERE notes_fts MATCH 'zzzq'",
        )
        .fetch_one(&db_b)
        .await?;
        assert_eq!(
            fts_count.0, 1,
            "synced note must be in the FTS index (triggers must fire on cr-sqlite apply)"
        );

        for db in [&db_a, &db_b] {
            sqlx::query("SELECT crsql_finalize()").execute(db).await?;
            db.close().await;
        }
        Ok(())
    }

    /// `attachments` rows (paper attachments, including the main-PDF record
    /// created at import) must sync like papers: previously the table was
    /// missing from CORE_SYNC_TABLES, so every paper's attachment list was
    /// empty on peers even though the papers themselves synced.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn paper_attachments_sync_to_peer() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "siku-attach-sync-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;

        let db_a = crate::core::db::tests::connect_with_crsqlite(&dir.join("a.db")).await?;
        let db_b = crate::core::db::tests::connect_with_crsqlite(&dir.join("b.db")).await?;
        for db in [&db_a, &db_b] {
            sqlx::query(crate::core::db::SCHEMA_INIT_SQL).execute(db).await?;
            crate::core::db::register_crr_tables(db, crate::core::db::CORE_SYNC_TABLES).await?;
        }

        let now = "2026-01-01T00:00:00Z";
        sqlx::query(
            "INSERT INTO papers (id, title, authors, keywords, file_path, created_at, updated_at, imported_at) \
             VALUES ('p-att', 'Attached Paper', '[]', '[]', 'blobs/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.pdf', ?, ?, ?)",
        )
        .bind(now).bind(now).bind(now)
        .execute(&db_a)
        .await?;
        sqlx::query(
            "INSERT INTO attachments (id, paper_id, file_name, file_path, file_type, description, created_at) \
             VALUES ('att-1', 'p-att', 'main.pdf', 'blobs/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.pdf', 'pdf', 'the pdf', ?)",
        )
        .bind(now)
        .execute(&db_a)
        .await?;
        sqlx::query(
            "INSERT INTO attachments (id, paper_id, file_name, file_path, file_type, created_at) \
             VALUES ('att-2', 'p-att', 'supplement.xlsx', 'blobs/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.pdf', 'xlsx', ?)",
        )
        .bind(now)
        .execute(&db_a)
        .await?;

        let changes = crate::sync::crdt::export_changes_since(&db_a, 0).await?;
        crate::sync::crdt::apply_changes(&db_b, &changes).await?;

        for id in ["att-1", "att-2"] {
            let count: (i64,) = sqlx::query_as("SELECT count(*) FROM attachments WHERE id = ?")
                .bind(id)
                .fetch_one(&db_b)
                .await?;
            assert_eq!(count.0, 1, "attachment {id} must sync to B");
        }
        let paper_count: (i64,) =
            sqlx::query_as("SELECT count(*) FROM papers WHERE id = 'p-att'")
                .fetch_one(&db_b)
                .await?;
        assert_eq!(paper_count.0, 1);

        for db in [&db_a, &db_b] {
            sqlx::query("SELECT crsql_finalize()").execute(db).await?;
            db.close().await;
        }
        Ok(())
    }

    /// The mailbox cursor may only advance after the relay ACKNOWLEDGES the
    /// deposit (durably stored). A rejected deposit must NOT advance the
    /// cursor; the changeset goes to the outbox for retry instead of being
    /// lost behind the watermark.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn mailbox_cursor_advances_only_after_ack() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "siku-ack-cursor-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;

        let db = crate::core::db::tests::connect_with_crsqlite(&dir.join("a.db")).await?;
        sqlx::query(crate::core::db::SCHEMA_INIT_SQL).execute(&db).await?;
        crate::core::db::register_crr_tables(&db, crate::core::db::CORE_SYNC_TABLES).await?;
        let now = "2026-01-01T00:00:00Z";
        sqlx::query(
            "INSERT INTO notes (id, vault_id, title, content, content_plain, tags, aliases, created_at, updated_at) \
             VALUES ('ack-note', 1, 'Ack Note', 'body', 'body', '[]', '[]', ?, ?)",
        )
        .bind(now).bind(now)
        .execute(&db)
        .await?;
        let expected = crate::sync::crdt::export_changes_since(&db, 0).await?.to_db_version;
        let key = crate::sync::crypto::generate_sync_key();
        let cursor_key = sent_cursor_key("account");

        // 1) Accepted deposit → cursor advances.
        {
            let (tx, mut rx) = mpsc::unbounded_channel::<crate::sync::types::RelayClientMsg>();
            let relay = crate::sync::relay_client::RelayClient::new_for_test(tx);
            let relay_for_ack = relay.clone();
            let ack_task = tokio::spawn(async move {
                let msg = rx.recv().await.expect("deposit must be sent");
                let crate::sync::types::RelayClientMsg::MailboxDeposit { payload } = msg else {
                    panic!("expected mailbox deposit");
                };
                let id = payload.message_id.expect("client must send message_id");
                relay_for_ack.route_ack(crate::sync::types::MailboxDepositAckPayload {
                    id,
                    ok: true,
                    error: None,
                });
            });
            let delivered = deliver_changes_mailbox(&db, &relay, &key, "", "account").await?;
            assert_eq!(delivered, expected, "cursor should advance to the acked watermark");
            ack_task.await?;
            let cursor = crate::core::settings_service::get_device_setting(&db, &cursor_key)
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
            assert_eq!(cursor, expected.to_string(), "cursor must be persisted after ack");
        }

        // 2) Rejected deposit → cursor NOT advanced, changeset queued.
        {
            // 2a) No new changes since the acked watermark → no deposit is
            // sent at all (a no-op); the cursor stays put.
            let (tx, _rx) = mpsc::unbounded_channel::<crate::sync::types::RelayClientMsg>();
            let relay = crate::sync::relay_client::RelayClient::new_for_test(tx);
            let delivered = deliver_changes_mailbox(&db, &relay, &key, "", "account").await?;
            assert_eq!(delivered, expected, "no-op call must not move the cursor");

            // 2b) Force a new change; the deposit is now sent and REJECTED by
            // the (fake) relay. The cursor must NOT advance and the changeset
            // must be queued to the outbox.
            sqlx::query(
                "UPDATE notes SET content = 'edited', updated_at = ? WHERE id = 'ack-note'",
            )
            .bind("2026-01-01T00:00:01Z")
            .execute(&db)
            .await?;
            let (tx2, mut rx2) = mpsc::unbounded_channel::<crate::sync::types::RelayClientMsg>();
            let relay2 = crate::sync::relay_client::RelayClient::new_for_test(tx2);
            let relay2_for_ack = relay2.clone();
            let ack_task2 = tokio::spawn(async move {
                let msg = rx2.recv().await.expect("deposit must be sent");
                let crate::sync::types::RelayClientMsg::MailboxDeposit { payload } = msg else {
                    panic!("expected mailbox deposit");
                };
                let id = payload.message_id.expect("client must send message_id");
                relay2_for_ack.route_ack(crate::sync::types::MailboxDepositAckPayload {
                    id,
                    ok: false,
                    error: Some("target device not in room".to_string()),
                });
            });
            let delivered2 = deliver_changes_mailbox(&db, &relay2, &key, "", "account").await?;
            assert_eq!(delivered2, expected, "cursor must NOT advance on rejection");
            ack_task2.await?;

            let outbox: (i64,) = sqlx::query_as("SELECT count(*) FROM sync_outbox")
                .fetch_one(&db)
                .await?;
            assert_eq!(outbox.0, 1, "rejected changeset must be queued for retry");
            let cursor2 = crate::core::settings_service::get_device_setting(&db, &cursor_key)
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
            assert_eq!(cursor2, expected.to_string(), "cursor must stay at the acked watermark");
        }

        sqlx::query("SELECT crsql_finalize()").execute(&db).await?;
        db.close().await;
        Ok(())
    }

    /// Outbox rows are dropped only after the relay ACKS the re-deposit; a
    /// rejected re-deposit keeps the row (retry later).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn outbox_row_dropped_only_after_ack() -> anyhow::Result<()> {
        let _guard = FLUSH_TEST_MUTEX.lock().unwrap();
        flush_backoff_reset();
        let dir = std::env::temp_dir().join(format!(
            "siku-outbox-ack-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;

        let db = crate::core::db::tests::connect_with_crsqlite(&dir.join("a.db")).await?;
        sqlx::query(crate::core::db::SCHEMA_INIT_SQL).execute(&db).await?;
        // sync_outbox must exist (created by SCHEMA_INIT_SQL).
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        use base64::Engine as _;
        let ct = base64::engine::general_purpose::STANDARD.encode(b"cipher");
        let nonce_b64 = base64::engine::general_purpose::STANDARD.encode([0u8; 12]);
        sqlx::query(
            "INSERT INTO sync_outbox (id, to_device_id, ciphertext, nonce, ttl_seconds, created_at) \
             VALUES ('o1', 'dev-b', ?, ?, 604800, ?)",
        )
        .bind(&ct)
        .bind(&nonce_b64)
        .bind(&now)
        .execute(&db)
        .await?;

        // Ack accepted → row dropped.
        {
            let (tx, mut rx) = mpsc::unbounded_channel::<crate::sync::types::RelayClientMsg>();
            let relay = crate::sync::relay_client::RelayClient::new_for_test(tx);
            let relay_for_ack = relay.clone();
            let ack_task = tokio::spawn(async move {
                let msg = rx.recv().await.expect("deposit must be sent");
                let crate::sync::types::RelayClientMsg::MailboxDeposit { payload } = msg else {
                    panic!("expected mailbox deposit");
                };
                relay_for_ack.route_ack(crate::sync::types::MailboxDepositAckPayload {
                    id: payload.message_id.unwrap(),
                    ok: true,
                    error: None,
                });
            });
            flush_outbox_with(&db, &relay).await?;
            ack_task.await?;
            let remaining: (i64,) = sqlx::query_as("SELECT count(*) FROM sync_outbox")
                .fetch_one(&db)
                .await?;
            assert_eq!(remaining.0, 0, "acked outbox row must be dropped");
        }

        // Rejected → row kept with bumped retry_count.
        {
            sqlx::query(
                "INSERT INTO sync_outbox (id, to_device_id, ciphertext, nonce, ttl_seconds, created_at) \
                 VALUES ('o2', 'dev-b', ?, ?, 604800, ?)",
            )
            .bind(&ct)
            .bind(&nonce_b64)
            .bind(&now)
            .execute(&db)
            .await?;
            let (tx, mut rx) = mpsc::unbounded_channel::<crate::sync::types::RelayClientMsg>();
            let relay = crate::sync::relay_client::RelayClient::new_for_test(tx);
            let relay_for_ack = relay.clone();
            let ack_task = tokio::spawn(async move {
                let msg = rx.recv().await.expect("deposit must be sent");
                let crate::sync::types::RelayClientMsg::MailboxDeposit { payload } = msg else {
                    panic!("expected mailbox deposit");
                };
                relay_for_ack.route_ack(crate::sync::types::MailboxDepositAckPayload {
                    id: payload.message_id.unwrap(),
                    ok: false,
                    error: Some("target device not in room".to_string()),
                });
            });
            flush_outbox_with(&db, &relay).await?;
            ack_task.await?;
            let retries: (i64,) =
                sqlx::query_as("SELECT retry_count FROM sync_outbox WHERE id = 'o2'")
                    .fetch_one(&db)
                    .await?;
            assert_eq!(retries.0, 1, "rejected row must be kept for retry");
        }

        sqlx::query("SELECT crsql_finalize()").execute(&db).await?;
        db.close().await;
        Ok(())
    }

    /// Backpressure: while an outbox row for the same target is still
    /// pending, repeated deliver attempts must NOT queue duplicates — the
    /// cursor never advanced, so every tick re-exports the same range and
    /// would otherwise grow the outbox without bound while the relay is
    /// unreachable.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn deliver_changes_mailbox_backpressure_keeps_single_outbox_row() -> anyhow::Result<()> {
        let (db, _dir) = create_test_db().await?;
        let now = "2026-01-01T00:00:00Z";
        sqlx::query(
            "INSERT INTO notes (id, vault_id, title, content, content_plain, tags, aliases, created_at, updated_at) \
             VALUES ('bp-note', 1, 'Backpressure', 'body', 'body', '[]', '[]', ?, ?)",
        )
        .bind(now)
        .bind(now)
        .execute(&db)
        .await?;
        let key = crate::sync::crypto::generate_sync_key();

        // First deliver: the relay rejects → exactly one outbox row.
        {
            let (tx, mut rx) = mpsc::unbounded_channel::<crate::sync::types::RelayClientMsg>();
            let relay = crate::sync::relay_client::RelayClient::new_for_test(tx);
            let relay_for_ack = relay.clone();
            let ack_task = tokio::spawn(async move {
                let msg = rx.recv().await.expect("deposit must be sent");
                let crate::sync::types::RelayClientMsg::MailboxDeposit { payload } = msg else {
                    panic!("expected mailbox deposit");
                };
                relay_for_ack.route_ack(crate::sync::types::MailboxDepositAckPayload {
                    id: payload.message_id.unwrap(),
                    ok: false,
                    error: Some("relay overloaded".to_string()),
                });
            });
            deliver_changes_mailbox(&db, &relay, &key, "dev-b", "dev-b").await?;
            ack_task.await?;
        }

        // Further delivers see the pending row and skip entirely: no new
        // deposit is even sent.
        for _ in 0..2 {
            let (tx, mut rx) = mpsc::unbounded_channel::<crate::sync::types::RelayClientMsg>();
            let relay = crate::sync::relay_client::RelayClient::new_for_test(tx);
            deliver_changes_mailbox(&db, &relay, &key, "dev-b", "dev-b").await?;
            assert!(
                rx.try_recv().is_err(),
                "no new deposit may be sent while an outbox row is pending"
            );
        }

        let count: (i64,) =
            sqlx::query_as("SELECT count(*) FROM sync_outbox WHERE to_device_id = 'dev-b'")
                .fetch_one(&db)
                .await?;
        assert_eq!(count.0, 1, "outbox must hold exactly one row per pending range");

        sqlx::query("SELECT crsql_finalize()").execute(&db).await?;
        db.close().await;
        Ok(())
    }

    /// An outbox retry must re-deposit with the SAME message_id the row was
    /// queued with, so the relay dedupes retransmits of a deposit it already
    /// stored (idempotent retry after a lost ack).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn flush_outbox_reuses_stored_message_id() -> anyhow::Result<()> {
        let _guard = FLUSH_TEST_MUTEX.lock().unwrap();
        flush_backoff_reset();
        let (db, _dir) = create_test_db().await?;
        write_outbox_row(&db, "dev-b", b"cipher", &[7u8; 12], "mid-fixed-1").await?;

        let (tx, mut rx) = mpsc::unbounded_channel::<crate::sync::types::RelayClientMsg>();
        let relay = crate::sync::relay_client::RelayClient::new_for_test(tx);
        let relay_for_ack = relay.clone();
        let ack_task = tokio::spawn(async move {
            let msg = rx.recv().await.expect("deposit must be sent");
            let crate::sync::types::RelayClientMsg::MailboxDeposit { payload } = msg else {
                panic!("expected mailbox deposit");
            };
            assert_eq!(
                payload.message_id.as_deref(),
                Some("mid-fixed-1"),
                "flush must reuse the stored message_id"
            );
            relay_for_ack.route_ack(crate::sync::types::MailboxDepositAckPayload {
                id: "mid-fixed-1".to_string(),
                ok: true,
                error: None,
            });
        });
        flush_outbox_with(&db, &relay).await?;
        ack_task.await?;

        let remaining: (i64,) = sqlx::query_as("SELECT count(*) FROM sync_outbox")
            .fetch_one(&db)
            .await?;
        assert_eq!(remaining.0, 0, "acked outbox row must be dropped");

        sqlx::query("SELECT crsql_finalize()").execute(&db).await?;
        db.close().await;
        Ok(())
    }

    /// Backoff growth: 10s initial, doubling on consecutive failures, capped
    /// at 5 minutes; reset clears it entirely.
    #[test]
    fn flush_backoff_doubles_and_caps() {
        let _guard = FLUSH_TEST_MUTEX.lock().unwrap();
        flush_backoff_reset();
        flush_backoff_record_failure();
        assert_eq!(
            FLUSH_BACKOFF.lock().unwrap().as_ref().unwrap().current,
            FLUSH_BACKOFF_INITIAL
        );
        flush_backoff_record_failure();
        assert_eq!(
            FLUSH_BACKOFF.lock().unwrap().as_ref().unwrap().current,
            FLUSH_BACKOFF_INITIAL * 2
        );
        for _ in 0..10 {
            flush_backoff_record_failure();
        }
        assert_eq!(
            FLUSH_BACKOFF.lock().unwrap().as_ref().unwrap().current,
            FLUSH_BACKOFF_MAX
        );
        flush_backoff_reset();
        assert!(FLUSH_BACKOFF.lock().unwrap().is_none());
    }

    /// After a transport-level flush failure (ack timeout), the process-level
    /// backoff holds off subsequent flushes instead of re-sending the whole
    /// backlog on the next proxy tick (production: a 13.4MB outbox
    /// retransmitted every 10s for 8 minutes). A successful ack clears it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn flush_outbox_backs_off_after_timeout_until_ack() -> anyhow::Result<()> {
        let _guard = FLUSH_TEST_MUTEX.lock().unwrap();
        flush_backoff_reset();
        let (db, _dir) = create_test_db().await?;
        write_outbox_row(&db, "dev-b", b"cipher", &[7u8; 12], "mid-backoff-1").await?;

        // No ack arrives → the deposit times out (ack-timeout floor, ~3s) and
        // the flush engages the backoff.
        let (tx, _rx) = mpsc::unbounded_channel::<crate::sync::types::RelayClientMsg>();
        let relay = crate::sync::relay_client::RelayClient::new_for_test(tx);
        flush_outbox_with(&db, &relay).await?;
        assert!(flush_backoff_active(), "a timed-out flush must engage the backoff");
        let kept: (i64,) = sqlx::query_as("SELECT count(*) FROM sync_outbox")
            .fetch_one(&db)
            .await?;
        assert_eq!(kept.0, 1, "an unconfirmed row stays queued (retry count untouched)");

        // Inside the backoff window the flush fast-returns without sending.
        let (tx2, mut rx2) = mpsc::unbounded_channel::<crate::sync::types::RelayClientMsg>();
        let relay2 = crate::sync::relay_client::RelayClient::new_for_test(tx2);
        let start = std::time::Instant::now();
        flush_outbox_with(&db, &relay2).await?;
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "a backed-off flush must fast-return, took {:?}",
            start.elapsed()
        );
        assert!(rx2.try_recv().is_err(), "no deposit may be sent during the backoff");

        // Once the window clears, a successful ack delivers the row and the
        // backoff stays reset.
        flush_backoff_reset();
        let (tx3, mut rx3) = mpsc::unbounded_channel::<crate::sync::types::RelayClientMsg>();
        let relay3 = crate::sync::relay_client::RelayClient::new_for_test(tx3);
        let relay_for_ack = relay3.clone();
        let ack_task = tokio::spawn(async move {
            let msg = rx3.recv().await.expect("deposit must be sent");
            let crate::sync::types::RelayClientMsg::MailboxDeposit { payload } = msg else {
                panic!("expected mailbox deposit");
            };
            relay_for_ack.route_ack(crate::sync::types::MailboxDepositAckPayload {
                id: payload.message_id.unwrap(),
                ok: true,
                error: None,
            });
        });
        flush_outbox_with(&db, &relay3).await?;
        ack_task.await?;
        let remaining: (i64,) = sqlx::query_as("SELECT count(*) FROM sync_outbox")
            .fetch_one(&db)
            .await?;
        assert_eq!(remaining.0, 0, "the row is delivered once the transport recovers");
        assert!(!flush_backoff_active(), "a successful ack must clear the backoff");

        sqlx::query("SELECT crsql_finalize()").execute(&db).await?;
        db.close().await;
        Ok(())
    }
}
