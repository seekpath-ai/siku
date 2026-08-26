use std::path::Path;

use crate::core::error::{Result, SikuError};

#[derive(Debug, Clone)]
pub struct PdfMetadata {
    pub title: Option<String>,
    pub authors: Vec<String>,
    pub subject: Option<String>,
    pub keywords: Vec<String>,
    pub page_count: usize,
}

/// Extract metadata from a PDF file using lopdf.
pub fn extract_metadata(path: &Path) -> Result<PdfMetadata> {
    let doc = lopdf::Document::load(path).map_err(|e| {
        SikuError::PdfParse(format!("failed to open PDF '{}': {}", path.display(), e))
    })?;

    let page_count = doc.get_pages().len();

    let title = get_info_entry(&doc, b"Title");
    let authors_raw = get_info_entry(&doc, b"Author");
    let subject = get_info_entry(&doc, b"Subject");
    let keywords_raw = get_info_entry(&doc, b"Keywords");

    let authors = parse_authors(&authors_raw.unwrap_or_default());
    let keywords = parse_keywords(&keywords_raw.unwrap_or_default());

    Ok(PdfMetadata {
        title,
        authors,
        subject,
        keywords,
        page_count,
    })
}

/// Get a string value from the PDF /Info dictionary for a given key.
fn get_info_entry(doc: &lopdf::Document, key: &[u8]) -> Option<String> {
    // Resolve the /Info dictionary from the trailer
    let info_ref = doc.trailer.get(b"Info").ok()?;
    let info_dict = match info_ref {
        lopdf::Object::Reference(id) => {
            doc.get_object(*id).ok()?.as_dict().ok()?
        }
        lopdf::Object::Dictionary(dict) => dict,
        _ => return None,
    };

    let value = info_dict.get(key).ok()?;
    match value {
        lopdf::Object::String(s, _) => decode_pdf_text_string(s),
        _ => None,
    }
}

/// Decode a PDF text string (Info dictionary values). The spec allows only
/// PDFDocEncoding or UTF-16BE (0xFEFF BOM), but real-world files also carry
/// plain UTF-8, BOM-less UTF-16, and — from Chinese producers — GBK bytes.
/// Decoding these as UTF-8-lossy produced the "tofu" replacement boxes.
fn decode_pdf_text_string(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() {
        return Some(String::new());
    }
    // UTF-16 with BOM (spec mandates BE; tolerate LE).
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return decode_utf16(&bytes[2..], false);
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return decode_utf16(&bytes[2..], true);
    }
    // Plain UTF-8 (modern producers, and pure ASCII which is a subset).
    // Embedded NULs mean this was never UTF-8 — likely BOM-less UTF-16.
    if let Ok(s) = std::str::from_utf8(bytes) {
        if !s.contains('\0') {
            return Some(s.to_string());
        }
    }
    // BOM-less UTF-16BE heuristic: even length and many NUL high bytes
    // (Latin-script text). CJK without a BOM is left to the GBK path below.
    if bytes.len() % 2 == 0 && !bytes.is_empty() {
        let nul_high = bytes.iter().step_by(2).filter(|&&b| b == 0).count();
        if nul_high * 3 >= bytes.len() / 2 && nul_high > 0 {
            if let Some(s) = decode_utf16(bytes, false) {
                return Some(s);
            }
        }
    }
    // Non-spec GBK (common in Chinese-produced PDFs): accept only when it
    // decodes cleanly and actually yields CJK text.
    let (gbk, _, had_errors) = encoding_rs::GBK.decode(bytes);
    if !had_errors && gbk.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)) {
        return Some(gbk.into_owned());
    }
    // PDFDocEncoding, approximated as Latin-1 — the differences live in the
    // 0x80–0x9F control range and rarely matter for titles/authors.
    Some(bytes.iter().map(|&b| b as char).collect())
}

fn decode_utf16(bytes: &[u8], little_endian: bool) -> Option<String> {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| {
            if little_endian {
                u16::from_le_bytes([c[0], c[1]])
            } else {
                u16::from_be_bytes([c[0], c[1]])
            }
        })
        .collect();
    String::from_utf16(&units).ok()
}

/// Best-effort DOI extraction: PDF Info dictionary, XMP metadata, then
/// first-page text. Used to look up full bibliographic metadata.
pub fn extract_doi(path: &Path) -> Option<String> {
    let doc = lopdf::Document::load(path).ok()?;
    // 1. Info dict keys.
    for key in [&b"doi"[..], &b"DOI"[..], &b"prism:doi"[..], &b"xmp:doi"[..], &b"dc:identifier"[..]] {
        if let Some(v) = get_info_entry(&doc, key) {
            if let Some(doi) = find_doi(&v) {
                return Some(doi);
            }
        }
    }
    // 2. XMP metadata stream.
    if let Some(xmp) = extract_xmp(&doc) {
        if let Some(doi) = find_doi(&xmp) {
            return Some(doi);
        }
    }
    // 3. First-page text (many PDFs print the DOI in the header/footer).
    if let Ok(pages) = crate::pdf::extractor::extract_text(path) {
        if let Some(first) = pages.first() {
            if let Some(doi) = find_doi(&first.text) {
                return Some(doi);
            }
        }
    }
    None
}

/// Read the XMP metadata stream as text.
fn extract_xmp(doc: &lopdf::Document) -> Option<String> {
    let root = doc.trailer.get(b"Root").ok()?;
    let root_id = match root {
        lopdf::Object::Reference(id) => *id,
        _ => return None,
    };
    let root_dict = doc.get_object(root_id).ok()?.as_dict().ok()?;
    let meta = root_dict.get(b"Metadata").ok()?;
    let meta_id = match meta {
        lopdf::Object::Reference(id) => *id,
        _ => return None,
    };
    let obj = doc.get_object(meta_id).ok()?;
    let stream = obj.as_stream().ok()?;
    let content = stream.decompressed_content().ok()?;
    String::from_utf8(content).ok()
}

/// Scan text for a plausible DOI (prefix `10.xxxx/...`).
fn find_doi(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    // Prefer explicit markers: "doi:", "doi=", "doi.org/".
    for marker in ["doi:", "doi=", "doi.org/"] {
        if let Some(pos) = lower.find(marker) {
            let after = &lower[pos + marker.len()..];
            if let Some(s) = after.find("10.") {
                let start = pos + marker.len() + s;
                if let Some(doi) = take_doi(&text[start..]) {
                    return Some(doi);
                }
            }
        }
    }
    // Bare "10.xxxx/yyyy" anywhere.
    if let Some(pos) = text.find("10.") {
        return take_doi(&text[pos..]);
    }
    None
}

/// Consume a DOI-like run from `rest`, trimming trailing punctuation.
fn take_doi(rest: &str) -> Option<String> {
    let doi: String = rest
        .chars()
        .take_while(|c| !c.is_whitespace() && !matches!(c, '"' | '<' | '>' | ',' | ']' | '}'))
        .collect();
    let doi = doi.trim_end_matches(['.', ';', ')', '(', '?', '!']).to_string();
    if doi.len() > 6 && doi.starts_with("10.") && doi.contains('/') {
        Some(doi)
    } else {
        None
    }
}

/// Parse author strings separated by semicolons or commas.
fn parse_authors(raw: &str) -> Vec<String> {
    parse_separated(raw)
}

/// Parse keyword strings separated by semicolons or commas.
fn parse_keywords(raw: &str) -> Vec<String> {
    parse_separated(raw)
}

/// Split a string by semicolons or commas, trimming each part.
fn parse_separated(raw: &str) -> Vec<String> {
    raw.split(&[';', ','] as &[char])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_authors_semicolon() {
        let authors = parse_authors("Alice; Bob; Charlie");
        assert_eq!(authors, vec!["Alice", "Bob", "Charlie"]);
    }

    #[test]
    fn test_parse_authors_comma() {
        let authors = parse_authors("Alice, Bob, Charlie");
        assert_eq!(authors, vec!["Alice", "Bob", "Charlie"]);
    }

    #[test]
    fn test_parse_authors_empty() {
        let authors = parse_authors("");
        assert!(authors.is_empty());
    }

    #[test]
    fn test_parse_keywords() {
        let keywords = parse_keywords("rust, pdf, metadata");
        assert_eq!(keywords, vec!["rust", "pdf", "metadata"]);
    }

    #[test]
    fn test_extract_metadata_invalid_file() {
        let result = extract_metadata(Path::new("/nonexistent/file.pdf"));
        assert!(result.is_err());
    }

    #[test]
    fn test_decode_utf16be_with_bom() {
        // "标题" (U+6807 U+9898) in UTF-16BE with BOM — how Chinese titles
        // are usually stored.
        let bytes = [0xFE, 0xFF, 0x68, 0x07, 0x98, 0x98];
        assert_eq!(decode_pdf_text_string(&bytes).as_deref(), Some("标题"));
    }

    #[test]
    fn test_decode_utf16le_with_bom() {
        let bytes = [0xFF, 0xFE, 0x07, 0x68, 0x98, 0x98];
        assert_eq!(decode_pdf_text_string(&bytes).as_deref(), Some("标题"));
    }

    #[test]
    fn test_decode_plain_utf8_and_ascii() {
        assert_eq!(
            decode_pdf_text_string("A study of cafés".as_bytes()).as_deref(),
            Some("A study of cafés")
        );
        assert_eq!(
            decode_pdf_text_string(b"Plain ASCII title").as_deref(),
            Some("Plain ASCII title")
        );
    }

    #[test]
    fn test_decode_bomless_utf16be_latin() {
        // "AB" as BOM-less UTF-16BE.
        let bytes = [0x00, 0x41, 0x00, 0x42];
        assert_eq!(decode_pdf_text_string(&bytes).as_deref(), Some("AB"));
    }

    #[test]
    fn test_decode_gbk_chinese() {
        // "中文" in GBK (non-spec, emitted by some Chinese producers).
        let bytes = [0xD6, 0xD0, 0xCE, 0xC4];
        assert_eq!(decode_pdf_text_string(&bytes).as_deref(), Some("中文"));
    }

    #[test]
    fn test_decode_pdfdocencoding_latin1_fallback() {
        // "café" with a Latin-1 é (0xE9): not valid UTF-8, not valid GBK.
        let bytes = [0x63, 0x61, 0x66, 0xE9];
        assert_eq!(decode_pdf_text_string(&bytes).as_deref(), Some("café"));
    }
}
