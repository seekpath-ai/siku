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
    // Legacy app-level default pointer (superseded by is_default, but old
    // settings rows may still carry it).
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

    // Scrub dangling references from sessions: build_agent_config would warn
    // and fall back, but leaving the dead id around keeps the model badge
    // showing nothing checked forever. chat_sessions is a CRR table, so the
    // UPDATE propagates to synced devices (which may never have had this
    // provider and would otherwise hold a dangling id from the start).
    let rows = sqlx::query_as::<_, (String, String)>(
        "SELECT id, llm_provider_ids FROM chat_sessions WHERE llm_provider_ids LIKE ?",
    )
    .bind(format!("%{id}%"))
    .fetch_all(db)
    .await
    .map_err(|e| format!("db: {e}"))?;
    for (session_id, ids_json) in rows {
        if let Ok(mut ids) = serde_json::from_str::<Vec<String>>(&ids_json) {
            let before = ids.len();
            ids.retain(|x| x != id);
            if ids.len() != before {
                let new_json = serde_json::to_string(&ids).map_err(|e| format!("json: {e}"))?;
                sqlx::query("UPDATE chat_sessions SET llm_provider_ids = ? WHERE id = ?")
                    .bind(&new_json)
                    .bind(&session_id)
                    .execute(db)
                    .await
                    .map_err(|e| format!("db: {e}"))?;
            }
        }
    }
    // The vision-model column references the same pool with a single id —
    // NULL it out so image turns fall back to the clear "配置视觉模型" path
    // instead of a resolve error against a provider that no longer exists.
    sqlx::query("UPDATE chat_sessions SET vision_provider_id = NULL WHERE vision_provider_id = ?")
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;

    // If the deleted provider was the local default, promote the first
    // remaining one so the badge/config UI keeps a coherent default.
    let _ = ensure_local_default_provider(db).await;

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

/// DeepSeek 搜索引擎未单独配置 API Key 时的回退：取 LLM 提供商池中
/// provider = deepseek 的条目（优先默认项）的 api_key。
pub async fn deepseek_fallback_key(db: &SqlitePool) -> Result<Option<String>, String> {
    sqlx::query_scalar::<_, String>(
        "SELECT api_key FROM llm_providers \
         WHERE lower(provider) = 'deepseek' AND trim(api_key) != '' \
         ORDER BY is_default DESC, sort_order, created_at LIMIT 1"
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

/// Stable identity for a provider configuration.
///
/// Used ONLY by the one-time sync migration (and legacy seeding) so two devices
/// that configured the same endpoint/model converge on one row. Providers
/// created after the migration keep random UUIDs, which lets two accounts on
/// the same endpoint/model coexist.
fn hash_uuid(input: &str) -> String {
    // Deliberately SHA-256 instead of the uuid crate's v5 feature: the id is
    // opaque and only needs to be stable, and this avoids adding a dependency
    // (uuid's v5 pulls sha1_smol). Format it as a UUID so the rest of the code
    // keeps treating ids uniformly.
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(input.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50; // version nibble (informational)
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // RFC 4122 variant
    uuid::Uuid::from_bytes(bytes).to_string()
}

fn normalized_identity(provider: &str, base_url: &str, model: &str) -> String {
    format!(
        "{}|{}|{}",
        provider.trim().to_lowercase(),
        base_url.trim().trim_end_matches('/'),
        model.trim()
    )
}

pub(crate) fn deterministic_id(provider: &str, base_url: &str, model: &str) -> String {
    hash_uuid(&normalized_identity(provider, base_url, model))
}

/// Deterministic id for a provider that originated from an inline session
/// LLM block. The key is part of the hash so two different accounts on the
/// same endpoint/model stay distinct, while the same inline config on two
/// devices converges on one provider row.
pub(crate) fn deterministic_id_for_key(
    provider: &str,
    base_url: &str,
    model: &str,
    api_key: &str,
) -> String {
    hash_uuid(&format!(
        "{}|key:{}",
        normalized_identity(provider, base_url, model),
        api_key
    ))
}

/// Ensure keyed inline LLM blocks live in the provider pool and return their
/// ids. Blocks with an empty key are left inline (they mean "use the global
/// default"). This is what lets sessions stop persisting inline API keys.
pub async fn ensure_blocks_as_providers(
    db: &SqlitePool,
    blocks: &[LlmConfigBlock],
) -> Result<Vec<String>, String> {
    let mut ids = Vec::new();
    for block in blocks {
        if block.api_key.trim().is_empty() {
            continue;
        }
        let provider = block.provider.trim();
        let model = block.model.trim();
        let base_url = block.base_url.trim();
        let existing: Option<String> = sqlx::query_scalar(
            "SELECT id FROM llm_providers              WHERE lower(provider) = lower(?) AND trim(model) = ?                AND rtrim(trim(base_url), '/') = rtrim(trim(?), '/') AND api_key = ?              LIMIT 1",
        )
        .bind(provider)
        .bind(model)
        .bind(base_url)
        .bind(&block.api_key)
        .fetch_optional(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
        let id = match existing {
            Some(id) => id,
            None => {
                let id = deterministic_id_for_key(provider, base_url, model, &block.api_key);
                let present: Option<String> =
                    sqlx::query_scalar("SELECT id FROM llm_providers WHERE id = ?")
                        .bind(&id)
                        .fetch_optional(db)
                        .await
                        .map_err(|e| format!("db: {e}"))?;
                if present.is_none() {
                    let now = now_iso();
                    let name = format!("{provider} / {model}");
                    let extra_body = block
                        .extra_body
                        .as_ref()
                        .map(|v| serde_json::to_string(v).unwrap_or_default());
                    sqlx::query(
                        "INSERT INTO llm_providers                          (id, name, provider, model, api_key, base_url, proxy, max_tokens,                           temperature, extra_body, is_default, is_vision, sort_order, created_at, updated_at)                          VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?, 100, ?, ?)",
                    )
                    .bind(&id)
                    .bind(&name)
                    .bind(provider)
                    .bind(model)
                    .bind(&block.api_key)
                    .bind(base_url)
                    .bind(&block.proxy)
                    .bind(block.max_tokens.unwrap_or(4096))
                    .bind(block.temperature.map(|t| t as f64).unwrap_or(0.7))
                    .bind(&extra_body)
                    .bind(if block.is_vision.unwrap_or(false) { 1 } else { 0 })
                    .bind(&now)
                    .bind(&now)
                    .execute(db)
                    .await
                    .map_err(|e| format!("db: {e}"))?;
                }
                id
            }
        };
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    Ok(ids)
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
        is_vision: p.is_vision,
        // No per-provider column for this yet: auto (DeepSeek endpoint on).
        reasoning_passthrough: None,
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

/// Load the LLM config for a specific provider id; `None`/empty means the
/// default (starred) provider — the "跟随默认" behaviour of the batch
/// features (AI 表格列 / AI 综述).
pub async fn load_llm_config_for(
    db: &SqlitePool,
    provider_id: Option<&str>,
) -> Result<crate::ai::llm::LlmConfig, String> {
    match provider_id.filter(|s| !s.trim().is_empty()) {
        Some(id) => Ok(provider_to_block(&get_provider(db, id).await?).to_llm_config()),
        None => load_default_llm_config(db).await,
    }
}

async fn clear_default(db: &SqlitePool) -> Result<(), String> {
    sqlx::query("UPDATE llm_providers SET is_default = 0")
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    Ok(())
}

/// After sync (or on startup) a device may have providers but no local
/// default star — `is_default` is device-local and thus never synced. Star the
/// first provider so the model badge/config UI has a coherent default; runtime
/// `get_default_provider` already falls back to the first row.
pub async fn ensure_local_default_provider(db: &SqlitePool) -> Result<(), String> {
    let has_default: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM llm_providers WHERE is_default = 1")
            .fetch_one(db)
            .await
            .map_err(|e| format!("db: {e}"))?;
    if has_default > 0 {
        return Ok(());
    }
    let first: Option<String> = sqlx::query_scalar(
        "SELECT id FROM llm_providers ORDER BY sort_order, created_at LIMIT 1",
    )
    .fetch_optional(db)
    .await
    .map_err(|e| format!("db: {e}"))?;
    if let Some(id) = first {
        sqlx::query("UPDATE llm_providers SET is_default = 1 WHERE id = ?")
            .bind(&id)
            .execute(db)
            .await
            .map_err(|e| format!("db: {e}"))?;
        info!(provider_id = %id, "starred first synced llm provider as local default");
    }
    Ok(())
}

/// Seed providers from legacy settings if the table is empty.
async fn ensure_seeded(db: &SqlitePool) -> Result<(), String> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM llm_providers")
        .fetch_one(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    if count > 0 {
        // Record that seeding is done/pointless: without the marker, deleting
        // EVERY provider resurrected the legacy-settings entry on the next
        // read — the table being empty is a user decision, not a seed cue.
        if settings_service::get_setting(db, "llm_providers.seeded")
            .await?
            .is_none()
        {
            settings_service::set_setting(db, "llm_providers.seeded", "1").await?;
        }
        return Ok(());
    }
    if settings_service::get_setting(db, "llm_providers.seeded")
        .await?
        .is_some()
    {
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
    // 与 LlmConfig::default / agent config 一致的默认额度：思考模型推理与
    // 正文共享该额度，4096 过低；32768 是多数 provider 校验能容纳的上限。
    let max_tokens: i32 = settings_service::get_setting(db, &format!("{prefix}max_tokens"))
        .await?
        .and_then(|v| v.parse().ok())
        .unwrap_or(32768);
    let temperature: f64 = settings_service::get_setting(db, &format!("{prefix}temperature"))
        .await?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.7);

    // Deterministic so a legacy seed on device B merges with the migrated row
    // on device A instead of creating a duplicate.
    let id = deterministic_id(&provider, &base_url, &model);
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
    settings_service::set_setting(db, "llm_providers.seeded", "1").await?;
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

#[cfg(test)]
mod tests {
    use super::{
        deterministic_id, deterministic_id_for_key, ensure_blocks_as_providers,
    };
    use crate::ai::agent::config::LlmConfigBlock;
    use crate::core::db::SCHEMA_INIT_SQL;
    use sqlx::sqlite::SqlitePoolOptions;

    /// Same endpoint/model configured with different casing / trailing slash
    /// must derive the same id; a different model must not collide.
    #[test]
    fn deterministic_id_normalizes_identity() {
        let a = deterministic_id("DeepSeek", "https://api.deepseek.com/v1/", "deepseek-chat");
        let b = deterministic_id("deepseek", "https://api.deepseek.com/v1", "deepseek-chat");
        assert_eq!(a, b);
        assert_eq!(a.len(), 36, "must stay UUID-shaped");
        assert_ne!(a, deterministic_id("deepseek", "https://api.deepseek.com/v1", "deepseek-reasoner"));
    }

    fn block(provider: &str, model: &str, api_key: &str) -> LlmConfigBlock {
        LlmConfigBlock {
            provider: provider.to_string(),
            model: model.to_string(),
            api_key: api_key.to_string(),
            base_url: "https://api.deepseek.com/v1".to_string(),
            proxy: None,
            max_tokens: Some(4096),
            temperature: Some(0.7),
            extra_body: None,
            is_vision: None,
            reasoning_passthrough: None,
        }
    }

    /// Keyed inline blocks become pool rows with a stable id; empty keys are
    /// skipped; a different key on the same endpoint/model stays distinct.
    #[tokio::test]
    async fn keyed_blocks_become_pool_providers_idempotently() {
        let db = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(SCHEMA_INIT_SQL).execute(&db).await.unwrap();

        let blocks = vec![
            block("deepseek", "deepseek-chat", "sk-a"),
            block("deepseek", "deepseek-chat", ""),
            block("openai", "gpt-4o", "sk-b"),
        ];
        let ids = ensure_blocks_as_providers(&db, &blocks).await.unwrap();
        assert_eq!(ids.len(), 2, "empty-key blocks must be skipped");
        assert_eq!(
            ids[0],
            deterministic_id_for_key(
                "deepseek",
                "https://api.deepseek.com/v1",
                "deepseek-chat",
                "sk-a"
            )
        );

        let again = ensure_blocks_as_providers(&db, &blocks).await.unwrap();
        assert_eq!(ids, again, "repeat conversion must reuse the same providers");
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM llm_providers")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(count, 2);

        let other = ensure_blocks_as_providers(
            &db,
            &[block("deepseek", "deepseek-chat", "sk-c")],
        )
        .await
        .unwrap();
        assert_eq!(other.len(), 1);
        assert_ne!(other[0], ids[0], "a different key is a different account");
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM llm_providers")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(count, 3);

        let (key, provider): (String, String) =
            sqlx::query_as("SELECT api_key, provider FROM llm_providers WHERE id = ?")
                .bind(&ids[0])
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(key, "sk-a");
        assert_eq!(provider, "deepseek");
        db.close().await;
    }
}
