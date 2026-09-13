//! Geometry-based paragraph reconstruction for PDF text extraction.
//!
//! `pdfium`'s `text.all()` returns the page as physical lines; paragraph
//! boundaries are lost, and two-column papers interleave columns. This module
//! rebuilds paragraphs from per-character geometry (origin, font size):
//!
//! 1. Chars are grouped into visual lines by baseline proximity.
//! 2. A vertical gutter in the page's middle band (if any) splits columns;
//!    reading order is column-by-column, top to bottom.
//! 3. Paragraph breaks are detected from extra vertical gaps, first-line
//!    indents, short last lines, and font-size jumps (headings).
//!
//! The output is plain text with paragraphs separated by "\n\n", which is
//! exactly what `chunker::split_paragraphs` consumes — the chunker is
//! unchanged.

/// Normalise one glyph from a PDF text layer.
///
/// pdfium reports a **line-final hyphen** as `U+0002` (its soft-hyphen marker)
/// and real soft hyphens arrive as `U+00AD`. Both used to be dropped as control
/// characters, which broke one word in two: `join_lines` then saw "bench" +
/// "marking" and inserted a space, so "benchmarking" was indexed as
/// "bench marking" and could never be matched by FTS or embeddings. Measured on
/// the demo corpus: 149 / 54 / 96 lost hyphens (demo0/1/2).
///
/// Returns `None` for glyphs that carry no text (the real control characters).
pub fn normalize_glyph(c: char) -> Option<char> {
    match c {
        '\u{2}' | '\u{ad}' => Some('-'),
        c if c.is_control() => None,
        c => Some(c),
    }
}



/// The page's `/Rotate` value — the viewer's display rotation.
///
/// Both pdfium and pdf_oxide hand back character origins in the **content
/// frame** (the unrotated media box), while the page is *displayed* rotated.
/// On a `/Rotate 90` page that means a line of text arrives as glyphs stacked
/// along +y, so the baseline clustering below (which assumes horizontal text)
/// shreds it into one-character lines: on a landscape-table page that turned
/// 43% of the extracted tokens into single letters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageRotation {
    None,
    Degrees90,
    Degrees180,
    Degrees270,
}

impl PageRotation {
    /// From a /Rotate value in degrees (pdf_oxide's `get_page_rotation`).
    pub fn from_degrees(deg: i32) -> Self {
        match deg.rem_euclid(360) {
            90 => Self::Degrees90,
            180 => Self::Degrees180,
            270 => Self::Degrees270,
            _ => Self::None,
        }
    }

    /// From pdfium's reported page rotation.
    pub fn from_pdfium(rotation: pdfium_render::prelude::PdfPageRenderRotation) -> Self {
        use pdfium_render::prelude::PdfPageRenderRotation as R;
        match rotation {
            R::None => Self::None,
            R::Degrees90 => Self::Degrees90,
            R::Degrees180 => Self::Degrees180,
            R::Degrees270 => Self::Degrees270,
        }
    }

    /// Whether displaying the page swaps its width and height.
    pub fn swaps_axes(self) -> bool {
        matches!(self, Self::Degrees90 | Self::Degrees270)
    }

    /// Whether the content frame needs remapping before line reconstruction.
    ///
    /// Deliberately false for `Degrees180`: a half-turn leaves text advancing
    /// along +x, so the content frame is already in reading order — rotating it
    /// into the display frame would reverse every line. (The only thing
    /// `/Rotate 180` leaves wrong is the bbox handed to the reader's hit-test,
    /// which is no worse than before.)
    pub fn needs_frame_remap(self) -> bool {
        self.swaps_axes()
    }

    /// Map a point from the content frame into the display frame, both with the
    /// origin at the bottom-left and y growing upward. `display_w`/`display_h`
    /// are the display dimensions (pdfium's `page.width()/height()`; for
    /// pdf_oxide, the media box with the axes swapped when `swaps_axes()`).
    pub fn map_point(self, x: f32, y: f32, display_w: f32, display_h: f32) -> (f32, f32) {
        match self {
            Self::None | Self::Degrees180 => (x, y),
            // Verified against poppler on demo0 p11: the glyph 'T' of the
            // rotated "Table 1 (continued)" at content (43.31, 52.27) lands at
            // display (52.3, 43.3 from the top), matching poppler's word box.
            Self::Degrees90 => (y, display_h - x),
            Self::Degrees270 => (display_w - y, x),
        }
    }

    /// Map an axis-aligned content-frame rect into the display frame.
    pub fn map_rect(self, r: [f32; 4], display_w: f32, display_h: f32) -> [f32; 4] {
        if !self.needs_frame_remap() {
            return r;
        }
        let pts = [
            self.map_point(r[0], r[1], display_w, display_h),
            self.map_point(r[2], r[1], display_w, display_h),
            self.map_point(r[0], r[3], display_w, display_h),
            self.map_point(r[2], r[3], display_w, display_h),
        ];
        let x0 = pts.iter().map(|p| p.0).fold(f32::MAX, f32::min);
        let x1 = pts.iter().map(|p| p.0).fold(f32::MIN, f32::max);
        let y0 = pts.iter().map(|p| p.1).fold(f32::MAX, f32::min);
        let y1 = pts.iter().map(|p| p.1).fold(f32::MIN, f32::max);
        [x0, y0, x1, y1]
    }
}


/// One visual line with its geometry (PDF points; y grows upward).
#[derive(Debug, Clone)]
pub struct GeoLine {
    pub text: String,
    pub x_start: f32,
    pub x_end: f32,
    pub baseline_y: f32,
    pub font_size: f32,
    /// Majority of the line's characters are bold (heading signal).
    pub bold: bool,
}


/// pdfium's own text lines, in the order pdfium reads the page.
///
/// This replaces the geometry pipeline (baseline clustering → column-gutter
/// detection → indent/gap paragraphisation) for the pdfium path, because
/// pdfium already answers the questions that pipeline tried to reconstruct and
/// its answers measured better on the corpus:
///
/// * **Order** is content-stream order — the order the document was written in.
///   A two-column body reads column by column, a table comes out cell by cell
///   (verified on demo2 p6: every cell's text is contiguous), where geometry
///   sorting by baseline interleaves the columns and shreds the cells.
/// * **Line breaks** are pdfium's, and its newline characters are part of the
///   char list (verified byte-for-byte: concatenating the chars reproduces
///   `all()`, e.g. 5831 bytes / 100 lines on demo2 p3). Subscripts stay on their
///   line (demo2 p3: "the user input be xt"), where our baseline clustering
///   needed a separate merge pass.
///
/// Only the *boxes* are still computed here: the dual-pane line anchors and the
/// PDF highlight are built from them.
pub fn pdfium_lines(
    text_page: &pdfium_render::prelude::PdfPageText,
    display_w: f32,
    display_h: f32,
    rotation: PageRotation,
) -> Vec<GeoLine> {
    let mut lines: Vec<GeoLine> = Vec::new();
    let mut text = String::new();
    let mut x_start = f32::MAX;
    let mut x_end = f32::MIN;
    let mut baseline_y = 0.0f32;
    let mut fonts: Vec<f32> = Vec::new();
    let mut bold_count = 0usize;
    let mut any = false;

    macro_rules! flush {
        () => {
            if any {
                let trimmed = text.trim_end().to_string();
                if !trimmed.trim().is_empty() {
                    let real: Vec<f32> = fonts.iter().copied().filter(|&f| f >= 2.0).collect();
                    lines.push(GeoLine {
                        text: trimmed,
                        x_start,
                        x_end,
                        baseline_y,
                        font_size: median(real),
                        bold: bold_count * 2 > fonts.len().max(1),
                    });
                }
            }
            text.clear();
            fonts.clear();
            bold_count = 0;
            x_start = f32::MAX;
            x_end = f32::MIN;
            any = false;
        };
    }

    for ch in text_page.chars().iter() {
        let Some(raw) = ch.unicode_char() else { continue };
        // pdfium inserts the line breaks itself and they are characters in the
        // list — checked BEFORE `normalize_glyph`, which maps control characters
        // (newlines included) to nothing.
        if raw == '\n' || raw == '\r' {
            flush!();
            continue;
        }
        let Some(c) = crate::pdf::paragraphs::normalize_glyph(raw) else { continue };
        let Ok(origin_x) = ch.origin_x() else { continue };
        let Ok(origin_y) = ch.origin_y() else { continue };
        let reported_font = ch.scaled_font_size().value;
        let content_box = match ch.tight_bounds() {
            Ok(b) => [
                b.left().value,
                b.bottom().value,
                b.right().value,
                b.top().value,
            ],
            Err(_) => [
                origin_x.value,
                origin_y.value - reported_font * 0.2,
                origin_x.value + reported_font * 0.5,
                origin_y.value + reported_font * 0.8,
            ],
        };
        let box_w = (content_box[2] - content_box[0]).abs();
        let box_h = (content_box[3] - content_box[1]).abs();
        // pdfium reports a degenerate size for rotated text and space glyphs;
        // 0 would collapse every downstream threshold.
        let font = if reported_font >= 2.0 || !rotation.needs_frame_remap() {
            reported_font
        } else {
            box_w.max(box_h).max(2.0)
        };
        let (x, y) = rotation.map_point(origin_x.value, origin_y.value, display_w, display_h);
        let right = if rotation.needs_frame_remap() {
            rotation.map_rect(content_box, display_w, display_h)[2].max(x)
        } else {
            content_box[2]
        };

        // pdfium sometimes keeps two *visual* lines in one of its own lines —
        // typically a word broken with a printed hyphen ("…Evidence-aware or-"
        // + "chestration protects…"), where there is no space and no pdfium line
        // break. Split those apart again so the hyphen rule can drop it, using
        // the two signals that distinguish a wrap from a sub/superscript: the
        // baseline steps by about a line, and the glyph starts over at the
        // column's left edge (a subscript continues mid-line).
        let em = font.max(6.0);
        let wrap = any
            && (baseline_y - y).abs() > em * 0.8
            && x_end > x_start
            && x < x_end - em * 2.0;
        if wrap {
            flush!();
        }
        if !any {
            baseline_y = y;
            any = true;
        }
        if !c.is_whitespace() {
            x_start = x_start.min(x);
            x_end = x_end.max(right);
        }
        if c == ' ' && !text.is_empty() && !text.ends_with(' ') {
            text.push(' ');
        } else {
            text.push(c);
        }
        fonts.push(font);
        let bold = matches!(
            ch.font_weight(),
            Some(pdfium_render::prelude::PdfFontWeight::Weight600)
                | Some(pdfium_render::prelude::PdfFontWeight::Weight700Bold)
                | Some(pdfium_render::prelude::PdfFontWeight::Weight800)
                | Some(pdfium_render::prelude::PdfFontWeight::Weight900)
        );
        if bold {
            bold_count += 1;
        }
    }
    flush!();
    lines
}

/// Group pdfium's lines into paragraphs **without reordering them**: a new
/// paragraph starts where the vertical step to the next line is clearly larger
/// than the page's usual line pitch, which is what visually separates blocks.
///
/// Order-preserving on purpose: pdfium's order already handles columns and
/// tables, so the only job left is to insert the paragraph boundaries the
/// chunker and the dual-pane view need.
pub fn group_paragraphs(lines: &[GeoLine]) -> Vec<Vec<GeoLine>> {
    if lines.is_empty() {
        return Vec::new();
    }
    // Typical line pitch, ignoring the big jumps (they are the boundaries).
    let mut steps: Vec<f32> = Vec::new();
    for w in lines.windows(2) {
        let step = (w[0].baseline_y - w[1].baseline_y).abs();
        if step > 0.5 {
            steps.push(step);
        }
    }
    let pitch = median(steps).max(1.0);
    let limit = (pitch * 1.6).max(pitch + 2.0);

    let mut out: Vec<Vec<GeoLine>> = Vec::new();
    let mut current: Vec<GeoLine> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if let Some(prev) = lines.get(i.wrapping_sub(1)) {
            let step = (prev.baseline_y - line.baseline_y).abs();
            if !current.is_empty() && step > limit {
                out.push(std::mem::take(&mut current));
            }
        }
        current.push(line.clone());
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}


















fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{4e00}'..='\u{9fff}' | '\u{3000}'..='\u{303f}' | '\u{ff00}'..='\u{ffef}'
        | '\u{3400}'..='\u{4dbf}' | '\u{f900}'..='\u{faff}')
}

fn median(mut v: Vec<f32>) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    // Lower middle on even counts: for line-pitch estimation an occasional
    // paragraph gap in the sample must not inflate the "normal" pitch.
    v[(v.len() - 1) / 2]
}



/// Order lines for reading: full-width lines (crossing the gutter — title,
/// author block, …) first top-to-bottom, then column by column. PDF y grows
/// upward.
fn reading_order(lines: Vec<GeoLine>, gutter: Option<f32>) -> Vec<GeoLine> {
    let by_y_desc = |a: &GeoLine, b: &GeoLine| {
        b.baseline_y
            .partial_cmp(&a.baseline_y)
            .unwrap_or(std::cmp::Ordering::Equal)
    };
    match gutter {
        None => {
            let mut v = lines;
            v.sort_by(by_y_desc);
            v
        }
        Some(x) => {
            let (crossing, rest): (Vec<_>, Vec<_>) =
                lines.into_iter().partition(|l| l.x_start < x && l.x_end > x);
            let (mut left, mut right): (Vec<_>, Vec<_>) =
                rest.into_iter().partition(|l| (l.x_start + l.x_end) / 2.0 < x);
            let mut full = crossing;
            full.sort_by(by_y_desc);
            left.sort_by(by_y_desc);
            right.sort_by(by_y_desc);
            full.extend(left);
            full.extend(right);
            full
        }
    }
}

/// Join two consecutive lines of the same paragraph: hyphenation for Latin,
/// no space across CJK boundaries, a single space otherwise.
pub fn join_lines_pub(prev: &str, next: &str) -> String {
    join_lines(prev, next)
}

/// Split a paragraph's lines into the text they contribute, plus the whole
/// paragraph text.
///
/// The per-line pieces concatenate to exactly the string the `join_lines` fold
/// produces (trailing whitespace of the last line aside), so a text pane can
/// render the paragraph as one text node and still address a single line by
/// slicing at these offsets. Lengths are returned as **UTF-16 code units** so a
/// JavaScript string can be sliced with them directly.
pub fn line_segments(lines: &[GeoLine]) -> (String, Vec<usize>) {
    let mut segments: Vec<usize> = Vec::with_capacity(lines.len());
    let mut text = String::new();
    // Whether the previous line ended in a hyphen that this line continues:
    // the two are then glued without a space.
    let mut prev_dropped_hyphen = false;
    for (i, line) in lines.iter().enumerate() {
        let raw = line.text.trim_start().trim_end();
        if raw.is_empty() {
            segments.push(0);
            continue;
        }
        // A line-final hyphen that the next line continues is dropped: the word
        // is stored whole in the paragraph text (same rule as `join_lines`).
        let next_first = lines[i + 1..]
            .iter()
            .find_map(|n| n.text.trim_start().chars().next());
        let continues = next_first.is_some_and(|c| c.is_lowercase());
        let dropped_hyphen = raw.ends_with('-') && continues;
        let core = if dropped_hyphen { &raw[..raw.len() - 1] } else { raw };
        // Inter-word space unless the seam is CJK or a hyphen was removed here
        // (a removed hyphen joins the word directly, no space in between).
        let sep = match (text.chars().last(), core.chars().next()) {
            (Some(prev), Some(next))
                if !prev_dropped_hyphen && !is_cjk(prev) && !is_cjk(next) =>
            {
                " "
            }
            _ => "",
        };
        text.push_str(sep);
        text.push_str(core);
        segments.push(sep.len() + core.encode_utf16().count());
        prev_dropped_hyphen = dropped_hyphen;
    }
    (text, segments)
}

fn join_lines(prev: &str, next: &str) -> String {
    let prev_trim = prev.trim_end();
    let next_trim = next.trim_start();
    if prev_trim.is_empty() {
        return next_trim.to_string();
    }
    if next_trim.is_empty() {
        return prev_trim.to_string();
    }
    // "depen-" + "dencies" → "dependencies"
    if prev_trim.ends_with('-') {
        if let Some(c) = next_trim.chars().next() {
            if c.is_lowercase() {
                return format!("{}{}", &prev_trim[..prev_trim.len() - 1], next_trim);
            }
        }
    }
    let last = prev_trim.chars().last().unwrap();
    let first = next_trim.chars().next().unwrap();
    if is_cjk(last) || is_cjk(first) {
        format!("{prev_trim}{next_trim}")
    } else {
        format!("{prev_trim} {next_trim}")
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, x: f32, xe: f32, y: f32, font: f32) -> GeoLine {
        GeoLine {
            text: text.to_string(),
            x_start: x,
            x_end: xe,
            baseline_y: y,
            font_size: font,
            bold: false,
        }
    }

    /// The per-line slices must add up to exactly the paragraph text — the text
    /// pane renders one node per line and addresses lines by slicing, so a
    /// mismatch would highlight the wrong span.
    #[test]
    fn line_segments_reproduce_the_joined_text() {
        let cases: Vec<Vec<GeoLine>> = vec![
            vec![
                line("The proposed method achieves", 0.0, 100.0, 100.0, 10.0),
                line("state-of-the-art results on", 0.0, 100.0, 90.0, 10.0),
                line("three benchmarks.", 0.0, 100.0, 80.0, 10.0),
            ],
            vec![
                line("dependencies and depen-", 0.0, 100.0, 100.0, 10.0),
                line("dencies again", 0.0, 100.0, 90.0, 10.0),
            ],
            vec![
                line("本文提出了一种基于注意力的", 0.0, 100.0, 100.0, 10.0),
                line("语义分割方法，在多个数据集", 0.0, 100.0, 90.0, 10.0),
                line("上取得最优结果。", 0.0, 100.0, 80.0, 10.0),
            ],
        ];
        for (ci, lines) in cases.into_iter().enumerate() {
            let old_fold = lines
                .iter()
                .map(|l| l.text.as_str())
                .fold(String::new(), |acc, l| join_lines(&acc, l));
            let (text, lengths) = line_segments(&lines);
            assert_eq!(
                lengths.iter().sum::<usize>(),
                text.encode_utf16().count(),
                "case {ci}: 每行长度之和必须等于段落文本长度"
            );
            assert_eq!(text, old_fold.trim_end(), "case {ci}: 行切片拼接必须与折叠结果一致");
        }
    }

    /// Paragraph grouping preserves order and only inserts boundaries: a step
    /// well above the page's line pitch starts a new block, normal pitch does not.
    #[test]
    fn group_paragraphs_splits_on_vertical_gaps_only() {
        let lines = vec![
            line("first line", 0.0, 100.0, 300.0, 10.0),
            line("second line", 0.0, 100.0, 288.0, 10.0),
            line("third line", 0.0, 100.0, 276.0, 10.0),
            line("table row one", 0.0, 100.0, 240.0, 10.0),
            line("table row two", 0.0, 100.0, 228.0, 10.0),
        ];
        let groups = group_paragraphs(&lines);
        assert_eq!(groups.len(), 2, "应按垂直间隙切成两段");
        assert_eq!(groups[0].len(), 3);
        assert_eq!(groups[1].len(), 2);
        assert_eq!(groups[0][0].text, "first line");
        assert_eq!(groups[1][1].text, "table row two");
    }

    /// Evenly spaced lines stay one block, and the rotation mapping keeps working.
    #[test]
    fn group_paragraphs_keeps_even_spacing_together_and_rotation_maps() {
        let lines: Vec<GeoLine> = (0..10)
            .map(|i| line("x", 0.0, 100.0, 300.0 - i as f32 * 12.0, 10.0))
            .collect();
        assert_eq!(group_paragraphs(&lines).len(), 1);

        let (x, y) = PageRotation::Degrees270.map_point(43.31, 52.27, 794.0, 595.0);
        assert!((x - 741.7).abs() < 0.1 && (y - 43.31).abs() < 0.1);
        assert!(!PageRotation::None.needs_frame_remap());
        assert_eq!(PageRotation::from_degrees(-90), PageRotation::Degrees270);
    }
}
