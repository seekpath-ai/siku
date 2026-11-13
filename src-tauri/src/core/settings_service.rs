use sqlx::SqlitePool;
use tracing::instrument;
use once_cell::sync::Lazy;
use std::sync::RwLock;

use crate::core::models::{AppSettings, DeviceAppSettings, Setting};
use crate::core::time;

const APP_SETTINGS_KEY: &str = "app_settings";
const DEVICE_SETTINGS_KEY: &str = "device_settings";

static APP_SETTINGS_CACHE: Lazy<RwLock<AppSettings>> =
    Lazy::new(|| RwLock::new(AppSettings::default()));

static DEVICE_SETTINGS_CACHE: Lazy<RwLock<DeviceAppSettings>> =
    Lazy::new(|| RwLock::new(DeviceAppSettings::default()));

/// Read the cached app settings without hitting the database.
pub fn cached_settings() -> AppSettings {
    APP_SETTINGS_CACHE.read().unwrap().clone()
}

/// Update the in-memory settings cache.
pub fn update_cached_settings(settings: AppSettings) {
    *APP_SETTINGS_CACHE.write().unwrap() = settings;
}

/// Read the cached device-local settings without hitting the database.
pub fn cached_device_settings() -> DeviceAppSettings {
    DEVICE_SETTINGS_CACHE.read().unwrap().clone()
}

/// Update the in-memory device-local settings cache.
pub fn update_cached_device_settings(settings: DeviceAppSettings) {
    *DEVICE_SETTINGS_CACHE.write().unwrap() = settings;
}

/// Get a single setting by key
#[instrument(skip(db))]
pub async fn get_setting(db: &SqlitePool, key: &str) -> Result<Option<String>, String> {
    let result = sqlx::query_as::<_, Setting>("SELECT key, value, updated_at FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(db)
        .await
        .map_err(|e| format!("db error: {e}"))?;

    Ok(result.map(|s| s.value))
}

/// Get a single device-local setting by key
#[instrument(skip(db))]
pub async fn get_device_setting(db: &SqlitePool, key: &str) -> Result<Option<String>, String> {
    let result = sqlx::query_as::<_, Setting>("SELECT key, value, updated_at FROM device_settings WHERE key = ?")
        .bind(key)
        .fetch_optional(db)
        .await
        .map_err(|e| format!("db error: {e}"))?;

    Ok(result.map(|s| s.value))
}

/// Set a setting value
#[instrument(skip(db))]
pub async fn set_setting(db: &SqlitePool, key: &str, value: &str) -> Result<(), String> {
    let now = time::now_iso();

    sqlx::query(
        "INSERT INTO settings (key, value, updated_at) VALUES (?, ?, ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(key)
    .bind(value)
    .bind(&now)
    .execute(db)
    .await
    .map_err(|e| format!("db error: {e}"))?;

    Ok(())
}

/// Set a device-local setting value
#[instrument(skip(db))]
pub async fn set_device_setting(db: &SqlitePool, key: &str, value: &str) -> Result<(), String> {
    let now = time::now_iso();

    sqlx::query(
        "INSERT INTO device_settings (key, value, updated_at) VALUES (?, ?, ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(key)
    .bind(value)
    .bind(&now)
    .execute(db)
    .await
    .map_err(|e| format!("db error: {e}"))?;

    Ok(())
}

/// Get all settings
#[instrument(skip(db))]
pub async fn get_all_settings(db: &SqlitePool) -> Result<Vec<Setting>, String> {
    let settings = sqlx::query_as::<_, Setting>("SELECT key, value, updated_at FROM settings ORDER BY key")
        .fetch_all(db)
        .await
        .map_err(|e| format!("db error: {e}"))?;

    Ok(settings)
}

/// Load app settings from the database, falling back to defaults.
pub async fn load_app_settings(db: &SqlitePool) -> Result<AppSettings, String> {
    match get_setting(db, APP_SETTINGS_KEY).await? {
        Some(value) => serde_json::from_str(&value).map_err(|e| format!("invalid app settings: {e}")),
        None => Ok(AppSettings::default()),
    }
}

/// Save app settings to the database and refresh the in-memory cache.
pub async fn save_app_settings(
    db: &SqlitePool,
    settings: &AppSettings,
) -> Result<(), String> {
    let settings = settings.clone();
    let value = serde_json::to_string(&settings).map_err(|e| format!("json: {e}"))?;
    set_setting(db, APP_SETTINGS_KEY, &value).await?;
    update_cached_settings(settings);
    Ok(())
}

/// Load device-local settings from the database, falling back to defaults.
pub async fn load_device_settings(db: &SqlitePool) -> Result<DeviceAppSettings, String> {
    match get_device_setting(db, DEVICE_SETTINGS_KEY).await? {
        Some(value) => serde_json::from_str(&value).map_err(|e| format!("invalid device settings: {e}")),
        None => Ok(DeviceAppSettings::default()),
    }
}

/// Save device-local settings to the database and refresh the in-memory cache.
pub async fn save_device_settings(
    db: &SqlitePool,
    settings: &DeviceAppSettings,
) -> Result<(), String> {
    let value = serde_json::to_string(settings).map_err(|e| format!("json: {e}"))?;
    set_device_setting(db, DEVICE_SETTINGS_KEY, &value).await?;
    update_cached_device_settings(settings.clone());
    Ok(())
}

/// Load settings from DB into the global cache. Called once during startup.
pub async fn refresh_cache(db: &SqlitePool) -> Result<(), String> {
    let settings = load_app_settings(db).await?;
    update_cached_settings(settings);
    let device_settings = load_device_settings(db).await?;
    update_cached_device_settings(device_settings);
    Ok(())
}

/// One-time migration: account credentials (token, sync key, email, …) used
/// to live in the syncable `settings` table, which leaked them to every
/// paired device. Move any leftover rows into device-local settings and drop
/// them from the syncable table so they can never be synced again.
/// Key strings mirror `commands::account::ACCOUNT_*` and
/// `sync::onboarding::ACCOUNT_SYNC_KEY_SETTING`.
pub async fn migrate_legacy_account_settings(db: &SqlitePool) -> Result<(), String> {
    const LEGACY_KEYS: &[&str] = &[
        "account.relay_url",
        "account.token",
        "account.user_id",
        "account.email",
        "account.device_name",
        "account.sync_key",
    ];
    for key in LEGACY_KEYS {
        let Some(value) = get_setting(db, key).await? else {
            continue;
        };
        if value.is_empty() {
            continue;
        }
        let already = get_device_setting(db, key)
            .await?
            .map(|v| !v.is_empty())
            .unwrap_or(false);
        if !already {
            let _ = set_device_setting(db, key, &value).await;
        }
        // Remove from the syncable table regardless.
        let _ = sqlx::query("DELETE FROM settings WHERE key = ?")
            .bind(key)
            .execute(db)
            .await;
    }
    Ok(())
}

// ---------------------------------------------------------------- remote channels (synced)

/// Legacy combined runtime row. Kept for one-way migration and old clients;
/// new code splits the cursor and context tokens into separate rows below.
pub const REMOTE_WEIXIN_STATE_KEY: &str = "remote_weixin_state";
/// Long-poll cursor: written on every cursor advance. Must never share a
/// settings row with the chattier context tokens: last-writer-wins on the
/// whole row can otherwise resurrect an older cursor.
pub const REMOTE_WEIXIN_CURSOR_KEY: &str = "remote_weixin_cursor";
pub const REMOTE_WEIXIN_CONTEXT_KEY: &str = "remote_weixin_context_tokens";
/// Marker that the legacy combined row has been migrated to the split keys.
pub const REMOTE_WEIXIN_V2_KEY: &str = "remote_weixin_state_v2";
pub const REMOTE_HOLDER_KEY: &str = "remote_holder";
/// Feishu bots + weixin binding roam in this dedicated synced settings row.
/// NOT in `app_settings`: that row never syncs (it embeds LLM API keys),
/// which is why the earlier "roaming" design silently never left the device.
pub const REMOTE_CONFIG_KEY: &str = "remote_config";

/// Load the synced remote-channel config (feishu bots + weixin binding).
pub async fn load_remote_config(
    db: &SqlitePool,
) -> Result<crate::core::models::RemoteSettings, String> {
    match get_setting(db, REMOTE_CONFIG_KEY).await? {
        Some(v) => {
            let mut cfg: crate::core::models::RemoteSettings =
                serde_json::from_str(&v).map_err(|e| format!("invalid remote config: {e}"))?;
            cfg.normalize();
            Ok(cfg)
        }
        None => Ok(Default::default()),
    }
}

pub async fn save_remote_config(
    db: &SqlitePool,
    cfg: &crate::core::models::RemoteSettings,
) -> Result<(), String> {
    let mut cfg = cfg.clone();
    cfg.normalize();
    // The weixin long-poll cursor/context tokens ride their own synced row
    // (REMOTE_WEIXIN_STATE_KEY) so their churn cannot clobber config edits
    // via whole-row last-writer-wins. Never persist them here.
    cfg.weixin.get_updates_buf = String::new();
    cfg.weixin.context_tokens = Default::default();
    let v = serde_json::to_string(&cfg).map_err(|e| format!("json: {e}"))?;
    set_setting(db, REMOTE_CONFIG_KEY, &v).await
}

/// Load the synced weixin runtime state (long-poll cursor + context tokens).
/// Cursor and tokens live in separate settings rows; a legacy combined row is
/// migrated once (its value may be stale, so it only fills missing halves).
pub async fn load_weixin_state(
    db: &SqlitePool,
) -> Result<crate::core::models::WeixinChannelState, String> {
    let migrated = get_setting(db, REMOTE_WEIXIN_V2_KEY).await?.is_some();
    let mut cursor = get_setting(db, REMOTE_WEIXIN_CURSOR_KEY)
        .await?
        .unwrap_or_default();
    let mut context_tokens: std::collections::HashMap<String, String> =
        match get_setting(db, REMOTE_WEIXIN_CONTEXT_KEY).await? {
        Some(v) if !v.trim().is_empty() => serde_json::from_str(&v).unwrap_or_default(),
        _ => Default::default(),
    };

    if !migrated {
        let legacy = get_setting(db, REMOTE_WEIXIN_STATE_KEY).await?;
        let had_legacy = legacy.is_some();
        if let Some(legacy) = legacy {
            if let Ok(old) =
                serde_json::from_str::<crate::core::models::WeixinChannelState>(&legacy)
            {
                if cursor.is_empty() {
                    cursor = old.get_updates_buf;
                }
                if context_tokens.is_empty() {
                    context_tokens = old.context_tokens;
                }
            }
        }
        // Only mark migrated once there is something to migrate or split keys
        // already exist. On a fresh device the legacy row may sync in later;
        // writing the marker on the first empty load would make us ignore the
        // previous holder's cursor forever — exactly the cold-start takeover
        // race this migration is supposed to survive.
        if had_legacy || !cursor.is_empty() || !context_tokens.is_empty() {
            if !cursor.is_empty() {
                set_setting(db, REMOTE_WEIXIN_CURSOR_KEY, &cursor).await?;
            }
            if !context_tokens.is_empty() {
                let json =
                    serde_json::to_string(&context_tokens).map_err(|e| format!("json: {e}"))?;
                set_setting(db, REMOTE_WEIXIN_CONTEXT_KEY, &json).await?;
            }
            set_setting(db, REMOTE_WEIXIN_V2_KEY, "1").await?;
        }
    }

    Ok(crate::core::models::WeixinChannelState {
        get_updates_buf: cursor,
        context_tokens,
    })
}

/// Persist ONLY the long-poll cursor. This is the hot path; it must not touch
/// the context-token row.
pub async fn save_weixin_cursor(db: &SqlitePool, cursor: &str) -> Result<(), String> {
    set_setting(db, REMOTE_WEIXIN_CURSOR_KEY, cursor).await?;
    if get_setting(db, REMOTE_WEIXIN_V2_KEY).await?.is_none() {
        set_setting(db, REMOTE_WEIXIN_V2_KEY, "1").await?;
    }
    Ok(())
}

/// Persist ONLY the context-token map. This must not carry a cursor value: a
/// context update on a standby device used to write back its stale cursor.
pub async fn save_weixin_context_tokens(
    db: &SqlitePool,
    context_tokens: &std::collections::HashMap<String, String>,
) -> Result<(), String> {
    let json = serde_json::to_string(context_tokens).map_err(|e| format!("json: {e}"))?;
    set_setting(db, REMOTE_WEIXIN_CONTEXT_KEY, &json).await?;
    // Deliberately do NOT mark the legacy state migrated here: a token write
    // can happen before a mixed-version peer's legacy cursor row has synced.
    // `load_weixin_state` still fills the missing cursor from that row.
    Ok(())
}

/// Write both halves (logout / fresh binding / one-time migration path).
pub async fn save_weixin_state(
    db: &SqlitePool,
    state: &crate::core::models::WeixinChannelState,
) -> Result<(), String> {
    save_weixin_cursor(db, &state.get_updates_buf).await?;
    save_weixin_context_tokens(db, &state.context_tokens).await?;
    set_setting(db, REMOTE_WEIXIN_V2_KEY, "1").await
}

/// The manual-takeover record, if any.
pub async fn load_remote_holder(
    db: &SqlitePool,
) -> Result<Option<crate::core::models::RemoteHolder>, String> {
    match get_setting(db, REMOTE_HOLDER_KEY).await? {
        Some(v) => serde_json::from_str(&v)
            .map(Some)
            .map_err(|e| format!("invalid remote holder: {e}")),
        None => Ok(None),
    }
}

pub async fn save_remote_holder(
    db: &SqlitePool,
    holder: &crate::core::models::RemoteHolder,
) -> Result<(), String> {
    let v = serde_json::to_string(holder).map_err(|e| format!("json: {e}"))?;
    set_setting(db, REMOTE_HOLDER_KEY, &v).await
}

/// Fill gaps in `target` from `source`: feishu bots union by app_id (the
/// target's version of a bot wins), weixin taken wholesale only when the
/// target has no binding yet.
fn merge_remote_config(
    target: &mut crate::core::models::RemoteSettings,
    source: &crate::core::models::RemoteSettings,
) {
    for bot in &source.feishu_bots {
        if !bot.app_id.is_empty()
            && !target.feishu_bots.iter().any(|b| b.app_id == bot.app_id)
        {
            target.feishu_bots.push(bot.clone());
        }
    }
    if target.weixin.bot_token.is_empty() && !source.weixin.bot_token.is_empty() {
        target.weixin = source.weixin.clone();
    }
}

/// One-time migration: remote channel config lived in `app_settings.remote`
/// (and before that in device-local settings) — but the `app_settings` row
/// never syncs, so the "roaming" config never actually left the device.
/// Move everything into the dedicated synced `remote_config` row, merging
/// all legacy sources gap-fill style so no device's feishu bots or weixin
/// binding is lost, then blank the legacy copies so they cannot resurrect
/// deleted entries on a later run.
/// NOTE: bot tokens MAY roam — they are protected by the account sync key,
/// not the root of trust themselves.
pub async fn migrate_remote_settings(db: &SqlitePool) -> Result<(), String> {
    let mut merged = load_remote_config(db).await?;
    let mut app = load_app_settings(db).await?;
    app.remote.normalize();
    let mut dev = load_device_settings(db).await?.remote;
    dev.normalize();
    let app_has = !app.remote.feishu_bots.is_empty() || !app.remote.weixin.bot_token.is_empty();
    let dev_has = !dev.feishu_bots.is_empty() || !dev.weixin.bot_token.is_empty();
    if !app_has && !dev_has {
        return Ok(());
    }
    merge_remote_config(&mut merged, &app.remote);
    merge_remote_config(&mut merged, &dev);
    tracing::info!("migrating remote channel config to the synced remote_config row");
    save_remote_config(db, &merged).await?;
    if app_has {
        app.remote = Default::default();
        save_app_settings(db, &app).await?;
    }
    if dev_has {
        // The legacy device-local weixin cursor/context tokens move to their
        // synced state row (gap-fill: an existing synced cursor wins).
        if !dev.weixin.get_updates_buf.is_empty() || !dev.weixin.context_tokens.is_empty() {
            let existing = load_weixin_state(db).await?;
            if existing.get_updates_buf.is_empty() && existing.context_tokens.is_empty() {
                save_weixin_state(
                    db,
                    &crate::core::models::WeixinChannelState {
                        get_updates_buf: dev.weixin.get_updates_buf.clone(),
                        context_tokens: dev.weixin.context_tokens.clone(),
                    },
                )
                .await?;
            }
        }
        let mut dev_settings = load_device_settings(db).await?;
        dev_settings.remote = Default::default();
        save_device_settings(db, &dev_settings).await?;
    }
    Ok(())
}

/// One-time migration: multi-bot feishu support encodes the bot instance in
/// `remote_chats.channel` ("feishu:<app_id>"). Rewrite legacy bare "feishu"
/// rows when the bot list holds exactly one bot — their channel meant that
/// app. With zero or several bots the rows are left alone (members re-pair
/// on their next message, same as a fresh install).
/// The move is delete + insert, NOT a PK-changing UPDATE: crsql records such
/// an update only under the new key, leaving the bare-key row orphaned on
/// every peer (production: the same pairing listed twice after sync). The
/// INSERT is OR IGNORE so an already-paired suffixed row keeps its own
/// grant/session; the explicit DELETE propagates the bare row's removal.
pub async fn migrate_feishu_instance_channels(db: &SqlitePool) -> Result<(), String> {
    let remote = load_remote_config(db).await?;
    if remote.feishu_bots.len() != 1 {
        return Ok(());
    }
    let channel = format!("feishu:{}", remote.feishu_bots[0].app_id);
    sqlx::query(
        "INSERT OR IGNORE INTO remote_chats (channel, chat_id, user_id, user_name, session_id, approved, grant_level, created_at, updated_at)
         SELECT ?, chat_id, user_id, user_name, session_id, approved, grant_level, created_at, updated_at FROM remote_chats WHERE channel = 'feishu'",
    )
    .bind(&channel)
    .execute(db)
    .await
    .map_err(|e| format!("db error: {e}"))?;
    sqlx::query("DELETE FROM remote_chats WHERE channel = 'feishu'")
        .execute(db)
        .await
        .map_err(|e| format!("db error: {e}"))?;
    Ok(())
}

/// Build LLM config from the unified provider pool.
pub async fn load_llm_config(db: &SqlitePool) -> Result<crate::ai::llm::LlmConfig, String> {
    crate::core::llm_provider_service::load_default_llm_config(db).await
}

/// Validate an LLM config by making a test request
pub async fn validate_llm_config(config: &crate::ai::llm::LlmConfig) -> Result<bool, String> {
    let client = crate::ai::llm::client::create_llm_client(config)?;

    let test_messages = vec![crate::ai::llm::ChatMessage { reasoning_content: None,
        role: "user".to_string(),
        content: "Hello, respond with just 'ok'.".to_string(),
        attachments: None,
        tool_calls: None,
        tool_call_id: None,
        name: None,
    }];

    match client.chat_completion(&test_messages, &[]).await {
        Ok(_) => Ok(true),
        Err(e) => Err(format!("LLM validation failed: {e}")),
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn test_db() -> SqlitePool {
        let db = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE settings (
                key TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL DEFAULT '',
                updated_at TEXT NOT NULL DEFAULT ''
            )",
        )
        .execute(&db)
        .await
        .unwrap();
        db
    }

    #[tokio::test]
    async fn fresh_load_waits_for_late_legacy_state() {
        let db = test_db().await;
        // Startup before initial sync: no legacy row yet. Loading must NOT
        // write the v2 marker, otherwise the row that syncs in later is
        // ignored and the stale cursor race returns.
        let empty = load_weixin_state(&db).await.unwrap();
        assert!(empty.get_updates_buf.is_empty());
        assert!(get_setting(&db, REMOTE_WEIXIN_V2_KEY).await.unwrap().is_none());

        sqlx::query("INSERT INTO settings (key, value, updated_at) VALUES (?, ?, 't')")
            .bind(REMOTE_WEIXIN_STATE_KEY)
            .bind(
                serde_json::json!({
                    "get_updates_buf": "cursor-from-office",
                    "context_tokens": {}
                })
                .to_string(),
            )
            .execute(&db)
            .await
            .unwrap();
        let loaded = load_weixin_state(&db).await.unwrap();
        assert_eq!(loaded.get_updates_buf, "cursor-from-office");
        assert!(get_setting(&db, REMOTE_WEIXIN_V2_KEY).await.unwrap().is_some());
        db.close().await;
    }

    #[tokio::test]
    async fn weixin_state_migrates_and_split_writes_do_not_clobber() {
        let db = test_db().await;
        let legacy = serde_json::json!({
            "get_updates_buf": "cursor-old",
            "context_tokens": {"chat-1": "token-1"}
        });
        sqlx::query(
            "INSERT INTO settings (key, value, updated_at) VALUES (?, ?, 't')",
        )
        .bind(REMOTE_WEIXIN_STATE_KEY)
        .bind(legacy.to_string())
        .execute(&db)
        .await
        .unwrap();

        let loaded = load_weixin_state(&db).await.unwrap();
        assert_eq!(loaded.get_updates_buf, "cursor-old");
        assert_eq!(loaded.context_tokens.get("chat-1").map(String::as_str), Some("token-1"));
        assert!(get_setting(&db, REMOTE_WEIXIN_CURSOR_KEY).await.unwrap().is_some());
        assert!(get_setting(&db, REMOTE_WEIXIN_V2_KEY).await.unwrap().is_some());

        // A cursor-only write leaves tokens untouched.
        save_weixin_cursor(&db, "cursor-new").await.unwrap();
        let loaded = load_weixin_state(&db).await.unwrap();
        assert_eq!(loaded.get_updates_buf, "cursor-new");
        assert_eq!(loaded.context_tokens.get("chat-1").map(String::as_str), Some("token-1"));

        // A token-only write leaves the cursor untouched.
        let mut tokens = loaded.context_tokens.clone();
        tokens.insert("chat-2".to_string(), "token-2".to_string());
        save_weixin_context_tokens(&db, &tokens).await.unwrap();
        let loaded = load_weixin_state(&db).await.unwrap();
        assert_eq!(loaded.get_updates_buf, "cursor-new");
        assert_eq!(loaded.context_tokens.get("chat-2").map(String::as_str), Some("token-2"));

        // A fresh binding clears both and must not resurrect the legacy row.
        save_weixin_state(&db, &Default::default()).await.unwrap();
        let loaded = load_weixin_state(&db).await.unwrap();
        assert!(loaded.get_updates_buf.is_empty());
        assert!(loaded.context_tokens.is_empty());
        db.close().await;
    }
}
