use async_trait::async_trait;
use crate::ai::agent::tool_registry::{Tool, ToolParameter};
use crate::core::skills::Skill;

/// A loaded inline skill exposed as a `skill_<name>` tool. Calling it injects
/// the skill's instructions (and optional args) into the conversation, plus
/// the skill directory's absolute path and a file listing so SKILL.md can
/// reference bundled scripts/templates by relative path (multi-file skills).
///
/// 声明了 pip 依赖的技能（办公三件套）还会附带「运行环境」块：隔离 conda
/// 环境就绪时给出解释器绝对路径；未就绪时引导模型调用 `skill_env_setup`。
pub struct SkillTool {
    name: String,
    description: String,
    content: String,
    dir: std::path::PathBuf,
    files: Vec<String>,
    env: Option<crate::core::skill_env::SkillEnv>,
    has_pip_requires: bool,
}

impl SkillTool {
    pub fn new(skill: Skill, env: Option<crate::core::skill_env::SkillEnv>) -> Self {
        let files = crate::core::skills::list_skill_files(&skill.dir);
        Self {
            name: format!("skill_{}", skill.name),
            description: skill.description,
            has_pip_requires: !skill.requires.pip.is_empty(),
            content: skill.content,
            dir: skill.dir,
            files,
            env,
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
        let listing = if self.files.is_empty() {
            String::new()
        } else {
            format!("\n\n目录内容：\n{}", self.files.join("\n"))
        };
        // 环境状态每轮重新读取（execute 是运行时）：用户可能刚在插件弹窗
        // 装完环境，下一次技能调用就应看到「已就绪」。
        let env_block = match (self.has_pip_requires, &self.env) {
            (true, Some(snapshot)) => {
                let fresh = crate::core::skill_env::resolve(&snapshot.app_data_dir);
                format!("\n\n{}", crate::core::skill_env::prompt_block(&fresh))
            }
            _ => String::new(),
        };
        Ok(format!(
            "## Skill: {}\n\n{}\n\n---\n技能目录：{}（上述内容中的相对路径均基于该目录，可用 bash / file_read 等工具访问其中的脚本与文件）{}{}{}",
            self.name,
            self.content,
            self.dir.to_string_lossy(),
            listing,
            extra,
            env_block,
        ))
    }
}
