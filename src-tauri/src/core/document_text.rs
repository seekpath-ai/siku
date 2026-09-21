//! Unified document → text reader for chat file attachments.
//!
//! Dispatches by extension/magic instead of an extension whitelist:
//! PDF goes through the pdfium/pdf_oxide pipeline, OOXML office files through
//! `office_text`, and everything else is treated as text with encoding
//! fallback (UTF-8 → UTF-16 BOM → GBK) so custom-extension and non-UTF-8
//! text files just work.
//!
//! Size limits guard parsing/memory cost (per type). Extracted text is NOT
//! char-capped on extraction — but what gets injected into a chat message is:
//! over-budget text is written whole to a cache file and only the head plus a
//! continuation pointer is returned, so the agent can page the rest with
//! `file_read` (the paper_read pattern applied to ad-hoc attachments).

use std::path::{Path, PathBuf};

/// Per-file size limits: parsing cost / memory, not context budget.
/// Text and office files: 5 MB. PDFs: 20 MB (common papers run a few MB to
/// 10+ MB; 5 MB would reject normal literature).
pub const MAX_TEXT_OFFICE_BYTES: u64 = 5 * 1024 * 1024;
pub const MAX_PDF_BYTES: u64 = 20 * 1024 * 1024;

/// Max chars of one document injected into a single chat message. Larger
/// extractions are cached to disk and paged via file_read.
pub const INJECT_BUDGET_CHARS: usize = 30_000;

/// Read a document file as text for chat context injection.
///
/// `cache_dir` is where oversized extractions land; it must be a directory
/// the agent's file tools can read (the session's working dir when one is
/// set — the sandbox rejects absolute paths outside it).
pub fn read_document_text(path: &Path, cache_dir: &Path) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("read failed: {e}"))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let lower = name.to_ascii_lowercase();
    let is_pdf = lower.ends_with(".pdf");
    let is_office = crate::core::office_text::is_office_name(&lower);

    let limit = if is_pdf { MAX_PDF_BYTES } else { MAX_TEXT_OFFICE_BYTES };
    if meta.len() > limit {
        return Err(format!(
            "文件过大（{}MB，上限 {}MB）",
            meta.len() / (1024 * 1024),
            limit / (1024 * 1024)
        ));
    }

    let text = if is_pdf {
        extract_pdf_text(path)?
    } else if is_office {
        let bytes = std::fs::read(path).map_err(|e| format!("read failed: {e}"))?;
        let (text, _truncated) = crate::core::office_text::extract_text(&bytes, &name)?;
        text
    } else {
        decode_text_file(path)?
    };

    let total = text.chars().count();
    if total <= INJECT_BUDGET_CHARS {
        return Ok(text);
    }

    // Over budget: cache the FULL text and return the head plus a pointer the
    // agent can act on. Never silently drop the tail (the note_read lesson).
    let cache_path = write_cache(cache_dir, &name, &text)?;
    let head: String = text.chars().take(INJECT_BUDGET_CHARS).collect();
    Ok(format!(
        "{head}\n\n[...全文共 {total} 字符，超出单条消息注入上限（{INJECT_BUDGET_CHARS}）；完整文本已缓存至 {}，可用 file_read 以 line_offset/n_lines 分页续读。若为长篇文献，更建议导入图书馆后用 paper_read 按分块阅读]",
        cache_path.display()
    ))
}

/// pdfium → pdf_oxide extraction, pages joined with visible page markers so
/// the agent can cite locations.
fn extract_pdf_text(path: &Path) -> Result<String, String> {
    let pages = crate::pdf::extractor::extract_text(path).map_err(|e| format!("PDF 解析失败: {e}"))?;
    if pages.is_empty() {
        return Err("PDF 无文本层（可能为扫描版）".to_string());
    }
    Ok(pages
        .iter()
        .map(|p| format!("【第 {} 页】\n{}", p.page, p.text))
        .collect::<Vec<_>>()
        .join("\n\n"))
}

/// Decode a file as text: UTF-8, then UTF-16 BOM, then GBK. NUL bytes in the
/// head mark it binary. No extension check — the extension whitelist was the
/// bug this replaces.
fn decode_text_file(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read failed: {e}"))?;
    // BOM check comes first: UTF-16 text is full of NUL bytes and must not
    // trip the binary sniff below.
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let (decoded, _, _) = encoding_rs::UTF_16LE.decode(&bytes[2..]);
        return Ok(decoded.into_owned());
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let (decoded, _, _) = encoding_rs::UTF_16BE.decode(&bytes[2..]);
        return Ok(decoded.into_owned());
    }
    let head = &bytes[..bytes.len().min(8192)];
    if head.contains(&0) {
        return Err("二进制文件，无法提取文本".to_string());
    }
    if let Ok(s) = String::from_utf8(bytes.clone()) {
        return Ok(s.strip_prefix('\u{feff}').unwrap_or(&s).to_string());
    }
    // GBK covers legacy zh-CN Windows text. It maps every byte sequence,
    // so it must come last: binary garbage would decode "successfully".
    let (decoded, _, _) = encoding_rs::GBK.decode(&bytes);
    Ok(decoded.into_owned())
}

/// Write the full extracted text next to the agent's reachable area. Name is
/// content-addressed enough to avoid collisions without a new dependency.
fn write_cache(cache_dir: &Path, source_name: &str, text: &str) -> Result<PathBuf, String> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    source_name.hash(&mut h);
    text.len().hash(&mut h);
    text.chars().take(4096).collect::<String>().hash(&mut h);
    let stem = Path::new(source_name)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "document".into());
    let dir = cache_dir.join(".siku-cache");
    std::fs::create_dir_all(&dir).map_err(|e| format!("cache mkdir: {e}"))?;
    let path = dir.join(format!("{}-{:x}.txt", stem, h.finish()));
    std::fs::write(&path, text).map_err(|e| format!("cache write: {e}"))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_tmp(bytes: &[u8], name: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(bytes).unwrap();
        (dir, path)
    }

    #[test]
    fn utf8_text_reads_verbatim() {
        let (dir, path) = write_tmp("你好，世界".as_bytes(), "note.weirdext");
        let out = read_document_text(&path, dir.path()).unwrap();
        assert_eq!(out, "你好，世界");
    }

    #[test]
    fn gbk_text_decodes() {
        let (bytes, _, _) = encoding_rs::GBK.encode("中文内容");
        let (dir, path) = write_tmp(&bytes, "legacy.txt");
        let out = read_document_text(&path, dir.path()).unwrap();
        assert_eq!(out, "中文内容");
    }

    #[test]
    fn utf16le_bom_decodes() {
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend("hello 中文".encode_utf16().flat_map(|u| u.to_le_bytes()));
        let (dir, path) = write_tmp(&bytes, "log.txt");
        let out = read_document_text(&path, dir.path()).unwrap();
        assert_eq!(out, "hello 中文");
    }

    #[test]
    fn binary_is_rejected() {
        let (dir, path) = write_tmp(&[0x50, 0x4B, 0x00, 0x00, 0xFF], "blob.dat");
        let err = read_document_text(&path, dir.path()).unwrap_err();
        assert!(err.contains("二进制"), "{err}");
    }

    #[test]
    fn over_budget_text_is_cached_with_continuation_pointer() {
        let big = "长".repeat(INJECT_BUDGET_CHARS + 1000);
        let (dir, path) = write_tmp(big.as_bytes(), "big.md");
        let cache_dir = dir.path().join("wd");
        let out = read_document_text(&path, &cache_dir).unwrap();
        assert!(out.contains("file_read"), "marker must point at file_read: {}", &out[out.len() - 300..]);
        assert!(out.contains("分页续读"));
        // The cache file holds the FULL text; the message only the head.
        let marker_at = out.find("[...全文共").unwrap();
        let cached_path = out[marker_at..]
            .split("缓存至 ")
            .nth(1)
            .and_then(|s| s.split('，').next())
            .expect("marker carries cache path");
        let cached = std::fs::read_to_string(cached_path).unwrap();
        assert_eq!(cached.chars().count(), big.chars().count());
        assert!(cached_path.starts_with(&cache_dir.join(".siku-cache").to_string_lossy().to_string()));
    }

    #[test]
    fn oversized_file_is_rejected_by_size() {
        // 文本/Office 上限 5MB — 用稀疏内容超过即可。
        let big = vec![b'a'; (MAX_TEXT_OFFICE_BYTES + 1) as usize];
        let (dir, path) = write_tmp(&big, "huge.txt");
        let err = read_document_text(&path, dir.path()).unwrap_err();
        assert!(err.contains("文件过大"), "{err}");
    }
}
