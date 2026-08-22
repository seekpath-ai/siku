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
