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
        } else {
            Self::trim(&mut conn, room_id, to_device_id, MAX_MESSAGES_PER_DEVICE)?;
        }
        Ok(id)
    }

    /// Take up to `max_count` pending messages for `device_id`: its own queue
    /// (stamped `delivered_at`, deleted only on ack — redelivered after
    /// `REDELIVERY_WINDOW_SECS` if never acked) plus account-level messages it
    /// has not seen yet (marked seen). Account-level messages are shared, so a
    /// device can page through them by polling repeatedly.
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

        // 2) Account-level archive (shared; mark seen, don't remove).
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
            drop(stmt);
            for m in &rows {
                Self::append_seen(&conn, &m.id, device_id);
            }
            out.extend(rows);
        }
        out
    }

    /// Acknowledge per-device messages (removes them from the device queue).
    /// Account-level messages are shared and never removed by ack.
    pub fn ack(&self, room_id: &str, device_id: &str, message_ids: &[String]) {
        let conn = self.conn.lock().unwrap();
        for id in message_ids {
            let _ = conn.execute(
                "DELETE FROM mailbox_messages WHERE room_id = ?1 AND to_device_id = ?2 AND id = ?3",
                rusqlite::params![room_id, device_id, id],
            );
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

        // dev-b already consumed it (marked seen) → second poll is empty.
        assert!(mb.poll("room-a", "dev-b", None).is_empty());
    }

    /// Polling pages through a large account archive: each poll returns the
    /// next unseen slice; devices can drain the whole archive by polling.
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

        let first = mb.poll("room-a", "dev-b", Some(100));
        assert_eq!(first.len(), 100);
        let second = mb.poll("room-a", "dev-b", Some(100));
        assert_eq!(second.len(), 100);
        let third = mb.poll("room-a", "dev-b", Some(100));
        assert_eq!(third.len(), 50);
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
            assert_eq!(mb.poll("room-a", &dev, None).len(), 1, "every device still gets the message");
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
}
