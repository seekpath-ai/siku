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
        lopdf::Object::String(s, _) => {
            String::from_utf8(s.clone()).ok()
                .or_else(|| Some(String::from_utf8_lossy(s).into_owned()))
        }
        _ => None,
    }
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
}
