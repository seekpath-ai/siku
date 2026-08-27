use async_trait::async_trait;
use crate::ai::agent::tool_registry::{Tool, ToolParameter};
use super::path::{resolve_path, working_dir_from_args};

/// Maximum bytes scanned per file and total results.
const MAX_FILE_BYTES: u64 = 512 * 1024;
const MAX_RESULTS: usize = 100;
const MAX_FILES: usize = 500;

pub struct FileGrepTool;

impl FileGrepTool {
    pub fn new() -> Self {
        Self
    }

    fn matches(haystack: &str, needle: &str) -> bool {
        haystack.contains(needle)
    }
}

#[async_trait]
impl Tool for FileGrepTool {
    fn name(&self) -> &str {
        "file_grep"
    }

    fn description(&self) -> &str {
        "Search text files within the working directory for a substring, returning matching lines with file and line numbers. Matching is a case-sensitive substring search (not regex). Read-only, auto-approved."
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![
            ToolParameter {
                name: "pattern".into(),
                param_type: "string".into(),
                description: "Substring to search for".into(),
                required: true,
            },
            ToolParameter {
                name: "path".into(),
                param_type: "string".into(),
                description: "Optional file or directory to scope the search (relative to the working directory)".into(),
                required: false,
            },
            ToolParameter {
                name: "max_results".into(),
                param_type: "integer".into(),
                description: "Maximum matching lines to return (default 50, max 100)".into(),
                required: false,
            },
        ]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let pattern = args["pattern"].as_str().ok_or("pattern required")?;
        let scope = args["path"].as_str();
        let max_results = args["max_results"].as_u64().unwrap_or(50).min(MAX_RESULTS as u64) as usize;
        let wd = working_dir_from_args(&args);

        let base = resolve_path(wd.as_deref(), scope.unwrap_or("."))?;

        let mut files = Vec::new();
        collect_files(&base, &mut files, 0)?;
        if files.is_empty() {
            return Ok("No files found.".to_string());
        }
        // collect_files caps at MAX_FILES, so a full vec means more files may
        // exist on disk that were never scanned.
        let files_truncated = files.len() >= MAX_FILES;

        let mut results: Vec<String> = Vec::new();
        for file in files {
            if results.len() >= max_results {
                break;
            }
            let Ok(meta) = std::fs::metadata(&file) else { continue };
            if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&file) else { continue };
            let rel = file.strip_prefix(&base).unwrap_or(&file);
            for (idx, line) in content.lines().enumerate() {
                if results.len() >= max_results {
                    break;
                }
                if Self::matches(line, pattern) {
                    results.push(format!("{}:{}:{}", rel.display(), idx + 1, line));
                }
            }
        }

        if results.is_empty() {
            return Ok(format!("No matches for '{pattern}'."));
        }
        let mut out = results.join("\n");
        // State both truncation causes explicitly so the model narrows the
        // pattern/path instead of assuming the listing is complete.
        if results.len() >= max_results {
            out.push_str(&format!(
                "\n…(已截断:结果已达上限 {max_results} 条,请缩小 pattern 或指定更精确的 path)"
            ));
        }
        if files_truncated {
            out.push_str(&format!(
                "\n…(已截断:仅扫描了前 {MAX_FILES} 个文件,请缩小 pattern 或指定更精确的 path)"
            ));
        }
        Ok(out)
    }
}

fn collect_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>, depth: usize) -> Result<(), String> {
    if depth > 12 {
        return Ok(());
    }
    let entries = std::fs::read_dir(dir).map_err(|e| format!("read dir: {e}"))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            collect_files(&path, out, depth + 1)?;
        } else if out.len() < MAX_FILES {
            out.push(path);
        }
    }
    Ok(())
}
