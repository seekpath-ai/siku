use std::path::{Path, PathBuf};

use sqlx::SqlitePool;
use tracing::instrument;

use crate::core::models::FileItem;
use crate::core::time;

/// List all managed files of a vault.
#[instrument(skip(db))]
pub async fn list_files(db: &SqlitePool, vault_id: &str) -> Result<Vec<FileItem>, String> {
    sqlx::query_as::<_, FileItem>(
        "SELECT * FROM files WHERE vault_id = ? ORDER BY sort_order ASC, name COLLATE NOCASE ASC",
    )
    .bind(vault_id)
    .fetch_all(db)
    .await
    .map_err(|e| format!("db: {e}"))
}

/// Import a local file into the vault: copy it into the content-addressed
/// blob store (dedup by sha256) and insert a `files` row. The display name is
/// the source file's name; the blob name is content-derived, so importing the
/// same name again never collides on disk.
#[instrument(skip(db))]
pub async fn import_file(
    db: &SqlitePool,
    app_data_dir: &Path,
    vault_id: &str,
    parent_id: Option<&str>,
    source_path: &str,
) -> Result<FileItem, String> {
    let source = Path::new(source_path);
    if !source.is_file() {
        return Err(format!("not a file: {source_path}"));
    }
    let name = source
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or_else(|| format!("invalid file name: {source_path}"))?;
    let size = source
        .metadata()
        .map_err(|e| format!("metadata: {e}"))?
        .len() as i64;

    let blob_path = crate::file_store::copy_file_to_blob(app_data_dir, source)
        .map_err(|e| format!("copy to blob store: {e}"))?;

    let id = uuid::Uuid::new_v4().to_string();
    let now = time::now_iso();
    let mime_type = crate::core::file_service::mime_guess(&name);

    sqlx::query(
        "INSERT INTO files (id, vault_id, parent_id, name, blob_path, size, mime_type, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(vault_id)
    .bind(parent_id)
    .bind(&name)
    .bind(&blob_path)
    .bind(size)
    .bind(&mime_type)
    .bind(&now)
    .bind(&now)
    .execute(db)
    .await
    .map_err(|e| format!("db: {e}"))?;

    get_file(db, &id).await
}

/// Get a managed file by ID.
pub async fn get_file(db: &SqlitePool, id: &str) -> Result<FileItem, String> {
    sqlx::query_as::<_, FileItem>("SELECT * FROM files WHERE id = ?")
        .bind(id)
        .fetch_optional(db)
        .await
        .map_err(|e| format!("db: {e}"))?
        .ok_or_else(|| format!("file not found: {id}"))
}

/// Move a file: update parent_id and/or sort_order.
/// `parent_id = None` clears the parent (moves the file to the root level).
#[instrument(skip(db))]
pub async fn move_file(
    db: &SqlitePool,
    id: &str,
    parent_id: Option<&str>,
    sort_order: Option<i32>,
) -> Result<FileItem, String> {
    let now = time::now_iso();
    sqlx::query("UPDATE files SET parent_id = ?, updated_at = ? WHERE id = ?")
        .bind(parent_id)
        .bind(&now)
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    if let Some(so) = sort_order {
        sqlx::query("UPDATE files SET sort_order = ?, updated_at = ? WHERE id = ?")
            .bind(so)
            .bind(&now)
            .bind(id)
            .execute(db)
            .await
            .map_err(|e| format!("db: {e}"))?;
    }
    get_file(db, id).await
}

/// Rename a file: only the display name changes; the blob stays untouched.
#[instrument(skip(db))]
pub async fn rename_file(db: &SqlitePool, id: &str, name: &str) -> Result<FileItem, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("文件名不能为空".to_string());
    }
    let now = time::now_iso();
    sqlx::query("UPDATE files SET name = ?, mime_type = ?, updated_at = ? WHERE id = ?")
        .bind(name)
        .bind(crate::core::file_service::mime_guess(name))
        .bind(&now)
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    get_file(db, id).await
}

/// Delete a managed file record. The blob is intentionally left in place: it
/// is content-addressed and may be shared with papers/attachments. Blob GC is
/// a separate concern shared with the papers store.
#[instrument(skip(db))]
pub async fn delete_file(db: &SqlitePool, id: &str) -> Result<(), String> {
    sqlx::query("DELETE FROM files WHERE id = ?")
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    Ok(())
}

/// Resolve a managed file's blob to an absolute path on disk.
pub async fn resolve_file_path(
    db: &SqlitePool,
    app_data_dir: &Path,
    id: &str,
) -> Result<PathBuf, String> {
    let file = get_file(db, id).await?;
    let path = crate::file_store::resolve_blob_path(app_data_dir, &file.blob_path);
    if !path.exists() {
        return Err(format!("file blob missing on disk: {}", path.display()));
    }
    Ok(path)
}

/// Preview a managed file as text. Whether a file is previewable is decided
/// by CONTENT, not extension: a NUL byte in the first 8 KB marks it binary
/// (git-style detection), so misnamed binaries are rejected and unknown
/// text formats just work. Text is decoded lossily (GBK and friends degrade
/// to replacement chars instead of failing) and capped at 2 MB.
///
/// OOXML office documents (.docx/.xlsx/.pptx) are binary zips, so they are
/// dispatched by name to text extraction BEFORE the NUL sniff.
#[instrument(skip(db))]
pub async fn read_file_text(
    db: &SqlitePool,
    app_data_dir: &Path,
    id: &str,
) -> Result<crate::core::models::TextPreview, String> {
    use std::io::Read;

    let file = get_file(db, id).await?;
    let path = crate::file_store::resolve_blob_path(app_data_dir, &file.blob_path);
    if !path.exists() {
        return Err(format!("file blob missing on disk: {}", path.display()));
    }

    // Office documents: zip+XML, extract text instead of sniffing.
    if crate::core::office_text::is_office_name(&file.name) {
        let bytes = std::fs::read(&path).map_err(|e| format!("read failed: {e}"))?;
        let (content, truncated) = crate::core::office_text::extract_text(&bytes, &file.name)?;
        return Ok(crate::core::models::TextPreview { content, truncated });
    }

    let file = std::fs::File::open(&path).map_err(|e| format!("read failed: {e}"))?;
    const MAX: u64 = 2 * 1024 * 1024;
    let mut buf = Vec::new();
    file.take(MAX + 1)
        .read_to_end(&mut buf)
        .map_err(|e| format!("read failed: {e}"))?;
    let truncated = buf.len() as u64 > MAX;
    if truncated {
        buf.truncate(MAX as usize);
    }
    let probe = &buf[..buf.len().min(8192)];
    if probe.contains(&0) {
        return Err("binary file".to_string());
    }
    Ok(crate::core::models::TextPreview {
        content: String::from_utf8_lossy(&buf).into_owned(),
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Text preview decodes plain text and rejects binary files by content
    /// (NUL-byte sniffing), regardless of extension.
    #[tokio::test]
    async fn read_file_text_sniffs_binary() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!("siku-filetext-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir)?;
        let db = crate::core::db::tests::connect_with_crsqlite(&dir.join("t.db")).await?;
        sqlx::query(crate::core::db::SCHEMA_INIT_SQL).execute(&db).await?;

        let txt = dir.join("a.conf");
        std::fs::write(&txt, "key=value\n")?;
        let bin = dir.join("b.txt"); // binary content behind a text extension
        std::fs::write(&bin, [0u8, 1, 2, 3])?;

        let f1 = import_file(&db, &dir, "vault-x", None, &txt.to_string_lossy())
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        let preview = read_file_text(&db, &dir, &f1.id)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        assert_eq!(preview.content.trim(), "key=value");
        assert!(!preview.truncated);

        let f2 = import_file(&db, &dir, "vault-x", None, &bin.to_string_lossy())
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        assert!(read_file_text(&db, &dir, &f2.id).await.is_err());

        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}
