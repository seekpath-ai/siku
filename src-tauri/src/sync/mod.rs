pub mod attachments;
pub mod crdt;
pub mod crypto;
pub mod engine;
pub mod lan_discovery;
pub mod local_signaling;
pub mod mailbox_client;
pub mod onboarding;
pub mod relay_client;
pub mod types;
pub mod webrtc_peer;

#[allow(unused_imports)]
pub use engine::SyncEngine;
#[allow(unused_imports)]
pub use webrtc_peer::{start_sync_session, SyncSession};

/// Event emitted after remote changes (changeset or snapshot) were applied to
/// the local database. Payload is the number of applied rows/statements. The
/// frontend listens, debounces, and reloads the affected views.
pub const REMOTE_APPLIED_EVENT: &str = "sync:remote_applied";

/// App handle used to emit sync events. Sync code runs in detached tasks with
/// no access to a handle, so it is registered once at app startup.
static APP_HANDLE: std::sync::OnceLock<tauri::AppHandle> = std::sync::OnceLock::new();

pub fn register_app_handle(handle: tauri::AppHandle) {
    let _ = APP_HANDLE.set(handle);
}

pub(crate) fn emit_remote_applied(count: i64) {
    if let Some(h) = APP_HANDLE.get() {
        use tauri::Emitter as _;
        let _ = h.emit(REMOTE_APPLIED_EVENT, count);
    }
}
