pub mod client;
pub mod openai;
pub mod anthropic;
pub mod ollama;

use serde::{Deserialize, Serialize};

/// Supported LLM providers (all OpenAI-compatible except Anthropic/Ollama)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum LlmProvider {
    OpenAI,
    Anthropic,
    DeepSeek,
    SiliconFlow,
    Ollama,
    Qwen,
    Zhipu,
    Kimi,
    Gemini,
}

impl std::fmt::Display for LlmProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LlmProvider::OpenAI => write!(f, "openai"),
            LlmProvider::Anthropic => write!(f, "anthropic"),
            LlmProvider::DeepSeek => write!(f, "deepseek"),
            LlmProvider::SiliconFlow => write!(f, "siliconflow"),
            LlmProvider::Ollama => write!(f, "ollama"),
            LlmProvider::Qwen => write!(f, "qwen"),
            LlmProvider::Zhipu => write!(f, "zhipu"),
            LlmProvider::Kimi => write!(f, "kimi"),
            LlmProvider::Gemini => write!(f, "gemini"),
        }
    }
}

/// LLM provider configuration
#[derive(Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    pub provider: LlmProvider,
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub proxy: Option<String>,
    pub max_tokens: u32,
    pub temperature: f32,
    pub is_vision: bool,
}

impl std::fmt::Debug for LlmConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmConfig")
            .field("provider", &self.provider)
            .field("api_key", &crate::core::redact::redact_api_key(&self.api_key))
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("proxy", &self.proxy)
            .field("max_tokens", &self.max_tokens)
            .field("temperature", &self.temperature)
            .field("is_vision", &self.is_vision)
            .finish()
    }
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: LlmProvider::DeepSeek,
            api_key: String::new(),
            base_url: "https://api.deepseek.com/v1".to_string(),
            model: "deepseek-v4-flash".to_string(),
            // Deliberately no proxy default: a baked-in local proxy (the old
            // 127.0.0.1:7890 default) silently broke requests whenever that
            // proxy was not running. Proxy must come from explicit config.
            proxy: None,
            max_tokens: 4096,
            temperature: 0.7,
            is_vision: false,
        }
    }
}

/// A chat message in the conversation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    /// JSON array of ImageAttachment; only user messages should carry images.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachments: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// An image payload (base64) for vision requests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImagePart {
    pub mime: String,
    pub base64: String,
}

/// An image attachment carried on a chat message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageAttachment {
    pub mime: String,
    pub base64: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// A tool call from the LLM
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

/// Tool definition sent to the LLM
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: FunctionDef,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Token usage for a single LLM call.
///
/// `tokens_in_hit` counts prompt tokens served from cache (e.g. DeepSeek
/// `prompt_cache_hit_tokens`, Anthropic `cache_read_input_tokens`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct LlmUsage {
    pub tokens_in: u32,
    pub tokens_in_hit: u32,
    pub tokens_out: u32,
}

impl LlmUsage {
    pub fn total(&self) -> u32 {
        self.tokens_in.saturating_add(self.tokens_out)
    }

    pub fn miss(&self) -> u32 {
        self.tokens_in.saturating_sub(self.tokens_in_hit)
    }
}

/// LLM response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmResponse {
    pub content: String,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub tokens_in: u32,
    pub tokens_out: u32,
    pub model: String,
}

/// Streaming event emitted during LLM response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub content: Option<String>,
    pub tool_call: Option<ToolCallDelta>,
    /// Populated once by the provider at the end of the stream, if available.
    pub usage: Option<LlmUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallDelta {
    pub index: u32,
    pub id: Option<String>,
    pub function: Option<FunctionCallDelta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCallDelta {
    pub name: Option<String>,
    pub arguments: Option<String>,
}

/// Main LLM client trait
#[async_trait::async_trait]
pub trait LlmClient: Send + Sync {
    /// Non-streaming chat completion
    async fn chat_completion(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<LlmResponse, String>;

    /// Streaming chat completion — yields events via channel
    async fn chat_completion_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        sender: tokio::sync::mpsc::UnboundedSender<StreamEvent>,
    ) -> Result<(), String>;

    /// Single-turn vision request with one or more images (base64).
    /// Providers without vision support return an error.
    async fn chat_completion_vision(
        &self,
        system: &str,
        user: &str,
        images: &[ImagePart],
    ) -> Result<LlmResponse, String>;
}

/// Helper: create a ToolDefinition from name, description, and JSON Schema parameters
pub fn make_tool_definition(
    name: &str,
    description: &str,
    parameters: serde_json::Value,
) -> ToolDefinition {
    ToolDefinition {
        tool_type: "function".to_string(),
        function: FunctionDef {
            name: name.to_string(),
            description: description.to_string(),
            parameters,
        },
    }
}
