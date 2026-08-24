//! Account/device persistence for the production sync service.
//!
//! Single-process in-memory store persisted to a JSON file. Sufficient for a
//! small sync service; swap for SQLite/Postgres when scaling out.

use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use tracing::info;

#[derive(Clone, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub email: String,
    pub password_hash: String,
    /// Account-level sync key (base64). Issued to every device on login so
    /// they can decrypt mailbox messages without a separate pairing step.
    pub sync_key: String,
    pub created_at: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub revoked_at: Option<String>,
    pub last_seen_at: Option<String>,
    pub created_at: String,
    /// Opaque refresh token for this device. Devices created before this field
    /// existed will have None and must re-login to obtain one.
    #[serde(default)]
    pub refresh_token: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
struct Snapshot {
    users: HashMap<String, User>,
    devices: HashMap<String, Device>,
    email_to_user: HashMap<String, String>,
}

pub struct Db {
    inner: Mutex<Snapshot>,
    path: Option<std::path::PathBuf>,
}

impl Db {
    pub fn new(path: &Path) -> anyhow::Result<Self> {
        let snapshot = if path.as_os_str() == ":memory:" {
            Snapshot::default()
        } else {
            std::fs::create_dir_all(path.parent().unwrap_or(path))?;
            match std::fs::read_to_string(path) {
                Ok(json) => serde_json::from_str(&json).unwrap_or_default(),
                Err(_) => Snapshot::default(),
            }
        };
        Ok(Self {
            inner: Mutex::new(snapshot),
            path: (path.as_os_str() != ":memory:").then(|| path.to_path_buf()),
        })
    }

    fn persist(&self, snap: &Snapshot) {
        let Some(path) = &self.path else { return };
        if let Ok(json) = serde_json::to_string_pretty(snap) {
            let _ = std::fs::write(path, json);
        }
    }

    pub fn create_user(
        &self,
        id: &str,
        email: &str,
        password_hash: &str,
        sync_key: &str,
    ) -> anyhow::Result<()> {
        let mut snap = self.inner.lock().unwrap();
        if snap.email_to_user.contains_key(email) {
            anyhow::bail!("email already registered");
        }
        snap.users.insert(
            id.to_string(),
            User {
                id: id.to_string(),
                email: email.to_string(),
                password_hash: password_hash.to_string(),
                sync_key: sync_key.to_string(),
                created_at: crate::now_iso(),
            },
        );
        snap.email_to_user.insert(email.to_string(), id.to_string());
        self.persist(&snap);
        Ok(())
    }

    pub fn get_user_by_email(&self, email: &str) -> Option<User> {
        let snap = self.inner.lock().unwrap();
        snap.email_to_user
            .get(email)
            .and_then(|id| snap.users.get(id))
            .cloned()
    }

    pub fn get_user(&self, id: &str) -> Option<User> {
        self.inner.lock().unwrap().users.get(id).cloned()
    }

    /// Lazily assign a sync key to a user (legacy accounts registered before
    /// the sync_key column existed).
    pub fn set_user_sync_key(&self, id: &str, sync_key: &str) -> anyhow::Result<()> {
        let mut snap = self.inner.lock().unwrap();
        let Some(user) = snap.users.get_mut(id) else {
            anyhow::bail!("user not found");
        };
        user.sync_key = sync_key.to_string();
        self.persist(&snap);
        Ok(())
    }

    /// Register a device under a user. Returns Ok(false) when the device is
    /// already registered (idempotent re-login).
    ///
    /// A previously **revoked** device is reactivated: logging in again with
    /// valid credentials re-authorizes the device (revoked_at cleared).
    pub fn register_device(
        &self,
        user_id: &str,
        device_id: &str,
        name: &str,
    ) -> anyhow::Result<bool> {
        let mut snap = self.inner.lock().unwrap();
        match snap.devices.get_mut(device_id) {
            Some(dev) => {
                if dev.user_id != user_id {
                    anyhow::bail!("device belongs to another user");
                }
                let was_revoked = dev.revoked_at.is_some();
                dev.revoked_at = None;
                dev.last_seen_at = Some(crate::now_iso());
                if !name.is_empty() {
                    dev.name = name.to_string();
                }
                if was_revoked {
                    info!(
                        device_id = %device_id,
                        "revoked device reactivated by re-login"
                    );
                }
                self.persist(&snap);
                Ok(false)
            }
            None => {
                snap.devices.insert(
                    device_id.to_string(),
                    Device {
                        id: device_id.to_string(),
                        user_id: user_id.to_string(),
                        name: name.to_string(),
                        revoked_at: None,
                        last_seen_at: Some(crate::now_iso()),
                        created_at: crate::now_iso(),
                        refresh_token: None,
                    },
                );
                self.persist(&snap);
                Ok(true)
            }
        }
    }

    pub fn get_device(&self, device_id: &str) -> Option<Device> {
        self.inner.lock().unwrap().devices.get(device_id).cloned()
    }

    pub fn list_devices(&self, user_id: &str) -> Vec<Device> {
        let snap = self.inner.lock().unwrap();
        let mut list: Vec<Device> = snap
            .devices
            .values()
            .filter(|d| d.user_id == user_id)
            .cloned()
            .collect();
        list.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        list
    }

    pub fn set_device_refresh_token(
        &self,
        device_id: &str,
        refresh_token: &str,
    ) -> anyhow::Result<()> {
        let mut snap = self.inner.lock().unwrap();
        let Some(dev) = snap.devices.get_mut(device_id) else {
            anyhow::bail!("device not found");
        };
        dev.refresh_token = Some(refresh_token.to_string());
        self.persist(&snap);
        Ok(())
    }

    pub fn find_device_by_refresh_token(&self, token: &str) -> Option<Device> {
        let snap = self.inner.lock().unwrap();
        snap.devices
            .values()
            .find(|d| d.refresh_token.as_deref() == Some(token))
            .cloned()
    }

    pub fn revoke_device(&self, user_id: &str, device_id: &str) -> bool {
        let mut snap = self.inner.lock().unwrap();
        let Some(dev) = snap.devices.get_mut(device_id) else {
            return false;
        };
        if dev.user_id != user_id {
            return false;
        }
        dev.revoked_at = Some(crate::now_iso());
        self.persist(&snap);
        true
    }

    pub fn rename_device(&self, user_id: &str, device_id: &str, name: &str) -> bool {
        let mut snap = self.inner.lock().unwrap();
        let Some(dev) = snap.devices.get_mut(device_id) else {
            return false;
        };
        if dev.user_id != user_id {
            return false;
        }
        dev.name = name.to_string();
        self.persist(&snap);
        true
    }

    pub fn touch_device(&self, device_id: &str) {
        let mut snap = self.inner.lock().unwrap();
        if let Some(dev) = snap.devices.get_mut(device_id) {
            dev.last_seen_at = Some(crate::now_iso());
        }
    }
}

pub fn init(path: &Path) -> anyhow::Result<Db> {
    let db = Db::new(path).context("init account db")?;
    info!(db = %path.display(), "account database ready");
    Ok(db)
}
