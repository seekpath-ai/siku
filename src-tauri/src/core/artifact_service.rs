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
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sqlx::SqlitePool;

use crate::core::time::now_iso;

pub const ARTIFACT_SCHEME: &str = "siku://artifact/";

/// Every file produced in a turn is a local artifact row. Cap the list so a
/// huge repo scan cannot insert hundreds of thousands of rows in one turn.
const MAX_FILES_PER_TURN: usize = 200;
const MAX_SCAN_ENTRIES: usize = 5_000;
const MAX_SCAN_DEPTH: usize = 4;
/// Filesystem mtime granularity (FAT: 2s) means "written this turn" needs a
/// little slack or a file written at the turn boundary could be missed.
const MTIME_SLACK: Duration = Duration::from_secs(5);

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

#[derive(Debug, Clone)]
struct LocalFile {
    name: String,
    path: String,
    ext: String,
    size: u64,
    modified_at_ms: Option<i64>,
}

#[derive(Debug, Default)]
struct ScanResult {
    files: Vec<LocalFile>,
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
            // Browser profiles / caches are tool scratch, not deliverables
            // and not useful process files.
            | "user data"
            | "cache"
            | "code cache"
            | "gpucache"
            | "crashpad"
            | "service worker"
            | "local storage"
            | "session storage"
            | "indexeddb"
            | "temp"
            | "tmp"
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
        if meta.len() == 0 {
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

fn scan_turn_files(dir: &Path, since: SystemTime) -> Result<ScanResult, String> {
    let mut files = Vec::new();
    let mut seen = 0usize;
    collect_files(dir, 0, since, &mut files, &mut seen);
    files.sort_by_key(|(mtime, _, _)| *mtime);

    let mut result = ScanResult::default();
    for (modified, path, size) in files {
        if result.files.len() >= MAX_FILES_PER_TURN {
            break;
        }
        let ext = sanitize_ext(path.extension().and_then(|e| e.to_str()).unwrap_or("bin"));
        result.files.push(LocalFile {
            name: source_name(&path),
            path: path.to_string_lossy().into_owned(),
            ext,
            size,
            modified_at_ms: modified
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|d| d.as_millis() as i64),
        });
    }
    Ok(result)
}

/// Index files written into `dir` since `since` for the turn that produced
/// `message_id`. This only writes the device-local file table: bytes remain in
/// the workspace and are copied into the blob store only when the user asks to
/// sync an artifact from the sidebar.
pub async fn register_turn_artifacts(
    db: &SqlitePool,
    _app_data_dir: &Path,
    session_id: &str,
    message_id: &str,
    dir: Option<&str>,
    since: SystemTime,
) -> Result<(), String> {
    let Some(dir) = dir.filter(|d| !d.trim().is_empty()) else {
        return Ok(());
    };
    let dir = PathBuf::from(dir);
    if !dir.is_dir() {
        return Ok(());
    }
    let scan_root = dir.clone();
    let since = since.checked_sub(MTIME_SLACK).unwrap_or(since);
    let scan = tokio::task::spawn_blocking(move || scan_turn_files(&scan_root, since))
        .await
        .map_err(|e| format!("artifact scan task failed: {e}"))??;

    for f in scan.files {
        sqlx::query(
            "INSERT INTO session_process_files \
             (id, session_id, message_id, name, path, ext, size, modified_at_ms, artifact_id, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, '', ?)",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(session_id)
        .bind(message_id)
        .bind(&f.name)
        .bind(&f.path)
        .bind(&f.ext)
        .bind(f.size as i64)
        .bind(f.modified_at_ms)
        .bind(now_iso())
        .execute(db)
        .await
        .map_err(|e| format!("db error: {e}"))?;
    }
    Ok(())
}

/// Newest local files of a session, for the next turn's context prompt.
pub async fn recent_local_paths(
    db: &SqlitePool,
    session_id: &str,
    limit: i64,
) -> Vec<(String, String)> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT name, path FROM session_process_files \
         WHERE session_id = ? ORDER BY COALESCE(modified_at_ms, 0) DESC, created_at DESC LIMIT ?",
    )
    .bind(session_id)
    .bind(limit.clamp(1, 50))
    .fetch_all(db)
    .await
    .unwrap_or_default();
    rows.into_iter()
        .filter(|(_, path)| Path::new(path).is_file())
        .collect()
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
    async fn indexes_all_turn_files_locally_without_blobs() {
        let root = tempfile::tempdir().unwrap();
        let app = tempfile::tempdir().unwrap();
        let db = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query(
            "CREATE TABLE session_process_files (
                id TEXT PRIMARY KEY NOT NULL,
                session_id TEXT NOT NULL DEFAULT '',
                message_id TEXT NOT NULL DEFAULT '',
                name TEXT NOT NULL DEFAULT '',
                path TEXT NOT NULL DEFAULT '',
                ext TEXT NOT NULL DEFAULT '',
                size INTEGER NOT NULL DEFAULT 0,
                modified_at_ms INTEGER,
                artifact_id TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL DEFAULT ''
            )",
        )
        .execute(&db)
        .await
        .unwrap();

        let old = root.path().join("old.txt");
        std::fs::write(&old, "old").unwrap();
        filetime_set(&old, SystemTime::now() - Duration::from_secs(3600));
        std::fs::write(root.path().join("report.md"), "fresh report").unwrap();
        std::fs::write(root.path().join("chart.png"), "png").unwrap();
        std::fs::write(root.path().join("settings.dat"), "data").unwrap();
        // Browser profile internals are skipped as tool scratch.
        let profile = root.path().join("chrome").join("User Data").join("Default");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(profile.join("f_00b858"), "profile").unwrap();

        register_turn_artifacts(
            &db,
            app.path(),
            "s1",
            "m1",
            Some(&root.path().to_string_lossy()),
            SystemTime::now() - Duration::from_secs(60),
        )
        .await
        .unwrap();

        let rows: Vec<(String, String, String, String)> = sqlx::query_as(
            "SELECT name, ext, path, artifact_id FROM session_process_files ORDER BY name",
        )
        .fetch_all(&db)
        .await
        .unwrap();
        assert_eq!(rows.len(), 3, "{rows:?}");
        assert_eq!(rows[0].0, "chart.png");
        assert_eq!(rows[1].0, "report.md");
        assert_eq!(rows[2].0, "settings.dat");
        assert!(rows.iter().all(|row| row.3.is_empty()), "nothing is synced by default");

        let blobs = crate::file_store::blob_dir(app.path());
        assert!(!blobs.exists(), "scanning must not create a blob store");

        let recent = recent_local_paths(&db, "s1", 10).await;
        assert_eq!(recent.len(), 3);
        assert!(recent.iter().any(|(name, _)| name == "report.md"));

        // A second turn creates its own rows; it must not overwrite the first
        // turn's message association.
        register_turn_artifacts(
            &db,
            app.path(),
            "s1",
            "m2",
            Some(&root.path().to_string_lossy()),
            SystemTime::now() - Duration::from_secs(5),
        )
        .await
        .unwrap();
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM session_process_files")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(count.0, 6);
        db.close().await;
    }

    fn filetime_set(path: &std::path::Path, t: SystemTime) {
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_times(std::fs::FileTimes::new().set_modified(t))
            .unwrap();
    }
}
