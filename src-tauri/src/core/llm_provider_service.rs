use sqlx::SqlitePool;
use tracing::{info, instrument};
use uuid::Uuid;

use crate::ai::agent::config::LlmConfigBlock;
use crate::core::models::LlmProvider;
use crate::core::settings_service;
use crate::core::time::now_iso;

/// List all LLM providers, default first, then by sort_order.
#[instrument(skip(db))]
pub async fn list_providers(db: &SqlitePool) -> Result<Vec<LlmProvider>, String> {
    ensure_seeded(db).await?;
    sqlx::query_as::<_, LlmProvider>(
        "SELECT id, name, provider, model, api_key, base_url, proxy, max_tokens, temperature, \
         extra_body, is_default, is_vision, sort_order, created_at, updated_at \
         FROM llm_providers \
         ORDER BY is_default DESC, sort_order, created_at"
    )
    .fetch_all(db)
    .await
    .map_err(|e| format!("db: {e}"))
}

/// Get a single provider.
#[instrument(skip(db))]
pub async fn get_provider(db: &SqlitePool, id: &str) -> Result<LlmProvider, String> {
    sqlx::query_as::<_, LlmProvider>(
        "SELECT id, name, provider, model, api_key, base_url, proxy, max_tokens, temperature, \
         extra_body, is_default, is_vision, sort_order, created_at, updated_at \
         FROM llm_providers WHERE id = ?"
    )
    .bind(id)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("db: {e}"))?
    .ok_or_else(|| format!("provider not found: {id}"))
}

/// Resolve a provider into an LLM config block.
#[instrument(skip(db))]
pub async fn resolve_block(db: &SqlitePool, id: &str) -> Result<LlmConfigBlock, String> {
    let p = get_provider(db, id).await?;
    Ok(provider_to_block(&p))
}

/// Create a new provider.
#[instrument(skip(db))]
pub async fn create_provider(
    db: &SqlitePool,
    input: LlmProviderInput,
) -> Result<LlmProvider, String> {
    let id = Uuid::new_v4().to_string();
    let now = now_iso();

    let sort_order: i32 = sqlx::query_scalar::<_, i32>(
        "SELECT COALESCE(MAX(sort_order), 0) + 1 FROM llm_providers"
    )
    .fetch_one(db)
    .await
    .map_err(|e| format!("db: {e}"))?;

    if input.is_default.unwrap_or(false) {
        clear_default(db).await?;
    }

    sqlx::query(
        "INSERT INTO llm_providers \
         (id, name, provider, model, api_key, base_url, proxy, max_tokens, temperature, extra_body, is_default, is_vision, sort_order, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
    )
    .bind(&id)
    .bind(&input.name)
    .bind(&input.provider)
    .bind(&input.model)
    .bind(&input.api_key)
    .bind(&input.base_url)
    .bind(&input.proxy)
    .bind(input.max_tokens)
    .bind(input.temperature)
    .bind(&input.extra_body)
    .bind(if input.is_default.unwrap_or(false) { 1 } else { 0 })
    .bind(if input.is_vision.unwrap_or(false) { 1 } else { 0 })
    .bind(sort_order)
    .bind(&now)
    .bind(&now)
    .execute(db)
    .await
    .map_err(|e| format!("db: {e}"))?;

    info!(provider_id = %id, "created llm provider");
    get_provider(db, &id).await
}

/// Update a provider.
#[instrument(skip(db))]
pub async fn update_provider(
    db: &SqlitePool,
    id: &str,
    input: LlmProviderInput,
) -> Result<LlmProvider, String> {
    if input.is_default == Some(true) {
        clear_default(db).await?;
    }

    let now = now_iso();
    sqlx::query(
        "UPDATE llm_providers SET \
            name = ?, provider = ?, model = ?, api_key = ?, base_url = ?, proxy = ?, \
            max_tokens = ?, temperature = ?, extra_body = ?, is_default = ?, is_vision = ?, updated_at = ? \
         WHERE id = ?"
    )
    .bind(&input.name)
    .bind(&input.provider)
    .bind(&input.model)
    .bind(&input.api_key)
    .bind(&input.base_url)
    .bind(&input.proxy)
    .bind(input.max_tokens)
    .bind(input.temperature)
    .bind(&input.extra_body)
    .bind(if input.is_default.unwrap_or(false) { 1 } else { 0 })
    .bind(if input.is_vision.unwrap_or(false) { 1 } else { 0 })
    .bind(&now)
    .bind(id)
    .execute(db)
    .await
    .map_err(|e| format!("db: {e}"))?;

    info!(provider_id = %id, "updated llm provider");
    get_provider(db, id).await
}

/// Delete a provider.
#[instrument(skip(db))]
pub async fn delete_provider(db: &SqlitePool, id: &str) -> Result<(), String> {
    // If this provider was the app-wide default, clear the stale reference.
    if let Ok(settings) = settings_service::load_app_settings(db).await {
        if settings.default_llm_provider_id.as_deref() == Some(id) {
            let mut updated = settings.clone();
            updated.default_llm_provider_id = None;
            let _ = settings_service::save_app_settings(db, &updated).await;
        }
    }

    sqlx::query("DELETE FROM llm_providers WHERE id = ?")
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    info!(provider_id = %id, "deleted llm provider");
    Ok(())
}

/// Get the default provider.
#[instrument(skip(db))]
pub async fn get_default_provider(db: &SqlitePool) -> Result<Option<LlmProvider>, String> {
    ensure_seeded(db).await?;
    let provider = sqlx::query_as::<_, LlmProvider>(
        "SELECT id, name, provider, model, api_key, base_url, proxy, max_tokens, temperature, \
         extra_body, is_default, is_vision, sort_order, created_at, updated_at \
         FROM llm_providers WHERE is_default = 1 LIMIT 1"
    )
    .fetch_optional(db)
    .await
    .map_err(|e| format!("db: {e}"))?;

    if provider.is_some() {
        return Ok(provider);
    }

    // Fallback to first provider
    sqlx::query_as::<_, LlmProvider>(
        "SELECT id, name, provider, model, api_key, base_url, proxy, max_tokens, temperature, \
         extra_body, is_default, is_vision, sort_order, created_at, updated_at \
         FROM llm_providers \
         ORDER BY sort_order, created_at LIMIT 1"
    )
    .fetch_optional(db)
    .await
    .map_err(|e| format!("db: {e}"))
}

/// Set the default provider.
#[instrument(skip(db))]
pub async fn set_default_provider(db: &SqlitePool, id: &str) -> Result<LlmProvider, String> {
    clear_default(db).await?;
    sqlx::query("UPDATE llm_providers SET is_default = 1 WHERE id = ?")
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    get_provider(db, id).await
}

/// Convert a provider row into an agent LLM config block.
pub fn provider_to_block(p: &LlmProvider) -> LlmConfigBlock {
    LlmConfigBlock {
        provider: p.provider.clone(),
        model: p.model.clone(),
        api_key: p.api_key.clone(),
        base_url: p.base_url.clone(),
        proxy: p.proxy.clone(),
        max_tokens: p.max_tokens,
        temperature: p.temperature.map(|t| t as f32),
        extra_body: p.extra_body.as_ref().and_then(|s| serde_json::from_str(s).ok()),
    }
}

/// Build a runtime LLM config from the default provider.
#[instrument(skip(db))]
pub async fn load_default_llm_config(db: &SqlitePool) -> Result<crate::ai::llm::LlmConfig, String> {
    let provider = get_default_provider(db)
        .await?
        .ok_or_else(|| "no LLM provider configured".to_string())?;
    Ok(provider_to_block(&provider).to_llm_config())
}

async fn clear_default(db: &SqlitePool) -> Result<(), String> {
    sqlx::query("UPDATE llm_providers SET is_default = 0")
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    Ok(())
}

/// Seed providers from legacy settings if the table is empty.
async fn ensure_seeded(db: &SqlitePool) -> Result<(), String> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM llm_providers")
        .fetch_one(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    if count > 0 {
        return Ok(());
    }

    // Migrate from legacy llm.* settings — only when a legacy provider was
    // actually configured. A fresh install must NOT get a "deepseek (迁移)"
    // entry with defaulted values.
    let Some(provider) = settings_service::get_setting(db, "llm.provider").await? else {
        return Ok(());
    };
    let provider = provider.to_lowercase();
    let prefix = format!("llm.{provider}.");

    let api_key = settings_service::get_setting(db, &format!("{prefix}api_key"))
        .await?
        .unwrap_or_default();
    let base_url = settings_service::get_setting(db, &format!("{prefix}base_url"))
        .await?
        .unwrap_or_else(|| default_base_url(&provider));
    let model = settings_service::get_setting(db, &format!("{prefix}model"))
        .await?
        .unwrap_or_else(|| default_model(&provider));
    let proxy = settings_service::get_setting(db, "llm.proxy").await?;
    let max_tokens: i32 = settings_service::get_setting(db, &format!("{prefix}max_tokens"))
        .await?
        .and_then(|v| v.parse().ok())
        .unwrap_or(4096);
    let temperature: f64 = settings_service::get_setting(db, &format!("{prefix}temperature"))
        .await?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.7);

    let id = Uuid::new_v4().to_string();
    let now = now_iso();
    let name = format!("{} (迁移)", provider);

    sqlx::query(
        "INSERT INTO llm_providers \
         (id, name, provider, model, api_key, base_url, proxy, max_tokens, temperature, is_default, sort_order, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 1, 0, ?, ?)"
    )
    .bind(&id)
    .bind(&name)
    .bind(&provider)
    .bind(&model)
    .bind(&api_key)
    .bind(&base_url)
    .bind(&proxy)
    .bind(max_tokens)
    .bind(temperature)
    .bind(&now)
    .bind(&now)
    .execute(db)
    .await
    .map_err(|e| format!("db: {e}"))?;

    info!(provider_id = %id, "seeded default llm provider from legacy settings");
    Ok(())
}

fn default_base_url(provider: &str) -> String {
    match provider {
        "openai" => "https://api.openai.com/v1",
        "anthropic" => "https://api.anthropic.com",
        "deepseek" => "https://api.deepseek.com/v1",
        "siliconflow" => "https://api.siliconflow.cn/v1",
        "ollama" => "http://localhost:11434/v1",
        "qwen" => "https://dashscope.aliyuncs.com/compatible-mode/v1",
        "zhipu" => "https://open.bigmodel.cn/api/paas/v4",
        "kimi" => "https://api.moonshot.cn/v1",
        "gemini" => "https://generativelanguage.googleapis.com/v1beta/openai",
        _ => "https://api.deepseek.com/v1",
    }
    .to_string()
}

fn default_model(provider: &str) -> String {
    match provider {
        "openai" => "gpt-4o",
        "anthropic" => "claude-sonnet-4-20250514",
        "deepseek" => "deepseek-v4-flash",
        "siliconflow" => "deepseek-ai/DeepSeek-V3",
        "ollama" => "llama3.2",
        "qwen" => "qwen-max",
        "zhipu" => "glm-4-plus",
        "kimi" => "moonshot-v1-auto",
        "gemini" => "gemini-2.5-pro",
        _ => "deepseek-v4-flash",
    }
    .to_string()
}

/// Input for creating/updating an LLM provider.
#[derive(Clone, serde::Deserialize)]
pub struct LlmProviderInput {
    pub name: String,
    pub provider: String,
    pub model: String,
    pub api_key: String,
    pub base_url: String,
    pub proxy: Option<String>,
    pub max_tokens: Option<i32>,
    pub temperature: Option<f64>,
    pub extra_body: Option<String>,
    pub is_default: Option<bool>,
    pub is_vision: Option<bool>,
}

impl std::fmt::Debug for LlmProviderInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmProviderInput")
            .field("name", &self.name)
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("api_key", &crate::core::redact::redact_api_key(&self.api_key))
            .field("base_url", &self.base_url)
            .field("proxy", &self.proxy)
            .field("max_tokens", &self.max_tokens)
            .field("temperature", &self.temperature)
            .field("extra_body", &self.extra_body)
            .field("is_default", &self.is_default)
            .field("is_vision", &self.is_vision)
            .finish()
    }
}
