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
    /// Prepended to every command so the console emits UTF-8. zh-CN Windows
    /// defaults to GBK (cp936): any non-ASCII byte in the output fails strict
    /// UTF-8 decoding downstream and the whole stdout vanishes.
    utf8_prefix: &'static str,
}

impl Shell {
    fn new(program: impl Into<std::path::PathBuf>, args: Vec<&'static str>, utf8_prefix: &'static str) -> Self {
        Self {
            program: program.into(),
            args,
            utf8_prefix,
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
                "[Console]::OutputEncoding=[Text.Encoding]::UTF8;$OutputEncoding=[Text.Encoding]::UTF8;",
            ));
        }
        "cmd" => {
            return Ok(Shell::new("cmd.exe", vec!["/c"], "chcp 65001 >nul & "));
        }
        "bash" => {
            return Ok(find_bash()
                .map(|p| Shell::new(p, vec!["-lc"], ""))
                .ok_or_else(|| {
                    "bash not found: install Git for Windows and add bash.exe to PATH, or use powershell/cmd".to_string()
                })?);
        }
        "sh" => {
            // Non-login: sh is often dash, and a login shell sources
            // /etc/profile.d — a single bashism in a user profile kills the
            // command with "Syntax error" before it ever runs.
            return Ok(Shell::new("sh", vec!["-c"], ""));
        }
        "" => {} // fall through to platform defaults
        _ => return Err(format!("unsupported shell: {name}")),
    }

    // Platform defaults.
    if cfg!(windows) {
        Ok(Shell::new(
            "powershell.exe",
            vec!["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command"],
            "[Console]::OutputEncoding=[Text.Encoding]::UTF8;$OutputEncoding=[Text.Encoding]::UTF8;",
        ))
    } else if find_bash().is_some() {
        // Prefer bash (login shell, for profile PATH) over sh: bash parses
        // the bashisms common in user profiles that break dash, and models
        // write more reliable bash anyway.
        Ok(Shell::new(find_bash().unwrap(), vec!["-lc"], ""))
    } else {
        Ok(Shell::new("sh", vec!["-c"], ""))
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
    /// 归属会话 id：run_background 产生的 TaskInfo 会带上它
    session_id: Option<String>,
    desc: String,
}

impl BashTool {
    pub fn new(tasks: TaskStore, output_dir: PathBuf, session_id: Option<String>) -> Self {
        Self { tasks, output_dir, session_id, desc: build_description() }
    }
}

fn build_description() -> String {
    const BASE: &str = "Execute a shell command. Windows defaults to PowerShell; Unix-like systems default to bash (falling back to sh). Use shell=bash|powershell|cmd|sh to override. Requires approval. Commands run in the working directory by default when one is set (otherwise the process cwd). run_in_background=true returns a task id immediately; otherwise waits for completion. Foreground timeout: default 60s, max 5min. Background tasks accept timeout_ms=0 to disable the timeout entirely (long builds, watchers, servers).";
    // LLMs write far more reliable bash than PowerShell; when Git Bash is
    // installed, say so and steer Unix-style work to it instead of keeping
    // bash as a fallback the model never thinks to use.
    if cfg!(windows) && find_bash().is_some() {
        format!("{BASE} Git Bash IS available on this machine: prefer shell=bash for Unix-style work (pipes, grep/sed/awk, globbing, file inspection); use PowerShell only for Windows-specific tasks (registry, services, WMI, systeminfo).")
    } else {
        BASE.to_string()
    }
}

/// stdbuf (GNU coreutils) LD_PRELOADs libstdbuf into the shell, and the
/// preload + env propagate to dynamically-linked descendants — forcing
/// line-buffered stdout/stderr. Without it, output of a redirected (non-TTY)
/// process sits in a 4–8KB block buffer and is LOST when the task is killed
/// before flush: the classic "background task log is empty" bug.
#[cfg(unix)]
fn stdbuf_available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        std::process::Command::new("stdbuf")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })
}

/// Build the command for a shell invocation, applying the shared process
/// setup: unbuffered-output env hints, a dedicated Unix process group (so
/// stop/timeout can kill the whole tree), and the stdbuf wrapper.
fn build_command(shell: &Shell, command: &str) -> Command {
    #[cfg(unix)]
    let mut cmd = if stdbuf_available() {
        // stdbuf execs the shell, so the pid (and process group) is unchanged.
        let mut c = Command::new("stdbuf");
        c.arg("-oL").arg("-eL").arg(&shell.program);
        c
    } else {
        Command::new(&shell.program)
    };
    #[cfg(not(unix))]
    let mut cmd = Command::new(&shell.program);

    for arg in &shell.args {
        cmd.arg(arg);
    }
    cmd.arg(command);
    // Unbuffered-output hints for runtimes that read them (stdbuf can't
    // reach interpreters that manage their own buffering).
    cmd.env("PYTHONUNBUFFERED", "1");
    cmd.env("PYTHONIOENCODING", "utf-8");
    // Own process group on Unix: pgid == child pid, so stop/timeout can
    // SIGKILL the whole tree instead of orphaning the shell's children.
    #[cfg(unix)]
    cmd.process_group(0);
    cmd
}

/// Kill the whole process tree of a spawned child. `Child::kill` only
/// terminates the shell itself; its children would survive as orphans and
/// keep writing to the log after the task already reads "stopped".
fn kill_process_tree(pid: Option<u32>) {
    let Some(pid) = pid else { return };
    #[cfg(unix)]
    {
        // process_group(0) at spawn ⇒ pgid == pid. The `--` is required:
        // without it kill(1) parses "-<pgid>" as options and does nothing.
        let _ = std::process::Command::new("kill")
            .args(["-9", "--", &format!("-{pid}")])
            .status();
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .status();
    }
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        &self.desc
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
                description: "Working directory for the command (relative to the working directory). Defaults to the working directory itself when one is set; otherwise the process cwd.".into(),
                required: false,
            },
            ToolParameter {
                name: "timeout_ms".into(),
                param_type: "integer".into(),
                description: "Timeout in milliseconds (default 60000, max 300000; background tasks accept 0 = no timeout)".into(),
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
        // Force the console to UTF-8 (no-op prefix on bash/sh).
        let command = format!("{}{}", shell.utf8_prefix, command);

        let mut cmd = build_command(&shell, &command);
        #[cfg(windows)]
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        // Explicit cwd: fail loudly — the caller asked for that directory.
        // Default cwd (= sandbox root): a missing root must NOT kill
        // sandbox-agnostic commands (e.g. `systeminfo`). The working dir
        // never constrained bash anyway (a command can `cd` anywhere), so
        // degrade to the process cwd and say so in the output.
        let mut cwd_fallback: Option<String> = None;
        match args["cwd"].as_str().filter(|c| !c.trim().is_empty()) {
            Some(cwd) => {
                cmd.current_dir(resolve_path(wd.as_deref(), cwd)?);
            }
            None if wd.is_some() => match resolve_path(wd.as_deref(), ".") {
                Ok(dir) => {
                    cmd.current_dir(dir);
                }
                Err(e) => {
                    cwd_fallback = Some(format!(
                        "working directory unavailable ({e}); command ran in the process cwd instead"
                    ));
                }
            },
            None => {}
        }

        if background {
            return self.run_background(&mut cmd, &args, cwd_fallback).await;
        }

        // Foreground: capture output with a hard timeout. timeout_ms=0 means
        // "no timeout" for background tasks only; foreground treats it as
        // "default" (a 1s clamp would surprise the caller).
        let timeout_ms = args["timeout_ms"]
            .as_u64()
            .filter(|&v| v > 0)
            .unwrap_or(DEFAULT_TIMEOUT_MS)
            .clamp(1_000, MAX_TIMEOUT_MS);
        let timeout = std::time::Duration::from_millis(timeout_ms);

        cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| format!("spawn failed: {e}"))?;
        let child_pid = child.id();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let run = async {
            // Read raw bytes and decode lossily: a stray non-UTF-8 byte must
            // degrade to �, never discard the whole output (read_to_string
            // errors out and leaves the buffer empty — the "PowerShell stdout
            // never arrives" bug).
            let mut out_bytes = Vec::new();
            let mut err_bytes = Vec::new();
            let _ = tokio::join!(
                async {
                    if let Some(mut s) = stdout {
                        let _ = s.read_to_end(&mut out_bytes).await;
                    }
                },
                async {
                    if let Some(mut s) = stderr {
                        let _ = s.read_to_end(&mut err_bytes).await;
                    }
                },
            );
            let status = child.wait().await.ok();
            let out = String::from_utf8_lossy(&out_bytes).into_owned();
            let err = String::from_utf8_lossy(&err_bytes).into_owned();
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
                if let Some(w) = cwd_fallback {
                    msg = format!("[warning: {w}]\n{msg}");
                }
                Ok(msg)
            }
            Err(_) => {
                // kill_on_drop terminates the shell when `run` is dropped;
                // sweep the rest of the tree so spawned children don't
                // survive the timeout as orphans.
                kill_process_tree(child_pid);
                let mut msg = format!("Command timed out after {timeout_ms}ms");
                if let Some(w) = cwd_fallback {
                    msg = format!("[warning: {w}]\n{msg}");
                }
                Ok(msg)
            }
        }
    }
}

impl BashTool {
    async fn run_background(
        &self,
        cmd: &mut Command,
        args: &serde_json::Value,
        cwd_fallback: Option<String>,
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
            session_id: self.session_id.clone(),
            created_at: crate::core::time::now_iso(),
            // Filled live at snapshot time.
            log_bytes: None,
            log_modified: None,
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
        // Background tasks are for long-running work: timeout_ms=0 disables
        // the timeout entirely; an explicit value keeps the 5min cap; absent
        // means the 60s default (foreground semantics, least surprise).
        let timeout_ms = args["timeout_ms"].as_u64().unwrap_or(DEFAULT_TIMEOUT_MS);
        let deadline = (timeout_ms > 0).then(|| {
            std::time::Instant::now()
                + std::time::Duration::from_millis(timeout_ms.clamp(1_000, MAX_TIMEOUT_MS))
        });
        tokio::spawn(async move {
            let mut timed_out = false;
            // Watch for cancellation, timeout, or completion.
            loop {
                if cancel2.load(Ordering::Relaxed) {
                    kill_process_tree(child.id());
                    break;
                }
                if let Some(deadline) = deadline {
                    if std::time::Instant::now() >= deadline {
                        timed_out = true;
                        kill_process_tree(child.id());
                        break;
                    }
                }
                if let Ok(Some(_)) = child.try_wait() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
            let status = child.wait().await.ok();
            let stopped = cancel2.load(Ordering::Relaxed);
            if timed_out {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&log_path2) {
                    let _ = writeln!(f, "\n[...task timed out after {timeout_ms}ms]");
                }
                tracing::warn!(task_id = %task_id, timeout_ms, "background task timed out");
            }
            let status_str = if stopped {
                "stopped"
            } else if timed_out {
                "timed_out"
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

        let mut msg =
            format!("Started background task `{id}`: {description}\nLog: {}", log_path.display());
        if let Some(w) = cwd_fallback {
            msg = format!("[warning: {w}]\n{msg}");
        }
        Ok(msg)
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn missing_dir() -> PathBuf {
        std::env::temp_dir()
            .join(format!("siku-bash-wd-test-{}", std::process::id()))
            .join("no-such-dir")
    }

    /// A missing sandbox root must degrade to the process cwd with a
    /// warning instead of killing the command — the working dir never
    /// actually confined bash, so sandbox-agnostic commands (e.g.
    /// `systeminfo`) must keep working.
    #[tokio::test]
    async fn missing_working_dir_degrades_with_warning() {
        let tool = BashTool::new(TaskStore::default(), std::env::temp_dir(), None);
        let missing = missing_dir();
        let out = tool
            .execute(serde_json::json!({
                "command": "echo siku-bash-degrade-ok",
                "_working_dir": missing.to_str().unwrap(),
            }))
            .await
            .expect("command must run despite the missing working dir");
        assert!(out.contains("warning"), "expected warning prefix: {out}");
        assert!(
            out.contains("no-such-dir"),
            "warning must name the failing path: {out}"
        );
    }

    /// An explicitly requested cwd must still fail loudly — the caller
    /// asked for that directory.
    #[tokio::test]
    async fn explicit_cwd_fails_loudly() {
        let tool = BashTool::new(TaskStore::default(), std::env::temp_dir(), None);
        let missing = missing_dir();
        let err = tool
            .execute(serde_json::json!({
                "command": "echo hi",
                "cwd": ".",
                "_working_dir": missing.to_str().unwrap(),
            }))
            .await
            .unwrap_err();
        assert!(err.contains("working directory error"), "{err}");
        assert!(err.contains("no-such-dir"), "error must name the path: {err}");
    }

    /// A background task past its timeout must end as `timed_out` (not
    /// `failed`), with the marker appended to the log.
    #[cfg(unix)]
    #[tokio::test]
    async fn background_timeout_marks_timed_out() {
        let tasks: TaskStore = Default::default();
        let tool = BashTool::new(tasks.clone(), std::env::temp_dir(), None);
        let out = tool
            .execute(serde_json::json!({
                "command": "sleep 30",
                "run_in_background": true,
                "timeout_ms": 1000,
                "shell": "sh",
            }))
            .await
            .expect("background spawn");
        let id = out.split('`').nth(1).expect("task id in response").to_string();
        // Poll until the watcher finishes (timeout 1s + poll interval).
        let mut status = String::new();
        for _ in 0..40 {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            let map = tasks.lock().await;
            if let Some(h) = map.get(&id) {
                status = h.info.status.clone();
            }
            drop(map);
            if status != "running" {
                break;
            }
        }
        assert_eq!(status, "timed_out", "task must end as timed_out");
    }
}
