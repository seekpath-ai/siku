use std::collections::HashMap;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A parameter definition for a tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolParameter {
    pub name: String,
    pub param_type: String,
    pub description: String,
    pub required: bool,
}

/// A tool that can be used by the agent
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    /// Unique name of the tool (used in function calling)
    fn name(&self) -> &str;

    /// Human-readable description for the LLM
    fn description(&self) -> &str;

    /// JSON Schema for the tool's parameters
    fn parameters(&self) -> Vec<ToolParameter>;

    /// Whether this tool is read-only. Read-only tools are auto-approved;
    /// everything else follows the session's approval policy.
    fn readonly(&self) -> bool {
        false
    }

    /// Execute the tool with the given arguments (as JSON Value).
    /// The registry injects `_working_dir` into args before dispatch.
    async fn execute(&self, args: serde_json::Value) -> Result<String, String>;
}

/// Build a JSON Schema from tool parameters
pub fn parameters_to_schema(params: &[ToolParameter]) -> serde_json::Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();

    for p in params {
        properties.insert(
            p.name.clone(),
            serde_json::json!({
                "type": p.param_type,
                "description": p.description,
            }),
        );
        if p.required {
            required.push(p.name.clone());
        }
    }

    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
    })
}

/// Registry of all available tools
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
    /// Sandbox root injected into every tool call; None = full disk access.
    working_dir: Option<String>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            working_dir: None,
        }
    }

    /// Register a tool
    pub fn register<T: Tool + 'static>(&mut self, tool: T) {
        self.tools.insert(tool.name().to_string(), Box::new(tool));
    }

    /// Get tool definitions for the LLM API
    pub fn get_definitions(&self) -> Vec<crate::ai::llm::ToolDefinition> {
        self.tools
            .iter()
            .map(|(name, tool)| {
                let schema = parameters_to_schema(&tool.parameters());
                crate::ai::llm::make_tool_definition(name, tool.description(), schema)
            })
            .collect()
    }

    /// Execute a tool by name. Injects the registry's working dir into args.
    pub async fn execute(&self, name: &str, args: serde_json::Value) -> Result<String, String> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| format!("unknown tool: {name}"))?;
        let mut args = args;
        if let Some(wd) = &self.working_dir {
            args["_working_dir"] = serde_json::json!(wd);
        }
        tool.execute(args).await
    }

    /// Whether a tool is read-only (auto-approved).
    pub fn is_readonly(&self, name: &str) -> bool {
        self.tools
            .get(name)
            .map(|t| t.readonly())
            .unwrap_or(false)
    }

    /// Keep only tools whose names are in `allowed`. `None` (the column was
    /// NULL or unparsable) keeps every tool — the backward-compatible default.
    /// `Some([])` keeps NOTHING: an explicitly empty list means "no tools".
    /// Unknown names are logged, not silently dropped — a stale name here used
    /// to silently strip capabilities from the built-in domain agents.
    pub fn retain(&mut self, allowed: Option<&[String]>) {
        let allowed = match allowed {
            None => return,
            Some(a) => a,
        };
        let allowed_set: std::collections::HashSet<&str> =
            allowed.iter().map(|s| s.as_str()).collect();
        for name in &allowed_set {
            if !self.tools.contains_key(*name) {
                tracing::warn!(tool = %name, "retain: unknown tool name ignored");
            }
        }
        self.tools.retain(|name, _| allowed_set.contains(name.as_str()));
    }

    /// Register inline skills from a skills directory as `skill_<name>` tools.
    /// Call AFTER `retain` so skills are always available to the agent.
    pub fn register_skills(&mut self, dir: &std::path::Path) {
        for skill in crate::core::skills::scan(dir) {
            self.register(crate::ai::agent::tools::skill::SkillTool::new(skill));
        }
    }

    /// List available tool names.
    pub fn tool_names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    /// Create a registry with all built-in tools.
    ///
    /// `working_dir` is the sandbox root for file tools (None = full access);
    /// `tasks` is the app-wide background task store (for bash);
    /// `vision_llm` is the agent's multimodal model config (for read_media_file);
    /// `web_proxy` is the per-agent proxy for web tools (None = global).
    pub fn default_registry(
        db: &sqlx::SqlitePool,
        app_data_dir: &std::path::Path,
        working_dir: Option<String>,
        tasks: crate::core::tasks::TaskStore,
        vision_llm: Option<crate::ai::llm::LlmConfig>,
        web_proxy: Option<String>,
    ) -> Self {
        let mut registry = Self::new();
        registry.working_dir = working_dir;

        // Paper tools
        registry.register(crate::ai::agent::tools::paper_search::PaperSearchTool::new(db.clone()));
        registry.register(crate::ai::agent::tools::paper_read::PaperReadTool::new(db.clone()));
        registry.register(crate::ai::agent::tools::paper_import::PaperImportTool::new(db.clone(), app_data_dir.to_path_buf()));

        // Note tools
        registry.register(crate::ai::agent::tools::note_read::NoteReadTool::new(db.clone()));
        registry.register(crate::ai::agent::tools::note_write::NoteWriteTool::new(db.clone()));

        // Web tools
        registry.register(crate::ai::agent::tools::web_fetch::WebFetchTool::new(db.clone(), web_proxy.clone()));
        registry.register(crate::ai::agent::tools::web_search::WebSearchTool::new(db.clone(), web_proxy));

        // Translation
        registry.register(crate::ai::agent::tools::translation::TranslationTool::new(db.clone()));

        // Knowledge
        registry.register(crate::ai::agent::tools::knowledge::KnowledgeQueryTool::new(db.clone()));
        registry.register(crate::ai::agent::tools::knowledge_write::KnowledgeWriteTool::new(db.clone()));

        // File tools
        registry.register(crate::ai::agent::tools::file_ops::FileReadTool::new());
        registry.register(crate::ai::agent::tools::file_ops::FileListTool::new());
        registry.register(crate::ai::agent::tools::file_write::FileWriteTool::new());
        registry.register(crate::ai::agent::tools::file_edit::FileEditTool::new());
        registry.register(crate::ai::agent::tools::file_grep::FileGrepTool::new());
        registry.register(crate::ai::agent::tools::file_glob::FileGlobTool::new());

        // Shell + background tasks
        registry.register(crate::ai::agent::tools::bash::BashTool::new(tasks.clone(), app_data_dir.to_path_buf()));
        registry.register(crate::ai::agent::tools::tasks::TaskListTool::new(tasks.clone()));
        registry.register(crate::ai::agent::tools::tasks::TaskOutputTool::new(tasks.clone()));
        registry.register(crate::ai::agent::tools::tasks::TaskStopTool::new(tasks.clone()));

        // Ask the user for clarification (handled inline by the engine)
        registry.register(crate::ai::agent::tools::ask_user::AskUserTool::new());

        // Vision (multimodal) — uses the agent's vision model
        registry.register(crate::ai::agent::tools::read_media_file::ReadMediaFileTool::new(vision_llm));

        registry
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}
