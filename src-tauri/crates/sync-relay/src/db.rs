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
    /// Custom storage quota in bytes. While `quota_expires_at` is in the
    /// future (or None = admin-granted permanent quota) this overrides the
    /// relay default; otherwise the default applies.
    #[serde(default)]
    pub storage_quota_bytes: Option<i64>,
    /// RFC3339 expiry for `storage_quota_bytes`; None = never expires.
    #[serde(default)]
    pub quota_expires_at: Option<String>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    Pending,
    Paid,
    Rejected,
    Cancelled,
}

/// A storage upgrade order: the user applies for a plan, pays either online
/// (alipay, `channel` set) or out-of-band, and the quota activates on payment
/// confirmation (async notify or admin API respectively).
#[derive(Clone, Serialize, Deserialize)]
pub struct Order {
    pub id: String,
    pub user_id: String,
    pub plan_id: String,
    pub quota_bytes: i64,
    pub duration_days: u32,
    pub amount_cny: f64,
    pub status: OrderStatus,
    pub created_at: String,
    pub paid_at: Option<String>,
    pub admin_note: Option<String>,
    /// 支付渠道（如 "alipay"）；None = 线下转账。旧快照无此字段。
    #[serde(default)]
    pub channel: Option<String>,
    /// 第三方交易号（支付宝 trade_no），回调履约时记录。旧快照无此字段。
    #[serde(default)]
    pub trade_no: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
struct Snapshot {
    users: HashMap<String, User>,
    devices: HashMap<String, Device>,
    email_to_user: HashMap<String, String>,
    #[serde(default)]
    orders: HashMap<String, Order>,
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
                storage_quota_bytes: None,
                quota_expires_at: None,
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
    /// Legacy rows may still carry `revoked_at` (from before device removal
    /// replaced revocation): re-login clears it and re-authorizes the device.
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
            // Legacy rows may still carry revoked_at; keep them hidden so the
            // device list only ever shows live devices.
            .filter(|d| d.user_id == user_id && d.revoked_at.is_none())
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

    /// Remove a device entirely. The row disappears from the list and its
    /// tokens die with it: refresh-token lookup finds nothing and the WS
    /// handshake rejects unknown device ids. Logging in again with the same
    /// device id simply registers a fresh row.
    pub fn delete_device(&self, user_id: &str, device_id: &str) -> bool {
        let mut snap = self.inner.lock().unwrap();
        let Some(dev) = snap.devices.get(device_id) else {
            return false;
        };
        if dev.user_id != user_id {
            return false;
        }
        snap.devices.remove(device_id);
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

    pub fn list_users(&self) -> Vec<User> {
        let snap = self.inner.lock().unwrap();
        let mut list: Vec<User> = snap.users.values().cloned().collect();
        list.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        list
    }

    /// The user's custom quota and its expiry (both None when never set).
    pub fn get_user_quota(&self, user_id: &str) -> Option<(Option<i64>, Option<String>)> {
        let snap = self.inner.lock().unwrap();
        snap.users
            .get(user_id)
            .map(|u| (u.storage_quota_bytes, u.quota_expires_at.clone()))
    }

    pub fn set_user_quota(
        &self,
        user_id: &str,
        quota_bytes: Option<i64>,
        expires_at: Option<String>,
    ) -> anyhow::Result<()> {
        let mut snap = self.inner.lock().unwrap();
        let Some(user) = snap.users.get_mut(user_id) else {
            anyhow::bail!("user not found");
        };
        user.storage_quota_bytes = quota_bytes;
        user.quota_expires_at = expires_at;
        self.persist(&snap);
        Ok(())
    }

    pub fn create_order(&self, order: Order) -> anyhow::Result<()> {
        let mut snap = self.inner.lock().unwrap();
        if !snap.users.contains_key(&order.user_id) {
            anyhow::bail!("user not found");
        }
        snap.orders.insert(order.id.clone(), order);
        self.persist(&snap);
        Ok(())
    }

    pub fn get_order(&self, id: &str) -> Option<Order> {
        self.inner.lock().unwrap().orders.get(id).cloned()
    }

    pub fn list_orders(&self, status: Option<OrderStatus>) -> Vec<Order> {
        let snap = self.inner.lock().unwrap();
        let mut list: Vec<Order> = snap
            .orders
            .values()
            .filter(|o| status.is_none() || Some(o.status) == status)
            .cloned()
            .collect();
        list.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        list
    }

    /// A user's pending upgrade order, if any. Order creation is idempotent
    /// per (plan, period): repeating the same selection returns this order,
    /// while choosing a different plan/period cancels it (see
    /// api_create_storage_order).
    pub fn pending_order_for_user(&self, user_id: &str) -> Option<Order> {
        let snap = self.inner.lock().unwrap();
        snap.orders
            .values()
            .find(|o| o.user_id == user_id && o.status == OrderStatus::Pending)
            .cloned()
    }

    pub fn update_order_status(
        &self,
        id: &str,
        status: OrderStatus,
        paid_at: Option<String>,
        admin_note: Option<String>,
    ) -> anyhow::Result<Order> {
        let mut snap = self.inner.lock().unwrap();
        let Some(order) = snap.orders.get_mut(id) else {
            anyhow::bail!("order not found");
        };
        order.status = status;
        if paid_at.is_some() {
            order.paid_at = paid_at;
        }
        if admin_note.is_some() {
            order.admin_note = admin_note;
        }
        let order = order.clone();
        self.persist(&snap);
        Ok(order)
    }

    /// 记录订单的支付渠道（下单后追加，如复用的 pending 订单改用在线支付）。
    pub fn set_order_channel(&self, id: &str, channel: &str) -> anyhow::Result<()> {
        let mut snap = self.inner.lock().unwrap();
        let Some(order) = snap.orders.get_mut(id) else {
            anyhow::bail!("order not found");
        };
        order.channel = Some(channel.to_string());
        self.persist(&snap);
        Ok(())
    }

    /// 原子履约订单：在同一把锁内完成「检查未支付 → 叠加配额 → 置为已支付」，
    /// 因此并发/重复的支付回调与管理员确认天然幂等，绝不会重复叠加配额。
    ///
    /// 当前订阅未过期则新周期叠加到到期日（续期），否则从现在开始。已取消/
    /// 被拒绝的订单也允许履约（用户真金白银付了款，必须交付），由调用方决定
    /// 是否允许（管理员确认只允许 pending；支付回调允许任何非已支付状态）。
    pub fn activate_order(
        &self,
        id: &str,
        trade_no: Option<String>,
        admin_note: Option<String>,
    ) -> anyhow::Result<Activation> {
        let mut snap = self.inner.lock().unwrap();
        let order = snap.orders.get(id).cloned().context("order not found")?;
        if order.status == OrderStatus::Paid {
            return Ok(Activation::AlreadyPaid(order));
        }
        let now = chrono::Utc::now();
        let base = snap
            .users
            .get(&order.user_id)
            .and_then(|u| u.quota_expires_at.as_deref())
            .and_then(|exp| chrono::DateTime::parse_from_rfc3339(exp).ok())
            .map(|t| t.with_timezone(&chrono::Utc))
            .filter(|t| *t > now)
            .unwrap_or(now);
        let expires_at = (base + chrono::Duration::days(order.duration_days as i64))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let user = snap.users.get_mut(&order.user_id).context("user not found")?;
        user.storage_quota_bytes = Some(order.quota_bytes);
        user.quota_expires_at = Some(expires_at);
        let order = snap.orders.get_mut(id).unwrap();
        order.status = OrderStatus::Paid;
        order.paid_at = Some(crate::now_iso());
        if trade_no.is_some() {
            order.trade_no = trade_no;
        }
        if admin_note.is_some() {
            order.admin_note = admin_note;
        }
        let order = order.clone();
        self.persist(&snap);
        Ok(Activation::Fulfilled(order))
    }
}

/// 订单履约结果：首次履约（配额已发放）或幂等命中（订单早已支付，未做任何变更）。
pub enum Activation {
    Fulfilled(Order),
    AlreadyPaid(Order),
}

pub fn init(path: &Path) -> anyhow::Result<Db> {
    let db = Db::new(path).context("init account db")?;
    info!(db = %path.display(), "account database ready");
    Ok(db)
}
