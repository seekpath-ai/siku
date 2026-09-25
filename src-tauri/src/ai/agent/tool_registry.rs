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

    /// Allowed values for this tool's string parameters, merged into the
    /// generated JSON Schema as `enum`. Keyed by parameter name; parameters
    /// without an entry stay unconstrained. Opt-in (empty by default), so a
    /// tool that enumerates its values stops the model from guessing them
    /// from prose (e.g. `mode` accepting "Append" or `{"value":"append"}`).
    fn param_enums(&self) -> &'static [(&'static str, &'static [&'static str])] {
        &[]
    }

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

/// Merge the `enum` constraints declared by `Tool::param_enums` into a schema
/// built by `parameters_to_schema`. Names that are not in the schema are
/// ignored, so a stale declaration can never produce a broken schema.
pub fn apply_param_enums(
    mut schema: serde_json::Value,
    enums: &[(&str, &[&str])],
) -> serde_json::Value {
    for (name, values) in enums {
        if let Some(prop) = schema
            .get_mut("properties")
            .and_then(|p| p.get_mut(*name))
            .and_then(|p| p.as_object_mut())
        {
            prop.insert("enum".into(), serde_json::json!(values));
        }
    }
    schema
}

/// Registry of all available tools
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
    /// Default *write directory* injected into every tool call as
    /// `_working_dir`. This is NOT a sandbox: absolute paths go anywhere, and
    /// paths outside this directory are allowed — they are merely flagged in
    /// the approval card (see `writes_outside_write_dir`). `None` means no
    /// default, so relative paths are refused instead of resolving against the
    /// process cwd.
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

    /// Does this call target a path outside the session's write directory?
    ///
    /// Purely informational: nothing is blocked. The approval card uses it to
    /// highlight a write that leaves the usual place, which is the only
    /// boundary an app whose shell tool is unconfined can honestly offer — the
    /// human's attention at the moment of approval.
    ///
    /// Only arguments that are unambiguously a path are considered (`path` for
    /// the file tools, `cwd` for bash); a shell command string is not parsed,
    /// so `bash` without an explicit cwd is never flagged.
    pub fn writes_outside_write_dir(&self, tool_name: &str, args: &serde_json::Value) -> bool {
        let Some(base) = self.working_dir.as_deref().filter(|b| !b.trim().is_empty()) else {
            return false; // no default directory to be outside of
        };
        let arg = match tool_name {
            "file_read" | "file_write" | "file_edit" | "file_grep" | "file_glob" | "file_list"
            | "read_media_file" => "path",
            "bash" => "cwd",
            _ => return false,
        };
        let Some(path) = args.get(arg).and_then(|v| v.as_str()) else {
            return false;
        };
        if path.is_empty() {
            return false;
        }

        let Ok(base_path) = crate::ai::agent::tools::path::resolve_path(Some(base), ".") else {
            return false;
        };
        let Ok(target) = crate::ai::agent::tools::path::resolve_path(Some(base), path) else {
            return false;
        };
        // Component-wise comparison, so `…/sandbox2` is not "inside" `…/sandbox`.
        !target.starts_with(&base_path)
    }

    /// Get tool definitions for the LLM API
    pub fn get_definitions(&self) -> Vec<crate::ai::llm::ToolDefinition> {
        self.tools
            .iter()
            .map(|(name, tool)| {
                let schema =
                    apply_param_enums(parameters_to_schema(&tool.parameters()), tool.param_enums());
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
        // `_working_dir` is registry-owned, so a model-supplied value is
        // dropped before the session's own write directory is injected — a
        // hallucinated (or adversarial) value must never decide where relative
        // paths land.
        //
        // Injection happens only for object arguments: `args["_working_dir"] =
        // …` panics on a non-object (`Value::index_mut` aborts on an array or
        // string), and release builds are `panic = "abort"`, so a malformed
        // tool call could have taken the whole app down. Now the tool reports
        // its own "… required" error instead.
        if let Some(obj) = args.as_object_mut() {
            obj.remove("_working_dir");
            if let Some(wd) = &self.working_dir {
                obj.insert("_working_dir".into(), serde_json::json!(wd));
            }
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

    /// Register the session's mounted skills as `skill_<name>` tools. Only
    /// names in `selected` register — unmounted skills stay invisible to the
    /// model, so their descriptions cost no context. Call AFTER `retain` so
    /// mounted skills are always available to the agent.
    pub fn register_skills(&mut self, dir: &std::path::Path, selected: &std::collections::HashSet<String>) {
        if selected.is_empty() {
            return;
        }
        for skill in crate::core::skills::scan(dir) {
            if selected.contains(&skill.name) {
                self.register(crate::ai::agent::tools::skill::SkillTool::new(skill));
            }
        }
    }

    /// List available tool names.
    pub fn tool_names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    /// Create a registry with all built-in tools.
    ///
    /// `working_dir` is the default write directory for relative paths — a
    /// project folder or the session workspace, never a sandbox (the caller
    /// always supplies one);
    /// `tasks` is the app-wide background task store (for bash);
    /// `session_id` 归属会话 id，写入 bash 后台任务的 TaskInfo，方便任务中心按会话过滤；
    /// `app_handle` 用于工具向前端广播变更事件（目前仅 note_write 的 `note:changed`）；
    /// `vision_llm` is the agent's multimodal model config (for read_media_file);
    /// `web_proxy` is the per-agent proxy for web tools (None = global).
    pub fn default_registry(
        db: &sqlx::SqlitePool,
        app_data_dir: &std::path::Path,
        working_dir: Option<String>,
        tasks: crate::core::tasks::TaskStore,
        session_id: Option<String>,
        app_handle: Option<tauri::AppHandle>,
        vision_llm: Option<crate::ai::llm::LlmConfig>,
        web_proxy: Option<String>,
    ) -> Self {
        let mut registry = Self::new();
        registry.working_dir = working_dir;

        // Paper tools
        registry.register(crate::ai::agent::tools::paper_search::PaperSearchTool::new(db.clone()));
        registry.register(crate::ai::agent::tools::paper_read::PaperReadTool::new(db.clone()));
        // PDF region screenshots for figures/tables; `vision_llm` powers the
        // optional one-shot analysis (same config as read_media_file).
        registry.register(crate::ai::agent::tools::paper_snapshot::PaperSnapshotTool::new(
            db.clone(),
            app_data_dir.to_path_buf(),
            vision_llm.clone(),
        ));
        // Retrieval half of the RAG pipeline (keyword + optional semantic).
        registry.register(crate::ai::agent::tools::library_search::LibrarySearchTool::new(db.clone()));
        registry.register(crate::ai::agent::tools::paper_import::PaperImportTool::new(db.clone(), app_data_dir.to_path_buf()));

        // Note tools
        registry.register(crate::ai::agent::tools::note_read::NoteReadTool::new(db.clone()));
        registry.register(crate::ai::agent::tools::note_write::NoteWriteTool::new(db.clone(), app_handle));

        // Web tools
        registry.register(crate::ai::agent::tools::web_fetch::WebFetchTool::new(db.clone(), web_proxy.clone()));
        registry.register(crate::ai::agent::tools::web_search::WebSearchTool::new(db.clone(), web_proxy));

        // Translation
        registry.register(crate::ai::agent::tools::translation::TranslationTool::new(db.clone()));

        // Knowledge
        registry.register(crate::ai::agent::tools::knowledge::KnowledgeQueryTool::new(db.clone()));
        registry.register(crate::ai::agent::tools::knowledge_write::KnowledgeWriteTool::new(db.clone()));

        // Long-term memory (per-session; needs the session id, so only for
        // real chat sessions — not for session-less helper registries).
        if let Some(sid) = session_id.clone() {
            registry.register(crate::ai::agent::tools::memory::MemoryReadTool::new(db.clone(), sid.clone()));
            registry.register(crate::ai::agent::tools::memory::MemoryWriteTool::new(db.clone(), sid));
        }

        // File tools
        registry.register(crate::ai::agent::tools::file_ops::FileReadTool::new());
        registry.register(crate::ai::agent::tools::file_ops::FileListTool::new());
        registry.register(crate::ai::agent::tools::file_write::FileWriteTool::new());
        registry.register(crate::ai::agent::tools::file_edit::FileEditTool::new());
        registry.register(crate::ai::agent::tools::file_grep::FileGrepTool::new());
        registry.register(crate::ai::agent::tools::file_glob::FileGlobTool::new());

        // Shell + background tasks
        registry.register(crate::ai::agent::tools::bash::BashTool::new(tasks.clone(), app_data_dir.to_path_buf(), session_id));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::agent::tools::file_write::FileWriteTool;

    /// `Tool::param_enums` must reach the schema the model is actually sent —
    /// that is the only place the accepted `mode` strings are advertised.
    #[test]
    fn param_enums_reach_the_tool_schema() {
        let mut registry = ToolRegistry::new();
        registry.register(FileWriteTool::new());

        let defs = registry.get_definitions();
        let fw = defs
            .iter()
            .find(|d| d.function.name == "file_write")
            .expect("file_write must be registered");
        let schema = &fw.function.parameters;

        assert_eq!(
            schema["properties"]["mode"]["enum"],
            serde_json::json!(["overwrite", "append"])
        );
        // An enum must not imply required: the omitted case is the safe one.
        let required = schema["required"].as_array().unwrap();
        assert!(!required.iter().any(|v| v == "mode"), "{required:?}");
        assert!(required.iter().any(|v| v == "content"), "{required:?}");
    }

    /// Tools that declare nothing keep their old schema (opt-in behaviour).
    #[test]
    fn tools_without_enums_are_unchanged() {
        let schema = parameters_to_schema(&[ToolParameter {
            name: "path".into(),
            param_type: "string".into(),
            description: "p".into(),
            required: true,
        }]);
        assert_eq!(schema["properties"]["path"]["enum"], serde_json::Value::Null);
    }

    /// The registry owns `_working_dir`. A model-supplied value must never be
    /// honoured — with no write directory configured it would otherwise be
    /// adopted as the base for relative paths.
    #[tokio::test]
    async fn model_supplied_working_dir_is_discarded() {
        let mut registry = ToolRegistry::new();
        registry.register(FileWriteTool::new());

        let err = registry
            .execute(
                "file_write",
                serde_json::json!({
                    "path": "x.txt",
                    "content": "hi",
                    "_working_dir": std::env::temp_dir().to_str().unwrap(),
                }),
            )
            .await
            .unwrap_err();
        assert!(err.contains("no write directory"), "{err}");
    }

    /// …and with a write directory configured it is replaced by the session's
    /// own, so relative paths land where the session says and not where the
    /// model asked.
    #[tokio::test]
    async fn session_working_dir_overrides_the_supplied_one() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = ToolRegistry::new();
        registry.register(FileWriteTool::new());
        registry.working_dir = Some(dir.path().to_str().unwrap().to_string());

        registry
            .execute(
                "file_write",
                serde_json::json!({
                    "path": "x.txt",
                    "content": "hi",
                    "_working_dir": dir.path().join("no-such-dir").to_str().unwrap(),
                }),
            )
            .await
            .expect("the session root must win");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("x.txt")).unwrap(),
            "hi"
        );
    }

    /// Non-object arguments used to panic inside `execute`; they must surface
    /// as an ordinary tool error.
    #[tokio::test]
    async fn non_object_arguments_do_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = ToolRegistry::new();
        registry.register(FileWriteTool::new());
        registry.working_dir = Some(dir.path().to_str().unwrap().to_string());

        for args in [serde_json::json!(["x"]), serde_json::json!("x"), serde_json::json!(7)] {
            let err = registry.execute("file_write", args).await.unwrap_err();
            assert!(err.contains("path required"), "{err}");
        }
    }

    /// The out-of-directory flag is advisory: it must fire for a write leaving
    /// the default directory and stay quiet for one inside it.
    #[test]
    fn out_of_write_dir_flag_is_advisory_only() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("write-dir");
        std::fs::create_dir_all(base.join("sub")).unwrap();
        // A sibling whose name shares the base's prefix must not count as inside.
        let sibling = dir.path().join("write-dir-2");
        std::fs::create_dir_all(&sibling).unwrap();

        let mut registry = ToolRegistry::new();
        registry.working_dir = Some(base.to_str().unwrap().to_string());

        let w = |args: serde_json::Value| registry.writes_outside_write_dir("file_write", &args);

        assert!(!w(serde_json::json!({ "path": "sub/a.txt" })));
        assert!(!w(serde_json::json!({ "path": "a.txt" })));
        assert!(!w(serde_json::json!({
            "path": base.join("sub/a.txt").to_str().unwrap()
        })));
        assert!(w(serde_json::json!({ "path": "../elsewhere.txt" })));
        assert!(w(serde_json::json!({
            "path": sibling.join("a.txt").to_str().unwrap()
        })));
        assert!(w(serde_json::json!({
            "path": std::env::temp_dir().to_str().unwrap()
        })));
        // No path argument at all (or a different tool) is never flagged.
        assert!(!w(serde_json::json!({})));
        assert!(!registry.writes_outside_write_dir("file_read", &serde_json::json!({})));
        assert!(!registry.writes_outside_write_dir("web_search", &serde_json::json!({
            "query": "x"
        })));

        // bash is judged by its explicit cwd only — the command string is not
        // parsed, so no cwd means no flag.
        assert!(!registry.writes_outside_write_dir("bash", &serde_json::json!({
            "command": "rm -rf /"
        })));
        assert!(registry.writes_outside_write_dir("bash", &serde_json::json!({
            "command": "ls",
            "cwd": std::env::temp_dir().to_str().unwrap()
        })));
    }

    /// Without a default write directory there is nothing to be outside of.
    #[test]
    fn no_write_dir_never_flags() {
        let registry = ToolRegistry::new();
        assert!(!registry.writes_outside_write_dir("file_write", &serde_json::json!({
            "path": "/anywhere/x.txt"
        })));
    }
}
