use anyhow::Context;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Connection, Pool, Row, Sqlite};
use std::path::{Path, PathBuf};
use tauri::Manager;
use tracing::{info, instrument};

use crate::core::models::DeviceAppSettings;

pub(crate) const SCHEMA_INIT_SQL: &str = include_str!("../../schema_init.sql");

pub(crate) const CORE_SYNC_TABLES: &[&str] = &[
    "notes",
    "papers",
    "attachments",
    "annotations",
    // Note organization
    "vaults",
    "note_versions",
    "note_links",
    "files",
    // Library organization
    "tags",
    "paper_tags",
    "collections",
    "paper_collections",
    "related_papers",
    "bookmarks",
    "file_bookmarks",
    "saved_searches",
    "saved_items",
    "imports",
    // Per-agent long-term memory (1:1 with chat_sessions, which is optional:
    // memory rows may arrive before/regardless of their session — harmless,
    // they apply once the session exists).
    "agent_memories",
    // NOTE: `creators` is intentionally NOT synced — it is a device-local
    // overlay rebuilt from the papers.authors/editor JSON columns (which do
    // sync via the papers table). See paper_service::rebuild_creators_for_papers.
];
pub(crate) const OPTIONAL_SYNC_TABLES: &[&str] = &["chat_sessions", "chat_messages", "settings"];

/// Fixed uuid of the default vault: every device seeds the same id so the
/// default vault converges to a single row when syncing.
pub(crate) const DEFAULT_VAULT_ID: &str = "00000000-0000-0000-0000-000000000001";

pub type Db = Pool<Sqlite>;

/// Platform-specific CR-SQLite extension filename.
#[cfg(target_os = "windows")]
const CRSQLITE_FILENAME: &str = "crsqlite.dll";
#[cfg(target_os = "macos")]
const CRSQLITE_FILENAME: &str = "crsqlite.dylib";
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const CRSQLITE_FILENAME: &str = "crsqlite.so";

/// Path to the CR-SQLite extension shared library.
///
/// `resource_dir` should be supplied in production so that bundled resources
/// (e.g. `ext/crsqlite.dll` placed via `bundle.resources`) are found even when
/// the installer puts them in a separate `resources` directory.
pub(crate) fn crsqlite_extension_path(
    resource_dir: Option<PathBuf>,
) -> anyhow::Result<PathBuf> {
    // In development the extension lives next to Cargo.toml.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = manifest_dir.join("ext").join(CRSQLITE_FILENAME);
    if path.exists() {
        return Ok(path);
    }

    // Fallback 1: same directory as the running executable (useful for portable distribution).
    let mut exe_dir = std::env::current_exe()?;
    exe_dir.pop();
    let fallback = exe_dir.join(CRSQLITE_FILENAME);
    if fallback.exists() {
        return Ok(fallback);
    }

    // Fallback 2: bundled Tauri resources directory.
    if let Some(resource_dir) = resource_dir {
        let resource = resource_dir.join("ext").join(CRSQLITE_FILENAME);
        if resource.exists() {
            return Ok(resource);
        }
    }

    anyhow::bail!(
        "{} not found at {:?}, {:?}, or bundled resources/ext/",
        CRSQLITE_FILENAME,
        path,
        fallback
    )
}

/// Check whether a column exists on a table using SQLite PRAGMA.
/// Table name is interpolated because SQLite PRAGMA does not accept parameters.
async fn column_exists(db: &Db, table: &str, column: &str) -> anyhow::Result<bool> {
    let sql = format!("PRAGMA table_info({})", table);
    let rows = sqlx::query_as::<_, (i32, String, String, i32, Option<String>, i32)>(&sql)
        .fetch_all(db)
        .await?;
    Ok(rows.into_iter().any(|r| r.1 == column))
}

/// Add a column only if it does not already exist.
async fn add_column_if_missing(
    db: &Db,
    table: &str,
    column: &str,
    def: &str,
) -> anyhow::Result<()> {
    if !column_exists(db, table, column).await? {
        let sql = format!("ALTER TABLE {} ADD COLUMN {} {}", table, column, def);
        info!("running migration: {}", sql);
        sqlx::query(&sql).execute(db).await?;
    } else {
        info!("column {}.{} already exists, skipping migration", table, column);
    }
    Ok(())
}

/// Read the device_id stored in a SQLite database file without loading the
/// CR-SQLite extension. Returns None if the file does not exist or the setting
/// is missing/empty.
async fn read_device_id_without_extension(db_path: &Path) -> Option<String> {
    if !db_path.exists() {
        return None;
    }
    let options = SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(false)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
    let db = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .ok()?;
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT value FROM device_settings WHERE key = 'device_settings'",
    )
    .fetch_optional(&db)
    .await
    .ok()?;
    db.close().await;
    let value = row?.0;
    let settings: DeviceAppSettings = serde_json::from_str(&value).ok()?;
    if settings.device_id.is_empty() {
        None
    } else {
        Some(settings.device_id)
    }
}

/// Outcome of applying a pending seed import at startup.
enum SeedImportOutcome {
    /// No pending import was found — normal startup.
    None,
    /// A pending import was applied. The previous device_id is `Some` when an
    /// old database existed on this machine (same-machine restore); `None`
    /// means this machine had no database (fresh install / new device).
    Applied(Option<String>),
}

/// If a seed import was marked pending, move it into place before the database
/// is opened. The current database and blobs are backed up. Returns whether an
/// import was applied and the previous device_id so the caller can decide how
/// to handle device identity (preserve it on the same machine, or mint a fresh
/// one on a brand-new device).
async fn apply_pending_seed_import(app_data_dir: &Path) -> anyhow::Result<SeedImportOutcome> {
    let seed_imports_dir = app_data_dir.join("seed-imports");
    let marker = seed_imports_dir.join(".pending-import");
    if !marker.exists() {
        return Ok(SeedImportOutcome::None);
    }

    let pending_str = tokio::fs::read_to_string(&marker).await?;
    let pending_dir = PathBuf::from(pending_str.trim());
    info!(
        pending_dir = %pending_dir.display(),
        "found pending seed import"
    );

    // Read the current device_id before we replace the database.
    let old_device_id = read_device_id_without_extension(&app_data_dir.join("siku.db")).await;

    let timestamp = crate::core::time::now_iso().replace([':', '.'], "-");
    let backup_dir = app_data_dir.join(format!("pre-seed-backup-{timestamp}"));

    let current_db = app_data_dir.join("siku.db");
    let current_db_wal = app_data_dir.join("siku.db-wal");
    let current_db_shm = app_data_dir.join("siku.db-shm");
    let current_blobs = app_data_dir.join("blobs");
    let pending_db = pending_dir.join("siku.db");
    let pending_blobs = pending_dir.join("blobs");

    if !pending_db.exists() {
        anyhow::bail!(
            "pending seed import does not contain siku.db: {}",
            pending_db.display()
        );
    }
    let db_size = tokio::fs::metadata(&pending_db)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    if db_size == 0 {
        anyhow::bail!("pending seed import siku.db is empty (0 bytes)");
    }
    info!(db_size, "pending seed import siku.db size");

    std::fs::create_dir_all(&backup_dir)?;

    // Backup current files if they exist. The -wal/-shm sidecars must move
    // with the database: leaving the old WAL behind would let SQLite try to
    // recover it against the freshly imported database.
    if current_db.exists() {
        std::fs::rename(&current_db, backup_dir.join("siku.db"))
            .with_context(|| format!("backup current db to {}", backup_dir.display()))?;
        if current_db_wal.exists() {
            std::fs::rename(&current_db_wal, backup_dir.join("siku.db-wal"))
                .with_context(|| format!("backup current db-wal to {}", backup_dir.display()))?;
        }
        if current_db_shm.exists() {
            std::fs::rename(&current_db_shm, backup_dir.join("siku.db-shm"))
                .with_context(|| format!("backup current db-shm to {}", backup_dir.display()))?;
        }
    }
    if current_blobs.exists() {
        std::fs::rename(&current_blobs, backup_dir.join("blobs"))
            .with_context(|| format!("backup current blobs to {}", backup_dir.display()))?;
    }

    // Move imported files into place.
    std::fs::rename(&pending_db, &current_db)
        .with_context(|| format!("move imported db to {}", current_db.display()))?;
    if pending_blobs.exists() {
        std::fs::rename(&pending_blobs, &current_blobs)
            .with_context(|| format!("move imported blobs to {}", current_blobs.display()))?;
    }

    // Clean up marker and pending dir.
    let _ = tokio::fs::remove_file(&marker).await;
    let _ = tokio::fs::remove_dir_all(&pending_dir).await;

    info!(backup_dir = %backup_dir.display(), "applied pending seed import");

    Ok(SeedImportOutcome::Applied(old_device_id))
}

#[instrument]
pub async fn init(app_handle: &tauri::AppHandle) -> anyhow::Result<Db> {
    let app_data_dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| anyhow::anyhow!("failed to get app data dir: {}", e))?;

    std::fs::create_dir_all(&app_data_dir)?;

    // If the user previously imported an offline seed, move it into place
    // before opening the database. The old database/blobs are backed up.
    let seed_import_outcome = apply_pending_seed_import(&app_data_dir).await?;

    let db_path = app_data_dir.join("siku.db");
    info!("database path: {:?}", db_path);

    let options = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .synchronous(sqlx::sqlite::SqliteSynchronous::Normal);

    let resource_dir = app_handle.path().resource_dir().ok();
    let ext_path = crsqlite_extension_path(resource_dir)?;
    let ext_escaped = ext_path.to_string_lossy().replace('\'', "''");
    let load_sql = format!(
        "SELECT load_extension('{}', 'sqlite3_crsqlite_init')",
        ext_escaped
    );
    info!("crsqlite extension path: {:?}", ext_path);

    // CRITICAL: keep connections alive for the whole process lifetime.
    // sqlx's default max_lifetime is 30 minutes (and idle_timeout 10 min):
    // when the pool's reaper closes an expired connection, CR-SQLite's
    // prepared statements on it are never finalized, sqlite3_close() panics
    // inside sqlx (SQLITE_BUSY "unable to close due to unfinalized
    // statements"), and the tokio worker crash takes the whole app down —
    // exactly "app crashes ~30 min after launch". Connections are only closed
    // by the shutdown cleanup, which runs crsql_finalize() on each one first.
    let db = SqlitePoolOptions::new()
        .max_connections(5)
        .max_lifetime(None)
        .idle_timeout(None)
        .after_connect(move |conn, _meta| {
            let load = load_sql.clone();
            Box::pin(async move {
                let mut handle = conn.lock_handle().await?;
                let raw = handle.as_raw_handle().as_ptr();
                let rc = unsafe { libsqlite3_sys::sqlite3_enable_load_extension(raw, 1) };
                if rc != libsqlite3_sys::SQLITE_OK {
                    return Err(sqlx::Error::Io(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("sqlite3_enable_load_extension failed: {}", rc),
                    )));
                }
                drop(handle);
                sqlx::query(&load).execute(&mut *conn).await?;
                Ok(())
            })
        })
        .connect_with(options)
        .await?;

    // Initialize schema if tables don't exist (data persists across restarts)
    info!("initializing schema from schema_init.sql");
    sqlx::query(SCHEMA_INIT_SQL).execute(&db).await?;

    // Migration: sync_outbox gains the relay message_id so outbox retries
    // re-deposit with the same id (relay-side idempotent dedupe) (added 2026-08-31)
    add_column_if_missing(&db, "sync_outbox", "message_id", "TEXT")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for sync_outbox.message_id: {}", e))?;

    // Migration: add tags/translation columns to annotations (added 2026-06-19)
    add_column_if_missing(&db, "annotations", "tags", "TEXT DEFAULT '[]'")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for annotations.tags: {}", e))?;
    add_column_if_missing(&db, "annotations", "translation", "TEXT")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for annotations.translation: {}", e))?;

    // Migration: soft-delete (trash) support for papers (added 2026-08-16)
    add_column_if_missing(&db, "papers", "deleted_at", "TEXT")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for papers.deleted_at: {}", e))?;
    // Migration: read status + favorite for papers (added 2026-08-16)
    add_column_if_missing(&db, "papers", "is_favorite", "INTEGER NOT NULL DEFAULT 0")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for papers.is_favorite: {}", e))?;
    add_column_if_missing(&db, "papers", "read_status", "TEXT NOT NULL DEFAULT 'unread'")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for papers.read_status: {}", e))?;
    // Migration: track the last time a paper was opened in the reader (added 2026-08-17)
    add_column_if_missing(&db, "papers", "last_read_at", "TEXT")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for papers.last_read_at: {}", e))?;

    // Migration: backfill structured creators from the legacy authors/editor
    // JSON columns (added 2026-08-16). Runs only when the table is empty.
    {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM creators")
            .fetch_one(&db)
            .await?;
        if count == 0 {
            crate::core::paper_service::backfill_creators(&db)
                .await
                .map_err(|e| anyhow::anyhow!("failed to backfill creators: {e}"))?;
        }
    }

    // Migration: add reasoning_content column to chat_messages (added 2026-08-07)
    add_column_if_missing(&db, "chat_messages", "reasoning_content", "TEXT")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for chat_messages.reasoning_content: {}", e))?;

    // Migration: token usage breakdown for assistant messages
    add_column_if_missing(&db, "chat_messages", "tokens_in", "INTEGER")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for chat_messages.tokens_in: {}", e))?;
    add_column_if_missing(&db, "chat_messages", "tokens_in_hit", "INTEGER")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for chat_messages.tokens_in_hit: {}", e))?;
    add_column_if_missing(&db, "chat_messages", "tokens_out", "INTEGER")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for chat_messages.tokens_out: {}", e))?;

    // Migration: image attachments on chat messages (added 2026-08-25)
    add_column_if_missing(&db, "chat_messages", "attachments", "TEXT")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for chat_messages.attachments: {}", e))?;

    // Migration: sync chat_sessions columns added for agent features
    add_column_if_missing(&db, "chat_sessions", "llm_models", "TEXT")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for chat_sessions.llm_models: {}", e))?;
    add_column_if_missing(&db, "chat_sessions", "approval_config", "TEXT")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for chat_sessions.approval_config: {}", e))?;
    add_column_if_missing(&db, "chat_sessions", "max_loops", "INTEGER DEFAULT 10")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for chat_sessions.max_loops: {}", e))?;
    add_column_if_missing(&db, "chat_sessions", "max_tokens", "INTEGER DEFAULT 28000")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for chat_sessions.max_tokens: {}", e))?;
    // Migration: chat_sessions.max_tokens used to be the conversation context
    // budget; it now means the per-round OUTPUT cap sent to the LLM API
    // (NULL = follow the model config). Move legacy values to the new
    // context_budget column, then clear max_tokens. Runs only when the column
    // is first added — otherwise every restart would wipe user-set caps.
    let had_context_budget = column_exists(&db, "chat_sessions", "context_budget").await?;
    add_column_if_missing(&db, "chat_sessions", "context_budget", "INTEGER DEFAULT 28000")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for chat_sessions.context_budget: {}", e))?;
    if !had_context_budget {
        // The UPDATE below fires the CR-SQLite update trigger; if this DB was
        // registered as a CRR before some columns were added, the stale
        // trigger aborts the statement with "expected N values, got M".
        refresh_stale_crr_triggers(&db, "chat_sessions").await?;
        sqlx::query(
            "UPDATE chat_sessions SET context_budget = max_tokens, max_tokens = NULL \
             WHERE max_tokens IS NOT NULL"
        )
        .execute(&db)
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for chat_sessions max_tokens resemantic: {}", e))?;
    }
    add_column_if_missing(&db, "chat_sessions", "max_memory_rounds", "INTEGER DEFAULT 10")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for chat_sessions.max_memory_rounds: {}", e))?;
    add_column_if_missing(&db, "chat_sessions", "memory_file_path", "TEXT")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for chat_sessions.memory_file_path: {}", e))?;
    add_column_if_missing(&db, "chat_sessions", "is_pinned", "INTEGER DEFAULT 0")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for chat_sessions.is_pinned: {}", e))?;
    add_column_if_missing(&db, "chat_sessions", "sort_order", "INTEGER DEFAULT 0")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for chat_sessions.sort_order: {}", e))?;
    add_column_if_missing(&db, "chat_sessions", "icon", "TEXT")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for chat_sessions.icon: {}", e))?;
    add_column_if_missing(&db, "chat_sessions", "color", "TEXT")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for chat_sessions.color: {}", e))?;

    // Migration: domain (pet) sessions used to pin {"mode":"manual"} at
    // creation. Approval is now governed by the user's global
    // default_approval (pet panel shield), so clear that pin — NULL falls
    // back to the global default at runtime. Idempotent.
    refresh_stale_crr_triggers(&db, "chat_sessions").await?;
    sqlx::query(
        "UPDATE chat_sessions SET approval_config = NULL \
         WHERE domain IS NOT NULL AND approval_config = '{\"mode\":\"manual\"}'"
    )
    .execute(&db)
    .await
    .map_err(|e| anyhow::anyhow!("migration failed for chat_sessions.domain approval unpin: {}", e))?;

    // Migration: LLM provider pool (replaces scattered llm.* settings and app_settings.default_llm)
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS llm_providers (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            provider TEXT NOT NULL,
            model TEXT NOT NULL,
            api_key TEXT NOT NULL DEFAULT '',
            base_url TEXT NOT NULL,
            proxy TEXT,
            max_tokens INTEGER DEFAULT 4096,
            temperature REAL DEFAULT 0.7,
            extra_body TEXT,
            is_default INTEGER DEFAULT 0,
            sort_order INTEGER DEFAULT 0,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        )"
    )
    .execute(&db)
    .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_llm_providers_default ON llm_providers(is_default)")
        .execute(&db)
        .await?;
    add_column_if_missing(&db, "chat_sessions", "llm_provider_ids", "TEXT")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for chat_sessions.llm_provider_ids: {}", e))?;
    add_column_if_missing(&db, "chat_sessions", "memory_dir", "TEXT")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for chat_sessions.memory_dir: {}", e))?;
    add_column_if_missing(&db, "chat_sessions", "skills_dir", "TEXT")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for chat_sessions.skills_dir: {}", e))?;

    // Migration: agent_steps for ReAct iterations (added 2026-08-07)
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS agent_steps (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL REFERENCES chat_sessions(id) ON DELETE CASCADE,
            message_id TEXT REFERENCES chat_messages(id) ON DELETE CASCADE,
            step_index INTEGER NOT NULL,
            reasoning_content TEXT,
            tool_calls TEXT,
            created_at TEXT NOT NULL
        )"
    )
    .execute(&db)
    .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_agent_steps_session ON agent_steps(session_id)")
        .execute(&db)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_agent_steps_message ON agent_steps(message_id)")
        .execute(&db)
        .await?;
    add_column_if_missing(&db, "agent_steps", "message_id", "TEXT REFERENCES chat_messages(id) ON DELETE CASCADE")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for agent_steps.message_id: {}", e))?;

    // Migration: Codex-style projects (added 2026-08-09)
    add_column_if_missing(&db, "chat_sessions", "project_id", "TEXT")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for chat_sessions.project_id: {}", e))?;
    crate::core::project_service::ensure_default_project(&db, &app_data_dir)
        .await
        .map_err(|e| anyhow::anyhow!("failed to seed default project: {e}"))?;

    // Migration: per-session working directory (sandbox scope) + cron jobs (added 2026-08-09)
    add_column_if_missing(&db, "chat_sessions", "working_dir", "TEXT")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for chat_sessions.working_dir: {}", e))?;
    add_column_if_missing(&db, "chat_sessions", "vision_provider_id", "TEXT")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for chat_sessions.vision_provider_id: {}", e))?;
    add_column_if_missing(&db, "chat_sessions", "web_proxy", "TEXT")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for chat_sessions.web_proxy: {}", e))?;
    add_column_if_missing(&db, "llm_providers", "is_vision", "INTEGER DEFAULT 0")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for llm_providers.is_vision: {}", e))?;

    // Migration: cron_jobs 支持启用/禁用开关并记录最近触发时间 (added 2026-08-27)
    add_column_if_missing(&db, "cron_jobs", "enabled", "INTEGER DEFAULT 1")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for cron_jobs.enabled: {}", e))?;
    add_column_if_missing(&db, "cron_jobs", "last_fired", "TEXT")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for cron_jobs.last_fired: {}", e))?;
    add_column_if_missing(&db, "notes", "tags", "TEXT DEFAULT '[]'")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for notes.tags: {}", e))?;
    add_column_if_missing(&db, "notes", "aliases", "TEXT DEFAULT '[]'")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for notes.aliases: {}", e))?;
    add_column_if_missing(&db, "notes", "is_favorite", "INTEGER DEFAULT 0")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for notes.is_favorite: {}", e))?;
    add_column_if_missing(&db, "notes", "is_folder", "INTEGER DEFAULT 0")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for notes.is_folder: {}", e))?;

    // Migration: vaults (Obsidian-style note vaults, added 2026-08-09)
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS vaults (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        )"
    )
    .execute(&db)
    .await?;
    // (REFERENCES dropped here: SQLite's ADD COLUMN with a REFERENCES clause
    // requires a NULL default when foreign keys are enabled.)
    add_column_if_missing(&db, "notes", "vault_id", "INTEGER NOT NULL DEFAULT 1")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for notes.vault_id: {}", e))?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_notes_vault ON notes(vault_id)")
        .execute(&db)
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for idx_notes_vault: {e}"))?;
    crate::core::vault_service::ensure_defaults(&db)
        .await
        .map_err(|e| anyhow::anyhow!("failed to seed default vault: {e}"))?;
    add_column_if_missing(&db, "notes", "is_folder", "INTEGER DEFAULT 0")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for notes.is_folder: {}", e))?;
    // Mark existing notes that already have children as folders (legacy data).
    refresh_stale_crr_triggers(&db, "notes").await?;
    sqlx::query(
        "UPDATE notes SET is_folder = 1 WHERE id IN \
         (SELECT DISTINCT parent_id FROM notes WHERE parent_id IS NOT NULL)"
    )
    .execute(&db)
    .await
    .map_err(|e| anyhow::anyhow!("migration failed for legacy folder marking: {e}"))?;

    // Migration: note_versions (AI-edit snapshots) + agent edit markers (added 2026-08-10)
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS note_versions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            note_id TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
            title TEXT NOT NULL,
            content TEXT NOT NULL DEFAULT '',
            edited_by TEXT NOT NULL DEFAULT 'agent',
            created_at TEXT NOT NULL
        )"
    )
    .execute(&db)
    .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_note_versions_note ON note_versions(note_id)")
        .execute(&db)
        .await?;
    add_column_if_missing(&db, "notes", "agent_edited_at", "TEXT")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for notes.agent_edited_at: {}", e))?;
    add_column_if_missing(&db, "notes", "agent_edit_count", "INTEGER DEFAULT 0")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for notes.agent_edit_count: {}", e))?;
    // System folder marker + collection link for paper-mapped note trees.
    add_column_if_missing(&db, "notes", "is_system", "INTEGER DEFAULT 0")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for notes.is_system: {}", e))?;
    add_column_if_missing(&db, "notes", "source_collection_id", "TEXT")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for notes.source_collection_id: {}", e))?;
    add_column_if_missing(&db, "notes", "is_excerpt", "INTEGER DEFAULT 0")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for notes.is_excerpt: {}", e))?;

    // Migration: pet domain sessions (built-in agents per page, added 2026-08-10)
    add_column_if_missing(&db, "chat_sessions", "domain", "TEXT")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for chat_sessions.domain: {}", e))?;
    add_column_if_missing(&db, "chat_sessions", "context", "TEXT")
        .await
        .map_err(|e| anyhow::anyhow!("migration failed for chat_sessions.context: {}", e))?;

    // Migration: Zotero-style paper metadata (added 2026-08-10)
    for (col, def) in [
        ("item_type", "TEXT DEFAULT 'journal'"),
        ("volume", "TEXT"),
        ("issue", "TEXT"),
        ("pages", "TEXT"),
        ("conference_name", "TEXT"),
        ("publisher", "TEXT"),
        ("place", "TEXT"),
        ("editor", "TEXT NOT NULL DEFAULT '[]'"),
        ("series", "TEXT"),
        ("edition", "TEXT"),
        ("isbn", "TEXT"),
        ("issn", "TEXT"),
        ("num_pages", "INTEGER"),
        ("archive_location", "TEXT"),
        ("call_number", "TEXT"),
        ("rights", "TEXT"),
    ] {
        add_column_if_missing(&db, "papers", col, def)
            .await
            .map_err(|e| anyhow::anyhow!("migration failed for papers.{col}: {e}"))?;
    }

    // Migration: chunks_fts → trigram tokenizer (added 2026-08-10).
    // The default unicode61 tokenizer treats CJK text (no spaces) as single
    // tokens, making Chinese keyword search effectively broken. Trigram
    // tokenizes into overlapping 3-char n-grams, which handles CJK well.
    // FTS5 virtual tables cannot be altered, so drop + recreate + rebuild.
    let chunks_fts_sql: Option<(String,)> =
        sqlx::query_as("SELECT sql FROM sqlite_master WHERE type='table' AND name='chunks_fts'")
            .fetch_optional(&db)
            .await?;
    if let Some((sql,)) = chunks_fts_sql {
        if !sql.contains("trigram") {
            info!("migrating chunks_fts to trigram tokenizer");
            sqlx::query("DROP TABLE chunks_fts").execute(&db).await?;
            sqlx::query(
                "CREATE VIRTUAL TABLE chunks_fts USING fts5(
                    content,
                    tokenize='trigram',
                    content='chunks', content_rowid='rowid'
                )"
            )
            .execute(&db)
            .await?;
            sqlx::query("INSERT INTO chunks_fts(chunks_fts) VALUES('rebuild')")
                .execute(&db)
                .await
                .map_err(|e| anyhow::anyhow!("failed to rebuild chunks_fts: {e}"))?;
            // Recreate the sync triggers (they were dropped with the table).
            sqlx::query("CREATE TRIGGER IF NOT EXISTS chunks_fts_ai AFTER INSERT ON chunks BEGIN
                INSERT INTO chunks_fts(rowid, content) VALUES (new.rowid, new.content);
            END")
                .execute(&db)
                .await?;
            sqlx::query("CREATE TRIGGER IF NOT EXISTS chunks_fts_ad AFTER DELETE ON chunks BEGIN
                INSERT INTO chunks_fts(chunks_fts, rowid, content) VALUES('delete', old.rowid, old.content);
            END")
                .execute(&db)
                .await?;
            sqlx::query("CREATE TRIGGER IF NOT EXISTS chunks_fts_au AFTER UPDATE ON chunks BEGIN
                INSERT INTO chunks_fts(chunks_fts, rowid, content) VALUES('delete', old.rowid, old.content);
                INSERT INTO chunks_fts(rowid, content) VALUES (new.rowid, new.content);
            END")
                .execute(&db)
                .await?;
            info!("chunks_fts migrated to trigram");
        }
    }

    // One-time FTS rebuild: the external-content FTS indexes existed but were
    // never populated; rebuild them once from the source tables.
    let fts_rebuilt: Option<(String,)> =
        sqlx::query_as("SELECT value FROM settings WHERE key = 'fts.rebuilt'")
            .fetch_optional(&db)
            .await?;
    if fts_rebuilt.is_none() {
        for table in ["papers_fts", "notes_fts", "chunks_fts", "knowledge_items_fts"] {
            sqlx::query(&format!("INSERT INTO {table}({table}) VALUES('rebuild')"))
                .execute(&db)
                .await
                .map_err(|e| anyhow::anyhow!("failed to rebuild {table}: {e}"))?;
        }
        let now = crate::core::time::now_iso();
        sqlx::query("INSERT INTO settings (key, value, updated_at) VALUES ('fts.rebuilt', '1', ?)")
            .bind(&now)
            .execute(&db)
            .await?;
        info!("FTS indexes rebuilt");
    }

    // Migration: imports table now supports both local file imports and link imports.
    // SQLite cannot drop NOT NULL, so recreate the table if the new source_url column is missing.
    if !column_exists(&db, "imports", "source_url").await? {
        info!("migrating imports table to support source_url and nullable file_path");
        sqlx::query("ALTER TABLE imports RENAME TO imports_old")
            .execute(&db)
            .await
            .map_err(|e| anyhow::anyhow!("migration failed to rename imports: {e}"))?;
        sqlx::query(
            "CREATE TABLE imports (
                id TEXT PRIMARY KEY,
                file_path TEXT,
                source_url TEXT,
                paper_id TEXT,
                status TEXT NOT NULL,
                error TEXT,
                created_at TEXT NOT NULL,
                completed_at TEXT
            )"
        )
        .execute(&db)
        .await
        .map_err(|e| anyhow::anyhow!("migration failed to create imports: {e}"))?;
        sqlx::query(
            "INSERT INTO imports (id, file_path, source_url, paper_id, status, error, created_at, completed_at)
             SELECT id, file_path, NULL, paper_id, status, error, created_at, completed_at FROM imports_old"
        )
        .execute(&db)
        .await
        .map_err(|e| anyhow::anyhow!("migration failed to copy imports: {e}"))?;
        sqlx::query("DROP TABLE imports_old")
            .execute(&db)
            .await
            .map_err(|e| anyhow::anyhow!("migration failed to drop imports_old: {e}"))?;
    }

    // Migration: device-local settings table (added for multi-device sync phase 1).
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS device_settings (
            key TEXT PRIMARY KEY NOT NULL,
            value TEXT NOT NULL,
            updated_at TEXT NOT NULL DEFAULT ''
        )"
    )
    .execute(&db)
    .await?;

    // Migration: multi-device sync preparation (added 2026-08-19).
    // Rebuilds vaults / note_versions with TEXT uuid PKs and drops the
    // UNIQUE(name) constraint on tags. Must run BEFORE CRR registration so
    // the triggers are created against the final table schemas.
    migrate_sync_schema_v2(&db)
        .await
        .map_err(|e| anyhow::anyhow!("sync schema v2 migration failed: {e}"))?;

    // Register core sync tables as CRDTs for multi-device sync.
    // This must happen AFTER all schema migrations so the CRR triggers match
    // the final table schemas. If columns are added after registration, the
    // triggers become stale and UPDATEs fail with "expected N values, got M".
    register_crr_tables(&db, CORE_SYNC_TABLES).await?;

    // Provision (or converge) the system "我的图书馆" folder for the default
    // vault. Must run AFTER CRR registration so the folder row is tracked as a
    // sync delta from day one; converging legacy random-id folders here also
    // makes every device settle on one deterministic folder id, so literature
    // notes never dangle under a folder the peer does not have.
    {
        let vault_id = crate::core::vault_service::get_current_vault_id(&db)
            .await
            .unwrap_or_else(|_| DEFAULT_VAULT_ID.to_string());
        if let Err(e) =
            crate::core::note_service::ensure_system_library_folder(&db, &vault_id).await
        {
            tracing::warn!(error = %e, "failed to provision system library folder");
        }
    }

    // Optionally register chat sessions/messages and global settings.
    let mut device_settings = crate::core::settings_service::load_device_settings(&db)
        .await
        .unwrap_or_default();

    // Device identity after a seed import:
    // - Same-machine restore (an old database existed): keep the current
    //   machine's device_id so its sync identity is preserved.
    // - Fresh install / new device (no old database): the seed carries the
    //   source device's id — that must NOT be reused, or both machines would
    //   collide in sync. Always mint a brand-new id instead.
    // - Normal startup (no import): only generate when the setting is empty.
    match &seed_import_outcome {
        SeedImportOutcome::Applied(Some(old_id)) => {
            if device_settings.device_id != *old_id {
                info!(
                    old_id = %old_id,
                    imported_id = %device_settings.device_id,
                    "preserving current device_id after seed import"
                );
                device_settings.device_id = old_id.clone();
            }
        }
        SeedImportOutcome::Applied(None) => {
            info!(
                imported_id = %device_settings.device_id,
                "fresh machine after seed import — minting new device_id"
            );
            device_settings.device_id = uuid::Uuid::new_v4().to_string();
        }
        SeedImportOutcome::None => {
            if device_settings.device_id.is_empty() {
                device_settings.device_id = uuid::Uuid::new_v4().to_string();
                info!(device_id = %device_settings.device_id, "generated new device_id");
            }
        }
    }
    if device_settings.device_id != crate::core::settings_service::cached_device_settings().device_id {
        let _ = crate::core::settings_service::save_device_settings(&db, &device_settings).await;
    }

    if device_settings.sync_optional_data {
        register_crr_tables(&db, OPTIONAL_SYNC_TABLES).await?;
        info!("optional sync tables registered");
    } else {
        info!("optional sync disabled");
    }

    info!("database initialized successfully");
    Ok(db)
}

/// Finalize CR-SQLite state and close the pool gracefully.
///
/// CR-SQLite keeps prepared statements open while the extension is loaded.
/// SQLite refuses to close a connection that still has unfinalized statements
/// (`SQLITE_BUSY`, code 5). Calling `SELECT crsql_finalize()` before closing
/// releases those statements.
///
/// To make shutdown deterministic this runs in two phases:
///  1. Poll-acquire EVERY pooled connection up to a hard deadline. A
///     connection that is still checked out when the deadline hits is owned by
///     a task that ignored the shutdown signal — the exit path aborts tracked
///     background tasks before calling this, so the deadline is only a safety
///     net.
///  2. Only once every connection is in hand (and finalized) is the pool
///     closed. Closing the pool while a connection is still checked out would
///     drop that connection without `crsql_finalize()` and panic inside sqlx
///     (`sqlite3_close` → SQLITE_BUSY "unable to close due to unfinalized
///     statements or unfinished backups").
pub async fn finalize_db(pool: &Db) -> anyhow::Result<()> {
    info!(
        pool_size = pool.size(),
        idle_connections = pool.num_idle(),
        checked_out = (pool.size() as usize).saturating_sub(pool.num_idle()),
        "finalizing CR-SQLite before shutdown"
    );

    // Poll-acquire every connection. A checked-out connection is returned by
    // its owner once the owner has observed the shutdown signal; keep retrying
    // so late returners still get finalized instead of being dropped raw by
    // pool.close().
    //
    // CRITICAL: never probe with `pool.acquire()` just to see if a new
    // connection appears — sqlx creates a NEW connection when no idle one is
    // available and `size < max_connections`. The old code did exactly this in
    // the "confirm" phase, which grew the pool from 4 to 5 on every shutdown
    // and then sat through two 2s timeouts (~4s of delay, and `held=5` in the
    // logs every single time). Instead, only call acquire() when
    // `pool.size()` is greater than the number of connections we already hold;
    // otherwise sleep briefly and re-check the pool size.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut conns = Vec::new();
    // Number of consecutive quiet confirmations while every existing pool
    // connection is already held; two in a row means the pool is quiescent.
    let mut confirm_waits = 0u32;
    let mut waits = 0u32;
    loop {
        if tokio::time::Instant::now() >= deadline {
            info!(
                held = conns.len(),
                pool_size = pool.size(),
                "finalize acquisition deadline reached"
            );
            break;
        }
        if pool.size() as usize > conns.len() {
            // The pool holds a connection we don't have yet — either idle
            // (acquire returns it immediately) or checked out by a task that
            // hasn't observed the shutdown signal (acquire waits for it to be
            // returned, up to the timeout below).
            match tokio::time::timeout(std::time::Duration::from_secs(2), pool.acquire()).await {
                Ok(Ok(conn)) => {
                    conns.push(conn);
                    confirm_waits = 0;
                    waits = 0;
                }
                Ok(Err(e)) => {
                    info!("pool acquire returned error during finalize: {e}");
                    break;
                }
                Err(_) => {
                    waits += 1;
                    if waits % 5 == 0 {
                        info!(
                            held = conns.len(),
                            pool_size = pool.size(),
                            "finalize still waiting for connections to be returned"
                        );
                    }
                }
            }
        } else {
            // We hold every connection the pool currently has. A late
            // acquisition by another task first grows `pool.size()`, so just
            // wait a short moment and re-check — probing with acquire() here
            // would itself grow the pool (see comment above).
            confirm_waits += 1;
            if confirm_waits >= 2 {
                info!(
                    held = conns.len(),
                    pool_size = pool.size(),
                    "all existing connections acquired"
                );
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }

    info!(
        acquired = conns.len(),
        pool_size = pool.size(),
        checked_out_left = (pool.size() as usize).saturating_sub(conns.len()),
        "CR-SQLite connection acquisition complete"
    );

    for (i, conn) in conns.iter_mut().enumerate() {
        match sqlx::query("SELECT crsql_finalize()")
            .execute(&mut **conn)
            .await
        {
            Ok(_) => info!("connection {} finalized", i),
            Err(e) => tracing::error!("crsql_finalize failed for connection {}: {e}", i),
        }
    }

    // Return connections to the pool so close() can shut them down cleanly.
    drop(conns);

    // Every connection we could acquire has been finalized, so close()
    // completes immediately unless a connection is still checked out. The
    // timeout is a safety net for that case.
    match tokio::time::timeout(std::time::Duration::from_secs(30), pool.close()).await {
        Ok(()) => info!("database pool closed"),
        Err(_) => {
            tracing::error!(
                pool_size = pool.size(),
                idle_connections = pool.num_idle(),
                checked_out = (pool.size() as usize).saturating_sub(pool.num_idle()),
                "pool.close() timed out; a task is still holding a connection"
            );
            // Returning allows Tauri to finish its exit sequence; the leftover
            // connection is dropped by runtime shutdown. If it was never
            // finalized this panics inside sqlx — aborting tracked background
            // tasks before finalize is what prevents that.
        }
    }

    Ok(())
}

/// Fetch the `sql` text of a table's definition (empty string when missing).
async fn table_definition_sql(
    conn: &mut sqlx::SqliteConnection,
    name: &str,
) -> anyhow::Result<String> {
    let sql: Option<String> =
        sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE type='table' AND name = ?")
            .bind(name)
            .fetch_optional(&mut *conn)
            .await?;
    Ok(sql.unwrap_or_default())
}

/// Rebuild pre-sync tables whose schemas are incompatible with CRDT sync:
///
/// - `vaults`: INTEGER AUTOINCREMENT PK → TEXT uuid PK. Autoincrement ids
///   collide across devices; the default vault (old id=1) gets the fixed
///   `DEFAULT_VAULT_ID` so every device converges on one row. `notes.vault_id`
///   and the `notes.current_vault_id` setting are remapped to the new ids.
/// - `note_versions`: AUTOINCREMENT PK → TEXT uuid PK (same collision issue).
/// - `tags`: drop UNIQUE(name) — CRDT merges cannot enforce uniqueness, and a
///   same-name tag created on two devices would poison changeset application.
///
/// All steps are idempotent: they inspect the live schema and no-op once the
/// new shape is in place.
async fn migrate_sync_schema_v2(db: &Db) -> anyhow::Result<()> {
    // The whole migration must run on ONE connection: the PRAGMAs below are
    // per-connection and would be lost between pool checkouts.
    let mut conn = db.acquire().await?;
    sqlx::query("PRAGMA foreign_keys = OFF").execute(&mut *conn).await?;
    // legacy_alter_table: renaming a table must NOT rewrite REFERENCES in
    // dependent tables (paper_tags must keep pointing at "tags").
    sqlx::query("PRAGMA legacy_alter_table = ON").execute(&mut *conn).await?;

    let result = migrate_sync_schema_v2_inner(&mut *conn).await;

    sqlx::query("PRAGMA legacy_alter_table = OFF").execute(&mut *conn).await?;
    sqlx::query("PRAGMA foreign_keys = ON").execute(&mut *conn).await?;
    result
}

async fn migrate_sync_schema_v2_inner(conn: &mut sqlx::SqliteConnection) -> anyhow::Result<()> {
    // ── vaults → TEXT uuid PK ──
    let vaults_sql = table_definition_sql(&mut *conn, "vaults").await?;
    if vaults_sql.contains("AUTOINCREMENT") {
        let old_rows: Vec<(i64, String, String, String)> =
            sqlx::query_as("SELECT id, name, created_at, updated_at FROM vaults")
                .fetch_all(&mut *conn)
                .await?;
        sqlx::query("ALTER TABLE vaults RENAME TO vaults_old")
            .execute(&mut *conn)
            .await?;
        sqlx::query(
            "CREATE TABLE vaults (
                id TEXT PRIMARY KEY NOT NULL,
                name TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL DEFAULT '',
                updated_at TEXT NOT NULL DEFAULT ''
            )",
        )
        .execute(&mut *conn)
        .await?;
        let mut id_map: Vec<(i64, String)> = Vec::with_capacity(old_rows.len());
        for (old_id, name, created_at, updated_at) in old_rows {
            let new_id = if old_id == 1 {
                DEFAULT_VAULT_ID.to_string()
            } else {
                uuid::Uuid::new_v4().to_string()
            };
            sqlx::query("INSERT INTO vaults (id, name, created_at, updated_at) VALUES (?, ?, ?, ?)")
                .bind(&new_id)
                .bind(&name)
                .bind(&created_at)
                .bind(&updated_at)
                .execute(&mut *conn)
                .await?;
            id_map.push((old_id, new_id));
        }
        // Remap notes.vault_id. The column keeps INTEGER affinity on existing
        // databases, but SQLite's dynamic typing stores the uuid strings as
        // TEXT; schema_init.sql declares it TEXT for fresh installs.
        for (old_id, new_id) in &id_map {
            sqlx::query("UPDATE notes SET vault_id = ? WHERE vault_id = ?")
                .bind(new_id)
                .bind(old_id)
                .execute(&mut *conn)
                .await?;
        }
        // Remap the current-vault setting (stored as the old integer string).
        let cur: Option<String> = sqlx::query_scalar(
            "SELECT value FROM settings WHERE key = ?",
        )
        .bind(crate::core::vault_service::CURRENT_VAULT_KEY)
        .fetch_optional(&mut *conn)
        .await?;
        if let Some(cur) = cur {
            if let Ok(old_id) = cur.parse::<i64>() {
                if let Some((_, new_id)) = id_map.iter().find(|(o, _)| o == &old_id) {
                    sqlx::query(
                        "UPDATE settings SET value = ?, updated_at = ? WHERE key = ?",
                    )
                    .bind(new_id)
                    .bind(crate::core::time::now_iso())
                    .bind(crate::core::vault_service::CURRENT_VAULT_KEY)
                    .execute(&mut *conn)
                    .await?;
                }
            }
        }
        sqlx::query("DROP TABLE vaults_old").execute(&mut *conn).await?;
        info!(rows = id_map.len(), "migrated vaults to TEXT uuid PK");
    }

    // ── note_versions → TEXT uuid PK ──
    let nv_sql = table_definition_sql(&mut *conn, "note_versions").await?;
    if nv_sql.contains("AUTOINCREMENT") {
        let old_rows: Vec<(String, String, String, String, String)> = sqlx::query_as(
            "SELECT note_id, title, content, edited_by, created_at FROM note_versions",
        )
        .fetch_all(&mut *conn)
        .await?;
        sqlx::query("ALTER TABLE note_versions RENAME TO note_versions_old")
            .execute(&mut *conn)
            .await?;
        sqlx::query(
            "CREATE TABLE note_versions (
                id TEXT PRIMARY KEY NOT NULL,
                note_id TEXT NOT NULL DEFAULT '',
                title TEXT NOT NULL DEFAULT '',
                content TEXT NOT NULL DEFAULT '',
                edited_by TEXT NOT NULL DEFAULT 'agent',
                created_at TEXT NOT NULL DEFAULT ''
            )",
        )
        .execute(&mut *conn)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_note_versions_note ON note_versions(note_id)",
        )
        .execute(&mut *conn)
        .await?;
        for (note_id, title, content, edited_by, created_at) in &old_rows {
            sqlx::query(
                "INSERT INTO note_versions (id, note_id, title, content, edited_by, created_at) \
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(note_id)
            .bind(title)
            .bind(content)
            .bind(edited_by)
            .bind(created_at)
            .execute(&mut *conn)
            .await?;
        }
        sqlx::query("DROP TABLE note_versions_old").execute(&mut *conn).await?;
        info!(rows = old_rows.len(), "migrated note_versions to TEXT uuid PK");
    }

    // ── attachments: drop the checked FK so it can become a CRR ──
    // CR-SQLite forbids checked foreign keys on CRR tables. The paper
    // attachments table currently declares REFERENCES papers(id) ON DELETE
    // CASCADE; rebuild it FK-free (purge_paper deletes attachment rows
    // explicitly, tracked as CRDT deletes). Requires NOT NULL TEXT PK.
    //
    // Crash recovery: a crash between RENAME and DROP leaves `attachments_old`
    // behind while the guard below no longer fires (schema_init recreated an
    // empty FK-free `attachments`). Merge any stranded rows back — OR IGNORE
    // covers a crash after the INSERT — and drop the leftover table. This must
    // run BEFORE the rebuild below, whose RENAME would collide with it.
    if !table_definition_sql(&mut *conn, "attachments_old")
        .await?
        .is_empty()
    {
        let mut tx = conn.begin().await?;
        sqlx::query(
            "INSERT OR IGNORE INTO attachments (id, paper_id, file_name, file_path, file_type, description, created_at)
             SELECT id, paper_id, file_name, file_path, file_type, description, created_at FROM attachments_old",
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query("DROP TABLE attachments_old")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        info!("recovered stranded attachments_old rows after interrupted rebuild");
    }

    // The rebuild (RENAME → CREATE → INSERT → DROP) runs in ONE transaction:
    // previously each statement auto-committed, so a crash after the RENAME
    // stranded the data in `attachments_old` (the REFERENCES guard would no
    // longer fire on the next startup).
    if table_definition_sql(&mut *conn, "attachments")
        .await?
        .contains("REFERENCES")
    {
        info!("rebuilding attachments table without FK constraint (CRR sync)");
        let mut tx = conn.begin().await?;
        sqlx::query("ALTER TABLE attachments RENAME TO attachments_old")
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "CREATE TABLE attachments (
                id TEXT PRIMARY KEY NOT NULL,
                paper_id TEXT NOT NULL DEFAULT '',
                file_name TEXT NOT NULL DEFAULT '',
                file_path TEXT NOT NULL DEFAULT '',
                file_type TEXT NOT NULL DEFAULT '',
                description TEXT,
                created_at TEXT NOT NULL DEFAULT ''
            )",
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO attachments (id, paper_id, file_name, file_path, file_type, description, created_at)
             SELECT id, paper_id, file_name, file_path, file_type, description, created_at FROM attachments_old",
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query("DROP TABLE attachments_old")
            .execute(&mut *tx)
            .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_attachments_paper ON attachments(paper_id)")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
    }

    // ── TEXT primary keys must be declared NOT NULL for CR-SQLite ──
    // SQLite treats non-INTEGER PRIMARY KEY columns as nullable unless NOT
    // NULL is stated, and crsql_as_crr rejects such tables. The tags rebuild
    // also drops UNIQUE(name), which CRDT merges cannot enforce.
    const PK_FIX_TABLES: &[(&str, &str, &[&str])] = &[
        ("tags",
         "CREATE TABLE tags (id TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL DEFAULT '', color TEXT DEFAULT '#3b82f6', parent_id TEXT, created_at TEXT NOT NULL DEFAULT '')",
         &[]),
        ("collections",
         "CREATE TABLE collections (id TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL DEFAULT '', parent_id TEXT, sort_order INTEGER DEFAULT 0, created_at TEXT NOT NULL DEFAULT '')",
         &[]),
        ("saved_searches",
         "CREATE TABLE saved_searches (id TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL DEFAULT '', params_json TEXT NOT NULL DEFAULT '{}', created_at TEXT NOT NULL DEFAULT '')",
         &[]),
        ("saved_items",
         "CREATE TABLE saved_items (id TEXT PRIMARY KEY NOT NULL, title TEXT, url TEXT, doi TEXT, pdf_url TEXT, metadata TEXT, status TEXT NOT NULL DEFAULT 'pending', error TEXT, created_at TEXT NOT NULL DEFAULT '', processed_at TEXT)",
         &["CREATE INDEX IF NOT EXISTS idx_saved_items_status ON saved_items(status)"]),
        ("imports",
         "CREATE TABLE imports (id TEXT PRIMARY KEY NOT NULL, file_path TEXT, source_url TEXT, paper_id TEXT, status TEXT NOT NULL DEFAULT '', error TEXT, created_at TEXT NOT NULL DEFAULT '', completed_at TEXT)",
         &[]),
        ("file_bookmarks",
         "CREATE TABLE file_bookmarks (id TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL DEFAULT '', path TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL DEFAULT '')",
         &[]),
        ("bookmarks",
         "CREATE TABLE bookmarks (id TEXT PRIMARY KEY NOT NULL, title TEXT NOT NULL DEFAULT '', route TEXT NOT NULL DEFAULT '', params_json TEXT NOT NULL DEFAULT '{}', created_at TEXT NOT NULL DEFAULT '')",
         &["CREATE INDEX IF NOT EXISTS idx_bookmarks_route ON bookmarks(route)"]),
    ];
    for (table, create_sql, indexes) in PK_FIX_TABLES {
        let nullable_pk: Option<(i64,)> = sqlx::query_as(
            "SELECT \"notnull\" FROM pragma_table_info(?) WHERE pk = 1 LIMIT 1",
        )
        .bind(table)
        .fetch_optional(&mut *conn)
        .await?;
        let needs_fix = matches!(nullable_pk, Some((0,)))
            || (*table == "tags"
                && table_definition_sql(&mut *conn, table).await?.contains("UNIQUE"));
        if !needs_fix {
            continue;
        }
        sqlx::query(&format!("ALTER TABLE \"{table}\" RENAME TO \"{table}_old\""))
            .execute(&mut *conn)
            .await?;
        sqlx::query(create_sql).execute(&mut *conn).await?;
        sqlx::query(&format!("INSERT INTO \"{table}\" SELECT * FROM \"{table}_old\""))
            .execute(&mut *conn)
            .await?;
        sqlx::query(&format!("DROP TABLE \"{table}_old\""))
            .execute(&mut *conn)
            .await?;
        for index_sql in *indexes {
            sqlx::query(index_sql).execute(&mut *conn).await?;
        }
        info!(table = %table, "rebuilt table with NOT NULL primary key");
    }

    Ok(())
}

/// Check whether the CR-SQLite triggers for a table still match the current
/// table schema. If columns were added after `crsql_as_crr` was called, the
/// update trigger will not reference the new columns and will fail with a
/// mismatch like "expected 57 values, got 53" on UPDATE.
async fn crr_triggers_fresh(db: &Db, table: &str) -> anyhow::Result<bool> {
    let trigger_name = format!("{}__crsql_utrig", table);
    let trigger_sql: Option<(String,)> = sqlx::query_as(
        "SELECT sql FROM sqlite_master WHERE type='trigger' AND name = ?",
    )
    .bind(&trigger_name)
    .fetch_optional(db)
    .await
    .with_context(|| format!("fetch update trigger for {}", table))?;

    let Some((sql,)) = trigger_sql else {
        return Ok(false);
    };

    let rows = sqlx::query(&format!("PRAGMA table_info({})", table))
        .fetch_all(db)
        .await
        .with_context(|| format!("PRAGMA table_info for {}", table))?;

    for row in rows {
        let name: String = row.try_get("name")?;
        // The update trigger must reference every current column as NEW."col".
        if !sql.contains(&format!("NEW.\"{}\"", name)) {
            return Ok(false);
        }
    }

    Ok(true)
}

/// Drop all CR-SQLite artifacts for a table so it can be re-registered.
/// Existing clock / pks tables are removed; this resets sync metadata for the
/// table but is the only reliable way to fix stale triggers on schema change.
/// Also used by `set_sync_config` to unregister optional tables at runtime.
pub(crate) async fn drop_crr_objects(db: &Db, table: &str) -> anyhow::Result<()> {
    info!(table = %table, "dropping stale CRR objects");
    for suffix in ["_itrig", "_utrig", "_dtrig"] {
        let name = format!("{}__crsql{}", table, suffix);
        let sql = format!("DROP TRIGGER IF EXISTS \"{}\"", name);
        sqlx::query(&sql).execute(db).await.with_context(|| {
            format!("drop trigger {} for {}", name, table)
        })?;
    }
    for suffix in ["__crsql_clock", "__crsql_pks"] {
        let name = format!("{}{}", table, suffix);
        let sql = format!("DROP TABLE IF EXISTS \"{}\"", name);
        sqlx::query(&sql).execute(db).await.with_context(|| {
            format!("drop table {} for {}", name, table)
        })?;
    }
    Ok(())
}

/// If `table` is already registered as a CRR but its triggers are stale
/// (columns added after `crsql_as_crr`), drop and re-register it so that
/// subsequent migration UPDATEs don't trip the stale trigger's
/// "expected N values, got M" error. No-op for tables that are not CRRs
/// (fresh installs, or optional sync tables while optional sync is off) —
/// those get registered later by `register_crr_tables`.
/// Call this right BEFORE any data-migration UPDATE that can match rows on
/// a syncable table; the freshness pass in `register_crr_tables` runs too
/// late (after all migrations) to protect them.
pub(crate) async fn refresh_stale_crr_triggers(db: &Db, table: &str) -> anyhow::Result<()> {
    let clock_table = format!("{}__crsql_clock", table);
    let is_crr: (i64,) = sqlx::query_as(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name = ?",
    )
    .bind(&clock_table)
    .fetch_one(db)
    .await
    .with_context(|| format!("check CRR status for {}", table))?;
    if is_crr.0 == 0 || crr_triggers_fresh(db, table).await? {
        return Ok(());
    }
    info!(table = %table, "refreshing stale CRR triggers before data migration");
    drop_crr_objects(db, table).await?;
    let sql = format!("SELECT crsql_as_crr('{}')", table);
    sqlx::query(&sql)
        .execute(db)
        .await
        .with_context(|| format!("re-register {} as CRR", table))?;
    Ok(())
}

/// Register tables as CR-SQLite CRRs.
///
/// If a table is already a CRR but its triggers are stale (because columns
/// were added after the initial registration), the stale CRR objects are
/// dropped and the table is re-registered.
#[instrument(skip(db))]
pub(crate) async fn register_crr_tables(db: &Db, tables: &[&str]) -> anyhow::Result<()> {
    for table in tables {
        let clock_table = format!("{}__crsql_clock", table);
        let is_crr: (i64,) = sqlx::query_as(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name = ?",
        )
        .bind(&clock_table)
        .fetch_one(db)
        .await
        .with_context(|| format!("check CRR status for {}", table))?;

        if is_crr.0 > 0 {
            if crr_triggers_fresh(db, table).await? {
                info!(table = %table, "already registered as CRR and triggers are fresh");
                continue;
            }
            info!(table = %table, "CRR triggers stale, re-registering");
            drop_crr_objects(db, table).await?;
        }

        let sql = format!("SELECT crsql_as_crr('{}')", table);
        sqlx::query(&sql)
            .execute(db)
            .await
            .with_context(|| format!("register {} as CRR", table))?;
        info!(table = %table, "registered as CRR");
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    pub(crate) async fn connect_with_crsqlite(path: &std::path::Path) -> anyhow::Result<Db> {
        connect_with_crsqlite_max(path, 1).await
    }

    pub(crate) async fn connect_with_crsqlite_max(
        path: &std::path::Path,
        max_connections: u32,
    ) -> anyhow::Result<Db> {
        let ext_path = crsqlite_extension_path(None)?;
        let ext_escaped = ext_path.to_string_lossy().replace('\'', "''");
        let load_sql =
            format!("SELECT load_extension('{}', 'sqlite3_crsqlite_init')", ext_escaped);
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(max_connections)
            // Keep connections alive for the whole test: sqlx's default
            // 30-minute max_lifetime would close a connection without
            // crsql_finalize() and panic (see init()).
            .max_lifetime(None)
            .idle_timeout(None)
            .after_connect(move |conn, _meta| {
                let load = load_sql.clone();
                Box::pin(async move {
                    let mut handle = conn.lock_handle().await?;
                    let raw = handle.as_raw_handle().as_ptr();
                    let rc = unsafe { libsqlite3_sys::sqlite3_enable_load_extension(raw, 1) };
                    if rc != libsqlite3_sys::SQLITE_OK {
                        return Err(sqlx::Error::Io(std::io::Error::new(
                            std::io::ErrorKind::Other,
                            format!("sqlite3_enable_load_extension failed: {}", rc),
                        )));
                    }
                    drop(handle);
                    sqlx::query(&load).execute(&mut *conn).await?;
                    Ok(())
                })
            })
            .connect_with(options)
            .await?;
        Ok(pool)
    }

    /// The v2 sync migration must convert legacy INTEGER-PK tables to TEXT
    /// uuid PKs, remap notes.vault_id and the current-vault setting, and drop
    /// the UNIQUE(name) constraint on tags.
    #[tokio::test]
    async fn migrate_sync_schema_v2_converts_legacy_tables() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "siku-migration-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        let db = connect_with_crsqlite(&dir.join("m.db")).await?;

        // Legacy (pre-2026-08-19) table shapes.
        sqlx::query(
            "CREATE TABLE vaults (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, created_at TEXT NOT NULL, updated_at TEXT NOT NULL)",
        )
        .execute(&db)
        .await?;
        sqlx::query("CREATE TABLE notes (id TEXT PRIMARY KEY, vault_id INTEGER NOT NULL DEFAULT 1)")
            .execute(&db)
            .await?;
        sqlx::query(
            "CREATE TABLE settings (key TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL DEFAULT '', updated_at TEXT NOT NULL DEFAULT '')",
        )
        .execute(&db)
        .await?;
        sqlx::query(
            "CREATE TABLE tags (id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, color TEXT DEFAULT '#3b82f6', parent_id TEXT, created_at TEXT NOT NULL)",
        )
        .execute(&db)
        .await?;
        sqlx::query(
            "CREATE TABLE note_versions (id INTEGER PRIMARY KEY AUTOINCREMENT, note_id TEXT NOT NULL, title TEXT NOT NULL, content TEXT NOT NULL DEFAULT '', edited_by TEXT NOT NULL DEFAULT 'agent', created_at TEXT NOT NULL)",
        )
        .execute(&db)
        .await?;

        let ts = "2026-01-01T00:00:00Z";
        // vault id 1 = default, vault id 2 = custom.
        sqlx::query("INSERT INTO vaults (name, created_at, updated_at) VALUES ('cognitive-archive', ?, ?)")
            .bind(ts).bind(ts).execute(&db).await?;
        sqlx::query("INSERT INTO vaults (name, created_at, updated_at) VALUES ('work', ?, ?)")
            .bind(ts).bind(ts).execute(&db).await?;
        sqlx::query("INSERT INTO notes (id, vault_id) VALUES ('n1', 1), ('n2', 2)")
            .execute(&db)
            .await?;
        sqlx::query("INSERT INTO settings (key, value, updated_at) VALUES (?, '2', ?)")
            .bind(crate::core::vault_service::CURRENT_VAULT_KEY)
            .bind(ts)
            .execute(&db)
            .await?;
        sqlx::query("INSERT INTO note_versions (note_id, title, created_at) VALUES ('n1', 'Note 1', ?)")
            .bind(ts)
            .execute(&db)
            .await?;
        sqlx::query("INSERT INTO tags (id, name, created_at) VALUES ('t1', 'ml', ?)")
            .bind(ts)
            .execute(&db)
            .await?;

        super::migrate_sync_schema_v2(&db).await?;

        // The default vault converges on the fixed uuid; the custom one got a
        // fresh uuid (i.e. not a bare integer).
        let (default_id,): (String,) =
            sqlx::query_as("SELECT id FROM vaults WHERE name = 'cognitive-archive'")
                .fetch_one(&db)
                .await?;
        assert_eq!(default_id, DEFAULT_VAULT_ID);
        let (work_id,): (String,) = sqlx::query_as("SELECT id FROM vaults WHERE name = 'work'")
            .fetch_one(&db)
            .await?;
        assert!(work_id.len() > 8 && work_id.parse::<i64>().is_err());

        // notes.vault_id + the current-vault setting were remapped.
        let (n1_vault,): (String,) = sqlx::query_as("SELECT vault_id FROM notes WHERE id = 'n1'")
            .fetch_one(&db)
            .await?;
        assert_eq!(n1_vault, DEFAULT_VAULT_ID);
        let (n2_vault,): (String,) = sqlx::query_as("SELECT vault_id FROM notes WHERE id = 'n2'")
            .fetch_one(&db)
            .await?;
        assert_eq!(n2_vault, work_id);
        let (cur,): (String,) = sqlx::query_as(
            "SELECT value FROM settings WHERE key = ?",
        )
        .bind(crate::core::vault_service::CURRENT_VAULT_KEY)
        .fetch_one(&db)
        .await?;
        assert_eq!(cur, work_id);

        // note_versions rows survived with uuid ids.
        let (nv_id,): (String,) = sqlx::query_as("SELECT id FROM note_versions WHERE note_id = 'n1'")
            .fetch_one(&db)
            .await?;
        assert!(nv_id.parse::<i64>().is_err());

        // tags no longer enforces UNIQUE(name).
        sqlx::query("INSERT INTO tags (id, name, created_at) VALUES ('t2', 'ml', ?)")
            .bind(ts)
            .execute(&db)
            .await?;

        // Idempotent: a second run is a no-op.
        super::migrate_sync_schema_v2(&db).await?;
        let (vault_count,): (i64,) = sqlx::query_as("SELECT count(*) FROM vaults")
            .fetch_one(&db)
            .await?;
        assert_eq!(vault_count, 2);

        sqlx::query("SELECT crsql_finalize()").execute(&db).await?;
        db.close().await;
        Ok(())
    }

    /// Legacy `attachments` tables declare `REFERENCES papers(id) ON DELETE
    /// CASCADE`; CR-SQLite forbids checked FKs on CRR tables, so the v2 sync
    /// migration must rebuild the table FK-free (data preserved) before
    /// attachments can join CORE_SYNC_TABLES.
    #[tokio::test]
    async fn sync_schema_v2_rebuilds_attachments_without_fk() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "siku-attach-migrate-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        let db_path = dir.join("test.db");
        let db = connect_with_crsqlite(&db_path).await?;

        // Legacy schema: NOT NULL TEXT PK (nullable PK would be fixed by the
        // same migration family) with a checked FK.
        sqlx::query(
            "CREATE TABLE papers (id TEXT PRIMARY KEY NOT NULL, title TEXT NOT NULL DEFAULT '')",
        )
        .execute(&db)
        .await?;
        sqlx::query(
            "CREATE TABLE attachments (
                id TEXT PRIMARY KEY,
                paper_id TEXT NOT NULL REFERENCES papers(id) ON DELETE CASCADE,
                file_name TEXT NOT NULL,
                file_path TEXT NOT NULL,
                file_type TEXT NOT NULL,
                description TEXT,
                created_at TEXT NOT NULL
            )",
        )
        .execute(&db)
        .await?;
        sqlx::query(
            "INSERT INTO papers (id, title) VALUES ('p1', 'Paper One')",
        )
        .execute(&db)
        .await?;
        sqlx::query(
            "INSERT INTO attachments (id, paper_id, file_name, file_path, file_type, created_at) \
             VALUES ('att-legacy', 'p1', 'main.pdf', 'blobs/x.pdf', 'pdf', '2026-01-01T00:00:00Z')",
        )
        .execute(&db)
        .await?;

        super::migrate_sync_schema_v2(&db).await?;

        let ddl: (String,) = sqlx::query_as(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='attachments'",
        )
        .fetch_one(&db)
        .await?;
        assert!(
            !ddl.0.contains("REFERENCES"),
            "attachments must be FK-free after migration: {}",
            ddl.0
        );
        assert!(
            ddl.0.contains("NOT NULL"),
            "attachments PK must be NOT NULL for CR-SQLite: {}",
            ddl.0
        );
        let (name,): (String,) =
            sqlx::query_as("SELECT file_name FROM attachments WHERE id = 'att-legacy'")
                .fetch_one(&db)
                .await?;
        assert_eq!(name, "main.pdf", "data must survive the rebuild");

        // The FK-free table must register as a CRR without error.
        register_crr_tables(&db, &["attachments"]).await?;
        let clock: (i64,) = sqlx::query_as(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='attachments__crsql_clock'",
        )
        .fetch_one(&db)
        .await?;
        assert_eq!(clock.0, 1, "attachments must be registered as a CRR");

        sqlx::query("SELECT crsql_finalize()").execute(&db).await?;
        db.close().await;
        Ok(())
    }

    /// Crash recovery: if the app died between the RENAME and DROP of the
    /// attachments rebuild, the next startup has an empty FK-free
    /// `attachments` (recreated by schema_init's CREATE TABLE IF NOT EXISTS)
    /// plus a stranded `attachments_old` — and the `REFERENCES` guard no
    /// longer fires. The migration must merge the stranded rows back
    /// (OR IGNORE also covers rows already copied before a later crash point)
    /// and drop the old table.
    #[tokio::test]
    async fn sync_schema_v2_recovers_stranded_attachments_old() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "siku-attach-recover-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        let db = connect_with_crsqlite(&dir.join("test.db")).await?;

        sqlx::query(
            "CREATE TABLE papers (id TEXT PRIMARY KEY NOT NULL, title TEXT NOT NULL DEFAULT '')",
        )
        .execute(&db)
        .await?;
        sqlx::query("INSERT INTO papers (id, title) VALUES ('p1', 'Paper One')")
            .execute(&db)
            .await?;

        // Post-crash state: the FK-free table recreated by schema_init holds
        // only what the interrupted INSERT had already copied…
        sqlx::query(
            "CREATE TABLE attachments (
                id TEXT PRIMARY KEY NOT NULL,
                paper_id TEXT NOT NULL DEFAULT '',
                file_name TEXT NOT NULL DEFAULT '',
                file_path TEXT NOT NULL DEFAULT '',
                file_type TEXT NOT NULL DEFAULT '',
                description TEXT,
                created_at TEXT NOT NULL DEFAULT ''
            )",
        )
        .execute(&db)
        .await?;
        sqlx::query(
            "INSERT INTO attachments (id, paper_id, file_name, file_path, file_type, created_at) \
             VALUES ('att-copied', 'p1', 'main.pdf', 'blobs/x.pdf', 'pdf', '2026-01-01T00:00:00Z')",
        )
        .execute(&db)
        .await?;
        // …while the legacy table (still carrying the FK) holds everything.
        sqlx::query(
            "CREATE TABLE attachments_old (
                id TEXT PRIMARY KEY,
                paper_id TEXT NOT NULL REFERENCES papers(id) ON DELETE CASCADE,
                file_name TEXT NOT NULL,
                file_path TEXT NOT NULL,
                file_type TEXT NOT NULL,
                description TEXT,
                created_at TEXT NOT NULL
            )",
        )
        .execute(&db)
        .await?;
        sqlx::query(
            "INSERT INTO attachments_old (id, paper_id, file_name, file_path, file_type, created_at) \
             VALUES ('att-copied', 'p1', 'main.pdf', 'blobs/x.pdf', 'pdf', '2026-01-01T00:00:00Z'), \
                    ('att-stranded', 'p1', 'supp.pdf', 'blobs/y.pdf', 'pdf', '2026-01-01T00:00:00Z')",
        )
        .execute(&db)
        .await?;

        super::migrate_sync_schema_v2(&db).await?;

        let names: Vec<String> = sqlx::query_scalar(
            "SELECT file_name FROM attachments ORDER BY id",
        )
        .fetch_all(&db)
        .await?;
        assert_eq!(names, vec!["main.pdf", "supp.pdf"], "stranded rows must be merged back");
        let old_left: (i64,) = sqlx::query_as(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='attachments_old'",
        )
        .fetch_one(&db)
        .await?;
        assert_eq!(old_left.0, 0, "attachments_old must be dropped");

        // Idempotent: a second run leaves the data untouched.
        super::migrate_sync_schema_v2(&db).await?;
        let (count,): (i64,) = sqlx::query_as("SELECT count(*) FROM attachments")
            .fetch_one(&db)
            .await?;
        assert_eq!(count, 2);

        sqlx::query("SELECT crsql_finalize()").execute(&db).await?;
        db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn core_tables_register_as_crr() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!("siku-crr-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        let db_path = dir.join("test.db");
        let db = connect_with_crsqlite(&db_path).await?;

        sqlx::query(SCHEMA_INIT_SQL).execute(&db).await?;

        register_crr_tables(&db, CORE_SYNC_TABLES).await?;

        for table in CORE_SYNC_TABLES {
            let clock: (i64,) = sqlx::query_as(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name = ?",
            )
            .bind(format!("{}__crsql_clock", table))
            .fetch_one(&db)
            .await?;
            assert_eq!(clock.0, 1, "{} should be a CRR", table);
        }

        // Idempotency: re-registering should succeed without error.
        register_crr_tables(&db, CORE_SYNC_TABLES).await?;

        // CR-SQLite requires explicit finalization before closing the connection.
        sqlx::query("SELECT crsql_finalize()").execute(&db).await?;
        db.close().await;

        Ok(())
    }

    #[tokio::test]
    async fn optional_tables_register_as_crr() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "siku-crr-optional-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        let db_path = dir.join("test.db");
        let db = connect_with_crsqlite(&db_path).await?;

        sqlx::query(SCHEMA_INIT_SQL).execute(&db).await?;
        register_crr_tables(&db, CORE_SYNC_TABLES).await?;
        register_crr_tables(&db, OPTIONAL_SYNC_TABLES).await?;

        for table in OPTIONAL_SYNC_TABLES {
            let clock: (i64,) = sqlx::query_as(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name = ?",
            )
            .bind(format!("{}__crsql_clock", table))
            .fetch_one(&db)
            .await?;
            assert_eq!(clock.0, 1, "{} should be a CRR", table);
        }

        sqlx::query("SELECT crsql_finalize()").execute(&db).await?;
        db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn crr_triggers_are_refreshed_after_schema_change() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "siku-crr-stale-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        let db_path = dir.join("test.db");
        let db = connect_with_crsqlite(&db_path).await?;

        sqlx::query(SCHEMA_INIT_SQL).execute(&db).await?;

        // Register chat_sessions while it has the original schema.
        register_crr_tables(&db, &["chat_sessions"]).await?;

        // Add a column after CRR registration, simulating a future migration.
        sqlx::query("ALTER TABLE chat_sessions ADD COLUMN new_test_col TEXT")
            .execute(&db)
            .await?;

        // Re-registering must detect the stale trigger and recreate it.
        register_crr_tables(&db, &["chat_sessions"]).await?;

        let trigger_sql: Option<(String,)> = sqlx::query_as(
            "SELECT sql FROM sqlite_master WHERE type='trigger' AND name = 'chat_sessions__crsql_utrig'",
        )
        .fetch_optional(&db)
        .await?;
        let trigger_sql = trigger_sql.expect("update trigger should exist").0;
        assert!(
            trigger_sql.contains("NEW.\"new_test_col\""),
            "update trigger should reference the newly added column: {trigger_sql}"
        );

        // An UPDATE on the migrated table must not fail with a value-count mismatch.
        sqlx::query("INSERT INTO chat_sessions (id, title) VALUES ('s1', 't')")
            .execute(&db)
            .await?;
        sqlx::query("UPDATE chat_sessions SET title = 'updated' WHERE id = 's1'")
            .execute(&db)
            .await?;

        sqlx::query("SELECT crsql_finalize()").execute(&db).await?;
        db.close().await;
        Ok(())
    }

    /// Regression test for the startup crash "migration failed for
    /// chat_sessions max_tokens resemantic: expected 59 values, got 57":
    /// a data-migration UPDATE on a table whose CRR triggers went stale
    /// (columns added after registration) must refresh the triggers first —
    /// register_crr_tables runs too late (after all migrations).
    #[tokio::test]
    async fn data_migration_update_refreshes_stale_crr_triggers() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "siku-crr-migration-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        let db_path = dir.join("test.db");
        let db = connect_with_crsqlite(&db_path).await?;

        sqlx::query(SCHEMA_INIT_SQL).execute(&db).await?;
        sqlx::query("INSERT INTO chat_sessions (id, title) VALUES ('s1', 't')")
            .execute(&db)
            .await?;

        // CRR registered against the current schema, then a later migration
        // adds a column — the update trigger is now stale.
        register_crr_tables(&db, &["chat_sessions"]).await?;
        sqlx::query("ALTER TABLE chat_sessions ADD COLUMN another_test_col TEXT")
            .execute(&db)
            .await?;

        // Without the refresh the UPDATE would fail inside the stale trigger.
        refresh_stale_crr_triggers(&db, "chat_sessions").await?;
        sqlx::query("UPDATE chat_sessions SET another_test_col = 'x' WHERE id = 's1'")
            .execute(&db)
            .await?;

        // No-op for a table that is not a CRR (must not register it).
        refresh_stale_crr_triggers(&db, "papers").await?;
        let clock: (i64,) = sqlx::query_as(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name = 'papers__crsql_clock'",
        )
        .fetch_one(&db)
        .await?;
        assert_eq!(clock.0, 0, "refresh must not register a non-CRR table");

        sqlx::query("SELECT crsql_finalize()").execute(&db).await?;
        db.close().await;
        Ok(())
    }

    /// Regression test for the shutdown panic
    /// ("(code: 5) unable to close due to unfinalized statements or unfinished
    /// backups", sqlx handle.rs). finalize_db must wait for connections that
    /// are still checked out at shutdown (a background task that has not yet
    /// observed the shutdown signal), finalize them, and close the pool
    /// without sqlite3_close panicking.
    #[tokio::test]
    async fn finalize_db_waits_for_connections_returned_during_shutdown() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!("siku-finalize-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        let db_path = dir.join("test.db");
        let db = connect_with_crsqlite_max(&db_path, 2).await?;

        sqlx::query(SCHEMA_INIT_SQL).execute(&db).await?;
        register_crr_tables(&db, CORE_SYNC_TABLES).await?;
        // Touch the CRR so the extension prepares (and caches) statements on
        // the pooled connections — exactly the state that panics on close if
        // crsql_finalize() is skipped.
        sqlx::query("INSERT INTO notes (id, title, content) VALUES ('n1', 't', 'c')")
            .execute(&db)
            .await?;

        // Occupy one connection, like a background task that has not yet
        // observed the shutdown signal.
        let held = db.acquire().await?;

        // Finalize while one connection is still checked out: the poll-acquire
        // loop must wait for the holder, finalize it too, and close the pool
        // without panicking inside ConnectionHandle::drop.
        let pool = db.clone();
        let finalize_task = tokio::spawn(async move {
            finalize_db(&pool).await.unwrap();
        });

        // Release the connection after the first acquire attempt has timed out.
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        drop(held);

        tokio::time::timeout(std::time::Duration::from_secs(40), finalize_task)
            .await
            .map_err(|_| anyhow::anyhow!("finalize_db did not complete in time"))??;

        Ok(())
    }
}
