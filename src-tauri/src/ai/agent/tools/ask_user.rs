use async_trait::async_trait;
use crate::ai::agent::tool_registry::{Tool, ToolParameter};

/// AskUserQuestion — handled inline by the engine (emits an `ask_user` event
/// and waits for the user's answers). This stub only exposes the tool to the
/// LLM; `execute` is never reached.
pub struct AskUserTool;

impl AskUserTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for AskUserTool {
    fn name(&self) -> &str {
        "ask_user"
    }

    fn description(&self) -> &str {
        "Ask the user a structured multiple-choice question. questions: array of { question, header?, options: [{ label, description? }], multi_select? }. Use when you need clarification or a choice before continuing."
    }

    fn readonly(&self) -> bool {
        true
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![ToolParameter {
            name: "questions".into(),
            param_type: "array".into(),
            description: "1-4 questions, each with question text and 2-4 options".into(),
            required: true,
        }]
    }

    async fn execute(&self, _args: serde_json::Value) -> Result<String, String> {
        Err("ask_user is handled by the engine".to_string())
    }
}
