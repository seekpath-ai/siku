//! Batch import of local PDF files (multi-select / folder scan) with progress
//! events, blob-hash dedup and cancellation. The Zotero importer reuses the
//! progress channel, guard and dedup helpers from here.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use sqlx::SqlitePool;
use tauri::{AppHandle, Emitter};

use crate::core::paper_service;
use crate::file_store;

/// Progress event channel consumed by the frontend batch-import dialog.
pub const PROGRESS_EVENT: &str = "library:batch-import-progress";

/// One batch at a time: `RUNNING` guards re-entrancy, `CANCEL` requests a
/// stop between items (the in-flight item always finishes).
static RUNNING: AtomicBool = AtomicBool::new(false);
static CANCEL: AtomicBool = AtomicBool::new(false);

/// Per-item progress payload (also used as the terminal event with done=true).
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchImportProgress {
    /// "files" | "folder" | "zotero" — informational, for the UI label.
    pub batch_id: String,
    pub total: usize,
    /// 1-based index of the item currently being processed (0 = not started).
    pub current: usize,
    /// Display name of the current item.
    pub file: String,
    /// Outcome of the item that just finished: imported | skipped | failed.
    pub last_status: Option<String>,
    pub error: Option<String>,
    pub done: bool,
    pub cancelled: bool,
    pub imported: usize,
    pub skipped: usize,
    pub failed: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchImportSummary {
    pub total: usize,
    pub imported: usize,
    pub skipped: usize,
    pub failed: usize,
    pub cancelled: bool,
    /// Per-item failures as "file: reason", capped at 50 entries.
    pub errors: Vec<String>,
}

/// Ask the running batch to stop after the current item.
pub fn request_cancel() {
    CANCEL.store(true, Ordering::SeqCst);
}

/// Re-entrancy guard: marks the batch as running, resets flags on drop.
pub struct BatchGuard;
impl BatchGuard {
    pub fn try_acquire() -> Result<Self, String> {
        if RUNNING.swap(true, Ordering::SeqCst) {
            return Err("已有批量导入任务正在进行中".to_string());
        }
        CANCEL.store(false, Ordering::SeqCst);
        Ok(Self)
    }
    pub fn cancelled(&self) -> bool {
        CANCEL.load(Ordering::SeqCst)
    }
}
impl Drop for BatchGuard {
    fn drop(&mut self) {
        RUNNING.store(false, Ordering::SeqCst);
    }
}

pub fn emit_progress(app: &AppHandle, p: &BatchImportProgress) {
    if let Err(e) = app.emit(PROGRESS_EVENT, p) {
        tracing::warn!(error = %e, "failed to emit batch import progress");
    }
}

/// Recursively (or flat) collect PDF files under `dir`, sorted for
/// determinism.
pub fn scan_folder_pdfs(dir: &Path, recursive: bool) -> Result<Vec<String>, String> {
    if !dir.is_dir() {
        return Err(format!("目录不存在：{}", dir.display()));
    }
    let mut out: Vec<String> = Vec::new();
    let walker = walkdir::WalkDir::new(dir)
        .max_depth(if recursive { usize::MAX } else { 1 })
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            // Skip hidden dirs (.git etc.) — they never hold user PDFs.
            // Depth 0 is the root itself (tempdir names may start with '.');
            // never filter it out.
            e.depth() == 0
                || !(e.file_type().is_dir() && e.file_name().to_string_lossy().starts_with('.'))
        });
    for entry in walker.filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let is_pdf = entry
            .path()
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("pdf"))
            .unwrap_or(false);
        if is_pdf {
            out.push(entry.path().to_string_lossy().to_string());
        }
    }
    out.sort();
    Ok(out)
}

/// A non-deleted paper whose main file is the same blob content, if any.
pub async fn find_paper_by_blob_hash(
    db: &SqlitePool,
    hash: &str,
    ext: &str,
) -> Result<Option<String>, String> {
    let rel = format!("blobs/{hash}.{ext}");
    sqlx::query_scalar("SELECT id FROM papers WHERE file_path = ? AND deleted_at IS NULL LIMIT 1")
        .bind(rel)
        .fetch_optional(db)
        .await
        .map_err(|e| format!("db: {e}"))
}

/// A non-deleted paper with the same DOI (case-insensitive), if any.
pub async fn find_paper_by_doi(db: &SqlitePool, doi: &str) -> Result<Option<String>, String> {
    sqlx::query_scalar(
        "SELECT id FROM papers WHERE deleted_at IS NULL AND doi IS NOT NULL \
         AND TRIM(LOWER(doi)) = TRIM(LOWER(?)) LIMIT 1",
    )
    .bind(doi)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("db: {e}"))
}

pub enum ItemOutcome {
    Imported(String),
    Skipped,
    Failed(String),
}

/// Import one local PDF through the standard pipeline, skipping when the same
/// content is already in the library (blob-hash dedup).
pub async fn import_pdf_one(db: &SqlitePool, app_data_dir: &Path, path: &Path) -> ItemOutcome {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => return ItemOutcome::Failed(format!("读取失败：{e}")),
    };
    let hash = file_store::sha256_hex(&bytes);
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("pdf")
        .to_string();
    match find_paper_by_blob_hash(db, &hash, &ext).await {
        Ok(Some(id)) => return ItemOutcome::Skipped,
        Ok(None) => {}
        Err(e) => return ItemOutcome::Failed(e),
    }
    match paper_service::import_paper(db, app_data_dir, path).await {
        Ok(p) => ItemOutcome::Imported(p.id),
        Err(e) => ItemOutcome::Failed(e.to_string()),
    }
}

/// Run a batch of local PDF paths serially, emitting progress events.
pub async fn run_pdf_batch(
    app: &AppHandle,
    db: &SqlitePool,
    app_data_dir: &Path,
    paths: Vec<String>,
    batch_id: &str,
) -> Result<BatchImportSummary, String> {
    let guard = BatchGuard::try_acquire()?;
    let total = paths.len();
    let mut summary = BatchImportSummary {
        total,
        imported: 0,
        skipped: 0,
        failed: 0,
        cancelled: false,
        errors: Vec::new(),
    };

    for (idx, p) in paths.iter().enumerate() {
        if guard.cancelled() {
            summary.cancelled = true;
            break;
        }
        let path = Path::new(p);
        let file = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| p.clone());
        let base = BatchImportProgress {
            batch_id: batch_id.to_string(),
            total,
            current: idx + 1,
            file: file.clone(),
            last_status: None,
            error: None,
            done: false,
            cancelled: false,
            imported: summary.imported,
            skipped: summary.skipped,
            failed: summary.failed,
        };
        emit_progress(app, &base);

        match import_pdf_one(db, app_data_dir, path).await {
            ItemOutcome::Imported(_) => {
                summary.imported += 1;
                emit_progress(app, &BatchImportProgress { last_status: Some("imported".into()), imported: summary.imported, ..base });
            }
            ItemOutcome::Skipped => {
                summary.skipped += 1;
                emit_progress(app, &BatchImportProgress { last_status: Some("skipped".into()), skipped: summary.skipped, ..base });
            }
            ItemOutcome::Failed(e) => {
                summary.failed += 1;
                if summary.errors.len() < 50 {
                    summary.errors.push(format!("{file}：{e}"));
                }
                emit_progress(app, &BatchImportProgress { last_status: Some("failed".into()), error: Some(e), failed: summary.failed, ..base });
            }
        }
    }

    emit_progress(
        app,
        &BatchImportProgress {
            batch_id: batch_id.to_string(),
            total,
            current: total,
            file: String::new(),
            last_status: None,
            error: None,
            done: true,
            cancelled: summary.cancelled,
            imported: summary.imported,
            skipped: summary.skipped,
            failed: summary.failed,
        },
    );
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_folder_collects_pdfs_recursively_and_skips_hidden_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.pdf"), b"%PDF-1.4").unwrap();
        std::fs::write(root.join("B.PDF"), b"%PDF-1.7").unwrap();
        std::fs::write(root.join("note.txt"), b"nope").unwrap();
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("c.pdf"), b"%PDF-1.5").unwrap();
        let hidden = root.join(".git");
        std::fs::create_dir_all(&hidden).unwrap();
        std::fs::write(hidden.join("d.pdf"), b"%PDF-1.6").unwrap();

        let all = scan_folder_pdfs(root, true).unwrap();
        assert_eq!(all.len(), 3);
        assert!(all.iter().any(|p| p.ends_with("a.pdf")));
        assert!(all.iter().any(|p| p.ends_with("B.PDF")));
        assert!(all.iter().any(|p| p.ends_with("c.pdf")));

        // Flat scan: only top-level PDFs.
        let flat = scan_folder_pdfs(root, false).unwrap();
        assert_eq!(flat.len(), 2);

        assert!(scan_folder_pdfs(&root.join("missing"), true).is_err());
    }
}
