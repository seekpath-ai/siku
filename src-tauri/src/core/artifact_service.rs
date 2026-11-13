//! Session artifacts: files the agent produced during a turn.
//!
//! The bytes live in the content-addressed blob store (shared with papers,
//! notes and attachments); `session_artifacts` is the synced index that tells
//! other devices what exists. Blob transfer reuses the attachment channel
//! (`collect_missing_blob_hashes` + `mark_blob_pending_push`), so no new sync
//! protocol is involved.
//!
//! References use `siku://artifact/<sha256>.<ext>` — device-independent, unlike
//! the absolute paths agents see in their tools.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use sqlx::SqlitePool;

use crate::core::time::now_iso;

pub const ARTIFACT_SCHEME: &str = "siku://artifact/";

/// Per-turn registration bounds: a runaway build must not turn into hundreds
/// of blob writes right after the answer.
const MAX_ARTIFACTS_PER_TURN: usize = 30;
const MAX_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 128 * 1024 * 1024;
const MAX_SCAN_ENTRIES: usize = 5_000;
const MAX_SCAN_DEPTH: usize = 4;
/// Filesystem mtime granularity (FAT: 2s) means "written this turn" needs a
/// little slack or a file written at the turn boundary could be missed.
const MTIME_SLACK: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct Artifact {
    pub hash: String,
    pub ext: String,
    pub name: String,
}

pub fn artifact_uri(hash: &str, ext: &str) -> String {
    format!("{ARTIFACT_SCHEME}{hash}.{ext}")
}

pub fn is_artifact_uri(s: &str) -> bool {
    s.starts_with(ARTIFACT_SCHEME)
}

/// Parse `siku://artifact/<64-hex>.<ext>` (query/fragment ignored). The ext is
/// validated (alphanumeric, 1-8 chars) so a crafted URI cannot escape the blob
/// directory.
pub fn parse_artifact_uri(uri: &str) -> Option<(String, String)> {
    let rest = uri
        .trim()
        .strip_prefix(ARTIFACT_SCHEME)?
        .split(['?', '#'])
        .next()?;
    let (hash, ext) = rest.rsplit_once('.')?;
    if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    if ext.is_empty() || ext.len() > 8 || !ext.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some((hash.to_ascii_lowercase(), ext.to_ascii_lowercase()))
}

/// Absolute path of an artifact's blob on this device. Errors when the blob
/// has not been synced yet (the peer requested it; the user can retry).
pub fn resolve_uri(app_data_dir: &Path, uri: &str) -> Result<String, String> {
    if !is_artifact_uri(uri) {
        return Err("无效的产物引用".to_string());
    }
    let (hash, ext) = parse_artifact_uri(uri).ok_or_else(|| "无效的产物引用".to_string())?;
    let path = crate::file_store::blob_path(app_data_dir, &hash, &ext);
    if path.exists() {
        Ok(path.to_string_lossy().into_owned())
    } else {
        Err("产物尚未同步到本机，请稍后重试".to_string())
    }
}

struct Candidate {
    hash: String,
    ext: String,
    name: String,
    size: u64,
}

fn sanitize_ext(ext: &str) -> String {
    let cleaned: String = ext
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect::<String>()
        .to_ascii_lowercase();
    if cleaned.is_empty() {
        "bin".to_string()
    } else {
        cleaned
    }
}

fn source_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "artifact".to_string())
        .chars()
        .take(120)
        .collect()
}

fn skip_dir(name: &str) -> bool {
    if name.starts_with('.') {
        return true;
    }
    matches!(
        name.to_ascii_lowercase().as_str(),
        "node_modules"
            | "target"
            | "dist"
            | "build"
            | "venv"
            | "__pycache__"
            | "blobs"
            | "snapshots"
    )
}

fn collect_files(
    dir: &Path,
    depth: usize,
    since: SystemTime,
    out: &mut Vec<(SystemTime, PathBuf, u64)>,
    seen: &mut usize,
) {
    if depth > MAX_SCAN_DEPTH || *seen >= MAX_SCAN_ENTRIES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if *seen >= MAX_SCAN_ENTRIES {
            return;
        }
        *seen += 1;
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().to_string();
        if ft.is_dir() {
            if !skip_dir(&name) {
                collect_files(&entry.path(), depth + 1, since, out, seen);
            }
            continue;
        }
        if !ft.is_file() || name.starts_with('.') {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.len() == 0 || meta.len() > MAX_ARTIFACT_BYTES {
            continue;
        }
        let Ok(modified) = meta.modified() else {
            continue;
        };
        if modified >= since {
            out.push((modified, entry.path(), meta.len()));
        }
    }
}

fn scan_and_copy(
    dir: &Path,
    app_data_dir: &Path,
    since: SystemTime,
) -> Result<Vec<Candidate>, String> {
    let mut files = Vec::new();
    let mut seen = 0usize;
    collect_files(dir, 0, since, &mut files, &mut seen);
    files.sort_by_key(|(mtime, _, _)| *mtime);

    let mut out = Vec::new();
    let mut total = 0u64;
    for (_, path, size) in files {
        if out.len() >= MAX_ARTIFACTS_PER_TURN || total + size > MAX_TOTAL_BYTES {
            break;
        }
        let ext = sanitize_ext(path.extension().and_then(|e| e.to_str()).unwrap_or("bin"));
        // Read + hash + content-addressed write. The file could disappear
        // between scan and read (tool cleanup); skip instead of failing the
        // whole registration.
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let rel = match crate::file_store::write_blob(app_data_dir, &bytes, &ext) {
            Ok(rel) => rel,
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "artifact blob write failed");
                continue;
            }
        };
        let Some((hash, ext)) = crate::file_store::parse_blob_path(&rel) else {
            continue;
        };
        total += size;
        out.push(Candidate {
            hash,
            ext,
            name: source_name(&path),
            size,
        });
    }
    Ok(out)
}

/// Register files written into `dir` since `since` as artifacts of the turn
/// that just produced `message_id`. Returns the newly registered rows.
pub async fn register_turn_artifacts(
    db: &SqlitePool,
    app_data_dir: &Path,
    session_id: &str,
    message_id: &str,
    dir: Option<&str>,
    since: SystemTime,
) -> Result<Vec<Artifact>, String> {
    let Some(dir) = dir.filter(|d| !d.trim().is_empty()) else {
        return Ok(Vec::new());
    };
    let dir = PathBuf::from(dir);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let scan_root = dir.clone();
    let app_dir = app_data_dir.to_path_buf();
    let since = since.checked_sub(MTIME_SLACK).unwrap_or(since);
    let candidates =
        tokio::task::spawn_blocking(move || scan_and_copy(&scan_root, &app_dir, since))
            .await
            .map_err(|e| format!("artifact scan task failed: {e}"))??;
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    for c in candidates {
        // The same content rewritten in a later turn is already registered
        // for this session; skip the duplicate index row (the blob stays).
        let exists: Option<(String,)> = sqlx::query_as(
            "SELECT id FROM session_artifacts WHERE session_id = ? AND hash = ? LIMIT 1",
        )
        .bind(session_id)
        .bind(&c.hash)
        .fetch_optional(db)
        .await
        .map_err(|e| format!("db error: {e}"))?;
        if exists.is_some() {
            continue;
        }
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO session_artifacts (id, session_id, message_id, hash, ext, name, size, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(session_id)
        .bind(message_id)
        .bind(&c.hash)
        .bind(&c.ext)
        .bind(&c.name)
        .bind(c.size as i64)
        .bind(now_iso())
        .execute(db)
        .await
        .map_err(|e| format!("db error: {e}"))?;
        // Proactively push small blobs to the account archive so the peer can
        // open them without waiting for a request round-trip.
        crate::sync::attachments::mark_blob_pending_push(db, app_data_dir, &c.hash, &c.ext).await;
        out.push(Artifact {
            hash: c.hash,
            ext: c.ext,
            name: c.name,
        });
    }
    Ok(out)
}

/// Newest artifacts of a session that are present locally, for the turn
/// prompt (`name`, `siku://...` URI, absolute path). Missing blobs are skipped:
/// the model must not be told to read a file that is not here yet.
pub async fn recent_local_paths(
    db: &SqlitePool,
    app_data_dir: &Path,
    session_id: &str,
    limit: i64,
) -> Vec<(String, String, String)> {
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT name, hash, ext FROM session_artifacts \
         WHERE session_id = ? ORDER BY created_at DESC LIMIT ?",
    )
    .bind(session_id)
    .bind(limit.clamp(1, 50))
    .fetch_all(db)
    .await
    .unwrap_or_default();
    let mut out = Vec::new();
    for (name, hash, ext) in rows {
        let path = crate::file_store::blob_path(app_data_dir, &hash, &ext);
        if path.exists() {
            out.push((
                name,
                artifact_uri(&hash, &ext),
                path.to_string_lossy().into_owned(),
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_rejects_artifact_uris() {
        let hash = "a".repeat(64);
        assert_eq!(
            parse_artifact_uri(&format!("{ARTIFACT_SCHEME}{hash}.PNG")),
            Some((hash.clone(), "png".to_string()))
        );
        assert!(parse_artifact_uri(&format!("{ARTIFACT_SCHEME}{hash}")).is_none());
        assert!(parse_artifact_uri(&format!("{ARTIFACT_SCHEME}../{hash}.png")).is_none());
        assert!(parse_artifact_uri(&format!("{ARTIFACT_SCHEME}{hash}.png/../../x")).is_none());
        assert!(parse_artifact_uri(&format!("{ARTIFACT_SCHEME}{hash}.t ool")).is_none());
    }

    #[tokio::test]
    async fn registers_only_files_written_since_turn_start() {
        let root = tempfile::tempdir().unwrap();
        let app = tempfile::tempdir().unwrap();
        let db = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query(
            "CREATE TABLE session_artifacts (
                id TEXT PRIMARY KEY NOT NULL,
                session_id TEXT NOT NULL DEFAULT '',
                message_id TEXT NOT NULL DEFAULT '',
                hash TEXT NOT NULL DEFAULT '',
                ext TEXT NOT NULL DEFAULT '',
                name TEXT NOT NULL DEFAULT '',
                size INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT ''
            )",
        )
        .execute(&db)
        .await
        .unwrap();

        let old = root.path().join("old.txt");
        std::fs::write(&old, "old").unwrap();
        let old_mtime = SystemTime::now() - Duration::from_secs(3600);
        filetime_set(&old, old_mtime);
        let new = root.path().join("report.md");
        std::fs::write(&new, "fresh report").unwrap();

        let since = SystemTime::now() - Duration::from_secs(60);
        let artifacts = register_turn_artifacts(
            &db,
            app.path(),
            "s1",
            "m1",
            Some(&root.path().to_string_lossy()),
            since,
        )
        .await
        .unwrap();
        assert_eq!(artifacts.len(), 1, "{artifacts:?}");
        assert_eq!(artifacts[0].name, "report.md");
        assert_eq!(artifacts[0].ext, "md");

        let rows: Vec<(String, String, String, i64)> =
            sqlx::query_as("SELECT session_id, hash, ext, size FROM session_artifacts")
                .fetch_all(&db)
                .await
                .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "s1");
        let uri = artifact_uri(&rows[0].1, &rows[0].2);
        let resolved = resolve_uri(app.path(), &uri).unwrap();
        assert!(resolved.ends_with(&format!(".{}", rows[0].2)));
        assert!(Path::new(&resolved).exists());

        // Re-registering the same content is a no-op (same session + hash).
        let again = register_turn_artifacts(
            &db,
            app.path(),
            "s1",
            "m2",
            Some(&root.path().to_string_lossy()),
            SystemTime::now() - Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert!(again.is_empty());
    }

    /// `std::fs` has no set-mtime; use the libc-free `filetime`-less trick:
    /// write the file via a subprocess? No — tests run on Linux/macOS/Windows
    /// with `std` only. Use `std::fs::File::set_times` (Rust 1.75+).
    fn filetime_set(path: &std::path::Path, t: SystemTime) {
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_times(std::fs::FileTimes::new().set_modified(t))
            .unwrap();
    }
}
