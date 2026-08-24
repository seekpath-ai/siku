use sqlx::SqlitePool;
use tracing::{error, info, info_span};

use crate::ai::agent::config::AgentConfig;
use crate::ai::agent::memory::ConversationMemory;
use crate::ai::agent::memory_store::{MemoryRecord, MemoryStore};
use crate::ai::agent::tool_registry::ToolRegistry;
use crate::ai::llm::{self, ChatMessage, LlmClient, StreamEvent, ToolCall};
use crate::core::models::AgentStep;
use crate::core::time;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AgentEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub session_id: String,
    pub step_index: Option<i32>,
    pub content: Option<String>,
    pub tool_call_id: Option<String>,
    pub tool_name: Option<String>,
    pub tool_args: Option<serde_json::Value>,
    pub tool_result: Option<String>,
    pub status: Option<String>,
    pub duration_ms: Option<i32>,
    /// Token usage breakdown for terminal done/cancelled events.
    pub tokens_used: Option<i32>,
    pub tokens_in: Option<i32>,
    pub tokens_in_hit: Option<i32>,
    pub tokens_out: Option<i32>,
}

#[derive(Debug, Clone)]
pub enum ApprovalResponse {
    Approved,
    Declined,
    ModifiedArgs(serde_json::Value),
}

/// Record of a tool call executed during an agent turn, suitable for persistence.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolCallRecord {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
    pub result: String,
    pub status: String,
    pub duration_ms: i32,
}

pub struct AgentEngine {
    llm: Box<dyn LlmClient>,
    tool_registry: ToolRegistry,
    memory: ConversationMemory,
    memory_store: MemoryStore,
    config: AgentConfig,
    session_id: String,
    db: SqlitePool,
    cancel_token: tokio_util::sync::CancellationToken,
    project_dir: Option<String>,
    /// Extra context injected into the system prompt (pet domain agents).
    context_prompt: Option<String>,
}

impl AgentEngine {
    pub fn new(
        llm: Box<dyn LlmClient>,
        tool_registry: ToolRegistry,
        session_id: String,
        db: SqlitePool,
        config: AgentConfig,
        memory_store: MemoryStore,
        cancel_token: tokio_util::sync::CancellationToken,
        project_dir: Option<String>,
        context_prompt: Option<String>,
    ) -> Self {
        let system_prompt = Some(config.effective_system_prompt());
        let max_tokens = config.effective_max_tokens();
        let memory = ConversationMemory::new(max_tokens, system_prompt);
        Self {
            llm,
            tool_registry,
            memory,
            memory_store,
            config,
            session_id,
            db,
            cancel_token,
            project_dir,
            context_prompt,
        }
    }

    fn is_cancelled(&self) -> bool {
        self.cancel_token.is_cancelled()
    }

    /// Some models wrap tool arguments as `{"arguments": {...}}` instead of
    /// emitting the parameters at the top level. Normalize to the flat form so
    /// tools always receive their parameters directly.
    fn normalize_args(args: serde_json::Value) -> serde_json::Value {
        if let Some(obj) = args.as_object() {
            if obj.len() == 1 {
                if let Some(inner) = obj.get("arguments") {
                    if inner.is_object() {
                        return inner.clone();
                    }
                }
            }
        }
        args
    }

    /// Emit an `ask_user` event with the questions and wait for the user's
    /// answers (delivered via the ask channel), with a 5-minute timeout.
    async fn handle_ask_user(
        &self,
        event_tx: &tokio::sync::mpsc::UnboundedSender<AgentEvent>,
        args: &serde_json::Value,
        ask_rx: &mut tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>,
    ) -> String {
        // Tool arguments are usually { "questions": [...] } — accept both that
        // and a bare array so the frontend always receives an array.
        let questions = if args.is_array() {
            args
        } else {
            &args["questions"]
        };
        self.emit(
            event_tx,
            "ask_user",
            None,
            Some(questions.to_string()),
            None,
            None,
            None,
            None,
            None,
            None,
        );
        match tokio::select! {
            biased;
            _ = self.cancel_token.cancelled() => None,
            result = tokio::time::timeout(std::time::Duration::from_secs(300), ask_rx.recv()) => Some(result),
        } {
            Some(Ok(Some(answers))) => answers.to_string(),
            Some(Ok(None)) => "AskUserQuestion failed: channel closed".to_string(),
            Some(Err(_)) => "AskUserQuestion timed out".to_string(),
            None => "AskUserQuestion cancelled by user".to_string(),
        }
    }

    /// Load conversation memory for this agent.
    /// `max_memory_rounds == 0` loads the full conversation.
    pub async fn load_memory(&self) -> Result<Vec<MemoryRecord>, String> {
        let rounds = self.config.effective_max_memory_rounds();
        if rounds == 0 {
            Ok(self.memory_store.load_all())
        } else {
            Ok(self.memory_store.load_recent(rounds))
        }
    }

    fn emit(
        &self,
        event_tx: &tokio::sync::mpsc::UnboundedSender<AgentEvent>,
        event_type: &str,
        step_index: Option<i32>,
        content: Option<String>,
        tool_call_id: Option<String>,
        tool_name: Option<String>,
        tool_args: Option<serde_json::Value>,
        tool_result: Option<String>,
        status: Option<String>,
        duration_ms: Option<i32>,
    ) {
        let _ = event_tx.send(AgentEvent {
            event_type: event_type.into(),
            session_id: self.session_id.clone(),
            step_index,
            content,
            tool_call_id,
            tool_name,
            tool_args,
            tool_result,
            status,
            duration_ms,
            tokens_used: None,
            tokens_in: None,
            tokens_in_hit: None,
            tokens_out: None,
        });
    }

    /// Process a user message with streaming output via event channel.
    /// Returns the final assistant content, the ReAct steps, whether the
    /// turn was cancelled, and the token usage across all LLM rounds.
    /// Does NOT emit the terminal done/cancelled event — the caller emits it
    /// after persisting the results.
    pub async fn process_message(
        &self,
        user_message: &str,
        history: &[ChatMessage],
        event_tx: tokio::sync::mpsc::UnboundedSender<AgentEvent>,
        approval_rx: &mut tokio::sync::mpsc::UnboundedReceiver<ApprovalResponse>,
        ask_rx: &mut tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>,
    ) -> Result<(String, Vec<AgentStep>, bool, crate::ai::llm::LlmUsage), String> {
        let span = info_span!("agent_turn", session_id = %self.session_id);
        let _guard = span.enter();
        let sid = self.session_id.clone();

        info!(user_message_len = user_message.len(), history_len = history.len(), "starting agent turn");

        // Emit thinking
        self.emit(&event_tx, "thinking", None, Some("Analyzing request...".into()), None, None, None, None, None, None);

        // Build messages
        let mut messages: Vec<ChatMessage> = Vec::new();
        if let Some(ref sys) = self.memory.system_prompt {
            let mut content = sys.clone();
            if let Some(dir) = &self.project_dir {
                if !dir.is_empty() {
                    content.push_str(&format!(
                        "\n\n当前项目目录：{dir}。使用 file_list / file_read 等文件工具时请基于该目录操作。"
                    ));
                }
            }
            if let Some(ctx) = &self.context_prompt {
                content.push_str(&format!("\n\n{ctx}"));
            }
            messages.push(ChatMessage { role: "system".into(), content, tool_calls: None, tool_call_id: None, name: None });
        }
        messages.extend(history.iter().cloned());
        messages.push(ChatMessage { role: "user".into(), content: user_message.to_string(), tool_calls: None, tool_call_id: None, name: None });

        info!(message_count=%messages.len(), messages_summary=%messages.iter().map(|m| {
            let tc = m.tool_calls.as_ref().map(|v| v.len()).unwrap_or(0);
            format!("{}[c={},tc={}]", m.role, m.content.len(), tc)
        }).collect::<Vec<_>>().join(","), "agent messages built for LLM");

        let tool_defs = self.tool_registry.get_definitions();
        let max_loops = self.config.effective_max_loops();

        // — ReAct loop with streaming —
        let mut final_content = String::new();
        // Every round's visible text, concatenated. The frontend streams ALL
        // rounds into one reply, so the persisted message must match: the
        // final round alone can be empty or just a trailer (e.g. the model
        // putting the ```evidence block in a round of its own), which used to
        // make the reply "vanish" after the history reload.
        let mut full_text = String::new();
        // Set when a round ends on finish_reason=length with no visible text:
        // the completion budget was exhausted (thinking models count reasoning
        // tokens toward max_tokens) before any content was produced.
        let mut truncated_empty = false;
        let mut steps: Vec<AgentStep> = Vec::new();
        let mut round = 0;
        let mut last_approval_at: Option<std::time::Instant> = None;
        let mut usage = crate::ai::llm::LlmUsage {
            tokens_in: 0,
            tokens_in_hit: 0,
            tokens_out: 0,
        };

        let mut cancelled = false;
        loop {
            if self.is_cancelled() {
                info!(round, "agent cancelled by user before round");
                cancelled = true;
                break;
            }
            if max_loops > 0 && round >= max_loops {
                messages.push(ChatMessage { role: "system".into(),
                    content: "Max tool calls reached. Provide your final answer now based on gathered information.".into(),
                    tool_calls: None, tool_call_id: None, name: None });
                let resp = self.llm.chat_completion(&messages, &[]).await.map_err(|e| format!("LLM: {e}"))?;
                final_content = resp.content;
                if !final_content.trim().is_empty() {
                    if !full_text.is_empty() { full_text.push_str("\n\n"); }
                    full_text.push_str(final_content.trim_end());
                }
                break;
            }
            round += 1;
            let step_index = round as i32;

            // Truncate if needed
            if ConversationMemory::estimate_messages_tokens(&messages) > 100_000 {
                messages = self.memory.truncate(&messages, 28_000);
            }

            // Accumulate streaming response + tool call deltas
            let (stream_tx, mut stream_rx) = tokio::sync::mpsc::unbounded_channel();
            let msgs_clone = messages.clone();
            let llm_fut = self.llm.chat_completion_stream(&msgs_clone, &tool_defs, stream_tx);

            let mut stream_text = String::new();
            let mut round_reasoning = String::new();
            let mut round_finish: Option<String> = None;
            let mut tool_call_buf: std::collections::HashMap<u32, (String, String, String)> = std::collections::HashMap::new();

            tokio::pin!(llm_fut);
            loop {
                tokio::select! {
                    biased;
                    _ = self.cancel_token.cancelled() => {
                        info!(round, "agent cancelled during streaming");
                        cancelled = true;
                        break;
                    }
                    event = stream_rx.recv() => {
                        match event {
                            Some(StreamEvent { event_type, content, tool_call, usage: round_usage }) => {
                                if self.is_cancelled() {
                                    info!(round, "agent cancelled during streaming");
                                    cancelled = true;
                                    break;
                                }
                                match event_type.as_str() {
                                    "delta" => {
                                        if let Some(ref c) = content {
                                            stream_text.push_str(c);
                                            self.emit(&event_tx, "delta", Some(step_index), Some(c.clone()), None, None, None, None, None, None);
                                        }
                                    }
                                    "reasoning" => {
                                        if let Some(ref c) = content {
                                            round_reasoning.push_str(c);
                                            self.emit(&event_tx, "reasoning", Some(step_index), Some(c.clone()), None, None, None, None, None, None);
                                        }
                                    }
                                    "tool_call_delta" => {
                                        if let Some(ref tc) = tool_call {
                                            let entry = tool_call_buf.entry(tc.index).or_default();
                                            if let Some(ref id) = tc.id { entry.0 = id.clone(); }
                                            if let Some(ref f) = tc.function {
                                                if let Some(ref n) = f.name { entry.1 = n.clone(); }
                                                if let Some(ref a) = f.arguments { entry.2.push_str(a); }
                                            }
                                        }
                                    }
                                    "finish" => {
                                        if let Some(ref c) = content {
                                            round_finish = Some(c.clone());
                                        }
                                    }
                                    "usage" => {
                                        if let Some(u) = round_usage {
                                            usage.tokens_in = usage.tokens_in.saturating_add(u.tokens_in);
                                            usage.tokens_in_hit = usage.tokens_in_hit.saturating_add(u.tokens_in_hit);
                                            usage.tokens_out = usage.tokens_out.saturating_add(u.tokens_out);
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            None => break,
                        }
                    }
                    result = &mut llm_fut => {
                        if let Err(e) = result {
                            error!(error = %e, "LLM stream failed");
                            self.emit(&event_tx, "error", Some(step_index), Some(format!("LLM error: {e}")), None, None, None, None, None, None);
                            return Err(format!("LLM: {e}"));
                        }
                        info!(stream_text_len = stream_text.len(), tool_calls = tool_call_buf.len(), "LLM stream finished");
                        break;
                    }
                }
            }

            // Drain remaining stream events
            while let Ok(event) = stream_rx.try_recv() {
                if self.is_cancelled() {
                    cancelled = true;
                    break;
                }
                match event.event_type.as_str() {
                    "delta" => {
                        if let Some(ref c) = event.content {
                            stream_text.push_str(c);
                            self.emit(&event_tx, "delta", Some(step_index), Some(c.clone()), None, None, None, None, None, None);
                        }
                    }
                    "reasoning" => {
                        if let Some(ref c) = event.content {
                            round_reasoning.push_str(c);
                            self.emit(&event_tx, "reasoning", Some(step_index), Some(c.clone()), None, None, None, None, None, None);
                        }
                    }
                    "tool_call_delta" => {
                        if let Some(ref tc) = event.tool_call {
                            let entry = tool_call_buf.entry(tc.index).or_default();
                            if let Some(ref id) = tc.id { entry.0 = id.clone(); }
                            if let Some(ref f) = tc.function {
                                if let Some(ref n) = f.name { entry.1 = n.clone(); }
                                if let Some(ref a) = f.arguments { entry.2.push_str(a); }
                            }
                        }
                    }
                    "finish" => {
                        if let Some(ref c) = event.content {
                            round_finish = Some(c.clone());
                        }
                    }
                    "usage" => {
                        if let Some(u) = event.usage {
                            usage.tokens_in = usage.tokens_in.saturating_add(u.tokens_in);
                            usage.tokens_in_hit = usage.tokens_in_hit.saturating_add(u.tokens_in_hit);
                            usage.tokens_out = usage.tokens_out.saturating_add(u.tokens_out);
                        }
                    }
                    _ => {}
                }
            }

            // Fold this round's visible text into the full reply (see
            // full_text above).
            if !stream_text.trim().is_empty() {
                if !full_text.is_empty() { full_text.push_str("\n\n"); }
                full_text.push_str(stream_text.trim_end());
            }
            if stream_text.trim().is_empty() && round_finish.as_deref() == Some("length") {
                truncated_empty = true;
            }

            if cancelled {
                // Keep whatever partial text was generated before stopping.
                if final_content.trim().is_empty() && !stream_text.trim().is_empty() {
                    final_content = stream_text;
                }
                info!(round, "agent cancelled after streaming, skipping tool execution");
                break;
            }

            // Build tool calls from accumulated deltas
            let mut tool_calls: Vec<ToolCall> = Vec::new();
            let mut indices: Vec<u32> = tool_call_buf.keys().copied().collect();
            indices.sort();
            for idx in indices {
                if let Some((id, name, args)) = tool_call_buf.remove(&idx) {
                    if !name.is_empty() {
                        tool_calls.push(ToolCall { id, call_type: "function".into(), function: llm::FunctionCall { name, arguments: args } });
                    }
                }
            }

            // No tool calls => this is the final answer.
            if tool_calls.is_empty() {
                final_content = stream_text;
                // Persist the final round's reasoning even when no tools were called,
                // so it survives session switches.
                let final_step = AgentStep {
                    id: uuid::Uuid::new_v4().to_string(),
                    session_id: sid.clone(),
                    message_id: None,
                    step_index,
                    reasoning_content: if round_reasoning.trim().is_empty() { None } else { Some(round_reasoning.clone()) },
                    tool_calls: None,
                    created_at: time::now_iso(),
                };
                steps.push(final_step);
                break;
            }

            // This round includes tool calls. Store a normalized copy back into
            // the LLM context so later rounds see flat parameters (and don't
            // keep mimicking a nested `{"arguments": {...}}` shape).
            let normalized_calls: Vec<ToolCall> = tool_calls
                .iter()
                .map(|tc| {
                    let mut tc2 = tc.clone();
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&tc.function.arguments) {
                        tc2.function.arguments = Self::normalize_args(v).to_string();
                    }
                    tc2
                })
                .collect();
            messages.push(ChatMessage { role: "assistant".into(), content: stream_text.clone(), tool_calls: Some(normalized_calls), tool_call_id: None, name: None });

            // Execute each tool call and collect records for this step.
            let mut step_tool_records: Vec<ToolCallRecord> = Vec::new();
            info!(approval_mode=?self.config.approval.mode, "starting tool execution round");
            for tc in &tool_calls {
                if self.is_cancelled() {
                    info!(round, "agent cancelled during tool execution");
                    cancelled = true;
                    break;
                }
                let tool_id = format!("tool_{}", uuid::Uuid::new_v4());
                let mut args: serde_json::Value = serde_json::from_str(&tc.function.arguments).unwrap_or(serde_json::Value::Null);
                args = Self::normalize_args(args);

                self.emit(&event_tx, "tool_call", Some(step_index), None, Some(tool_id.clone()), Some(tc.function.name.clone()), Some(args.clone()), None, None, None);

                // AskUserQuestion: handled inline by the engine — emit an
                // `ask_user` event and wait for the user's answers.
                if tc.function.name == "ask_user" {
                    let answers = self.handle_ask_user(&event_tx, &args, ask_rx).await;
                    let failed = answers.starts_with("AskUserQuestion") || answers.contains("timed out");
                    let status = if failed { "error" } else { "completed" };
                    self.emit(&event_tx, "tool_result", Some(step_index), None, Some(tool_id.clone()), Some("ask_user".into()), None, Some(answers.clone()), Some(status.into()), Some(0));
                    step_tool_records.push(ToolCallRecord {
                        id: tool_id.clone(),
                        name: "ask_user".into(),
                        arguments: args.clone(),
                        result: answers.clone(),
                        status: status.into(),
                        duration_ms: 0,
                    });
                    messages.push(ChatMessage {
                        role: "tool".into(), content: answers,
                        tool_calls: None, tool_call_id: Some(tc.id.clone()), name: Some("ask_user".into()),
                    });
                    continue;
                }

                // Approval logic: read-only tools are always auto-approved;
                // write/execute tools follow the configured policy (Auto mode
                // bypasses approval entirely).
                let readonly_tool = self.tool_registry.is_readonly(&tc.function.name);
                let requires_approval = !readonly_tool
                    && !matches!(self.config.approval.mode, crate::ai::agent::config::ApprovalMode::Auto);
                info!(tool_name=%tc.function.name, readonly_tool, requires_approval, approval_mode=?self.config.approval.mode, "checking tool approval");
                let approved = if requires_approval {
                    let elapsed = last_approval_at.map(|t| t.elapsed().as_secs());
                    let auto_approved = self.config.approval.is_auto_approved(&tc.function.name, elapsed);
                    info!(tool_name=%tc.function.name, auto_approved, elapsed_sec=?elapsed, "approval decision");
                    if auto_approved {
                        true
                    } else {
                        self.emit(&event_tx, "tool_approval_required", Some(step_index), None, Some(tool_id.clone()), Some(tc.function.name.clone()), Some(args.clone()), None, None, None);

                        match tokio::select! {
                            biased;
                            _ = self.cancel_token.cancelled() => None,
                            result = tokio::time::timeout(
                                std::time::Duration::from_secs(300),
                                approval_rx.recv(),
                            ) => Some(result),
                        } {
                            Some(Ok(Some(ApprovalResponse::Approved))) => {
                                last_approval_at = Some(std::time::Instant::now());
                                true
                            }
                            Some(Ok(Some(ApprovalResponse::ModifiedArgs(new_args)))) => {
                                args = new_args;
                                last_approval_at = Some(std::time::Instant::now());
                                true
                            }
                            Some(Ok(Some(ApprovalResponse::Declined))) | Some(Ok(None)) | None => false,
                            Some(Err(_)) => {
                                self.emit(&event_tx, "tool_result", Some(step_index), None, Some(tool_id.clone()), Some(tc.function.name.clone()), None, Some("Error: approval timeout".into()), Some("timeout".into()), None);
                                false
                            }
                        }
                    }
                } else {
                    true
                };

                if !approved {
                    let tool_result = "User declined the operation".to_string();
                    self.emit(&event_tx, "tool_result", Some(step_index), None, Some(tool_id.clone()), Some(tc.function.name.clone()), None, Some(tool_result.clone()), Some("error".into()), Some(0));
                    step_tool_records.push(ToolCallRecord {
                        id: tool_id.clone(),
                        name: tc.function.name.clone(),
                        arguments: args.clone(),
                        result: tool_result.clone(),
                        status: "error".into(),
                        duration_ms: 0,
                    });
                    messages.push(ChatMessage {
                        role: "tool".into(), content: tool_result.clone(),
                        tool_calls: None, tool_call_id: Some(tc.id.clone()), name: Some(tc.function.name.clone()),
                    });
                    continue;
                }

                let start = std::time::Instant::now();
                // Allow long-running tools (e.g. bash up to 5min), but abort
                // promptly when the user hits stop.
                let result = tokio::select! {
                    biased;
                    _ = self.cancel_token.cancelled() => {
                        info!(round, tool_name=%tc.function.name, "agent cancelled during tool execution");
                        cancelled = true;
                        break;
                    }
                    result = tokio::time::timeout(
                        std::time::Duration::from_secs(320),
                        self.tool_registry.execute(&tc.function.name, args.clone()),
                    ) => result,
                };

                let duration_ms = start.elapsed().as_millis() as i32;
                let (tool_result, tool_status) = match result {
                    Ok(Ok(output)) => {
                        self.emit(&event_tx, "tool_result", Some(step_index), None, Some(tool_id.clone()), Some(tc.function.name.clone()), None, Some(output.clone()), Some("completed".into()), Some(duration_ms));
                        (output, "completed")
                    }
                    Ok(Err(e)) => {
                        error!("tool error: {e}");
                        let msg = format!("Error: {e}");
                        self.emit(&event_tx, "tool_result", Some(step_index), None, Some(tool_id.clone()), Some(tc.function.name.clone()), None, Some(msg.clone()), Some("error".into()), Some(duration_ms));
                        (msg, "error")
                    }
                    Err(_) => {
                        let msg = "Error: timeout".to_string();
                        self.emit(&event_tx, "tool_result", Some(step_index), None, Some(tool_id.clone()), Some(tc.function.name.clone()), None, Some(msg.clone()), Some("timeout".into()), Some(duration_ms));
                        (msg, "timeout")
                    }
                };

                step_tool_records.push(ToolCallRecord {
                    id: tool_id.clone(),
                    name: tc.function.name.clone(),
                    arguments: args.clone(),
                    result: tool_result.clone(),
                    status: tool_status.into(),
                    duration_ms,
                });

                // Save tool execution
                let exec_id = uuid::Uuid::new_v4().to_string();
                let now = time::now_iso();
                let _ = sqlx::query(
                    "INSERT INTO tool_executions (id, session_id, tool_name, tool_input, tool_output, status, duration_ms, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
                ).bind(&exec_id).bind(&self.session_id).bind(&tc.function.name).bind(&tc.function.arguments).bind(&tool_result).bind(tool_status).bind(duration_ms).bind(&now).execute(&self.db).await;

                messages.push(ChatMessage {
                    role: "tool".into(), content: tool_result.clone(),
                    tool_calls: None, tool_call_id: Some(tc.id.clone()), name: Some(tc.function.name.clone()),
                });
            }

            // Persist this ReAct step.
            let tool_calls_json = serde_json::to_string(&step_tool_records).unwrap_or_default();
            let reasoning_for_memory = if round_reasoning.trim().is_empty() { None } else { Some(round_reasoning.as_str()) };
            let step = AgentStep {
                id: uuid::Uuid::new_v4().to_string(),
                session_id: sid.clone(),
                message_id: None,
                step_index,
                reasoning_content: reasoning_for_memory.map(|s| s.to_string()),
                tool_calls: if step_tool_records.is_empty() { None } else { Some(tool_calls_json.clone()) },
                created_at: time::now_iso(),
            };
            steps.push(step.clone());

            // Notify frontend that a full step is complete.
            self.emit(&event_tx, "step_complete", Some(step_index), None, None, None, None, None, None, None);

            info!(round, step_index, "tool round completed");
        }

        // The reply shown/persisted is ALL rounds' visible text — matching
        // what the user watched stream by. Falling back to the final round's
        // content only when nothing was ever streamed. When the budget was
        // exhausted by reasoning (finish_reason=length, no content anywhere),
        // say so explicitly instead of persisting an empty bubble.
        let reply = if !full_text.trim().is_empty() {
            full_text
        } else if !final_content.trim().is_empty() {
            final_content
        } else if truncated_empty {
            "（输出被截断：模型的思考过程占满了 max_tokens 额度，没有生成正文。请在「设置」中调大该模型的 max_tokens 后重试。）".to_string()
        } else {
            final_content
        };
        if cancelled {
            info!(content_len = reply.len(), "agent turn cancelled");
        } else {
            info!(content_len = reply.len(), "agent turn complete");
        }

        Ok((reply, steps, cancelled, usage))
    }
}
