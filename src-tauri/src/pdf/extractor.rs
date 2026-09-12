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

/// Extract anchored paragraphs for the dual-pane view. Geometry comes from
/// pdfium, with the pdf_oxide fallback running the same pipeline.
pub fn extract_paragraphs(path: &Path) -> Result<Vec<AnchoredParagraph>> {
    // pdfium is process-global and not thread-safe; see `bindings::pdfium_guard`.
    let _pdfium = crate::pdf::bindings::pdfium_guard();
    if let Ok(pdfium) = crate::pdf::bindings::pdfium() {
        let doc = pdfium
            .load_pdf_from_file(path, None)
            .map_err(|e| SikuError::PdfParse(format!("failed to load PDF: {e}")))?;
        let pages = doc.pages();
        let mut out = Vec::new();
        for (index, page) in pages.iter().enumerate() {
            let Ok(text_page) = page.text() else { continue };
            let rotation = crate::pdf::paragraphs::PageRotation::from_pdfium(
                page.rotation().unwrap_or(pdfium_render::prelude::PdfPageRenderRotation::None),
            );
            let (lines, gutter) = crate::pdf::paragraphs::page_to_lines(
                &text_page,
                page.width().value,
                page.height().value,
                rotation,
            );
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
    // Fallback: pdf_oxide (pure Rust) — its char data carries real space
    // glyphs, glyph widths and font weights, so the full geometry pipeline
    // (columns, paragraphs, bbox anchors) applies even without pdfium.
    let pages = extract_pages_oxide(path)?;
    let mut out = Vec::new();
    for (page_no, width, height, lines, gutter) in pages {
        for para in crate::pdf::paragraphs::paragraphize(lines, width, height, gutter) {
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
                page: page_no,
                bbox: Some([x0, y0, x1, y1]),
                text,
            });
        }
    }
    Ok(out)
}

/// pdf_oxide fallback: per-page geometric lines + gutter, or empty pages.
/// Pure Rust, no dynamic library — replaces the old lopdf fallback, which
/// extracted nothing from CID/Type1C-font PDFs (Elsevier etc.) and caused
/// false "scanned PDF" reports.
#[allow(clippy::type_complexity)]
fn extract_pages_oxide(
    path: &Path,
) -> Result<Vec<(u16, f32, f32, Vec<crate::pdf::paragraphs::GeoLine>, Option<f32>)>> {
    use crate::pdf::paragraphs::{PageRotation, RawChar};
    let bytes = std::fs::read(path)?;
    let mut doc = pdf_oxide::PdfDocument::from_bytes(bytes)
        .map_err(|e| SikuError::PdfParse(format!("failed to load PDF with pdf_oxide: {e}")))?;
    let page_count = doc.page_count().unwrap_or(0);
    let mut pages = Vec::new();
    for idx in 0..page_count {
        let chars = doc.extract_chars(idx).unwrap_or_default();
        let (x0, y0, x1, y1) = doc
            .get_page_media_box(idx)
            .unwrap_or((0.0, 0.0, 612.0, 792.0));
        // pdf_oxide also reports char boxes in the CONTENT frame (verified on
        // demo0 p11: media box 595x794 while /Rotate is 90), so the same
        // display-frame mapping applies; only then do the page dimensions swap.
        //
        // Known limitation: pdf_oxide's `TextChar` exposes only an ink box, no
        // pen origin, so on a rotated page the mapped boxes can tile without
        // gaps and inter-word spaces are lost ("wordswouldglue"). The pdfium
        // path above is unaffected (it has true char origins). Still a strict
        // improvement over the previous behaviour, which returned one-char
        // lines on such pages.
        let rotation = PageRotation::from_degrees(doc.get_page_rotation(idx).unwrap_or(0));
        let (content_w, content_h) = (x1 - x0, y1 - y0);
        let (display_w, display_h) = if rotation.swaps_axes() {
            (content_h, content_w)
        } else {
            (content_w, content_h)
        };
        let raw: Vec<RawChar> = chars
            .iter()
            .filter_map(|ch| {
                // Same glyph normalisation as the pdfium path (keeps the
                // line-final hyphen marker as a real '-').
                let ch_char = crate::pdf::paragraphs::normalize_glyph(ch.char)?;
                let (mut x, y) = rotation.map_point(ch.bbox.x, ch.bbox.y, display_w, display_h);
                let mut right = ch.bbox.x + ch.bbox.width;
                if rotation.needs_frame_remap() {
                    let mapped = rotation.map_rect(
                        [
                            ch.bbox.x,
                            ch.bbox.y,
                            ch.bbox.x + ch.bbox.width,
                            ch.bbox.y + ch.bbox.height,
                        ],
                        display_w,
                        display_h,
                    );
                    x = mapped[0];
                    right = mapped[2];
                }
                Some(RawChar {
                    x,
                    y,
                    right,
                    font: ch.font_size,
                    ch: ch_char,
                    bold: (ch.font_weight as u16) >= 600,
                })
            })
            .collect();
        let (lines, gutter) = crate::pdf::paragraphs::chars_to_lines(raw, display_w);
        pages.push(((idx + 1) as u16, display_w, display_h, lines, gutter));
    }
    Ok(pages)
}


/// Extract text from all pages of a PDF file.
///
/// Tries pdfium-render first (needs the platform `pdfium.dll`), and falls
/// back to pdf_oxide (pure Rust, no external library) when pdfium is
/// unavailable or yields no text. This keeps chunking / re-indexing working
/// out of the box.
pub fn extract_text(path: &Path) -> Result<Vec<PageText>> {
    match extract_text_pdfium(path) {
        Ok(pages) if !pages.is_empty() => Ok(pages),
        Ok(_) => extract_text_oxide(path),
        Err(e) => {
            tracing::warn!("pdfium extraction failed ({e}), falling back to pdf_oxide");
            extract_text_oxide(path)
        }
    }
}

/// pdfium-based extraction (requires the dynamic pdfium library).
fn extract_text_pdfium(path: &Path) -> Result<Vec<PageText>> {
    // pdfium is process-global and not thread-safe; see `bindings::pdfium_guard`.
    let _pdfium = crate::pdf::bindings::pdfium_guard();
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
        let rotation = crate::pdf::paragraphs::PageRotation::from_pdfium(
            page.rotation().unwrap_or(pdfium_render::prelude::PdfPageRenderRotation::None),
        );
        let (lines, gutter) = crate::pdf::paragraphs::page_to_lines(
            &text_page,
            page.width().value,
            page.height().value,
            rotation,
        );
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

/// pdf_oxide text extraction for the fallback path: runs the same geometry
/// pipeline as the pdfium path.
fn extract_text_oxide(path: &Path) -> Result<Vec<PageText>> {
    let mut result = Vec::new();
    for (page_no, width, height, lines, gutter) in extract_pages_oxide(path)? {
        let text = crate::pdf::paragraphs::lines_to_text(lines, width, height, gutter);
        let trimmed = text.trim().to_string();
        if !trimmed.is_empty() {
            result.push(PageText {
                page: page_no,
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
