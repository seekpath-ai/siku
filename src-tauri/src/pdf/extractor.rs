use std::path::Path;

use crate::core::error::{Result, SikuError};

/// Extracted text from a single page of a PDF.
#[derive(Debug, Clone)]
pub struct PageText {
    pub page: u16,
    pub text: String,
}

/// One paragraph anchored to its page region — the dual-pane (PDF ↔
/// Markdown) view renders these and hit-tests clicks against `bbox`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AnchoredParagraph {
    pub page: u16,
    /// [x0, y0(bottom), x1, y1(top)] in PDF points (y grows upward from the
    /// bottom-left page corner). None when geometry was unavailable (lopdf
    /// fallback) — the anchor then degrades to page level.
    pub bbox: Option<[f32; 4]>,
    pub text: String,
}

/// Extract anchored paragraphs for the dual-pane view. Geometry comes from
/// pdfium; without it the lopdf fallback yields page-level paragraphs
/// (bbox = None).
pub fn extract_paragraphs(path: &Path) -> Result<Vec<AnchoredParagraph>> {
    if let Ok(pdfium) = crate::pdf::bindings::pdfium() {
        let doc = pdfium
            .load_pdf_from_file(path, None)
            .map_err(|e| SikuError::PdfParse(format!("failed to load PDF: {e}")))?;
        let pages = doc.pages();
        let mut out = Vec::new();
        for (index, page) in pages.iter().enumerate() {
            let Ok(text_page) = page.text() else { continue };
            let (lines, gutter) =
                crate::pdf::paragraphs::page_to_lines(&text_page, page.width().value);
            for para in crate::pdf::paragraphs::paragraphize(
                lines,
                page.width().value,
                page.height().value,
                gutter,
            ) {
                let text = para
                    .iter()
                    .map(|l| l.text.as_str())
                    .fold(String::new(), |acc, l| crate::pdf::paragraphs::join_lines_pub(&acc, l));
                if text.trim().is_empty() {
                    continue;
                }
                let x0 = para.iter().map(|l| l.x_start).fold(f32::MAX, f32::min);
                let x1 = para.iter().map(|l| l.x_end).fold(0.0f32, f32::max);
                let y0 = para
                    .iter()
                    .map(|l| l.baseline_y - l.font_size * 0.35)
                    .fold(f32::MAX, f32::min);
                let y1 = para
                    .iter()
                    .map(|l| l.baseline_y + l.font_size * 1.1)
                    .fold(0.0f32, f32::max);
                out.push(AnchoredParagraph {
                    page: (index + 1) as u16,
                    bbox: Some([x0, y0, x1, y1]),
                    text,
                });
            }
        }
        if !out.is_empty() {
            return Ok(out);
        }
    }
    // Fallback: lopdf text without geometry.
    let pages = extract_text_lopdf(path)?;
    let mut out = Vec::new();
    for p in pages {
        for para in p.text.split("\n\n") {
            let t = para.trim();
            if !t.is_empty() {
                out.push(AnchoredParagraph {
                    page: p.page,
                    bbox: None,
                    text: t.to_string(),
                });
            }
        }
    }
    Ok(out)
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
        let text_page = match page.text() {
            Ok(t) => t,
            Err(_) => continue,
        };

        // Geometry-first: rebuild paragraphs (columns, indents, gaps) so the
        // chunker's paragraph split sees real boundaries. Fall back to the
        // flat dump when geometry yields nothing (unusual encodings).
        let (lines, gutter) =
            crate::pdf::paragraphs::page_to_lines(&text_page, page.width().value);
        let text = if lines.is_empty() {
            text_page.all()
        } else {
            crate::pdf::paragraphs::lines_to_text(
                lines,
                page.width().value,
                page.height().value,
                gutter,
            )
        };

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
