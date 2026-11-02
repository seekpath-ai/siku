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

/// Delete a setting
#[instrument(skip(db))]
pub async fn delete_setting(db: &SqlitePool, key: &str) -> Result<(), String> {
    sqlx::query("DELETE FROM settings WHERE key = ?")
        .bind(key)
        .execute(db)
        .await
        .map_err(|e| format!("db error: {e}"))?;

    Ok(())
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

pub const REMOTE_WEIXIN_STATE_KEY: &str = "remote_weixin_state";
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
pub async fn load_weixin_state(
    db: &SqlitePool,
) -> Result<crate::core::models::WeixinChannelState, String> {
    match get_setting(db, REMOTE_WEIXIN_STATE_KEY).await? {
        Some(v) => serde_json::from_str(&v).map_err(|e| format!("invalid weixin state: {e}")),
        None => Ok(Default::default()),
    }
}

pub async fn save_weixin_state(
    db: &SqlitePool,
    state: &crate::core::models::WeixinChannelState,
) -> Result<(), String> {
    let v = serde_json::to_string(state).map_err(|e| format!("json: {e}"))?;
    set_setting(db, REMOTE_WEIXIN_STATE_KEY, &v).await
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
pub async fn migrate_feishu_instance_channels(db: &SqlitePool) -> Result<(), String> {
    let remote = load_remote_config(db).await?;
    if remote.feishu_bots.len() != 1 {
        return Ok(());
    }
    let channel = format!("feishu:{}", remote.feishu_bots[0].app_id);
    sqlx::query("UPDATE remote_chats SET channel = ? WHERE channel = 'feishu'")
        .bind(&channel)
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
