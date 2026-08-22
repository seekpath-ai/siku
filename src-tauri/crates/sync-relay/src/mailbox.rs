//! Encrypted mailbox: per-device queues PLUS an account-level archive.
//! The service stores ciphertext only and never sees keys.
//!
//! Account-level messages are how offline delivery works for devices that do
//! not exist yet (or were never online): a device deposits its changes with an
//! empty `to_device_id`, any device of the account that later connects polls
//! the archive and applies them. Every device of the account sees the archive;
//! per-device "seen" marks keep each device from re-fetching the same message.

use crate::MailboxMessage;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

pub const DEFAULT_TTL_SECONDS: u64 = 7 * 24 * 3600; // 7 days
const MAX_MESSAGES_PER_DEVICE: usize = 500;
const MAX_ACCOUNT_MESSAGES: usize = 2000;

/// Empty `to_device_id` means "account-level archive" (any device of the
/// account may poll it).
pub const ACCOUNT_LEVEL_TARGET: &str = "";

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    id: String,
    from_device_id: String,
    ciphertext: String,
    nonce: String,
    expires_at: u64, // unix seconds
    /// Devices that already consumed this account-level message. Per-device
    /// messages are drained (consumed) instead.
    seen: HashSet<String>,
}

pub struct Mailbox {
    /// room_id -> device_id -> queue (per-device, poll = consume)
    inner: Mutex<HashMap<String, HashMap<String, Vec<Entry>>>>,
    /// room_id -> account-level archive (shared by all devices of the room)
    account: Mutex<HashMap<String, Vec<Entry>>>,
}

impl Default for Mailbox {
    fn default() -> Self {
        Self::new()
    }
}

impl Mailbox {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            account: Mutex::new(HashMap::new()),
        }
    }

    fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    fn prune(queue: &mut Vec<Entry>, now: u64) {
        queue.retain(|e| e.expires_at > now);
    }

    fn entry_to_message(e: &Entry, account_level: bool) -> MailboxMessage {
        MailboxMessage {
            id: e.id.clone(),
            from_device_id: e.from_device_id.clone(),
            ciphertext: e.ciphertext.clone(),
            nonce: e.nonce.clone(),
            account_level,
        }
    }

    /// Deposit a ciphertext message. An empty `to_device_id` stores it in the
    /// account-level archive (no target device needs to exist or be online);
    /// otherwise it goes to that device's queue (target must have joined).
    pub fn deposit(
        &self,
        room_id: &str,
        from_device_id: &str,
        to_device_id: &str,
        ciphertext: String,
        nonce: String,
        ttl_seconds: Option<u64>,
    ) -> Result<(), String> {
        let now = Self::now();
        let ttl = ttl_seconds.unwrap_or(DEFAULT_TTL_SECONDS).min(DEFAULT_TTL_SECONDS);
        let entry = Entry {
            id: Uuid::new_v4().to_string(),
            from_device_id: from_device_id.to_string(),
            ciphertext,
            nonce,
            expires_at: now + ttl,
            seen: HashSet::new(),
        };

        if to_device_id == ACCOUNT_LEVEL_TARGET {
            // Account-level archive.
            let mut account = self.account.lock().unwrap();
            let queue = account.entry(room_id.to_string()).or_default();
            Self::prune(queue, now);
            // The depositor has already "seen" it (they created it).
            let mut e = entry;
            e.seen.insert(from_device_id.to_string());
            queue.push(e);
            if queue.len() > MAX_ACCOUNT_MESSAGES {
                let overflow = queue.len() - MAX_ACCOUNT_MESSAGES;
                queue.drain(..overflow);
            }
            return Ok(());
        }

        let mut inner = self.inner.lock().unwrap();
        let room = inner.entry(room_id.to_string()).or_default();
        let queue = room
            .get_mut(to_device_id)
            .ok_or_else(|| "target device not in room".to_string())?;
        Self::prune(queue, now);
        queue.push(entry);
        // FIFO quota: drop oldest when over the cap.
        if queue.len() > MAX_MESSAGES_PER_DEVICE {
            let overflow = queue.len() - MAX_MESSAGES_PER_DEVICE;
            queue.drain(..overflow);
        }
        Ok(())
    }

    /// Take up to `max_count` pending messages for `device_id`: its own queue
    /// (consumed) plus account-level messages it has not seen yet (marked
    /// seen). Account-level messages are shared, so a device can page through
    /// them by polling repeatedly.
    pub fn poll(&self, room_id: &str, device_id: &str, max_count: Option<usize>) -> Vec<MailboxMessage> {
        let mut out = Vec::new();
        let limit = max_count.unwrap_or(100);

        // 1) Per-device queue (poll = consume).
        {
            let mut inner = self.inner.lock().unwrap();
            if let Some(room) = inner.get_mut(room_id) {
                if let Some(queue) = room.get_mut(device_id) {
                    let now = Self::now();
                    Self::prune(queue, now);
                    let take = limit.min(queue.len());
                    out.extend(
                        queue
                            .drain(..take)
                            .map(|e| Self::entry_to_message(&e, false)),
                    );
                }
            }
        }
        if out.len() >= limit {
            return out;
        }

        // 2) Account-level archive (shared; mark seen, don't remove).
        let mut account = self.account.lock().unwrap();
        if let Some(queue) = account.get_mut(room_id) {
            let now = Self::now();
            Self::prune(queue, now);
            for e in queue.iter_mut() {
                if out.len() >= limit {
                    break;
                }
                if e.seen.insert(device_id.to_string()) {
                    out.push(Self::entry_to_message(e, true));
                }
            }
        }
        out
    }

    /// Acknowledge per-device messages (removes them from the device queue).
    /// Account-level messages are shared and never removed by ack.
    pub fn ack(&self, room_id: &str, device_id: &str, message_ids: &[String]) {
        let mut inner = self.inner.lock().unwrap();
        let Some(room) = inner.get_mut(room_id) else {
            return;
        };
        let Some(queue) = room.get_mut(device_id) else {
            return;
        };
        queue.retain(|e| !message_ids.contains(&e.id));
    }

    /// Register a device so deposits can target it. Called on Join.
    pub fn ensure_device(&self, room_id: &str, device_id: &str) {
        let mut inner = self.inner.lock().unwrap();
        inner
            .entry(room_id.to_string())
            .or_default()
            .entry(device_id.to_string())
            .or_default();
    }

    /// Remove a device when it leaves the room.
    pub fn remove_device(&self, room_id: &str, device_id: &str) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(room) = inner.get_mut(room_id) {
            room.remove(device_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deposit_poll_ack_round_trip() {
        let mb = Mailbox::new();
        mb.ensure_device("room-a", "dev-a");
        mb.ensure_device("room-a", "dev-b");

        mb.deposit("room-a", "dev-a", "dev-b", "cipher-1".into(), "nonce-1".into(), None)
            .unwrap();

        // poll consumes the message
        let msgs = mb.poll("room-a", "dev-b", None);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].ciphertext, "cipher-1");
        assert_eq!(msgs[0].from_device_id, "dev-a");
        assert!(!msgs[0].account_level);

        // second poll is empty
        assert!(mb.poll("room-a", "dev-b", None).is_empty());
    }

    #[test]
    fn deposit_to_unknown_device_fails() {
        let mb = Mailbox::new();
        mb.ensure_device("room-a", "dev-a");
        let err = mb
            .deposit("room-a", "dev-a", "ghost", "c".into(), "n".into(), None)
            .unwrap_err();
        assert!(err.contains("not in room"));
    }

    #[test]
    fn quota_fifo_drops_oldest() {
        let mb = Mailbox::new();
        mb.ensure_device("room-a", "dev-a");
        mb.ensure_device("room-a", "dev-b");
        for i in 0..600 {
            mb.deposit("room-a", "dev-a", "dev-b", format!("c{i}"), "n".into(), None)
                .unwrap();
        }
        let msgs = mb.poll("room-a", "dev-b", Some(1000));
        assert_eq!(msgs.len(), 500); // capped
        assert_eq!(msgs[0].ciphertext, "c100"); // oldest 100 dropped
    }

    #[test]
    fn ack_removes_messages() {
        let mb = Mailbox::new();
        mb.ensure_device("room-a", "dev-a");
        mb.ensure_device("room-a", "dev-b");
        mb.deposit("room-a", "dev-a", "dev-b", "c1".into(), "n1".into(), None)
            .unwrap();
        mb.deposit("room-a", "dev-a", "dev-b", "c2".into(), "n2".into(), None)
            .unwrap();

        let msgs = mb.poll("room-a", "dev-b", None);
        assert_eq!(msgs.len(), 2);
        mb.ack("room-a", "dev-b", &[msgs[0].id.clone()]);
        // ack after poll has no effect on already-consumed messages, but must
        // not error; re-deposit and ack before poll removes them
        mb.deposit("room-a", "dev-a", "dev-b", "c3".into(), "n3".into(), None)
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
        let mb = Mailbox::new();
        // No device ever joined: deposit must still succeed.
        mb.deposit(
            "room-a",
            "dev-a",
            ACCOUNT_LEVEL_TARGET,
            "account-cipher".into(),
            "nonce".into(),
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
        let mb = Mailbox::new();
        for i in 0..250 {
            mb.deposit(
                "room-a",
                "dev-a",
                ACCOUNT_LEVEL_TARGET,
                format!("c{i}"),
                "n".into(),
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
        let mb = Mailbox::new();
        mb.ensure_device("room-a", "dev-a");
        mb.ensure_device("room-a", "dev-b");
        mb.deposit("room-a", "dev-a", "dev-b", "device-msg".into(), "n".into(), None)
            .unwrap();
        mb.deposit("room-a", "dev-a", ACCOUNT_LEVEL_TARGET, "account-msg".into(), "n".into(), None)
            .unwrap();

        let msgs = mb.poll("room-a", "dev-b", None);
        assert_eq!(msgs.len(), 2);
        assert!(!msgs[0].account_level);
        assert!(msgs[1].account_level);
    }
}
