use sqlx::SqlitePool;
use tracing::{error, info, info_span, warn};

use crate::ai::agent::config::AgentConfig;
use crate::ai::agent::memory::ConversationMemory;
use crate::ai::agent::memory_store::{MemoryRecord, MemoryStore};
use crate::ai::agent::tool_registry::ToolRegistry;
use crate::ai::llm::{self, ChatMessage, LlmClient, StreamEvent, ToolCall};
use crate::core::models::AgentStep;
use crate::core::time;

/// One event of an agent turn, streamed to the frontend over "agent:event".
/// Internally tagged: the wire shape is `{"type": "<variant>", "session_id":
/// ..., ...variant fields}` — the same keys the old all-Option struct
/// produced, except absent fields are omitted instead of serialized as null
/// (the frontend reads every field as optional, so both decode alike).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    Thinking {
        session_id: String,
        content: String,
    },
    Delta {
        session_id: String,
        step_index: i32,
        content: String,
    },
    Reasoning {
        session_id: String,
        step_index: i32,
        content: String,
    },
    ToolCall {
        session_id: String,
        step_index: i32,
        tool_call_id: String,
        tool_name: String,
        tool_args: serde_json::Value,
    },
    ToolResult {
        session_id: String,
        step_index: i32,
        tool_call_id: String,
        tool_name: String,
        tool_result: String,
        status: String,
        /// Absent for results that never ran (approval timeout).
        #[serde(skip_serializing_if = "Option::is_none")]
        duration_ms: Option<i32>,
    },
    ToolApprovalRequired {
        session_id: String,
        step_index: i32,
        tool_call_id: String,
        tool_name: String,
        tool_args: serde_json::Value,
    },
    StepComplete {
        session_id: String,
        step_index: i32,
    },
    /// The agent is waiting for structured answers (AskUserQuestion);
    /// `content` is the questions JSON.
    AskUser {
        session_id: String,
        content: String,
    },
    Done {
        session_id: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        tokens_used: Option<i32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tokens_in: Option<i32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tokens_in_hit: Option<i32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tokens_out: Option<i32>,
    },
    Cancelled {
        session_id: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        tokens_used: Option<i32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tokens_in: Option<i32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tokens_in_hit: Option<i32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tokens_out: Option<i32>,
    },
    Error {
        session_id: String,
        content: String,
    },
}

#[derive(Debug, Clone)]
pub enum ApprovalDecision {
    Approved,
    Declined,
    /// Declined, with user feedback handed to the agent so it can adjust
    /// course instead of retrying blindly; the turn continues.
    DeclinedWithGuidance(String),
    /// Declined and the whole turn ends (agent was on the wrong track).
    DeclinedStop,
    ModifiedArgs(serde_json::Value),
}

/// A user's answer to a tool-approval prompt. `tool_call_id` tags which
/// pending call it answers so a late response (arriving after the 300s
/// timeout of an earlier prompt) can be dropped instead of being consumed
/// by the next tool's wait.
#[derive(Debug, Clone)]
pub struct ApprovalResponse {
    pub decision: ApprovalDecision,
    pub tool_call_id: Option<String>,
}

/// Batches delta/reasoning stream fragments so the frontend receives one
/// merged event per ~40ms window instead of one per token. The wire protocol
/// is unchanged: same event types, `content` is the concatenated string and
/// the frontend keeps accumulating.
struct StreamBatcher {
    delta: String,
    reasoning: String,
    last_emit: std::time::Instant,
}

impl StreamBatcher {
    const FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(40);

    fn new() -> Self {
        Self {
            delta: String::new(),
            reasoning: String::new(),
            last_emit: std::time::Instant::now(),
        }
    }

    fn has_pending(&self) -> bool {
        !self.delta.is_empty() || !self.reasoning.is_empty()
    }

    /// Whether the current 40ms window has elapsed with fragments pending.
    fn due(&self) -> bool {
        self.has_pending() && self.last_emit.elapsed() >= Self::FLUSH_INTERVAL
    }

    /// Next instant at which pending fragments must go out.
    fn deadline(&self) -> tokio::time::Instant {
        tokio::time::Instant::from_std(self.last_emit + Self::FLUSH_INTERVAL)
    }

    fn push_delta(&mut self, c: &str) {
        self.delta.push_str(c);
    }

    fn push_reasoning(&mut self, c: &str) {
        self.reasoning.push_str(c);
    }

    /// Take all pending fragments, starting a new batching window.
    fn take(&mut self) -> (Option<String>, Option<String>) {
        self.last_emit = std::time::Instant::now();
        let delta = if self.delta.is_empty() { None } else { Some(std::mem::take(&mut self.delta)) };
        let reasoning = if self.reasoning.is_empty() { None } else { Some(std::mem::take(&mut self.reasoning)) };
        (delta, reasoning)
    }
}

/// Extract the tool_call id an approval response refers to. Id-less responses
/// (legacy callers) are treated as matching the pending call; tagged responses
/// for a different tool_call are dropped by the approval waiter.
fn approval_tool_call_id(resp: &ApprovalResponse) -> Option<&str> {
    resp.tool_call_id.as_deref()
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
    /// The effective config the client was built with (post vision-switch,
    /// post output-cap). Kept so a mid-turn retry can rebuild a client with a
    /// bumped max_tokens without losing the routing decisions.
    llm_config: crate::ai::llm::LlmConfig,
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
    /// Active long-term memory for this agent, appended to the system prompt.
    long_term_memory: Option<String>,
}

impl AgentEngine {
    pub fn new(
        llm: Box<dyn LlmClient>,
        llm_config: crate::ai::llm::LlmConfig,
        tool_registry: ToolRegistry,
        session_id: String,
        db: SqlitePool,
        config: AgentConfig,
        memory_store: MemoryStore,
        cancel_token: tokio_util::sync::CancellationToken,
        project_dir: Option<String>,
        context_prompt: Option<String>,
        long_term_memory: Option<String>,
    ) -> Self {
        let system_prompt = Some(config.effective_system_prompt());
        let max_tokens = config.effective_context_budget();
        let memory = ConversationMemory::new(max_tokens, system_prompt);
        Self {
            llm,
            llm_config,
            tool_registry,
            memory,
            memory_store,
            config,
            session_id,
            db,
            cancel_token,
            project_dir,
            context_prompt,
            long_term_memory,
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
        // Drop stale answers left in the channel by earlier (timed-out or
        // cancelled) prompts, so a late reply can never be consumed as the
        // answer to a different question.
        while ask_rx.try_recv().is_ok() {}
        self.emit(event_tx, |session_id| AgentEvent::AskUser {
            session_id,
            content: questions.to_string(),
        });
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

    /// Send one event, filling in this engine's session id. The closure
    /// builds the concrete `AgentEvent` variant, so call sites name their
    /// fields instead of passing a positional `None` list.
    fn emit(
        &self,
        event_tx: &tokio::sync::mpsc::UnboundedSender<AgentEvent>,
        build: impl FnOnce(String) -> AgentEvent,
    ) {
        let _ = event_tx.send(build(self.session_id.clone()));
    }

    /// Flush pending batched stream fragments as merged delta/reasoning
    /// events. Must run before any non-stream event (tool_call, tool_result,
    /// ask_user, tool_approval_required, step_complete, turn end) so event
    /// ordering matches what the model actually produced.
    fn flush_batcher(
        &self,
        event_tx: &tokio::sync::mpsc::UnboundedSender<AgentEvent>,
        step_index: i32,
        batcher: &mut StreamBatcher,
    ) {
        let (delta, reasoning) = batcher.take();
        if let Some(c) = delta {
            self.emit(event_tx, |session_id| AgentEvent::Delta { session_id, step_index, content: c });
        }
        if let Some(c) = reasoning {
            self.emit(event_tx, |session_id| AgentEvent::Reasoning { session_id, step_index, content: c });
        }
    }

    /// Process a user message with streaming output via event channel.
    /// Returns the final assistant content, the ReAct steps, whether the
    /// turn was cancelled, and the token usage across all LLM rounds.
    /// Does NOT emit the terminal done/cancelled event — the caller emits it
    /// after persisting the results.
    pub async fn process_message(
        &self,
        user_message: &str,
        attachments: Option<&str>,
        history: &[ChatMessage],
        event_tx: tokio::sync::mpsc::UnboundedSender<AgentEvent>,
        approval_rx: &mut tokio::sync::mpsc::UnboundedReceiver<ApprovalResponse>,
        ask_rx: &mut tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>,
    ) -> Result<(String, Vec<AgentStep>, bool, crate::ai::llm::LlmUsage, Option<String>), String> {
        let span = info_span!("agent_turn", session_id = %self.session_id);
        let _guard = span.enter();
        let sid = self.session_id.clone();

        info!(user_message_len = user_message.len(), history_len = history.len(), "starting agent turn");

        // Emit thinking
        self.emit(&event_tx, |session_id| AgentEvent::Thinking {
            session_id,
            content: "Analyzing request...".into(),
        });

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
            if let Some(ltm) = &self.long_term_memory {
                content.push_str(&format!("\n\n# 长期记忆\n{ltm}"));
            }
            messages.push(ChatMessage { role: "system".into(), content, attachments: None, tool_calls: None, tool_call_id: None, name: None });
        }
        messages.extend(history.iter().cloned());
        messages.push(ChatMessage { role: "user".into(), content: user_message.to_string(), attachments: attachments.map(|s| s.to_string()), tool_calls: None, tool_call_id: None, name: None });

        info!(message_count=%messages.len(), messages_summary=%messages.iter().map(|m| {
            let tc = m.tool_calls.as_ref().map(|v| v.len()).unwrap_or(0);
            format!("{}[c={},tc={}]", m.role, m.content.len(), tc)
        }).collect::<Vec<_>>().join(","), "agent messages built for LLM");

        let tool_defs = self.tool_registry.get_definitions();
        let max_loops = self.config.effective_max_loops();

        // Per-round output cap currently in effect. Starts at the configured
        // value; a truncation retry (ask_user) rebuilds the client with a
        // bumped cap and updates this.
        let mut output_cap = self.llm_config.max_tokens;
        // Retry client built on a cap bump; when present it serves all later
        // rounds of this turn (kept local so the turn stays &self).
        let mut retry_llm: Option<Box<dyn LlmClient>> = None;
        let mut cap_bumps = 0u32;

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
        // Set when the LLM call itself fails mid-turn (e.g. a context-size
        // 400 after several tool rounds). The turn then ends gracefully —
        // accumulated steps/text/usage are returned so the caller can
        // persist them exactly like a user-cancelled turn, instead of
        // throwing the whole turn's work away via Err.
        let mut turn_error: Option<String> = None;
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
                let active_llm: &dyn LlmClient = retry_llm.as_deref().unwrap_or(&*self.llm);
                messages.push(ChatMessage { role: "system".into(),
                    content: "Max tool calls reached. Provide your final answer now based on gathered information.".into(),
                    attachments: None, tool_calls: None, tool_call_id: None, name: None });
                let resp = match active_llm.chat_completion(&messages, &[]).await {
                    Ok(r) => r,
                    Err(e) => {
                        error!(error = %e, "LLM final-answer call failed");
                        turn_error = Some(format!("LLM: {e}"));
                        break;
                    }
                };
                final_content = resp.content;
                if !final_content.trim().is_empty() {
                    if !full_text.is_empty() { full_text.push_str("\n\n"); }
                    full_text.push_str(final_content.trim_end());
                }
                break;
            }
            round += 1;
            let step_index = round as i32;

            // Truncate when the estimated INPUT exceeds the context budget.
            // `reserved` is headroom for the model's OUTPUT — the per-round
            // output cap — NOT the context budget itself: reserving the full
            // budget zeroed the inner budget and dropped the entire history,
            // including the current user message.
            let context_budget = self.config.effective_context_budget();
            if ConversationMemory::estimate_messages_tokens(&messages) > context_budget {
                messages = self.memory.truncate(&messages, output_cap as usize);
            }

            let mut stream_text = String::new();
            let mut round_reasoning = String::new();
            let mut round_finish: Option<String> = None;
            let mut batcher = StreamBatcher::new();
            let mut tool_call_buf: std::collections::HashMap<u32, (String, String, String)> = std::collections::HashMap::new();

            // Stream one round. The borrow of retry_llm (via active_llm) is
            // scoped to this block so the truncation-retry below may replace
            // retry_llm with a bumped-cap client.
            {
                let active_llm: &dyn LlmClient = retry_llm.as_deref().unwrap_or(&*self.llm);
                // Accumulate streaming response + tool call deltas
                let (stream_tx, mut stream_rx) = tokio::sync::mpsc::unbounded_channel();
                let msgs_clone = messages.clone();
                let llm_fut = active_llm.chat_completion_stream(&msgs_clone, &tool_defs, stream_tx);

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
                                            batcher.push_delta(c);
                                        }
                                        if batcher.due() {
                                            self.flush_batcher(&event_tx, step_index, &mut batcher);
                                        }
                                    }
                                    "reasoning" => {
                                        if let Some(ref c) = content {
                                            round_reasoning.push_str(c);
                                            batcher.push_reasoning(c);
                                        }
                                        if batcher.due() {
                                            self.flush_batcher(&event_tx, step_index, &mut batcher);
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
                    // Flush batched delta/reasoning fragments once per
                    // 40ms window instead of emitting one event per token.
                    _ = tokio::time::sleep_until(batcher.deadline()), if batcher.has_pending() => {
                        self.flush_batcher(&event_tx, step_index, &mut batcher);
                    }
                    result = &mut llm_fut => {
                        if let Err(e) = result {
                            error!(error = %e, "LLM stream failed");
                            // Do NOT emit an "error" event here: the outer
                            // caller (commands/agent.rs) reports the failure,
                            // and emitting both renders two assistant error
                            // bubbles for one failure. Do NOT return Err
                            // either: record it and end the turn gracefully so
                            // the rounds/steps already produced are persisted
                            // (same path as a user cancel).
                            turn_error = Some(format!("LLM: {e}"));
                            break;
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
                            batcher.push_delta(c);
                        }
                    }
                    "reasoning" => {
                        if let Some(ref c) = event.content {
                            round_reasoning.push_str(c);
                            batcher.push_reasoning(c);
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
            } // end scoped stream block (drops the retry_llm borrow)

            // Force-flush any buffered stream fragments before anything else
            // is emitted this round (ask_user, tool_call, tool_result,
            // tool_approval_required, step_complete). The batcher is only fed
            // during streaming, so this single flush keeps every later event
            // of the round correctly ordered.
            self.flush_batcher(&event_tx, step_index, &mut batcher);

            // Fold this round's visible text into the full reply (see
            // full_text above).
            if !stream_text.trim().is_empty() {
                if !full_text.is_empty() { full_text.push_str("\n\n"); }
                full_text.push_str(stream_text.trim_end());
            }
            // Mid-turn LLM failure: stop the ReAct loop here (never execute
            // tool calls or start another round against a failing backend).
            // Keep the failed round's partial reasoning as a step so the
            // 推理过程 card reflects everything the model did before dying.
            if turn_error.is_some() {
                if !round_reasoning.trim().is_empty() {
                    steps.push(AgentStep {
                        id: uuid::Uuid::new_v4().to_string(),
                        session_id: sid.clone(),
                        message_id: None,
                        step_index,
                        reasoning_content: Some(round_reasoning.clone()),
                        tool_calls: None,
                        created_at: time::now_iso(),
                    });
                }
                break;
            }
            if stream_text.trim().is_empty() && round_finish.as_deref() == Some("length") {
                truncated_empty = true;
                // Reasoning exhausted the completion budget with no visible
                // output. Offer the user a one-off cap bump and continue the
                // ReAct loop with the rebuilt client — never replay the turn
                // (earlier tool side effects must not repeat). The model
                // cannot ask this itself: in this failure mode it produced no
                // tool call at all, so the engine asks on its behalf.
                if !cancelled && cap_bumps < 3 {
                    let next = output_cap.saturating_mul(2);
                    let questions = serde_json::json!({ "questions": [{
                        "question": format!("模型的思考过程占满了单轮输出额度（当前 max_tokens={output_cap}），没有生成正文。要调大额度重试吗？"),
                        "header": "输出额度不足",
                        "options": [
                            { "label": format!("调大到 {next} 重试"), "description": "仅本轮对话生效；长期调整请改会话设置或模型配置" },
                            { "label": "不重试" }
                        ]
                    }]});
                    let answers = self.handle_ask_user(&event_tx, &questions, ask_rx).await;
                    if answers.contains("调大到") {
                        let mut cfg = self.llm_config.clone();
                        cfg.max_tokens = next;
                        match llm::client::create_llm_client(&cfg) {
                            Ok(client) => {
                                info!(old_cap = output_cap, new_cap = next, round, "retrying round with bumped max_tokens");
                                retry_llm = Some(client);
                                output_cap = next;
                                cap_bumps += 1;
                                truncated_empty = false;
                                continue;
                            }
                            Err(e) => {
                                warn!(error = %e, "failed to rebuild LLM client for cap bump");
                            }
                        }
                    }
                }
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
            messages.push(ChatMessage { role: "assistant".into(), content: stream_text.clone(), attachments: None, tool_calls: Some(normalized_calls), tool_call_id: None, name: None });

            // Execute each tool call and collect records for this step.
            let mut step_tool_records: Vec<ToolCallRecord> = Vec::new();
            info!(approval_mode=?self.config.approval.mode, "starting tool execution round");
            let mut call_idx = 0;
            while call_idx < tool_calls.len() {
                if self.is_cancelled() {
                    info!(round, "agent cancelled during tool execution");
                    cancelled = true;
                    break;
                }

                // Collect a run of consecutive calls eligible for PARALLEL
                // execution: read-only, approval-exempt, not ask_user, and
                // well-formed args. Approval-gated/write/ask_user/malformed
                // calls fall through to the serial path below. ToolRegistry
                // is Send+Sync, so its execute futures can be driven together
                // with join_all while results are written back in the
                // original tool_calls order.
                let mut batch: Vec<(usize, String, serde_json::Value)> = Vec::new();
                while call_idx < tool_calls.len() {
                    let tc = &tool_calls[call_idx];
                    let readonly_tool = self.tool_registry.is_readonly(&tc.function.name);
                    let requires_approval = match self.config.approval.mode {
                        crate::ai::agent::config::ApprovalMode::Auto => false,
                        crate::ai::agent::config::ApprovalMode::ManualAll => true,
                        _ => !readonly_tool,
                    };
                    let parsed = serde_json::from_str::<serde_json::Value>(&tc.function.arguments)
                        .ok()
                        .map(Self::normalize_args);
                    if tc.function.name == "ask_user" || !readonly_tool || requires_approval || parsed.is_none() {
                        break;
                    }
                    let args = parsed.unwrap();
                    let tool_id = format!("tool_{}", uuid::Uuid::new_v4());
                    self.emit(&event_tx, |session_id| AgentEvent::ToolCall {
                        session_id,
                        step_index,
                        tool_call_id: tool_id.clone(),
                        tool_name: tc.function.name.clone(),
                        tool_args: args.clone(),
                    });
                    batch.push((call_idx, tool_id, args));
                    call_idx += 1;
                }

                if !batch.is_empty() {
                    info!(batch_size = batch.len(), "executing read-only tools in parallel");
                    let futs: Vec<_> = batch
                        .iter()
                        .map(|(i, _, args)| {
                            let tc = &tool_calls[*i];
                            async move {
                                let start = std::time::Instant::now();
                                let result = tokio::time::timeout(
                                    std::time::Duration::from_secs(320),
                                    self.tool_registry.execute(&tc.function.name, args.clone()),
                                )
                                .await;
                                (result, start.elapsed().as_millis() as i32)
                            }
                        })
                        .collect();
                    let results = tokio::select! {
                        biased;
                        _ = self.cancel_token.cancelled() => {
                            info!(round, "agent cancelled during parallel tool execution");
                            cancelled = true;
                            None
                        }
                        r = futures::future::join_all(futs) => Some(r),
                    };
                    let Some(results) = results else { break };
                    for ((i, tool_id, args), (result, duration_ms)) in batch.iter().zip(results) {
                        let tc = &tool_calls[*i];
                        let (tool_result, tool_status) = match result {
                            Ok(Ok(output)) => {
                                self.emit(&event_tx, |session_id| AgentEvent::ToolResult {
                                    session_id,
                                    step_index,
                                    tool_call_id: tool_id.clone(),
                                    tool_name: tc.function.name.clone(),
                                    tool_result: output.clone(),
                                    status: "completed".into(),
                                    duration_ms: Some(duration_ms),
                                });
                                (output, "completed")
                            }
                            Ok(Err(e)) => {
                                error!("tool error: {e}");
                                let msg = format!("Error: {e}");
                                self.emit(&event_tx, |session_id| AgentEvent::ToolResult {
                                    session_id,
                                    step_index,
                                    tool_call_id: tool_id.clone(),
                                    tool_name: tc.function.name.clone(),
                                    tool_result: msg.clone(),
                                    status: "error".into(),
                                    duration_ms: Some(duration_ms),
                                });
                                (msg, "error")
                            }
                            Err(_) => {
                                let msg = "Error: timeout".to_string();
                                self.emit(&event_tx, |session_id| AgentEvent::ToolResult {
                                    session_id,
                                    step_index,
                                    tool_call_id: tool_id.clone(),
                                    tool_name: tc.function.name.clone(),
                                    tool_result: msg.clone(),
                                    status: "timeout".into(),
                                    duration_ms: Some(duration_ms),
                                });
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
                        let exec_id = uuid::Uuid::new_v4().to_string();
                        let now = time::now_iso();
                        let _ = sqlx::query(
                            "INSERT INTO tool_executions (id, session_id, tool_name, tool_input, tool_output, status, duration_ms, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
                        ).bind(&exec_id).bind(&self.session_id).bind(&tc.function.name).bind(&tc.function.arguments).bind(&tool_result).bind(tool_status).bind(duration_ms).bind(&now).execute(&self.db).await;
                        messages.push(ChatMessage {
                            role: "tool".into(), content: tool_result.clone(),
                            attachments: None, tool_calls: None, tool_call_id: Some(tc.id.clone()), name: Some(tc.function.name.clone()),
                        });
                    }
                    continue;
                }

                // — Serial path: approval-gated, write, ask_user, or malformed
                // args; semantics unchanged from the original per-call loop. —
                let tc = &tool_calls[call_idx];
                let tool_id = format!("tool_{}", uuid::Uuid::new_v4());
                // 参数 JSON 本身非法时不能拿 Value::Null 硬跑——工具只会报
                // "path required" 之类的错,模型误以为参数缺失而原样重发。
                // 标记该调用失败,把解析错误和原始参数前缀回给模型,让它修正
                // JSON 后重试;引擎循环不中断。
                let mut args: serde_json::Value = match serde_json::from_str(&tc.function.arguments) {
                    Ok(v) => Self::normalize_args(v),
                    Err(e) => {
                        let prefix: String = tc.function.arguments.chars().take(200).collect();
                        let tool_result = format!("参数 JSON 解析失败:{e};原始参数前缀:{prefix}");
                        self.emit(&event_tx, |session_id| AgentEvent::ToolCall {
                            session_id,
                            step_index,
                            tool_call_id: tool_id.clone(),
                            tool_name: tc.function.name.clone(),
                            tool_args: serde_json::Value::Null,
                        });
                        self.emit(&event_tx, |session_id| AgentEvent::ToolResult {
                            session_id,
                            step_index,
                            tool_call_id: tool_id.clone(),
                            tool_name: tc.function.name.clone(),
                            tool_result: tool_result.clone(),
                            status: "error".into(),
                            duration_ms: Some(0),
                        });
                        step_tool_records.push(ToolCallRecord {
                            id: tool_id.clone(),
                            name: tc.function.name.clone(),
                            arguments: serde_json::Value::Null,
                            result: tool_result.clone(),
                            status: "error".into(),
                            duration_ms: 0,
                        });
                        messages.push(ChatMessage {
                            role: "tool".into(), content: tool_result,
                            attachments: None, tool_calls: None, tool_call_id: Some(tc.id.clone()), name: Some(tc.function.name.clone()),
                        });
                        call_idx += 1;
                        continue;
                    }
                };

                self.emit(&event_tx, |session_id| AgentEvent::ToolCall {
                    session_id,
                    step_index,
                    tool_call_id: tool_id.clone(),
                    tool_name: tc.function.name.clone(),
                    tool_args: args.clone(),
                });

                // AskUserQuestion: handled inline by the engine — emit an
                // `ask_user` event and wait for the user's answers.
                if tc.function.name == "ask_user" {
                    let answers = self.handle_ask_user(&event_tx, &args, ask_rx).await;
                    let failed = answers.starts_with("AskUserQuestion") || answers.contains("timed out");
                    let status = if failed { "error" } else { "completed" };
                    self.emit(&event_tx, |session_id| AgentEvent::ToolResult {
                        session_id,
                        step_index,
                        tool_call_id: tool_id.clone(),
                        tool_name: "ask_user".into(),
                        tool_result: answers.clone(),
                        status: status.into(),
                        duration_ms: Some(0),
                    });
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
                        attachments: None, tool_calls: None, tool_call_id: Some(tc.id.clone()), name: Some("ask_user".into()),
                    });
                    call_idx += 1;
                    continue;
                }

                // Approval logic: Auto bypasses approval entirely; ManualAll
                // gates every tool (read-only included, to throttle runaway
                // exploration); the other modes gate write/execute tools only.
                // (`ask_user` bypasses approval — it IS the user interaction.)
                let readonly_tool = self.tool_registry.is_readonly(&tc.function.name);
                let requires_approval = match self.config.approval.mode {
                    crate::ai::agent::config::ApprovalMode::Auto => false,
                    crate::ai::agent::config::ApprovalMode::ManualAll => true,
                    _ => !readonly_tool,
                };
                info!(tool_name=%tc.function.name, readonly_tool, requires_approval, approval_mode=?self.config.approval.mode, "checking tool approval");
                let mut decline_guidance: Option<String> = None;
                let mut decline_stop = false;
                let approved = if requires_approval {
                    let elapsed = last_approval_at.map(|t| t.elapsed().as_secs());
                    let auto_approved = self.config.approval.is_auto_approved(&tc.function.name, elapsed);
                    info!(tool_name=%tc.function.name, auto_approved, elapsed_sec=?elapsed, "approval decision");
                    if auto_approved {
                        true
                    } else {
                        self.emit(&event_tx, |session_id| AgentEvent::ToolApprovalRequired {
                            session_id,
                            step_index,
                            tool_call_id: tool_id.clone(),
                            tool_name: tc.function.name.clone(),
                            tool_args: args.clone(),
                        });

                        // Drain stale responses left in the channel by earlier
                        // waits (e.g. a click that landed after the 300s
                        // timeout), so an "allow" meant for tool A can never
                        // be consumed as the answer for tool B.
                        while approval_rx.try_recv().is_ok() {}

                        enum ApprovalWait {
                            Response(Option<ApprovalResponse>),
                            TimedOut,
                        }

                        // Wait up to 300s. Responses tagged with a tool_call_id
                        // that does not match the pending call are dropped and
                        // the wait continues; id-less responses (legacy
                        // callers) always match.
                        let wait = tokio::select! {
                            biased;
                            _ = self.cancel_token.cancelled() => None,
                            outcome = async {
                                let deadline = tokio::time::sleep(std::time::Duration::from_secs(300));
                                tokio::pin!(deadline);
                                loop {
                                    tokio::select! {
                                        resp = approval_rx.recv() => {
                                            match resp {
                                                Some(resp) => {
                                                    let matches = match approval_tool_call_id(&resp) {
                                                        Some(id) => id == tool_id || (!tc.id.is_empty() && id == tc.id),
                                                        None => true,
                                                    };
                                                    if matches {
                                                        break ApprovalWait::Response(Some(resp));
                                                    }
                                                    warn!(tool_name=%tc.function.name, tool_id=%tool_id, "dropping approval response tagged for a different tool_call");
                                                }
                                                None => break ApprovalWait::Response(None),
                                            }
                                        }
                                        _ = &mut deadline => break ApprovalWait::TimedOut,
                                    }
                                }
                            } => Some(outcome),
                        };
                        match wait {
                            Some(ApprovalWait::Response(Some(resp))) => {
                                match resp.decision {
                                    ApprovalDecision::Approved => {
                                        last_approval_at = Some(std::time::Instant::now());
                                        true
                                    }
                                    ApprovalDecision::ModifiedArgs(new_args) => {
                                        args = new_args;
                                        last_approval_at = Some(std::time::Instant::now());
                                        true
                                    }
                                    ApprovalDecision::DeclinedWithGuidance(g) => {
                                        decline_guidance = Some(g);
                                        false
                                    }
                                    ApprovalDecision::DeclinedStop => {
                                        decline_stop = true;
                                        false
                                    }
                                    ApprovalDecision::Declined => false,
                                }
                            }
                            Some(ApprovalWait::Response(None)) | None => false,
                            Some(ApprovalWait::TimedOut) => {
                                self.emit(&event_tx, |session_id| AgentEvent::ToolResult {
                                    session_id,
                                    step_index,
                                    tool_call_id: tool_id.clone(),
                                    tool_name: tc.function.name.clone(),
                                    tool_result: "Error: approval timeout".into(),
                                    status: "timeout".into(),
                                    duration_ms: None,
                                });
                                false
                            }
                        }
                    }
                } else {
                    true
                };

                if !approved {
                    let tool_result = match &decline_guidance {
                        Some(g) if !g.trim().is_empty() => {
                            format!("User declined the operation. User feedback: {g}")
                        }
                        _ => "User declined the operation".to_string(),
                    };
                    self.emit(&event_tx, |session_id| AgentEvent::ToolResult {
                        session_id,
                        step_index,
                        tool_call_id: tool_id.clone(),
                        tool_name: tc.function.name.clone(),
                        tool_result: tool_result.clone(),
                        status: "error".into(),
                        duration_ms: Some(0),
                    });
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
                        attachments: None, tool_calls: None, tool_call_id: Some(tc.id.clone()), name: Some(tc.function.name.clone()),
                    });
                    if decline_stop {
                        info!(tool_name=%tc.function.name, "user declined with stop; ending turn");
                        cancelled = true;
                        break;
                    }
                    call_idx += 1;
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
                        self.emit(&event_tx, |session_id| AgentEvent::ToolResult {
                            session_id,
                            step_index,
                            tool_call_id: tool_id.clone(),
                            tool_name: tc.function.name.clone(),
                            tool_result: output.clone(),
                            status: "completed".into(),
                            duration_ms: Some(duration_ms),
                        });
                        (output, "completed")
                    }
                    Ok(Err(e)) => {
                        error!("tool error: {e}");
                        let msg = format!("Error: {e}");
                        self.emit(&event_tx, |session_id| AgentEvent::ToolResult {
                            session_id,
                            step_index,
                            tool_call_id: tool_id.clone(),
                            tool_name: tc.function.name.clone(),
                            tool_result: msg.clone(),
                            status: "error".into(),
                            duration_ms: Some(duration_ms),
                        });
                        (msg, "error")
                    }
                    Err(_) => {
                        let msg = "Error: timeout".to_string();
                        self.emit(&event_tx, |session_id| AgentEvent::ToolResult {
                            session_id,
                            step_index,
                            tool_call_id: tool_id.clone(),
                            tool_name: tc.function.name.clone(),
                            tool_result: msg.clone(),
                            status: "timeout".into(),
                            duration_ms: Some(duration_ms),
                        });
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
                    attachments: None, tool_calls: None, tool_call_id: Some(tc.id.clone()), name: Some(tc.function.name.clone()),
                });
                call_idx += 1;
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
            self.emit(&event_tx, |session_id| AgentEvent::StepComplete { session_id, step_index });

            // Decline-and-stop (or a cancel that arrived mid-step) ends the
            // turn here; token cancels are also caught at the loop head.
            if cancelled {
                break;
            }

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
            format!("输出被截断：模型的思考过程占满了单轮输出额度（max_tokens={output_cap}），没有生成正文。请调大该智能体的 max_tokens（会话设置）或模型的 max_tokens（设置 → 模型）后重试。")
        } else {
            final_content
        };
        if cancelled {
            info!(content_len = reply.len(), "agent turn cancelled");
        } else if turn_error.is_some() {
            info!(content_len = reply.len(), "agent turn ended on LLM error (partial results kept)");
        } else {
            info!(content_len = reply.len(), "agent turn complete");
        }

        Ok((reply, steps, cancelled, usage, turn_error))
    }
}
