use crate::core::settings_service;
use crate::sync::onboarding::ensure_device_id;
use crate::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;
use tracing::info;

// ── Account state persisted in the global settings table ──────────────────

pub const ACCOUNT_RELAY_URL_KEY: &str = "account.relay_url";
pub const ACCOUNT_TOKEN_KEY: &str = "account.token";
pub const ACCOUNT_REFRESH_TOKEN_KEY: &str = "account.refresh_token";
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

/// Decode the `exp` claim from a JWT without verifying the signature.
/// Returns the Unix timestamp, or None if the token is malformed.
fn decode_jwt_exp(token: &str) -> Option<i64> {
    let payload_b64 = token.split('.').nth(1)?;
    use base64::Engine as _;
    let padded = match payload_b64.len() % 4 {
        0 => payload_b64.to_string(),
        r => format!("{}{}", payload_b64, "=".repeat(4 - r)),
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(padded)
        .ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    claims.get("exp")?.as_i64()
}

/// Clear locally stored account credentials.
pub async fn clear_account_credentials(db: &sqlx::SqlitePool) {
    let _ = settings_service::set_device_setting(db, ACCOUNT_TOKEN_KEY, "").await;
    let _ = settings_service::set_device_setting(db, ACCOUNT_REFRESH_TOKEN_KEY, "").await;
    let _ = settings_service::set_device_setting(db, ACCOUNT_USER_ID_KEY, "").await;
    let _ = settings_service::set_device_setting(db, ACCOUNT_EMAIL_KEY, "").await;
}

/// Refresh the access token using the stored refresh token.
/// On success returns the new access token and persists both tokens.
/// On definitive auth failure (invalid refresh token) it clears credentials.
pub async fn refresh_access_token(
    db: &sqlx::SqlitePool,
    relay_url: &str,
) -> Result<String, String> {
    let refresh_token = settings_service::get_device_setting(db, ACCOUNT_REFRESH_TOKEN_KEY)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    if refresh_token.is_empty() {
        return Err("登录已过期，请重新登录".to_string());
    }

    let base_url = crate::sync::onboarding::normalize_http_base(relay_url);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let resp = client
        .post(format!("{}/api/refresh", base_url.trim_end_matches('/')))
        .json(&serde_json::json!({ "refresh_token": refresh_token }))
        .send()
        .await
        .map_err(|e| format!("刷新登录状态失败: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        clear_account_credentials(db).await;
        return Err("登录已过期，请重新登录".to_string());
    }
    if !status.is_success() {
        return Err(format!("刷新登录状态失败: {} {}", status, text.chars().take(200).collect::<String>()));
    }
    let json: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("parse refresh response: {e}"))?;
    let access_token = json
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or("refresh response missing access_token")?
        .to_string();
    let new_refresh_token = json
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .unwrap_or(&refresh_token)
        .to_string();
    settings_service::set_device_setting(db, ACCOUNT_TOKEN_KEY, &access_token).await?;
    settings_service::set_device_setting(db, ACCOUNT_REFRESH_TOKEN_KEY, &new_refresh_token).await?;
    Ok(access_token)
}

/// Check whether the stored access token is expired or about to expire.
/// Returns true when there is a token and it is valid for at least 5 minutes.
pub async fn access_token_is_fresh(db: &sqlx::SqlitePool) -> bool {
    let token = settings_service::get_device_setting(db, ACCOUNT_TOKEN_KEY)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    if token.is_empty() {
        return false;
    }
    let Some(exp) = decode_jwt_exp(&token) else {
        return false;
    };
    let now = chrono::Utc::now().timestamp();
    // Treat as stale if it expires within 5 minutes.
    exp > now + 300
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
    let refresh_token = resp
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .ok_or("login response missing refresh_token")?
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
    settings_service::set_device_setting(db, ACCOUNT_REFRESH_TOKEN_KEY, &refresh_token).await?;
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
    let _ = settings_service::set_device_setting(db, ACCOUNT_REFRESH_TOKEN_KEY, "").await;
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

async fn stored_access_token(db: &sqlx::SqlitePool) -> Result<String, String> {
    settings_service::get_device_setting(db, ACCOUNT_TOKEN_KEY)
        .await
        .ok()
        .flatten()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "登录已过期，请重新登录".to_string())
}

async fn auth_request(
    state: &AppState,
    relay_url: &str,
    method: reqwest::Method,
    path: &str,
    body: Option<serde_json::Value>,
) -> Result<(reqwest::StatusCode, String), String> {
    let base_url = crate::sync::onboarding::normalize_http_base(relay_url);
    let token = stored_access_token(&state.db).await?;
    let url = format!("{}/{}", base_url.trim_end_matches('/'), path);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let mut req = client.request(method.clone(), url).header("Authorization", format!("Bearer {token}"));
    if let Some(ref b) = body {
        req = req.json(b);
    }
    let resp = req.send().await.map_err(|e| format!("request failed: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();

    if status == reqwest::StatusCode::UNAUTHORIZED {
        // Access token expired: try to refresh once and retry the original request.
        let new_token = refresh_access_token(&state.db, relay_url).await?;
        let mut req2 = client
            .request(method, format!("{}/{}", base_url.trim_end_matches('/'), path))
            .header("Authorization", format!("Bearer {new_token}"));
        if let Some(ref b) = body {
            req2 = req2.json(b);
        }
        let resp2 = req2.send().await.map_err(|e| format!("request failed: {e}"))?;
        let status2 = resp2.status();
        let text2 = resp2.text().await.unwrap_or_default();
        if status2 == reqwest::StatusCode::UNAUTHORIZED {
            return Err("登录已过期，请重新登录".to_string());
        }
        if !status2.is_success() {
            return Err(format!("{}: {}", status2, text2.chars().take(200).collect::<String>()));
        }
        return Ok((status2, text2));
    }

    if !status.is_success() {
        return Err(format!("{}: {}", status, text.chars().take(200).collect::<String>()));
    }
    Ok((status, text))
}

/// List the account's devices (requires a valid token).
#[tauri::command]
pub async fn device_list(
    state: State<'_, AppState>,
    relay_url: String,
) -> Result<Vec<DeviceRow>, String> {
    let (_, text) = auth_request(&state, &relay_url, reqwest::Method::GET, "api/devices", None).await?;
    let rows: Vec<serde_json::Value> = serde_json::from_str(&text).map_err(|e| format!("parse: {e}"))?;
    Ok(rows
        .into_iter()
        .map(|r| DeviceRow {
            device_id: r.get("device_id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            name: r.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            online: r.get("online").and_then(|v| v.as_bool()).unwrap_or(false),
        })
        .collect())
}

/// Remove a device: its row is deleted on the relay, so its tokens die
/// immediately and it disappears from the device list. Re-login on that
/// device simply registers it again.
#[tauri::command]
pub async fn device_remove(
    state: State<'_, AppState>,
    relay_url: String,
    device_id: String,
) -> Result<(), String> {
    auth_request(
        &state,
        &relay_url,
        reqwest::Method::DELETE,
        &format!("api/devices/{}", device_id),
        None,
    )
    .await?;
    Ok(())
}

/// Rename a device.
#[tauri::command]
pub async fn device_rename(
    state: State<'_, AppState>,
    relay_url: String,
    device_id: String,
    name: String,
) -> Result<(), String> {
    auth_request(
        &state,
        &relay_url,
        reqwest::Method::PATCH,
        &format!("api/devices/{}", device_id),
        Some(serde_json::json!({ "name": name })),
    )
    .await?;
    Ok(())
}

// ── 云端存储配额 / 扩容订单 ────────────────────────────────────────────────

/// 云端存储用量与配额（GET /api/storage 的响应）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageStatus {
    pub used_bytes: i64,
    pub quota_bytes: i64,
    /// 当前套餐 id（free/plus/pro/max；管理员直调配额时为 "custom"）。
    #[serde(default)]
    pub plan_id: Option<String>,
    /// 配额到期时间（RFC3339）；None = 默认免费额度或永久配额。
    #[serde(default)]
    pub expires_at: Option<String>,
}

/// 可选购的扩容套餐（GET /api/plans 的数组元素）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoragePlan {
    pub id: String,
    pub name: String,
    pub quota_bytes: i64,
    pub monthly_cny: f64,
    pub yearly_cny: f64,
}

/// 创建扩容订单的结果（POST /api/storage/orders 的响应）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageOrderCreateResult {
    pub order_id: String,
    pub plan_id: String,
    pub quota_bytes: i64,
    pub duration_days: u32,
    pub amount_cny: f64,
    /// 订单状态：pending / paid / rejected / cancelled。
    pub status: String,
    /// 收款说明（relay 的 RELAY_PAYMENT_INFO），前端随订单号一起展示，
    /// 提示用户转账备注填订单号。
    pub payment_info: String,
}

/// 扩容订单（GET /api/storage/orders 的数组元素）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageOrder {
    pub id: String,
    pub plan_id: String,
    pub quota_bytes: i64,
    pub duration_days: u32,
    pub amount_cny: f64,
    /// 订单状态：pending / paid / rejected / cancelled。
    pub status: String,
    pub created_at: String,
    #[serde(default)]
    pub paid_at: Option<String>,
    #[serde(default)]
    pub admin_note: Option<String>,
}

/// 查询当前账号的云端存储用量与配额。
#[tauri::command]
pub async fn storage_status(
    state: State<'_, AppState>,
    relay_url: String,
) -> Result<StorageStatus, String> {
    let (_, text) =
        auth_request(&state, &relay_url, reqwest::Method::GET, "api/storage", None).await?;
    serde_json::from_str(&text).map_err(|e| format!("parse: {e}"))
}

/// 列出可选购的扩容套餐。
#[tauri::command]
pub async fn storage_plans(
    state: State<'_, AppState>,
    relay_url: String,
) -> Result<Vec<StoragePlan>, String> {
    let (_, text) =
        auth_request(&state, &relay_url, reqwest::Method::GET, "api/plans", None).await?;
    serde_json::from_str(&text).map_err(|e| format!("parse: {e}"))
}

/// 提交扩容申请（创建 pending 订单）。period 为 "month" 或 "year"；同账号
/// 已有 pending 订单时 relay 幂等返回原订单，不会重复创建。
#[tauri::command]
pub async fn storage_order_create(
    state: State<'_, AppState>,
    relay_url: String,
    plan_id: String,
    period: String,
) -> Result<StorageOrderCreateResult, String> {
    let (_, text) = auth_request(
        &state,
        &relay_url,
        reqwest::Method::POST,
        "api/storage/orders",
        Some(serde_json::json!({ "plan_id": plan_id, "period": period })),
    )
    .await?;
    serde_json::from_str(&text).map_err(|e| format!("parse: {e}"))
}

/// 列出当前账号的扩容订单（含审核状态）。
#[tauri::command]
pub async fn storage_order_list(
    state: State<'_, AppState>,
    relay_url: String,
) -> Result<Vec<StorageOrder>, String> {
    let (_, text) =
        auth_request(&state, &relay_url, reqwest::Method::GET, "api/storage/orders", None).await?;
    serde_json::from_str(&text).map_err(|e| format!("parse: {e}"))
}
