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

/// What the user asked to carry over. The three scopes exist because
/// "just make my logins work" and "clone my whole browser" are different
/// intents: the first is a few MB, the second can be several GB.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ImportScope {
    /// Cookies / passwords / extensions only (the historical behaviour).
    Slim,
    /// Slim + the site data that modern sites keep their session in
    /// (IndexedDB, service workers, web storage). Caches and history are
    /// still excluded.
    Site,
    /// Everything under the profile, caches and history included. Only when
    /// the user explicitly asks for it — copy time and disk use are real.
    Full,
}

impl ImportScope {
    fn parse(raw: Option<&str>) -> Self {
        match raw.unwrap_or("slim").trim().to_ascii_lowercase().as_str() {
            "full" => Self::Full,
            "site" => Self::Site,
            _ => Self::Slim,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Slim => "slim",
            Self::Site => "site",
            Self::Full => "full",
        }
    }
}

/// Chromium keeps singleton locks in the user-data dir; they are stale by
/// definition when copied and can make Chrome refuse to start.
const SKIP_NAMES: &[&str] = &[
    "SingletonLock",
    "SingletonCookie",
    "SingletonSocket",
    "DevToolsActivePort",
    "lockfile",
    "LOCK",
];

/// Extra per-profile entries copied by the `site` scope. These are the
/// places modern web apps actually keep their session.
const SITE_DIRS: &[&str] = &["IndexedDB", "Service Worker", "WebStorage", "Session Storage"];
const SITE_FILES: &[&str] = &[
    "Network/Network Persistent State",
    "Network/TransportSecurity",
];

fn is_skipped(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| SKIP_NAMES.contains(&n))
        .unwrap_or(false)
}

/// One file to copy, with its source size for progress accounting.
struct PlannedFile {
    src: PathBuf,
    dst: PathBuf,
    size: u64,
}

fn plan_file(out: &mut Vec<PlannedFile>, src: &Path, dst: &Path) {
    if let Ok(meta) = std::fs::metadata(src) {
        if meta.is_file() {
            out.push(PlannedFile {
                src: src.to_path_buf(),
                dst: dst.to_path_buf(),
                size: meta.len(),
            });
        }
    }
}

/// Walk `src` into `dst`. Symlinks are skipped (loops / escaping the tree),
/// as are Chromium's singleton lock files.
fn plan_dir(out: &mut Vec<PlannedFile>, src: &Path, dst: &Path) -> std::io::Result<()> {
    if !src.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_symlink() || is_skipped(&entry.path()) {
            continue;
        }
        let target = dst.join(entry.file_name());
        if ty.is_dir() {
            plan_dir(out, &entry.path(), &target)?;
        } else if ty.is_file() {
            plan_file(out, &entry.path(), &target);
        }
    }
    Ok(())
}

/// Build the full copy plan up front: it gives the progress bar a total, and
/// keeps the copy loop free of filesystem surprises.
fn build_plan(
    profile_src: &Path,
    dest: &Path,
    local_state: &Path,
    scope: ImportScope,
) -> std::io::Result<Vec<PlannedFile>> {
    let profile_dst = dest.join("Default");
    let mut out = Vec::new();
    // Local State always rides along: it holds the key that decrypts Cookies.
    plan_file(&mut out, local_state, &dest.join("Local State"));

    match scope {
        ImportScope::Slim => {
            for rel in PROFILE_FILES {
                plan_file(&mut out, &profile_src.join(rel), &profile_dst.join(rel));
            }
            for rel in PROFILE_DIRS {
                plan_dir(&mut out, &profile_src.join(rel), &profile_dst.join(rel))?;
            }
        }
        ImportScope::Site => {
            for rel in PROFILE_FILES {
                plan_file(&mut out, &profile_src.join(rel), &profile_dst.join(rel));
            }
            for rel in PROFILE_DIRS.iter().chain(SITE_DIRS.iter()) {
                plan_dir(&mut out, &profile_src.join(rel), &profile_dst.join(rel))?;
            }
            for rel in SITE_FILES {
                plan_file(&mut out, &profile_src.join(rel), &profile_dst.join(rel));
            }
        }
        ImportScope::Full => {
            plan_dir(&mut out, profile_src, &profile_dst)?;
        }
    }
    Ok(out)
}

#[derive(Clone, serde::Serialize)]
struct ImportProgress {
    scope: String,
    /// scanning | copying | done
    phase: String,
    files: usize,
    bytes: u64,
    total_files: usize,
    total_bytes: u64,
    /// Relative path of the file being copied (tail-truncated by the UI).
    current: String,
}

/// Windows reports locked browser files as sharing violation (os error 32).
fn friendly_io(e: std::io::Error, what: &str) -> String {
    if e.raw_os_error() == Some(32) || e.kind() == std::io::ErrorKind::PermissionDenied {
        format!("{what} 被占用——请完全退出来源浏览器（含后台进程）后重试")
    } else {
        format!("{what}失败: {e}")
    }
}

/// Copy the login state (or, per scope, the whole profile) from a local
/// browser profile into the destination profile directory.
///
/// Emits `browser:import-progress` while it runs so the settings page can
/// show a live file/byte counter — a full copy of a multi-GB profile is
/// otherwise a silent multi-minute freeze.
#[tauri::command]
pub async fn browser_profile_import(
    app: tauri::AppHandle,
    source_root: String,
    profile: String,
    dest: String,
    scope: Option<String>,
) -> Result<BrowserImportSummary, String> {
    let scope = ImportScope::parse(scope.as_deref());
    let source_root = PathBuf::from(source_root.trim());
    let dest = PathBuf::from(dest.trim());
    let profile = profile.trim().to_string();

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
    let profile_src = source_root.join(&profile);
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

    let app_handle = app.clone();
    let label = scope.label();
    tokio::task::spawn_blocking(move || {
        use tauri::Emitter;

        std::fs::create_dir_all(&dest).map_err(|e| friendly_io(e, "创建目的目录"))?;

        // Scanning phase: the plan gives us exact totals for the bar.
        let mut plan = build_plan(&profile_src, &dest, &local_state, scope)
            .map_err(|e| friendly_io(e, "扫描来源目录"))?;
        let total_files = plan.len();
        let total_bytes: u64 = plan.iter().map(|f| f.size).sum();
        let _ = app_handle.emit(
            "browser:import-progress",
            ImportProgress {
                scope: label.to_string(),
                phase: "scanning".to_string(),
                files: 0,
                bytes: 0,
                total_files,
                total_bytes,
                current: String::new(),
            },
        );

        let mut copied = 0usize;
        let mut bytes = 0u64;
        let mut last_emit = std::time::Instant::now();
        for file in &mut plan {
            if let Some(parent) = file.dst.parent() {
                std::fs::create_dir_all(parent).map_err(|e| friendly_io(e, "创建目的目录"))?;
            }
            std::fs::copy(&file.src, &file.dst).map_err(|e| {
                friendly_io(
                    e,
                    &format!("复制 {}", file.src.display()),
                )
            })?;
            copied += 1;
            bytes += file.size;
            if last_emit.elapsed().as_millis() >= 200 || copied == total_files {
                last_emit = std::time::Instant::now();
                let current = file
                    .dst
                    .strip_prefix(&dest)
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| file.dst.display().to_string());
                let _ = app_handle.emit(
                    "browser:import-progress",
                    ImportProgress {
                        scope: label.to_string(),
                        phase: "copying".to_string(),
                        files: copied,
                        bytes,
                        total_files,
                        total_bytes,
                        current,
                    },
                );
            }
        }

        let extensions = std::fs::read_dir(dest.join("Default").join("Extensions"))
            .map(|rd| rd.flatten().filter(|e| e.path().is_dir()).count())
            .unwrap_or(0);
        let _ = app_handle.emit(
            "browser:import-progress",
            ImportProgress {
                scope: label.to_string(),
                phase: "done".to_string(),
                files: copied,
                bytes,
                total_files,
                total_bytes,
                current: String::new(),
            },
        );

        Ok(BrowserImportSummary {
            files: copied,
            bytes,
            extensions,
            dest: dest.display().to_string(),
        })
    })
    .await
    .map_err(|e| format!("导入任务异常: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a fake Chromium user-data root with a `Default` profile.
    fn import_fixture() -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("siku-imp-{}", uuid::Uuid::new_v4().simple()));
        let root = base.join("root");
        let profile = root.join("Default");
        for rel in ["Network/Cookies", "Preferences", "IndexedDB/store/x", "Service Worker/sw", "Cache/c"] {
            let path = profile.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"data").unwrap();
        }
        std::fs::create_dir_all(root.join("Spare").join("nested")).unwrap();
        std::fs::write(root.join("Local State"), b"ls").unwrap();
        std::fs::write(root.join("Spare").join("nested").join("z"), b"zz").unwrap();
        std::fs::write(profile.join("SingletonLock"), b"lock").unwrap();
        (base, root)
    }

    fn plan_paths(root: &Path, scope: ImportScope) -> Vec<String> {
        let dest = root.parent().unwrap().join("dest");
        let plan = build_plan(&root.join("Default"), &dest, &root.join("Local State"), scope).unwrap();
        let mut out: Vec<String> = plan
            .iter()
            .map(|f| f.dst.strip_prefix(&dest).unwrap().display().to_string())
            .collect();
        out.sort();
        out
    }

    /// The slim scope is the historical login set: cookies, passwords,
    /// extension state — never site databases or caches.
    #[test]
    fn slim_plan_covers_login_state_only() {
        let (base, root) = import_fixture();
        let paths = plan_paths(&root, ImportScope::Slim);
        assert!(paths.contains(&"Local State".to_string()));
        assert!(paths.contains(&format!("Default{}Network{}Cookies", std::path::MAIN_SEPARATOR, std::path::MAIN_SEPARATOR)));
        assert!(paths.contains(&"Default/Preferences".to_string()) || paths.iter().any(|p| p.ends_with("Preferences")));
        assert!(!paths.iter().any(|p| p.contains("IndexedDB")), "slim must not copy site DBs: {paths:?}");
        assert!(!paths.iter().any(|p| p.contains("Cache")), "slim must not copy caches: {paths:?}");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The `site` scope adds the stores modern apps keep their session in,
    /// while still staying out of the cache directories.
    #[test]
    fn site_plan_adds_site_storage_but_not_caches() {
        let (base, root) = import_fixture();
        let paths = plan_paths(&root, ImportScope::Site);
        assert!(paths.iter().any(|p| p.contains("IndexedDB")), "{paths:?}");
        assert!(paths.iter().any(|p| p.contains("Service Worker")), "{paths:?}");
        assert!(!paths.iter().any(|p| p.contains("Cache")), "site must not copy caches: {paths:?}");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Full copy takes the whole profile (cache included, that is the point
    /// of the choice) but still drops stale singleton locks.
    #[test]
    fn full_plan_includes_caches_and_skips_locks() {
        let (base, root) = import_fixture();
        let paths = plan_paths(&root, ImportScope::Full);
        assert!(paths.iter().any(|p| p.contains("Cache")), "full must include caches: {paths:?}");
        assert!(!paths.iter().any(|p| p.contains("SingletonLock")), "locks must be skipped: {paths:?}");
        assert!(paths.contains(&"Local State".to_string()));
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
