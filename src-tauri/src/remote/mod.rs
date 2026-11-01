//! Remote IM channels: let the user talk to this device's agent from
//! feishu/weixin while the app is running. All connections are outbound
//! (websocket/long-poll), so no public endpoint is needed.

pub mod feishu;
pub mod router;
pub mod weixin;

use std::sync::Arc;

use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;

pub use router::is_remote_context;

#[derive(Clone, serde::Serialize)]
pub struct ChannelStatusInfo {
    pub channel: String,
    pub enabled: bool,
    pub connected: bool,
    pub detail: String,
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
}

#[derive(Default)]
struct Inner {
    feishu: ChannelSlot,
    weixin: ChannelSlot,
    /// Active QR login flow (cancel + verify-code slot), at most one.
    weixin_login: Option<(tokio_util::sync::CancellationToken, Arc<weixin::LoginState>)>,
}

#[derive(Clone, Default)]
pub struct RemoteManager {
    inner: Arc<Mutex<Inner>>,
}

impl RemoteManager {
    /// Start/stop channel tasks so they match the device settings. Called at
    /// startup and after every settings save.
    pub async fn sync_config(&self, db: &sqlx::SqlitePool, app: &AppHandle) {
        let remote = crate::core::settings_service::cached_device_settings().remote;
        let feishu = remote.feishu.clone();
        let weixin = remote.weixin.clone();

        let mut inner = self.inner.lock().await;
        Self::sync_one(
            &mut inner.feishu,
            feishu.enabled && !feishu.app_id.is_empty() && !feishu.app_secret.is_empty(),
            |cancel| feishu::spawn(db.clone(), app.clone(), self.clone(), feishu, cancel),
        );
        Self::sync_one(
            &mut inner.weixin,
            weixin.enabled && !weixin.bot_token.is_empty(),
            |cancel| weixin::spawn(db.clone(), app.clone(), self.clone(), weixin, cancel),
        );
        drop(inner);
        self.emit_status(app).await;
    }

    /// Generic start/restart/stop for one channel slot. A live task is always
    /// restarted when the channel is wanted, so credential edits take effect.
    fn sync_one(
        slot: &mut ChannelSlot,
        want: bool,
        spawn: impl FnOnce(tokio_util::sync::CancellationToken),
    ) {
        slot.enabled = want;
        if let Some(rt) = slot.runtime.take() {
            rt.cancel.cancel();
        }
        if want {
            let cancel = tokio_util::sync::CancellationToken::new();
            spawn(cancel.clone());
            slot.runtime = Some(ChannelRuntime { cancel });
        } else {
            slot.connected = false;
            slot.detail.clear();
        }
    }

    pub async fn set_feishu_status(&self, connected: bool, detail: String, app: &AppHandle) {
        {
            let mut inner = self.inner.lock().await;
            inner.feishu.connected = connected;
            inner.feishu.detail = detail;
        }
        self.emit_status(app).await;
    }

    pub async fn set_weixin_status(&self, connected: bool, detail: String, app: &AppHandle) {
        {
            let mut inner = self.inner.lock().await;
            inner.weixin.connected = connected;
            inner.weixin.detail = detail;
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
        vec![
            ChannelStatusInfo {
                channel: "feishu".to_string(),
                enabled: inner.feishu.enabled,
                connected: inner.feishu.connected,
                detail: inner.feishu.detail.clone(),
            },
            ChannelStatusInfo {
                channel: "weixin".to_string(),
                enabled: inner.weixin.enabled,
                connected: inner.weixin.connected,
                detail: inner.weixin.detail.clone(),
            },
        ]
    }

    async fn emit_status(&self, app: &AppHandle) {
        let status = self.status().await;
        let _ = app.emit("remote:status", status);
    }
}
