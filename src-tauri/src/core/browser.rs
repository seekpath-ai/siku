//! L1 browser automation — connection layer.
//!
//! One app-global [`BrowserManager`] owns at most one CDP connection to a
//! real Chrome/Edge instance. Three supply modes:
//!
//! - **Persistent** (default): launches the detected browser with a dedicated
//!   profile under `app_data/browser-profile`, so site logins survive restarts
//!   (Chrome 136+ refuses CDP on the *default* user profile, hence a dedicated
//!   one).
//! - **Temporary**: launches with a clean throwaway profile — a stronger
//!   `web_fetch` for JS-rendered pages.
//! - **Attach**: connects to a browser the user started with
//!   `--remote-debugging-port`; siku must never close that browser.
//!
//! Launched children are closed gracefully on shutdown; chromiumoxide also
//! sets `kill_on_drop`, so even an abort-path process exit cannot leave an
//! orphaned automation browser.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use std::sync::Arc;

use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::Page;
use futures::StreamExt;
use tokio::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectMode {
    Persistent,
    Temporary,
    Attach { port: u16 },
}

impl ConnectMode {
    pub fn label(&self) -> String {
        match self {
            ConnectMode::Persistent => "persistent".to_string(),
            ConnectMode::Temporary => "temporary".to_string(),
            ConnectMode::Attach { port } => format!("attach (port {port})"),
        }
    }
}

#[derive(Debug)]
pub struct ConnectOutcome {
    pub reused: bool,
    pub product: String,
    pub mode: ConnectMode,
    pub executable: Option<PathBuf>,
    pub profile_dir: Option<PathBuf>,
    pub tabs: usize,
}

struct Connection {
    browser: Arc<Browser>,
    handler: tokio::task::JoinHandle<()>,
    mode: ConnectMode,
    executable: Option<PathBuf>,
    profile_dir: Option<PathBuf>,
    /// Throwaway profile removed when the connection ends (temporary mode).
    cleanup_dir: Option<PathBuf>,
}

pub struct BrowserManager {
    app_data_dir: PathBuf,
    conn: Mutex<Option<Connection>>,
    /// Target id of the tab browser tools operate on. Resolved lazily: an
    /// empty or stale value falls back to the most recent tab.
    active_target: Mutex<Option<String>>,
}

static MANAGER: OnceLock<BrowserManager> = OnceLock::new();

/// Called once during app setup. Subsequent calls are ignored.
pub fn init(app_data_dir: PathBuf) {
    let _ = MANAGER.set(BrowserManager {
        app_data_dir,
        conn: Mutex::new(None),
        active_target: Mutex::new(None),
    });
}

/// `None` before app setup (unit tests) — tools must report that as an error,
/// not panic.
pub fn global() -> Option<&'static BrowserManager> {
    MANAGER.get()
}

/// Resolve the browser executable: settings override wins, then
/// chromiumoxide's detector (CHROME env, PATH names, Windows registry,
/// common install paths — Chrome and Edge on all three platforms).
pub fn detect_executable() -> Result<PathBuf, String> {
    let custom = crate::core::settings_service::cached_settings().browser_path;
    if let Some(custom) = custom.filter(|p| !p.trim().is_empty()) {
        let path = PathBuf::from(custom.trim());
        if path.is_file() {
            return Ok(path);
        }
        return Err(format!(
            "Configured browser_path does not exist: {}. Fix it in Settings or clear it to auto-detect.",
            path.display()
        ));
    }
    chromiumoxide::detection::default_executable(Default::default()).map_err(|_| {
        "No Chrome or Edge installation found. Install Chrome/Edge, or set the executable path in Settings.".to_string()
    })
}

/// The persistent automation profile lives inside app data so it survives
/// restarts and stays clearly separated from the user's real profile.
pub fn persistent_profile_dir(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("browser-profile")
}

impl BrowserManager {
    /// Connect (or reuse a healthy existing connection). Idempotent.
    pub async fn connect(&self, mode: ConnectMode) -> Result<ConnectOutcome, String> {
        let mut guard = self.conn.lock().await;

        if let Some(existing) = guard.as_mut() {
            match existing.browser.pages().await {
                Ok(pages) => {
                    let product = existing
                        .browser
                        .version()
                        .await
                        .map(|v| v.product)
                        .unwrap_or_else(|_| "unknown".to_string());
                    return Ok(ConnectOutcome {
                        reused: true,
                        product,
                        mode: existing.mode.clone(),
                        executable: existing.executable.clone(),
                        profile_dir: existing.profile_dir.clone(),
                        tabs: pages.len(),
                    });
                }
                Err(_) => {
                    // Stale connection (browser closed by the user, crashed…):
                    // tear it down, then fall through to a fresh connect.
                    tracing::info!("stale browser connection detected, reconnecting");
                    let stale = guard.take().expect("checked above");
                    teardown(stale).await;
                }
            }
        }

        let (browser, handler, executable, profile_dir, cleanup_dir) = match &mode {
            ConnectMode::Attach { port } => {
                let (browser, handler) = Browser::connect(format!("http://127.0.0.1:{port}"))
                    .await
                    .map_err(|e| {
                        format!(
                            "No browser debug endpoint at 127.0.0.1:{port} ({e}). Start Chrome/Edge with --remote-debugging-port={port} first."
                        )
                    })?;
                (browser, handler, None, None, None)
            }
            ConnectMode::Persistent | ConnectMode::Temporary => {
                let exe = detect_executable()?;
                let (profile_dir, cleanup_dir) = match mode {
                    ConnectMode::Persistent => {
                        let dir = persistent_profile_dir(&self.app_data_dir);
                        std::fs::create_dir_all(&dir)
                            .map_err(|e| format!("cannot create browser profile dir: {e}"))?;
                        (Some(dir), None)
                    }
                    _ => {
                        let dir = std::env::temp_dir()
                            .join(format!("siku-browser-{}", uuid::Uuid::new_v4().simple()));
                        (None, Some(dir))
                    }
                };
                let user_data_dir = profile_dir
                    .clone()
                    .or_else(|| cleanup_dir.clone())
                    .expect("one of profile/cleanup dir is always set");
                let config = BrowserConfig::builder()
                    .chrome_executable(&exe)
                    .user_data_dir(user_data_dir)
                    .with_head()
                    .launch_timeout(std::time::Duration::from_secs(30))
                    .arg("--no-first-run")
                    .arg("--no-default-browser-check")
                    .arg("--disable-session-crashed-bubble")
                    .arg("--hide-crash-restore-bubble")
                    .build()
                    .map_err(|e| format!("invalid browser config: {e}"))?;
                let (browser, handler) = Browser::launch(config)
                    .await
                    .map_err(|e| format!("failed to launch {}: {e}", exe.display()))?;
                (browser, handler, Some(exe), profile_dir, cleanup_dir)
            }
        };

        let product = browser
            .version()
            .await
            .map(|v| v.product)
            .unwrap_or_else(|_| "unknown".to_string());
        let tabs = browser.pages().await.map(|p| p.len()).unwrap_or(0);

        let handler_task = tokio::spawn(async move {
            let mut handler = handler;
            loop {
                match handler.next().await {
                    Some(Ok(_)) => {}
                    _ => break,
                }
            }
        });
        let browser = Arc::new(browser);

        let outcome = ConnectOutcome {
            reused: false,
            product,
            mode: mode.clone(),
            executable: executable.clone(),
            profile_dir: profile_dir.clone(),
            tabs,
        };
        guard.replace(Connection {
            browser,
            handler: handler_task,
            mode,
            executable,
            profile_dir,
            cleanup_dir,
        });
        Ok(outcome)
    }

    /// Close the connection (and the browser, if we launched it). Returns
    /// whether there was anything to close.
    pub async fn disconnect(&self) -> bool {
        let conn = self.conn.lock().await.take();
        match conn {
            Some(c) => {
                teardown(c).await;
                true
            }
            None => false,
        }
    }

    /// App-exit hook: same as disconnect; launched browsers are closed so the
    /// profile flushes cleanly.
    pub async fn shutdown(&self) {
        if self.disconnect().await {
            tracing::info!("browser connection closed during shutdown");
        }
    }

    /// Shared handle to the live `Browser` for phase-2 tools (navigate,
    /// snapshot…). Verifies liveness before handing it out.
    pub async fn live_browser(&self) -> Option<Arc<Browser>> {
        let mut guard = self.conn.lock().await;
        let alive = match guard.as_ref() {
            Some(c) => c.browser.pages().await.is_ok(),
            None => false,
        };
        if alive {
            guard.as_ref().map(|c| c.browser.clone())
        } else if let Some(stale) = guard.take() {
            teardown(stale).await;
            None
        } else {
            None
        }
    }

    /// The tab browser tools act on: the remembered target when it still
    /// exists, otherwise the most recent tab (which becomes the new memory).
    pub async fn active_page(&self) -> Result<Page, String> {
        let browser = self
            .live_browser()
            .await
            .ok_or("No browser connection — call browser_connect first")?;
        let pages = browser
            .pages()
            .await
            .map_err(|e| format!("cannot list tabs: {e}"))?;
        let Some(last) = pages.last() else {
            return Err("The browser has no open tab — call browser_navigate to open one".to_string());
        };
        let stored = self.active_target.lock().await.clone();
        let page = stored
            .and_then(|id| pages.iter().find(|p| p.target_id().as_ref().to_string() == id).cloned())
            .unwrap_or_else(|| last.clone());
        *self.active_target.lock().await = Some(page.target_id().as_ref().to_string());
        Ok(page)
    }

    /// All open tabs in browser order, newest last.
    pub async fn list_tabs(&self) -> Result<Vec<(String, Page)>, String> {
        let browser = self
            .live_browser()
            .await
            .ok_or("No browser connection — call browser_connect first")?;
        let pages = browser
            .pages()
            .await
            .map_err(|e| format!("cannot list tabs: {e}"))?;
        Ok(pages
            .into_iter()
            .map(|p| (p.target_id().as_ref().to_string(), p))
            .collect())
    }

    /// Remember which target subsequent tool calls act on.
    pub async fn set_active(&self, target_id: &str) {
        *self.active_target.lock().await = Some(target_id.to_string());
    }

    /// Forget the remembered target (e.g. after closing the active tab).
    pub async fn clear_active(&self) {
        *self.active_target.lock().await = None;
    }
}

/// Close what we own, never what we merely attached to. Dropping `Browser`
/// kills a launched child even if graceful close fails (kill_on_drop).
async fn teardown(conn: Connection) {
    let Connection {
        browser,
        handler,
        mode,
        cleanup_dir,
        ..
    } = conn;
    // Graceful close needs &mut; possible only when no phase-2 tool still
    // holds an Arc clone — otherwise drop alone still kills a launched child
    // (kill_on_drop), so this is best-effort by design.
    let mut browser = browser;
    if !matches!(mode, ConnectMode::Attach { .. }) {
        if let Some(b) = Arc::get_mut(&mut browser) {
            let _ = b.close().await;
        }
    }
    drop(browser);
    handler.abort();
    if let Some(dir) = cleanup_dir {
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_labels() {
        assert_eq!(ConnectMode::Persistent.label(), "persistent");
        assert_eq!(ConnectMode::Temporary.label(), "temporary");
        assert_eq!(ConnectMode::Attach { port: 9222 }.label(), "attach (port 9222)");
    }

    #[test]
    fn persistent_profile_under_app_data() {
        let dir = persistent_profile_dir(Path::new("/data/siku"));
        assert_eq!(dir, PathBuf::from("/data/siku/browser-profile"));
    }

    #[tokio::test]
    async fn attach_without_browser_fails_with_guidance() {
        let dir = std::env::temp_dir().join(format!("siku-test-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let mgr = BrowserManager {
            app_data_dir: dir.clone(),
            conn: Mutex::new(None),
            active_target: Mutex::new(None),
        };
        // Port 1 is never a browser; the error must point the user at the fix.
        let err = mgr.connect(ConnectMode::Attach { port: 1 }).await.unwrap_err();
        assert!(err.contains("--remote-debugging-port"), "unexpected: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
