//! Remote IM channels: let the user talk to the account's agent from
//! feishu/weixin while the app is running. All connections are outbound
//! (websocket/long-poll), so no public endpoint is needed.
//!
//! The connection is single-active across the account's devices: the holder
//! is elected deterministically from relay room presence (lowest online
//! device id wins), optionally overridden by a synced `remote_holder`
//! record written on manual takeover. Config roams via synced settings, so
//! any device can take over without re-binding (crucial for WeChat, where
//! a fresh QR login replaces the previous binding).

pub mod feishu;
pub mod router;
pub mod weixin;

use std::sync::Arc;

use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;

pub use router::is_remote_context;

/// How often the arbiter re-evaluates presence (also the worst-case delay
/// for noticing config/holder changes synced from other devices).
const ARBITER_INTERVAL_SECS: u64 = 45;

#[derive(Clone, serde::Serialize)]
pub struct ChannelStatusInfo {
    pub channel: String,
    pub enabled: bool,
    pub connected: bool,
    pub detail: String,
    /// "unknown" | "active" | "standby" — single-active arbitration outcome.
    pub role: String,
    /// Display name of the device holding the connection (standby only).
    pub holder: String,
}

struct ChannelRuntime {
    cancel: tokio_util::sync::CancellationToken,
}

#[derive(Default)]
struct ChannelSlot {
    runtime: Option<ChannelRuntime>,
    enabled: bool,
    connected: bool,
    detail: String,
    /// Fingerprint of the last applied (want, config) pair. Periodic
    /// arbitration must not restart a healthy connection, so sync_one is
    /// skipped while the signature is unchanged.
    signature: Option<String>,
}

#[derive(Clone, Default, PartialEq)]
enum Role {
    #[default]
    Unknown,
    Active,
    Standby { holder_id: String, holder_name: String },
}

impl Role {
    fn label(&self) -> &'static str {
        match self {
            Role::Unknown => "unknown",
            Role::Active => "active",
            Role::Standby { .. } => "standby",
        }
    }
}

#[derive(Default)]
struct Inner {
    /// Live channel slots keyed by instance: "feishu:<app_id>" per feishu
    /// bot, "weixin" for the single WeChat binding.
    slots: std::collections::HashMap<String, ChannelSlot>,
    /// Active QR login flow (cancel + verify-code slot), at most one.
    weixin_login: Option<(tokio_util::sync::CancellationToken, Arc<weixin::LoginState>)>,
    role: Role,
    arbiter: Option<tokio_util::sync::CancellationToken>,
}

#[derive(Clone, Default)]
pub struct RemoteManager {
    inner: Arc<Mutex<Inner>>,
}

impl RemoteManager {
    /// Re-evaluate arbitration, then start/stop channel tasks to match.
    /// Called at startup, after every settings save, and by the arbiter.
    pub async fn sync_config(&self, db: &sqlx::SqlitePool, app: &AppHandle) {
        self.evaluate(db).await;
        self.apply_channels(db, app).await;
    }

    /// Periodic presence re-evaluation. Runs until app shutdown.
    pub fn spawn_arbiter(&self, db: sqlx::SqlitePool, app: AppHandle) {
        let manager = self.clone();
        let cancel = tokio_util::sync::CancellationToken::new();
        let token = cancel.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = token.cancelled() => break,
                    _ = tokio::time::sleep(std::time::Duration::from_secs(ARBITER_INTERVAL_SECS)) => {
                        manager.sync_config(&db, &app).await;
                    }
                }
            }
        });
        let inner = self.inner.clone();
        tokio::spawn(async move {
            inner.lock().await.arbiter = Some(cancel);
        });
    }

    /// Manual takeover: write this device as the preferred holder into the
    /// synced settings, then re-evaluate immediately. Other devices honor
    /// the record while this device is online in the relay room.
    pub async fn takeover(&self, db: &sqlx::SqlitePool, app: &AppHandle) -> Result<(), String> {
        let device_id = crate::sync::onboarding::ensure_device_id(db)
            .await
            .map_err(|e| e.to_string())?;
        let device_name = crate::core::settings_service::get_device_setting(
            db,
            crate::commands::account::ACCOUNT_DEVICE_NAME_KEY,
        )
        .await?
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| short_id(&device_id));
        crate::core::settings_service::save_remote_holder(
            db,
            &crate::core::models::RemoteHolder {
                device_id,
                device_name,
                ts: crate::core::time::now_iso(),
            },
        )
        .await?;
        self.sync_config(db, app).await;
        Ok(())
    }

    /// Decide this device's role. Presence failure keeps the previous role
    /// (hysteresis): an Active device does not drop its connection on a
    /// transient relay outage, and a Standby device does not connect —
    /// double-replies are worse than a temporary outage.
    async fn evaluate(&self, db: &sqlx::SqlitePool) {
        let own_id = crate::sync::onboarding::ensure_device_id(db)
            .await
            .unwrap_or_default();

        let role = match crate::commands::account::fetch_account_devices(db).await {
            Ok(devices) => {
                let override_holder = crate::core::settings_service::load_remote_holder(db)
                    .await
                    .ok()
                    .flatten();
                let online = |id: &str| devices.iter().any(|d| d.device_id == id && d.online);
                let holder_id = match &override_holder {
                    // Manual override wins while its device is online (own
                    // device counts as online even if the relay is slow to
                    // notice us).
                    Some(h) if h.device_id == own_id || online(&h.device_id) => {
                        h.device_id.clone()
                    }
                    _ => {
                        let mut candidates: Vec<&str> = devices
                            .iter()
                            .filter(|d| d.online)
                            .map(|d| d.device_id.as_str())
                            .collect();
                        if !own_id.is_empty() {
                            candidates.push(&own_id);
                        }
                        candidates.sort();
                        candidates.dedup();
                        candidates.first().map(|s| s.to_string()).unwrap_or_default()
                    }
                };
                if holder_id.is_empty() || holder_id == own_id {
                    Role::Active
                } else {
                    let holder_name = devices
                        .iter()
                        .find(|d| d.device_id == holder_id)
                        .map(|d| d.name.clone())
                        .filter(|n| !n.is_empty())
                        .or_else(|| {
                            override_holder
                                .filter(|h| h.device_id == holder_id)
                                .map(|h| h.device_name.clone())
                        })
                        .filter(|n| !n.is_empty())
                        .unwrap_or_else(|| short_id(&holder_id));
                    Role::Standby { holder_id, holder_name }
                }
            }
            Err(e) => {
                if e == "未登录同步账号" {
                    // No sync account: single-device legacy behavior.
                    Role::Active
                } else {
                    tracing::warn!(error = %e, "remote arbitration: presence query failed, keeping previous role");
                    return;
                }
            }
        };

        let mut inner = self.inner.lock().await;
        if inner.role != role {
            tracing::info!(role = role.label(), "remote arbitration role changed");
            inner.role = role;
        }
    }

    /// Start/stop channel tasks so they match the synced config and the
    /// current role. Only the Active device connects. Feishu runs one
    /// connection per configured bot; slots for removed bots are torn down.
    async fn apply_channels(&self, db: &sqlx::SqlitePool, app: &AppHandle) {
        let remote = crate::core::settings_service::cached_settings().remote;
        let weixin_cfg = remote.weixin.clone();

        let mut inner = self.inner.lock().await;
        let active = inner.role == Role::Active;
        let standby_detail = match &inner.role {
            Role::Standby { holder_name, .. } => Some(format!("由 {holder_name} 接管")),
            _ => None,
        };

        let mut desired: Vec<(String, bool, String)> = Vec::new();
        for bot in &remote.feishu_bots {
            let key = format!("feishu:{}", bot.app_id);
            let want = active && bot.enabled && !bot.app_id.is_empty() && !bot.app_secret.is_empty();
            let fingerprint = format!("{}:{}", bot.enabled, bot.app_secret);
            desired.push((key, want, fingerprint));
        }
        {
            let want = active && weixin_cfg.enabled && !weixin_cfg.bot_token.is_empty();
            let fingerprint = format!("{}:{}", weixin_cfg.enabled, weixin_cfg.bot_token);
            desired.push(("weixin".to_string(), want, fingerprint));
        }
        let desired_keys: std::collections::HashSet<&str> =
            desired.iter().map(|(k, _, _)| k.as_str()).collect();

        // Tear down slots whose bot was removed from the config.
        let stale: Vec<String> = inner
            .slots
            .keys()
            .filter(|k| !desired_keys.contains(k.as_str()))
            .cloned()
            .collect();
        for key in stale {
            if let Some(mut slot) = inner.slots.remove(&key) {
                if let Some(rt) = slot.runtime.take() {
                    rt.cancel.cancel();
                }
            }
        }

        for (key, want, fingerprint) in desired {
            let slot = inner.slots.entry(key.clone()).or_default();
            let standby = standby_detail.clone();
            match key.strip_prefix("feishu:") {
                Some(app_id) => {
                    let bot = remote
                        .feishu_bots
                        .iter()
                        .find(|b| b.app_id == app_id)
                        .cloned()
                        .unwrap_or_default();
                    Self::sync_one(slot, want, fingerprint, standby, |cancel| {
                        feishu::spawn(db.clone(), app.clone(), self.clone(), bot, cancel)
                    });
                }
                None => {
                    let cfg = weixin_cfg.clone();
                    Self::sync_one(slot, want, fingerprint, standby, |cancel| {
                        weixin::spawn(db.clone(), app.clone(), self.clone(), cfg, cancel)
                    });
                }
            }
        }
        drop(inner);
        self.emit_status(app).await;
    }

    /// Generic start/restart/stop for one channel slot. A healthy channel
    /// is left running while its (want, config) signature is unchanged.
    fn sync_one(
        slot: &mut ChannelSlot,
        want: bool,
        fingerprint: String,
        standby_detail: Option<String>,
        spawn: impl FnOnce(tokio_util::sync::CancellationToken),
    ) {
        slot.enabled = want;
        let standby = standby_detail.is_some();
        if let Some(detail) = standby_detail {
            if !want {
                slot.connected = false;
                slot.detail = detail;
            }
        }
        let signature = format!("{want}:{fingerprint}");
        if slot.signature.as_deref() == Some(signature.as_str()) {
            return;
        }
        slot.signature = Some(signature);
        if let Some(rt) = slot.runtime.take() {
            rt.cancel.cancel();
        }
        if want {
            slot.detail.clear();
            let cancel = tokio_util::sync::CancellationToken::new();
            spawn(cancel.clone());
            slot.runtime = Some(ChannelRuntime { cancel });
        } else {
            slot.connected = false;
            if !standby {
                slot.detail.clear();
            }
        }
    }

    /// Channel instances report their own status by slot key
    /// ("feishu:<app_id>" / "weixin").
    pub async fn set_channel_status(
        &self,
        key: &str,
        connected: bool,
        detail: String,
        app: &AppHandle,
    ) {
        {
            let mut inner = self.inner.lock().await;
            let slot = inner.slots.entry(key.to_string()).or_default();
            slot.connected = connected;
            slot.detail = detail;
        }
        self.emit_status(app).await;
    }

    // ------------------------------------------------------------ weixin login

    /// Start the QR login flow. Returns false when one is already running.
    pub async fn start_weixin_login(&self, app: &AppHandle) -> bool {
        let mut inner = self.inner.lock().await;
        if inner.weixin_login.is_some() {
            return false;
        }
        let cancel = tokio_util::sync::CancellationToken::new();
        let state = Arc::new(weixin::LoginState {
            verify_code: Mutex::new(None),
        });
        weixin::spawn_login(app.clone(), self.clone(), state.clone());
        inner.weixin_login = Some((cancel, state));
        true
    }

    pub async fn weixin_login_verify(&self, code: String) -> bool {
        let inner = self.inner.lock().await;
        if let Some((_, state)) = &inner.weixin_login {
            *state.verify_code.lock().await = Some(code);
            true
        } else {
            false
        }
    }

    /// Called by the login task when it finishes (success or failure).
    pub async fn clear_weixin_login(&self) {
        let mut inner = self.inner.lock().await;
        inner.weixin_login = None;
    }

    pub async fn cancel_weixin_login(&self) {
        let login = {
            let mut inner = self.inner.lock().await;
            inner.weixin_login.take()
        };
        if let Some((cancel, _)) = login {
            cancel.cancel();
        }
    }

    pub async fn status(&self) -> Vec<ChannelStatusInfo> {
        let inner = self.inner.lock().await;
        let role = inner.role.label().to_string();
        let holder = match &inner.role {
            Role::Standby { holder_name, .. } => holder_name.clone(),
            _ => String::new(),
        };
        let mut keys: Vec<&String> = inner.slots.keys().collect();
        keys.sort();
        keys.into_iter()
            .map(|key| {
                let slot = &inner.slots[key];
                ChannelStatusInfo {
                    channel: key.clone(),
                    enabled: slot.enabled,
                    connected: slot.connected,
                    detail: slot.detail.clone(),
                    role: role.clone(),
                    holder: holder.clone(),
                }
            })
            .collect()
    }

    async fn emit_status(&self, app: &AppHandle) {
        let status = self.status().await;
        let _ = app.emit("remote:status", status);
    }
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}
