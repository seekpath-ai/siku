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
//! Launched children are closed gracefully on shutdown. The browser is
//! spawned by us (not by chromiumoxide) so the launch flags stay under our
//! control — see `launch_args` — which means its process is owned here too:
//! `BrowserChild` kills it on drop, so an abort-path exit cannot leave an
//! orphaned automation browser behind.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use std::sync::Arc;

use chromiumoxide::browser::Browser;
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
    /// The browser process WE spawned (persistent/temporary). Attach mode
    /// leaves this `None` — its browser belongs to the user and must never be
    /// killed.
    child: Option<BrowserChild>,
}

/// Owns the spawned browser process. `Drop` kills it, preserving the old
/// `kill_on_drop` guarantee now that chromiumoxide is no longer the launcher
/// (a dropped `Connection` must not leave a browser running).
struct BrowserChild(std::process::Child);

impl BrowserChild {
    fn kill_and_reap(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for BrowserChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
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

/// Effective persistent profile dir: the settings override when set,
/// otherwise the given default. Public for the settings/import commands.
pub fn resolve_persistent_profile_dir(default_dir: &Path) -> PathBuf {
    let custom = crate::core::settings_service::cached_settings().browser_profile_dir;
    if let Some(dir) = custom.filter(|d| !d.trim().is_empty()) {
        return PathBuf::from(dir.trim());
    }
    default_dir.to_path_buf()
}

/// Shell-ish splitting for user-supplied launch args: whitespace separates,
/// double quotes group (Windows paths with spaces), quotes are stripped.
fn parse_launch_args(raw: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut has_content = false;
    for c in raw.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                has_content = true;
            }
            c if c.is_whitespace() && !in_quotes => {
                if has_content {
                    args.push(std::mem::take(&mut cur));
                    has_content = false;
                }
            }
            _ => {
                cur.push(c);
                has_content = true;
            }
        }
    }
    if has_content {
        args.push(cur);
    }
    args
}

/// Args that would break the launch/attach contract or the headed-window
/// requirement — dropped with a warning rather than honored.
const BLOCKED_LAUNCH_ARGS: &[&str] = &[
    "--user-data-dir",
    "--remote-debugging-port",
    "--remote-debugging-pipe",
    "--headless",
    // Would silently undo the whole point of the profile import (session
    // cookies and proxy/ad-block extensions are why users import at all).
    "--disable-extensions",
    "--disable-extensions-except",
];

/// Drop user-supplied args that would break the launch/attach contract.
fn filter_launch_args(args: Vec<String>) -> Vec<String> {
    args.into_iter()
        .filter(|a| {
            let key = a.split('=').next().unwrap_or(a);
            let blocked = BLOCKED_LAUNCH_ARGS
                .iter()
                .any(|b| key.eq_ignore_ascii_case(b));
            if blocked {
                tracing::warn!("ignoring blocked browser launch arg: {a}");
            }
            !blocked
        })
        .collect()
}

/// Extra launch args from Settings, minus anything that conflicts with how
/// the manager launches and drives the browser.
fn custom_launch_args() -> Vec<String> {
    let raw = crate::core::settings_service::cached_settings().browser_launch_args;
    let Some(raw) = raw.filter(|s| !s.trim().is_empty()) else {
        return Vec::new();
    };
    filter_launch_args(parse_launch_args(&raw))
}

/// Liveness probes must never hang: a wedged browser process (alive but not
/// answering CDP) would otherwise block the manager lock forever, taking
/// every subsequent browser call down with it.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Tab count with a hard timeout. `None` = browser unreachable or wedged;
/// callers treat that as a dead connection and rebuild.
async fn tab_count(browser: &Browser) -> Option<usize> {
    tokio::time::timeout(PROBE_TIMEOUT, browser.pages())
        .await
        .ok()
        .and_then(|r| r.ok())
        .map(|p| p.len())
}

/// Reserve a loopback port for `--remote-debugging-port` (bind-then-drop; the
/// gap before Chrome binds it is tiny, and a collision surfaces as a clear
/// startup failure rather than a silent misconnection).
fn free_debug_port() -> Result<u16, String> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
        .map_err(|e| format!("cannot allocate a debug port: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("cannot read the allocated port: {e}"))?
        .port();
    drop(listener);
    Ok(port)
}

/// Launch arguments for the browser we spawn ourselves.
///
/// Deliberately NOT chromiumoxide's `DEFAULT_ARGS`: that set force-adds
/// `--disable-extensions` (silently disables every extension in the imported
/// profile) and `--password-store=basic` (saved passwords stop autofilling).
/// Only flags that keep the automation window usable are kept here.
fn launch_args(port: u16, user_data_dir: &Path, extra: &[String]) -> Vec<String> {
    let mut args = vec![
        format!("--remote-debugging-port={port}"),
        format!("--user-data-dir={}", user_data_dir.display()),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
        "--disable-session-crashed-bubble".to_string(),
        "--hide-crash-restore-bubble".to_string(),
        // Chromium containers: the default /dev/shm is often too small.
        "--disable-dev-shm-usage".to_string(),
    ];
    args.extend(extra.iter().cloned());
    args
}

/// Poll the DevTools endpoint until it answers. Also watches the child so an
/// immediate exit (profile locked by another Chrome, bad flags) becomes a
/// useful error instead of a 30-second hang.
async fn wait_for_debug_endpoint(
    port: u16,
    child: &mut BrowserChild,
) -> Result<(), String> {
    let url = format!("http://127.0.0.1:{port}/json/version");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(800))
        .build()
        .map_err(|e| format!("cannot build the readiness probe: {e}"))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if let Ok(Some(status)) = child.0.try_wait() {
            return Err(format!(
                "浏览器启动后立即退出（{status}）。常见原因：该配置目录正被另一个 Chrome/Edge 实例占用，或启动参数有误。"
            ));
        }
        if let Ok(resp) = client.get(&url).send().await {
            if resp.status().is_success() {
                return Ok(());
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "浏览器调试端口 {port} 在 30 秒内未就绪（启动被阻塞或窗口未创建）"
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }
}

impl BrowserManager {
    /// Connect (or reuse a healthy existing connection). Idempotent.
    pub async fn connect(&self, mode: ConnectMode) -> Result<ConnectOutcome, String> {
        let mut guard = self.conn.lock().await;

        if let Some(existing) = guard.as_mut() {
            match tab_count(&existing.browser).await {
                Some(tabs) => {
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
                        tabs,
                    });
                }
                None => {
                    // Stale connection (browser closed by the user, crashed…):
                    // tear it down, then fall through to a fresh connect.
                    tracing::info!("stale browser connection detected, reconnecting");
                    let stale = guard.take().expect("checked above");
                    teardown(stale).await;
                }
            }
        }

        let (browser, handler, executable, profile_dir, cleanup_dir, child) = match &mode {
            ConnectMode::Attach { port } => {
                let (browser, handler) = Browser::connect(format!("http://127.0.0.1:{port}"))
                    .await
                    .map_err(|e| {
                        format!(
                            "No browser debug endpoint at 127.0.0.1:{port} ({e}). Start Chrome/Edge with --remote-debugging-port={port} first."
                        )
                    })?;
                (browser, handler, None, None, None, None)
            }
            ConnectMode::Persistent | ConnectMode::Temporary => {
                let exe = detect_executable()?;
                let (profile_dir, cleanup_dir) = match mode {
                    ConnectMode::Persistent => {
                        let dir = resolve_persistent_profile_dir(&persistent_profile_dir(
                            &self.app_data_dir,
                        ));
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

                // Manual launch on purpose. chromiumoxide's `Browser::launch`
                // appends `--disable-extensions` whenever it was not given an
                // explicit extension list (config.rs: "if self.extensions
                // .is_empty()"), which silently disabled every extension in
                // the imported profile. It also forces `--password-store=basic`
                // (saved passwords stop autofilling). We need neither, so we
                // spawn the browser ourselves and only borrow its CDP endpoint.
                let port = free_debug_port()?;
                let args = launch_args(port, &user_data_dir, &custom_launch_args());
                let child = std::process::Command::new(&exe)
                    .args(&args)
                    .spawn()
                    .map_err(|e| format!("failed to launch {}: {e}", exe.display()))?;
                let mut child = BrowserChild(child);
                if let Err(e) = wait_for_debug_endpoint(port, &mut child).await {
                    child.kill_and_reap();
                    return Err(e);
                }
                let (browser, handler) = match Browser::connect(format!(
                    "http://127.0.0.1:{port}"
                ))
                .await
                {
                    Ok(v) => v,
                    Err(e) => {
                        child.kill_and_reap();
                        return Err(format!(
                            "浏览器已启动但 CDP 连接失败（{e}）。请检查启动参数与配置目录。"
                        ));
                    }
                };
                (browser, handler, Some(exe), profile_dir, cleanup_dir, Some(child))
            }
        };

        // The handler MUST be driven before any CDP command: version()/pages()
        // only receive their responses while the handler loop is being polled.
        // Spawning it after those calls deadlocks the connect (browser window
        // opens, the tool hangs forever).
        let handler_task = tokio::spawn(async move {
            let mut handler = handler;
            loop {
                match handler.next().await {
                    Some(Ok(_)) => {}
                    _ => break,
                }
            }
        });

        let product = browser
            .version()
            .await
            .map(|v| v.product)
            .unwrap_or_else(|_| "unknown".to_string());
        // A browser that cannot answer this right after launch is not usable;
        // report 0 tabs rather than hang — the next connect retries cleanly.
        let tabs = tab_count(&browser).await.unwrap_or(0);

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
            child,
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
            Some(c) => tab_count(&c.browser).await.is_some(),
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

/// Human-verification heuristics: matched challenge providers/widgets on the
/// current page. Empty = no challenge detected. Deliberately conservative —
/// a false positive costs one browser_captcha_wait round-trip, a false
/// negative strands the agent clicking against an unsolvable wall.
const CAPTCHA_PROBE_JS: &str = r#"(() => {
  const signals = [];
  for (const f of document.querySelectorAll('iframe[src]')) {
    const src = (f.getAttribute('src') || '').toLowerCase();
    if (/recaptcha|hcaptcha|challenges\.cloudflare\.com|arkoselabs|geetest|turnstile/.test(src)) {
      signals.push('captcha iframe: ' + src.slice(0, 80));
    }
  }
  if (document.querySelector('.cf-turnstile, #cf-chl-widget, .g-recaptcha, .h-captcha, .geetest_holder, #geetest-wrap')) {
    signals.push('captcha widget element');
  }
  const title = (document.title || '');
  if (/just a moment|attention required|verify you are human|安全验证|人机验证/i.test(title)) {
    signals.push('challenge page title: ' + title);
  }
  if (/cdn-cgi\/challenge/.test(location.pathname)) {
    signals.push('cloudflare challenge url');
  }
  return signals;
})()"#;

/// Probe the page for human-verification challenges. Any error (page
/// mid-navigation, evaluate failed) is treated as "no signals" — the probe
/// must never break a navigation or click that actually succeeded.
pub async fn captcha_signals(page: &Page) -> Vec<String> {
    page.evaluate(CAPTCHA_PROBE_JS)
        .await
        .ok()
        .and_then(|r| r.into_value::<Vec<String>>().ok())
        .unwrap_or_default()
}

/// Warning appended to tool results when a challenge is on screen.
pub fn captcha_warning(signals: &[String]) -> String {
    format!(
        "\n⚠ HUMAN VERIFICATION detected ({}). A person must solve it in the browser window — \
         call browser_captcha_wait to wait for completion instead of clicking blindly.",
        signals.join("; ")
    )
}

/// Close what we own, never what we merely attached to. Dropping `Browser`
/// kills a launched child even if graceful close fails (kill_on_drop).
async fn teardown(conn: Connection) {
    let Connection {
        browser,
        handler,
        mode,
        cleanup_dir,
        child,
        ..
    } = conn;
    // Graceful close needs &mut; possible only when no phase-2 tool still
    // holds an Arc clone — otherwise drop alone still kills a launched child
    // (kill_on_drop), so this is best-effort by design. Timeout-bounded: a
    // wedged browser must not hang browser_close / app shutdown.
    let mut browser = browser;
    if !matches!(mode, ConnectMode::Attach { .. }) {
        if let Some(b) = Arc::get_mut(&mut browser) {
            let _ = tokio::time::timeout(PROBE_TIMEOUT, b.close()).await;
        }
    }
    drop(browser);
    handler.abort();
    // Reap the process we spawned ourselves (the `Browser` handle no longer
    // owns it): a wedged browser must not survive browser_close/shutdown.
    if let Some(mut child) = child {
        child.kill_and_reap();
    }
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

    /// Regression: the launch flags must never disable extensions or switch
    /// off the OS keychain. chromiumoxide's own defaults did both — the first
    /// silently killed every imported extension, the second broke password
    /// autofill. This is the whole reason we spawn the browser ourselves.
    #[test]
    fn manual_launch_args_keep_extensions_and_keychain() {
        let dir = PathBuf::from("/tmp/siku-profile");
        let args = launch_args(9333, &dir, &["--lang=zh-CN".to_string()]);
        assert!(args.iter().any(|a| a == "--remote-debugging-port=9333"), "{args:?}");
        assert!(
            args.iter().any(|a| a == &format!("--user-data-dir={}", dir.display())),
            "{args:?}"
        );
        assert!(args.iter().any(|a| a == "--lang=zh-CN"), "custom args pass through: {args:?}");
        for forbidden in ["--disable-extensions", "--disable-extensions-except", "--password-store=basic"] {
            assert!(!args.iter().any(|a| a == forbidden), "{forbidden} must not be passed: {args:?}");
        }
    }

    /// User-supplied args may not re-introduce what we removed either.
    #[test]
    fn blocked_launch_args_are_filtered_out() {
        let filtered = filter_launch_args(vec![
            "--disable-extensions".to_string(),
            "--disable-extensions-except=/tmp/x".to_string(),
            "--headless=new".to_string(),
            "--user-data-dir=/tmp/other".to_string(),
            "--lang=zh-CN".to_string(),
        ]);
        assert_eq!(filtered, vec!["--lang=zh-CN".to_string()]);
    }

    #[test]
    fn free_debug_port_is_usable() {
        let port = free_debug_port().expect("a loopback port");
        assert!(port > 1024, "{port}");
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

    #[test]
    fn launch_args_splitting() {
        assert_eq!(parse_launch_args(""), Vec::<String>::new());
        assert_eq!(parse_launch_args("  "), Vec::<String>::new());
        assert_eq!(
            parse_launch_args("--proxy-server=http://127.0.0.1:7890"),
            vec!["--proxy-server=http://127.0.0.1:7890"]
        );
        // Quoted Windows path with spaces stays one argument, quotes stripped.
        assert_eq!(
            parse_launch_args(r#"--load-extension="D:\my ext\helper" --foo"#),
            vec!["--load-extension=D:\\my ext\\helper", "--foo"]
        );
        assert_eq!(parse_launch_args(r#"--flag="""#), vec!["--flag="]);
    }

    #[test]
    fn blocked_args_filtered() {
        for bad in [
            "--user-data-dir=/tmp/x",
            "--remote-debugging-port=1234",
            "--headless",
        ] {
            let key = bad.split('=').next().unwrap_or(bad);
            assert!(
                BLOCKED_LAUNCH_ARGS.iter().any(|b| key.eq_ignore_ascii_case(b)),
                "{bad} must be blocked"
            );
        }
        assert!(
            !BLOCKED_LAUNCH_ARGS
                .iter()
                .any(|b| "--proxy-server".eq_ignore_ascii_case(b))
        );
    }

    #[test]
    fn captcha_warning_lists_signals() {
        let w = captcha_warning(&["captcha iframe: x".to_string()]);
        assert!(w.contains("HUMAN VERIFICATION"));
        assert!(w.contains("captcha iframe: x"));
        assert!(w.contains("browser_captcha_wait"));
    }
}
