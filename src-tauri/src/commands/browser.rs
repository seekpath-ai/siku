//! Browser automation settings commands.

use tauri::State;

/// Detect the Chrome/Edge executable the browser tools would use: the
/// settings override when set, otherwise the system-wide auto-detection.
/// Returns the resolved path or a human-readable error.
#[tauri::command]
pub fn browser_detect() -> Result<String, String> {
    crate::core::browser::detect_executable().map(|p| p.display().to_string())
}

/// What the settings page needs to render its status line.
#[derive(serde::Serialize)]
pub struct BrowserProfileInfo {
    pub default_profile_dir: String,
    pub effective_profile_dir: String,
    /// Whether the effective directory already holds a profile (created on
    /// first launch, removed by "reset").
    pub effective_profile_exists: bool,
}

#[tauri::command]
pub fn browser_profile_info(state: State<'_, crate::AppState>) -> BrowserProfileInfo {
    let default = crate::core::browser::persistent_profile_dir(&state.app_data_dir);
    let effective = crate::core::browser::resolve_persistent_profile_dir(&default);
    BrowserProfileInfo {
        default_profile_dir: default.display().to_string(),
        effective_profile_exists: effective.is_dir(),
        effective_profile_dir: effective.display().to_string(),
    }
}

/// Windows reports locked browser files as sharing violation (os error 32).
fn friendly_io(e: std::io::Error, what: &str) -> String {
    if e.raw_os_error() == Some(32) || e.kind() == std::io::ErrorKind::PermissionDenied {
        format!("{what} 被占用——请完全退出来源浏览器（含后台进程）后重试")
    } else {
        format!("{what}失败: {e}")
    }
}

/// Open the persistent automation browser for a one-time human setup pass:
/// sign in to sites and install/enable extensions. The profile is persistent,
/// so the agent reuses that state later — this is the reliable path, whereas
/// cloning a signed-in Chrome profile loses cookies to app-bound encryption.
///
/// Returns the profile directory (shown in the settings page).
#[tauri::command]
pub async fn browser_open_setup() -> Result<String, String> {
    let manager = crate::core::browser::global()
        .ok_or_else(|| "浏览器管理器未初始化".to_string())?;
    manager.open_setup().await
}

/// Delete the automation browser profile: disconnect (releasing our file
/// locks), remove the directory, and let the next launch start clean. This is
/// the escape hatch for "the browser got into a weird state".
#[tauri::command]
pub async fn browser_reset_profile(state: State<'_, crate::AppState>) -> Result<(), String> {
    if let Some(mgr) = crate::core::browser::global() {
        mgr.disconnect().await;
    }
    let dir = crate::core::browser::resolve_persistent_profile_dir(
        &crate::core::browser::persistent_profile_dir(&state.app_data_dir),
    );
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| friendly_io(e, "删除浏览器配置目录"))?;
    }
    Ok(())
}
