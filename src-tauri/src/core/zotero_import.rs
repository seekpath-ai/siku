//! Import a Zotero library by reading its data directory directly.
//!
//! Zotero keeps everything in `{dataDir}/zotero.sqlite` plus stored files
//! under `{dataDir}/storage/<itemKey>/`. The live DB is locked while Zotero
//! runs, so we copy `zotero.sqlite` (+ `-wal`/`-shm`) into a temp dir and
//! query the copy read-only. Stored-file PDF attachments go through the same
//! import pipeline as local files; the Zotero collection tree and item tags
//! are mirrored into siku collections/tags.
//!
//! Limitation: linked-file attachments (Zotero "Link to file" mode) are only
//! resolved when stored as absolute paths; entries relative to Zotero's
//! Linked Attachment Base Directory are skipped (that setting lives in
//! Zotero's prefs, not in the sqlite DB).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use tauri::AppHandle;
use uuid::Uuid;

use crate::core::batch_import::{
    emit_progress, find_paper_by_blob_hash, find_paper_by_doi, BatchGuard, BatchImportProgress,
    BatchImportSummary, ItemOutcome,
};
use crate::core::paper_service::{finalize_paper_import, ImportMetadata};
use crate::core::{collection_service, tag_service};
use crate::file_store;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ZoteroPreview {
    pub data_dir: String,
    pub items: usize,
    /// Items whose PDF attachment resolves to a file on disk.
    pub with_pdf: usize,
    pub collections: usize,
    pub tags: usize,
}

/// Default Zotero data directory (`~/Zotero` on all platforms), if it holds
/// a database.
pub fn detect_zotero_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)?;
    let dir = home.join("Zotero");
    if dir.join("zotero.sqlite").is_file() {
        Some(dir)
    } else {
        None
    }
}

/// Copy the Zotero DB (and WAL sidecars) to a private temp dir and open it
/// read-only. The copy lets us read while Zotero itself is running.
async fn open_zotero_copy(data_dir: &Path, app_data_dir: &Path) -> Result<(SqlitePool, PathBuf), String> {
    let src = data_dir.join("zotero.sqlite");
    if !src.is_file() {
        return Err(format!("未找到 Zotero 数据库：{}", src.display()));
    }
    let tmp = app_data_dir
        .join("tmp")
        .join(format!("zotero-import-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).map_err(|e| format!("创建临时目录失败：{e}"))?;
    for name in ["zotero.sqlite", "zotero.sqlite-wal", "zotero.sqlite-shm"] {
        let from = data_dir.join(name);
        if from.is_file() {
            std::fs::copy(&from, tmp.join(name)).map_err(|e| format!("复制 Zotero 数据库失败：{e}"))?;
        }
    }
    let options = SqliteConnectOptions::new()
        .filename(tmp.join("zotero.sqlite"))
        .read_only(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .map_err(|e| format!("打开 Zotero 数据库失败：{e}"))?;
    Ok((pool, tmp))
}

struct ZoteroCollection {
    id: i64,
    name: String,
    parent_id: Option<i64>,
}

struct ZoteroItem {
    id: i64,
    key: String,
    title: String,
    authors: Vec<String>,
    year: Option<i32>,
    journal: Option<String>,
    doi: Option<String>,
    url: Option<String>,
    abstract_text: Option<String>,
    isbn: Option<String>,
    pdf_path: Option<PathBuf>,
    file_name: Option<String>,
    collections: Vec<i64>,
    tags: Vec<String>,
}

/// A stored-file attachment path ("storage:Foo.pdf") resolves into the item's
/// own storage folder; absolute linked paths are used as-is.
fn resolve_attachment_path(data_dir: &Path, item_key: &str, raw: &str) -> Option<PathBuf> {
    if let Some(name) = raw.strip_prefix("storage:") {
        let p = data_dir.join("storage").join(item_key).join(name);
        return p.is_file().then_some(p);
    }
    if let Some(rel) = raw.strip_prefix("attachments:") {
        // Relative to Zotero's Linked Attachment Base Directory, which lives
        // in prefs.js — unreadable here. Bail unless it happens to exist
        // relative to the data dir.
        let p = data_dir.join(rel);
        return p.is_file().then_some(p);
    }
    let p = PathBuf::from(raw);
    if p.is_absolute() && p.is_file() {
        return Some(p);
    }
    None
}

fn extract_year(date: &str) -> Option<i32> {
    let mut digits = String::new();
    for c in date.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_digit() {
            digits.push(c);
        } else {
            if digits.len() == 4 {
                if let Ok(y) = digits.parse::<i32>() {
                    if (1900..=2100).contains(&y) {
                        return Some(y);
                    }
                }
            }
            digits.clear();
        }
    }
    None
}

async fn load_library(zdb: &SqlitePool, data_dir: &Path) -> Result<(Vec<ZoteroCollection>, Vec<ZoteroItem>), String> {
    let collections: Vec<ZoteroCollection> = sqlx::query_as::<_, (i64, String, Option<i64>)>(
        "SELECT collectionID, collectionName, parentCollectionID FROM collections ORDER BY collectionName",
    )
    .fetch_all(zdb)
    .await
    .map_err(|e| format!("读取 Zotero 分类失败：{e}"))?
    .into_iter()
    .map(|(id, name, parent_id)| ZoteroCollection { id, name, parent_id })
    .collect();

    // Regular items only — attachments/notes/annotations hang off parents.
    let items: Vec<(i64, String)> = sqlx::query_as(
        "SELECT i.itemID, i.key FROM items i JOIN itemTypes t ON i.itemTypeID = t.itemTypeID \
         WHERE t.typeName NOT IN ('attachment', 'note', 'annotation') \
         AND i.itemID NOT IN (SELECT itemID FROM deletedItems) \
         ORDER BY i.itemID",
    )
    .fetch_all(zdb)
    .await
    .map_err(|e| format!("读取 Zotero 条目失败：{e}"))?;

    let mut out = Vec::with_capacity(items.len());
    for (item_id, key) in items {
        // Fields
        let fields: Vec<(String, String)> = sqlx::query_as(
            "SELECT f.fieldName, v.value FROM itemData d \
             JOIN fields f ON d.fieldID = f.fieldID \
             JOIN itemDataValues v ON d.valueID = v.valueID WHERE d.itemID = ?",
        )
        .bind(item_id)
        .fetch_all(zdb)
        .await
        .map_err(|e| format!("读取 Zotero 字段失败：{e}"))?;
        let field = |name: &str| -> Option<String> {
            fields
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };

        // Creators (fieldMode=1 → single-field name, e.g. institutions)
        let creators: Vec<(String, String, i64)> = sqlx::query_as(
            "SELECT c.firstName, c.lastName, c.fieldMode FROM itemCreators ic \
             JOIN creators c ON ic.creatorID = c.creatorID WHERE ic.itemID = ? \
             ORDER BY ic.orderIndex",
        )
        .bind(item_id)
        .fetch_all(zdb)
        .await
        .map_err(|e| format!("读取 Zotero 作者失败：{e}"))?;
        let authors: Vec<String> = creators
            .into_iter()
            .map(|(first, last, mode)| {
                if mode == 1 {
                    last.trim().to_string()
                } else {
                    format!("{first} {last}").trim().to_string()
                }
            })
            .filter(|a| !a.is_empty())
            .collect();

        // Tags (itemTags.type: 0 = custom, 1 = automatic; automatic tags are
        // noisy keyword dumps — skip them. Older schemas lack the column, so
        // fall back to importing all tags.)
        let tags: Vec<String> = match sqlx::query_as::<_, (String, i64)>(
            "SELECT t.name, it.type FROM itemTags it JOIN tags t ON it.tagID = t.tagID WHERE it.itemID = ?",
        )
        .bind(item_id)
        .fetch_all(zdb)
        .await
        {
            Ok(rows) => rows
                .into_iter()
                .filter(|(_, ty)| *ty == 0)
                .map(|(n, _)| n)
                .collect(),
            Err(_) => sqlx::query_as::<_, (String,)>(
                "SELECT t.name FROM itemTags it JOIN tags t ON it.tagID = t.tagID WHERE it.itemID = ?",
            )
            .bind(item_id)
            .fetch_all(zdb)
            .await
            .map_err(|e| format!("读取 Zotero 标签失败：{e}"))?
            .into_iter()
            .map(|(n,)| n)
            .collect(),
        };

        // First on-disk PDF attachment
        let attachments: Vec<(String, i64, String, String)> = sqlx::query_as(
            "SELECT ia.path, ia.linkMode, child.key, ia.contentType FROM itemAttachments ia \
             JOIN items child ON ia.itemID = child.itemID \
             WHERE ia.parentItemID = ? AND ia.itemID NOT IN (SELECT itemID FROM deletedItems) \
             ORDER BY ia.itemID",
        )
        .bind(item_id)
        .fetch_all(zdb)
        .await
        .map_err(|e| format!("读取 Zotero 附件失败：{e}"))?;
        let mut pdf_path = None;
        let mut file_name = None;
        for (raw, _link_mode, child_key, content_type) in attachments {
            if !content_type.eq_ignore_ascii_case("application/pdf") {
                continue;
            }
            if let Some(p) = resolve_attachment_path(data_dir, &child_key, &raw) {
                file_name = Some(
                    raw.rsplit(['/', '\\'])
                        .next()
                        .unwrap_or("attachment.pdf")
                        .trim_start_matches("storage:")
                        .to_string(),
                );
                pdf_path = Some(p);
                break;
            }
        }

        let memberships: Vec<(i64,)> =
            sqlx::query_as("SELECT collectionID FROM collectionItems WHERE itemID = ?")
                .bind(item_id)
                .fetch_all(zdb)
                .await
                .map_err(|e| format!("读取 Zotero 分类归属失败：{e}"))?;

        let title = field("title").unwrap_or_default();
        out.push(ZoteroItem {
            id: item_id,
            key,
            title,
            authors,
            year: field("date").and_then(|d| extract_year(&d)),
            journal: field("publicationTitle")
                .or_else(|| field("bookTitle"))
                .or_else(|| field("proceedingsTitle")),
            doi: field("DOI"),
            url: field("url"),
            abstract_text: field("abstractNote"),
            isbn: field("ISBN"),
            pdf_path,
            file_name,
            collections: memberships.into_iter().map(|(c,)| c).collect(),
            tags,
        });
    }
    Ok((collections, out))
}

/// Scan a Zotero data directory and report what an import would see.
pub async fn preview(data_dir: &Path, app_data_dir: &Path) -> Result<ZoteroPreview, String> {
    let (zdb, tmp) = open_zotero_copy(data_dir, app_data_dir).await?;
    let result = load_library(&zdb, data_dir).await;
    zdb.close().await;
    let _ = std::fs::remove_dir_all(&tmp);
    let (collections, items) = result?;
    let with_pdf = items.iter().filter(|i| i.pdf_path.is_some()).count();
    let tag_count: usize = items.iter().map(|i| i.tags.len()).sum();
    Ok(ZoteroPreview {
        data_dir: data_dir.to_string_lossy().to_string(),
        items: items.len(),
        with_pdf,
        collections: collections.len(),
        tags: tag_count,
    })
}

/// Find or create a siku collection for one Zotero collection, recreating the
/// parent chain first. Matches existing collections by (name, parent).
/// Boxed because the parent-chain recursion is async.
fn map_collection<'a>(
    db: &'a SqlitePool,
    zcols: &'a HashMap<i64, &'a ZoteroCollection>,
    zid: i64,
    cache: &'a mut HashMap<i64, String>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send + 'a>> {
    Box::pin(async move {
        if let Some(id) = cache.get(&zid) {
            return Ok(id.clone());
        }
        let zc = zcols
            .get(&zid)
            .ok_or_else(|| format!("Zotero 分类 {zid} 不存在"))?;
        let parent = match zc.parent_id {
            Some(pid) if zcols.contains_key(&pid) => Some(map_collection(db, zcols, pid, cache).await?),
            _ => None,
        };
        let existing: Option<(String,)> = sqlx::query_as(
            "SELECT id FROM collections WHERE name = ? AND parent_id IS ? LIMIT 1",
        )
        .bind(&zc.name)
        .bind(parent.as_deref())
        .fetch_optional(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
        let id = match existing {
            Some((id,)) => id,
            None => collection_service::create_collection(db, &zc.name, parent.as_deref()).await?.id,
        };
        cache.insert(zid, id.clone());
        Ok(id)
    })
}

/// Find or create a tag by name (create_tag errors on duplicates, so look up
/// first).
async fn map_tag(db: &SqlitePool, name: &str, cache: &mut HashMap<String, String>) -> Result<String, String> {
    if let Some(id) = cache.get(name) {
        return Ok(id.clone());
    }
    let existing: Option<(String,)> = sqlx::query_as("SELECT id FROM tags WHERE name = ? LIMIT 1")
        .bind(name)
        .fetch_optional(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    let id = match existing {
        Some((id,)) => id,
        None => tag_service::create_tag(db, name, None).await?.id,
    };
    cache.insert(name.to_string(), id.clone());
    Ok(id)
}

/// Full import: mirror the collection tree, then import every item (metadata
/// always, PDF when resolvable), dedup by DOI and blob hash.
pub async fn import(
    app: &AppHandle,
    db: &SqlitePool,
    app_data_dir: &Path,
    data_dir: &Path,
) -> Result<BatchImportSummary, String> {
    let guard = BatchGuard::try_acquire()?;
    let (zdb, tmp) = open_zotero_copy(data_dir, app_data_dir).await?;
    let loaded = load_library(&zdb, data_dir).await;
    zdb.close().await;
    let _ = std::fs::remove_dir_all(&tmp);
    let (zcollections, items) = loaded?;

    let zcols: HashMap<i64, &ZoteroCollection> = zcollections.iter().map(|c| (c.id, c)).collect();
    let mut collection_cache: HashMap<i64, String> = HashMap::new();
    let mut tag_cache: HashMap<String, String> = HashMap::new();

    let total = items.len();
    let mut summary = BatchImportSummary {
        total,
        imported: 0,
        skipped: 0,
        failed: 0,
        cancelled: false,
        errors: Vec::new(),
    };

    for (idx, item) in items.iter().enumerate() {
        if guard.cancelled() {
            summary.cancelled = true;
            break;
        }
        let display = if item.title.is_empty() {
            item.key.clone()
        } else {
            item.title.clone()
        };
        let base = BatchImportProgress {
            batch_id: "zotero".to_string(),
            total,
            current: idx + 1,
            file: display.clone(),
            last_status: None,
            error: None,
            done: false,
            cancelled: false,
            imported: summary.imported,
            skipped: summary.skipped,
            failed: summary.failed,
        };
        emit_progress(app, &base);

        match import_one(db, app_data_dir, item, &zcols, &mut collection_cache, &mut tag_cache).await {
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
                    summary.errors.push(format!("{display}：{e}"));
                }
                emit_progress(app, &BatchImportProgress { last_status: Some("failed".into()), error: Some(e), failed: summary.failed, ..base });
            }
        }
    }

    emit_progress(
        app,
        &BatchImportProgress {
            batch_id: "zotero".to_string(),
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

async fn import_one(
    db: &SqlitePool,
    app_data_dir: &Path,
    item: &ZoteroItem,
    zcols: &HashMap<i64, &ZoteroCollection>,
    collection_cache: &mut HashMap<i64, String>,
    tag_cache: &mut HashMap<String, String>,
) -> ItemOutcome {
    // Dedup: DOI first (catches metadata-only duplicates), then blob hash.
    if let Some(doi) = &item.doi {
        match find_paper_by_doi(db, doi).await {
            Ok(Some(_)) => return ItemOutcome::Skipped,
            Ok(None) => {}
            Err(e) => return ItemOutcome::Failed(e),
        }
    }
    let mut blob_rel = None;
    let mut file_size = None;
    if let Some(pdf) = &item.pdf_path {
        let bytes = match std::fs::read(pdf) {
            Ok(b) => b,
            Err(e) => return ItemOutcome::Failed(format!("读取 PDF 失败：{e}")),
        };
        let hash = file_store::sha256_hex(&bytes);
        match find_paper_by_blob_hash(db, &hash, "pdf").await {
            Ok(Some(_)) => return ItemOutcome::Skipped,
            Ok(None) => {}
            Err(e) => return ItemOutcome::Failed(e),
        }
        blob_rel = match file_store::write_blob(app_data_dir, &bytes, "pdf") {
            Ok(rel) => Some(rel),
            Err(e) => return ItemOutcome::Failed(e.to_string()),
        };
        file_size = Some(bytes.len() as i64);
        crate::sync::attachments::mark_blob_pending_push(db, app_data_dir, &hash, "pdf").await;
    }

    let title = if item.title.is_empty() {
        item.file_name
            .clone()
            .unwrap_or_else(|| format!("Zotero 条目 {}", item.key))
    } else {
        item.title.clone()
    };
    let paper_id = Uuid::new_v4().to_string();
    let metadata = ImportMetadata {
        title,
        authors: item.authors.clone(),
        year: item.year,
        journal: item.journal.clone(),
        doi: item.doi.clone(),
        url: item.url.clone(),
        abstract_text: item.abstract_text.clone(),
        keywords: vec![],
        file_path: blob_rel,
        file_size,
        page_count: None,
        language: None,
        isbn: item.isbn.clone(),
    };
    let file_name = item.file_name.clone().unwrap_or_else(|| "attachment.pdf".to_string());
    let paper = match finalize_paper_import(db, app_data_dir, paper_id, &file_name, metadata).await {
        Ok(p) => p,
        Err(e) => return ItemOutcome::Failed(e.to_string()),
    };

    // Mirror collection membership and tags.
    for zcid in &item.collections {
        if let Ok(siku_cid) = map_collection(db, zcols, *zcid, collection_cache).await {
            let _ = collection_service::add_papers_to_collection(db, &siku_cid, &[paper.id.clone()]).await;
        }
    }
    for tag in &item.tags {
        let t = tag.trim();
        if t.is_empty() {
            continue;
        }
        if let Ok(tag_id) = map_tag(db, t, tag_cache).await {
            let _ = tag_service::add_tags_to_paper(db, &paper.id, &[tag_id]).await;
        }
    }
    ItemOutcome::Imported(paper.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_year_parses_common_date_shapes() {
        assert_eq!(extract_year("2021-03-15"), Some(2021));
        assert_eq!(extract_year("Mar 2021"), Some(2021));
        assert_eq!(extract_year("2021"), Some(2021));
        assert_eq!(extract_year("no date"), None);
        assert_eq!(extract_year("31"), None);
        // First plausible year wins.
        assert_eq!(extract_year("received 2019, published 2020"), Some(2019));
    }

    #[test]
    fn resolve_attachment_prefers_storage_then_absolute() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path();
        let stored = data.join("storage").join("ABCD1234");
        std::fs::create_dir_all(&stored).unwrap();
        std::fs::write(stored.join("paper.pdf"), b"%PDF").unwrap();

        assert_eq!(
            resolve_attachment_path(data, "ABCD1234", "storage:paper.pdf"),
            Some(stored.join("paper.pdf"))
        );
        // Missing stored file → None.
        assert_eq!(resolve_attachment_path(data, "ABCD1234", "storage:gone.pdf"), None);
        // Absolute linked path.
        let abs = stored.join("linked.pdf");
        std::fs::write(&abs, b"%PDF").unwrap();
        assert_eq!(
            resolve_attachment_path(data, "XXXX9999", &abs.to_string_lossy()),
            Some(abs.clone())
        );
        // Linked-attachment-base-relative paths are not resolvable here.
        assert_eq!(resolve_attachment_path(data, "XXXX9999", "attachments:rel/x.pdf"), None);
    }
}
