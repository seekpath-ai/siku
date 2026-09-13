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
    /// bottom-left page corner).
    pub bbox: Option<[f32; 4]>,
    pub text: String,
}

/// Bbox of one geometric line, in display-frame PDF points (y-up). Same padding
/// the paragraph box uses, so a line box never sticks out of its paragraph.
pub fn line_bbox(line: &crate::pdf::paragraphs::GeoLine) -> [f32; 4] {
    [
        line.x_start,
        line.baseline_y - line.font_size * 0.35,
        line.x_end,
        line.baseline_y + line.font_size * 1.1,
    ]
}

/// Anchor one paragraph (its lines) for the dual-pane view. `None` when the
/// paragraph has no visible text.
fn anchor_paragraph(
    page: u16,
    para: &[crate::pdf::paragraphs::GeoLine],
) -> Option<AnchoredParagraph> {
    // The paragraph's own text: its lines joined with the word-space and
    // de-hyphenation rules (see `line_segments`). Line structure is not exposed
    // — the dual-pane view compares paragraphs, and the chunker only needs the
    // paragraph breaks.
    let (text, _lengths) = crate::pdf::paragraphs::line_segments(para);
    if text.trim().is_empty() {
        return None;
    }
    let boxes: Vec<[f32; 4]> = para.iter().map(line_bbox).collect();
    let x0 = boxes.iter().map(|b| b[0]).fold(f32::MAX, f32::min);
    let x1 = boxes.iter().map(|b| b[2]).fold(0.0f32, f32::max);
    let y0 = boxes.iter().map(|b| b[1]).fold(f32::MAX, f32::min);
    let y1 = boxes.iter().map(|b| b[3]).fold(0.0f32, f32::max);
    Some(AnchoredParagraph {
        page,
        bbox: Some([x0, y0, x1, y1]),
        text,
    })
}

/// Extract anchored paragraphs for the dual-pane view. Geometry comes from
/// pdfium, with the pdf_oxide fallback running the same pipeline.
pub fn extract_paragraphs(path: &Path) -> Result<Vec<AnchoredParagraph>> {
    // pdfium is process-global and not thread-safe; see `bindings::pdfium_guard`.
    let _pdfium = crate::pdf::bindings::pdfium_guard();
    if let Ok(pdfium) = crate::pdf::bindings::pdfium() {
        let doc = pdfium
            .load_pdf_from_file(path, None)
            .map_err(|e| SikuError::PdfParse(format!("failed to load PDF: {e}")))?;
        let mut out = Vec::new();
        for (index, page) in doc.pages().iter().enumerate() {
            let Ok(text_page) = page.text() else { continue };
            let rotation = crate::pdf::paragraphs::PageRotation::from_pdfium(
                page.rotation()
                    .unwrap_or(pdfium_render::prelude::PdfPageRenderRotation::None),
            );
            let lines = crate::pdf::paragraphs::pdfium_lines(
                &text_page,
                page.width().value,
                page.height().value,
                rotation,
            );
            for group in crate::pdf::paragraphs::group_paragraphs(&lines) {
                if let Some(anchored) = anchor_paragraph((index + 1) as u16, &group) {
                    out.push(anchored);
                }
            }
        }
        if !out.is_empty() {
            return Ok(out);
        }
    }
    // No pdfium: pdf_oxide's own text, without anchors — it exposes no line
    // geometry, so the dual-pane view degrades to whole-paragraph alignment.
    Ok(extract_text_oxide(path)?
        .into_iter()
        .flat_map(|page| {
            page.text
                .split("\n\n")
                .filter(|b| !b.trim().is_empty())
                .map(|block| AnchoredParagraph {
                    page: page.page,
                    bbox: None,
                    text: block.trim().to_string(),
                })
                .collect::<Vec<_>>()
        })
        .collect())
}

/// Extract text from all pages of a PDF file.
///
/// pdfium first — its own text lines and reading order, grouped into paragraphs
/// by vertical gaps (see `paragraphs::pdfium_lines`). pdf_oxide (pure Rust, no
/// external library) is the fallback when pdfium is unavailable, and then the
/// text is used exactly as `extract_text()` returns it.
pub fn extract_text(path: &Path) -> Result<Vec<PageText>> {
    let _pdfium = crate::pdf::bindings::pdfium_guard();
    if let Ok(pdfium) = crate::pdf::bindings::pdfium() {
        if let Ok(doc) = pdfium.load_pdf_from_file(path, None) {
            let mut result = Vec::new();
            for (index, page) in doc.pages().iter().enumerate() {
                let Ok(text_page) = page.text() else { continue };
                let rotation = crate::pdf::paragraphs::PageRotation::from_pdfium(
                    page.rotation()
                        .unwrap_or(pdfium_render::prelude::PdfPageRenderRotation::None),
                );
                let lines = crate::pdf::paragraphs::pdfium_lines(
                    &text_page,
                    page.width().value,
                    page.height().value,
                    rotation,
                );
                // Paragraph breaks are the only thing added on top of pdfium's
                // text: the chunker splits on them, and `all()` has none.
                let text = crate::pdf::paragraphs::group_paragraphs(&lines)
                    .iter()
                    .map(|group| crate::pdf::paragraphs::line_segments(group).0)
                    .collect::<Vec<_>>()
                    .join("\n\n");
                let trimmed = text.trim().to_string();
                if !trimmed.is_empty() {
                    result.push(PageText {
                        page: (index + 1) as u16,
                        text: trimmed,
                    });
                }
            }
            if !result.is_empty() {
                return Ok(result);
            }
        }
    }
    extract_text_oxide(path)
}

/// pdf_oxide text extraction: its own `extract_text()`, used verbatim.
fn extract_text_oxide(path: &Path) -> Result<Vec<PageText>> {
    let bytes = std::fs::read(path)?;
    let doc = pdf_oxide::PdfDocument::from_bytes(bytes)
        .map_err(|e| SikuError::PdfParse(format!("failed to load PDF with pdf_oxide: {e}")))?;
    let page_count = doc.page_count().unwrap_or(0);
    let mut result = Vec::new();
    for index in 0..page_count {
        let text = doc.extract_text(index).unwrap_or_default();
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

/// Extract full text concatenated from all pages.
pub fn extract_full_text(path: &Path) -> Result<String> {
    let pages = extract_text(path)?;
    Ok(pages
        .into_iter()
        .map(|p| p.text)
        .collect::<Vec<_>>()
        .join("\n\n"))
}
