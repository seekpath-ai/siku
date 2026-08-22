use async_trait::async_trait;
use crate::ai::agent::tool_registry::{Tool, ToolParameter};
use super::path::{resolve_path, working_dir_from_args};

pub struct FileEditTool;

impl FileEditTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for FileEditTool {
    fn name(&self) -> &str {
        "file_edit"
    }

    fn description(&self) -> &str {
        "Replace a unique substring in a text file within the working directory. Returns an error if old_string matches multiple times unless replace_all is true. Requires approval."
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![
            ToolParameter {
                name: "path".into(),
                param_type: "string".into(),
                description: "File path (absolute, or relative to the working directory)".into(),
                required: true,
            },
            ToolParameter {
                name: "old_string".into(),
                param_type: "string".into(),
                description: "Exact text to find (must be unique unless replace_all)".into(),
                required: true,
            },
            ToolParameter {
                name: "new_string".into(),
                param_type: "string".into(),
                description: "Replacement text".into(),
                required: true,
            },
            ToolParameter {
                name: "replace_all".into(),
                param_type: "boolean".into(),
                description: "Replace every occurrence instead of requiring a unique match".into(),
                required: false,
            },
        ]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let path = args["path"].as_str().ok_or("path required")?;
        let old_string = args["old_string"].as_str().ok_or("old_string required")?;
        let new_string = args["new_string"].as_str().ok_or("new_string required")?;
        let replace_all = args["replace_all"].as_bool().unwrap_or(false);
        let wd = working_dir_from_args(&args);

        if old_string == new_string {
            return Err("old_string and new_string must differ".to_string());
        }

        let resolved = resolve_path(wd.as_deref(), path)?;
        let content =
            std::fs::read_to_string(&resolved).map_err(|e| format!("read failed: {e}"))?;

        let count = content.matches(old_string).count();
        if count == 0 {
            return Err("old_string not found in file".to_string());
        }
        if !replace_all && count > 1 {
            return Err(format!(
                "old_string matches {count} times; set replace_all=true to replace every occurrence"
            ));
        }

        let updated = if replace_all {
            content.replace(old_string, new_string)
        } else {
            content.replacen(old_string, new_string, 1)
        };

        std::fs::write(&resolved, &updated).map_err(|e| format!("write failed: {e}"))?;

        Ok(format!(
            "Replaced {count} occurrence(s) in {}",
            resolved.display()
        ))
    }
}
