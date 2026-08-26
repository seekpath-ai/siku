use tauri::State;
use tracing::instrument;

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

/// Launch the OS-native region screenshot tool; the captured image lands in
/// the clipboard and the frontend picks it up on window refocus. Returns the
/// launched tool's name. Errors when no known tool is available (Linux
/// without flameshot/spectacle/gnome-screenshot).
#[tauri::command]
#[instrument]
pub fn screenshot_start() -> Result<String, String> {
    #[cfg(target_os = "windows")]
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
