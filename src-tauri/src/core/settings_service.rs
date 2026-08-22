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
    let value = serde_json::to_string(settings).map_err(|e| format!("json: {e}"))?;
    set_setting(db, APP_SETTINGS_KEY, &value).await?;
    update_cached_settings(settings.clone());
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

/// Build LLM config from the unified provider pool.
pub async fn load_llm_config(db: &SqlitePool) -> Result<crate::ai::llm::LlmConfig, String> {
    crate::core::llm_provider_service::load_default_llm_config(db).await
}

/// Validate an LLM config by making a test request
pub async fn validate_llm_config(config: &crate::ai::llm::LlmConfig) -> Result<bool, String> {
    let client = crate::ai::llm::client::create_llm_client(config)?;

    let test_messages = vec![crate::ai::llm::ChatMessage {
        role: "user".to_string(),
        content: "Hello, respond with just 'ok'.".to_string(),
        tool_calls: None,
        tool_call_id: None,
        name: None,
    }];

    match client.chat_completion(&test_messages, &[]).await {
        Ok(_) => Ok(true),
        Err(e) => Err(format!("LLM validation failed: {e}")),
    }
}
