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
        "Replace a unique substring in a text file. Relative paths resolve against the session's write directory; absolute paths may point anywhere. Returns an error if old_string matches multiple times unless replace_all is true. Requires approval."
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![
            ToolParameter {
                name: "path".into(),
                param_type: "string".into(),
                description: "File path (absolute anywhere, or relative to the session's write directory)".into(),
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

        // Same atomic replace as file_write: a truncated-then-rewritten file
        // is exactly the failure this avoids.
        super::write_atomically(&resolved, &updated)?;
        // The file now holds content the agent authored, so a later
        // whole-file overwrite may proceed without a fresh read.
        super::known_files::remember(&resolved);

        Ok(format!(
            "Replaced {count} occurrence(s) in {}",
            resolved.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(dir: &std::path::Path, old: &str, new: &str) -> serde_json::Value {
        serde_json::json!({
            "path": "f.txt",
            "old_string": old,
            "new_string": new,
            "_working_dir": dir.to_str().unwrap(),
        })
    }

    #[tokio::test]
    async fn replaces_and_keeps_the_file_mode() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("f.txt");
        std::fs::write(&target, "a = 1;\n").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let out = FileEditTool::new()
            .execute(args(dir.path(), "a = 1;", "a = 2;"))
            .await
            .expect("edit must succeed");
        assert!(out.contains("Replaced 1 occurrence(s)"), "{out}");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "a = 2;\n");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
                0o755
            );
        }
        // No temp file may survive the replace.
        let leftovers: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains("siku-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    /// An edit makes the file's content known, so a following whole-file
    /// overwrite no longer needs a read.
    #[tokio::test]
    async fn edited_file_counts_as_known_content() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("f.txt");
        std::fs::write(&target, "a = 1;\n").unwrap();

        FileEditTool::new()
            .execute(args(dir.path(), "a = 1;", "a = 2;"))
            .await
            .unwrap();

        crate::ai::agent::tools::file_write::FileWriteTool::new()
            .execute(serde_json::json!({
                "path": "f.txt",
                "content": "rewritten\n",
                "mode": "overwrite",
                "_working_dir": dir.path().to_str().unwrap(),
            }))
            .await
            .expect("the edit made the content known");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "rewritten\n");
    }
}
