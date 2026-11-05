//! Browser automation settings commands.

use std::path::{Path, PathBuf};
use tauri::State;

/// Detect the Chrome/Edge executable the browser tools would use: the
/// settings override when set, otherwise the system-wide auto-detection.
/// Returns the resolved path or a human-readable error.
#[tauri::command]
pub fn browser_detect() -> Result<String, String> {
    crate::core::browser::detect_executable().map(|p| p.display().to_string())
}

// ---------------------------------------------------------------------------
// Login-state / extension import
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
pub struct BrowserProfileEntry {
    /// Directory name under the user-data root, e.g. "Default", "Profile 1".
    pub dir: String,
    /// Display name from Preferences (`profile.name`).
    pub name: String,
}

#[derive(serde::Serialize)]
pub struct BrowserProfileSource {
    pub browser: String,
    /// Chromium "User Data" directory.
    pub root: String,
    pub profiles: Vec<BrowserProfileEntry>,
}

#[derive(serde::Serialize)]
pub struct BrowserProfileSources {
    pub sources: Vec<BrowserProfileSource>,
    pub default_profile_dir: String,
    pub effective_profile_dir: String,
}

#[derive(serde::Serialize)]
pub struct BrowserImportSummary {
    pub files: usize,
    pub bytes: u64,
    pub extensions: usize,
    pub dest: String,
}

/// Well-known Chromium-family user-data directories for this platform.
fn chromium_user_data_roots() -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    #[cfg(target_os = "windows")]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            let local = PathBuf::from(local);
            out.push(("Chrome".into(), local.join(r"Google\Chrome\User Data")));
            out.push(("Edge".into(), local.join(r"Microsoft\Edge\User Data")));
            out.push(("Chromium".into(), local.join(r"Chromium\User Data")));
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            let base = PathBuf::from(home).join("Library/Application Support");
            out.push(("Chrome".into(), base.join("Google/Chrome")));
            out.push(("Edge".into(), base.join("Microsoft Edge")));
            out.push(("Chromium".into(), base.join("Chromium")));
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(home) = std::env::var_os("HOME") {
            let base = PathBuf::from(home).join(".config");
            out.push(("Chrome".into(), base.join("google-chrome")));
            out.push(("Edge".into(), base.join("microsoft-edge")));
            out.push(("Chromium".into(), base.join("chromium")));
        }
    }
    out
}

/// Display name of a profile, read from its Preferences (`profile.name`).
fn profile_display_name(preferences: &Path) -> Option<String> {
    let raw = std::fs::read(preferences).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    v["profile"]["name"].as_str().map(str::to_string)
}

/// List "Default" / "Profile N" directories that hold a Preferences file.
fn enumerate_profiles(root: &Path) -> Vec<BrowserProfileEntry> {
    let mut profiles = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else {
        return profiles;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let is_profile = name == "Default"
            || (name.len() > 8
                && name.starts_with("Profile ")
                && name[8..].chars().all(|c| c.is_ascii_digit()));
        if !is_profile || !path.join("Preferences").is_file() {
            continue;
        }
        let display = profile_display_name(&path.join("Preferences")).unwrap_or_else(|| name.clone());
        profiles.push(BrowserProfileEntry { dir: name, name: display });
    }
    profiles.sort_by(|a, b| (a.dir != "Default", &a.dir).cmp(&(b.dir != "Default", &b.dir)));
    profiles
}

/// Enumerate local browsers/profiles that can donate login state, plus the
/// persistent-profile destination paths for the picker UI.
#[tauri::command]
pub fn browser_profile_sources(state: State<'_, crate::AppState>) -> BrowserProfileSources {
    let sources = chromium_user_data_roots()
        .into_iter()
        .filter(|(_, root)| root.is_dir())
        .map(|(browser, root)| {
            let profiles = enumerate_profiles(&root);
            BrowserProfileSource {
                browser,
                root: root.display().to_string(),
                profiles,
            }
        })
        .filter(|s| !s.profiles.is_empty())
        .collect();
    let default = crate::core::browser::persistent_profile_dir(&state.app_data_dir);
    let effective = crate::core::browser::resolve_persistent_profile_dir(&default);
    BrowserProfileSources {
        sources,
        default_profile_dir: default.display().to_string(),
        effective_profile_dir: effective.display().to_string(),
    }
}

/// Files copied verbatim from the source profile (login state and settings).
/// `Secure Preferences` must travel with `Local State` or Chrome treats the
/// profile as tampered and disables extensions.
const PROFILE_FILES: &[&str] = &[
    "Network/Cookies",
    "Network/Cookies-journal",
    "Login Data",
    "Login Data-journal",
    "Preferences",
    "Secure Preferences",
];

/// Directories copied recursively: extension binaries plus their
/// configuration (a proxy extension is useless without its settings).
const PROFILE_DIRS: &[&str] = &[
    "Extensions",
    "Local Extension Settings",
    "Sync Extension Settings",
    "Extension State",
    "Local Storage",
];

fn copy_dir_recursive(src: &Path, dst: &Path, stats: &mut (usize, u64)) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &target, stats)?;
        } else if ty.is_file() {
            std::fs::copy(entry.path(), &target)?;
            stats.0 += 1;
            stats.1 += entry.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    Ok(())
}

/// Windows reports locked browser files as sharing violation (os error 32).
fn friendly_io(e: std::io::Error, what: &str) -> String {
    if e.raw_os_error() == Some(32) || e.kind() == std::io::ErrorKind::PermissionDenied {
        format!("{what} 被占用——请完全退出来源浏览器（含后台进程）后重试")
    } else {
        format!("{what}失败: {e}")
    }
}

/// Copy the slim login-state set from a local browser profile into the given
/// destination profile directory. Cache/history are deliberately skipped —
/// they make up the multi-GB bulk of a profile and carry no login value.
#[tauri::command]
pub async fn browser_profile_import(
    source_root: String,
    profile: String,
    dest: String,
) -> Result<BrowserImportSummary, String> {
    let source_root = PathBuf::from(source_root.trim());
    let dest = PathBuf::from(dest.trim());
    let profile = profile.trim();

    if profile.is_empty() || profile.contains(['/', '\\']) || profile.starts_with('.') {
        return Err(format!("无效的配置目录名: {profile}"));
    }
    let local_state = source_root.join("Local State");
    if !local_state.is_file() {
        return Err(format!(
            "来源目录不是 Chromium 用户数据目录（缺少 Local State）: {}",
            source_root.display()
        ));
    }
    let profile_src = source_root.join(profile);
    if !profile_src.is_dir() {
        return Err(format!("来源配置不存在: {}", profile_src.display()));
    }
    if dest.starts_with(&source_root) {
        return Err("目的位置不能位于来源目录内部".to_string());
    }

    // Release our own browser's file locks on the destination first.
    if let Some(mgr) = crate::core::browser::global() {
        mgr.disconnect().await;
    }

    let mut stats = (0usize, 0u64);
    std::fs::create_dir_all(&dest).map_err(|e| friendly_io(e, "创建目的目录"))?;
    std::fs::copy(&local_state, dest.join("Local State"))
        .map_err(|e| friendly_io(e, "复制 Local State"))?;
    stats.0 += 1;
    stats.1 += std::fs::metadata(&local_state).map(|m| m.len()).unwrap_or(0);

    // Always land in the destination's "Default" profile — that is the one
    // the automation browser actually uses, whatever the source was named.
    let profile_dst = dest.join("Default");
    for rel in PROFILE_FILES {
        let src = profile_src.join(rel);
        if !src.is_file() {
            continue;
        }
        let target = profile_dst.join(rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| friendly_io(e, "创建目的目录"))?;
        }
        std::fs::copy(&src, &target).map_err(|e| friendly_io(e, &format!("复制 {rel}")))?;
        stats.0 += 1;
        stats.1 += std::fs::metadata(&src).map(|m| m.len()).unwrap_or(0);
    }
    for rel in PROFILE_DIRS {
        let src = profile_src.join(rel);
        if !src.is_dir() {
            continue;
        }
        copy_dir_recursive(&src, &profile_dst.join(rel), &mut stats)
            .map_err(|e| friendly_io(e, &format!("复制 {rel}")))?;
    }

    let extensions = std::fs::read_dir(profile_dst.join("Extensions"))
        .map(|rd| rd.flatten().filter(|e| e.path().is_dir()).count())
        .unwrap_or(0);

    Ok(BrowserImportSummary {
        files: stats.0,
        bytes: stats.1,
        extensions,
        dest: dest.display().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recursive_copy_counts_files_and_bytes() {
        let base = std::env::temp_dir().join(format!("siku-imp-{}", uuid::Uuid::new_v4().simple()));
        let src = base.join("src/sub");
        let dst = base.join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("a.bin"), b"12345").unwrap();
        std::fs::write(base.join("src/top.bin"), b"xy").unwrap();

        let mut stats = (0usize, 0u64);
        copy_dir_recursive(&base.join("src"), &dst, &mut stats).unwrap();
        assert_eq!(stats, (2, 7));
        assert_eq!(std::fs::read(dst.join("sub/a.bin")).unwrap(), b"12345");
        assert_eq!(std::fs::read(dst.join("top.bin")).unwrap(), b"xy");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn profile_enumeration_prefers_default_and_requires_preferences() {
        let root = std::env::temp_dir().join(format!("siku-prof-{}", uuid::Uuid::new_v4().simple()));
        for (dir, with_prefs) in [
            ("Profile 2", true),
            ("Default", true),
            ("Profile x", false), // not numeric — skipped even with prefs
            ("Guest Profile", true), // not a numbered profile — skipped
            ("Profile 11", false), // no Preferences — skipped
        ] {
            let d = root.join(dir);
            std::fs::create_dir_all(&d).unwrap();
            if with_prefs {
                std::fs::write(d.join("Preferences"), r#"{"profile":{"name":"n"}}"#).unwrap();
            }
        }
        let profiles = enumerate_profiles(&root);
        let dirs: Vec<&str> = profiles.iter().map(|p| p.dir.as_str()).collect();
        assert_eq!(dirs, ["Default", "Profile 2"]);
        assert_eq!(profiles[0].name, "n");
        let _ = std::fs::remove_dir_all(&root);
    }
}
