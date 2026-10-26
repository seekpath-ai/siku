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
        // No total timeout: see OpenAiClient::new — long local generations
        // must not be cut off; time out on connect and per-read stalls only.
        let builder = Client::builder()
            .connect_timeout(std::time::Duration::from_secs(15))
            .read_timeout(std::time::Duration::from_secs(300))
            .tcp_keepalive(std::time::Duration::from_secs(30));
        let http = builder.build().map_err(|e| format!("failed to build HTTP client: {e}"))?;

        Ok(Self { config, http })
    }

    fn chat_url(&self) -> String {
        format!("{}/chat/completions", self.config.base_url.trim_end_matches('/'))
    }
}

/// Convert a ChatMessage to the JSON shape expected by OpenAI-compatible APIs.
fn message_to_openai_json(msg: &ChatMessage) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert("role".to_string(), serde_json::Value::String(msg.role.clone()));

    if let Some(attachments_json) = &msg.attachments {
        let mut content_blocks: Vec<serde_json::Value> = Vec::new();
        if !msg.content.is_empty() {
            content_blocks.push(serde_json::json!({"type": "text", "text": msg.content}));
        }
        if let Ok(attachments) = serde_json::from_str::<Vec<crate::ai::llm::ImageAttachment>>(attachments_json) {
            for att in attachments {
                content_blocks.push(serde_json::json!({
                    "type": "image_url",
                    "image_url": { "url": format!("data:{};base64,{}", att.mime, att.base64) },
                }));
            }
        }
        obj.insert("content".to_string(), serde_json::Value::Array(content_blocks));
    } else {
        obj.insert("content".to_string(), serde_json::Value::String(msg.content.clone()));
    }

    if let Some(ref tool_calls) = msg.tool_calls {
        obj.insert("tool_calls".to_string(), serde_json::to_value(tool_calls).unwrap_or_default());
    }
    if let Some(ref tool_call_id) = msg.tool_call_id {
        obj.insert("tool_call_id".to_string(), serde_json::Value::String(tool_call_id.clone()));
    }
    if let Some(ref name) = msg.name {
        obj.insert("name".to_string(), serde_json::Value::String(name.clone()));
    }

    serde_json::Value::Object(obj)
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
            "messages": messages.iter().map(message_to_openai_json).collect::<Vec<_>>(),
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

        // OpenAI 兼容形状直接给出 finish_reason；空正文 + "length" 即思考模型
        // 耗尽输出额度的典型信号。
        let finish_reason = choice["finish_reason"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

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
            finish_reason = ?finish_reason,
            "Ollama completion finished"
        );

        Ok(LlmResponse {
            content,
            tool_calls,
            tokens_in,
            tokens_out,
            model: self.config.model.clone(),
            finish_reason,
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
        let finish_reason = choice["finish_reason"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let tokens_in = json["usage"]["prompt_tokens"].as_u64().unwrap_or(0) as u32;
        let tokens_out = json["usage"]["completion_tokens"].as_u64().unwrap_or(0) as u32;
        Ok(LlmResponse {
            content,
            tool_calls: None,
            tokens_in,
            tokens_out,
            model: self.config.model.clone(),
            finish_reason,
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
            "messages": messages.iter().map(message_to_openai_json).collect::<Vec<_>>(),
            "tools": tools,
            "max_tokens": self.config.max_tokens,
            "temperature": self.config.temperature,
            "stream": true,
            // 让最后一个 chunk 携带 usage（OpenAI 兼容形状，Ollama ≥ v0.5 支持）
            "stream_options": { "include_usage": true },
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

                                // 思考模型的推理增量（DeepSeek-R1 / qwq 等经
                                // OpenAI 兼容接口输出 reasoning_content）
                                if let Some(reasoning) = delta["reasoning_content"].as_str() {
                                    if !reasoning.is_empty() {
                                        let _ = sender.send(StreamEvent {
                                            event_type: "reasoning".to_string(),
                                            content: Some(reasoning.to_string()),
                                            tool_call: None,
                                            usage: None,
                                        });
                                    }
                                }

                                if let Some(content) = delta["content"].as_str() {
                                    let _ = sender.send(StreamEvent {
                                        event_type: "delta".to_string(),
                                        content: Some(content.to_string()),
                                        tool_call: None,
                                        usage: None,
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
                                            usage: None,
                                        });
                                    }
                                }

                                // finish_reason：下游引擎靠 "length" 区分额度截断
                                if let Some(fr) = choice["finish_reason"].as_str() {
                                    if !fr.is_empty() {
                                        let _ = sender.send(StreamEvent {
                                            event_type: "finish".to_string(),
                                            content: Some(fr.to_string()),
                                            tool_call: None,
                                            usage: None,
                                        });
                                    }
                                }
                            }
                        }

                        // OpenAI 兼容形状的 usage：请求里开了 stream_options.
                        // include_usage 后由最后一个 chunk 携带；旧版本 Ollama
                        // 不发该字段时放弃（不硬编默认值）。
                        if let Some(usage) = json["usage"].as_object() {
                            let tokens_in = usage
                                .get("prompt_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0) as u32;
                            let tokens_out = usage
                                .get("completion_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0) as u32;
                            if tokens_in > 0 || tokens_out > 0 {
                                let _ = sender.send(StreamEvent {
                                    event_type: "usage".to_string(),
                                    content: None,
                                    tool_call: None,
                                    usage: Some(super::LlmUsage { tokens_in, tokens_in_hit: 0, tokens_out }),
                                });
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }
}
