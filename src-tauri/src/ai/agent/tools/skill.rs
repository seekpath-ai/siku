use async_trait::async_trait;
use crate::ai::agent::tool_registry::{Tool, ToolParameter};
use crate::core::skills::Skill;

/// A loaded inline skill exposed as a `skill_<name>` tool. Calling it injects
/// the skill's instructions (and optional args) into the conversation.
pub struct SkillTool {
    name: String,
    description: String,
    content: String,
}

impl SkillTool {
    pub fn new(skill: Skill) -> Self {
        Self {
            name: format!("skill_{}", skill.name),
            description: skill.description,
            content: skill.content,
        }
    }
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn readonly(&self) -> bool {
        true
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![ToolParameter {
            name: "args".into(),
            param_type: "string".into(),
            description: "Optional arguments or context to pass to the skill".into(),
            required: false,
        }]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let extra = args["args"]
            .as_str()
            .map(|s| format!("\n\n--- Arguments ---\n{s}"))
            .unwrap_or_default();
        Ok(format!(
            "## Skill: {}\n\n{}\n{}",
            self.name, self.content, extra
        ))
    }
}
