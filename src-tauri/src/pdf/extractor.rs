use std::path::Path;

use crate::core::error::{Result, SikuError};

/// Extracted text from a single page of a PDF.
#[derive(Debug, Clone)]
pub struct PageText {
    pub page: u16,
    pub text: String,
}

/// Extract text from all pages of a PDF file.
///
/// Tries pdfium-render first (needs the platform `pdfium.dll`), and falls
/// back to lopdf (pure Rust, no external DLL) when pdfium is unavailable or
/// yields no text. This keeps chunking / re-indexing working out of the box.
pub fn extract_text(path: &Path) -> Result<Vec<PageText>> {
    match extract_text_pdfium(path) {
        Ok(pages) if !pages.is_empty() => Ok(pages),
        Ok(_) => extract_text_lopdf(path),
        Err(e) => {
            tracing::warn!("pdfium extraction failed ({e}), falling back to lopdf");
            extract_text_lopdf(path)
        }
    }
}

/// pdfium-based extraction (requires the dynamic pdfium library).
fn extract_text_pdfium(path: &Path) -> Result<Vec<PageText>> {
    let pdfium = crate::pdf::bindings::pdfium()
        .map_err(SikuError::PdfParse)?;

    let doc = pdfium
        .load_pdf_from_file(path, None)
        .map_err(|e| SikuError::PdfParse(format!("failed to load PDF: {e}")))?;

    let pages = doc.pages();
    let mut result = Vec::with_capacity(pages.len() as usize);

    for (index, page) in pages.iter().enumerate() {
        let text = page
            .text()
            .map(|t| t.all())
            .unwrap_or_default();

        // Only include non-empty pages
        let trimmed = text.trim().to_string();
        if !trimmed.is_empty() {
            result.push(PageText {
                page: (index + 1) as u16,
                text: trimmed,
            });
        }
    }

    Ok(result)
}

/// lopdf-based extraction (pure Rust, works without any external DLL).
fn extract_text_lopdf(path: &Path) -> Result<Vec<PageText>> {
    let doc = lopdf::Document::load(path)
        .map_err(|e| SikuError::PdfParse(format!("failed to load PDF with lopdf: {e}")))?;

    let mut result = Vec::new();
    for (page_no, _page_id) in doc.get_pages() {
        let text = doc.extract_text(&[page_no]).unwrap_or_default();
        let trimmed = text.trim().to_string();
        if !trimmed.is_empty() {
            result.push(PageText {
                page: page_no as u16,
                text: trimmed,
            });
        }
    }
    // get_pages() iterates in page order; keep a stable sort for safety.
    result.sort_by_key(|p| p.page);
    Ok(result)
}

/// Extract full text concatenated from all pages.
pub fn extract_full_text(path: &Path) -> Result<String> {
    let pages = extract_text(path)?;
    Ok(pages
        .into_iter()
        .map(|p| p.text)
        .collect::<Vec<_>>()
        .join("\n\n"))
}
