//! Read-only local storage audit.
//!
//! The blob store is content-addressed (`blobs/<sha256>.<ext>`). There is no
//! delete-on-unreference yet, so the only safe first step is to *measure*:
//! scan the directory, collect every blob reference from the synced database,
//! and report current/history/unreferenced/staging/trash breakdowns. Nothing
//! here deletes or moves a file.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::Serialize;
use sqlx::SqlitePool;
use walkdir::WalkDir;

use crate::core::artifact_service::ARTIFACT_SCHEME;
use crate::file_store;

#[derive(Debug, Clone, Default, Serialize)]
pub struct StorageBreakdown {
    pub files: u64,
    pub bytes: u64,
}

impl StorageBreakdown {
    fn add(&mut self, bytes: u64) {
        self.files += 1;
        self.bytes += bytes;
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct StorageFile {
    pub name: String,
    pub hash: String,
    pub ext: String,
    pub bytes: u64,
    pub path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct StorageAudit {
    pub data_dir: String,
    pub blob_root: String,
    pub scanned_at: String,
    /// Every recognizable/file under `blobs/`, excluding `.incoming`/`.trash`.
    pub total: StorageBreakdown,
    pub current: StorageBreakdown,
    pub history_only: StorageBreakdown,
    pub orphan: StorageBreakdown,
    pub unknown: StorageBreakdown,
    pub staging: StorageBreakdown,
    pub trash: StorageBreakdown,
    pub duplicate_ext_groups: u64,
    /// Largest unreferenced blobs, for a quick "what is this?" check.
    pub top_orphans: Vec<StorageFile>,
}

#[derive(Debug, Clone)]
struct BlobFile {
    name: String,
    hash: String,
    ext: String,
    key: String,
    bytes: u64,
    path: String,
}

#[derive(Debug, Default)]
struct ScanResult {
    blobs: Vec<BlobFile>,
    total: StorageBreakdown,
    unknown: StorageBreakdown,
    staging: StorageBreakdown,
    trash: StorageBreakdown,
    duplicate_ext_groups: u64,
}

/// Run the read-only audit. Filesystem walking is blocking, so it happens on a
/// blocking worker; reference extraction is async DB reads.
pub async fn audit_storage(db: &SqlitePool, app_data_dir: &Path) -> Result<StorageAudit, String> {
    let root = file_store::blob_dir(app_data_dir);
    let scan_root = root.clone();
    let scan = tokio::task::spawn_blocking(move || scan_blob_files(&scan_root))
        .await
        .map_err(|e| format!("storage scan task failed: {e}"))??;

    let mut current = HashSet::new();
    let mut history = HashSet::new();

    // Current references: library files, vault files, current notes, artifacts,
    // and chat message content (artifact URIs / pasted blob links).
    collect_paths(
        db,
        "SELECT file_path FROM papers WHERE file_path LIKE 'blobs/%' \
         UNION SELECT file_path FROM attachments WHERE file_path LIKE 'blobs/%' \
         UNION SELECT blob_path FROM files WHERE blob_path LIKE 'blobs/%'",
        &mut current,
    )
    .await?;
    collect_paths(
        db,
        "SELECT 'blobs/' || hash || '.' || ext FROM session_artifacts",
        &mut current,
    )
    .await?;
    collect_text_refs(
        db,
        "SELECT content FROM notes \
         WHERE content LIKE '%blobs/%' OR content LIKE '%siku://artifact/%'",
        &mut current,
    )
    .await?;
    collect_text_refs(
        db,
        "SELECT content FROM chat_messages \
         WHERE content LIKE '%blobs/%' OR content LIKE '%siku://artifact/%'",
        &mut current,
    )
    .await?;

    // Historical-only references. These should not be deleted casually: a
    // restored note version or an expanded memory may still need the image.
    collect_text_refs(
        db,
        "SELECT content FROM note_versions \
         WHERE content LIKE '%blobs/%' OR content LIKE '%siku://artifact/%'",
        &mut history,
    )
    .await?;
    collect_text_refs(
        db,
        "SELECT content FROM agent_memories \
         WHERE content LIKE '%blobs/%' OR content LIKE '%siku://artifact/%' \
         UNION ALL SELECT content FROM agent_memories_local \
         WHERE content LIKE '%blobs/%' OR content LIKE '%siku://artifact/%'",
        &mut history,
    )
    .await?;
    collect_agent_steps(db, &mut history).await?;

    let mut current_breakdown = StorageBreakdown::default();
    let mut history_breakdown = StorageBreakdown::default();
    let mut orphan_breakdown = StorageBreakdown::default();
    let mut top_orphans = Vec::new();
    for file in scan.blobs {
        if current.contains(&file.key) {
            current_breakdown.add(file.bytes);
        } else if history.contains(&file.key) {
            history_breakdown.add(file.bytes);
        } else {
            orphan_breakdown.add(file.bytes);
            top_orphans.push(StorageFile {
                name: file.name,
                hash: file.hash,
                ext: file.ext,
                bytes: file.bytes,
                path: file.path,
            });
        }
    }
    top_orphans.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    top_orphans.truncate(20);

    Ok(StorageAudit {
        data_dir: app_data_dir.to_string_lossy().into_owned(),
        blob_root: root.to_string_lossy().into_owned(),
        scanned_at: crate::core::time::now_iso(),
        total: scan.total,
        current: current_breakdown,
        history_only: history_breakdown,
        orphan: orphan_breakdown,
        unknown: scan.unknown,
        staging: scan.staging,
        trash: scan.trash,
        duplicate_ext_groups: scan.duplicate_ext_groups,
        top_orphans,
    })
}

fn scan_blob_files(root: &Path) -> Result<ScanResult, String> {
    let mut result = ScanResult::default();
    if !root.exists() {
        return Ok(result);
    }
    let mut ext_by_hash: HashMap<String, HashSet<String>> = HashMap::new();

    for entry in WalkDir::new(root).follow_links(false).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let rel = path.strip_prefix(root).unwrap_or(path);
        let first = rel
            .components()
            .next()
            .and_then(|c| c.as_os_str().to_str())
            .unwrap_or("");
        let bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
        match first {
            ".incoming" => {
                result.staging.add(bytes);
                continue;
            }
            ".trash" => {
                result.trash.add(bytes);
                continue;
            }
            _ => {}
        }

        let name = entry.file_name().to_string_lossy().to_string();
        let Some((hash, ext)) = parse_blob_name(&name) else {
            // `.DS_Store`, temp files, or a future non-blob layout.
            result.unknown.add(bytes);
            result.total.add(bytes);
            continue;
        };
        let key = ref_key(&hash, &ext);
        ext_by_hash.entry(hash.clone()).or_default().insert(ext.clone());
        result.blobs.push(BlobFile {
            name,
            hash,
            ext,
            key,
            bytes,
            path: path.to_string_lossy().into_owned(),
        });
        result.total.add(bytes);
    }

    result.duplicate_ext_groups = ext_by_hash
        .values()
        .filter(|exts| exts.len() > 1)
        .count() as u64;
    Ok(result)
}

fn parse_blob_name(name: &str) -> Option<(String, String)> {
    let (hash, ext) = name.rsplit_once('.')?;
    if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    if ext.is_empty() || ext.len() > 16 || !ext.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some((hash.to_ascii_lowercase(), ext.to_ascii_lowercase()))
}

fn ref_key(hash: &str, ext: &str) -> String {
    format!("{}.{}", hash.to_ascii_lowercase(), ext.to_ascii_lowercase())
}

async fn collect_paths(db: &SqlitePool, sql: &str, target: &mut HashSet<String>) -> Result<(), String> {
    let rows: Vec<(String,)> = sqlx::query_as(sql)
        .fetch_all(db)
        .await
        .map_err(|e| format!("storage audit query failed: {e}"))?;
    for (path,) in rows {
        if let Some((hash, ext)) = file_store::parse_blob_path(&path) {
            target.insert(ref_key(&hash, &ext));
        }
    }
    Ok(())
}

async fn collect_text_refs(
    db: &SqlitePool,
    sql: &str,
    target: &mut HashSet<String>,
) -> Result<(), String> {
    let rows: Vec<(String,)> = sqlx::query_as(sql)
        .fetch_all(db)
        .await
        .map_err(|e| format!("storage audit query failed: {e}"))?;
    for (text,) in rows {
        for key in extract_refs(&text) {
            target.insert(key);
        }
    }
    Ok(())
}

async fn collect_agent_steps(db: &SqlitePool, target: &mut HashSet<String>) -> Result<(), String> {
    let rows: Vec<(Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT narration, tool_calls, reasoning_content FROM agent_steps \
         WHERE narration LIKE '%blobs/%' OR narration LIKE '%siku://artifact/%' \
            OR tool_calls LIKE '%blobs/%' OR tool_calls LIKE '%siku://artifact/%' \
            OR reasoning_content LIKE '%blobs/%' OR reasoning_content LIKE '%siku://artifact/%'",
    )
    .fetch_all(db)
    .await
    .map_err(|e| format!("storage audit query failed: {e}"))?;
    for (narration, tool_calls, reasoning) in rows {
        for text in [narration, tool_calls, reasoning].into_iter().flatten() {
            for key in extract_refs(&text) {
                target.insert(key);
            }
        }
    }
    Ok(())
}

fn extract_refs(text: &str) -> Vec<String> {
    let mut out = HashSet::new();
    extract_blob_refs(text, &mut out);
    extract_artifact_refs(text, &mut out);
    out.into_iter().collect()
}

fn extract_blob_refs(text: &str, out: &mut HashSet<String>) {
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let Some(rel) = bytes[i..].windows(6).position(|w| w == b"blobs/") else {
            break;
        };
        let hash_start = i + rel + 6;
        let hash_len = bytes[hash_start..]
            .iter()
            .take_while(|b| b.is_ascii_hexdigit())
            .count();
        if hash_len < 64 {
            i = hash_start + hash_len;
            continue;
        }
        let hash = String::from_utf8_lossy(&bytes[hash_start..hash_start + 64]);
        let mut end = hash_start + 64;
        let mut ext = String::new();
        if bytes.get(end) == Some(&b'.') {
            let ext_len = bytes[end + 1..]
                .iter()
                .take_while(|b| b.is_ascii_alphanumeric())
                .take(16)
                .count();
            if ext_len >= 1 {
                ext = String::from_utf8_lossy(&bytes[end + 1..end + 1 + ext_len]).to_string();
                end += 1 + ext_len;
            }
        }
        if !ext.is_empty() {
            out.insert(ref_key(&hash, &ext));
        }
        i = end;
    }
}

fn extract_artifact_refs(text: &str, out: &mut HashSet<String>) {
    let mut i = 0usize;
    while let Some(rel) = text[i..].find(ARTIFACT_SCHEME) {
        let start = i + rel + ARTIFACT_SCHEME.len();
        let rest = &text[start..];
        let end = rest
            .find(|c: char| c.is_whitespace() || matches!(c, ')' | ']' | '"' | '\'' | '>' | '<'))
            .unwrap_or(rest.len());
        let token = &rest[..end];
        if let Some((hash, ext)) = token.rsplit_once('.') {
            if hash.len() == 64
                && hash.chars().all(|c| c.is_ascii_hexdigit())
                && !ext.is_empty()
                && ext.len() <= 8
                && ext.chars().all(|c| c.is_ascii_alphanumeric())
            {
                out.insert(ref_key(hash, ext));
            }
        }
        i = start + end.max(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    fn hash(ch: char) -> String {
        ch.to_string().repeat(64)
    }

    async fn test_db() -> SqlitePool {
        let db = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        for sql in [
            "CREATE TABLE papers (file_path TEXT)",
            "CREATE TABLE attachments (file_path TEXT)",
            "CREATE TABLE files (blob_path TEXT)",
            "CREATE TABLE notes (content TEXT)",
            "CREATE TABLE note_versions (content TEXT)",
            "CREATE TABLE chat_messages (content TEXT)",
            "CREATE TABLE session_artifacts (hash TEXT, ext TEXT)",
            "CREATE TABLE agent_steps (narration TEXT, tool_calls TEXT, reasoning_content TEXT)",
            "CREATE TABLE agent_memories (content TEXT)",
            "CREATE TABLE agent_memories_local (content TEXT)",
        ] {
            sqlx::query(sql).execute(&db).await.unwrap();
        }
        db
    }

    #[tokio::test]
    async fn audit_classifies_current_history_orphan_and_staging() {
        let dir = tempfile::tempdir().unwrap();
        let blobs = file_store::blob_dir(dir.path());
        std::fs::create_dir_all(blobs.join(".incoming")).unwrap();
        std::fs::create_dir_all(blobs.join(".trash")).unwrap();

        let a = hash('a');
        let b = hash('b');
        let c = hash('c');
        let d = hash('d');
        std::fs::write(blobs.join(format!("{a}.pdf")), vec![0u8; 10]).unwrap();
        std::fs::write(blobs.join(format!("{b}.png")), vec![0u8; 20]).unwrap();
        std::fs::write(blobs.join(format!("{c}.bin")), vec![0u8; 30]).unwrap();
        std::fs::write(blobs.join(format!("{d}.png")), vec![0u8; 40]).unwrap();
        std::fs::write(blobs.join(format!("{a}.bin")), vec![0u8; 5]).unwrap();
        std::fs::write(blobs.join(".DS_Store"), b"x").unwrap();
        std::fs::write(blobs.join(".incoming").join("chunk.part"), vec![0u8; 7]).unwrap();
        std::fs::write(blobs.join(".trash").join("old.pdf"), vec![0u8; 9]).unwrap();

        let db = test_db().await;
        sqlx::query("INSERT INTO papers (file_path) VALUES (?)")
            .bind(format!("blobs/{a}.pdf"))
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO note_versions (content) VALUES (?)")
            .bind(format!("![](blobs/{b}.png)"))
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO chat_messages (content) VALUES (?)")
            .bind(format!("附件 [x](siku://artifact/{d}.png)"))
            .execute(&db)
            .await
            .unwrap();

        let audit = audit_storage(&db, dir.path()).await.unwrap();
        assert_eq!(audit.total.files, 6, "5 blobs + .DS_Store");
        assert_eq!(audit.current.files, 2);
        assert_eq!(audit.current.bytes, 50);
        assert_eq!(audit.history_only.files, 1);
        assert_eq!(audit.history_only.bytes, 20);
        assert_eq!(audit.orphan.files, 2);
        assert_eq!(audit.orphan.bytes, 35);
        assert_eq!(audit.unknown.files, 1);
        assert_eq!(audit.staging.files, 1);
        assert_eq!(audit.trash.files, 1);
        assert_eq!(audit.duplicate_ext_groups, 1, "a.pdf + a.bin share a hash");
        assert_eq!(audit.top_orphans.len(), 2);
        assert_eq!(audit.top_orphans[0].bytes, 30, "largest orphan first");
        db.close().await;
    }
}
