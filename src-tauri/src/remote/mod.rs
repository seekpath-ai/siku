//! Remote IM channels: let the user talk to this device's agent from
//! feishu/weixin while the app is running. All connections are outbound
//! (websocket/long-poll), so no public endpoint is needed.

pub mod feishu;
pub mod router;

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

struct FeishuRuntime {
    cancel: tokio_util::sync::CancellationToken,
}

#[derive(Default)]
struct Inner {
    feishu: Option<FeishuRuntime>,
    feishu_enabled: bool,
    feishu_connected: bool,
    feishu_detail: String,
}

#[derive(Clone, Default)]
pub struct RemoteManager {
    inner: Arc<Mutex<Inner>>,
}

impl RemoteManager {
    /// Start/stop channel tasks so they match the device settings. Called at
    /// startup and after every settings save.
    pub async fn sync_config(&self, db: &sqlx::SqlitePool, app: &AppHandle) {
        let cfg = crate::core::settings_service::cached_device_settings()
            .remote
            .feishu;
        let want = cfg.enabled && !cfg.app_id.is_empty() && !cfg.app_secret.is_empty();

        let mut inner = self.inner.lock().await;
        inner.feishu_enabled = want;
        if want && inner.feishu.is_none() {
            let cancel = tokio_util::sync::CancellationToken::new();
            feishu::spawn(
                db.clone(),
                app.clone(),
                self.clone(),
                cfg,
                cancel.clone(),
            );
            inner.feishu = Some(FeishuRuntime { cancel });
        } else if !want {
            if let Some(rt) = inner.feishu.take() {
                rt.cancel.cancel();
            }
            inner.feishu_connected = false;
            inner.feishu_detail.clear();
        } else if want {
            // Credentials may have changed under a live task: restart.
            if let Some(rt) = inner.feishu.take() {
                rt.cancel.cancel();
            }
            let cancel = tokio_util::sync::CancellationToken::new();
            feishu::spawn(
                db.clone(),
                app.clone(),
                self.clone(),
                cfg,
                cancel.clone(),
            );
            inner.feishu = Some(FeishuRuntime { cancel });
        }
        drop(inner);
        self.emit_status(app).await;
    }

    pub async fn set_feishu_status(&self, connected: bool, detail: String, app: &AppHandle) {
        {
            let mut inner = self.inner.lock().await;
            inner.feishu_connected = connected;
            inner.feishu_detail = detail;
        }
        self.emit_status(app).await;
    }

    pub async fn status(&self) -> Vec<ChannelStatusInfo> {
        let inner = self.inner.lock().await;
        vec![ChannelStatusInfo {
            channel: "feishu".to_string(),
            enabled: inner.feishu_enabled,
            connected: inner.feishu_connected,
            detail: inner.feishu_detail.clone(),
        }]
    }

    async fn emit_status(&self, app: &AppHandle) {
        let status = self.status().await;
        let _ = app.emit("remote:status", status);
    }
}
