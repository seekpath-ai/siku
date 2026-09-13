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
    /// One entry per extracted line, in reading order. Defaulted so caches
    /// written before line anchors existed still deserialize.
    #[serde(default)]
    pub lines: Vec<AnchoredLine>,
}

/// One line of a paragraph. Only the box and the length of the line's slice of
/// `AnchoredParagraph::text` travel over IPC: the pane renders the paragraph as
/// a single text node and slices by `len` (UTF-16 code units) to address a
/// line, which keeps a 700-page book from becoming tens of thousands of DOM
/// nodes.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AnchoredLine {
    /// [x0, y0(bottom), x1, y1(top)] in PDF points, y-up.
    pub bbox: [f32; 4],
    /// Length of this line's slice of the paragraph text, in UTF-16 code units.
    pub len: usize,
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
    let (text, lengths) = crate::pdf::paragraphs::line_segments(para);
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
        lines: boxes
            .into_iter()
            .zip(lengths)
            .map(|(bbox, len)| AnchoredLine { bbox, len })
            .collect(),
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
                if let Some(anchored) = anchor_paragraph((index + 1) as u16, &para) {
                    out.push(anchored);
                }
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
            if let Some(anchored) = anchor_paragraph(page_no, &para) {
                out.push(anchored);
            }
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
