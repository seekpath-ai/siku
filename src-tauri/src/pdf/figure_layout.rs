use std::path::Path;

use crate::core::error::{Result, SikuError};
use crate::pdf::extractor::{anchor_paragraph, line_bbox};
use crate::pdf::figures::{caption_label, FigureKind};
use crate::pdf::paragraphs::{GeoLine, PageRotation};

const IMAGE_GROUP_GAP: f32 = 72.0;
const VECTOR_GROUP_GAP: f32 = 96.0;
const ANNOTATION_GAP: f32 = 28.0;

#[derive(Debug, Clone)]
pub struct LocatedFigure {
    pub rect: [f32; 4],
    pub source: &'static str,
    pub confidence: f32,
}

#[derive(Debug, Clone, Default)]
struct PageLayout {
    width: f32,
    height: f32,
    images: Vec<[f32; 4]>,
    vectors: Vec<[f32; 4]>,
    text_lines: Vec<[f32; 4]>,
    body_blocks: Vec<[f32; 4]>,
    caption_blocks: Vec<[f32; 4]>,
}

#[derive(Clone, Copy)]
struct Band {
    low: f32,
    high: f32,
}

#[derive(Clone)]
struct Candidate {
    rect: [f32; 4],
    source: &'static str,
    confidence: f32,
}

pub fn locate_figure_region(
    path: &Path,
    page_index: u16,
    kind: FigureKind,
    caption_bbox: [f32; 4],
    indexed_bbox: Option<[f32; 4]>,
    include_caption: bool,
) -> Result<Option<LocatedFigure>> {
    let layout = extract_page_layout(path, page_index)?;
    Ok(locate_in_layout(
        &layout,
        kind,
        caption_bbox,
        indexed_bbox,
        include_caption,
    ))
}

fn locate_in_layout(
    layout: &PageLayout,
    kind: FigureKind,
    caption_bbox: [f32; 4],
    indexed_bbox: Option<[f32; 4]>,
    include_caption: bool,
) -> Option<LocatedFigure> {
    if !valid_rect(caption_bbox) || is_page_background(caption_bbox, layout) {
        return fallback(indexed_bbox, caption_bbox, include_caption, layout);
    }

    let preferred = candidate_for_side(layout, kind, caption_bbox, include_caption, true);
    let candidate = preferred.or_else(|| {
        candidate_for_side(layout, kind, caption_bbox, include_caption, false).map(|mut c| {
            c.confidence = (c.confidence - 0.12).max(0.3);
            c
        })
    });

    candidate
        .map(|c| LocatedFigure {
            rect: c.rect,
            source: c.source,
            confidence: c.confidence,
        })
        .or_else(|| fallback(indexed_bbox, caption_bbox, include_caption, layout))
}

fn fallback(
    indexed_bbox: Option<[f32; 4]>,
    caption_bbox: [f32; 4],
    include_caption: bool,
    layout: &PageLayout,
) -> Option<LocatedFigure> {
    let mut rect = indexed_bbox.filter(|b| valid_rect(*b) && !is_page_background(*b, layout))?;
    if include_caption && valid_rect(caption_bbox) {
        rect = union(rect, caption_bbox);
    }
    Some(LocatedFigure {
        rect,
        source: "indexed_image",
        confidence: 0.8,
    })
}

fn candidate_for_side(
    layout: &PageLayout,
    kind: FigureKind,
    caption: [f32; 4],
    include_caption: bool,
    preferred: bool,
) -> Option<Candidate> {
    let above = matches!(kind, FigureKind::Figure) == preferred;
    let band = search_band(layout, caption, above);
    if band.high - band.low < 8.0 {
        return None;
    }
    let column = caption_column(caption, layout.width);

    let images: Vec<[f32; 4]> = layout
        .images
        .iter()
        .copied()
        .filter(|r| candidate_rect(*r, band, column, layout))
        .collect();
    let vectors: Vec<[f32; 4]> = layout
        .vectors
        .iter()
        .copied()
        .filter(|r| candidate_rect(*r, band, column, layout))
        .collect();

    let mut core = if images.is_empty() {
        None
    } else {
        grouped_core(&images, caption, IMAGE_GROUP_GAP)
    };
    let mut source = if core.is_some() { "image_layout" } else { "" };

    if let Some(image_core) = core {
        let mut expanded = image_core;
        for vector in &vectors {
            if rect_distance(*vector, image_core) <= ANNOTATION_GAP {
                expanded = union(expanded, *vector);
            }
        }
        core = Some(expanded);
    } else if !vectors.is_empty() {
        core = grouped_core(&vectors, caption, VECTOR_GROUP_GAP);
        source = "vector_layout";
    }

    let mut rect = match core {
        Some(core) => {
            let mut rect = core;
            for line in &layout.text_lines {
                if candidate_text(*line, caption, band, column, layout.height)
                    && rect_distance(*line, core) <= ANNOTATION_GAP
                {
                    rect = union(rect, *line);
                }
            }
            rect
        }
        None if matches!(kind, FigureKind::Table) => {
            let lines: Vec<[f32; 4]> = layout
                .text_lines
                .iter()
                .copied()
                .filter(|r| candidate_text(*r, caption, band, column, layout.height))
                .collect();
            if lines.len() < 2 {
                return None;
            }
            source = "text_layout";
            lines.into_iter().reduce(union)?
        }
        None => return None,
    };

    if !valid_rect(rect) {
        return None;
    }
    let mut confidence = match source {
        "image_layout" => 0.92,
        "vector_layout" => 0.7,
        _ => 0.55,
    };
    let page_area = (layout.width * layout.height).max(1.0);
    if (rect[2] - rect[0]) * (rect[3] - rect[1]) > page_area * 0.75 {
        confidence *= 0.7;
    }
    if include_caption {
        rect = union(rect, caption);
    }
    Some(Candidate {
        rect,
        source,
        confidence,
    })
}

fn search_band(layout: &PageLayout, caption: [f32; 4], above: bool) -> Band {
    let column = caption_column(caption, layout.width);
    let mut band = if above {
        Band {
            low: caption[3],
            high: layout.height,
        }
    } else {
        Band {
            low: 0.0,
            high: caption[1],
        }
    };

    for block in layout
        .body_blocks
        .iter()
        .chain(layout.caption_blocks.iter())
    {
        if same_rect(*block, caption) || horizontal_overlap(*block, column) <= 0.0 {
            continue;
        }
        if above && block[1] >= caption[3] - 2.0 {
            band.high = band.high.min((block[1] - 4.0).max(caption[3]));
        } else if !above && block[2] <= caption[1] + 2.0 {
            band.low = band.low.max((block[2] + 4.0).min(caption[1]));
        }
    }
    band
}

fn caption_column(caption: [f32; 4], page_width: f32) -> [f32; 4] {
    let middle = page_width / 2.0;
    let width = caption[2] - caption[0];
    let center = (caption[0] + caption[2]) / 2.0;
    let centered = center > page_width * 0.45
        && center < page_width * 0.55
        && caption[0] < middle
        && caption[2] > middle;
    if width > page_width * 0.62 || centered {
        [0.0, 0.0, page_width, 0.0]
    } else if center < middle {
        [0.0, 0.0, middle + 8.0, 0.0]
    } else {
        [middle - 8.0, 0.0, page_width, 0.0]
    }
}

fn candidate_rect(rect: [f32; 4], band: Band, column: [f32; 4], layout: &PageLayout) -> bool {
    valid_rect(rect)
        && !is_page_background(rect, layout)
        && center_in_band(rect, band)
        && horizontal_overlap(rect, column) > 0.0
}

fn candidate_text(
    rect: [f32; 4],
    caption: [f32; 4],
    band: Band,
    column: [f32; 4],
    page_height: f32,
) -> bool {
    let center_y = (rect[1] + rect[3]) / 2.0;
    let width = (rect[2] - rect[0]).max(1.0);
    valid_rect(rect)
        && !same_rect(rect, caption)
        && rect_distance(rect, caption) > 1.0
        && center_y > page_height * 0.05
        && center_y < page_height * 0.95
        && center_in_band(rect, band)
        && horizontal_overlap(rect, column) >= width * 0.6
}

fn grouped_core(rects: &[[f32; 4]], caption: [f32; 4], gap: f32) -> Option<[f32; 4]> {
    let mut ordered: Vec<[f32; 4]> = rects.to_vec();
    ordered.sort_by(|a, b| {
        rect_distance(*a, caption)
            .partial_cmp(&rect_distance(*b, caption))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut core = ordered.first().copied()?;
    for rect in ordered.into_iter().skip(1) {
        if rect_distance(rect, core) <= gap {
            core = union(core, rect);
        }
    }
    Some(core)
}

fn center_in_band(rect: [f32; 4], band: Band) -> bool {
    let center = (rect[1] + rect[3]) / 2.0;
    center >= band.low && center <= band.high
}

fn valid_rect(rect: [f32; 4]) -> bool {
    rect.iter().all(|v| v.is_finite()) && rect[2] - rect[0] > 1.0 && rect[3] - rect[1] > 1.0
}

fn is_page_background(rect: [f32; 4], layout: &PageLayout) -> bool {
    let area = ((rect[2] - rect[0]) * (rect[3] - rect[1])).max(0.0);
    area > layout.width * layout.height * 0.9
}

fn same_rect(a: [f32; 4], b: [f32; 4]) -> bool {
    (0..4).all(|i| (a[i] - b[i]).abs() < 2.0)
}

fn union(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [
        a[0].min(b[0]),
        a[1].min(b[1]),
        a[2].max(b[2]),
        a[3].max(b[3]),
    ]
}

fn horizontal_overlap(a: [f32; 4], b: [f32; 4]) -> f32 {
    a[2].min(b[2]) - a[0].max(b[0])
}

fn rect_distance(a: [f32; 4], b: [f32; 4]) -> f32 {
    let dx = (a[0] - b[2]).max(b[0] - a[2]).max(0.0);
    let dy = (a[1] - b[3]).max(b[1] - a[3]).max(0.0);
    (dx * dx + dy * dy).sqrt()
}

fn extract_page_layout(path: &Path, page_index: u16) -> Result<PageLayout> {
    let _pdfium = crate::pdf::bindings::pdfium_guard();
    let pdfium = crate::pdf::bindings::pdfium().map_err(SikuError::PdfParse)?;
    let doc = pdfium
        .load_pdf_from_file(path, None)
        .map_err(|e| SikuError::PdfParse(format!("failed to load PDF: {e}")))?;
    let page = doc
        .pages()
        .get(page_index as i32)
        .map_err(|e| SikuError::PdfParse(format!("failed to get page {}: {e}", page_index + 1)))?;
    let width = page.width().value;
    let height = page.height().value;
    let rotation = PageRotation::from_pdfium(
        page.rotation()
            .unwrap_or(pdfium_render::prelude::PdfPageRenderRotation::None),
    );
    let mut layout = PageLayout {
        width,
        height,
        ..PageLayout::default()
    };

    use pdfium_render::prelude::{PdfPageObjectCommon, PdfPageObjectType, PdfPageObjectsCommon};
    for object in page.objects().iter() {
        let kind = object.object_type();
        let is_image = kind == PdfPageObjectType::Image;
        let is_vector = matches!(
            kind,
            PdfPageObjectType::Path | PdfPageObjectType::Shading | PdfPageObjectType::XObjectForm
        );
        if !is_image && !is_vector {
            continue;
        }
        if let Ok(quad) = object.bounds() {
            let raw = quad.to_rect();
            let rect = rotation.map_rect(
                [
                    raw.left().value,
                    raw.bottom().value,
                    raw.right().value,
                    raw.top().value,
                ],
                width,
                height,
            );
            if valid_rect(rect) {
                if is_image {
                    layout.images.push(rect);
                } else {
                    layout.vectors.push(rect);
                }
            }
        }
    }

    if let Ok(text_page) = page.text() {
        let lines = crate::pdf::paragraphs::pdfium_lines(&text_page, width, height, rotation);
        for group in crate::pdf::paragraphs::group_paragraphs(&lines) {
            let Some(anchor) = anchor_paragraph(page_index + 1, &group) else {
                continue;
            };
            let Some(bbox) = anchor.bbox else {
                continue;
            };
            if caption_label(&anchor.text).is_some() {
                layout.caption_blocks.push(bbox);
            } else if is_body_group(&group, width) {
                layout.body_blocks.push(bbox);
            } else {
                layout.text_lines.extend(group.iter().map(line_bbox));
            }
        }
    }
    Ok(layout)
}

fn is_body_group(group: &[GeoLine], page_width: f32) -> bool {
    if group.len() < 2 {
        return false;
    }
    let total_chars: usize = group.iter().map(|l| l.text.chars().count()).sum();
    let average_chars = total_chars / group.len();
    let wide_lines = group
        .iter()
        .filter(|l| l.x_end - l.x_start > page_width * 0.28)
        .count();
    (group.len() >= 3 && total_chars >= 160 && (wide_lines >= 2 || average_chars >= 45))
        || (group.len() == 2 && total_chars >= 120 && wide_lines == 2 && average_chars >= 40)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> PageLayout {
        PageLayout {
            width: 600.0,
            height: 800.0,
            ..PageLayout::default()
        }
    }

    #[test]
    fn caption_column_distinguishes_centered_and_right_column_captions() {
        assert_eq!(
            caption_column([250.0, 100.0, 350.0, 115.0], 600.0)[2],
            600.0
        );
        let right = caption_column([250.0, 100.0, 550.0, 115.0], 600.0);
        assert_eq!(right[0], 292.0);
        assert_eq!(right[2], 600.0);
    }

    #[test]
    fn image_crop_stays_in_column_and_includes_annotations_and_caption() {
        let mut page = layout();
        let caption = [50.0, 200.0, 250.0, 215.0];
        page.images = vec![[60.0, 350.0, 240.0, 550.0], [330.0, 350.0, 550.0, 550.0]];
        page.text_lines = vec![[48.0, 330.0, 90.0, 342.0], [330.0, 320.0, 370.0, 332.0]];
        page.body_blocks = vec![[50.0, 640.0, 250.0, 750.0]];
        page.caption_blocks = vec![caption];

        let found = locate_in_layout(&page, FigureKind::Figure, caption, None, true).unwrap();
        assert_eq!(found.source, "image_layout");
        assert!(found.confidence > 0.8);
        assert!(
            found.rect[2] <= 250.0,
            "right column leaked into {:?}",
            found.rect
        );
        assert!(found.rect[0] <= 48.0 && found.rect[1] <= 200.0);
        assert!(found.rect[3] >= 550.0 && found.rect[3] < 640.0);
    }

    #[test]
    fn vector_crop_uses_drawing_bounds_not_a_page_wide_fixed_band() {
        let mut page = layout();
        let caption = [50.0, 100.0, 250.0, 115.0];
        page.vectors = vec![[60.0, 200.0, 240.0, 205.0], [60.0, 300.0, 240.0, 305.0]];
        page.text_lines = vec![[70.0, 250.0, 110.0, 262.0]];
        page.caption_blocks = vec![caption];

        let found = locate_in_layout(&page, FigureKind::Figure, caption, None, false).unwrap();
        assert_eq!(found.source, "vector_layout");
        assert!(
            found.confidence < 0.75,
            "vector layout should allow vision refinement"
        );
        assert_eq!(found.rect, [60.0, 200.0, 240.0, 305.0]);
    }

    #[test]
    fn text_table_excludes_following_body_paragraph() {
        let mut page = layout();
        let caption = [50.0, 600.0, 250.0, 615.0];
        page.text_lines = vec![
            [60.0, 520.0, 220.0, 532.0],
            [60.0, 480.0, 225.0, 492.0],
            [60.0, 440.0, 215.0, 452.0],
        ];
        page.body_blocks = vec![[50.0, 100.0, 250.0, 250.0]];
        page.caption_blocks = vec![caption];

        let found = locate_in_layout(&page, FigureKind::Table, caption, None, true).unwrap();
        assert_eq!(found.source, "text_layout");
        assert_eq!(found.rect, [50.0, 440.0, 250.0, 615.0]);
    }

    #[test]
    fn full_page_scan_is_not_accepted_as_a_precise_figure() {
        let mut page = layout();
        let caption = [50.0, 100.0, 250.0, 115.0];
        page.images = vec![[0.0, 0.0, 600.0, 800.0]];
        page.caption_blocks = vec![caption];
        assert!(locate_in_layout(&page, FigureKind::Figure, caption, None, false).is_none());
    }

    #[test]
    fn demo_corpus_layout_location_renders_real_crops() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
        if !["demo0.pdf", "demo1.pdf", "demo2.pdf"]
            .iter()
            .all(|name| dir.join(name).exists())
        {
            return;
        }
        let out_dir = std::env::temp_dir().join("siku_figure_layout_demo");
        std::fs::create_dir_all(&out_dir).unwrap();

        for name in ["demo0.pdf", "demo1.pdf", "demo2.pdf"] {
            let path = dir.join(name);
            let records = crate::pdf::figures::extract_figures(&path).unwrap();
            let mut located_count = 0;
            let mut rendered = false;
            for fig in records
                .iter()
                .filter(|r| !r.label.is_empty() && r.caption_bbox.is_some())
                .take(8)
            {
                let Some(found) = locate_figure_region(
                    &path,
                    fig.page - 1,
                    fig.kind,
                    fig.caption_bbox.unwrap(),
                    fig.bbox,
                    true,
                )
                .unwrap() else {
                    continue;
                };
                let (pw, ph) = crate::pdf::figures::page_size(&path, fig.page - 1).unwrap();
                assert!(found.rect[2] > 0.0 && found.rect[0] < pw);
                assert!(found.rect[3] > 0.0 && found.rect[1] < ph);
                located_count += 1;
                if !rendered {
                    let rect = crate::pdf::figures::pad_rect(found.rect, 8.0, pw, ph);
                    let out = out_dir.join(format!(
                        "{}_p{}_{}.png",
                        name,
                        fig.page,
                        fig.label.replace(' ', "_")
                    ));
                    crate::pdf::renderer::render_page_region(
                        &path,
                        fig.page - 1,
                        rect,
                        160.0,
                        &out,
                    )
                    .unwrap();
                    println!(
                        "[figure-layout] {} p.{} {} source={} confidence={:.2} rect={:?} output={}",
                        name,
                        fig.page,
                        fig.label,
                        found.source,
                        found.confidence,
                        found.rect,
                        out.display()
                    );
                    rendered = true;
                }
            }
            assert!(located_count > 0, "no figure could be located in {name}");
        }
    }
}
