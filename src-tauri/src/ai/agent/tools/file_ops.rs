use async_trait::async_trait;
use crate::ai::agent::tool_registry::{Tool, ToolParameter};
use super::path::{resolve_path, working_dir_from_args};

const MAX_LINES: usize = 1000;

pub struct FileReadTool;

impl FileReadTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str {
        "file_read"
    }

    fn description(&self) -> &str {
        "Read lines from a text file within the working directory. Supports line_offset (1-based, negative counts from the end) and n_lines. Read-only, auto-approved."
    }

    fn readonly(&self) -> bool {
        true
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
                name: "line_offset".into(),
                param_type: "integer".into(),
                description: "1-based start line; negative counts from the end (default 1)".into(),
                required: false,
            },
            ToolParameter {
                name: "n_lines".into(),
                param_type: "integer".into(),
                description: "Maximum number of lines to return (default 200, max 1000)".into(),
                required: false,
            },
        ]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let path = args["path"].as_str().ok_or("path required")?;
        let wd = working_dir_from_args(&args);
        let resolved = resolve_path(wd.as_deref(), path)?;

        if !resolved.exists() {
            return Ok(format!("File not found: {path}"));
        }
        if !resolved.is_file() {
            return Ok(format!("Not a file: {path}"));
        }

        let content = std::fs::read_to_string(&resolved).map_err(|e| format!("read failed: {e}"))?;
        let lines: Vec<&str> = content.lines().collect();
        let total = lines.len();

        let mut offset = args["line_offset"].as_i64().unwrap_or(1);
        let n_lines = args["n_lines"].as_u64().unwrap_or(200).min(MAX_LINES as u64) as usize;

        if offset < 0 {
            offset = total as i64 + offset + 1;
        }
        if offset < 1 {
            offset = 1;
        }
        let start = (offset - 1) as usize;
        let end = (start + n_lines).min(total);
        if start >= total {
            return Ok(format!("File has {total} lines; offset {offset} is past the end."));
        }

        let mut out = String::new();
        for (idx, line) in lines[start..end].iter().enumerate() {
            out.push_str(&format!("{}: {}\n", start + idx + 1, line));
        }
        let shown = end - start;
        let mut note = format!("Lines {}-{} of {total}", start + 1, end);
        if shown >= MAX_LINES && end < total {
            note.push_str(" (truncated — use line_offset to page further)");
        }
        Ok(format!("{note}\n{out}"))
    }
}

pub struct FileListTool;

impl FileListTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for FileListTool {
    fn name(&self) -> &str {
        "file_list"
    }

    fn description(&self) -> &str {
        "List files and directories in a directory within the working directory. Read-only, auto-approved."
    }

    fn readonly(&self) -> bool {
        true
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![ToolParameter {
            name: "path".into(),
            param_type: "string".into(),
            description: "Directory path (absolute, or relative to the working directory; default working directory)".into(),
            required: false,
        }]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let path = args["path"].as_str().unwrap_or(".");
        let wd = working_dir_from_args(&args);
        let resolved = resolve_path(wd.as_deref(), path)?;

        if !resolved.is_dir() {
            return Ok(format!("Not a directory: {path}"));
        }

        let entries: Vec<String> = match std::fs::read_dir(&resolved) {
            Ok(iter) => {
                let mut items: Vec<_> = iter
                    .filter_map(|e| e.ok())
                    .map(|e| {
                        let name = e.file_name().to_string_lossy().to_string();
                        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                        let suffix = if is_dir { "/" } else { "" };
                        (name, suffix)
                    })
                    .collect();
                items.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
                items.iter().map(|(n, s)| format!("- {}{}", n, s)).collect()
            }
            Err(e) => return Err(format!("Cannot read directory: {e}")),
        };

        if entries.is_empty() {
            Ok("Directory is empty.".to_string())
        } else {
            Ok(format!("Contents of {}:\n{}", resolved.display(), entries.join("\n")))
        }
    }
}
