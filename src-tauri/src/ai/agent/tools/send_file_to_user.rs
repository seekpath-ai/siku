//! Send a file from this computer to the remote user's chat app.
//!
//! Registered ONLY for remote IM turns in a private, fully-granted chat
//! (docs/remote-channels.md §7.5), and it never sends anything itself: it
//! registers the file on the turn's outbound queue and `run_agent_turn`
//! serializes that queue into the assistant row, which is what the router
//! later uploads. That keeps the conversation and the chat app in sync — the
//! ledger row is the delivery source of truth (§7.1).

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::ai::agent::tool_registry::{Tool, ToolParameter};
use crate::remote::router::{OutboundFile, OutboundQueue};

/// Mirrors `remote::weixin::MAX_OUTBOUND_BYTES`; the channel enforces it again
/// before uploading, so a mismatch can never send something oversized.
const MAX_FILE_BYTES: u64 = 25 * 1024 * 1024;
const MAX_FILES_PER_TURN: usize = 5;
const MAX_TOTAL_BYTES: u64 = 50 * 1024 * 1024;

pub struct SendFileToUserTool {
    queue: OutboundQueue,
    allow_roots: Vec<PathBuf>,
    /// Monotonic counter so the ledger records a stable ordering.
    seq: AtomicU64,
}

impl SendFileToUserTool {
    pub fn new(queue: OutboundQueue, allow_roots: Vec<PathBuf>) -> Self {
        Self {
            queue,
            allow_roots,
            seq: AtomicU64::new(0),
        }
    }

    /// Directories a remote-initiated send may read from. Deliberately narrow:
    /// remote turns run with approval forced to Auto, so this is the only
    /// guard between "the user asked for their report" and "the agent mails
    /// itself the SSH keys".
    pub fn default_allow_roots(working_dir: Option<&Path>, app_data_dir: &Path) -> Vec<PathBuf> {
        let mut roots = vec![
            app_data_dir.join("remote_outbox"),
            app_data_dir.join("remote_downloads"),
        ];
        if let Some(wd) = working_dir {
            roots.push(wd.to_path_buf());
        }
        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from);
        if let Some(home) = home {
            for sub in ["Desktop", "Downloads", "Documents"] {
                roots.push(home.join(sub));
            }
        }
        roots
    }

    fn is_allowed(&self, path: &Path) -> bool {
        let target = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.allow_roots.iter().any(|root| {
            let root = root.canonicalize().unwrap_or_else(|_| root.clone());
            target.starts_with(&root)
        })
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn human_size(n: u64) -> String {
    if n >= 1024 * 1024 {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    } else if n >= 1024 {
        format!("{:.0} KB", n as f64 / 1024.0)
    } else {
        format!("{n} B")
    }
}

#[async_trait]
impl Tool for SendFileToUserTool {
    fn name(&self) -> &str {
        "send_file_to_user"
    }

    fn description(&self) -> &str {
        "Send a file from this computer to the user's chat app (WeChat/Feishu). \
         Use it when the user asks you to send them a file (e.g. a PDF you just made, a report, \
         a screenshot). The file goes out with this reply — the text you write is sent first, then \
         the file. Allowed locations: this session's workspace, files the user sent you, and the \
         user's Desktop/Downloads/Documents. Max 25 MB per file, 5 files per reply."
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![
            ToolParameter {
                name: "path".into(),
                param_type: "string".into(),
                description: "Absolute path of the file to send (or relative to the session's write directory)".into(),
                required: true,
            },
            ToolParameter {
                name: "caption".into(),
                param_type: "string".into(),
                description: "Optional one-line note sent right before the file (omit to send the file alone)".into(),
                required: false,
            },
        ]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let raw_path = args["path"].as_str().ok_or("path required")?;
        let wd = super::path::working_dir_from_args(&args);
        let resolved = super::path::resolve_path(wd.as_deref(), raw_path)?;

        if !resolved.exists() {
            return Ok(format!("File not found: {raw_path}"));
        }
        if !resolved.is_file() {
            return Ok(format!("Not a file: {raw_path}"));
        }
        let meta = std::fs::metadata(&resolved).map_err(|e| format!("读取文件信息失败: {e}"))?;
        let size = meta.len();
        if size == 0 {
            return Ok("文件为空，未发送。".to_string());
        }
        if size > MAX_FILE_BYTES {
            return Ok(format!(
                "文件过大（{}，上限 25MB），未发送。可先压缩或拆分后再发。",
                human_size(size)
            ));
        }
        if !self.is_allowed(&resolved) {
            return Ok(format!(
                "该路径不在允许发送的范围内（仅允许会话工作区、用户发来的文件、桌面/下载/文档目录）：{}",
                resolved.display()
            ));
        }

        let name = resolved
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "文件".to_string());
        let mime = crate::remote::weixin::outbound_mime(&name).to_string();
        let caption = args["caption"]
            .as_str()
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty());

        // Read once: the digest goes into the ledger and dedupe is content
        // based, so re-sending the same file in one turn is a no-op.
        let bytes = tokio::fs::read(&resolved).await.map_err(|e| format!("读取文件失败: {e}"))?;
        let sha256 = sha256_hex(&bytes);
        drop(bytes);

        let mut queued = self
            .queue
            .lock()
            .map_err(|_| "内部错误：发送队列不可用".to_string())?;
        if queued
            .iter()
            .any(|f| f.path == resolved && f.size == size)
        {
            return Ok(format!("{name} 已在本轮发送队列中，不会重复发送。"));
        }
        if queued.len() >= MAX_FILES_PER_TURN {
            return Ok(format!("每轮最多发送 {MAX_FILES_PER_TURN} 个文件，本轮已满，未发送 {name}。"));
        }
        let total: u64 = queued.iter().map(|f| f.size).sum();
        if total + size > MAX_TOTAL_BYTES {
            return Ok(format!(
                "本轮发送总量已达上限（{}），未发送 {name}。",
                human_size(MAX_TOTAL_BYTES)
            ));
        }
        self.seq.fetch_add(1, Ordering::Relaxed);
        queued.push(OutboundFile {
            path: resolved.clone(),
            name: name.clone(),
            size,
            mime,
            sha256,
            caption,
        });
        drop(queued);

        Ok(format!(
            "已登记：{name}（{}）。将随本轮回复发送到用户的聊天应用。",
            human_size(size)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn queue() -> OutboundQueue {
        Arc::new(std::sync::Mutex::new(Vec::new()))
    }

    #[tokio::test]
    async fn registers_files_inside_the_allowlist_and_ignores_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("workspace");
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("report.pdf");
        std::fs::write(&file, b"%PDF-1.4 test").unwrap();

        let q = queue();
        let tool = SendFileToUserTool::new(
            q.clone(),
            vec![root.clone()],
        );
        let args = serde_json::json!({
            "path": file.to_str().unwrap(),
            "caption": "这是报告",
            "_working_dir": root.to_str().unwrap(),
        });
        let out = tool.execute(args.clone()).await.unwrap();
        assert!(out.contains("已登记"), "{out}");
        assert_eq!(q.lock().unwrap().len(), 1);
        assert_eq!(q.lock().unwrap()[0].caption.as_deref(), Some("这是报告"));

        // A second identical call must not queue the file twice.
        let out = tool.execute(args).await.unwrap();
        assert!(out.contains("不会重复发送"), "{out}");
        assert_eq!(q.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn refuses_paths_outside_the_allowlist() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("workspace");
        std::fs::create_dir_all(&root).unwrap();
        let secret = dir.path().join("id_rsa");
        std::fs::write(&secret, b"private").unwrap();

        let q = queue();
        let tool = SendFileToUserTool::new(q.clone(), vec![root]);
        let out = tool
            .execute(serde_json::json!({ "path": secret.to_str().unwrap() }))
            .await
            .unwrap();
        assert!(out.contains("不在允许发送的范围"), "{out}");
        assert!(q.lock().unwrap().is_empty());
    }

    #[test]
    fn padded_and_human_size_helpers() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2 KB");
        assert_eq!(human_size(3 * 1024 * 1024), "3.0 MB");
    }
}
