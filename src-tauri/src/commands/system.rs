use tauri::State;
use tracing::instrument;

use tauri::AppHandle;

use crate::core::models::SystemInfo;
use crate::core::time;
use crate::AppState;

/// Log startup performance metrics to system_events table.
/// Called by the frontend after startup completes.
#[tauri::command]
#[instrument(skip(state))]
pub async fn log_startup_metrics(
    state: State<'_, AppState>,
    metrics: Vec<serde_json::Value>,
) -> Result<(), String> {
    let now = time::now_iso();
    for m in &metrics {
        let phase = m["phase"].as_str().unwrap_or("unknown");
        let elapsed = m["elapsed_ms"].as_u64().unwrap_or(0);

        let id = uuid::Uuid::new_v4().to_string();
        let metadata = serde_json::json!({
            "phase": phase,
            "elapsed_ms": elapsed,
        });

        sqlx::query(
            "INSERT INTO system_events (id, event_type, level, message, metadata, created_at)
             VALUES (?, 'startup_metric', 'debug', ?, ?, ?)",
        )
        .bind(&id)
        .bind(phase)
        .bind(metadata.to_string())
        .bind(&now)
        .execute(&state.db)
        .await
        .map_err(|e| format!("Failed to log startup metric: {e}"))?;
    }
    Ok(())
}

/// The system-wide screenshot hotkey (WeChat-style: fires even while the
/// main window is minimized). Kept as a single constant so registration and
/// the handler stay in sync.
pub const SCREENSHOT_HOTKEY: &str = "ctrl+shift+s";

/// Apply the `global_screenshot_hotkey` setting: (un)register the OS-level
/// hotkey. Called on app start and after the settings toggle flips.
/// Unregister-when-absent is ignored; register-conflict (another app grabbed
/// the combo) is surfaced as an error for the UI to show.
#[tauri::command]
#[instrument(skip(state, app_handle))]
pub async fn screenshot_hotkey_sync(
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<(), String> {
    let settings = crate::core::settings_service::load_app_settings(&state.db).await?;
    use tauri_plugin_global_shortcut::GlobalShortcutExt;
    let gs = app_handle.global_shortcut();
    let _ = gs.unregister(SCREENSHOT_HOTKEY);
    if settings.global_screenshot_hotkey {
        gs.register(SCREENSHOT_HOTKEY)
            .map_err(|e| format!("注册全局截图热键失败（可能被其他应用占用）：{e}"))?;
    }
    Ok(())
}

/// Launch the OS-native region screenshot tool; the captured image lands in
/// the clipboard and the frontend picks it up on window refocus. Returns the
/// launched tool's name. Errors when no known tool is available (Linux
/// without flameshot/spectacle/gnome-screenshot).
#[tauri::command]
#[instrument]
pub fn screenshot_start() -> Result<String, String> {    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg("ms-screenclip:")
            .spawn()
            .map_err(|e| format!("launch ms-screenclip: {e}"))?;
        return Ok("ms-screenclip".into());
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("screencapture")
            .args(["-i", "-c"])
            .spawn()
            .map_err(|e| format!("launch screencapture: {e}"))?;
        return Ok("screencapture".into());
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        let candidates: [(&str, &[&str]); 3] = [
            ("flameshot", &["gui", "-c"]),
            ("spectacle", &["-rbc"]),
            ("gnome-screenshot", &["-i", "-c"]),
        ];
        for (bin, args) in candidates {
            if std::process::Command::new(bin).args(args).spawn().is_ok() {
                return Ok(bin.to_string());
            }
        }
        Err("未找到可用的截图工具（尝试过 flameshot / spectacle / gnome-screenshot），请使用系统截图后 Ctrl+V 粘贴".to_string())
    }
}

/// "Open" on these extensions executes code, so `open_local_path` refuses
/// them and the UI points the user at reveal-in-file-manager instead.
const EXECUTABLE_EXTS: &[&str] = &[
    "exe", "bat", "cmd", "ps1", "com", "scr", "msi", "sh", "app", "dll",
];

fn is_executable_path(p: &std::path::Path) -> bool {
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    EXECUTABLE_EXTS.contains(&ext.as_str())
}

/// Resolve a chat-prose path candidate to a path that exists on disk. The
/// remark plugin's greedy candidates may legitimately contain spaces and may
/// have prose glued to the end, so truncation walks right-to-left until
/// something exists. `Ok(None)` = dead candidate; the chip renders as plain
/// text and never calls open/reveal.
#[tauri::command]
#[instrument]
pub fn resolve_existing_path(candidate: String) -> Result<Option<String>, String> {
    Ok(resolve_existing(&candidate))
}

fn resolve_existing(candidate: &str) -> Option<String> {
    let trimmed = candidate.trim().trim_matches(|c| c == '"' || c == '\'');
    if trimmed.len() < 4 {
        return None;
    }
    if std::path::Path::new(trimmed).exists() {
        return Some(trimmed.to_string());
    }
    // Right-to-left word truncation: `…/my docs/a.png 备注` → `…/my docs/a.png`.
    let mut s = trimmed;
    while let Some(idx) = s.rfind(' ') {
        s = s[..idx].trim_end();
        if s.len() < 4 {
            break;
        }
        if std::path::Path::new(s).exists() {
            return Some(s.to_string());
        }
    }
    // Extension-boundary truncation: `…/y.zip注意` → `…/y.zip`.
    for end in extension_ends(trimmed).into_iter().rev() {
        let s = &trimmed[..end];
        if std::path::Path::new(s).exists() {
            return Some(s.to_string());
        }
    }
    None
}

/// End offsets (exclusive) of every `.<2-5 ASCII alnum>` extension run.
fn extension_ends(s: &str) -> Vec<usize> {
    let bytes = s.as_bytes();
    let mut ends = Vec::new();
    for (i, &b) in bytes.iter().enumerate() {
        if b != b'.' {
            continue;
        }
        let mut j = i + 1;
        while j < bytes.len() && bytes[j].is_ascii_alphanumeric() {
            j += 1;
        }
        let len = j - (i + 1);
        if (2..=5).contains(&len) {
            ends.push(j);
        }
    }
    ends
}

/// Open a local file with the OS default application (chat file-path chips).
/// Directories fall through to the file manager; nonexistent paths and
/// executables are refused with a user-facing message.
#[tauri::command]
#[instrument]
pub fn open_local_path(path: String) -> Result<(), String> {
    let p = std::path::Path::new(&path);
    if !p.exists() {
        return Err(format!("路径不存在：{path}"));
    }
    if p.is_dir() {
        return reveal_in_file_manager(path);
    }
    if is_executable_path(p) {
        return Err("为安全起见，可执行文件不直接打开，请改用「在文件夹中显示」".into());
    }
    #[cfg(target_os = "windows")]
    {
        // `start` is a cmd builtin; no_window suppresses the console flash.
        crate::core::process::no_window(&mut std::process::Command::new("cmd"))
            .args(["/c", "start", "", &path])
            .spawn()
            .map_err(|e| format!("打开文件失败：{e}"))?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&path)
            .spawn()
            .map_err(|e| format!("打开文件失败：{e}"))?;
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(&path)
            .spawn()
            .map_err(|e| format!("打开文件失败：{e}"))?;
    }
    Ok(())
}

/// Select the file in the OS file manager (Linux has no portable "select",
/// so it opens the containing folder).
#[tauri::command]
#[instrument]
pub fn reveal_in_file_manager(path: String) -> Result<(), String> {
    let p = std::path::Path::new(&path);
    if !p.exists() {
        return Err(format!("路径不存在：{path}"));
    }
    #[cfg(target_os = "windows")]
    {
        // explorer returns non-zero even on success, so spawn and don't wait.
        let mut cmd = std::process::Command::new("explorer");
        if p.is_dir() {
            cmd.arg(&path);
        } else {
            cmd.arg(format!("/select,{path}"));
        }
        cmd.spawn().map_err(|e| format!("打开文件管理器失败：{e}"))?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("-R")
            .arg(&path)
            .spawn()
            .map_err(|e| format!("打开文件管理器失败：{e}"))?;
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        let dir = if p.is_dir() { p } else { p.parent().unwrap_or(p) };
        std::process::Command::new("xdg-open")
            .arg(dir)
            .spawn()
            .map_err(|e| format!("打开文件管理器失败：{e}"))?;
    }
    Ok(())
}

#[tauri::command]
#[instrument]
pub async fn system_info() -> Result<SystemInfo, String> {
    let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string()).unwrap_or_else(|_| "unknown".into());

    let cpu_count = std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(1);

    // Memory info from /proc/meminfo (Linux)
    let (mem_total, mem_avail) = std::fs::read_to_string("/proc/meminfo")
        .map(|s| {
            let mut total = 0u64; let mut avail = 0u64;
            for line in s.lines() {
                if line.starts_with("MemTotal:") {
                    total = line.split_whitespace().nth(1).and_then(|v| v.parse().ok()).unwrap_or(0);
                }
                if line.starts_with("MemAvailable:") {
                    avail = line.split_whitespace().nth(1).and_then(|v| v.parse().ok()).unwrap_or(0);
                }
            }
            (total / 1024, (total.saturating_sub(avail)) / 1024)
        }).unwrap_or((0, 0));

    Ok(SystemInfo {
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        hostname,
        cpu_count,
        memory_total_mb: mem_total,
        memory_used_mb: Some(mem_avail),
        disk_total_gb: None,
        disk_used_gb: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executable_exts_are_blocked_case_insensitively() {
        assert!(is_executable_path(std::path::Path::new("/tmp/a.exe")));
        assert!(is_executable_path(std::path::Path::new("C:\\Tools\\RUN.SH")));
        assert!(is_executable_path(std::path::Path::new("/tmp/b.Ps1")));
    }

    #[test]
    fn documents_and_extensionless_files_open_normally() {
        assert!(!is_executable_path(std::path::Path::new("/tmp/笔记.png")));
        assert!(!is_executable_path(std::path::Path::new("/tmp/report.docx")));
        assert!(!is_executable_path(std::path::Path::new("/tmp/Makefile")));
    }

    #[test]
    fn resolve_existing_as_is_and_quoted() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.txt");
        std::fs::write(&f, b"x").unwrap();
        let s = f.to_str().unwrap().to_string();
        assert_eq!(resolve_existing(&s), Some(s.clone()));
        assert_eq!(resolve_existing(&format!("\"{s}\"")), Some(s.clone()));
        assert_eq!(resolve_existing(&format!("  '{s}' ")), Some(s.clone()));
    }

    #[test]
    fn resolve_existing_truncates_space_glued_prose() {
        let dir = tempfile::tempdir().unwrap();
        // Space inside the file name AND prose glued behind it.
        let sub = dir.path().join("my docs");
        std::fs::create_dir(&sub).unwrap();
        let f = sub.join("a file.png");
        std::fs::write(&f, b"x").unwrap();
        let s = f.to_str().unwrap().to_string();
        assert_eq!(
            resolve_existing(&format!("{s} 备注文字")),
            Some(s.clone())
        );
        assert_eq!(resolve_existing(&format!("{s} then more words")), Some(s.clone()));
    }

    #[test]
    fn resolve_existing_truncates_at_extension_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("y.zip");
        std::fs::write(&f, b"x").unwrap();
        let s = f.to_str().unwrap().to_string();
        // CJK glued directly to the extension, no space.
        assert_eq!(resolve_existing(&format!("{s}注意查收")), Some(s.clone()));
    }

    #[test]
    fn resolve_existing_returns_none_for_dead_candidates() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope").join("missing.txt");
        let s = missing.to_str().unwrap().to_string();
        assert_eq!(resolve_existing(&s), None);
        assert_eq!(resolve_existing(&format!("{s} 备注")), None);
        assert_eq!(resolve_existing("/flag"), None);
    }
}
