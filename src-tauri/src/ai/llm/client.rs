use super::{LlmClient, LlmConfig, LlmProvider};
use super::openai::OpenAiClient;
use super::anthropic::AnthropicClient;
use super::ollama::OllamaClient;

/// Factory: create the appropriate LlmClient for the given config
pub fn create_llm_client(config: &LlmConfig) -> Result<Box<dyn LlmClient>, String> {
    match config.provider {
        // All OpenAI-compatible providers share the same client
        LlmProvider::OpenAI
        | LlmProvider::DeepSeek
        | LlmProvider::SiliconFlow
        | LlmProvider::Qwen
        | LlmProvider::Zhipu
        | LlmProvider::Kimi
        | LlmProvider::Gemini => {
            Ok(Box::new(OpenAiClient::new(config.clone())?))
        }
        LlmProvider::Anthropic => {
            Ok(Box::new(AnthropicClient::new(config.clone())?))
        }
        LlmProvider::Ollama => {
            Ok(Box::new(OllamaClient::new(config.clone())?))
        }
    }
}
