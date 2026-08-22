//! JSONL memory storage for agent conversations.
//! Each agent/session gets a `.jsonl` file; each line is one message record.
//! A `.meta` sidecar caches the line count for efficient tail reads.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use tracing::{error, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub timestamp: i64,
    pub time: DateTime<Utc>,
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
}

pub struct MemoryStore {
    file_path: PathBuf,
}

impl MemoryStore {
    pub fn new(file_path: impl AsRef<Path>) -> Self {
        let file_path = file_path.as_ref().to_path_buf();
        if let Some(parent) = file_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        Self { file_path }
    }

    pub fn default_path(base_dir: &Path, session_id: &str) -> PathBuf {
        base_dir.join(format!("session_{}.jsonl", session_id))
    }

    fn meta_path(&self) -> PathBuf {
        self.file_path.with_extension("jsonl.meta")
    }

    fn read_meta(&self) -> Option<usize> {
        let data = std::fs::read_to_string(self.meta_path()).ok()?;
        let parsed: serde_json::Value = serde_json::from_str(&data).ok()?;
        parsed.get("line_count")?.as_u64().map(|n| n as usize)
    }

    fn write_meta(&self, line_count: usize) {
        let _ = std::fs::write(
            self.meta_path(),
            serde_json::json!({ "line_count": line_count }).to_string(),
        );
    }

    fn rebuild_meta(&self) -> usize {
        let count = self.count_lines();
        self.write_meta(count);
        count
    }

    fn count_lines(&self) -> usize {
        if !self.file_path.exists() {
            return 0;
        }
        match std::fs::File::open(&self.file_path) {
            Ok(file) => std::io::BufReader::new(file)
                .lines()
                .filter_map(|l| l.ok())
                .filter(|l| !l.trim().is_empty())
                .count(),
            Err(_) => 0,
        }
    }

    /// Append a message record to the JSONL file.
    pub fn append(
        &self,
        role: &str,
        content: &str,
        tool_calls: Option<&str>,
        tool_call_id: Option<&str>,
        tool_name: Option<&str>,
    ) {
        let now = Utc::now();
        let record = MemoryRecord {
            timestamp: now.timestamp(),
            time: now,
            role: role.to_string(),
            content: content.to_string(),
            tool_calls: tool_calls.map(|s| s.to_string()),
            tool_call_id: tool_call_id.map(|s| s.to_string()),
            tool_name: tool_name.map(|s| s.to_string()),
        };

        let line = match serde_json::to_string(&record) {
            Ok(s) => s,
            Err(e) => {
                error!(error = %e, "failed to serialize memory record");
                return;
            }
        };

        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.file_path)
        {
            let _ = writeln!(file, "{}", line);
        }

        let current = self.read_meta().unwrap_or_else(|| self.rebuild_meta());
        self.write_meta(current + 1);
    }

    /// Load the most recent `max_rounds` of conversation.
    pub fn load_recent(&self, max_rounds: usize) -> Vec<MemoryRecord> {
        let max_lines = max_rounds.saturating_mul(2).max(1);
        let lines = self.tail_lines(max_lines);
        let mut records: Vec<MemoryRecord> = Vec::with_capacity(lines.len());
        for line in lines {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<MemoryRecord>(&line) {
                Ok(r) => records.push(r),
                Err(e) => warn!(error = %e, "skipping malformed memory line"),
            }
        }
        records
    }

    /// Load the full conversation memory.
    pub fn load_all(&self) -> Vec<MemoryRecord> {
        if !self.file_path.exists() {
            return Vec::new();
        }
        match std::fs::File::open(&self.file_path) {
            Ok(file) => {
                let mut records = Vec::new();
                for line in std::io::BufReader::new(file).lines() {
                    let line = match line {
                        Ok(l) => l,
                        Err(e) => {
                            warn!(error = %e, "skipping unreadable memory line");
                            continue;
                        }
                    };
                    if line.trim().is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<MemoryRecord>(&line) {
                        Ok(r) => records.push(r),
                        Err(e) => warn!(error = %e, "skipping malformed memory line"),
                    }
                }
                records
            }
            Err(_) => Vec::new(),
        }
    }

    /// Load a paginated range from the end of the file.
    pub fn load_range(&self, offset: usize, limit: usize) -> (Vec<MemoryRecord>, usize, bool) {
        let total = self.read_meta().unwrap_or_else(|| self.rebuild_meta());
        let tail_count = (offset + limit).min(total).max(1);
        let lines = self.tail_lines(tail_count);
        let mut records: Vec<MemoryRecord> = Vec::with_capacity(limit);
        for line in lines.into_iter().skip(offset).take(limit) {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(r) = serde_json::from_str::<MemoryRecord>(&line) {
                records.push(r);
            }
        }
        let has_more = total > offset + limit;
        (records, total, has_more)
    }

    /// Delete the memory file and its meta sidecar.
    pub fn delete(&self) {
        let _ = std::fs::remove_file(&self.file_path);
        let _ = std::fs::remove_file(self.meta_path());
    }

    /// Read the last `max_lines` non-empty lines from the file efficiently.
    fn tail_lines(&self, max_lines: usize) -> Vec<String> {
        if !self.file_path.exists() {
            return Vec::new();
        }

        const CHUNK_SIZE: usize = 4096;
        let mut file = match std::fs::File::open(&self.file_path) {
            Ok(f) => f,
            Err(_) => return Vec::new(),
        };
        let metadata = match file.metadata() {
            Ok(m) => m,
            Err(_) => return Vec::new(),
        };
        let file_size = metadata.len() as usize;
        if file_size == 0 {
            return Vec::new();
        }

        let mut lines: Vec<String> = Vec::new();
        let mut read_pos = file_size;
        let mut leftover = String::new();

        while read_pos > 0 && lines.len() <= max_lines {
            let chunk_len = CHUNK_SIZE.min(read_pos);
            read_pos -= chunk_len;

            use std::io::{Read, Seek};
            if file.seek(std::io::SeekFrom::Start(read_pos as u64)).is_err() {
                break;
            }
            let mut buf = vec![0u8; chunk_len];
            if file.read_exact(&mut buf).is_err() {
                break;
            }
            let chunk = String::from_utf8_lossy(&buf);
            let combined = format!("{}{}", chunk, leftover);
            let parts: Vec<&str> = combined.split('\n').collect();
            leftover = parts[0].to_string();
            for i in (1..parts.len()).rev() {
                let trimmed = parts[i].trim();
                if !trimmed.is_empty() {
                    lines.push(parts[i].to_string());
                    if lines.len() >= max_lines {
                        break;
                    }
                }
            }
        }

        if read_pos == 0 && !leftover.trim().is_empty() {
            lines.push(leftover);
        }

        lines.reverse();
        lines
    }
}
