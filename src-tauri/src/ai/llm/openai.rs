use async_trait::async_trait;
use reqwest::Client;
use tracing::{info, instrument};

use super::{ChatMessage, ImagePart, LlmClient, LlmConfig, LlmResponse, StreamEvent, ToolCall, ToolDefinition};

/// Wrap a request error with a connectivity hint when the failure looks like a
/// network/proxy problem — the most common cause is a configured local proxy
/// (e.g. http://127.0.0.1:7890) that is not actually running.
fn connection_error(context: &str, err: &reqwest::Error, config: &LlmConfig) -> String {
    let is_conn = err.is_connect() || err.is_timeout();
    let mut msg = format!("{context}: {err}");
    if is_conn {
        match config.proxy.as_deref() {
            Some(p) if !p.is_empty() => {
                msg.push_str(&format!(
                    " — cannot reach the server (proxy configured: {p}; ensure the proxy is running or clear the proxy setting)"
                ));
            }
            _ => {
                msg.push_str(" — cannot reach the server, check your network connection");
            }
        }
    }
    msg
}

/// OpenAI-compatible client (covers OpenAI, DeepSeek, SiliconFlow)
pub struct OpenAiClient {
    config: LlmConfig,
    http: Client,
}

impl OpenAiClient {
    pub fn new(config: LlmConfig) -> Result<Self, String> {
        let mut builder = Client::builder().timeout(std::time::Duration::from_secs(120));

        if let Some(ref proxy) = config.proxy {
            if !proxy.is_empty() {
                let proxy = reqwest::Proxy::all(proxy).map_err(|e| format!("invalid proxy: {e}"))?;
                builder = builder.proxy(proxy);
            }
        }

        let http = builder.build().map_err(|e| format!("failed to build HTTP client: {e}"))?;

        Ok(Self { config, http })
    }

    fn chat_url(&self) -> String {
        format!("{}/chat/completions", self.config.base_url.trim_end_matches('/'))
    }
}

#[async_trait]
impl LlmClient for OpenAiClient {
    #[instrument(skip(self, messages, tools))]
    async fn chat_completion(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<LlmResponse, String> {
        let body = serde_json::json!({
            "model": self.config.model,
            "messages": messages,
            "tools": tools,
            "max_tokens": self.config.max_tokens,
            "temperature": self.config.temperature,
        });

        let resp = self
            .http
            .post(self.chat_url())
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| connection_error("LLM request failed", &e, &self.config))?;

        let status = resp.status();
        let body_text = resp.text().await.map_err(|e| format!("read response: {e}"))?;

        if !status.is_success() {
            return Err(format!("LLM API error ({status}): {body_text}"));
        }

        let json: serde_json::Value =
            serde_json::from_str(&body_text).map_err(|e| format!("parse response: {e}"))?;

        let choice = &json["choices"][0];
        let msg = &choice["message"];
        let usage = &json["usage"];

        // Some models (e.g. DeepSeek reasoning) put output in reasoning_content
        let mut content = msg["content"].as_str().unwrap_or("").to_string();
        if content.is_empty() {
            content = msg["reasoning_content"].as_str().unwrap_or("").to_string();
        }
        // Log raw message keys for debugging
        if let Some(obj) = msg.as_object() {
            let keys: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
            info!(
                provider = "openai-compatible",
                model = %self.config.model,
                msg_keys = ?keys,
                content_len = content.len(),
                "LLM response message fields"
            );
        }

        let tool_calls: Option<Vec<ToolCall>> = msg
            .get("tool_calls")
            .and_then(|tc| serde_json::from_value(tc.clone()).ok());

        let tokens_in = usage["prompt_tokens"].as_u64().unwrap_or(0) as u32;
        let tokens_out = usage["completion_tokens"].as_u64().unwrap_or(0) as u32;

        let preview_limit = crate::core::settings_service::cached_settings()
            .log_llm_response_preview_max_chars
            .max(1) as usize;
        let body_preview = match body_text.char_indices().nth(preview_limit) {
            Some((idx, _)) => &body_text[..idx],
            None => &body_text,
        };
        info!(
            provider = "openai-compatible",
            model = %self.config.model,
            tokens_in,
            tokens_out,
            content_len = content.len(),
            response_preview = %body_preview,
            "LLM completion finished"
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
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| connection_error("LLM vision request failed", &e, &self.config))?;

        let status = resp.status();
        let body_text = resp.text().await.map_err(|e| format!("read response: {e}"))?;
        if !status.is_success() {
            return Err(format!("LLM API error ({status}): {body_text}"));
        }
        let json: serde_json::Value =
            serde_json::from_str(&body_text).map_err(|e| format!("parse response: {e}"))?;
        let choice = &json["choices"][0];
        let msg = &choice["message"];
        let mut content = msg["content"].as_str().unwrap_or("").to_string();
        if content.is_empty() {
            content = msg["reasoning_content"].as_str().unwrap_or("").to_string();
        }
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
            "stream_options": { "include_usage": true },
        });

        let resp = self
            .http
            .post(self.chat_url())
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| connection_error("LLM stream request failed", &e, &self.config))?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(format!("LLM API error ({status}): {body_text}"));
        }

        use futures::StreamExt;
        let mut stream = resp.bytes_stream();
        let mut line_buf = bytes::BytesMut::new();
        let mut json_buf = String::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| format!("stream read error: {e}"))?;
            line_buf.extend_from_slice(&chunk);

            // Process complete lines only, so a multi-byte UTF-8 char split across
            // chunk boundaries is never converted to lossy text.
            while let Some(pos) = line_buf.iter().position(|&b| b == b'\n') {
                let line_bytes = line_buf.split_to(pos + 1);
                // Drop the trailing newline and any \r for robust CRLF handling.
                let line = line_bytes
                    .trim_ascii_end()
                    .strip_suffix(b"\r")
                    .unwrap_or(line_bytes.trim_ascii_end());
                let line = String::from_utf8_lossy(line).trim().to_string();

                if line.is_empty() || line == "data: [DONE]" {
                    continue;
                }

                // Some providers send JSON over multiple SSE data lines; buffer until parseable.
                if let Some(data) = line.strip_prefix("data: ") {
                    json_buf.push_str(data);
                    let json: serde_json::Value = match serde_json::from_str(&json_buf) {
                        Ok(v) => {
                            json_buf.clear();
                            v
                        }
                        Err(_) => continue,
                    };

                    // OpenAI sends a final chunk with empty choices and usage info
                    // when stream_options.include_usage is true.
                    if let Some(usage) = json["usage"].as_object() {
                        let tokens_in = usage["prompt_tokens"].as_u64().unwrap_or(0) as u32;
                        let tokens_out = usage["completion_tokens"].as_u64().unwrap_or(0) as u32;
                        // Some providers (e.g. DeepSeek) split prompt tokens into
                        // cache hit / miss. Fall back to zero when absent.
                        let tokens_in_hit = usage["prompt_cache_hit_tokens"]
                            .as_u64()
                            .or_else(|| usage["cache_read_input_tokens"].as_u64())
                            .unwrap_or(0) as u32;
                        if tokens_in > 0 || tokens_out > 0 {
                            let _ = sender.send(StreamEvent {
                                event_type: "usage".to_string(),
                                content: None,
                                tool_call: None,
                                usage: Some(super::LlmUsage { tokens_in, tokens_in_hit, tokens_out }),
                            });
                        }
                    }

                    let choices = match json["choices"].as_array() {
                        Some(c) if !c.is_empty() => c,
                        _ => continue,
                    };
                    let delta = &choices[0]["delta"];

                    // Reasoning delta (from reasoning models like DeepSeek-R1 / o1)
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

                    // Content delta
                    if let Some(content) = delta["content"].as_str() {
                        if !content.is_empty() {
                            let _ = sender.send(StreamEvent {
                                event_type: "delta".to_string(),
                                content: Some(content.to_string()),
                                tool_call: None,
                                usage: None,
                            });
                        }
                    }

                    // Tool call delta
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
                                    index,
                                    id,
                                    function: Some(super::FunctionCallDelta {
                                        name: func_name,
                                        arguments: func_args,
                                    }),
                                }),
                                usage: None,
                            });
                        }
                    }

                    // Finish reason — the engine needs "length" to tell "the
                    // model chose to say nothing" apart from "the completion
                    // budget was exhausted (e.g. by reasoning tokens), so no
                    // content was ever produced".
                    if let Some(fr) = choices[0]["finish_reason"].as_str() {
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
        }

        Ok(())
    }
}
