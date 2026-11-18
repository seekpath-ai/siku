//! In-memory live state for one running agent turn.
//!
//! The frontend used to keep streaming steps / pending approval / pending
//! AskUserQuestion only in the store of whichever session happened to be
//! active. Switching away dropped that state, and switching back showed only
//! "thinking". This module is the backend-side snapshot: the event forwarder
//! applies every `AgentEvent` here before emitting it, and a session switch can
//! ask `agent_get_live_state` for the current snapshot.
//!
//! It is deliberately memory-only: a live turn dies with the process, while a
//! finished turn is loaded from the database as before. `turn_id` keeps an old
//! turn's cleanup from deleting a newer turn's snapshot.

use serde::Serialize;
use serde_json::Value;

use crate::ai::agent::engine::AgentEvent;

/// Hard caps: a single turn must not duplicate an unbounded tool result or a
/// runaway stream into backend memory.
const MAX_LIVE_STEPS: usize = 100;
const MAX_TOOL_CALLS_PER_STEP: usize = 64;
const MAX_TOOL_RESULT_CHARS: usize = 64 * 1024;
const MAX_STREAM_CONTENT_CHARS: usize = 512 * 1024;
const MAX_PENDING_JSON_CHARS: usize = 64 * 1024;

/// A tool call as it should appear in the frontend's `ToolCallInfo`.
#[derive(Clone, Debug, Serialize)]
pub struct LiveToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i32>,
    pub outside_write_base: bool,
}

impl LiveToolCall {
    fn new(id: String, name: String, arguments: Value, status: &str) -> Self {
        Self {
            id,
            name,
            arguments,
            result: None,
            status: status.to_string(),
            duration_ms: None,
            outside_write_base: false,
        }
    }
}

/// A ReAct step as it should appear in the frontend's `StreamingStep`.
#[derive(Clone, Debug, Serialize)]
pub struct LiveStep {
    pub step_index: i32,
    pub reasoning_content: String,
    pub tool_calls: Vec<LiveToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub narration: Option<String>,
    pub status: String,
}

impl LiveStep {
    fn new(step_index: i32) -> Self {
        Self {
            step_index,
            reasoning_content: String::new(),
            tool_calls: Vec::new(),
            narration: None,
            status: "streaming".to_string(),
        }
    }
}

/// The interaction the turn is currently blocked on. The frontend uses `kind`
/// to decide between 批准/拒绝 and 回答.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LivePending {
    Approval {
        tool_call_id: String,
        tool_name: String,
        tool_args: Value,
        outside_write_base: bool,
    },
    Question {
        questions: Value,
    },
}

/// Snapshot returned by `agent_get_live_state`.
#[derive(Clone, Debug, Serialize)]
pub struct AgentLiveState {
    /// Identifies the turn, so cleanup cannot remove a newer turn's state.
    /// Not needed by the frontend.
    #[serde(skip)]
    pub turn_id: String,
    pub running: bool,
    /// Monotonic per-turn revision. The frontend can use it to reject a
    /// snapshot that is older than the state it already applied.
    pub revision: u64,
    pub stream_content: String,
    pub stream_text_step: Option<i32>,
    /// Completed steps of the current turn.
    pub steps: Vec<LiveStep>,
    /// The step currently being streamed.
    pub current_step: Option<LiveStep>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending: Option<LivePending>,
    pub updated_at: String,
}

impl AgentLiveState {
    pub fn start(turn_id: String) -> Self {
        Self {
            turn_id,
            running: true,
            revision: 0,
            stream_content: String::new(),
            stream_text_step: None,
            steps: Vec::new(),
            current_step: None,
            pending: None,
            updated_at: crate::core::time::now_iso(),
        }
    }

    /// Apply one forwarded `AgentEvent` and bump the revision.
    pub fn apply(&mut self, event: &AgentEvent) {
        // Any event other than a new pending request means the turn resumed
        // after the previous approval/answer. Without this, switching back
        // right after answering would resurrect the old AskUserQuestion from
        // the still-unchanged snapshot.
        if !matches!(
            event,
            AgentEvent::AskUser { .. } | AgentEvent::ToolApprovalRequired { .. }
        ) {
            self.pending = None;
        }
        match event {
            AgentEvent::Thinking { .. } => {
                self.running = true;
            }
            AgentEvent::Reasoning {
                step_index,
                content,
                ..
            } => {
                self.running = true;
                self.ensure_step(*step_index);
                if let Some(step) = self.current_step.as_mut() {
                    step.reasoning_content.push_str(content);
                }
            }
            AgentEvent::Delta {
                step_index,
                content,
                ..
            } => {
                self.running = true;
                // Mirror chatStore.appendStreamContent: when the step changes,
                // the previous streamed text becomes that step's narration.
                if self.stream_text_step.is_some()
                    && self.stream_text_step != Some(*step_index)
                    && !self.stream_content.is_empty()
                {
                    let old = self.stream_text_step;
                    let text = std::mem::take(&mut self.stream_content);
                    if let Some(old_idx) = old {
                        self.set_narration(old_idx, text);
                    }
                    self.stream_content = content.clone();
                } else {
                    self.stream_content.push_str(content);
                }
                self.stream_text_step = Some(*step_index);
                truncate_chars(&mut self.stream_content, MAX_STREAM_CONTENT_CHARS);
            }
            AgentEvent::ToolCall {
                step_index,
                tool_call_id,
                tool_name,
                tool_args,
                ..
            } => {
                self.running = true;
                self.ensure_step(*step_index);
                self.migrate_stream_to_narration(*step_index);
                self.upsert_tool_call(
                    *step_index,
                    LiveToolCall::new(
                        tool_call_id.clone(),
                        tool_name.clone(),
                        tool_args.clone(),
                        "running",
                    ),
                );
            }
            AgentEvent::ToolApprovalRequired {
                step_index,
                tool_call_id,
                tool_name,
                tool_args,
                outside_write_base,
                ..
            } => {
                self.running = true;
                self.ensure_step(*step_index);
                self.migrate_stream_to_narration(*step_index);
                let mut call = LiveToolCall::new(
                    tool_call_id.clone(),
                    tool_name.clone(),
                    tool_args.clone(),
                    "pending",
                );
                call.outside_write_base = *outside_write_base;
                self.upsert_tool_call(*step_index, call);
                self.pending = Some(LivePending::Approval {
                    tool_call_id: tool_call_id.clone(),
                    tool_name: tool_name.clone(),
                    tool_args: tool_args.clone(),
                    outside_write_base: *outside_write_base,
                });
            }
            AgentEvent::ToolResult {
                step_index,
                tool_call_id,
                tool_result,
                status,
                duration_ms,
                ..
            } => {
                self.running = true;
                self.ensure_step(*step_index);
                self.upsert_tool_result(
                    *step_index,
                    tool_call_id,
                    tool_result,
                    status,
                    *duration_ms,
                );
                if matches!(
                    &self.pending,
                    Some(LivePending::Approval { tool_call_id: pending, .. })
                        if pending == tool_call_id
                ) {
                    self.pending = None;
                }
            }
            AgentEvent::StepComplete { step_index, .. } => {
                self.finalize_step(*step_index);
            }
            AgentEvent::AskUser { content, .. } => {
                self.running = true;
                self.pending = Some(LivePending::Question {
                    questions: parse_questions(content),
                });
            }
            AgentEvent::Done { .. } => {
                self.running = false;
                self.pending = None;
                self.finalize_current();
            }
            AgentEvent::Cancelled { .. } | AgentEvent::Error { .. } => {
                self.running = false;
                self.pending = None;
            }
        }
        self.revision += 1;
        self.updated_at = crate::core::time::now_iso();
    }

    /// The user answered/submitted the pending interaction. Clear it now,
    /// before the engine emits its next event, so an immediate session switch
    /// cannot resurrect the already-answered question/approval.
    pub fn clear_pending(&mut self) {
        if self.pending.take().is_some() {
            self.revision += 1;
            self.updated_at = crate::core::time::now_iso();
        }
    }

    /// An approval response was submitted. The tool call is no longer
    /// "pending": mark it running so an immediate switch-back does not render
    /// the approval card again before the engine's ToolResult arrives.
    pub fn mark_approval_submitted(&mut self) {
        self.pending = None;
        if let Some(step) = self.current_step.as_mut() {
            for call in &mut step.tool_calls {
                if call.status == "pending" {
                    call.status = "running".to_string();
                }
            }
        }
        self.revision += 1;
        self.updated_at = crate::core::time::now_iso();
    }

    fn ensure_step(&mut self, step_index: i32) {
        if self
            .current_step
            .as_ref()
            .is_some_and(|step| step.step_index == step_index)
        {
            return;
        }
        self.finalize_current();
        self.current_step = Some(LiveStep::new(step_index));
    }

    fn finalize_current(&mut self) {
        if let Some(mut step) = self.current_step.take() {
            step.status = "completed".to_string();
            if self.steps.len() < MAX_LIVE_STEPS {
                self.steps.push(step);
            }
        }
    }

    fn finalize_step(&mut self, step_index: i32) {
        if self
            .current_step
            .as_ref()
            .is_some_and(|step| step.step_index == step_index)
        {
            self.finalize_current();
        }
    }

    fn upsert_tool_call(&mut self, step_index: i32, call: LiveToolCall) {
        self.ensure_step(step_index);
        let Some(step) = self.current_step.as_mut() else {
            return;
        };
        if let Some(existing) = step.tool_calls.iter_mut().find(|c| c.id == call.id) {
            // Preserve result/status fields learned from an earlier event when
            // a duplicate ToolCall arrives, but refresh the descriptor.
            existing.name = call.name;
            existing.arguments = call.arguments;
            existing.outside_write_base = call.outside_write_base;
            if existing.status == "pending" || existing.status == "running" {
                existing.status = call.status;
            }
            return;
        }
        if step.tool_calls.len() < MAX_TOOL_CALLS_PER_STEP {
            step.tool_calls.push(call);
        }
    }

    fn upsert_tool_result(
        &mut self,
        step_index: i32,
        tool_call_id: &str,
        result: &str,
        status: &str,
        duration_ms: Option<i32>,
    ) {
        self.ensure_step(step_index);
        let Some(step) = self.current_step.as_mut() else {
            return;
        };
        let Some(call) = step.tool_calls.iter_mut().find(|c| c.id == tool_call_id) else {
            // A result without a preceding ToolCall (e.g. older backend): keep
            // it addressable instead of dropping it.
            if step.tool_calls.len() < MAX_TOOL_CALLS_PER_STEP {
                let mut call =
                    LiveToolCall::new(tool_call_id.to_string(), String::new(), Value::Null, status);
                call.result = Some(truncate_string(result, MAX_TOOL_RESULT_CHARS));
                call.duration_ms = duration_ms;
                step.tool_calls.push(call);
            }
            return;
        };
        call.result = Some(truncate_string(result, MAX_TOOL_RESULT_CHARS));
        call.status = status.to_string();
        call.duration_ms = duration_ms;
    }

    fn migrate_stream_to_narration(&mut self, step_index: i32) {
        if self.stream_text_step != Some(step_index) || self.stream_content.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.stream_content);
        self.stream_text_step = None;
        self.set_narration(step_index, text);
    }

    fn set_narration(&mut self, step_index: i32, text: String) {
        if let Some(step) = self.current_step.as_mut() {
            if step.step_index == step_index && step.narration.is_none() {
                step.narration = Some(text);
                return;
            }
        }
        if let Some(step) = self.steps.iter_mut().find(|s| s.step_index == step_index) {
            if step.narration.is_none() {
                step.narration = Some(text);
            }
        }
    }
}

fn parse_questions(content: &str) -> Value {
    let Ok(raw) = serde_json::from_str::<Value>(content) else {
        return Value::Null;
    };
    let mut out = raw.get("questions").cloned().unwrap_or_else(|| raw.clone());
    truncate_json(&mut out, MAX_PENDING_JSON_CHARS);
    out
}

fn truncate_json(value: &mut Value, max_chars: usize) {
    let encoded = value.to_string();
    if encoded.chars().count() <= max_chars {
        return;
    }
    *value = Value::String(truncate_string(&encoded, max_chars));
}

fn truncate_string(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut out: String = value.chars().take(max_chars).collect();
    out.push_str("\n…（已截断）");
    out
}

fn truncate_chars(value: &mut String, max_chars: usize) {
    if value.chars().count() > max_chars {
        *value = truncate_string(value, max_chars);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::agent::engine::AgentEvent;

    fn event_approval() -> AgentEvent {
        AgentEvent::ToolApprovalRequired {
            session_id: "s".into(),
            step_index: 0,
            tool_call_id: "call-1".into(),
            tool_name: "bash".into(),
            tool_args: serde_json::json!({"command": "ls"}),
            outside_write_base: false,
        }
    }

    #[test]
    fn reasoning_delta_and_tool_approval_are_recorded() {
        let mut state = AgentLiveState::start("turn".into());
        state.apply(&AgentEvent::Reasoning {
            session_id: "s".into(),
            step_index: 0,
            content: "think".into(),
        });
        state.apply(&AgentEvent::Delta {
            session_id: "s".into(),
            step_index: 0,
            content: "hello".into(),
        });
        state.apply(&AgentEvent::ToolCall {
            session_id: "s".into(),
            step_index: 0,
            tool_call_id: "call-1".into(),
            tool_name: "bash".into(),
            tool_args: serde_json::json!({"command": "ls"}),
        });
        state.apply(&event_approval());

        let current = state.current_step.as_ref().expect("current step");
        assert_eq!(current.reasoning_content, "think");
        assert_eq!(current.narration.as_deref(), Some("hello"));
        assert_eq!(current.tool_calls[0].status, "pending");
        assert!(matches!(
            state.pending,
            Some(LivePending::Approval { ref tool_call_id, .. }) if tool_call_id == "call-1"
        ));
    }

    #[test]
    fn tool_result_clears_pending_and_step_complete_moves_step() {
        let mut state = AgentLiveState::start("turn".into());
        state.apply(&event_approval());
        state.apply(&AgentEvent::ToolResult {
            session_id: "s".into(),
            step_index: 0,
            tool_call_id: "call-1".into(),
            tool_name: "bash".into(),
            tool_result: "ok".into(),
            status: "completed".into(),
            duration_ms: Some(12),
        });
        assert!(state.pending.is_none());
        state.apply(&AgentEvent::StepComplete {
            session_id: "s".into(),
            step_index: 0,
        });
        assert!(state.current_step.is_none());
        assert_eq!(state.steps.len(), 1);
        assert_eq!(state.steps[0].tool_calls[0].result.as_deref(), Some("ok"));
    }

    #[test]
    fn ask_user_sets_pending_question() {
        let mut state = AgentLiveState::start("turn".into());
        state.apply(&AgentEvent::AskUser {
            session_id: "s".into(),
            content: r#"{"questions":[{"question":"继续？","options":[]}]}"#.into(),
        });
        match state.pending {
            Some(LivePending::Question { questions }) => {
                assert!(questions.is_array());
            }
            _ => panic!("expected pending question"),
        }
    }

    #[test]
    fn pending_question_clears_when_turn_resumes() {
        let mut state = AgentLiveState::start("turn".into());
        state.apply(&AgentEvent::AskUser {
            session_id: "s".into(),
            content: r#"{"questions":[{"question":"继续？","options":[]}]}"#.into(),
        });
        assert!(state.pending.is_some());
        state.apply(&AgentEvent::Reasoning {
            session_id: "s".into(),
            step_index: 1,
            content: "continue".into(),
        });
        assert!(state.pending.is_none());
    }

    #[test]
    fn done_clears_pending_and_marks_not_running() {
        let mut state = AgentLiveState::start("turn".into());
        state.apply(&event_approval());
        state.apply(&AgentEvent::Done {
            session_id: "s".into(),
            content: "done".into(),
            id: None,
            tokens_used: None,
            tokens_in: None,
            tokens_in_hit: None,
            tokens_out: None,
            truncated: false,
        });
        assert!(!state.running);
        assert!(state.pending.is_none());
        assert!(state.current_step.is_none());
    }
}
