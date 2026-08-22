use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use async_trait::async_trait;
use std::process::Stdio;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;
use crate::ai::agent::tool_registry::{Tool, ToolParameter};
use crate::core::tasks::{TaskHandle, TaskInfo, TaskStore};
use super::path::{resolve_path, working_dir_from_args};

const DEFAULT_TIMEOUT_MS: u64 = 60_000;
const MAX_TIMEOUT_MS: u64 = 300_000;
const OUTPUT_LIMIT_CHARS: usize = 20_000;

/// Shell invocation used by the bash tool.
#[derive(Clone)]
struct Shell {
    program: std::path::PathBuf,
    args: Vec<&'static str>,
}

impl Shell {
    fn new(program: impl Into<std::path::PathBuf>, args: Vec<&'static str>) -> Self {
        Self {
            program: program.into(),
            args,
        }
    }
}

/// Resolve which shell to use. On Windows we default to PowerShell because it
/// ships with the OS and exposes a modern command surface. On Unix-like systems
/// we default to `sh`. The caller can override with `shell=bash|powershell|cmd|sh`.
fn resolve_shell(requested: Option<&str>) -> Result<Shell, String> {
    let name = requested.unwrap_or_default().to_lowercase();

    // Explicit shell requests.
    match name.as_str() {
        "powershell" | "ps" => {
            return Ok(Shell::new(
                "powershell.exe",
                vec!["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command"],
            ));
        }
        "cmd" => {
            return Ok(Shell::new("cmd.exe", vec!["/c"]));
        }
        "bash" => {
            return Ok(find_bash()
                .map(|p| Shell::new(p, vec!["-lc"]))
                .ok_or_else(|| {
                    "bash not found: install Git for Windows and add bash.exe to PATH, or use powershell/cmd".to_string()
                })?);
        }
        "sh" => {
            return Ok(Shell::new("sh", vec!["-lc"]));
        }
        "" => {} // fall through to platform defaults
        _ => return Err(format!("unsupported shell: {name}")),
    }

    // Platform defaults.
    if cfg!(windows) {
        Ok(Shell::new(
            "powershell.exe",
            vec!["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command"],
        ))
    } else {
        Ok(Shell::new("sh", vec!["-lc"]))
    }
}

/// Find bash.exe on Windows (PATH first, then common install locations).
#[cfg(windows)]
fn find_bash() -> Option<std::path::PathBuf> {
    if let Ok(path_env) = std::env::var("PATH") {
        for dir in path_env.split(';') {
            let candidate = std::path::Path::new(dir).join("bash.exe");
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    for candidate in [
        r"C:\Program Files\Git\bin\bash.exe",
        r"C:\Program Files (x86)\Git\bin\bash.exe",
    ] {
        if std::path::Path::new(candidate).exists() {
            return Some(candidate.into());
        }
    }
    None
}

#[cfg(not(windows))]
fn find_bash() -> Option<std::path::PathBuf> {
    Some("bash".into())
}

pub struct BashTool {
    tasks: TaskStore,
    output_dir: PathBuf,
}

impl BashTool {
    pub fn new(tasks: TaskStore, output_dir: PathBuf) -> Self {
        Self { tasks, output_dir }
    }
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Execute a shell command. Windows defaults to PowerShell; Unix-like systems default to sh. Use shell=bash|powershell|cmd|sh to override. Requires approval. run_in_background=true returns a task id immediately; otherwise waits for completion (timeout default 60s, max 5min)."
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![
            ToolParameter {
                name: "command".into(),
                param_type: "string".into(),
                description: "Shell command to execute".into(),
                required: true,
            },
            ToolParameter {
                name: "cwd".into(),
                param_type: "string".into(),
                description: "Working directory for the command (relative to the working directory)".into(),
                required: false,
            },
            ToolParameter {
                name: "timeout_ms".into(),
                param_type: "integer".into(),
                description: "Timeout in milliseconds (default 60000, max 300000)".into(),
                required: false,
            },
            ToolParameter {
                name: "run_in_background".into(),
                param_type: "boolean".into(),
                description: "Run as a background task and return a task id".into(),
                required: false,
            },
            ToolParameter {
                name: "description".into(),
                param_type: "string".into(),
                description: "Short description (required when run_in_background=true)".into(),
                required: false,
            },
            ToolParameter {
                name: "shell".into(),
                param_type: "string".into(),
                description: "Shell to use: 'powershell' (Windows default), 'cmd', 'bash', or 'sh' (Unix default)".into(),
                required: false,
            },
        ]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let command = args["command"].as_str().ok_or("command required")?;
        let background = args["run_in_background"].as_bool().unwrap_or(false);
        let shell = resolve_shell(args["shell"].as_str())?;
        let wd = working_dir_from_args(&args);

        let mut cmd = Command::new(&shell.program);
        for arg in &shell.args {
            cmd.arg(arg);
        }
        cmd.arg(command);
        #[cfg(windows)]
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        if let Some(cwd) = args["cwd"].as_str() {
            if !cwd.trim().is_empty() {
                cmd.current_dir(resolve_path(wd.as_deref(), cwd)?);
            }
        }

        if background {
            return self.run_background(&mut cmd, command, &args).await;
        }

        // Foreground: capture output with a hard timeout.
        let timeout_ms = args["timeout_ms"].as_u64().unwrap_or(DEFAULT_TIMEOUT_MS).clamp(1_000, MAX_TIMEOUT_MS);
        let timeout = std::time::Duration::from_millis(timeout_ms);

        cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| format!("spawn failed: {e}"))?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let run = async {
            let mut out = String::new();
            let mut err = String::new();
            let _ = tokio::join!(
                async {
                    if let Some(mut s) = stdout {
                        let _ = s.read_to_string(&mut out).await;
                    }
                },
                async {
                    if let Some(mut s) = stderr {
                        let _ = s.read_to_string(&mut err).await;
                    }
                },
            );
            let status = child.wait().await.ok();
            (status.and_then(|s| s.code()), out, err)
        };

        match tokio::time::timeout(timeout, run).await {
            Ok((code, out, err)) => {
                let mut text = format!("{out}{err}");
                let truncated = text.chars().count() > OUTPUT_LIMIT_CHARS;
                if truncated {
                    text = text.chars().take(OUTPUT_LIMIT_CHARS).collect();
                }
                let mut msg = format!("exit code: {:?}\n{}", code, text);
                if truncated {
                    msg.push_str(&format!("\n[...output truncated at {OUTPUT_LIMIT_CHARS} chars]"));
                }
                Ok(msg)
            }
            Err(_) => {
                // kill_on_drop terminates the child when `run` is dropped.
                Ok(format!("Command timed out after {timeout_ms}ms"))
            }
        }
    }
}

impl BashTool {
    async fn run_background(
        &self,
        cmd: &mut Command,
        command: &str,
        args: &serde_json::Value,
    ) -> Result<String, String> {
        let description = args["description"].as_str().unwrap_or("bash").to_string();
        let id = uuid::Uuid::new_v4().to_string();
        std::fs::create_dir_all(&self.output_dir).map_err(|e| format!("mkdir: {e}"))?;
        let log_path = self.output_dir.join(format!("{id}.log"));
        let log_file = std::fs::File::create(&log_path).map_err(|e| format!("log file: {e}"))?;
        let log_file2 = log_file.try_clone().map_err(|e| format!("log clone: {e}"))?;

        cmd.stdout(Stdio::from(log_file)).stderr(Stdio::from(log_file2));
        let mut child = cmd.spawn().map_err(|e| format!("spawn failed: {e}"))?;

        let cancel = Arc::new(AtomicBool::new(false));
        let info = TaskInfo {
            id: id.clone(),
            description: description.clone(),
            status: "running".to_string(),
            exit_code: None,
            output_path: Some(log_path.to_string_lossy().to_string()),
            created_at: crate::core::time::now_iso(),
        };
        self.tasks.lock().await.insert(
            id.clone(),
            TaskHandle {
                info: info.clone(),
                cancel: cancel.clone(),
            },
        );

        let tasks = self.tasks.clone();
        let cancel2 = cancel.clone();
        let log_path2 = log_path.clone();
        let task_id = id.clone();
        tokio::spawn(async move {
            // Watch for cancellation or completion.
            loop {
                if cancel2.load(Ordering::Relaxed) {
                    let _ = child.kill().await;
                    break;
                }
                if let Ok(Some(_)) = child.try_wait() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
            let status = child.wait().await.ok();
            let stopped = cancel2.load(Ordering::Relaxed);
            let status_str = if stopped {
                "stopped"
            } else if status.map(|s| s.success()).unwrap_or(false) {
                "completed"
            } else {
                "failed"
            };
            let mut map = tasks.lock().await;
            if let Some(handle) = map.get_mut(&task_id) {
                handle.info.status = status_str.to_string();
                handle.info.exit_code = status.and_then(|s| s.code());
            }
            drop(map);
            tracing::info!(task_id = %task_id, status = status_str, log = %log_path2.display(), "background task finished");
        });

        Ok(format!(
            "Started background task `{id}`: {description}\nLog: {}",
            log_path.display()
        ))
    }
}
