mod ai;
mod commands;
mod core;
mod file_store;
mod pdf;
mod sync;

use std::sync::Mutex;
use tauri::{Emitter, Manager};
use tracing::info;

pub struct AppState {
    pub db: sqlx::SqlitePool,
    pub app_data_dir: std::path::PathBuf,
    pub approval_senders: std::sync::Arc<
        tokio::sync::Mutex<
            std::collections::HashMap<
                String,
                tokio::sync::mpsc::UnboundedSender<crate::ai::agent::engine::ApprovalResponse>,
            >,
        >,
    >,
    pub cancel_tokens: std::sync::Arc<
        tokio::sync::Mutex<
            std::collections::HashMap<String, tokio_util::sync::CancellationToken>,
        >,
    >,
    /// Background tasks (bash run_in_background).
    pub tasks: crate::core::tasks::TaskStore,
    /// AskUserQuestion answer channels per session.
    pub ask_senders: std::sync::Arc<
        tokio::sync::Mutex<
            std::collections::HashMap<
                String,
                tokio::sync::mpsc::UnboundedSender<serde_json::Value>,
            >,
        >,
    >,
    /// Broadcast channel used to signal background loops to shut down
    /// gracefully when the application is exiting.
    pub shutdown_tx: tokio::sync::broadcast::Sender<()>,
    /// JoinHandles of tracked background tasks. The exit path aborts them
    /// before finalizing the database, so no task can hold a pool connection
    /// past the finalize window (which would panic inside sqlx on close).
    pub background_tasks: std::sync::Arc<
        tokio::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
    >,
}

/// Spawn a background task and register its handle so the exit path can abort
/// it before CR-SQLite finalization. Aborting is deterministic: whatever the
/// task is doing, its pool connections are released back to the pool.
pub fn spawn_tracked(
    tasks: &std::sync::Arc<tokio::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    future: impl std::future::Future<Output = ()> + Send + 'static,
) {
    let handle = tokio::spawn(future);
    // Best-effort registration; if the exit path already holds the lock
    // (unlikely mid-shutdown), the task still runs and finalize_db's
    // poll-acquire loop covers it.
    if let Ok(mut list) = tasks.try_lock() {
        list.retain(|h| !h.is_finished());
        list.push(handle);
    }
}

impl Drop for AppState {
    fn drop(&mut self) {
        // Last-resort cleanup: if the exit-path cleanup (main window
        // CloseRequested / RunEvent::Exit) did not run finalize_db, the pool
        // will be closed when this state is dropped. SQLite/CR-SQLite panic
        // on close if statements are still open, so try to finalize here.
        // This block_on is best-effort; catch_unwind prevents a failing
        // runtime from making shutdown worse.
        let pool = self.db.clone();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            match tokio::runtime::Handle::try_current() {
                Ok(handle) => {
                    let _ = handle.block_on(async move {
                        if let Err(e) = crate::core::db::finalize_db(&pool).await {
                            tracing::error!("AppState drop finalize_db failed: {e}");
                        }
                    });
                }
                Err(_) => {
                    tracing::warn!(
                        "no tokio runtime on this thread during AppState drop; using a temporary runtime"
                    );
                    // The main runtime may already be gone, but the pool
                    // worker may still be alive on it. A temporary
                    // current-thread runtime can still drive the acquire +
                    // crsql_finalize + close sequence to completion.
                    if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    {
                        let _ = rt.block_on(async move {
                            if let Err(e) = crate::core::db::finalize_db(&pool).await {
                                tracing::error!("AppState drop finalize_db failed: {e}");
                            }
                        });
                    }
                }
            }
        }));
    }
}

/// Tracks whether the frontend and backend have finished initializing.
/// When both are done, the splashscreen window closes and the main
/// window becomes visible.
struct SetupState {
    frontend_done: bool,
    backend_done: bool,
}

/// Called by the frontend when React finishes loading settings.
/// Also called internally by the backend async task after DB init.
/// When both sides report ready, closes the splash window and
/// shows the main window.
fn try_finish_startup(app: &tauri::AppHandle) {
    let state = match app.try_state::<Mutex<SetupState>>() {
        Some(s) => s,
        None => return,
    };
    let both_done = {
        let s = state.lock().unwrap();
        s.frontend_done && s.backend_done
    };
    if !both_done {
        return;
    }

    info!("startup complete — closing splashscreen, showing main window");

    if let Some(splash) = app.get_webview_window("splashscreen") {
        let _ = splash.close();
        // The splash is gone; a later panic dialog must not be owned by a
        // stale HWND (it could attach to an unrelated reused window handle).
        #[cfg(target_os = "windows")]
        PANIC_DIALOG_OWNER.store(0, std::sync::atomic::Ordering::Relaxed);
    }
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.show();
        let _ = main.set_focus();
    }
}

#[tauri::command]
async fn set_complete(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(state) = app.try_state::<Mutex<SetupState>>() {
        let mut s = state.lock().map_err(|e| e.to_string())?;
        s.frontend_done = true;
    }
    try_finish_startup(&app);
    Ok(())
}

/// Create the always-on-top pet window: a small transparent ball
/// that the OS window manager can drag across all screens.
pub fn create_pet_window<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
) -> Result<tauri::WebviewWindow<R>, tauri::Error> {
    let pet = tauri::WebviewWindowBuilder::new(app, "pet", tauri::WebviewUrl::default())
        .title("思库宠物")
        .inner_size(48.0, 48.0)
        .resizable(false)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(false)
        .build()?;
    let _ = pet.set_background_color(Some(tauri::window::Color(0, 0, 0, 0)));
    tracing::info!("pet window created");
    Ok(pet)
}

/// Windows only: HWND of the splashscreen, used as the OWNER of the native
/// panic dialog. The splash is always-on-top; an owner-less MessageBox is a
/// normal (non-topmost) window and renders BEHIND it — a startup failure
/// (e.g. a missing DLL) would leave the user staring at a frozen splash,
/// with the error only discoverable via the taskbar. An owned dialog is
/// always placed above its owner.
#[cfg(target_os = "windows")]
static PANIC_DIALOG_OWNER: std::sync::atomic::AtomicIsize =
    std::sync::atomic::AtomicIsize::new(0);

/// Remember the splashscreen's HWND so startup failures show their native
/// dialog above the always-on-top splash. No-op on non-Windows.
#[cfg(target_os = "windows")]
pub fn capture_panic_dialog_owner<R: tauri::Runtime>(window: &tauri::WebviewWindow<R>) {
    use raw_window_handle::HasWindowHandle;
    if let Ok(handle) = window.window_handle() {
        if let raw_window_handle::RawWindowHandle::Win32(win) = handle.as_raw() {
            PANIC_DIALOG_OWNER.store(
                win.hwnd.get() as isize,
                std::sync::atomic::Ordering::Relaxed,
            );
        }
    }
}

#[cfg(not(target_os = "windows"))]
pub fn capture_panic_dialog_owner<R: tauri::Runtime>(_window: &tauri::WebviewWindow<R>) {}

/// Full shutdown cleanup: signal background loops to stop, cancel in-flight
/// agent turns, abort tracked background tasks, then finalize CR-SQLite and
/// close the pool.
///
/// This must run while the async runtime is still alive and *before* the pool
/// is dropped, because a connection dropped without `crsql_finalize()` panics
/// inside sqlx (`sqlite3_close` → SQLITE_BUSY). `RunEvent::Exit` alone is not
/// reliable on Windows (observed: window close exits the process without it),
/// so this is invoked from the main window's CloseRequested handler, from
/// `RunEvent::ExitRequested` and from `RunEvent::Exit` — the `done` flag makes
/// it idempotent.
/// Async core of the shutdown cleanup. See `run_shutdown_cleanup` for the
/// blocking wrapper used by synchronous exit hooks.
async fn run_shutdown_cleanup_async(
    app: &tauri::AppHandle,
    done: &std::sync::atomic::AtomicBool,
) {
    if done.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let Some(state) = app.try_state::<AppState>() else {
        tracing::warn!("AppState not available during shutdown cleanup");
        return;
    };
    let pool = state.db.clone();

    // 1. Signal background loops to stop so they release their
    // database connections before we try to finalize CR-SQLite.
    let _ = state.shutdown_tx.send(());

    // 2. Cancel any in-flight agent turns. The CancellationToken is
    // watched by the engine via tokio::select! as well as polled each round.
    {
        let tokens = state.cancel_tokens.lock().await;
        let session_ids: Vec<String> = tokens.keys().cloned().collect();
        for token in tokens.values() {
            token.cancel();
        }
        info!(
            session_ids = ?session_ids,
            "cancelled {} agent turn(s)",
            tokens.len()
        );
    }

    // 3. Abort tracked background tasks. If there really are tasks
    // running, give them a moment to observe the cancel/shutdown
    // signal and release their pool connections; otherwise skip the
    // sleep so a quiet app closes immediately.
    let has_work: bool;
    {
        let tokens = state.cancel_tokens.lock().await;
        let mut handles = state.background_tasks.lock().await;
        has_work = !tokens.is_empty() || !handles.is_empty();
        if has_work {
            // Agent turns may be waiting on an LLM response; a few
            // seconds covers the common case where the model has just
            // returned and is persisting results to the database.
            tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
        }
        info!(
            count = handles.len(),
            "aborting {} tracked background task(s)",
            handles.len()
        );
        for h in handles.iter() {
            h.abort();
        }
        handles.clear();
    }
    // Give the runtime a moment to drop the aborted tasks so
    // their connections return to the pool. Only wait when we
    // actually aborted something; an idle app needs no extra delay.
    if has_work {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    if let Err(e) = crate::core::db::finalize_db(&pool).await {
        tracing::error!("failed to finalize database on exit: {e}");
    }
}

/// Full shutdown cleanup: signal background loops to stop, cancel in-flight
/// agent turns, abort tracked background tasks, then finalize CR-SQLite and
/// close the pool.
///
/// This must run while the async runtime is still alive and *before* the pool
/// is dropped, because a connection dropped without `crsql_finalize()` panics
/// inside sqlx (`sqlite3_close` → SQLITE_BUSY). `RunEvent::Exit` alone is not
/// reliable on Windows (observed: window close exits the process without it),
/// so this is invoked from the main window's CloseRequested handler, from
/// `RunEvent::ExitRequested` and from `RunEvent::Exit` — the `done` flag makes
/// it idempotent.
fn run_shutdown_cleanup(
    app: &tauri::AppHandle,
    done: &std::sync::atomic::AtomicBool,
) {
    tauri::async_runtime::block_on(run_shutdown_cleanup_async(app, done));
}

/// 同步弹出原生错误对话框,在进程因 panic 终止前阻塞提示用户。
///
/// 项目 release 构建使用 `panic = "abort"`(见 Cargo.toml),任何 panic 都会
/// 直接终止进程、不运行析构——之前崩溃表现为"无提示地自动关闭"。panic
/// hook 在 abort 前必定执行,因此在这里用不依赖事件循环的原生 API 弹窗,
/// 让用户知道发生了什么(日志中另有完整记录)。
fn show_panic_dialog(message: &str) {
    use std::process::Command;

    // 防重入:并发 panic 时只弹一次。
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = match LOCK.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };

    let title = "思库出现内部错误";
    let text = format!(
        "应用遇到内部错误,即将退出：\n{}\n\n详情见日志文件。",
        message
    );

    #[cfg(target_os = "windows")]
    {
        // user32 MessageBoxW:同步阻塞、任意线程可用,不依赖事件循环。
        #[link(name = "user32")]
        extern "system" {
            fn MessageBoxW(
                hwnd: *mut std::ffi::c_void,
                text: *const u16,
                caption: *const u16,
                kind: u32,
            ) -> i32;
        }
        const MB_OK: u32 = 0x0000;
        const MB_ICONERROR: u32 = 0x0010;
        const MB_SETFOREGROUND: u32 = 0x0001_0000;
        let wide_text: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let wide_caption: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
        // Own the dialog by the splashscreen (captured at setup) so it renders
        // ABOVE the always-on-top splash; 0 = owner-less fallback.
        let owner = PANIC_DIALOG_OWNER.load(std::sync::atomic::Ordering::Relaxed) as *mut std::ffi::c_void;
        unsafe {
            MessageBoxW(
                owner,
                wide_text.as_ptr(),
                wide_caption.as_ptr(),
                MB_OK | MB_ICONERROR | MB_SETFOREGROUND,
            );
        }
    }

    #[cfg(target_os = "linux")]
    {
        // zenity 同步阻塞弹窗; 环境里没有时退回 notify-send(非阻塞通知)。
        if Command::new("zenity")
            .args(["--error", "--title", title, "--text", &text])
            .status()
            .is_err()
        {
            let _ = Command::new("notify-send")
                .args(["--urgency=critical", title, &text])
                .status();
        }
    }

    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "display alert \"{}\" message \"{}\" as critical",
            title,
            text.replace('"', "\\\"")
        );
        let _ = Command::new("osascript").args(["-e", &script]).status();
    }
}

pub fn run() {
    // Install a custom panic hook so that unclean database shutdowns (e.g.
    // CR-SQLite unfinalized statements) do not pop up multiple OS error
    // dialogs and obscure the main window. We still log the panic (with a
    // backtrace) and — because release builds use panic = "abort" and would
    // otherwise vanish silently — show a native error dialog before the
    // process terminates.
    std::panic::set_hook(Box::new(|info| {
        let msg = if let Some(s) = info.payload().downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "unknown panic".to_string()
        };
        let location = info.location().map(|l| format!("{}:{}", l.file(), l.line()));
        let backtrace = std::backtrace::Backtrace::force_capture();
        tracing::error!(
            panic_message = %msg,
            panic_location = ?location,
            panic_backtrace = ?backtrace,
            "application panicked during shutdown"
        );
        #[cfg(not(test))]
        show_panic_dialog(&format!("{msg} ({location:?})"));
    }));

    // Idempotency guard so the cleanup runs exactly once no matter which of
    // the exit paths (main-window CloseRequested / ExitRequested / Exit)
    // fires first.
    let cleanup_done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let setup_cleanup_done = cleanup_done.clone();

    tauri::Builder::default()
        .manage(Mutex::new(SetupState {
            frontend_done: false,
            backend_done: false,
        }))
        .manage(commands::sync::SyncState::default())
        .setup(move |app| {
            // Make the main window background fully transparent so the CSS
            // rounded corners show the desktop instead of black cutouts.
            if let Some(main) = app.get_webview_window("main") {
                let _ = main.set_background_color(Some(tauri::window::Color(0, 0, 0, 0)));

                // Make sure a startup failure (e.g. missing crsqlite.dll) shows
                // its native error dialog ABOVE the always-on-top splash —
                // otherwise the error sits behind it, visible only in the
                // taskbar, and the user stares at a frozen splash.
                if let Some(splash) = app.get_webview_window("splashscreen") {
                    capture_panic_dialog_owner(&splash);
                }

                // Main window close = application exit (the pet window is
                // closed from the global on_window_event handler below).
                // RunEvent::Exit is not reliably emitted on Windows window
                // close, so start the shutdown cleanup right here — the
                // runtime and the pool are still alive, which is exactly the
                // precondition finalize_db needs.
                let cleanup_app = app.handle().clone();
                let cleanup_done = setup_cleanup_done.clone();
                let main_for_event = main.clone();
                main.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        // Zero-perceived-latency shutdown: prevent the real
                        // close, hide the window immediately (the user sees an
                        // instant close), then finalize the database in the
                        // background and exit the process.
                        api.prevent_close();
                        info!("main window close requested — hiding and finalizing in background");
                        let window = main_for_event.clone();
                        let app = cleanup_app.clone();
                        let done = cleanup_done.clone();
                        let _ = window.hide();
                        let _ = window.emit("app:shutdown-started", ());
                        tauri::async_runtime::spawn(async move {
                            run_shutdown_cleanup_async(&app, &done).await;
                            // window.close() would re-trigger CloseRequested;
                            // exit the process directly instead.
                            app.exit(0);
                        });
                    }
                });
            }

            let handle = app.handle().clone();
            // Sync engine tasks emit UI events (e.g. "remote changes applied")
            // through this handle.
            sync::register_app_handle(handle.clone());

            // Phase 1: fast synchronous init happens in Phase 2 after DB + settings.

            // Phase 2: spawn async backend init (database + settings + logger + state).
            // This runs concurrently with the webview loading, so the
            // splash screen (a separate lightweight window) is visible
            // the entire time.
            let handle2 = handle.clone();
            tauri::async_runtime::spawn(async move {
                let _ = handle2.emit("splash:phase", "正在初始化数据库...");

                let db = core::db::init(&handle2)
                    .await
                    .expect("failed to initialize database");

                // Load settings into the global cache before logger init.
                let _ = core::settings_service::refresh_cache(&db).await;
                // One-time migration: move legacy account credentials out of
                // the syncable settings table into device-local settings.
                let _ = core::settings_service::migrate_legacy_account_settings(&db).await;
                let app_settings = core::settings_service::cached_settings();

                let _ = handle2.emit("splash:phase", "正在初始化日志系统...");
                core::logger::init(&handle2, &app_settings).expect("failed to initialize logger");
                info!("Siku application starting");

                // Create the pet window only when the user has it enabled.
                // Runs after the logger is up so any creation failure (e.g.
                // WebView2 not fully initialized yet) is written to the log
                // instead of being swallowed silently.
                if app_settings.show_pet {
                    if let Err(e) = create_pet_window(&handle2) {
                        tracing::error!("failed to create pet window: {e}");
                    }
                }

                let app_data_dir = handle2
                    .path()
                    .app_data_dir()
                    .expect("failed to get app data dir");

                let _ = handle2.emit("splash:phase", "即将就绪...");

                // Create a shutdown channel so background loops can be stopped
                // gracefully before the database is finalized on exit.
                let (shutdown_tx, _) = tokio::sync::broadcast::channel::<()>(1);

                handle2.manage(AppState {
                    db,
                    app_data_dir,
                    approval_senders: std::sync::Arc::new(tokio::sync::Mutex::new(
                        std::collections::HashMap::new(),
                    )),
                    cancel_tokens: std::sync::Arc::new(tokio::sync::Mutex::new(
                        std::collections::HashMap::new(),
                    )),
                    tasks: crate::core::tasks::new_task_store(),
                    ask_senders: std::sync::Arc::new(tokio::sync::Mutex::new(
                        std::collections::HashMap::new(),
                    )),
                    shutdown_tx: shutdown_tx.clone(),
                    background_tasks: std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new())),
                });

                // Start the cron scheduler (fires scheduled agent prompts).
                let sched_handle = handle2.clone();
                let sched_db = handle2.state::<AppState>().db.clone();
                let sched_shutdown = shutdown_tx.subscribe();
                tokio::spawn(async move {
                    crate::core::cron_scheduler::run(sched_db, sched_handle, sched_shutdown).await;
                });

                // Background research auto-discovery (active topics).
                let auto_db = handle2.state::<AppState>().db.clone();
                let auto_shutdown = shutdown_tx.subscribe();
                tokio::spawn(async move {
                    crate::core::research_service::run_auto_discovery(auto_db, auto_shutdown).await;
                });

                // Restore account auto-sync after a restart: `auth_login`
                // starts the proxy, and a relaunch with stored credentials
                // must do the same — otherwise the device never reconnects to
                // the relay (and shows offline to peers) until the user
                // re-enters the sync settings page. Frontend pages never
                // (re)start it, so visiting the settings page cannot churn the
                // connection.
                {
                    let db = handle2.state::<AppState>().db.clone();
                    let relay_url = core::settings_service::get_device_setting(
                        &db,
                        crate::commands::account::ACCOUNT_RELAY_URL_KEY,
                    )
                    .await
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                    let token = core::settings_service::get_device_setting(
                        &db,
                        crate::commands::account::ACCOUNT_TOKEN_KEY,
                    )
                    .await
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                    if !relay_url.is_empty() && !token.is_empty() {
                        let token_to_use = if crate::commands::account::access_token_is_fresh(&db).await {
                            token
                        } else {
                            match crate::commands::account::refresh_access_token(&db, &relay_url).await {
                                Ok(new_token) => {
                                    tracing::info!("refreshed access token at startup");
                                    new_token
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, "failed to refresh access token at startup; clearing stored credentials");
                                    let _ = crate::commands::account::clear_account_credentials(&db).await;
                                    String::new()
                                }
                            }
                        };
                        if !token_to_use.is_empty() {
                            crate::commands::sync::spawn_auto_sync_proxy(
                                &handle2.state::<crate::commands::sync::SyncState>(),
                                &handle2.state::<AppState>(),
                                &relay_url,
                                &token_to_use,
                            )
                            .await;
                            tracing::info!("restored account auto-sync proxy at startup");
                        }
                    }
                }

                // Mark backend done and check if we can finish
                if let Some(state) = handle2.try_state::<Mutex<SetupState>>() {
                    let mut s = state.lock().unwrap();
                    s.backend_done = true;
                }
                try_finish_startup(&handle2);
            });

            Ok(())
        })
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_http::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        // Updater: checks GitHub Releases for a newer signed build.
        // Process: relaunch after the updater installs a new version.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        // Only one instance may run: the main window hides during background
        // shutdown, and a fast restart must not open a second process against
        // the same database. The callback restores the existing window.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.show();
                let _ = win.set_focus();
            }
        }))
        .on_window_event(|window, event| {
            // Only the main window's close (application exit) should tear
            // down the pet window. This handler previously matched ANY
            // window's CloseRequested — including the splashscreen, whose
            // close at startup (try_finish_startup → splash.close()) fired
            // CloseRequested and destroyed the just-created pet window, so
            // the pet never showed until it was toggled off/on in settings.
            if matches!(event, tauri::WindowEvent::CloseRequested { .. })
                && window.label() == "main"
            {
                if let Some(pet) = window.get_webview_window("pet") {
                    let _ = pet.close();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::greet,
            // Library
            commands::library::import_paper,
            commands::library::list_papers,
            commands::library::get_paper,
            commands::library::open_paper_in_system,
            commands::library::reveal_paper_in_system,
            commands::library::paper_find_duplicates,
            commands::library::paper_merge,
            commands::library::paper_restore,
            commands::library::paper_purge,
            commands::library::paper_export,
            commands::library::paper_set_favorite,
            commands::library::paper_set_read_status,
            commands::library::paper_record_read,
            commands::library::paper_add_related,
            commands::library::paper_remove_related,
            commands::library::paper_list_related,
            commands::library::saved_searches_list,
            commands::library::saved_searches_create,
            commands::library::saved_searches_delete,
            commands::library::paper_get_creators,
            commands::library::paper_set_creators,
            commands::library::paper_list_attachments,
            commands::library::paper_add_attachment,
            commands::library::paper_remove_attachment,
            commands::library::paper_open_attachment,
            commands::library::paper_export_annotations,
            commands::library::update_paper,
            commands::library::delete_paper,
            commands::library::paper_import_bibtex,
            commands::library::preview_paper_from_link,
            commands::library::import_paper_from_link,
            commands::library::paper_reprocess_index,
            commands::library::paper_enrich_metadata,
            // Collections
            commands::collections::collections_list,
            commands::collections::collections_get,
            commands::collections::collections_create,
            commands::collections::collections_update,
            commands::collections::collections_delete,
            commands::collections::collections_add_papers,
            commands::collections::collections_remove_papers,
            commands::collections::paper_get_collections,
            // Tags
            commands::tags::tags_list,
            commands::tags::tags_get,
            commands::tags::tags_create,
            commands::tags::tags_delete,
            commands::tags::tags_update,
            commands::tags::tags_papers,
            commands::tags::tags_add_to_paper,
            commands::tags::tags_remove_from_paper,
            commands::tags::tags_list_papers,
            // Bookmarks
            commands::bookmarks::bookmarks_list,
            commands::bookmarks::bookmarks_create,
            commands::bookmarks::bookmarks_delete,
            // Reader
            commands::reader::read_pdf_bytes,
            commands::reader::export_pdf,
            // Agent
            commands::agent::agent_create_session,
            commands::agent::agent_update_session,
            commands::agent::agent_get_session,
            commands::agent::agent_send_message,
            commands::agent::agent_list_sessions,
            commands::agent::agent_delete_session,
            commands::agent::agent_approve_tool,
            commands::agent::agent_pin_session,
            commands::agent::agent_cancel,
            commands::agent::agent_rename_session,
            commands::agent::agent_answer_user,
            commands::agent::pet_create_session,
            commands::agent::pet_domains,
            commands::agent::get_agent_steps,
            // Settings
            commands::agent::settings_app_get,
            commands::agent::settings_app_save,
            commands::agent::settings_get,
            commands::agent::settings_set,
            commands::agent::settings_get_all,
            commands::agent::settings_get_data_dir,
            commands::agent::settings_set_data_dir,
            commands::agent::settings_get_memory_dir,
            commands::agent::settings_ensure_directories,
            commands::agent::settings_validate_llm,
            // Chat
            commands::chat::list_chat_sessions,
            commands::chat::create_chat_session,
            commands::chat::delete_chat_session,
            commands::chat::get_chat_messages,
            commands::chat::agent_memory_get,
            commands::chat::agent_memory_set,
            commands::chat::agent_memory_set_active,
            commands::chat::agent_memory_restore,
            // Projects
            commands::projects::projects_list,
            commands::projects::project_create,
            commands::projects::project_update,
            commands::projects::project_delete,
            // Cron (scheduled agent prompts)
            commands::cron::cron_create,
            commands::cron::cron_list,
            commands::cron::cron_delete,
            // Translation
            commands::translation::translate_text,
            commands::translation::translate_text_stream,
            commands::translation::translation_clear_cache,
            // Region detection
            commands::region::detect_regions_llm,
            // LLM Providers
            commands::llm_provider::llm_provider_list,
            commands::llm_provider::llm_provider_get,
            commands::llm_provider::llm_provider_create,
            commands::llm_provider::llm_provider_update,
            commands::llm_provider::llm_provider_delete,
            commands::llm_provider::llm_provider_set_default,
            commands::llm_provider::llm_provider_validate,
            // Knowledge
            commands::knowledge::knowledge_list_domains,
            commands::knowledge::knowledge_create_domain,
            commands::knowledge::knowledge_update_domain,
            commands::knowledge::knowledge_delete_domain,
            commands::knowledge::knowledge_create_item,
            commands::knowledge::knowledge_update_item,
            commands::knowledge::knowledge_list_items,
            commands::knowledge::knowledge_get_item,
            commands::knowledge::knowledge_delete_item,
            // Research
            commands::research::research_create_topic,
            commands::research::research_list_topics,
            commands::research::research_update_topic,
            commands::research::research_discover_sources,
            commands::research::research_list_sources,
            commands::research::research_import_source,
            commands::research::research_delete_topic,
            commands::research::research_update_source_status,
            // Notes
            commands::notes::notes_create,
            commands::notes::notes_get,
            commands::notes::notes_update,
            commands::notes::notes_delete,
            commands::notes::notes_list,
            commands::notes::notes_list_all,
            commands::notes::notes_move,
            commands::notes::notes_get_backlinks,
            commands::notes::notes_search,
            commands::notes::note_versions_list,
            commands::notes::note_version_restore,
            commands::notes::note_create_under_paper,
            commands::notes::note_add_excerpt,
            commands::notes::note_merge_into_excerpt,
            // Vault files
            commands::files::files_list,
            commands::files::files_import,
            commands::files::files_move,
            commands::files::files_rename,
            commands::files::files_delete,
            commands::files::files_open,
            commands::files::files_get,
            commands::files::files_resolve_path,
            commands::files::files_read_text,
            // Vaults
            commands::vault::vault_list,
            commands::vault::vault_current,
            commands::vault::vault_create,
            commands::vault::vault_rename,
            commands::vault::vault_delete,
            commands::vault::vault_set_current,
            commands::vault::vault_export,
            commands::vault::vault_import,
            // File Browser
            commands::file_browser::file_browser_list_dir,
            commands::file_browser::file_browser_get_info,
            commands::file_browser::file_browser_open_in_system,
            commands::file_browser::file_browser_reveal_in_system,
            commands::file_browser::read_text_file,
            commands::file_browser::save_text_file,
            // Image cache
            commands::image_cache::cache_remote_image,
            commands::image_cache::resolve_cached_image_path,
            // Attachments
            commands::attachments::save_clipboard_image,
            commands::attachments::save_attachment_bytes,
            commands::attachments::vault_attachments_dir,
            commands::attachments::read_image_file,
            // System
            commands::system::system_info,
            commands::system::log_startup_metrics,
            // Annotations
            commands::annotation::annotation_list,
            commands::annotation::annotation_create,
            commands::annotation::annotation_update_note,
            commands::annotation::annotation_update_tags,
            commands::annotation::annotation_update_translation,
            commands::annotation::annotation_delete,
            commands::annotation::annotation_clear_paper,
            // Graph
            commands::graph::graph_get,
            commands::graph::graph_get_local,
            // Search
            commands::search::search_hybrid,
            commands::search::search_generate_embeddings,
            commands::search::search_rag_query,
            // Timeline
            commands::timeline::timeline_list,
            // Sync PoC
            // Account
            commands::account::auth_register,
            commands::account::auth_login,
            commands::account::auth_logout,
            commands::account::auth_status,
            commands::account::device_list,
            commands::account::device_remove,
            commands::account::device_rename,
            commands::account::suggest_device_name,
            // Sync
            commands::sync::start_auto_sync,
            commands::sync::get_local_ip,
            commands::sync::start_local_host,
            commands::sync::stop_local_host,
            commands::sync::get_lan_host_active,
            commands::sync::connect_local_host,
            commands::sync::sync_once,
            commands::sync::flush_sync_outbox,
            commands::sync::get_sync_outbox_count,
            commands::sync::get_sync_config,
            commands::sync::set_sync_config,
            commands::sync::export_encrypted_seed,
            commands::sync::import_encrypted_seed,
            commands::sync::get_device_id,
            commands::sync::start_lan_beacon,
            commands::sync::stop_lan_beacon,
            commands::sync::start_lan_discovery,
            commands::sync::stop_lan_discovery,
            commands::sync::get_lan_peers,
            commands::sync::get_sync_status,
            commands::sync::stop_local_session,
            commands::sync::stop_cloud_session,
            // Startup
            set_complete,
        ])
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(move |app_handle, event| {
            match event {
                tauri::RunEvent::ExitRequested { .. } => {
                    // Fires when the last window has requested close and the
                    // app is about to exit. Runs the cleanup in case the main
                    // window's CloseRequested handler did not (e.g. exit via
                    // tray or app.exit()); idempotent via cleanup_done.
                    info!("RunEvent::ExitRequested — running shutdown cleanup");
                    run_shutdown_cleanup(&app_handle, &cleanup_done);
                }
                tauri::RunEvent::Exit => {
                    // Last-chance hook (may or may not fire on Windows).
                    info!("RunEvent::Exit — running shutdown cleanup");
                    run_shutdown_cleanup(&app_handle, &cleanup_done);
                }
                _ => {}
            }
        });
}
