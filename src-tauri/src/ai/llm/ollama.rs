use async_trait::async_trait;
use reqwest::Client;
use tracing::{info, instrument};

use super::{ChatMessage, ImagePart, LlmClient, LlmConfig, LlmResponse, StreamEvent, ToolCall, ToolDefinition};

/// Ollama local client (OpenAI-compatible API via /v1/chat/completions)
pub struct OllamaClient {
    config: LlmConfig,
    http: Client,
}

impl OllamaClient {
    pub fn new(config: LlmConfig) -> Result<Self, String> {
        let builder = Client::builder().timeout(std::time::Duration::from_secs(300));
        let http = builder.build().map_err(|e| format!("failed to build HTTP client: {e}"))?;

        Ok(Self { config, http })
    }

    fn chat_url(&self) -> String {
        format!("{}/chat/completions", self.config.base_url.trim_end_matches('/'))
    }
}

#[async_trait]
impl LlmClient for OllamaClient {
    #[instrument(skip(self, messages, tools))]
    async fn chat_completion(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<LlmResponse, String> {
        // Ollama supports OpenAI-compatible API since v0.5.0
        let body = serde_json::json!({
            "model": self.config.model,
            "messages": messages,
            "tools": tools,
            "max_tokens": self.config.max_tokens,
            "temperature": self.config.temperature,
            "stream": false,
        });

        let resp = self
            .http
            .post(self.chat_url())
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Ollama request failed: {e}"))?;

        let status = resp.status();
        let body_text = resp.text().await.map_err(|e| format!("read response: {e}"))?;

        if !status.is_success() {
            return Err(format!("Ollama API error ({status}): {body_text}"));
        }

        let json: serde_json::Value =
            serde_json::from_str(&body_text).map_err(|e| format!("parse response: {e}"))?;

        let choice = &json["choices"][0];
        let msg = &choice["message"];

        let content = msg["content"].as_str().unwrap_or("").to_string();

        let tool_calls: Option<Vec<ToolCall>> = msg
            .get("tool_calls")
            .and_then(|tc| serde_json::from_value(tc.clone()).ok());

        // Ollama doesn't always return usage
        let tokens_in = json["usage"]["prompt_tokens"].as_u64().unwrap_or(0) as u32;
        let tokens_out = json["usage"]["completion_tokens"].as_u64().unwrap_or(0) as u32;

        info!(
            provider = "ollama",
            model = %self.config.model,
            tokens_in,
            tokens_out,
            "Ollama completion finished"
        );

        Ok(LlmResponse {
            content,
            tool_calls,
            tokens_in,
            tokens_out,
            model: self.config.model.clone(),
        })
    }

    async fn chat_completion_vision(
        &self,
        system: &str,
        user: &str,
        images: &[ImagePart],
    ) -> Result<LlmResponse, String> {
        let mut content: Vec<serde_json::Value> = Vec::new();
        if !user.is_empty() {
            content.push(serde_json::json!({"type": "text", "text": user}));
        }
        for img in images {
            content.push(serde_json::json!({
                "type": "image_url",
                "image_url": { "url": format!("data:{};base64,{}", img.mime, img.base64) },
            }));
        }
        let mut messages: Vec<serde_json::Value> = Vec::new();
        if !system.is_empty() {
            messages.push(serde_json::json!({"role": "system", "content": system}));
        }
        messages.push(serde_json::json!({"role": "user", "content": content}));

        let body = serde_json::json!({
            "model": self.config.model,
            "messages": messages,
            "max_tokens": self.config.max_tokens,
            "temperature": self.config.temperature,
        });

        let resp = self
            .http
            .post(self.chat_url())
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Ollama vision request failed: {e}"))?;

        let status = resp.status();
        let body_text = resp.text().await.map_err(|e| format!("read response: {e}"))?;
        if !status.is_success() {
            return Err(format!("Ollama API error ({status}): {body_text}"));
        }
        let json: serde_json::Value =
            serde_json::from_str(&body_text).map_err(|e| format!("parse response: {e}"))?;
        let choice = &json["choices"][0];
        let content = choice["message"]["content"].as_str().unwrap_or("").to_string();
        let tokens_in = json["usage"]["prompt_tokens"].as_u64().unwrap_or(0) as u32;
        let tokens_out = json["usage"]["completion_tokens"].as_u64().unwrap_or(0) as u32;
        Ok(LlmResponse {
            content,
            tool_calls: None,
            tokens_in,
            tokens_out,
            model: self.config.model.clone(),
        })
    }

    #[instrument(skip(self, messages, tools, sender))]
    async fn chat_completion_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        sender: tokio::sync::mpsc::UnboundedSender<StreamEvent>,
    ) -> Result<(), String> {
        let body = serde_json::json!({
            "model": self.config.model,
            "messages": messages,
            "tools": tools,
            "max_tokens": self.config.max_tokens,
            "temperature": self.config.temperature,
            "stream": true,
        });

        let resp = self
            .http
            .post(self.chat_url())
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Ollama stream request failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(format!("Ollama API error ({status}): {body_text}"));
        }

        use futures::StreamExt;
        let mut stream = resp.bytes_stream();
        let mut buffer = String::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| format!("stream read error: {e}"))?;
            let text = String::from_utf8_lossy(&chunk);

            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() || line == "data: [DONE]" {
                    continue;
                }
                if let Some(data) = line.strip_prefix("data: ") {
                    buffer.push_str(data);
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&buffer) {
                        buffer.clear();

                        if let Some(choices) = json["choices"].as_array() {
                            if let Some(choice) = choices.first() {
                                let delta = &choice["delta"];

                                if let Some(content) = delta["content"].as_str() {
                                    let _ = sender.send(StreamEvent {
                                        event_type: "delta".to_string(),
                                        content: Some(content.to_string()),
                                        tool_call: None,
                                    });
                                }

                                // Handle tool call deltas (Ollama >= v0.5 supports function calling)
                                if let Some(tc_deltas) = delta["tool_calls"].as_array() {
                                    for tc in tc_deltas {
                                        let index = tc["index"].as_u64().unwrap_or(0) as u32;
                                        let id = tc["id"].as_str().map(|s| s.to_string());
                                        let func_name = tc["function"]["name"].as_str().map(|s| s.to_string());
                                        let func_args = tc["function"]["arguments"].as_str().map(|s| s.to_string());
                                        let _ = sender.send(StreamEvent {
                                            event_type: "tool_call_delta".to_string(),
                                            content: None,
                                            tool_call: Some(super::ToolCallDelta {
                                                index, id,
                                                function: Some(super::FunctionCallDelta {
                                                    name: func_name,
                                                    arguments: func_args,
                                                }),
                                            }),
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }
}
