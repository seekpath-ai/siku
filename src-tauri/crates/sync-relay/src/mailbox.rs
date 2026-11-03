//! Encrypted mailbox with SQLite persistence: per-device queues PLUS an
//! account-level archive. The service stores ciphertext only and never sees
//! keys.
//!
//! Persistence matters: the previous in-memory implementation lost every
//! undelivered message on a relay restart while senders had already advanced
//! their sync cursors — permanent data loss for offline devices. Deposits now
//! commit to SQLite and the relay acknowledges them (`MailboxDepositAck`), so
//! a sender only advances its cursor after the message is durably stored.
//!
//! Account-level messages are how offline delivery works for devices that do
//! not exist yet (or were never online): a device deposits its changes with an
//! empty `to_device_id`, any device of the account that later connects polls
//! the archive and applies them. Every device of the account sees the archive;
//! per-device "seen" marks keep each device from re-fetching the same message.

use crate::MailboxMessage;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

pub const DEFAULT_TTL_SECONDS: u64 = 7 * 24 * 3600; // 7 days
const MAX_MESSAGES_PER_DEVICE: usize = 500;

/// `expires_at` sentinel for rows that never age out: committed state-slot
/// slices and persistent account-archive WAL rows (deposits that omit
/// `ttl_seconds`). SQLite INTEGER is i64, so the sentinel fits comfortably.
pub const NEVER_EXPIRES: u64 = i64::MAX as u64;

/// Slot slices of a checkpoint group that never completes (crashed/abandoned
/// mid-upload) keep a finite TTL so they eventually clean themselves up.
const INCOMPLETE_SLOT_TTL_SECS: u64 = 30 * 24 * 3600; // 30 days

/// Per-device messages are re-delivered when a poll's delivery was never
/// acked within this window (the relay or client may have died between poll
/// and the client's apply + ack). Redelivery is safe: clients dedupe by
/// message id (applied cursor / INSERT OR IGNORE).
pub const REDELIVERY_WINDOW_SECS: u64 = 60;

/// Cap on `seen` entries per account-level message. `seen` is only a dedupe
/// hint for the shared archive; beyond this many devices we stop appending
/// and the affected devices simply re-fetch the message (clients dedupe by
/// message id anyway).
const MAX_SEEN_ENTRIES: usize = 100;

/// Empty `to_device_id` means "account-level archive" (any device of the
/// account may poll it).
pub const ACCOUNT_LEVEL_TARGET: &str = "";

/// State-slot metadata of one checkpoint slice. A device checkpoints its full
/// state as a group of slices sharing `group`; when the last slice lands the
/// relay atomically commits the group (never expires), deletes the sender's
/// older generations, and purges the sender's WAL rows older than the group.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SlotMeta {
    pub group: String,
    pub index: u32,
    pub total: u32,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS mailbox_messages (
    seq           INTEGER PRIMARY KEY AUTOINCREMENT,
    id            TEXT NOT NULL UNIQUE,
    room_id       TEXT NOT NULL,
    to_device_id  TEXT NOT NULL DEFAULT '',
    from_device_id TEXT NOT NULL,
    ciphertext    TEXT NOT NULL,
    nonce         TEXT NOT NULL,
    expires_at    INTEGER NOT NULL,
    created_at    INTEGER NOT NULL,
    delivered_at  INTEGER,
    seen          TEXT NOT NULL DEFAULT '',
    slot_group    TEXT,
    slot_index    INTEGER,
    slot_total    INTEGER,
    dedup_key     TEXT
);
CREATE INDEX IF NOT EXISTS idx_mailbox_room_to ON mailbox_messages(room_id, to_device_id);
CREATE TABLE IF NOT EXISTS dedup_keys (
    room_id    TEXT NOT NULL,
    dedup_key  TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    PRIMARY KEY (room_id, dedup_key)
);
";

/// Fresh mailbox db from this feature's dev iterations may carry the older
/// schema (id TEXT PRIMARY KEY, no seq). No released deployment exists yet, so
/// dropping and recreating is safe; the table holds ciphertext that would be
/// lost anyway (the pre-ack client model re-deposits after restart).
fn ensure_schema(conn: &mut rusqlite::Connection) -> anyhow::Result<()> {
    let table_count: i64 = conn
        .prepare("SELECT count(*) FROM sqlite_master WHERE type='table' AND name='mailbox_messages'")?
        .query_map([], |r| r.get(0))?
        .next()
        .transpose()?
        .unwrap_or(0);
    if table_count > 0 {
        let has_seq: i64 = conn
            .prepare("SELECT count(*) FROM pragma_table_info('mailbox_messages') WHERE name = 'seq'")?
            .query_map([], |r| r.get(0))?
            .next()
            .transpose()?
            .unwrap_or(0);
        if has_seq == 0 {
            conn.execute_batch("DROP TABLE mailbox_messages")?;
        }
    }
    conn.execute_batch(SCHEMA)?;
    // Databases created before at-least-once delivery lack `delivered_at`;
    // add it in place. NULL = "never delivered", which is true for every row
    // an older build stored (poll used to delete on delivery).
    let has_delivered_at: i64 = conn
        .prepare("SELECT count(*) FROM pragma_table_info('mailbox_messages') WHERE name = 'delivered_at'")?
        .query_map([], |r| r.get(0))?
        .next()
        .transpose()?
        .unwrap_or(0);
    if has_delivered_at == 0 {
        conn.execute_batch("ALTER TABLE mailbox_messages ADD COLUMN delivered_at INTEGER")?;
    }
    // State-slot columns (relay protocol v3). NULL slot_group = WAL row.
    // `dedup_key` mirrors the deposit's dedup key onto the row so a
    // checkpoint's WAL purge can exclude content-addressed blob messages
    // (a checkpoint covers row changes, never blobs).
    for (col, decl) in [
        ("slot_group", "TEXT"),
        ("slot_index", "INTEGER"),
        ("slot_total", "INTEGER"),
        ("dedup_key", "TEXT"),
    ] {
        let has_col: i64 = conn
            .prepare(&format!(
                "SELECT count(*) FROM pragma_table_info('mailbox_messages') WHERE name = '{col}'"
            ))?
            .query_map([], |r| r.get(0))?
            .next()
            .transpose()?
            .unwrap_or(0);
        if has_col == 0 {
            conn.execute_batch(&format!(
                "ALTER TABLE mailbox_messages ADD COLUMN {col} {decl}"
            ))?;
        }
    }
    // The slot index references the migrated columns, so it can only be
    // created AFTER the ALTERs above (an existing production db lacks them).
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_mailbox_slot ON mailbox_messages(room_id, from_device_id, slot_group)",
    )?;
    Ok(())
}

/// Concurrency trade-off: all operations run synchronously inside the async
/// WebSocket handlers under a single global `Mutex<Connection>`. The proper
/// fix would be `tokio::task::spawn_blocking` (or a dedicated writer task)
/// per call, but that means making every method async and threading
/// `Arc<Mutex<Connection>>` clones through spawn closures — a large diff for
/// no measurable gain here: the relay is self-hosted at small scale (a
/// handful of devices per account), and these are indexed SQLite statements
/// in the microsecond-to-millisecond range, so blocking the handler briefly
/// is acceptable. Revisit if the relay ever serves many concurrent rooms.
pub struct Mailbox {
    conn: Mutex<rusqlite::Connection>,
    /// Devices registered per room so per-device deposits can reject unknown
    /// targets (mirrors the pre-persistence semantics where a missing queue
    /// entry meant "not in room"). Membership is also tracked in `rooms`; this
    /// set is the mailbox-side bookkeeping updated by Join/Leave.
    registered: Mutex<HashSet<(String, String)>>, // (room_id, device_id)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Map one SQLite row to the wire message.
fn row_to_message(row: &rusqlite::Row<'_>, account_level: bool) -> rusqlite::Result<MailboxMessage> {
    Ok(MailboxMessage {
        id: row.get(0)?,
        from_device_id: row.get(1)?,
        ciphertext: row.get(2)?,
        nonce: row.get(3)?,
        account_level,
    })
}

impl Mailbox {
    /// Open (or create) the mailbox database at `path`. `":memory:"` gives a
    /// non-persistent instance (tests, or a relay configured without a file).
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let mut conn = rusqlite::Connection::open(path)?;
        ensure_schema(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            registered: Mutex::new(HashSet::new()),
        })
    }

    fn prune(conn: &mut rusqlite::Connection, now: u64) {
        let _ = conn.execute(
            "DELETE FROM mailbox_messages WHERE expires_at <= ?1",
            rusqlite::params![now],
        );
        let _ = conn.execute(
            "DELETE FROM dedup_keys WHERE expires_at <= ?1",
            rusqlite::params![now],
        );
    }

    /// Drop the oldest rows beyond the per-target cap (FIFO, same semantics as
    /// the previous in-memory quota): keep the NEWEST `cap` rows.
    fn trim(
        conn: &mut rusqlite::Connection,
        room_id: &str,
        to_device_id: &str,
        cap: usize,
    ) -> Result<(), String> {
        conn.execute(
            "DELETE FROM mailbox_messages WHERE room_id = ?1 AND to_device_id = ?2 AND id NOT IN (
                SELECT id FROM mailbox_messages
                WHERE room_id = ?1 AND to_device_id = ?2
                ORDER BY seq DESC
                LIMIT ?3
            )",
            rusqlite::params![room_id, to_device_id, cap as i64],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Append `device_id` to a message's comma-separated `seen` list, capped
    /// at `MAX_SEEN_ENTRIES` — past the cap the list stops growing (devices
    /// just re-fetch the message and dedupe by id).
    fn append_seen(conn: &rusqlite::Connection, id: &str, device_id: &str) {
        let seen: String = conn
            .query_row(
                "SELECT seen FROM mailbox_messages WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .unwrap_or_default();
        let count = seen.split(',').filter(|s| !s.is_empty()).count();
        if count >= MAX_SEEN_ENTRIES {
            tracing::warn!(id = %id, device = %device_id, "mailbox seen list full; skipping mark");
            return;
        }
        let _ = conn.execute(
            "UPDATE mailbox_messages SET seen = seen || ?1 WHERE id = ?2",
            rusqlite::params![format!("{device_id},"), id],
        );
    }

    /// Total stored bytes for a room: summed `LENGTH(ciphertext) +
    /// LENGTH(nonce)` across the account archive AND every per-device queue.
    /// This is the quota accounting unit reported by `GET /api/storage`.
    pub fn usage_bytes(&self, room_id: &str) -> i64 {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT COALESCE(SUM(LENGTH(ciphertext) + LENGTH(nonce)), 0)
             FROM mailbox_messages WHERE room_id = ?1",
            rusqlite::params![room_id],
            |r| r.get(0),
        )
        .unwrap_or(0)
    }

    /// Deposit a ciphertext message. An empty `to_device_id` stores it in the
    /// account-level archive (no target device needs to exist or be online);
    /// otherwise it goes to that device's queue (target must have joined).
    /// `message_id` is the client's correlation id: when present it is used as
    /// the stored id, and re-depositing the same id is an idempotent no-op —
    /// this is what makes "retry until acknowledged" safe. Returns the stored
    /// message id.
    ///
    /// Expiry policy:
    /// - Per-device queue rows: caller TTL, capped at `DEFAULT_TTL_SECONDS`.
    /// - Account-level WAL rows (`slot: None`, no `ttl_seconds`): never
    ///   expire — they are the durable replication log, reclaimed only by the
    ///   sender's next checkpoint commit (or the byte quota).
    /// - Account-level rows with an explicit TTL (legacy clients): honored,
    ///   capped at 7 days — pre-v3 clients keep their old semantics.
    /// - Slot slices: `INCOMPLETE_SLOT_TTL_SECS` until the group commits,
    ///   then never expire.
    ///
    /// `slot` marks the message as one slice of a state checkpoint group and
    /// is only valid for account-level deposits. Once every slice of the
    /// group is stored, the group commits atomically: all its slices become
    /// permanent, every older slot group of the same sender is deleted, and
    /// the sender's WAL rows older than the group are purged (the checkpoint
    /// covers them). A re-deposited slice (idempotent retry) still drives the
    /// commit check, so a slice that was lost and re-sent completes the group.
    ///
    /// `quota_bytes` caps the room's total stored bytes: a NEW message that
    /// would push usage past the quota is rejected with
    /// `Err("quota_exceeded")` (existing data is never deleted — it ages out
    /// via TTL or ack, or is reclaimed by a checkpoint commit). Idempotent
    /// replays bypass the quota check: they add no bytes, and rejecting them
    /// would wedge the client's outbox on its first message once the quota is
    /// full. For slot deposits the quota check subtracts the sender's other
    /// slot groups: a checkpoint replaces them on commit, so it must not be
    /// rejected for transiently double-counting the outgoing generation.
    ///
    /// `dedup_key` is an opaque content key (clients use
    /// `HMAC(sync_key, blob_hash)` for blob payloads, so the relay cannot
    /// correlate it with any known file). A deposit whose key is already
    /// stored for this room is acknowledged WITHOUT storing a second copy —
    /// two devices pushing the same file must not double-spend the account
    /// quota. Like an idempotent replay, a dedup hit bypasses the quota.
    #[allow(clippy::too_many_arguments)]
    pub fn deposit(
        &self,
        room_id: &str,
        from_device_id: &str,
        to_device_id: &str,
        ciphertext: String,
        nonce: String,
        ttl_seconds: Option<u64>,
        message_id: Option<String>,
        quota_bytes: i64,
        dedup_key: Option<String>,
        slot: Option<SlotMeta>,
    ) -> Result<String, String> {
        let id = message_id
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let ts = now();

        if let Some(s) = &slot {
            if to_device_id != ACCOUNT_LEVEL_TARGET {
                return Err("slot deposits must be account-level".to_string());
            }
            if s.group.is_empty() || s.group.len() > 64 || s.total == 0 || s.index >= s.total {
                return Err("invalid slot meta".to_string());
            }
            if s.total > 4096 {
                return Err("slot group too large".to_string());
            }
        }

        let expires_at = if slot.is_some() {
            ts + INCOMPLETE_SLOT_TTL_SECS
        } else if to_device_id == ACCOUNT_LEVEL_TARGET && ttl_seconds.is_none() {
            NEVER_EXPIRES
        } else {
            let ttl = ttl_seconds.unwrap_or(DEFAULT_TTL_SECONDS).min(DEFAULT_TTL_SECONDS);
            ts + ttl
        };

        if to_device_id != ACCOUNT_LEVEL_TARGET {
            let registered = self.registered.lock().unwrap();
            if !registered.contains(&(room_id.to_string(), to_device_id.to_string())) {
                return Err("target device not in room".to_string());
            }
        }

        let mut conn = self.conn.lock().unwrap();
        Self::prune(&mut conn, ts);

        // Idempotent replay: the id is already stored, acknowledge without
        // touching the quota (no new bytes are added). A slot slice still
        // falls through to the commit check: this re-deposit may be the
        // missing slice that completes its group.
        let already_stored: bool = conn
            .query_row(
                "SELECT 1 FROM mailbox_messages WHERE id = ?1",
                rusqlite::params![id],
                |_| Ok(true),
            )
            .unwrap_or(false);
        if !already_stored {
            // Content-level dedup: another device already stored a message
            // with this key — acknowledge without double-storing (no new
            // bytes, so the quota is not touched either).
            let dedup_hit = dedup_key
                .as_deref()
                .filter(|k| !k.is_empty())
                .map(|k| {
                    conn.query_row(
                        "SELECT 1 FROM dedup_keys WHERE room_id = ?1 AND dedup_key = ?2 AND expires_at > ?3",
                        rusqlite::params![room_id, k, ts],
                        |_| Ok(true),
                    )
                    .unwrap_or(false)
                })
                .unwrap_or(false);
            if dedup_hit {
                return Ok(id);
            }

            // Quota check inside the lock so concurrent deposits cannot both
            // pass. Slot deposits subtract the sender's COMMITTED generations
            // (never-expiring slot rows): the commit of this group deletes
            // them, so the checkpoint must not be rejected for transiently
            // double-counting what it replaces. Incomplete groups still count
            // in full — otherwise rotating abandoned groups could bypass the
            // quota indefinitely.
            let mut usage: i64 = conn
                .query_row(
                    "SELECT COALESCE(SUM(LENGTH(ciphertext) + LENGTH(nonce)), 0)
                     FROM mailbox_messages WHERE room_id = ?1",
                    rusqlite::params![room_id],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            if let Some(s) = &slot {
                let replaced: i64 = conn
                    .query_row(
                        "SELECT COALESCE(SUM(LENGTH(ciphertext) + LENGTH(nonce)), 0)
                         FROM mailbox_messages
                         WHERE room_id = ?1 AND from_device_id = ?2 AND to_device_id = ''
                           AND slot_group IS NOT NULL AND slot_group != ?3 AND expires_at = ?4",
                        rusqlite::params![room_id, from_device_id, s.group, NEVER_EXPIRES],
                        |r| r.get(0),
                    )
                    .map_err(|e| e.to_string())?;
                usage = usage.saturating_sub(replaced);
            }
            let incoming = (ciphertext.len() + nonce.len()) as i64;
            if usage + incoming > quota_bytes {
                return Err("quota_exceeded".to_string());
            }

            conn.execute(
                "INSERT OR IGNORE INTO mailbox_messages
                 (id, room_id, to_device_id, from_device_id, ciphertext, nonce, expires_at, created_at, seen,
                  slot_group, slot_index, slot_total, dedup_key)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, '', ?9, ?10, ?11, ?12)",
                rusqlite::params![
                    id,
                    room_id,
                    to_device_id,
                    from_device_id,
                    ciphertext,
                    nonce,
                    expires_at,
                    ts,
                    slot.as_ref().map(|s| s.group.as_str()),
                    slot.as_ref().map(|s| s.index),
                    slot.as_ref().map(|s| s.total),
                    dedup_key.as_deref().filter(|k| !k.is_empty()),
                ],
            )
            .map_err(|e| e.to_string())?;

            // Record the content key so later deposits of the same blob (from
            // any device of the account) dedupe instead of double-storing.
            if let Some(k) = dedup_key.as_deref().filter(|k| !k.is_empty()) {
                let _ = conn.execute(
                    "INSERT OR REPLACE INTO dedup_keys (room_id, dedup_key, expires_at) VALUES (?1, ?2, ?3)",
                    rusqlite::params![room_id, k, expires_at],
                );
            }

            if to_device_id == ACCOUNT_LEVEL_TARGET {
                // The depositor has already "seen" it (they created it).
                Self::append_seen(&conn, &id, from_device_id);
            } else {
                Self::trim(&mut conn, room_id, to_device_id, MAX_MESSAGES_PER_DEVICE)?;
            }
        }

        if let Some(s) = &slot {
            Self::maybe_commit_slot_group(&mut conn, room_id, from_device_id, s)?;
        }
        Ok(id)
    }

    /// Commit a checkpoint group once every slice is stored: make the group
    /// permanent, delete the sender's older generations, and purge the
    /// sender's WAL rows older than the group. Idempotent — safe to re-run on
    /// retried deposits. Runs inside one transaction so a crash mid-commit
    /// cannot leave the old generation deleted without the new one permanent.
    fn maybe_commit_slot_group(
        conn: &mut rusqlite::Connection,
        room_id: &str,
        from_device_id: &str,
        slot: &SlotMeta,
    ) -> Result<(), String> {
        let stored: i64 = conn
            .query_row(
                "SELECT COUNT(DISTINCT slot_index) FROM mailbox_messages
                 WHERE room_id = ?1 AND from_device_id = ?2 AND to_device_id = ''
                   AND slot_group = ?3",
                rusqlite::params![room_id, from_device_id, slot.group],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if stored < slot.total as i64 {
            return Ok(());
        }
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        tx.execute(
            "UPDATE mailbox_messages SET expires_at = ?1
             WHERE room_id = ?2 AND from_device_id = ?3 AND to_device_id = '' AND slot_group = ?4",
            rusqlite::params![NEVER_EXPIRES, room_id, from_device_id, slot.group],
        )
        .map_err(|e| e.to_string())?;
        // Older generations: committed ones and stale incomplete ones alike.
        tx.execute(
            "DELETE FROM mailbox_messages
             WHERE room_id = ?1 AND from_device_id = ?2 AND to_device_id = ''
               AND slot_group IS NOT NULL AND slot_group != ?3",
            rusqlite::params![room_id, from_device_id, slot.group],
        )
        .map_err(|e| e.to_string())?;
        // WAL rows this checkpoint covers (everything deposited before the
        // group's first slice). Later WAL rows survive. Blob messages
        // (dedup_key set) are NOT covered by a checkpoint — they age out on
        // their own TTL and are re-served on request.
        tx.execute(
            "DELETE FROM mailbox_messages
             WHERE room_id = ?1 AND from_device_id = ?2 AND to_device_id = ''
               AND slot_group IS NULL AND dedup_key IS NULL
               AND seq < (SELECT MIN(seq) FROM mailbox_messages
                          WHERE room_id = ?1 AND from_device_id = ?2 AND to_device_id = ''
                            AND slot_group = ?3)",
            rusqlite::params![room_id, from_device_id, slot.group],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        tracing::info!(
            room = %room_id,
            from = %from_device_id,
            group = %slot.group,
            slices = slot.total,
            "checkpoint group committed: older generation and covered WAL purged"
        );
        Ok(())
    }

    /// Take up to `max_count` pending messages for `device_id`: its own queue
    /// (stamped `delivered_at`, deleted only on ack — redelivered after
    /// `REDELIVERY_WINDOW_SECS` if never acked) plus account-level messages it
    /// has not seen yet. Account-level rows are NOT marked seen here: the
    /// batch can still be lost after the poll (oversized WS frame,
    /// disconnect), so `seen` is recorded only when the client acks. Until
    /// then a repeated poll returns the same archive messages; clients dedupe
    /// by message id.
    pub fn poll(&self, room_id: &str, device_id: &str, max_count: Option<usize>) -> Vec<MailboxMessage> {
        let limit = max_count.unwrap_or(100);
        let ts = now();
        let mut out = Vec::new();
        let mut conn = self.conn.lock().unwrap();
        Self::prune(&mut conn, ts);

        // 1) Per-device queue (at-least-once): poll marks `delivered_at` and
        // keeps the row; only an ack deletes it. Rows already delivered inside
        // the redelivery window are skipped so a polling loop does not spin on
        // duplicates.
        let redeliver_before = ts.saturating_sub(REDELIVERY_WINDOW_SECS);
        {
            let mut stmt = match conn.prepare(
                "SELECT id, from_device_id, ciphertext, nonce FROM mailbox_messages
                 WHERE room_id = ?1 AND to_device_id = ?2 AND expires_at > ?3
                   AND (delivered_at IS NULL OR delivered_at <= ?4)
                 ORDER BY seq ASC LIMIT ?5",
            ) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(error = %e, "mailbox poll prepare failed");
                    return out;
                }
            };
            let rows: Vec<MailboxMessage> = stmt
                .query_map(
                    rusqlite::params![room_id, device_id, ts, redeliver_before, limit as i64],
                    |r| row_to_message(r, false),
                )
                .ok()
                .and_then(|m| m.collect::<Result<Vec<_>, _>>().ok())
                .unwrap_or_default();
            drop(stmt);
            for m in &rows {
                let _ = conn.execute(
                    "UPDATE mailbox_messages SET delivered_at = ?1 WHERE id = ?2",
                    rusqlite::params![ts, &m.id],
                );
            }
            out.extend(rows);
        }
        if out.len() >= limit {
            return out;
        }

        // 2) Account-level archive (shared; never removed or marked seen by
        // poll — seen is recorded on ack, so an undelivered batch is simply
        // returned again by the next poll).
        let remaining = limit - out.len();
        {
            let mut stmt = match conn.prepare(
                "SELECT id, from_device_id, ciphertext, nonce FROM mailbox_messages
                 WHERE room_id = ?1 AND to_device_id = '' AND expires_at > ?2
                   AND (',' || seen || ',') NOT LIKE ('%,' || ?3 || ',%')
                 ORDER BY seq ASC LIMIT ?4",
            ) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(error = %e, "mailbox account poll prepare failed");
                    return out;
                }
            };
            let rows: Vec<MailboxMessage> = stmt
                .query_map(
                    rusqlite::params![room_id, ts, device_id, remaining as i64],
                    |r| row_to_message(r, true),
                )
                .ok()
                .and_then(|m| m.collect::<Result<Vec<_>, _>>().ok())
                .unwrap_or_default();
            out.extend(rows);
        }
        out
    }

    /// Acknowledge messages. A per-device row owned by `device_id` is deleted
    /// from its queue. An account-level archive row is shared and never
    /// deleted; the ack marks it seen for this device so later polls skip it.
    /// Message ids are globally unique, so the row's `to_device_id` decides
    /// which case applies.
    pub fn ack(&self, room_id: &str, device_id: &str, message_ids: &[String]) {
        let conn = self.conn.lock().unwrap();
        for id in message_ids {
            let target: Option<String> = conn
                .query_row(
                    "SELECT to_device_id FROM mailbox_messages WHERE room_id = ?1 AND id = ?2",
                    rusqlite::params![room_id, id],
                    |r| r.get(0),
                )
                .ok();
            match target.as_deref() {
                Some(ACCOUNT_LEVEL_TARGET) => Self::append_seen(&conn, id, device_id),
                Some(t) if t == device_id => {
                    let _ = conn.execute(
                        "DELETE FROM mailbox_messages WHERE id = ?1",
                        rusqlite::params![id],
                    );
                }
                _ => {}
            }
        }
    }

    /// Register a device so deposits can target it. Called on Join.
    pub fn ensure_device(&self, room_id: &str, device_id: &str) {
        self.registered
            .lock()
            .unwrap()
            .insert((room_id.to_string(), device_id.to_string()));
    }

    /// Remove a device when it leaves the room.
    pub fn remove_device(&self, room_id: &str, device_id: &str) {
        self.registered
            .lock()
            .unwrap()
            .remove(&(room_id.to_string(), device_id.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Temp directory that removes itself on drop, so tests don't leak
    /// `mailbox.sqlite` files (including on panic).
    struct TempDir(std::path::PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp_dir(tag: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!(
            "siku-relay-mailbox-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    fn temp_mailbox(tag: &str) -> (TempDir, Mailbox) {
        let dir = temp_dir(tag);
        let mb = Mailbox::open(&dir.0.join("mailbox.sqlite")).unwrap();
        (dir, mb)
    }

    /// Generous quota for tests that do not exercise quota rejection.
    const TEST_QUOTA: i64 = 1 << 30;

    /// Rows stored for one (room, device) queue — the ground truth a poll must
    /// NOT reduce on its own.
    fn row_count(mb: &Mailbox, room_id: &str, device_id: &str) -> i64 {
        let conn = mb.conn.lock().unwrap();
        conn.query_row(
            "SELECT count(*) FROM mailbox_messages WHERE room_id = ?1 AND to_device_id = ?2",
            rusqlite::params![room_id, device_id],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn deposit_poll_ack_round_trip() {
        let dir = temp_dir("roundtrip");
        let path = dir.0.join("mailbox.sqlite");

        let id;
        {
            let mb = Mailbox::open(&path).unwrap();
            mb.ensure_device("room-a", "dev-a");
            mb.ensure_device("room-a", "dev-b");

            id = mb
                .deposit("room-a", "dev-a", "dev-b", "cipher-1".into(), "nonce-1".into(), None, None, TEST_QUOTA, None, None)
                .unwrap();
            assert!(!id.is_empty(), "deposit must return the stored message id");

            // poll delivers the message but does NOT consume it
            let msgs = mb.poll("room-a", "dev-b", None);
            assert_eq!(msgs.len(), 1);
            assert_eq!(msgs[0].ciphertext, "cipher-1");
            assert_eq!(msgs[0].from_device_id, "dev-a");
            assert!(!msgs[0].account_level);

            // a second poll inside the redelivery window is empty
            assert!(mb.poll("room-a", "dev-b", None).is_empty());
        } // dropped = "relay restarted" before the client acked

        // Reopen the database: the un-acked row survived the restart and the
        // late ack still deletes it.
        {
            let mb = Mailbox::open(&path).unwrap();
            assert_eq!(row_count(&mb, "room-a", "dev-b"), 1, "un-acked message must persist");
            mb.ack("room-a", "dev-b", std::slice::from_ref(&id));
            assert_eq!(row_count(&mb, "room-a", "dev-b"), 0, "ack after reopen deletes the row");
        }
    }

    #[test]
    fn deposit_to_unknown_device_fails() {
        let (_dir, mb) = temp_mailbox("unknown");
        mb.ensure_device("room-a", "dev-a");
        let err = mb
            .deposit("room-a", "dev-a", "ghost", "c".into(), "n".into(), None, None, TEST_QUOTA, None, None)
            .unwrap_err();
        assert!(err.contains("not in room"));
    }

    #[test]
    fn quota_fifo_drops_oldest() {
        let (_dir, mb) = temp_mailbox("quota");
        mb.ensure_device("room-a", "dev-a");
        mb.ensure_device("room-a", "dev-b");
        for i in 0..600 {
            mb.deposit("room-a", "dev-a", "dev-b", format!("c{i}"), "n".into(), None, None, TEST_QUOTA, None, None)
                .unwrap();
        }
        let msgs = mb.poll("room-a", "dev-b", Some(1000));
        assert_eq!(msgs.len(), 500); // capped
        assert_eq!(msgs[0].ciphertext, "c100"); // oldest 100 dropped
    }

    #[test]
    fn ack_removes_messages() {
        let (_dir, mb) = temp_mailbox("ack");
        mb.ensure_device("room-a", "dev-a");
        mb.ensure_device("room-a", "dev-b");
        mb.deposit("room-a", "dev-a", "dev-b", "c1".into(), "n1".into(), None, None, TEST_QUOTA, None, None)
            .unwrap();
        mb.deposit("room-a", "dev-a", "dev-b", "c2".into(), "n2".into(), None, None, TEST_QUOTA, None, None)
            .unwrap();

        let msgs = mb.poll("room-a", "dev-b", None);
        assert_eq!(msgs.len(), 2);
        mb.ack("room-a", "dev-b", &[msgs[0].id.clone()]);
        // ack deletes the delivered row; the other delivered-but-unacked
        // message is not re-polled inside the redelivery window
        mb.deposit("room-a", "dev-a", "dev-b", "c3".into(), "n3".into(), None, None, TEST_QUOTA, None, None)
            .unwrap();
        let msgs2 = mb.poll("room-a", "dev-b", None);
        assert_eq!(msgs2.len(), 1);
        assert_eq!(msgs2[0].ciphertext, "c3");
        mb.ack("room-a", "dev-b", &[msgs2[0].id.clone()]);
        assert!(mb.poll("room-a", "dev-b", None).is_empty());
    }

    /// Account-level deposit needs NO target device (it may not exist yet or
    /// may never have been online), and every device of the room can poll it.
    #[test]
    fn account_level_archive_reaches_devices_without_target() {
        let (_dir, mb) = temp_mailbox("account");
        // No device ever joined: deposit must still succeed.
        mb.deposit(
            "room-a",
            "dev-a",
            ACCOUNT_LEVEL_TARGET,
            "account-cipher".into(),
            "nonce".into(),
            None,
            None,
            TEST_QUOTA,
            None,
            None,
        )
        .unwrap();

        // dev-b joins later and sees the archive message.
        mb.ensure_device("room-a", "dev-b");
        let msgs = mb.poll("room-a", "dev-b", None);
        assert_eq!(msgs.len(), 1, "dev-b should see the account-level message");
        assert!(msgs[0].account_level, "must be flagged account_level");
        assert_eq!(msgs[0].ciphertext, "account-cipher");

        // Seen is per-device: dev-c (a different device) still gets it.
        mb.ensure_device("room-a", "dev-c");
        let msgs_c = mb.poll("room-a", "dev-c", None);
        assert_eq!(msgs_c.len(), 1, "dev-c should also see it (shared archive)");

        // dev-b has not acked: the message is still delivered again (poll no
        // longer marks seen — a lost batch must be retried).
        assert_eq!(mb.poll("room-a", "dev-b", None).len(), 1);
        // After the ack, dev-b's polls skip it.
        mb.ack("room-a", "dev-b", &[msgs[0].id.clone()]);
        assert!(mb.poll("room-a", "dev-b", None).is_empty());
    }

    /// Polling pages through a large account archive: each poll returns the
    /// next unseen slice once the previous batch is acked; devices can drain
    /// the whole archive by polling + acking.
    #[test]
    fn account_level_archive_pages_across_polls() {
        let (_dir, mb) = temp_mailbox("paging");
        for i in 0..250 {
            mb.deposit(
                "room-a",
                "dev-a",
                ACCOUNT_LEVEL_TARGET,
                format!("c{i}"),
                "n".into(),
                None,
                None,
                TEST_QUOTA, None, None,
            )
            .unwrap();
        }
        mb.ensure_device("room-a", "dev-b");

        let ack_all = |mb: &Mailbox, msgs: &[MailboxMessage]| {
            let ids: Vec<String> = msgs.iter().map(|m| m.id.clone()).collect();
            mb.ack("room-a", "dev-b", &ids);
        };
        let first = mb.poll("room-a", "dev-b", Some(100));
        assert_eq!(first.len(), 100);
        // Without an ack the same slice comes back (poll no longer marks seen).
        assert_eq!(mb.poll("room-a", "dev-b", Some(100))[0].id, first[0].id);
        ack_all(&mb, &first);
        let second = mb.poll("room-a", "dev-b", Some(100));
        assert_eq!(second.len(), 100);
        assert_ne!(second[0].id, first[0].id);
        ack_all(&mb, &second);
        let third = mb.poll("room-a", "dev-b", Some(100));
        assert_eq!(third.len(), 50);
        ack_all(&mb, &third);
        assert!(mb.poll("room-a", "dev-b", Some(100)).is_empty());
    }

    /// Per-device and account-level messages mix in one poll, device queue
    /// first.
    #[test]
    fn poll_mixes_device_and_account_messages() {
        let (_dir, mb) = temp_mailbox("mix");
        mb.ensure_device("room-a", "dev-a");
        mb.ensure_device("room-a", "dev-b");
        mb.deposit("room-a", "dev-a", "dev-b", "device-msg".into(), "n".into(), None, None, TEST_QUOTA, None, None)
            .unwrap();
        mb.deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "account-msg".into(), "n".into(), None, None, TEST_QUOTA, None, None)
            .unwrap();

        let msgs = mb.poll("room-a", "dev-b", None);
        assert_eq!(msgs.len(), 2);
        assert!(!msgs[0].account_level);
        assert!(msgs[1].account_level);
    }

    /// Messages survive a relay restart: write to a file, reopen, poll.
    #[test]
    fn messages_persist_across_reopen() {
        let dir = temp_dir("persist");
        let path = dir.0.join("mailbox.sqlite");

        {
            let mb = Mailbox::open(&path).unwrap();
            mb.deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "durable".into(), "n".into(), None, None, TEST_QUOTA, None, None)
                .unwrap();
        } // dropped = "relay restarted"
        {
            let mb = Mailbox::open(&path).unwrap();
            mb.ensure_device("room-a", "dev-b");
            let msgs = mb.poll("room-a", "dev-b", None);
            assert_eq!(msgs.len(), 1, "message must survive the restart");
            assert_eq!(msgs[0].ciphertext, "durable");
        }
    }

    /// Re-depositing with the same client message_id is idempotent: the store
    /// keeps ONE row and reports success, so a sender can safely retry.
    #[test]
    fn deposit_is_idempotent_for_same_message_id() {
        let (_dir, mb) = temp_mailbox("idem");
        mb.ensure_device("room-a", "dev-a");
        mb.ensure_device("room-a", "dev-b");
        let client_id = "client-msg-1".to_string();
        mb.deposit("room-a", "dev-a", "dev-b", "c".into(), "n".into(), None, Some(client_id.clone()), TEST_QUOTA, None, None)
            .unwrap();
        let id2 = mb
            .deposit("room-a", "dev-a", "dev-b", "c".into(), "n".into(), None, Some(client_id.clone()), TEST_QUOTA, None, None)
            .unwrap();
        assert_eq!(id2, client_id);
        assert_eq!(mb.poll("room-a", "dev-b", None).len(), 1, "duplicate id must not double-store");
    }

    /// Expired messages are pruned on the next operation.
    #[test]
    fn expired_messages_are_pruned() {
        let (_dir, mb) = temp_mailbox("expiry");
        mb.ensure_device("room-a", "dev-a");
        mb.ensure_device("room-a", "dev-b");
        mb.deposit("room-a", "dev-a", "dev-b", "c".into(), "n".into(), Some(1), None, TEST_QUOTA, None, None)
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        assert!(mb.poll("room-a", "dev-b", None).is_empty(), "expired message must be pruned");
    }

    /// Poll delivers but never consumes: the row stays stored (stamped with
    /// `delivered_at`) until the client acks it.
    #[test]
    fn poll_does_not_consume_per_device_messages() {
        let (_dir, mb) = temp_mailbox("markonly");
        mb.ensure_device("room-a", "dev-a");
        mb.ensure_device("room-a", "dev-b");
        mb.deposit("room-a", "dev-a", "dev-b", "c".into(), "n".into(), None, None, TEST_QUOTA, None, None)
            .unwrap();

        let msgs = mb.poll("room-a", "dev-b", None);
        assert_eq!(msgs.len(), 1);
        assert_eq!(row_count(&mb, "room-a", "dev-b"), 1, "poll must not delete the row");

        let delivered_at: Option<i64> = {
            let conn = mb.conn.lock().unwrap();
            conn.query_row(
                "SELECT delivered_at FROM mailbox_messages WHERE id = ?1",
                rusqlite::params![&msgs[0].id],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert!(delivered_at.is_some(), "poll must stamp delivered_at");
    }

    /// Ack deletes the per-device row; repeating the ack (or acking an unknown
    /// id) is an idempotent no-op.
    #[test]
    fn ack_deletes_per_device_row() {
        let (_dir, mb) = temp_mailbox("ackdel");
        mb.ensure_device("room-a", "dev-a");
        mb.ensure_device("room-a", "dev-b");
        mb.deposit("room-a", "dev-a", "dev-b", "c".into(), "n".into(), None, None, TEST_QUOTA, None, None)
            .unwrap();

        let msgs = mb.poll("room-a", "dev-b", None);
        assert_eq!(msgs.len(), 1);
        mb.ack("room-a", "dev-b", &[msgs[0].id.clone()]);
        assert_eq!(row_count(&mb, "room-a", "dev-b"), 0, "ack must delete the row");

        mb.ack("room-a", "dev-b", &[msgs[0].id.clone()]);
        mb.ack("room-a", "dev-b", &["never-existed".to_string()]);
        assert_eq!(row_count(&mb, "room-a", "dev-b"), 0, "repeated/unknown ack must be a no-op");
        assert!(mb.poll("room-a", "dev-b", None).is_empty());
    }

    /// An un-acked delivery is not re-polled inside the redelivery window, but
    /// becomes eligible again once the window has passed.
    #[test]
    fn unacked_message_is_redelivered_after_window() {
        let (_dir, mb) = temp_mailbox("redeliver");
        mb.ensure_device("room-a", "dev-a");
        mb.ensure_device("room-a", "dev-b");
        mb.deposit("room-a", "dev-a", "dev-b", "c".into(), "n".into(), None, None, TEST_QUOTA, None, None)
            .unwrap();

        let msgs = mb.poll("room-a", "dev-b", None);
        assert_eq!(msgs.len(), 1);

        // No ack: inside the window the message is not re-polled.
        assert!(mb.poll("room-a", "dev-b", None).is_empty());

        // Age the delivery stamp beyond the window → redelivered.
        {
            let conn = mb.conn.lock().unwrap();
            conn.execute(
                "UPDATE mailbox_messages SET delivered_at = ?1 WHERE id = ?2",
                rusqlite::params![now() - REDELIVERY_WINDOW_SECS - 1, &msgs[0].id],
            )
            .unwrap();
        }
        let again = mb.poll("room-a", "dev-b", None);
        assert_eq!(again.len(), 1, "message must be redelivered after the window");
        assert_eq!(again[0].id, msgs[0].id, "same id, so the client can dedupe");
    }

    /// A per-device row delivered by poll but not yet acked survives a relay
    /// restart, and the late ack still deletes it after reopen.
    #[test]
    fn per_device_message_persists_across_reopen_until_acked() {
        let dir = temp_dir("persist-device");
        let path = dir.0.join("mailbox.sqlite");

        let id;
        {
            let mb = Mailbox::open(&path).unwrap();
            mb.ensure_device("room-a", "dev-a");
            mb.ensure_device("room-a", "dev-b");
            id = mb
                .deposit("room-a", "dev-a", "dev-b", "durable".into(), "n".into(), None, None, TEST_QUOTA, None, None)
                .unwrap();
            assert_eq!(mb.poll("room-a", "dev-b", None).len(), 1);
        } // dropped = "relay restarted" after poll, before the client's ack
        {
            let mb = Mailbox::open(&path).unwrap();
            mb.ensure_device("room-a", "dev-b");
            // Still inside the redelivery window: not re-polled yet…
            assert!(mb.poll("room-a", "dev-b", None).is_empty());
            // …but the row survived and the late ack deletes it.
            mb.ack("room-a", "dev-b", std::slice::from_ref(&id));
            assert_eq!(row_count(&mb, "room-a", "dev-b"), 0);
        }
    }

    /// An empty `message_id` means "no client correlation id", not the id "" —
    /// otherwise one such deposit would swallow every later one (id UNIQUE).
    #[test]
    fn empty_message_id_is_treated_as_none() {
        let (_dir, mb) = temp_mailbox("emptyid");
        mb.ensure_device("room-a", "dev-a");
        mb.ensure_device("room-a", "dev-b");
        let id1 = mb
            .deposit("room-a", "dev-a", "dev-b", "c1".into(), "n".into(), None, Some(String::new()), TEST_QUOTA, None, None)
            .unwrap();
        let id2 = mb
            .deposit("room-a", "dev-a", "dev-b", "c2".into(), "n".into(), None, Some(String::new()), TEST_QUOTA, None, None)
            .unwrap();
        assert!(!id1.is_empty() && !id2.is_empty());
        assert_ne!(id1, id2, "empty message_id must not collapse deposits into one row");
        assert_eq!(mb.poll("room-a", "dev-b", None).len(), 2);
    }

    /// The `seen` list stops growing at MAX_SEEN_ENTRIES; devices past the cap
    /// still receive the message (and may re-fetch it — clients dedupe by id).
    #[test]
    fn seen_list_is_capped() {
        let (_dir, mb) = temp_mailbox("seencap");
        mb.deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "c".into(), "n".into(), None, None, TEST_QUOTA, None, None)
            .unwrap();

        for i in 0..(MAX_SEEN_ENTRIES + 20) {
            let dev = format!("dev-{i}");
            mb.ensure_device("room-a", &dev);
            let msgs = mb.poll("room-a", &dev, None);
            assert_eq!(msgs.len(), 1, "every device still gets the message");
            let ids: Vec<String> = msgs.iter().map(|m| m.id.clone()).collect();
            mb.ack("room-a", &dev, &ids);
        }
        let seen: String = {
            let conn = mb.conn.lock().unwrap();
            conn.query_row(
                "SELECT seen FROM mailbox_messages WHERE to_device_id = '' LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        let count = seen.split(',').filter(|s| !s.is_empty()).count();
        assert_eq!(count, MAX_SEEN_ENTRIES, "seen list must stop growing at the cap");
    }

    /// Poll must NOT mark account-level messages seen: if the batch is lost
    /// after the poll (oversized WS frame, disconnect), the next poll from the
    /// same device returns the same messages again.
    #[test]
    fn poll_does_not_mark_account_messages_seen() {
        let (_dir, mb) = temp_mailbox("pollseen");
        mb.deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "c1".into(), "n".into(), None, None, TEST_QUOTA, None, None)
            .unwrap();
        mb.ensure_device("room-a", "dev-b");

        let first = mb.poll("room-a", "dev-b", None);
        assert_eq!(first.len(), 1);
        let second = mb.poll("room-a", "dev-b", None);
        assert_eq!(second.len(), 1, "un-acked account message must be re-polled");
        assert_eq!(second[0].id, first[0].id);

        // The `seen` column still only names the depositor.
        let seen: String = {
            let conn = mb.conn.lock().unwrap();
            conn.query_row(
                "SELECT seen FROM mailbox_messages WHERE id = ?1",
                rusqlite::params![&first[0].id],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert!(!seen.contains("dev-b"), "poll must not append to seen");
    }

    /// Ack on an account-level message marks it seen for the acking device
    /// only: that device's polls skip it, another device still receives it,
    /// and the shared row is never deleted.
    #[test]
    fn ack_marks_account_message_seen_for_that_device_only() {
        let (_dir, mb) = temp_mailbox("ackseen");
        mb.deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "c1".into(), "n".into(), None, None, TEST_QUOTA, None, None)
            .unwrap();
        mb.ensure_device("room-a", "dev-b");
        mb.ensure_device("room-a", "dev-c");

        let msgs = mb.poll("room-a", "dev-b", None);
        assert_eq!(msgs.len(), 1);
        mb.ack("room-a", "dev-b", &[msgs[0].id.clone()]);

        assert!(mb.poll("room-a", "dev-b", None).is_empty(), "acked device must not re-receive");
        assert_eq!(mb.poll("room-a", "dev-c", None).len(), 1, "other devices still receive it");
        assert_eq!(row_count(&mb, "room-a", ACCOUNT_LEVEL_TARGET), 1, "ack must not delete the shared row");

        // Repeated ack is an idempotent no-op.
        mb.ack("room-a", "dev-b", &[msgs[0].id.clone()]);
        assert_eq!(row_count(&mb, "room-a", ACCOUNT_LEVEL_TARGET), 1);
    }

    /// Mixed ack in one call: the per-device row is deleted, the account-level
    /// row is marked seen — matching the client, which acks every message id
    /// of an applied batch.
    #[test]
    fn ack_handles_mixed_device_and_account_ids() {
        let (_dir, mb) = temp_mailbox("ackmixed");
        mb.ensure_device("room-a", "dev-a");
        mb.ensure_device("room-a", "dev-b");
        mb.deposit("room-a", "dev-a", "dev-b", "device-c".into(), "n".into(), None, None, TEST_QUOTA, None, None)
            .unwrap();
        mb.deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "account-c".into(), "n".into(), None, None, TEST_QUOTA, None, None)
            .unwrap();

        let msgs = mb.poll("room-a", "dev-b", None);
        assert_eq!(msgs.len(), 2);
        let ids: Vec<String> = msgs.iter().map(|m| m.id.clone()).collect();
        mb.ack("room-a", "dev-b", &ids);

        assert_eq!(row_count(&mb, "room-a", "dev-b"), 0, "per-device row must be deleted");
        assert_eq!(row_count(&mb, "room-a", ACCOUNT_LEVEL_TARGET), 1, "account row must survive");
        assert!(mb.poll("room-a", "dev-b", None).is_empty(), "both must be gone from dev-b's polls");
    }

    /// A new deposit that would push the room's usage past its quota is
    /// rejected with "quota_exceeded"; existing data is left untouched.
    #[test]
    fn deposit_over_quota_is_rejected_without_eviction() {
        let (_dir, mb) = temp_mailbox("quota");
        let quota: i64 = 100;
        // Each message costs ciphertext + nonce bytes (5 + 1 = 6).
        mb.deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "aaaaa".into(), "n".into(), None, None, quota, None, None)
            .unwrap();
        let err = mb
            .deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "x".repeat(200), "n".into(), None, None, quota, None, None)
            .unwrap_err();
        assert_eq!(err, "quota_exceeded");
        // The oversized message was NOT stored and the old one survived.
        assert_eq!(mb.usage_bytes("room-a"), 6);
        mb.ensure_device("room-a", "dev-b");
        let msgs = mb.poll("room-a", "dev-b", None);
        assert_eq!(msgs.len(), 1, "existing data must survive quota rejection");
        assert_eq!(msgs[0].ciphertext, "aaaaa");
    }

    /// Idempotent replay of an already-stored message succeeds even when the
    /// room is over quota — it adds no bytes, and rejecting it would wedge
    /// the client's outbox on its first message.
    #[test]
    fn idempotent_replay_bypasses_quota() {
        let (_dir, mb) = temp_mailbox("quota-replay");
        let quota: i64 = 10;
        let id = mb
            .deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "aaaaa".into(), "n".into(), None, Some("m-1".into()), quota, None, None)
            .unwrap();
        assert_eq!(id, "m-1");
        // Usage (6) is already over a tighter quota of 1: a fresh deposit is
        // rejected, but replaying the stored id still succeeds.
        let err = mb
            .deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "b".into(), "n".into(), None, Some("m-2".into()), 1, None, None)
            .unwrap_err();
        assert_eq!(err, "quota_exceeded");
        let replayed = mb
            .deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "aaaaa".into(), "n".into(), None, Some("m-1".into()), 1, None, None)
            .unwrap();
        assert_eq!(replayed, "m-1");
        assert_eq!(mb.usage_bytes("room-a"), 6, "replay must not double-store");
    }

    /// Two deposits carrying the same dedup key (two devices pushing the same
    /// blob) store ONE copy; the second is acknowledged without touching the
    /// quota.
    #[test]
    fn dedup_key_prevents_double_storage() {
        let (_dir, mb) = temp_mailbox("dedup");
        mb.ensure_device("room-a", "dev-a");
        mb.ensure_device("room-a", "dev-b");
        let quota: i64 = 100;

        mb.deposit(
            "room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "blob-cipher".into(), "n".into(),
            None, None, quota, Some("dk-1".into()), None,
        )
        .unwrap();
        let id2 = mb
            .deposit(
                "room-a", "dev-b", ACCOUNT_LEVEL_TARGET, "blob-cipher".into(), "n".into(),
                None, None, quota, Some("dk-1".into()), None,
            )
            .unwrap();
        assert!(!id2.is_empty(), "deduped deposit is still acknowledged");
        assert_eq!(
            row_count(&mb, "room-a", ACCOUNT_LEVEL_TARGET),
            1,
            "same dedup key must not double-store"
        );

        // A different key stores normally; no key at all is never deduped.
        mb.deposit(
            "room-a", "dev-b", ACCOUNT_LEVEL_TARGET, "other".into(), "n".into(),
            None, None, quota, Some("dk-2".into()), None,
        )
        .unwrap();
        mb.deposit(
            "room-a", "dev-b", ACCOUNT_LEVEL_TARGET, "other".into(), "n".into(),
            None, None, quota, None, None,
        )
        .unwrap();
        assert_eq!(row_count(&mb, "room-a", ACCOUNT_LEVEL_TARGET), 3);
    }

    /// A dedup hit succeeds even when the room is over quota — it stores
    /// nothing, so rejecting it would only confuse the sender into retrying.
    #[test]
    fn dedup_hit_bypasses_quota() {
        let (_dir, mb) = temp_mailbox("dedup-quota");
        mb.deposit(
            "room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "aaaaa".into(), "n".into(),
            None, None, TEST_QUOTA, Some("dk-1".into()), None,
        )
        .unwrap();
        // Room usage is 6, quota 1: fresh deposits fail, dedup hit succeeds.
        let err = mb
            .deposit(
                "room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "b".into(), "n".into(),
                None, None, 1, Some("dk-other".into()), None,
            )
            .unwrap_err();
        assert_eq!(err, "quota_exceeded");
        mb.deposit(
            "room-a", "dev-b", ACCOUNT_LEVEL_TARGET, "aaaaa".into(), "n".into(),
            None, None, 1, Some("dk-1".into()), None,
        )
        .unwrap();
        assert_eq!(row_count(&mb, "room-a", ACCOUNT_LEVEL_TARGET), 1);
    }

    /// An expired dedup key no longer dedupes (it prunes with the messages).
    #[test]
    fn expired_dedup_key_stops_deduping() {
        let (_dir, mb) = temp_mailbox("dedup-expiry");
        mb.deposit(
            "room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "c".into(), "n".into(),
            Some(1), None, TEST_QUOTA, Some("dk-1".into()), None,
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        mb.deposit(
            "room-a", "dev-b", ACCOUNT_LEVEL_TARGET, "c".into(), "n".into(),
            None, None, TEST_QUOTA, Some("dk-1".into()), None,
        )
        .unwrap();
        assert_eq!(row_count(&mb, "room-a", ACCOUNT_LEVEL_TARGET), 1, "expired key must not dedup");
    }

    /// Quota accounting spans the account archive AND every per-device queue:
    /// `usage_bytes` sums both, and per-device deposits count against it.
    #[test]
    fn usage_bytes_counts_archive_and_device_queues() {
        let (_dir, mb) = temp_mailbox("usage");
        mb.ensure_device("room-a", "dev-a");
        mb.ensure_device("room-a", "dev-b");
        assert_eq!(mb.usage_bytes("room-a"), 0);
        // Archive: 2 + 1 = 3 bytes.
        mb.deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "aa".into(), "n".into(), None, None, TEST_QUOTA, None, None)
            .unwrap();
        // Per-device queue: 3 + 1 = 4 bytes.
        mb.deposit("room-a", "dev-a", "dev-b", "bbb".into(), "n".into(), None, None, TEST_QUOTA, None, None)
            .unwrap();
        assert_eq!(mb.usage_bytes("room-a"), 7);
        // A per-device deposit is rejected once usage + incoming > quota.
        let err = mb
            .deposit("room-a", "dev-a", "dev-b", "c".into(), "n".into(), None, None, 8, None, None)
            .unwrap_err();
        assert_eq!(err, "quota_exceeded", "per-device queues count against the quota");
        // Other rooms are unaffected.
        assert_eq!(mb.usage_bytes("room-b"), 0);
    }

    // ── State slots (protocol v3) ──────────────────────────────────────────

    fn slot(group: &str, index: u32, total: u32) -> Option<SlotMeta> {
        Some(SlotMeta {
            group: group.into(),
            index,
            total,
        })
    }

    /// expires_at of every account-level row of a sender, ordered by seq.
    fn account_rows(mb: &Mailbox, room_id: &str, from: &str) -> Vec<(String, u64)> {
        let conn = mb.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT ciphertext, expires_at FROM mailbox_messages
                 WHERE room_id = ?1 AND from_device_id = ?2 AND to_device_id = ''
                 ORDER BY seq",
            )
            .unwrap();
        stmt.query_map(rusqlite::params![room_id, from], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
    }

    /// Committing a checkpoint group makes its slices permanent, deletes the
    /// sender's older generation, and purges WAL rows older than the group.
    #[test]
    fn slot_commit_replaces_generation_and_purges_wal() {
        let (_dir, mb) = temp_mailbox("slot-commit");
        // WAL rows (no ttl → persistent) and a completed old generation.
        mb.deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "wal-1".into(), "n".into(), None, None, TEST_QUOTA, None, None)
            .unwrap();
        mb.deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "old-state".into(), "n".into(), None, None, TEST_QUOTA, None, slot("gen-1", 0, 1))
            .unwrap();
        mb.deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "wal-2".into(), "n".into(), None, None, TEST_QUOTA, None, None)
            .unwrap();
        // Sanity: gen-1 committed immediately (single slice) and already
        // purged the WAL it covered (wal-1).
        assert_eq!(
            account_rows(&mb, "room-a", "dev-a"),
            vec![
                ("old-state".to_string(), NEVER_EXPIRES),
                ("wal-2".to_string(), NEVER_EXPIRES),
            ]
        );

        // New two-slice checkpoint; WAL grows while it uploads.
        mb.deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "new-s0".into(), "n".into(), None, None, TEST_QUOTA, None, slot("gen-2", 0, 2))
            .unwrap();
        mb.deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "wal-3".into(), "n".into(), None, None, TEST_QUOTA, None, None)
            .unwrap();

        // Final slice commits the group: old generation and the WAL it
        // covers (wal-1, wal-2) are gone; wal-3 (deposited after the group
        // started) survives; the group is permanent.
        mb.deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "new-s1".into(), "n".into(), None, None, TEST_QUOTA, None, slot("gen-2", 1, 2))
            .unwrap();
        assert_eq!(
            account_rows(&mb, "room-a", "dev-a"),
            vec![
                ("new-s0".to_string(), NEVER_EXPIRES),
                ("wal-3".to_string(), NEVER_EXPIRES),
                ("new-s1".to_string(), NEVER_EXPIRES),
            ]
        );
    }

    /// An incomplete group changes nothing: WAL and the old generation stay.
    #[test]
    fn slot_incomplete_group_is_inert() {
        let (_dir, mb) = temp_mailbox("slot-partial");
        mb.deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "old-state".into(), "n".into(), None, None, TEST_QUOTA, None, slot("gen-1", 0, 1))
            .unwrap();
        mb.deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "wal-1".into(), "n".into(), None, None, TEST_QUOTA, None, None)
            .unwrap();
        // Slice 0 of 2 arrives; slice 1 never does.
        mb.deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "new-s0".into(), "n".into(), None, None, TEST_QUOTA, None, slot("gen-2", 0, 2))
            .unwrap();
        let rows = account_rows(&mb, "room-a", "dev-a");
        assert_eq!(
            rows.iter().map(|(c, _)| c.as_str()).collect::<Vec<_>>(),
            vec!["old-state", "wal-1", "new-s0"],
            "incomplete group must not purge anything"
        );
        // The uncommitted slice keeps a finite TTL so it cleans itself up.
        let new_exp = rows[2].1;
        assert!(new_exp < NEVER_EXPIRES && new_exp > now());
    }

    /// Slices may arrive out of order or be retried: the commit fires when
    /// the LAST MISSING slice lands, not necessarily the highest index.
    #[test]
    fn slot_commit_fires_on_completing_slice() {
        let (_dir, mb) = temp_mailbox("slot-ooo");
        mb.deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "wal-1".into(), "n".into(), None, None, TEST_QUOTA, None, None)
            .unwrap();
        // Final index first — group incomplete, nothing purged.
        mb.deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "s1".into(), "n".into(), None, None, TEST_QUOTA, None, slot("g", 1, 2))
            .unwrap();
        assert_eq!(account_rows(&mb, "room-a", "dev-a").len(), 2);
        // The missing slice completes the group → commit.
        mb.deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "s0".into(), "n".into(), None, Some("s0-msg".into()), TEST_QUOTA, None, slot("g", 0, 2))
            .unwrap();
        let rows = account_rows(&mb, "room-a", "dev-a");
        assert_eq!(
            rows.iter().map(|(c, _)| c.as_str()).collect::<Vec<_>>(),
            vec!["s1", "s0"],
            "covered WAL purged; group slices kept"
        );
        assert!(rows.iter().all(|(_, e)| *e == NEVER_EXPIRES));
        // Retrying the slice with the SAME message id is an idempotent no-op.
        mb.deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "s0".into(), "n".into(), None, Some("s0-msg".into()), TEST_QUOTA, None, slot("g", 0, 2))
            .unwrap();
        assert_eq!(account_rows(&mb, "room-a", "dev-a").len(), 2);
    }

    /// Slot deposits are account-level only and need sane meta.
    #[test]
    fn slot_validation() {
        let (_dir, mb) = temp_mailbox("slot-validate");
        mb.ensure_device("room-a", "dev-b");
        let err = mb
            .deposit("room-a", "dev-a", "dev-b", "c".into(), "n".into(), None, None, TEST_QUOTA, None, slot("g", 0, 1))
            .unwrap_err();
        assert!(err.contains("account-level"));
        for bad in [slot("", 0, 1), slot("g", 0, 0), slot("g", 2, 2)] {
            assert!(mb
                .deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "c".into(), "n".into(), None, None, TEST_QUOTA, None, bad)
                .is_err());
        }
    }

    /// Account-archive WAL rows that omit `ttl_seconds` never expire and are
    /// NOT count-capped — the byte quota is the only bound. An explicit TTL
    /// (legacy clients) is still honored and capped at 7 days.
    #[test]
    fn account_wal_persistent_and_uncapped() {
        let (_dir, mb) = temp_mailbox("wal-persistent");
        for i in 0..2100 {
            mb.deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, format!("c{i}"), "n".into(), None, None, TEST_QUOTA, None, None)
                .unwrap();
        }
        assert_eq!(
            row_count(&mb, "room-a", ACCOUNT_LEVEL_TARGET),
            2100,
            "no silent count cap on the account archive"
        );
        let rows = account_rows(&mb, "room-a", "dev-a");
        assert!(rows.iter().all(|(_, e)| *e == NEVER_EXPIRES));

        // Legacy explicit TTL: capped at 7 days.
        mb.deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "legacy".into(), "n".into(), Some(999 * 24 * 3600), None, TEST_QUOTA, None, None)
            .unwrap();
        let legacy_exp = account_rows(&mb, "room-a", "dev-a").pop().unwrap().1;
        assert_eq!(legacy_exp, now() + DEFAULT_TTL_SECONDS);
    }

    /// Blob messages (dedup_key set) are not WAL: a checkpoint commit must
    /// not purge them — a checkpoint covers row changes, never blobs.
    #[test]
    fn slot_commit_preserves_blob_rows() {
        let (_dir, mb) = temp_mailbox("slot-blob");
        mb.deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "blob".into(), "n".into(), Some(7 * 24 * 3600), None, TEST_QUOTA, Some("dk-blob".into()), None)
            .unwrap();
        mb.deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "wal-1".into(), "n".into(), None, None, TEST_QUOTA, None, None)
            .unwrap();
        mb.deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "state".into(), "n".into(), None, None, TEST_QUOTA, None, slot("g", 0, 1))
            .unwrap();
        let rows = account_rows(&mb, "room-a", "dev-a");
        assert_eq!(
            rows.iter().map(|(c, _)| c.as_str()).collect::<Vec<_>>(),
            vec!["blob", "state"],
            "blob row survives the checkpoint's WAL purge"
        );
    }

    /// A pre-v3 database (current base schema, no slot columns) migrates in
    /// place: the slot index is created only after the ALTERs.
    #[test]
    fn legacy_db_migrates_to_slot_schema() {
        let dir = temp_dir("slot-migrate");
        let path = dir.0.join("mailbox.sqlite");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE mailbox_messages (
                    seq           INTEGER PRIMARY KEY AUTOINCREMENT,
                    id            TEXT NOT NULL UNIQUE,
                    room_id       TEXT NOT NULL,
                    to_device_id  TEXT NOT NULL DEFAULT '',
                    from_device_id TEXT NOT NULL,
                    ciphertext    TEXT NOT NULL,
                    nonce         TEXT NOT NULL,
                    expires_at    INTEGER NOT NULL,
                    created_at    INTEGER NOT NULL,
                    delivered_at  INTEGER,
                    seen          TEXT NOT NULL DEFAULT ''
                );",
            )
            .unwrap();
        }
        let mb = Mailbox::open(&path).unwrap();
        mb.deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "state".into(), "n".into(), None, None, TEST_QUOTA, None, slot("g", 0, 1))
            .unwrap();
        let rows = account_rows(&mb, "room-a", "dev-a");
        assert_eq!(rows, vec![("state".to_string(), NEVER_EXPIRES)]);
    }

    /// A checkpoint that replaces a large old generation must not be rejected
    /// for transiently double-counting the outgoing generation's bytes.
    #[test]
    fn slot_quota_check_discounts_replaced_generation() {
        let (_dir, mb) = temp_mailbox("slot-quota");
        let big = "x".repeat(100);
        // Committed generation: 101 bytes.
        mb.deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, big.clone(), "n".into(), None, None, TEST_QUOTA, None, slot("gen-1", 0, 1))
            .unwrap();
        // Quota barely above one generation: the replacement still fits
        // because the outgoing generation is discounted.
        let quota = 105;
        mb.deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, big, "n".into(), None, None, quota, None, slot("gen-2", 0, 1))
            .unwrap();
        assert_eq!(account_rows(&mb, "room-a", "dev-a").len(), 1);
        // A plain WAL row does NOT get the discount and is rejected.
        let err = mb
            .deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "y".repeat(100), "n".into(), None, None, quota, None, None)
            .unwrap_err();
        assert_eq!(err, "quota_exceeded");
    }
}
