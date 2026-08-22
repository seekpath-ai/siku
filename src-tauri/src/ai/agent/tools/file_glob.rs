use async_trait::async_trait;
use crate::ai::agent::tool_registry::{Tool, ToolParameter};
use super::path::{resolve_path, working_dir_from_args};

const MAX_RESULTS: usize = 100;
const MAX_FILES: usize = 2000;
const MAX_DEPTH: usize = 16;

/// Minimal glob matcher supporting `*` (any chars except `/`), `**` (any
/// chars including `/`), and `?` (single char).
fn glob_match(pattern: &str, text: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let txt: Vec<char> = text.chars().collect();
    let (n, m) = (pat.len(), txt.len());
    // DP: dp[i][j] = pattern[0..i] matches text[0..j]
    let mut dp = vec![vec![false; m + 1]; n + 1];
    dp[0][0] = true;
    for i in 1..=n {
        if pat[i - 1] == '*' {
            dp[i][0] = dp[i - 1][0];
        }
    }
    for i in 1..=n {
        for j in 1..=m {
            match pat[i - 1] {
                '*' => {
                    // `**` also crosses `/`; `*` does not.
                    let cross_slash = i >= 2 && pat[i - 2] == '*';
                    dp[i][j] = dp[i - 1][j] || dp[i][j - 1]
                        && (cross_slash || txt[j - 1] != '/');
                }
                '?' => dp[i][j] = dp[i - 1][j - 1],
                c => dp[i][j] = dp[i - 1][j - 1] && c == txt[j - 1],
            }
        }
    }
    dp[n][m]
}

pub struct FileGlobTool;

impl FileGlobTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for FileGlobTool {
    fn name(&self) -> &str {
        "file_glob"
    }

    fn description(&self) -> &str {
        "List files within the working directory matching a glob pattern (e.g. **/*.rs, src/**/*.ts). Returns up to 100 entries sorted by modification time, newest first. Read-only, auto-approved."
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![
            ToolParameter {
                name: "pattern".into(),
                param_type: "string".into(),
                description: "Glob pattern, relative to the working directory".into(),
                required: true,
            },
            ToolParameter {
                name: "path".into(),
                param_type: "string".into(),
                description: "Optional base subdirectory to search from (default working directory)".into(),
                required: false,
            },
        ]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let pattern = args["pattern"].as_str().ok_or("pattern required")?;
        let base_arg = args["path"].as_str().unwrap_or(".");
        let wd = working_dir_from_args(&args);

        let base = resolve_path(wd.as_deref(), base_arg)?;

        let mut matches: Vec<(std::path::PathBuf, std::time::SystemTime)> = Vec::new();
        walk(&base, pattern, &base, &mut matches, 0)?;

        // Sort by modification time, newest first.
        matches.sort_by(|a, b| b.1.cmp(&a.1));

        if matches.is_empty() {
            return Ok(format!("No files match '{pattern}'."));
        }

        let shown = matches.iter().take(MAX_RESULTS);
        let lines: Vec<String> = shown
            .map(|(p, _)| {
                let rel = p.strip_prefix(&base).unwrap_or(p);
                rel.display().to_string()
            })
            .collect();
        let mut out = lines.join("\n");
        if matches.len() > MAX_RESULTS {
            out.push_str(&format!("\n... {} more", matches.len() - MAX_RESULTS));
        }
        Ok(out)
    }
}

fn walk(
    dir: &std::path::Path,
    pattern: &str,
    base: &std::path::Path,
    out: &mut Vec<(std::path::PathBuf, std::time::SystemTime)>,
    depth: usize,
) -> Result<(), String> {
    if depth > MAX_DEPTH || out.len() >= MAX_FILES {
        return Ok(());
    }
    let entries = std::fs::read_dir(dir).map_err(|e| format!("read dir: {e}"))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            walk(&path, pattern, base, out, depth + 1)?;
            continue;
        }
        let rel = path.strip_prefix(base).unwrap_or(&path).to_string_lossy().to_string();
        if glob_match(pattern, &rel) {
            let mtime = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            out.push((path, mtime));
        }
    }
    Ok(())
}
