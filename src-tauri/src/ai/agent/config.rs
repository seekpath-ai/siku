//! Agent configuration types shared between backend and frontend.
//! Mirrors ShellAgent's shared AgentConfig / LlmConfig / ApprovalConfig.

use serde::{Deserialize, Serialize};

/// A single LLM provider configuration block.
/// Agents can have multiple blocks (model pool); the first block is active.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct LlmConfigBlock {
    pub provider: String,
    pub model: String,
    pub api_key: String,
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_body: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_vision: Option<bool>,
}

impl std::fmt::Debug for LlmConfigBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmConfigBlock")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("api_key", &crate::core::redact::redact_api_key(&self.api_key))
            .field("base_url", &self.base_url)
            .field("proxy", &self.proxy)
            .field("max_tokens", &self.max_tokens)
            .field("temperature", &self.temperature)
            .field("extra_body", &self.extra_body)
            .field("is_vision", &self.is_vision)
            .finish()
    }
}

impl Default for LlmConfigBlock {
    fn default() -> Self {
        Self {
            provider: "deepseek".to_string(),
            model: "deepseek-v4-flash".to_string(),
            api_key: String::new(),
            base_url: "https://api.deepseek.com/v1".to_string(),
            proxy: None,
            max_tokens: None,
            temperature: None,
            extra_body: None,
            is_vision: None,
        }
    }
}

impl LlmConfigBlock {
    /// Convert this block into the internal LLM client config.
    pub fn to_llm_config(&self) -> crate::ai::llm::LlmConfig {
        let provider = match self.provider.to_lowercase().as_str() {
            "openai" => crate::ai::llm::LlmProvider::OpenAI,
            "anthropic" => crate::ai::llm::LlmProvider::Anthropic,
            "deepseek" => crate::ai::llm::LlmProvider::DeepSeek,
            "siliconflow" => crate::ai::llm::LlmProvider::SiliconFlow,
            "ollama" => crate::ai::llm::LlmProvider::Ollama,
            "qwen" => crate::ai::llm::LlmProvider::Qwen,
            "zhipu" => crate::ai::llm::LlmProvider::Zhipu,
            "kimi" => crate::ai::llm::LlmProvider::Kimi,
            "gemini" => crate::ai::llm::LlmProvider::Gemini,
            _ => crate::ai::llm::LlmProvider::DeepSeek,
        };

        crate::ai::llm::LlmConfig {
            provider,
            api_key: self.api_key.clone(),
            base_url: self.base_url.clone(),
            model: self.model.clone(),
            proxy: self.proxy.clone(),
            max_tokens: self.max_tokens.unwrap_or(4096) as u32,
            temperature: self.temperature.unwrap_or(0.7),
            is_vision: self.is_vision.unwrap_or(false),
        }
    }
}

/// Tool approval strategy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    Auto,
    AutoExpireTime,
    AutoByRules,
    Manual,
    /// Like Manual but read-only tools also require approval — a strict
    /// mode to throttle runaway LLM exploration (every call is confirmed).
    ManualAll,
}

impl Default for ApprovalMode {
    fn default() -> Self {
        ApprovalMode::Auto
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ApprovalConfig {
    pub mode: ApprovalMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expire_sec: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub whitelist: Option<Vec<String>>,
}

impl ApprovalConfig {
    /// Determine whether a tool call should be auto-approved based on this config.
    pub fn is_auto_approved(&self, tool_name: &str, last_approval_elapsed_sec: Option<u64>) -> bool {
        match self.mode {
            ApprovalMode::Auto => true,
            ApprovalMode::Manual | ApprovalMode::ManualAll => false,
            ApprovalMode::AutoByRules => {
                let list = self.whitelist.as_deref().unwrap_or_default();
                list.iter().any(|w| tool_name == w || tool_name.starts_with(w.trim_end_matches('*')))
            }
            ApprovalMode::AutoExpireTime => {
                let expire = self.expire_sec.unwrap_or(60) as u64;
                last_approval_elapsed_sec.map(|e| e <= expire).unwrap_or(false)
            }
        }
    }
}

/// Full per-agent configuration.
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct AgentConfig {
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    pub llm_models: Vec<LlmConfigBlock>,
    /// `None` = all built-in tools (unset/legacy); `Some([])` = explicitly none.
    pub tools: Option<Vec<String>>,
    pub approval: ApprovalConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_loops: Option<i32>,
    /// Per-round output cap sent to the LLM API; None = follow the model config.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<i32>,
    /// Conversation context truncation budget; never sent to the API.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_budget: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_memory_rounds: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_file_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skills_dir: Option<String>,
}

impl std::fmt::Debug for AgentConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentConfig")
            .field("display_name", &self.display_name)
            .field("persona", &self.persona)
            .field("system_prompt", &self.system_prompt.as_ref().map(|s| s.len()))
            .field("llm_models", &self.llm_models)
            .field("tools", &self.tools)
            .field("approval", &self.approval)
            .field("max_loops", &self.max_loops)
            .field("max_tokens", &self.max_tokens)
            .field("context_budget", &self.context_budget)
            .field("max_memory_rounds", &self.max_memory_rounds)
            .field("memory_file_path", &self.memory_file_path)
            .field("memory_dir", &self.memory_dir)
            .field("skills_dir", &self.skills_dir)
            .finish()
    }
}

impl AgentConfig {
    /// Effective system prompt: system_prompt > persona > default.
    pub fn effective_system_prompt(&self) -> String {
        self.system_prompt
            .clone()
            .or_else(|| self.persona.clone())
            .unwrap_or_else(|| {
                "You are Siku (思库), an intelligent assistant for knowledge management and research. \
                 You help users manage their literature library, take notes, search for information, \
                 translate text, and manage files. Use the available tools when needed. \
                 Always respond in the same language as the user. Be concise and helpful."
                    .to_string()
            })
    }

    /// Effective LLM block (first one).
    pub fn active_llm(&self) -> &LlmConfigBlock {
        self.llm_models.first().unwrap_or_else(|| {
            static DEFAULT: std::sync::OnceLock<LlmConfigBlock> = std::sync::OnceLock::new();
            DEFAULT.get_or_init(LlmConfigBlock::default)
        })
    }

    /// Maximum agent-tool loops. `0` means "no limit" for practical purposes,
    /// but we cap it at a hard ceiling to prevent runaway executions.
    pub fn effective_max_loops(&self) -> usize {
        const HARD_LIMIT: i32 = 1000;
        let v = self.max_loops.unwrap_or(10);
        if v == 0 {
            return HARD_LIMIT as usize;
        }
        v.max(1).min(HARD_LIMIT) as usize
    }

    /// Memory rounds to load from disk. `0` means load the full conversation.
    pub fn effective_max_memory_rounds(&self) -> usize {
        self.max_memory_rounds.unwrap_or(10).max(0) as usize
    }

    /// Token budget for the conversation context. `0` means do not truncate.
    /// Never sent to the LLM API — the per-round output cap is `max_tokens`.
    pub fn effective_context_budget(&self) -> usize {
        self.context_budget.unwrap_or(28000).max(0) as usize
    }
}
