use async_trait::async_trait;
use reqwest::Client;
use tracing::{info, instrument};

use super::{ChatMessage, ImagePart, LlmClient, LlmConfig, LlmResponse, StreamEvent, ToolCall, ToolDefinition};

/// Anthropic Claude client (Messages API)
pub struct AnthropicClient {
    config: LlmConfig,
    http: Client,
}

impl AnthropicClient {
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

    fn messages_url(&self) -> String {
        format!("{}/messages", self.config.base_url.trim_end_matches('/'))
    }

    /// Convert our internal tool definitions to Anthropic format
    fn convert_tools(tools: &[ToolDefinition]) -> Vec<serde_json::Value> {
        tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.function.name,
                    "description": t.function.description,
                    "input_schema": t.function.parameters,
                })
            })
            .collect()
    }

    /// Convert our ChatMessage to Anthropic format
    fn convert_messages(msgs: &[ChatMessage]) -> (Option<String>, Vec<serde_json::Value>) {
        let mut system: Option<String> = None;
        let mut converted = Vec::new();

        for msg in msgs {
            if msg.role == "system" {
                system = Some(msg.content.clone());
                continue;
            }

            // Build Anthropic content as a mixed array of text + tool_use blocks
            let mut content_blocks: Vec<serde_json::Value> = Vec::new();

            // Preserve text content if present
            if !msg.content.is_empty() {
                content_blocks.push(serde_json::json!({
                    "type": "text",
                    "text": msg.content,
                }));
            }

            // Add tool_use blocks
            if let Some(ref tool_calls) = msg.tool_calls {
                for tc in tool_calls {
                    content_blocks.push(serde_json::json!({
                        "type": "tool_use",
                        "id": tc.id,
                        "name": tc.function.name,
                        "input": serde_json::from_str::<serde_json::Value>(&tc.function.arguments).unwrap_or_default(),
                    }));
                }
            }

            // Tool result message
            let mut entry = if let Some(ref tool_call_id) = msg.tool_call_id {
                serde_json::json!({
                    "role": msg.role,
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": tool_call_id,
                        "content": msg.content,
                    }],
                })
            } else if content_blocks.is_empty() {
                serde_json::json!({
                    "role": msg.role,
                    "content": msg.content,
                })
            } else if content_blocks.len() == 1 && content_blocks[0]["type"] == "text" {
                // Single text block — use string form for Anthropic
                serde_json::json!({
                    "role": msg.role,
                    "content": msg.content,
                })
            } else {
                serde_json::json!({
                    "role": msg.role,
                    "content": content_blocks,
                })
            };

            converted.push(entry);
        }

        (system, converted)
    }
}

#[async_trait]
impl LlmClient for AnthropicClient {
    #[instrument(skip(self, messages, tools))]
    async fn chat_completion(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<LlmResponse, String> {
        let (system, converted_msgs) = Self::convert_messages(messages);
        let anthropic_tools = Self::convert_tools(tools);

        let mut body = serde_json::json!({
            "model": self.config.model,
            "max_tokens": self.config.max_tokens,
            "messages": converted_msgs,
            "tools": anthropic_tools,
        });

        if let Some(sys) = system {
            body["system"] = serde_json::json!(sys);
        }

        let resp = self
            .http
            .post(self.messages_url())
            .header("x-api-key", &self.config.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Anthropic request failed: {e}"))?;

        let status = resp.status();
        let body_text = resp.text().await.map_err(|e| format!("read response: {e}"))?;

        if !status.is_success() {
            return Err(format!("Anthropic API error ({status}): {body_text}"));
        }

        let json: serde_json::Value =
            serde_json::from_str(&body_text).map_err(|e| format!("parse response: {e}"))?;

        let content_blocks = json["content"].as_array();
        let mut text_content = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();

        if let Some(blocks) = content_blocks {
            for (i, block) in blocks.iter().enumerate() {
                match block["type"].as_str() {
                    Some("text") => {
                        if let Some(t) = block["text"].as_str() {
                            text_content.push_str(t);
                        }
                    }
                    Some("tool_use") => {
                        let name = block["name"].as_str().unwrap_or("").to_string();
                        let args = block["input"].to_string();
                        tool_calls.push(ToolCall {
                            id: block["id"].as_str().unwrap_or(&format!("tc_{i}")).to_string(),
                            call_type: "function".to_string(),
                            function: super::FunctionCall {
                                name,
                                arguments: args,
                            },
                        });
                    }
                    _ => {}
                }
            }
        }

        let usage = &json["usage"];
        let tokens_in = usage["input_tokens"].as_u64().unwrap_or(0) as u32;
        let tokens_out = usage["output_tokens"].as_u64().unwrap_or(0) as u32;

        info!(
            provider = "anthropic",
            model = %self.config.model,
            tokens_in,
            tokens_out,
            "Anthropic completion finished"
        );

        Ok(LlmResponse {
            content: text_content,
            tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
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
        let mut blocks: Vec<serde_json::Value> = Vec::new();
        if !user.is_empty() {
            blocks.push(serde_json::json!({"type": "text", "text": user}));
        }
        for img in images {
            blocks.push(serde_json::json!({
                "type": "image",
                "source": { "type": "base64", "media_type": img.mime, "data": img.base64 },
            }));
        }
        let mut body = serde_json::json!({
            "model": self.config.model,
            "max_tokens": self.config.max_tokens,
            "messages": [ { "role": "user", "content": blocks } ],
        });
        if !system.is_empty() {
            body["system"] = serde_json::json!(system);
        }

        let resp = self
            .http
            .post(self.messages_url())
            .header("x-api-key", &self.config.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Anthropic vision request failed: {e}"))?;

        let status = resp.status();
        let body_text = resp.text().await.map_err(|e| format!("read response: {e}"))?;
        if !status.is_success() {
            return Err(format!("Anthropic API error ({status}): {body_text}"));
        }
        let json: serde_json::Value =
            serde_json::from_str(&body_text).map_err(|e| format!("parse response: {e}"))?;

        let mut content = String::new();
        if let Some(blocks) = json["content"].as_array() {
            for block in blocks {
                if block["type"] == "text" {
                    if let Some(t) = block["text"].as_str() {
                        content.push_str(t);
                    }
                }
            }
        }
        let tokens_in = json["usage"]["input_tokens"].as_u64().unwrap_or(0) as u32;
        let tokens_out = json["usage"]["output_tokens"].as_u64().unwrap_or(0) as u32;
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
        let (system, converted_msgs) = Self::convert_messages(messages);
        let anthropic_tools = Self::convert_tools(tools);

        let mut body = serde_json::json!({
            "model": self.config.model,
            "max_tokens": self.config.max_tokens,
            "messages": converted_msgs,
            "tools": anthropic_tools,
            "stream": true,
        });

        if let Some(sys) = system {
            body["system"] = serde_json::json!(sys);
        }

        let resp = self
            .http
            .post(self.messages_url())
            .header("x-api-key", &self.config.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Anthropic stream request failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(format!("Anthropic API error ({status}): {body_text}"));
        }

        use futures::StreamExt;
        let mut stream = resp.bytes_stream();
        let mut current_tool_index: u32 = 0;
        let mut current_tool_id = String::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| format!("stream read error: {e}"))?;
            let text = String::from_utf8_lossy(&chunk);

            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                // Anthropic SSE: "event: ..." then "data: ..."
                if let Some(data) = line.strip_prefix("data: ") {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(data) {
                        match json["type"].as_str() {
                            Some("content_block_delta") => {
                                let delta_type = json["delta"]["type"].as_str();
                                match delta_type {
                                    Some("text_delta") => {
                                        if let Some(content) = json["delta"]["text"].as_str() {
                                            let _ = sender.send(StreamEvent {
                                                event_type: "delta".to_string(),
                                                content: Some(content.to_string()),
                                                tool_call: None,
                                            });
                                        }
                                    }
                                    Some("input_json_delta") => {
                                        if let Some(partial) = json["delta"]["partial_json"].as_str() {
                                            let _ = sender.send(StreamEvent {
                                                event_type: "tool_call_delta".to_string(),
                                                content: None,
                                                tool_call: Some(super::ToolCallDelta {
                                                    index: current_tool_index,
                                                    id: Some(current_tool_id.clone()),
                                                    function: Some(super::FunctionCallDelta {
                                                        name: None,
                                                        arguments: Some(partial.to_string()),
                                                    }),
                                                }),
                                            });
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            Some("content_block_start") => {
                                if json["content_block"]["type"] == "tool_use" {
                                    current_tool_index = json["index"].as_u64().unwrap_or(0) as u32;
                                    current_tool_id = json["content_block"]["id"]
                                        .as_str()
                                        .unwrap_or("")
                                        .to_string();
                                    let name = json["content_block"]["name"]
                                        .as_str()
                                        .unwrap_or("")
                                        .to_string();
                                    let _ = sender.send(StreamEvent {
                                        event_type: "tool_call_delta".to_string(),
                                        content: None,
                                        tool_call: Some(super::ToolCallDelta {
                                            index: current_tool_index,
                                            id: Some(current_tool_id.clone()),
                                            function: Some(super::FunctionCallDelta {
                                                name: Some(name),
                                                arguments: None,
                                            }),
                                        }),
                                    });
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        Ok(())
    }
}
