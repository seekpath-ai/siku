use crate::core::settings_service;
use crate::sync::onboarding::ensure_device_id;
use crate::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;
use tracing::info;

// ── Account state persisted in the global settings table ──────────────────

pub const ACCOUNT_RELAY_URL_KEY: &str = "account.relay_url";
pub const ACCOUNT_TOKEN_KEY: &str = "account.token";
pub const ACCOUNT_USER_ID_KEY: &str = "account.user_id";
pub const ACCOUNT_EMAIL_KEY: &str = "account.email";
pub const ACCOUNT_DEVICE_NAME_KEY: &str = "account.device_name";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthInfo {
    pub access_token: String,
    pub user_id: String,
    pub email: String,
    pub device_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceRow {
    pub device_id: String,
    pub name: String,
    pub revoked: bool,
    /// Live presence from the relay room membership (added in device_list).
    #[serde(default)]
    pub online: bool,
}

async fn http_post_json<T: Serialize, R: for<'de> Deserialize<'de>>(
    base_url: &str,
    path: &str,
    body: &T,
    bearer: Option<&str>,
) -> Result<R, String> {
    let base_url = crate::sync::onboarding::normalize_http_base(base_url);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let mut req = client.post(format!("{}/{}", base_url.trim_end_matches('/'), path)).json(body);
    if let Some(token) = bearer {
        req = req.header("Authorization", format!("Bearer {token}"));
    }
    let resp = req.send().await.map_err(|e| format!("request failed: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("{}: {}", status, text.chars().take(200).collect::<String>()));
    }
    serde_json::from_str(&text).map_err(|e| format!("parse response: {e}"))
}

/// Register a new account.
#[tauri::command]
pub async fn auth_register(
    state: State<'_, AppState>,
    relay_url: String,
    email: String,
    password: String,
) -> Result<(), String> {
    http_post_json::<_, serde_json::Value>(
        &relay_url,
        "api/register",
        &serde_json::json!({ "email": email, "password": password }),
        None,
    )
    .await?;
    info!("account registered");
    Ok(())
}

/// Log in and store the device token locally.
#[tauri::command]
pub async fn auth_login(
    state: State<'_, AppState>,
    sync_state: State<'_, crate::commands::sync::SyncState>,
    relay_url: String,
    email: String,
    password: String,
    device_name: String,
) -> Result<AuthInfo, String> {
    let device_id = ensure_device_id(&state.db).await.map_err(|e| e.to_string())?;
    let resp: serde_json::Value = http_post_json(
        &relay_url,
        "api/login",
        &serde_json::json!({
            "email": email,
            "password": password,
            "device_id": device_id,
            "device_name": device_name,
        }),
        None,
    )
    .await?;
    let access_token = resp
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or("login response missing access_token")?
        .to_string();
    let user_id = resp
        .get("user_id")
        .and_then(|v| v.as_str())
        .ok_or("login response missing user_id")?
        .to_string();

    // Persist account state in DEVICE-LOCAL settings: the `settings` table is
    // a syncable CRR, so storing the token / sync key there would replicate
    // credentials to every paired device and let last-writer-wins clobber
    // per-device secrets.
    let db = &state.db;
    settings_service::set_device_setting(db, ACCOUNT_RELAY_URL_KEY, &relay_url).await?;
    settings_service::set_device_setting(db, ACCOUNT_TOKEN_KEY, &access_token).await?;
    settings_service::set_device_setting(db, ACCOUNT_USER_ID_KEY, &user_id).await?;
    settings_service::set_device_setting(db, ACCOUNT_EMAIL_KEY, &email).await?;
    settings_service::set_device_setting(db, ACCOUNT_DEVICE_NAME_KEY, &device_name).await?;
    // Account-level sync key issued by the server: same for every device,
    // lets this device decrypt mailbox messages without pairing. Stored
    // device-locally; distribution happens through login / pairing, never
    // through the synced settings table.
    if let Some(key) = resp.get("sync_key").and_then(|v| v.as_str()) {
        if !key.is_empty() {
            let _ = settings_service::set_device_setting(
                db,
                crate::sync::onboarding::ACCOUNT_SYNC_KEY_SETTING,
                key,
            )
            .await;
        }
    }

    info!(user_id = %user_id, device_id = %device_id, "device logged in");

    // Login is the trust signal: start the account auto-sync proxy right here
    // (backend-side) so sync starts even if the frontend never triggers it.
    crate::commands::sync::spawn_auto_sync_proxy(&sync_state, &state, &relay_url, &access_token).await;

    Ok(AuthInfo {
        access_token,
        user_id,
        email,
        device_id,
    })
}

/// Log out: clear locally stored account state and stop all sync activity
/// (auto-sync proxy, host loop, current sessions).
#[tauri::command]
pub async fn auth_logout(
    state: State<'_, AppState>,
    sync_state: State<'_, crate::commands::sync::SyncState>,
) -> Result<(), String> {
    // Stop background sync before clearing credentials: the auto-sync proxy
    // and host loop stop via flags, and every live engine is stopped
    // explicitly (dropping the map references alone would leave the WebRTC
    // sessions and their mailbox transports running, still applying peer
    // data locally).
    tracing::info!("auth_logout: stopping all sync");
    crate::commands::sync::stop_all_sync(&sync_state).await;

    let db = &state.db;
    let _ = settings_service::set_device_setting(db, ACCOUNT_TOKEN_KEY, "").await;
    let _ = settings_service::set_device_setting(db, ACCOUNT_USER_ID_KEY, "").await;
    let _ = settings_service::set_device_setting(db, ACCOUNT_EMAIL_KEY, "").await;
    Ok(())
}

/// Suggest a default device name: local hostname + first 4 chars of the
/// device id, e.g. `DESKTOP-ABC1-1a2b`. Used as the fallback device name on
/// login so two machines never collide as plain "我的设备".
#[tauri::command]
pub async fn suggest_device_name(state: State<'_, AppState>) -> Result<String, String> {
    let device_id = ensure_device_id(&state.db).await.map_err(|e| e.to_string())?;
    let host = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "PC".to_string());
    let suffix = &device_id[..device_id.len().min(4)];
    Ok(format!("{host}-{suffix}"))
}

/// Current account state (empty strings when logged out). Reads device-local
/// settings — the token never lives in the syncable settings table.
#[tauri::command]
pub async fn auth_status(state: State<'_, AppState>) -> Result<AuthInfo, String> {
    let db = &state.db;
    let device_id = ensure_device_id(&db).await.map_err(|e| e.to_string())?;
    let token = settings_service::get_device_setting(db, ACCOUNT_TOKEN_KEY)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let user_id = settings_service::get_device_setting(db, ACCOUNT_USER_ID_KEY)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let email = settings_service::get_device_setting(db, ACCOUNT_EMAIL_KEY)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    Ok(AuthInfo {
        access_token: token,
        user_id,
        email,
        device_id,
    })
}

/// List the account's devices (requires a valid token).
#[tauri::command]
pub async fn device_list(relay_url: String, token: String) -> Result<Vec<DeviceRow>, String> {
    let relay_url = crate::sync::onboarding::normalize_http_base(&relay_url);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let resp = client
        .get(format!("{}/api/devices", relay_url.trim_end_matches('/')))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("{}: {}", status, text.chars().take(200).collect::<String>()));
    }
    let rows: Vec<serde_json::Value> = serde_json::from_str(&text).map_err(|e| format!("parse: {e}"))?;
    Ok(rows
        .into_iter()
        .map(|r| DeviceRow {
            device_id: r.get("device_id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            name: r.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            revoked: r.get("revoked").and_then(|v| v.as_bool()).unwrap_or(false),
            online: r.get("online").and_then(|v| v.as_bool()).unwrap_or(false),
        })
        .collect())
}

/// Revoke a device (kills its token on next use).
#[tauri::command]
pub async fn device_revoke(
    relay_url: String,
    token: String,
    device_id: String,
) -> Result<(), String> {
    let relay_url = crate::sync::onboarding::normalize_http_base(&relay_url);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let resp = client
        .delete(format!("{}/api/devices/{}", relay_url.trim_end_matches('/'), device_id))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("{}: {}", status, text.chars().take(200).collect::<String>()));
    }
    Ok(())
}

/// Rename a device.
#[tauri::command]
pub async fn device_rename(
    relay_url: String,
    token: String,
    device_id: String,
    name: String,
) -> Result<(), String> {
    let relay_url = crate::sync::onboarding::normalize_http_base(&relay_url);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let resp = client
        .patch(format!("{}/api/devices/{}", relay_url.trim_end_matches('/'), device_id))
        .header("Authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({ "name": name }))
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("{}: {}", status, text.chars().take(200).collect::<String>()));
    }
    Ok(())
}
