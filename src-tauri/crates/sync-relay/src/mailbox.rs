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
const MAX_ACCOUNT_MESSAGES: usize = 2000;

/// Byte quota for the account-level archive (summed `length(ciphertext)` per
/// room). Full-snapshot sync messages run to ~10MB each, so the 2000-row cap
/// alone allowed hundreds of MB to accumulate — more than a single WebSocket
/// frame can ever deliver. Oldest rows are evicted first; eviction is logged
/// because a dropped row may never have been seen by some device.
pub const ACCOUNT_ARCHIVE_MAX_BYTES: usize = 100 * 1024 * 1024;

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
    seen          TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_mailbox_room_to ON mailbox_messages(room_id, to_device_id);
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

    /// Enforce the account archive byte quota: while the summed ciphertext
    /// length exceeds `max_bytes`, delete rows oldest-first. Evictions are
    /// logged — an evicted row may be a message some device never saw.
    fn enforce_account_archive_quota(
        conn: &mut rusqlite::Connection,
        room_id: &str,
        max_bytes: usize,
    ) -> Result<(), String> {
        let total: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(LENGTH(ciphertext)), 0) FROM mailbox_messages
                 WHERE room_id = ?1 AND to_device_id = ''",
                rusqlite::params![room_id],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if total <= max_bytes as i64 {
            return Ok(());
        }
        let rows: Vec<(i64, i64)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT seq, LENGTH(ciphertext) FROM mailbox_messages
                     WHERE room_id = ?1 AND to_device_id = '' ORDER BY seq ASC",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(rusqlite::params![room_id], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            rows
        };
        let mut remaining = total;
        let mut evicted_rows = 0usize;
        let mut evicted_bytes = 0i64;
        for (seq, len) in rows {
            if remaining <= max_bytes as i64 {
                break;
            }
            conn.execute(
                "DELETE FROM mailbox_messages WHERE seq = ?1",
                rusqlite::params![seq],
            )
            .map_err(|e| e.to_string())?;
            remaining -= len;
            evicted_rows += 1;
            evicted_bytes += len;
        }
        if evicted_rows > 0 {
            tracing::warn!(
                room_id = %room_id,
                evicted_rows,
                evicted_bytes,
                remaining_bytes = remaining,
                "account archive byte quota evicted oldest messages (possibly unseen by some devices)"
            );
        }
        Ok(())
    }

    /// Deposit a ciphertext message. An empty `to_device_id` stores it in the
    /// account-level archive (no target device needs to exist or be online);
    /// otherwise it goes to that device's queue (target must have joined).
    /// `message_id` is the client's correlation id: when present it is used as
    /// the stored id, and re-depositing the same id is an idempotent no-op —
    /// this is what makes "retry until acknowledged" safe. Returns the stored
    /// message id.
    pub fn deposit(
        &self,
        room_id: &str,
        from_device_id: &str,
        to_device_id: &str,
        ciphertext: String,
        nonce: String,
        ttl_seconds: Option<u64>,
        message_id: Option<String>,
    ) -> Result<String, String> {
        let id = message_id
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let ttl = ttl_seconds.unwrap_or(DEFAULT_TTL_SECONDS).min(DEFAULT_TTL_SECONDS);
        let ts = now();
        let expires_at = ts + ttl;

        if to_device_id != ACCOUNT_LEVEL_TARGET {
            let registered = self.registered.lock().unwrap();
            if !registered.contains(&(room_id.to_string(), to_device_id.to_string())) {
                return Err("target device not in room".to_string());
            }
        }

        let mut conn = self.conn.lock().unwrap();
        Self::prune(&mut conn, ts);

        let inserted = conn
            .execute(
                "INSERT OR IGNORE INTO mailbox_messages
                 (id, room_id, to_device_id, from_device_id, ciphertext, nonce, expires_at, created_at, seen)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, '')",
                rusqlite::params![
                    id,
                    room_id,
                    to_device_id,
                    from_device_id,
                    ciphertext,
                    nonce,
                    expires_at,
                    ts
                ],
            )
            .map_err(|e| e.to_string())?;
        if inserted == 0 {
            // Already stored (client retried with the same id) — idempotent
            // success so the sender can safely advance its cursor.
            return Ok(id);
        }

        if to_device_id == ACCOUNT_LEVEL_TARGET {
            // The depositor has already "seen" it (they created it).
            Self::append_seen(&conn, &id, from_device_id);
            Self::trim(&mut conn, room_id, ACCOUNT_LEVEL_TARGET, MAX_ACCOUNT_MESSAGES)?;
            Self::enforce_account_archive_quota(&mut conn, room_id, ACCOUNT_ARCHIVE_MAX_BYTES)?;
        } else {
            Self::trim(&mut conn, room_id, to_device_id, MAX_MESSAGES_PER_DEVICE)?;
        }
        Ok(id)
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
                .deposit("room-a", "dev-a", "dev-b", "cipher-1".into(), "nonce-1".into(), None, None)
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
            .deposit("room-a", "dev-a", "ghost", "c".into(), "n".into(), None, None)
            .unwrap_err();
        assert!(err.contains("not in room"));
    }

    #[test]
    fn quota_fifo_drops_oldest() {
        let (_dir, mb) = temp_mailbox("quota");
        mb.ensure_device("room-a", "dev-a");
        mb.ensure_device("room-a", "dev-b");
        for i in 0..600 {
            mb.deposit("room-a", "dev-a", "dev-b", format!("c{i}"), "n".into(), None, None)
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
        mb.deposit("room-a", "dev-a", "dev-b", "c1".into(), "n1".into(), None, None)
            .unwrap();
        mb.deposit("room-a", "dev-a", "dev-b", "c2".into(), "n2".into(), None, None)
            .unwrap();

        let msgs = mb.poll("room-a", "dev-b", None);
        assert_eq!(msgs.len(), 2);
        mb.ack("room-a", "dev-b", &[msgs[0].id.clone()]);
        // ack deletes the delivered row; the other delivered-but-unacked
        // message is not re-polled inside the redelivery window
        mb.deposit("room-a", "dev-a", "dev-b", "c3".into(), "n3".into(), None, None)
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
        mb.deposit("room-a", "dev-a", "dev-b", "device-msg".into(), "n".into(), None, None)
            .unwrap();
        mb.deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "account-msg".into(), "n".into(), None, None)
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
            mb.deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "durable".into(), "n".into(), None, None)
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
        mb.deposit("room-a", "dev-a", "dev-b", "c".into(), "n".into(), None, Some(client_id.clone()))
            .unwrap();
        let id2 = mb
            .deposit("room-a", "dev-a", "dev-b", "c".into(), "n".into(), None, Some(client_id.clone()))
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
        mb.deposit("room-a", "dev-a", "dev-b", "c".into(), "n".into(), Some(1), None)
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
        mb.deposit("room-a", "dev-a", "dev-b", "c".into(), "n".into(), None, None)
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
        mb.deposit("room-a", "dev-a", "dev-b", "c".into(), "n".into(), None, None)
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
        mb.deposit("room-a", "dev-a", "dev-b", "c".into(), "n".into(), None, None)
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
                .deposit("room-a", "dev-a", "dev-b", "durable".into(), "n".into(), None, None)
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
            .deposit("room-a", "dev-a", "dev-b", "c1".into(), "n".into(), None, Some(String::new()))
            .unwrap();
        let id2 = mb
            .deposit("room-a", "dev-a", "dev-b", "c2".into(), "n".into(), None, Some(String::new()))
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
        mb.deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "c".into(), "n".into(), None, None)
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
        mb.deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "c1".into(), "n".into(), None, None)
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
        mb.deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "c1".into(), "n".into(), None, None)
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
        mb.deposit("room-a", "dev-a", "dev-b", "device-c".into(), "n".into(), None, None)
            .unwrap();
        mb.deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "account-c".into(), "n".into(), None, None)
            .unwrap();

        let msgs = mb.poll("room-a", "dev-b", None);
        assert_eq!(msgs.len(), 2);
        let ids: Vec<String> = msgs.iter().map(|m| m.id.clone()).collect();
        mb.ack("room-a", "dev-b", &ids);

        assert_eq!(row_count(&mb, "room-a", "dev-b"), 0, "per-device row must be deleted");
        assert_eq!(row_count(&mb, "room-a", ACCOUNT_LEVEL_TARGET), 1, "account row must survive");
        assert!(mb.poll("room-a", "dev-b", None).is_empty(), "both must be gone from dev-b's polls");
    }

    /// The account archive byte quota evicts the oldest rows once the summed
    /// ciphertext length exceeds it, converging back under the quota.
    #[test]
    fn account_archive_byte_quota_evicts_oldest() {
        let (_dir, mb) = temp_mailbox("bytequota");
        // 3 × 40MB > 100MB quota → the oldest 40MB row is evicted.
        let chunk = "x".repeat(40 * 1024 * 1024);
        let id1 = mb
            .deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, chunk.clone(), "n".into(), None, None)
            .unwrap();
        let id2 = mb
            .deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, chunk.clone(), "n".into(), None, None)
            .unwrap();
        let id3 = mb
            .deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, chunk, "n".into(), None, None)
            .unwrap();

        let (count, total): (i64, i64) = {
            let conn = mb.conn.lock().unwrap();
            conn.query_row(
                "SELECT count(*), COALESCE(SUM(LENGTH(ciphertext)), 0) FROM mailbox_messages
                 WHERE room_id = 'room-a' AND to_device_id = ''",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
        };
        assert!(total <= ACCOUNT_ARCHIVE_MAX_BYTES as i64, "archive must converge under the quota");
        assert_eq!(count, 2, "exactly the oldest row must be evicted");

        // The survivors are the two newest deposits.
        mb.ensure_device("room-a", "dev-b");
        let msgs = mb.poll("room-a", "dev-b", Some(10));
        let ids: Vec<&str> = msgs.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, [id2.as_str(), id3.as_str()], "evicted row must be the oldest: {id1}");
    }
}
