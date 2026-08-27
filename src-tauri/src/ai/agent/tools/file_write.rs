use async_trait::async_trait;
use crate::ai::agent::tool_registry::{Tool, ToolParameter};
use super::path::{resolve_path, working_dir_from_args};

pub struct FileWriteTool;

impl FileWriteTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &str {
        "file_write"
    }

    fn description(&self) -> &str {
        "Create, overwrite, or append a text file within the working directory. mode: overwrite (default) or append. Creates parent directories automatically. To modify an existing file, prefer file_edit (targeted replacement) over overwriting the whole file. Requires approval."
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
                name: "content".into(),
                param_type: "string".into(),
                description: "Text content to write".into(),
                required: true,
            },
            ToolParameter {
                name: "mode".into(),
                param_type: "string".into(),
                description: "overwrite or append (default overwrite)".into(),
                required: false,
            },
        ]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let path = args["path"].as_str().ok_or("path required")?;
        // content is required: silently defaulting to "" would TRUNCATE an
        // existing file when the model omits the argument.
        let content = args["content"]
            .as_str()
            .ok_or("content required (refusing to write an empty file by default)")?;
        let wd = working_dir_from_args(&args);

        let resolved = resolve_path(wd.as_deref(), path)?;
        if let Some(parent) = resolved.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("create dir: {e}"))?;
        }

        // Reject unknown modes explicitly — a typo like "Append" must not
        // fall through to a silent overwrite.
        match args["mode"].as_str().unwrap_or("overwrite") {
            "append" => {
                use std::io::Write;
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&resolved)
                    .map_err(|e| format!("open failed: {e}"))?;
                f.write_all(content.as_bytes())
                    .map_err(|e| format!("write failed: {e}"))?;
            }
            "overwrite" => {
                std::fs::write(&resolved, content).map_err(|e| format!("write failed: {e}"))?
            }
            other => return Err(format!("unknown mode '{other}' (expected overwrite or append)")),
        }

        Ok(format!(
            "Wrote {} chars to {}",
            content.chars().count(),
            resolved.display()
        ))
    }
}
