//! Figure/table metadata extraction at indexing time, for the agent's
//! `paper_snapshot` tool.
//!
//! Two sources per page:
//!
//! 1. pdfium page objects of type `Image`, which give real placement bboxes.
//!    Vector figures and tables draw no Image object — they are covered by
//!    their captions alone.
//! 2. Caption paragraphs from the anchored-paragraph pipeline, matched by
//!    `^(图|表)\s*\d+` / `^(Figure|Fig\.?|Table)\s*\d+`.
//!
//! A caption adopts the union bbox of the image objects on its conventional
//! side (figure captions sit below their figure, table captions above their
//! table), guarded by horizontal overlap and a distance cap so a caption never
//! claims an image in the other column. Captions with no matching image are
//! still stored (bbox = NULL — vector figures and tables land here); orphan
//! images are stored with an empty label/caption.
//!
//! All rects are `[x0, y0(bottom), x1, y1(top)]` in display-frame PDF points
//! (y grows upward), the same frame as `AnchoredParagraph::bbox`.

use std::path::Path;
use std::sync::OnceLock;

use crate::core::error::{Result, SikuError};
use crate::pdf::paragraphs::PageRotation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FigureKind {
    Figure,
    Table,
}

impl FigureKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Figure => "figure",
            Self::Table => "table",
        }
    }

    pub fn from_str(s: &str) -> Self {
        if s == "table" {
            Self::Table
        } else {
            Self::Figure
        }
    }
}

/// One figure/table metadata row destined for the `paper_figures` table.
#[derive(Debug, Clone)]
pub struct FigureRecord {
    pub page: u16,
    pub kind: FigureKind,
    /// Display label as printed, e.g. "图3", "Fig. 2", "Table 1" ("" for
    /// orphan images with no caption).
    pub label: String,
    pub caption: String,
    /// Figure/table bbox (union of the matched image objects). None when only
    /// the caption was found.
    pub bbox: Option<[f32; 4]>,
    /// The caption paragraph's own bbox — needed by `paper_snapshot` to infer
    /// a crop region when `bbox` is None.
    pub caption_bbox: Option<[f32; 4]>,
}

/// Detect a caption paragraph and return (kind, label). The label keeps the
/// paper's own spelling ("Fig. 3" vs "Figure 3") for display.
///
/// Prose mentions at a paragraph start ("Table 3 shows the results…") are
/// rejected for English labels: real captions follow the label with
/// punctuation, an uppercase word, or a CJK character — never a lowercase
/// verb (verified on demo1: "Table 3 shows…" / "Table 4 partitions…" vs the
/// real "Table 3: Overall benchmark results…").
pub fn caption_label(text: &str) -> Option<(FigureKind, String)> {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(
            r"^\s*(?:(图|表)\s*(\d+(?:[.\-]\d+)*)|(Figure|Fig\.?|Table)\s*(\d+(?:[.\-]\d+)*))",
        )
        .expect("caption regex")
    });
    let caps = re.captures(text)?;
    if let (Some(kw), Some(num)) = (caps.get(1), caps.get(2)) {
        let kind = if kw.as_str() == "表" {
            FigureKind::Table
        } else {
            FigureKind::Figure
        };
        return Some((kind, format!("{}{}", kw.as_str(), num.as_str())));
    }
    let (kw, num) = (caps.get(3)?, caps.get(4)?);
    // English prose-mention guard: inspect what follows the label.
    let rest = text[caps.get(0)?.end()..].trim_start();
    if let Some(c) = rest.chars().next() {
        let ok = matches!(c, ':' | '.' | ',' | ';' | '(' | '（' | '：')
            || c.is_ascii_uppercase()
            || c.is_ascii_digit()
            || !c.is_ascii();
        if !ok {
            return None;
        }
    }
    let kw = kw.as_str();
    let kind = if kw.to_ascii_lowercase().starts_with("tab") {
        FigureKind::Table
    } else {
        FigureKind::Figure
    };
    // Normalise "Figure 3" / "Fig 3" / "Fig. 3" to the dotted short form;
    // "Table" has no abbreviation.
    let head = if kw.to_ascii_lowercase().starts_with("fig") {
        "Fig."
    } else {
        "Table"
    };
    Some((kind, format!("{} {}", head, num.as_str())))
}

/// Aggressive label normalisation for lookup: "Fig. 3", "figure 3" and
/// "图 3" must all match the indexed label.
pub fn normalize_label(s: &str) -> String {
    let mut out: String = s
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect();
    if let Some(rest) = out.strip_prefix("figure") {
        out = format!("fig{rest}");
    }
    out
}

/// Side tolerance (points): a caption baseline box may overlap its figure by
/// a few points (padding, ascenders) and still be "on the correct side".
const SIDE_TOL: f32 = 10.0;
/// Max vertical gap (points) between an image and the caption claiming it —
/// beyond this they belong to different layout blocks.
const MAX_ASSOC_DIST: f32 = 260.0;

fn x_overlap(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    a[2].min(b[2]) - a[0].max(b[0])
}

fn union(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]
}

/// Match one page's image bboxes to its captions. Returns the page's records.
fn associate(
    page: u16,
    images: &[[f32; 4]],
    captions: &[(FigureKind, String, String, [f32; 4])],
) -> Vec<FigureRecord> {
    let mut records = Vec::new();
    let mut claimed = vec![false; images.len()];

    for (kind, label, text, cbbox) in captions {
        let mut matched: Option<[f32; 4]> = None;
        for (i, img) in images.iter().enumerate() {
            // Figure captions sit BELOW the figure (smaller y); table captions
            // sit ABOVE the table.
            let dist = match kind {
                FigureKind::Figure => {
                    if cbbox[3] > img[1] + SIDE_TOL {
                        continue;
                    }
                    (img[1] - cbbox[3]).max(0.0)
                }
                FigureKind::Table => {
                    if cbbox[1] < img[3] - SIDE_TOL {
                        continue;
                    }
                    (cbbox[1] - img[3]).max(0.0)
                }
            };
            if dist > MAX_ASSOC_DIST {
                continue;
            }
            // Horizontal overlap guard against cross-column matches: the
            // overlap must cover a fifth of the narrower box.
            let narrower = (img[2] - img[0]).min(cbbox[2] - cbbox[0]);
            if x_overlap(img, cbbox) < narrower * 0.2 {
                continue;
            }
            matched = Some(match matched {
                Some(m) => union(m, *img),
                None => *img,
            });
            claimed[i] = true;
        }
        records.push(FigureRecord {
            page,
            kind: *kind,
            label: label.clone(),
            caption: text.chars().take(500).collect(),
            bbox: matched,
            caption_bbox: Some(*cbbox),
        });
    }

    // Orphan images (caption missed by the regex, or in the references tail).
    for (i, img) in images.iter().enumerate() {
        if !claimed[i] {
            records.push(FigureRecord {
                page,
                kind: FigureKind::Figure,
                label: String::new(),
                caption: String::new(),
                bbox: Some(*img),
                caption_bbox: None,
            });
        }
    }
    records
}

/// Extract figure/table metadata for every page of a PDF.
///
/// pdfium-only: without it the text layer has no anchors (the pdf_oxide
/// fallback carries no geometry), so there is nothing to record.
pub fn extract_figures(path: &Path) -> Result<Vec<FigureRecord>> {
    // pdfium is process-global and not thread-safe; see `bindings::pdfium_guard`.
    let _pdfium = crate::pdf::bindings::pdfium_guard();
    let Ok(pdfium) = crate::pdf::bindings::pdfium() else {
        return Ok(Vec::new());
    };
    let doc = pdfium
        .load_pdf_from_file(path, None)
        .map_err(|e| SikuError::PdfParse(format!("failed to load PDF: {e}")))?;

    let mut out = Vec::new();
    for (index, page) in doc.pages().iter().enumerate() {
        let page_no = (index + 1) as u16;
        let display_w = page.width().value;
        let display_h = page.height().value;
        let rotation = PageRotation::from_pdfium(
            page.rotation()
                .unwrap_or(pdfium_render::prelude::PdfPageRenderRotation::None),
        );

        // 1. Image objects → bboxes. `FPDFPageObj_GetBounds` reports the
        // content frame; map to the display frame like the text anchors.
        let mut images: Vec<[f32; 4]> = Vec::new();
        use pdfium_render::prelude::PdfPageObjectsCommon;
        for object in page.objects().iter() {
            if object.object_type() != pdfium_render::prelude::PdfPageObjectType::Image {
                continue;
            }
            if let Ok(quad) = pdfium_render::prelude::PdfPageObjectCommon::bounds(&object) {
                let r = quad.to_rect();
                let raw = [
                    r.left().value,
                    r.bottom().value,
                    r.right().value,
                    r.top().value,
                ];
                let mapped = rotation.map_rect(raw, display_w, display_h);
                // Degenerate/zero-area placements carry no information.
                if mapped[2] - mapped[0] > 1.0 && mapped[3] - mapped[1] > 1.0 {
                    images.push(mapped);
                }
            }
        }

        // 2. Caption paragraphs via the anchored-paragraph pipeline.
        let mut captions: Vec<(FigureKind, String, String, [f32; 4])> = Vec::new();
        if let Ok(text_page) = page.text() {
            let lines =
                crate::pdf::paragraphs::pdfium_lines(&text_page, display_w, display_h, rotation);
            for group in crate::pdf::paragraphs::group_paragraphs(&lines) {
                if let Some(anchored) =
                    crate::pdf::extractor::anchor_paragraph(page_no, &group)
                {
                    if let Some((kind, label)) = caption_label(&anchored.text) {
                        captions.push((
                            kind,
                            label,
                            anchored.text.trim().to_string(),
                            anchored.bbox.unwrap_or([0.0, 0.0, display_w, display_h]),
                        ));
                    }
                }
            }
        }

        out.extend(associate(page_no, &images, &captions));
    }
    Ok(out)
}

/// Page size in display-frame PDF points (rotation applied), for callers that
/// must resolve a crop region before rendering.
pub fn page_size(path: &Path, page_index: u16) -> Result<(f32, f32)> {
    let _pdfium = crate::pdf::bindings::pdfium_guard();
    let pdfium = crate::pdf::bindings::pdfium().map_err(SikuError::PdfParse)?;
    let doc = pdfium
        .load_pdf_from_file(path, None)
        .map_err(|e| SikuError::PdfParse(format!("failed to load PDF: {e}")))?;
    let page = doc
        .pages()
        .get(page_index as i32)
        .map_err(|e| SikuError::PdfParse(format!("failed to get page {}: {e}", page_index + 1)))?;
    Ok((page.width().value, page.height().value))
}

/// Map a region hint to a y-up rect covering the page's width.
pub fn region_rect(region: &str, page_w: f32, page_h: f32) -> Option<[f32; 4]> {
    match region.to_ascii_lowercase().as_str() {
        "full" => Some([0.0, 0.0, page_w, page_h]),
        "top" => Some([0.0, page_h * 2.0 / 3.0, page_w, page_h]),
        "middle" => Some([0.0, page_h / 3.0, page_w, page_h * 2.0 / 3.0]),
        "bottom" => Some([0.0, 0.0, page_w, page_h / 3.0]),
        _ => None,
    }
}

/// Expand a rect by `pad` points on every side, clamped to the page.
pub fn pad_rect(r: [f32; 4], pad: f32, page_w: f32, page_h: f32) -> [f32; 4] {
    [
        (r[0] - pad).max(0.0),
        (r[1] - pad).max(0.0),
        (r[2] + pad).min(page_w),
        (r[3] + pad).min(page_h),
    ]
}

/// When a caption has no image bbox (vector figure, drawn table), infer the
/// content region from the caption's own position and the layout convention.
pub fn infer_rect_from_caption(
    kind: FigureKind,
    caption_bbox: [f32; 4],
    page_w: f32,
    page_h: f32,
) -> [f32; 4] {
    let band = page_h * 0.45;
    match kind {
        // Content sits above the figure caption.
        FigureKind::Figure => [
            0.0,
            caption_bbox[3],
            page_w,
            (caption_bbox[3] + band).min(page_h),
        ],
        // Content sits below the table caption.
        FigureKind::Table => [
            0.0,
            (caption_bbox[1] - band).max(0.0),
            page_w,
            caption_bbox[1],
        ],
    }
}

/// Parse "x0,y0,x1,y1" (PDF points, y-up) from a tool argument.
pub fn parse_rect(s: &str) -> Option<[f32; 4]> {
    let v: Vec<f32> = s
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .filter_map(|p| p.trim().parse::<f32>().ok())
        .collect();
    if v.len() != 4 {
        return None;
    }
    let r = [v[0], v[1], v[2], v[3]];
    if r[2] - r[0] <= 1.0 || r[3] - r[1] <= 1.0 {
        return None;
    }
    Some(r)
}

/// Convert a y-up points rect to bitmap pixel coordinates (y-down) for a page
/// rendered at `dpi`. Returns (x, y, width, height), clamped to the bitmap.
pub fn points_to_pixels(
    rect: [f32; 4],
    page_w: f32,
    page_h: f32,
    dpi: f32,
) -> Option<(u32, u32, u32, u32)> {
    let scale = dpi / 72.0;
    let bmp_w = (page_w * scale).round().max(1.0);
    let bmp_h = (page_h * scale).round().max(1.0);
    let x0 = (rect[0] * scale).clamp(0.0, bmp_w);
    let x1 = (rect[2] * scale).clamp(0.0, bmp_w);
    // y-up points → y-down pixels: the rect's TOP edge maps to the smaller y.
    let y0 = ((page_h - rect[3]) * scale).clamp(0.0, bmp_h);
    let y1 = ((page_h - rect[1]) * scale).clamp(0.0, bmp_h);
    let (w, h) = (x1 - x0, y1 - y0);
    if w < 2.0 || h < 2.0 {
        return None;
    }
    Some((x0 as u32, y0 as u32, w as u32, h as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caption_label_matches_chinese_and_english() {
        assert_eq!(
            caption_label("图3 模型架构总览"),
            Some((FigureKind::Figure, "图3".to_string()))
        );
        assert_eq!(
            caption_label("图 2-1 流程"),
            Some((FigureKind::Figure, "图2-1".to_string()))
        );
        assert_eq!(
            caption_label("表1 实验结果"),
            Some((FigureKind::Table, "表1".to_string()))
        );
        assert_eq!(
            caption_label("Figure 3: Accuracy over time"),
            Some((FigureKind::Figure, "Fig. 3".to_string()))
        );
        assert_eq!(
            caption_label("Fig. 2. Overview"),
            Some((FigureKind::Figure, "Fig. 2".to_string()))
        );
        assert_eq!(
            caption_label("Table 2 Results on GLUE"),
            Some((FigureKind::Table, "Table 2".to_string()))
        );
    }

    #[test]
    fn caption_label_rejects_inline_mentions() {
        assert!(caption_label("如图3所示，准确率提升").is_none());
        assert!(caption_label("In Figure 3 we show").is_none());
        assert!(caption_label("Tables and figures are listed below").is_none());
        assert!(caption_label("Figure captions without numbers").is_none());
        // Prose at a paragraph start: lowercase word right after the label.
        assert!(caption_label("Table 3 shows the overall results").is_none());
        assert!(caption_label("Fig. 6 plots mean total tokens per run").is_none());
        assert!(caption_label("Table 4 partitions SREGYM's problems").is_none());
        assert!(caption_label("").is_none());
    }

    #[test]
    fn normalize_label_unifies_spellings() {
        assert_eq!(normalize_label("Fig. 3"), "fig3");
        assert_eq!(normalize_label("figure 3"), "fig3");
        assert_eq!(normalize_label("FIG.3"), "fig3");
        assert_eq!(normalize_label("图 3"), "图3");
        assert_eq!(normalize_label("Table 2"), "table2");
        assert_ne!(normalize_label("图3"), normalize_label("表3"));
    }

    #[test]
    fn region_rect_splits_page_into_thirds() {
        let (w, h) = (612.0, 792.0);
        assert_eq!(region_rect("full", w, h), Some([0.0, 0.0, 612.0, 792.0]));
        assert_eq!(region_rect("top", w, h), Some([0.0, 528.0, 612.0, 792.0]));
        assert_eq!(region_rect("middle", w, h), Some([0.0, 264.0, 612.0, 528.0]));
        assert_eq!(region_rect("bottom", w, h), Some([0.0, 0.0, 612.0, 264.0]));
        assert!(region_rect("left", w, h).is_none());
    }

    #[test]
    fn points_to_pixels_flips_y_axis() {
        // Letter page at 144 DPI → 1224x1584 bitmap.
        let full = points_to_pixels([0.0, 0.0, 612.0, 792.0], 612.0, 792.0, 144.0).unwrap();
        assert_eq!(full, (0, 0, 1224, 1584));
        // Top half of the page (y-up: 396..792) is the TOP of the bitmap (y-down: 0..792).
        let top = points_to_pixels([0.0, 396.0, 612.0, 792.0], 612.0, 792.0, 144.0).unwrap();
        assert_eq!(top, (0, 0, 1224, 792));
        // A bottom band maps to the bitmap's bottom rows.
        let bottom = points_to_pixels([0.0, 0.0, 612.0, 100.0], 612.0, 792.0, 144.0).unwrap();
        assert_eq!(bottom, (0, 1384, 1224, 200));
        // Out-of-page rects clamp instead of panicking.
        let clamped = points_to_pixels([-50.0, 700.0, 900.0, 900.0], 612.0, 792.0, 144.0).unwrap();
        assert_eq!(clamped, (0, 0, 1224, 184));
        // Degenerate rects are rejected.
        assert!(points_to_pixels([10.0, 10.0, 10.5, 400.0], 612.0, 792.0, 144.0).is_none());
    }

    #[test]
    fn parse_rect_accepts_csv_and_brackets() {
        assert_eq!(parse_rect("10,20,100,200"), Some([10.0, 20.0, 100.0, 200.0]));
        assert_eq!(parse_rect("[10.5, 20, 100, 200]"), Some([10.5, 20.0, 100.0, 200.0]));
        assert!(parse_rect("10,20,100").is_none());
        assert!(parse_rect("10,20,10.2,200").is_none()); // degenerate
        assert!(parse_rect("garbage").is_none());
    }

    #[test]
    fn associate_figure_caption_below_image() {
        // Image occupies the top half; its caption sits just below it.
        let images = vec![[100.0, 400.0, 500.0, 700.0]];
        let captions = vec![(
            FigureKind::Figure,
            "图3".to_string(),
            "图3 架构".to_string(),
            [150.0, 380.0, 450.0, 396.0],
        )];
        let records = associate(1, &images, &captions);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].bbox, Some([100.0, 400.0, 500.0, 700.0]));
    }

    #[test]
    fn associate_table_caption_above_image() {
        let images = vec![[100.0, 300.0, 500.0, 500.0]];
        let captions = vec![(
            FigureKind::Table,
            "Table 2".to_string(),
            "Table 2 Results".to_string(),
            [150.0, 510.0, 450.0, 528.0],
        )];
        let records = associate(1, &images, &captions);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].bbox, Some([100.0, 300.0, 500.0, 500.0]));
    }

    #[test]
    fn associate_rejects_wrong_side_and_far_captions() {
        let images = vec![[100.0, 400.0, 500.0, 700.0]];
        // A figure caption ABOVE the image violates the convention.
        let wrong_side = vec![(
            FigureKind::Figure,
            "图3".to_string(),
            "图3".to_string(),
            [150.0, 710.0, 450.0, 726.0],
        )];
        let records = associate(1, &images, &wrong_side);
        assert_eq!(records[0].bbox, None);
        assert_eq!(records.len(), 2); // caption row + orphan image row
        // A caption far below (different block) is not claimed either.
        let too_far = vec![(
            FigureKind::Figure,
            "图3".to_string(),
            "图3".to_string(),
            [150.0, 50.0, 450.0, 66.0],
        )];
        let records = associate(1, &images, &too_far);
        assert_eq!(records[0].bbox, None);
    }

    #[test]
    fn associate_rejects_cross_column_captions() {
        // Image in the left column; caption in the right column.
        let images = vec![[50.0, 400.0, 280.0, 700.0]];
        let captions = vec![(
            FigureKind::Figure,
            "Fig. 1".to_string(),
            "Fig. 1".to_string(),
            [330.0, 380.0, 560.0, 396.0],
        )];
        let records = associate(1, &images, &captions);
        assert_eq!(records[0].bbox, None);
    }

    #[test]
    fn infer_rect_from_caption_respects_kind() {
        let (w, h) = (612.0, 792.0);
        // Figure caption at y 300..316: content band goes UP from its top.
        let r = infer_rect_from_caption(FigureKind::Figure, [100.0, 300.0, 500.0, 316.0], w, h);
        assert_eq!(r[1], 316.0);
        assert!((r[3] - (316.0 + 0.45 * 792.0)).abs() < 0.01);
        // Table caption: content band goes DOWN from its bottom.
        let r = infer_rect_from_caption(FigureKind::Table, [100.0, 500.0, 500.0, 516.0], w, h);
        assert_eq!(r[3], 500.0);
        assert!((r[1] - (500.0 - 0.45 * 792.0)).abs() < 0.01);
    }

    // ---- End-to-end on the demo corpus (skipped when the PDFs are absent,
    // same convention as pdf::corpus_regression) ----

    fn corpus_dir() -> Option<std::path::PathBuf> {
        let dir = std::env::var("SIKU_PDF_CORPUS")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."));
        if ["demo0.pdf", "demo1.pdf", "demo2.pdf"]
            .iter()
            .all(|n| dir.join(n).exists())
        {
            Some(dir)
        } else {
            println!("[figures] SKIPPED: demo0/1/2.pdf not found in {}", dir.display());
            None
        }
    }

    /// Real-PDF check: captions are found and at least some get a bbox from
    /// the image-object association. Run with --nocapture to see the listing.
    #[test]
    fn demo_corpus_figure_extraction() {
        let Some(dir) = corpus_dir() else { return };
        let mut total_captions = 0usize;
        let mut total_with_bbox = 0usize;
        for name in ["demo0.pdf", "demo1.pdf", "demo2.pdf"] {
            let records = extract_figures(&dir.join(name)).expect(name);
            let captions = records.iter().filter(|r| !r.label.is_empty()).count();
            let with_bbox = records
                .iter()
                .filter(|r| !r.label.is_empty() && r.bbox.is_some())
                .count();
            total_captions += captions;
            total_with_bbox += with_bbox;
            println!("[figures] {name}: {} captions ({} with bbox), {} orphan images",
                captions,
                with_bbox,
                records.iter().filter(|r| r.label.is_empty()).count());
            for r in records.iter().filter(|r| !r.label.is_empty()).take(30) {
                println!(
                    "  p.{} {} {:?} bbox={:?} :: {}",
                    r.page,
                    r.kind.as_str(),
                    r.label,
                    r.bbox.map(|b| [b[0] as i32, b[1] as i32, b[2] as i32, b[3] as i32]),
                    r.caption.chars().take(60).collect::<String>()
                );
            }
        }
        assert!(total_captions > 0, "no captions found in the demo corpus");
        assert!(total_with_bbox > 0, "no caption matched an image bbox");
    }

    /// Real-PDF render check: crop the first bbox-backed figure of demo0 and a
    /// region of demo2 to PNGs under the temp dir, and print the paths.
    #[test]
    fn demo_corpus_snapshot_render() {
        let Some(dir) = corpus_dir() else { return };
        let out_dir = std::env::temp_dir().join("siku_snapshot_demo");
        std::fs::create_dir_all(&out_dir).unwrap();

        // 1. Label-driven crop from indexed metadata (demo2's figures are
        // raster images, so the caption↔image association produces bboxes).
        let demo2 = dir.join("demo2.pdf");
        let records = extract_figures(&demo2).unwrap();
        let fig = records
            .iter()
            .find(|r| !r.label.is_empty() && r.bbox.is_some())
            .expect("demo2 has no bbox-backed figure");
        let (pw, ph) = page_size(&demo2, fig.page - 1).unwrap();
        let rect = pad_rect(fig.bbox.unwrap(), 8.0, pw, ph);
        let out = out_dir.join(format!("demo2_p{}_{}.png", fig.page, fig.label.replace(' ', "_")));
        let (w, h) = crate::pdf::renderer::render_page_region(&demo2, fig.page - 1, rect, 250.0, &out)
            .unwrap();
        println!("[figures] snapshot: {} ({w}x{h})", out.display());
        assert!(w > 10 && h > 10);
        assert!(std::fs::metadata(&out).unwrap().len() > 1000);

        // 2. Caption-inferred crop (demo0's figures are vector drawings — no
        // image objects, so the caption position drives the crop).
        let demo0 = dir.join("demo0.pdf");
        let records = extract_figures(&demo0).unwrap();
        let fig = records
            .iter()
            .find(|r| !r.label.is_empty() && r.bbox.is_none() && r.caption_bbox.is_some())
            .expect("demo0 has no vector-figure caption");
        let (pw, ph) = page_size(&demo0, fig.page - 1).unwrap();
        let rect = infer_rect_from_caption(fig.kind, fig.caption_bbox.unwrap(), pw, ph);
        let out = out_dir.join(format!(
            "demo0_p{}_{}_inferred.png",
            fig.page,
            fig.label.replace(' ', "_")
        ));
        let (w, h) =
            crate::pdf::renderer::render_page_region(&demo0, fig.page - 1, rect, 250.0, &out)
                .unwrap();
        println!("[figures] inferred snapshot: {} ({w}x{h})", out.display());
        assert!(w > 10 && h > 10);

        // 3. Region-hint crop.
        let (pw, ph) = page_size(&demo2, 2).unwrap();
        let rect = region_rect("middle", pw, ph).unwrap();
        let out = out_dir.join("demo2_p3_middle.png");
        let (w, h) = crate::pdf::renderer::render_page_region(&demo2, 2, rect, 250.0, &out).unwrap();
        println!("[figures] region snapshot: {} ({w}x{h})", out.display());
        assert!((h as f32 - ph / 3.0 * 250.0 / 72.0).abs() < 4.0);
    }
}
