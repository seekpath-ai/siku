use std::path::{Path, PathBuf};

use crate::core::error::Result;

/// Returns the root managed-papers directory: {app_data_dir}/papers/
pub fn papers_dir(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("papers")
}

/// Returns the directory for a specific paper: {app_data_dir}/papers/{paper_id}/
pub fn paper_dir(app_data_dir: &Path, paper_id: &str) -> PathBuf {
    papers_dir(app_data_dir).join(paper_id)
}

/// Creates the paper directory if it does not exist.
pub fn ensure_paper_dir(app_data_dir: &Path, paper_id: &str) -> Result<PathBuf> {
    let dir = paper_dir(app_data_dir, paper_id);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Returns the path to the thumbnail for a paper.
pub fn thumbnail_path(app_data_dir: &Path, paper_id: &str) -> PathBuf {
    paper_dir(app_data_dir, paper_id).join("thumbnail.png")
}

/// Returns the temporary download path for a paper PDF.
/// Kept for link-import streaming downloads before copying to blob storage.
pub fn original_pdf_path(app_data_dir: &Path, paper_id: &str) -> PathBuf {
    paper_dir(app_data_dir, paper_id).join("original.pdf")
}

/// Copy a source file to the managed paper store as the original PDF.
pub fn copy_pdf_to_store(app_data_dir: &Path, paper_id: &str, source: &Path) -> Result<PathBuf> {
    let dest = original_pdf_path(app_data_dir, paper_id);
    std::fs::create_dir_all(paper_dir(app_data_dir, paper_id))?;
    std::fs::copy(source, &dest)?;
    Ok(dest)
}

/// Returns the content-addressed blob directory: {app_data_dir}/blobs/
pub fn blob_dir(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("blobs")
}

/// Returns the path for a blob given its hash and extension.
pub fn blob_path(app_data_dir: &Path, hash: &str, ext: &str) -> PathBuf {
    blob_dir(app_data_dir).join(format!("{hash}.{ext}"))
}

/// Resolve a blob relative path (e.g. "blobs/abc.pdf") to an absolute path.
pub fn resolve_blob_path(app_data_dir: &Path, rel_path: &str) -> PathBuf {
    if let Some(stripped) = rel_path.strip_prefix("blobs/") {
        blob_dir(app_data_dir).join(stripped)
    } else {
        app_data_dir.join(rel_path)
    }
}

/// Compute SHA-256 hex digest of bytes.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

/// Write bytes to the blob store using content addressing.
/// Returns the relative path "blobs/{sha256}.{ext}".
pub fn write_blob(app_data_dir: &Path, bytes: &[u8], ext: &str) -> Result<String> {
    let hash = sha256_hex(bytes);
    let dest = blob_path(app_data_dir, &hash, ext);
    std::fs::create_dir_all(blob_dir(app_data_dir))?;
    if !dest.exists() {
        std::fs::write(&dest, bytes)?;
    }
    Ok(format!("blobs/{hash}.{ext}"))
}

/// Copy a file into the blob store using content addressing.
/// Returns the relative path "blobs/{sha256}.{ext}".
pub fn copy_file_to_blob(app_data_dir: &Path, source: &Path) -> Result<String> {
    let bytes = std::fs::read(source)?;
    let ext = source.extension().and_then(|e| e.to_str()).unwrap_or("bin");
    write_blob(app_data_dir, &bytes, ext)
}

/// Removes the entire paper directory from managed storage.
/// Does nothing if the directory does not exist.
pub fn remove_paper_store(app_data_dir: &Path, paper_id: &str) -> Result<()> {
    let dir = paper_dir(app_data_dir, paper_id);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    Ok(())
}

/// List all blob hashes currently in the blob store.
/// Hashes are the filename without extension, e.g. "abc123" from "abc123.pdf".
pub fn list_blob_hashes(app_data_dir: &Path) -> Result<std::collections::HashSet<String>> {
    let mut hashes = std::collections::HashSet::new();
    let dir = blob_dir(app_data_dir);
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

/// Check whether a blob with the given hash exists (any extension).
pub fn has_blob(app_data_dir: &Path, hash: &str) -> bool {
    let dir = blob_dir(app_data_dir);
    if !dir.exists() {
        return false;
    }
    std::fs::read_dir(dir)
        .ok()
        .and_then(|mut rd| {
            rd.find(|e| {
                e.as_ref()
                    .ok()
                    .map(|e| {
                        e.file_name()
                            .to_string_lossy()
                            .starts_with(&format!("{}.", hash))
                    })
                    .unwrap_or(false)
            })
        })
        .is_some()
}

/// Parse a blob relative path into (hash, ext).
pub fn parse_blob_path(rel_path: &str) -> Option<(String, String)> {
    let stripped = rel_path.strip_prefix("blobs/")?;
    let (hash, ext) = stripped.rsplit_once('.')?;
    Some((hash.to_string(), ext.to_string()))
}

/// Minimal percent-decoder for image destinations written by tools (hast and
/// markdown editors may percent-encode Windows path separators and spaces).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = |b: u8| (b as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Rewrite image refs that point at absolute paths inside the app's
/// `snapshots/` directory (what `paper_snapshot` returns and analysis notes
/// embed) into content-addressed `blobs/<sha256>.<ext>` refs, copying the
/// file into the blob store. Blob refs render identically (the blob dir IS
/// the attachments base) and — the point — travel with note sync; absolute
/// paths die on any other machine. Missing files and non-snapshot refs
/// (remote URLs, attachments/, blobs/, other absolute paths) pass through
/// unchanged.
///
/// Returns the rewritten content plus the `(hash, ext)` of every blob newly
/// referenced, so callers can register them for proactive sync push.
pub fn ingest_snapshot_image_refs(content: &str, app_data_dir: &Path) -> (String, Vec<(String, String)>) {
    let re = regex::Regex::new(r"!\[[^\]]*\]\(\s*<?([^<>\)\s]+)").unwrap();
    let snapshots = app_data_dir.join("snapshots");
    let snap_prefix = format!(
        "{}/",
        snapshots.to_string_lossy().replace('\\', "/").trim_end_matches('/')
    )
    .to_lowercase();

    let mut written: Vec<(String, String)> = Vec::new();
    let mut out = String::with_capacity(content.len());
    let mut last = 0;
    for caps in re.captures_iter(content) {
        let m = caps.get(1).unwrap();
        let dest = m.as_str();
        let decoded = percent_decode(dest).replace('\\', "/");
        // Only rewrite paths that live under OUR snapshots dir — other
        // absolute paths are user references we must not swallow.
        if !decoded.to_lowercase().starts_with(&snap_prefix) {
            continue;
        }
        let source = Path::new(&decoded);
        if !source.exists() {
            continue;
        }
        let Ok(blob_ref) = copy_file_to_blob(app_data_dir, source) else {
            continue;
        };
        if let Some((hash, ext)) = parse_blob_path(&blob_ref) {
            if !written.iter().any(|(h, _)| *h == hash) {
                written.push((hash, ext));
            }
        }
        out.push_str(&content[last..m.start()]);
        out.push_str(&blob_ref);
        last = m.end();
    }
    if last == 0 {
        return (content.to_string(), written);
    }
    out.push_str(&content[last..]);
    (out, written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_refs_become_blob_refs() {
        let dir = std::env::temp_dir().join(format!("siku-ingest-{}", std::process::id()));
        let snap = dir.join("snapshots");
        std::fs::create_dir_all(&snap).unwrap();
        let png = snap.join("fig1.png");
        std::fs::write(&png, b"fake-png-bytes").unwrap();

        let abs = png.to_string_lossy().replace('\\', "/");
        let content = format!("前文 ![]({abs}) 后文 ![](blobs/{0}aabb.png) 和 ![](https://x.com/a.png)", "cc".repeat(32));
        let (out, written) = ingest_snapshot_image_refs(&content, &dir);

        assert!(!out.contains("snapshots/"), "absolute path must be rewritten: {out}");
        assert!(out.contains("blobs/"), "blob ref expected: {out}");
        // Existing blob refs and remote URLs untouched.
        assert!(out.contains("https://x.com/a.png"));
        // Exactly one new blob reported, and the file exists with the same bytes.
        assert_eq!(written.len(), 1);
        let (hash, ext) = &written[0];
        assert_eq!(ext, "png");
        assert_eq!(std::fs::read(dir.join("blobs").join(format!("{hash}.png"))).unwrap(), b"fake-png-bytes");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_snapshot_file_is_left_alone() {
        let dir = std::env::temp_dir().join(format!("siku-ingest-miss-{}", std::process::id()));
        let abs = dir.join("snapshots").join("gone.png").to_string_lossy().replace('\\', "/");
        let content = format!("![]({abs})");
        assert_eq!(ingest_snapshot_image_refs(&content, &dir).0, content);
    }

    #[test]
    fn non_snapshot_absolute_path_is_left_alone() {
        let dir = std::env::temp_dir().join(format!("siku-ingest-other-{}", std::process::id()));
        let content = "![](/home/user/pictures/cat.png)";
        assert_eq!(ingest_snapshot_image_refs(content, &dir).0, content);
    }
}
