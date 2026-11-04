//! Browser automation settings commands.

/// Detect the Chrome/Edge executable the browser tools would use: the
/// settings override when set, otherwise the system-wide auto-detection.
/// Returns the resolved path or a human-readable error.
#[tauri::command]
pub fn browser_detect() -> Result<String, String> {
    crate::core::browser::detect_executable().map(|p| p.display().to_string())
}
