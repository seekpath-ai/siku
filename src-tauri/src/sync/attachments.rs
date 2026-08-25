use crate::file_store;
use anyhow::{Context, Result};
use base64::Engine;
use sqlx::SqlitePool;
use tracing::info;

#[allow(dead_code)]
/// List all blob hashes currently in the blob store.
/// Hashes are the filename without extension, e.g. "abc123" from "abc123.pdf".
pub fn list_blob_hashes(
    app_data_dir: &std::path::Path,
) -> Result<std::collections::HashSet<String>> {
    let mut hashes = std::collections::HashSet::new();
    let dir = file_store::blob_dir(app_data_dir);
    if !dir.exists() {
        return Ok(hashes);
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if let Some(hash) = name_str.split_once('.').map(|(h, _)| h.to_string()) {
            hashes.insert(hash);
        }
    }
    Ok(hashes)
}

/// Extract `blobs/<hash>.<ext>` references from a text (note Markdown content).
/// The paths produced by `file_store::write_blob` are `blobs/` + a 64-char hex
/// sha256 + optional `.ext`; anything shorter is not a blob reference.
fn extract_blob_refs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
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
        // Optional `.ext` (1-8 alphanumeric chars).
        let mut end = hash_start + hash_len;
        if bytes.get(end) == Some(&b'.') {
            let ext_len = bytes[end + 1..]
                .iter()
                .take_while(|b| b.is_ascii_alphanumeric())
                .take(8)
                .count();
            if ext_len >= 1 {
                end = end + 1 + ext_len;
            }
        }
        out.push(String::from_utf8_lossy(&bytes[hash_start - 6..end]).to_string());
        i = end;
    }
    out
}

/// Collect blob hashes referenced by synced papers/attachments/files **and
/// note Markdown content** that are not present locally. Notes embed
/// pasted/dropped images as `![...](blobs/<sha256>.png)`; without scanning
/// note content the image files would never be requested from the peer and
/// show as broken images on synced devices. Same for the vault `files` table:
/// its rows sync via CRR, but without scanning `blob_path` the actual
/// PDF/image/text content would never arrive.
pub async fn collect_missing_blob_hashes(
    db: &SqlitePool,
    app_data_dir: &std::path::Path,
) -> Result<Vec<(String, String)>> {
    let rows: Vec<(String,)> = sqlx::query_as::<_, (String,)>(
        "SELECT file_path FROM papers WHERE file_path LIKE 'blobs/%' \
         UNION \
         SELECT file_path FROM attachments WHERE file_path LIKE 'blobs/%' \
         UNION \
         SELECT blob_path FROM files WHERE blob_path LIKE 'blobs/%'",
    )
    .fetch_all(db)
    .await
    .context("collect blob paths")?;

    // Note content references: only fetch notes that mention `blobs/` so the
    // scan stays cheap on large note collections.
    let note_rows: Vec<(String,)> =
        sqlx::query_as::<_, (String,)>("SELECT content FROM notes WHERE content LIKE '%blobs/%'")
            .fetch_all(db)
            .await
            .context("collect note blob references")?;

    let mut missing = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut consider = |rel_path: &str| {
        if let Some((hash, ext)) = file_store::parse_blob_path(rel_path) {
            if seen.insert(hash.clone()) && !file_store::has_blob(app_data_dir, &hash) {
                missing.push((hash, ext));
            }
        }
    };
    for (rel_path,) in rows {
        consider(&rel_path);
    }
    for (content,) in note_rows {
        for rel in extract_blob_refs(&content) {
            consider(&rel);
        }
    }
    Ok(missing)
}

/// Maximum blob size served through the mailbox. A mailbox message is one
/// relay frame holding the entire base64 payload (~1.33x wire size, ~2.7x
/// memory after encryption) and the relay has no per-message size guard, so
/// oversized blobs are only served over a live P2P DataChannel (which chunks
/// them via `MAX_WIRE_MSG`) instead.
pub const MAX_MAILBOX_BLOB_BYTES: u64 = 20 * 1024 * 1024;

/// Whether the blob is small enough to be served through the mailbox.
pub fn blob_fits_mailbox(app_data_dir: &std::path::Path, hash: &str, ext: &str) -> bool {
    std::fs::metadata(file_store::blob_path(app_data_dir, hash, ext))
        .map(|m| m.len() <= MAX_MAILBOX_BLOB_BYTES)
        .unwrap_or(false)
}

/// Read a blob and encode it as base64 for transport.
pub fn read_blob_base64(
    app_data_dir: &std::path::Path,
    hash: &str,
    ext: &str,
) -> Result<Option<String>> {
    let path = file_store::blob_path(app_data_dir, hash, ext);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path).with_context(|| format!("read blob {}", path.display()))?;
    Ok(Some(
        base64::engine::general_purpose::STANDARD.encode(bytes),
    ))
}

/// Decode base64 data and write it into the blob store.
pub fn write_blob_from_base64(
    app_data_dir: &std::path::Path,
    hash: &str,
    ext: &str,
    data: &str,
) -> Result<()> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .context("decode blob base64")?;
    let computed = file_store::sha256_hex(&bytes);
    if computed != hash {
        anyhow::bail!("blob hash mismatch: expected {}, got {}", hash, computed);
    }
    let dest = file_store::blob_path(app_data_dir, hash, ext);
    std::fs::create_dir_all(file_store::blob_dir(app_data_dir))?;
    std::fs::write(&dest, bytes).with_context(|| format!("write blob {}", dest.display()))?;
    info!(hash = %hash, ext = %ext, "wrote received blob");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "siku-attachments-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn blob_ref(hash: &str, ext: &str) -> String {
        format!("blobs/{hash}.{ext}")
    }

    #[test]
    fn extract_blob_refs_finds_markdown_paths() {
        let h1 = "a".repeat(64);
        let h2 = "b".repeat(64);
        let h3 = "c".repeat(64);
        let text = format!(
            "开头 ![Pasted image]({}) 中间 ![{alt}]({}) 结尾引用 {}，\n\
             短路径 blobs/short.png 不算，非 blobs 的 ./img.png 不算",
            blob_ref(&h1, "png"),
            blob_ref(&h2, "jpg"),
            blob_ref(&h3, "webp"),
            alt = "alt"
        );
        let refs = extract_blob_refs(&text);
        assert!(refs.contains(&blob_ref(&h1, "png")), "png ref: {refs:?}");
        assert!(refs.contains(&blob_ref(&h2, "jpg")), "jpg ref: {refs:?}");
        assert!(refs.contains(&blob_ref(&h3, "webp")), "webp ref: {refs:?}");
        assert_eq!(refs.len(), 3, "only full sha256 blobs/: paths: {refs:?}");
    }

    #[tokio::test]
    async fn collect_missing_finds_note_image_blobs() {
        let dir = temp_dir("note-img");
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("CREATE TABLE notes (id TEXT PRIMARY KEY, content TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE papers (id TEXT PRIMARY KEY, file_path TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE attachments (id TEXT PRIMARY KEY, file_path TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE files (id TEXT PRIMARY KEY, blob_path TEXT)")
            .execute(&pool)
            .await
            .unwrap();

        let h1 = "a".repeat(64);
        let h2 = "b".repeat(64);
        let missing_ref = blob_ref(&h1, "png");
        let present_ref = blob_ref(&h2, "png");
        // h2 already exists locally, h1 does not.
        std::fs::create_dir_all(file_store::blob_dir(&dir)).unwrap();
        std::fs::write(file_store::blob_path(&dir, &h2, "png"), b"present").unwrap();

        sqlx::query("INSERT INTO notes (id, content) VALUES ('n1', ?)")
            .bind(format!("![a]({missing_ref}) 正文 ![b]({present_ref})"))
            .execute(&pool)
            .await
            .unwrap();

        let missing = collect_missing_blob_hashes(&pool, &dir).await.unwrap();
        assert!(
            missing.contains(&(h1.clone(), "png".to_string())),
            "note-referenced blob must be reported missing: {missing:?}"
        );
        assert!(
            !missing.iter().any(|(h, _)| *h == h2),
            "present blob must not be requested: {missing:?}"
        );
    }

    #[tokio::test]
    async fn collect_missing_finds_vault_file_blobs() {
        let dir = temp_dir("vault-file");
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("CREATE TABLE notes (id TEXT PRIMARY KEY, content TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE papers (id TEXT PRIMARY KEY, file_path TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE attachments (id TEXT PRIMARY KEY, file_path TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE files (id TEXT PRIMARY KEY, blob_path TEXT)")
            .execute(&pool)
            .await
            .unwrap();

        let h1 = "d".repeat(64);
        let h2 = "e".repeat(64);
        // h2 already exists locally, h1 does not.
        std::fs::create_dir_all(file_store::blob_dir(&dir)).unwrap();
        std::fs::write(file_store::blob_path(&dir, &h2, "pdf"), b"present").unwrap();

        sqlx::query("INSERT INTO files (id, blob_path) VALUES ('f1', ?), ('f2', ?), ('f3', '')")
            .bind(blob_ref(&h1, "pdf"))
            .bind(blob_ref(&h2, "pdf"))
            .execute(&pool)
            .await
            .unwrap();

        let missing = collect_missing_blob_hashes(&pool, &dir).await.unwrap();
        assert!(
            missing.contains(&(h1.clone(), "pdf".to_string())),
            "files.blob_path must be reported missing: {missing:?}"
        );
        assert!(
            !missing.iter().any(|(h, _)| *h == h2),
            "present blob must not be requested: {missing:?}"
        );
    }
}
