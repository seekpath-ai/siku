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

/// One character with layout geometry, source-agnostic: produced by the
/// pdfium adapter (`page_to_lines`) or the pdf_oxide fallback path.
#[derive(Debug, Clone)]
pub struct RawChar {
    pub x: f32,
    pub y: f32,
    pub right: f32,
    pub font: f32,
    pub ch: char,
    pub bold: bool,
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

/// Extract a page's geometric lines from its pdfium text page. Space glyphs
/// are unreliable in PDFs, so inter-word spaces are inferred from x gaps.
///
/// `display_w`/`display_h` are the page's displayed dimensions (what
/// `page.width()/height()` report) and `rotation` its `/Rotate`; character
/// origins are mapped from the content frame into that display frame first,
/// so `/Rotate 90|270` pages — landscape tables stored in a portrait media box
/// — reconstruct as ordinary horizontal text instead of one-char lines.
pub fn page_to_lines(
    text_page: &pdfium_render::prelude::PdfPageText,
    display_w: f32,
    display_h: f32,
    rotation: PageRotation,
) -> (Vec<GeoLine>, Option<f32>) {
    // Read every glyph, mapping it from the content frame into the display frame.
    let mut chars: Vec<RawChar> = Vec::new();
    for ch in text_page.chars().iter() {
        let Some(c) = ch.unicode_char() else { continue };
        let Some(c) = normalize_glyph(c) else { continue };
        let (Ok(origin_x), Ok(origin_y)) = (ch.origin_x(), ch.origin_y()) else { continue };
        let reported_font = ch.scaled_font_size().value;
        // Content-frame glyph box (fall back to an origin+advance estimate when
        // pdfium reports no bounds).
        let content_box = match ch.loose_bounds() {
            Ok(b) => [b.left().value, b.bottom().value, b.right().value, b.top().value],
            Err(_) => [
                origin_x.value,
                origin_y.value - reported_font * 0.2,
                origin_x.value + reported_font * 0.5,
                origin_y.value + reported_font * 0.8,
            ],
        };
        // pdfium reports a degenerate size (0 / 1) for some glyphs — notably
        // rotated text and space glyphs. A 0 would collapse every downstream
        // threshold, so recover the size from the glyph box (its extent across
        // the advance direction is the body height).
        let box_w = (content_box[2] - content_box[0]).abs();
        let box_h = (content_box[3] - content_box[1]).abs();
        let font = if reported_font >= 2.0 || !rotation.needs_frame_remap() {
            // Unrotated pages keep the historical behaviour exactly.
            reported_font
        } else {
            box_w.max(box_h).max(2.0)
        };
        // Pen position in the display frame; `right` is the glyph's ink edge
        // along the display advance direction. Using the box's *left* edge for
        // `x` here would mix ink and pen metrics and manufacture spaces inside
        // words ("desig n"), so `x` stays the mapped origin.
        let (x, y) = rotation.map_point(origin_x.value, origin_y.value, display_w, display_h);
        let right = if rotation.needs_frame_remap() {
            let mapped = rotation.map_rect(content_box, display_w, display_h);
            mapped[2].max(x)
        } else {
            content_box[2]
        };
        let bold = ch.font_name().to_lowercase().contains("bold");
        chars.push(RawChar { x, y, right, font, ch: c, bold });
    }

    // Every glyph is kept. Dropping the "display-vertical" minority (running
    // heads, page numbers, sideways insets) looks attractive but measured
    // worse: text-object boundaries inside a rotated table make that
    // classification unreliable, and dropping ~14% of the glyphs cost 4 points
    // of word-level F1 without improving the single-letter share. Such glyphs
    // fall to the ordinary margin/paragraph machinery instead.

    chars_to_lines(chars, display_w)
}

/// Core line reconstruction: baseline clustering, page-level gutter
/// detection, and per-cluster x-gap segment splits. Every returned line
/// belongs to exactly one column; the gutter (when found) is returned
/// alongside for the paragraph stage.
///
/// Baseline clustering alone would merge same-y text from DIFFERENT columns
/// into one line, so each cluster is then split at large internal x-gaps.
pub fn chars_to_lines(
    mut chars: Vec<RawChar>,
    page_width: f32,
) -> (Vec<GeoLine>, Option<f32>) {
    if chars.is_empty() {
        return (Vec::new(), None);
    }

    // Cluster into baseline groups (y desc, x asc within a group).
    chars.sort_by(|a, b| {
        b.y.partial_cmp(&a.y)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal))
    });
    let mut groups: Vec<(f32, Vec<RawChar>)> = Vec::new();
    let mut group: Vec<RawChar> = Vec::new();
    let mut group_y = 0.0f32;
    let mut group_font = 0.0f32;
    for ch in chars {
        // Group font is the running MAX: pdfium reports degenerate sizes
        // (font=1.0) for some space glyphs, and a first-char space would
        // otherwise collapse the clustering tolerance and split thresholds,
        // shredding the line into per-word pieces.
        let tol = (group_font.max(ch.font) * 0.5).max(1.0);
        let same_line = !group.is_empty() && (ch.y - group_y).abs() <= tol;
        if !same_line {
            if !group.is_empty() {
                groups.push((group_y, std::mem::take(&mut group)));
            }
            group_y = ch.y;
            group_font = ch.font;
        }
        group_font = group_font.max(ch.font);
        group.push(ch);
    }
    if !group.is_empty() {
        groups.push((group_y, group));
    }

    // Sub/superscript glyphs sit on their own baseline a fraction of an em off the
    // body line, so they arrive here as separate groups: on demo2 p3 the subscripts
    // of x_t / c_t / s_t sit 5.3pt below a 10pt line, just past the 5pt clustering
    // tolerance, and each became a stray one-character line ("… the session" / "t"
    // / "context be …"). Fold such a group back into the line it belongs to.
    //
    // The test is deliberately narrow — smaller glyphs, an offset well under a line
    // step, and horizontally inside the line's span — because that page's leading
    // is only 8.1pt: simply widening the tolerance would glue neighbouring lines
    // together.
    let mut merged: Vec<(f32, Vec<RawChar>)> = Vec::with_capacity(groups.len());
    for (y, g) in groups {
        let font_of = |chars: &[RawChar]| {
            median(chars.iter().map(|c| c.font).filter(|&f| f >= 2.0).collect())
        };
        let span_of = |chars: &[RawChar]| {
            chars.iter().fold((f32::MAX, f32::MIN), |(a, b), c| {
                (a.min(c.x), b.max(c.right))
            })
        };
        let (x0, x1) = span_of(&g);
        let font = font_of(&g);
        let attach = merged
            .last()
            .map(|(py, pg)| {
                let pfont = font_of(pg);
                let (px0, px1) = span_of(pg);
                let smaller = font > 0.0 && pfont > 0.0 && font < pfont * 0.85;
                let near = (py - y).abs() <= pfont * 0.6;
                let inside = x0 >= px0 - 1.0 && x1 <= px1 + 1.0;
                smaller && near && inside
            })
            .unwrap_or(false);
        if attach {
            let (_, pg) = merged.last_mut().expect("checked above");
            pg.extend(g);
            pg.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));
            continue;
        }
        merged.push((y, g));
    }
    let groups = merged;

    // Column gutter has to be known BEFORE splitting, because the split is what
    // separates the two columns sharing a baseline. The line-geometry detector
    // (widest empty vertical band) comes first: the char x-histogram misreads a
    // page whose top half is a table spanning both columns as single-column
    // (demo2 p6), and then merged left+right lines survive into the index.
    let rough: Vec<GeoLine> = groups.iter().map(|(y, g)| rough_line(g, *y)).collect();
    let body_font = median(
        rough.iter().map(|l| l.font_size).filter(|&f| f >= 2.0).collect(),
    )
    .max(2.0);
    // Median inter-glyph gap on the page — the yardstick that separates a word
    // space from a column gutter.
    let mut gaps: Vec<f32> = Vec::new();
    for (_, g) in &groups {
        for w in g.windows(2) {
            let gap = w[1].x - w[0].right;
            if gap > 0.2 && gap < page_width * 0.05 {
                gaps.push(gap);
            }
        }
    }
    let median_gap = median(gaps.clone());
    let min_gap = gutter_min_gap(&gaps);
    let gutter = detect_gutter_from_gaps(&groups, page_width, &gaps)
        .or_else(|| find_gutter(&rough, page_width, body_font))
        .or_else(|| detect_gutter_chars(&rough_to_chars(&groups), page_width))
        // Every detector's answer has to pass the same evidence test: a real
        // gutter has a wide gap on many rows and almost no ink there. The three
        // detectors look for different things, so on a single-column page any of
        // them can land mid-line — which cuts body lines in half (demo1 p2: a
        // candidate at x=225 with ink ≈ the whole column).
        .filter(|x| {
            let (ink, support) = gutter_evidence(&groups, min_gap, *x);
            ink == 0 || ink <= support
        });
    tracing::debug!(
        gutter = ?gutter,
        median_gap,
        groups = groups.len(),
        "column gutter detection"
    );

    let mut lines: Vec<GeoLine> = Vec::new();
    for (y, g) in &groups {
        lines.extend(split_line_segments(g, *y, gutter));
    }
    (lines, gutter)
}

/// The yardstick that separates a word space from a column gutter: well above
/// the bulk of the gap distribution, because justified text stretches word
/// spaces a long way (measured p95 ≈ 6pt, p99 ≈ 8pt on demo2 p6).
fn gutter_min_gap(gaps: &[f32]) -> f32 {
    let mut sorted = gaps.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let p95 = if sorted.is_empty() {
        2.0
    } else {
        sorted[((sorted.len() - 1) as f32 * 0.95) as usize]
    };
    (p95 * 1.5).max(8.0)
}

/// Evidence for one candidate x: (glyphs whose box covers x, rows whose gap at x
/// is wide enough to be a gutter). A real gutter has a wide gap on many rows and
/// almost no ink; a single-column page can offer a well-supported candidate in
/// the middle of a line, where the ink count is the whole column (measured on
/// demo1 p2: x=225 with ink ≈ 40 against support ≈ 7).
fn gutter_evidence(groups: &[(f32, Vec<RawChar>)], min_gap: f32, x: f32) -> (usize, usize) {
    let mut ink = 0usize;
    let mut support = 0usize;
    for (_, g) in groups {
        let mut row_gap = false;
        for w in g.windows(2) {
            if w[0].right < x && w[1].x > x && w[1].x - w[0].right >= min_gap {
                row_gap = true;
            }
        }
        if row_gap {
            support += 1;
        }
        for c in g {
            if !c.ch.is_whitespace() && c.x <= x && c.right >= x {
                ink += 1;
            }
        }
    }
    (ink, support)
}

/// A baseline group collapsed to one rough line — only the geometry is used
/// (gutter detection); the text is joined cheaply and never leaves this module.
fn rough_line(group: &[RawChar], baseline_y: f32) -> GeoLine {
    let x_start = group.iter().map(|c| c.x).fold(f32::MAX, f32::min);
    let x_end = group.iter().map(|c| c.right).fold(f32::MIN, f32::max);
    let real: Vec<f32> = group.iter().map(|c| c.font).filter(|&f| f >= 2.0).collect();
    GeoLine {
        text: group.iter().map(|c| c.ch).collect(),
        x_start,
        x_end,
        baseline_y,
        font_size: median(real),
        bold: false,
    }
}

fn rough_to_chars(groups: &[(f32, Vec<RawChar>)]) -> Vec<RawChar> {
    groups
        .iter()
        .flat_map(|(_, g)| g.iter().cloned())
        .collect()
}

/// Column gutter from *gap support*: an x that falls inside a wide inter-glyph
/// gap on many lines.
///
/// Why not the two simpler tests: a page can be two-column only in its lower
/// part while its top half is a table spanning both columns (demo2 p6). Then
/// (a) no x is empty across the whole page height, so an "empty vertical band"
/// never exists, and (b) the char x-histogram sees almost every line crossing
/// the gutter and concludes "single column". What survives is the local
/// evidence: the gutter's x sits inside a wide gap on many lines, consistently.
fn detect_gutter_from_gaps(groups: &[(f32, Vec<RawChar>)], page_width: f32, gaps: &[f32]) -> Option<f32> {
    // Justified text stretches word spaces a long way (measured p95 ≈ 6pt,
    // p99 ≈ 8pt on demo2 p6), so the yardstick has to be well above the bulk of
    // the gap distribution — otherwise ordinary spaces masquerade as gutters.
    let mut sorted = gaps.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let p95 = if sorted.is_empty() { 2.0 } else { sorted[((sorted.len() - 1) as f32 * 0.95) as usize] };
    let min_gap = (p95 * 1.5).max(8.0);
    let lo = page_width * 0.25;
    let hi = page_width * 0.75;
    let step = 2.0f32;
    let nb = (((hi - lo) / step) as usize) + 1;
    let mut support = vec![0usize; nb];
    let mut widest = vec![0.0f32; nb];

    for (_, g) in groups {
        for w in g.windows(2) {
            let gap = w[1].x - w[0].right;
            if gap < min_gap {
                continue;
            }
            let (a, b) = (w[0].right, w[1].x);
            let i0 = (((a - lo) / step).ceil().max(0.0)) as usize;
            let i1 = (((b - lo) / step).floor()).max(0.0) as usize;
            for i in i0..=i1.min(nb - 1) {
                support[i] += 1;
                widest[i] = widest[i].max(gap);
            }
        }
    }

    // How much glyph ink covers each candidate x. The gap histogram alone is
    // not a reliable gutter test: an equation-heavy column is full of wide
    // internal gaps that out-support the real gutter, and picking one of those
    // (measured on demo2 p4: x=335 at ink 21 vs support 6, inside the right
    // column, instead of the real gutter at ~306) leaves every left+right row
    // merged. A handful of glyphs may still graze a real gutter — a table row
    // or an indented block legitimately crosses it (demo2 p6: ink 20 vs support
    // 30) — so ink is compared against the gap support rather than required to
    // be zero.
    let mut ink = vec![0usize; nb];
    for (_, g) in groups {
        for c in g {
            if c.ch.is_whitespace() {
                continue;
            }
            let i0 = (((c.x - lo) / step).ceil().max(0.0)) as usize;
            let i1 = (((c.right - lo) / step).floor().max(0.0)) as usize;
            for slot in ink.iter_mut().take(i1.min(nb - 1) + 1).skip(i0) {
                *slot += 1;
            }
        }
    }

    let mut best: Option<(f32, usize, f32)> = None; // (x, support, score)
    for i in 0..nb {
        if support[i] < 5 || ink[i] > support[i] {
            continue;
        }
        let x = lo + step * i as f32;
        // A gutter is well-supported, empty of ink, AND near the middle: a
        // table's internal cell boundary can be supported by more rows than the
        // real gutter is by body lines (demo2 p6), so proximity to the centre
        // breaks that tie.
        let score = support[i] as f32 - (page_width / 2.0 - x).abs() * 0.5;
        if best.map(|(_, _, s)| score > s).unwrap_or(true) {
            best = Some((x, support[i], score));
        }
    }
    let (x, _, _) = best?;

    // Both sides must hold a real share of the text.
    let mut left = 0usize;
    let mut right = 0usize;
    for (_, g) in groups {
        for c in g {
            if c.ch.is_whitespace() {
                continue;
            }
            if (c.x + c.right) / 2.0 < x {
                left += 1;
            } else {
                right += 1;
            }
        }
    }
    let total = left + right;
    if total == 0 || left * 4 < total || right * 4 < total {
        return None;
    }
    Some(x)
}

/// Page-level column gutter from the char x-histogram. Metric per x bucket:
/// how many DISTINCT lines cross it (a full-width title/abstract line crosses
/// the gutter, but so does EVERY line of a single-column page — only a true
/// gutter has few crossing lines relative to the text buckets around it).
/// Accept the minimum when its ratio to the median is small and both sides
/// hold a decent share of the text mass.
fn detect_gutter_chars(chars: &[RawChar], page_width: f32) -> Option<f32> {
    const BUCKET: f32 = 4.0;
    let nb = (page_width / BUCKET).ceil() as usize + 1;
    // bucket -> set of coarse y keys (one entry per distinct line crossing it)
    let mut crossings: Vec<std::collections::HashSet<i32>> = (0..nb).map(|_| Default::default()).collect();
    let mut mass = vec![0usize; nb];
    for c in chars {
        if c.ch.is_whitespace() {
            continue;
        }
        let b = ((c.x / BUCKET) as usize).min(nb - 1);
        crossings[b].insert((c.y / 2.0).round() as i32);
        mass[b] += 1;
    }
    let total_chars: usize = mass.iter().sum();
    if total_chars < 200 {
        return None; // too little text to speak of columns
    }
    let cross_counts: Vec<f32> = crossings
        .iter()
        .map(|s| s.len() as f32)
        .filter(|&c| c > 0.0)
        .collect();
    let typical = median(cross_counts).max(1.0);

    let mut best: Option<(f32, f32)> = None; // (x, ratio)
    let mut x = page_width * 0.35;
    while x <= page_width * 0.65 {
        let b = ((x / BUCKET) as usize).min(nb - 1);
        let ratio = crossings[b].len() as f32 / typical;
        // Prefer the smallest ratio; ties go to the x nearest the center.
        let center_penalty = (page_width / 2.0 - x).abs() * 0.0001;
        let score = ratio + center_penalty;
        if best.map(|(_, s)| score < s).unwrap_or(true) {
            best = Some((x, score));
        }
        x += BUCKET;
    }
    let (x, ratio) = best?;
    if ratio > 0.4 {
        return None; // no band distinctly emptier than the text around it
    }
    let left_mass: usize = mass[..((x / BUCKET) as usize)].iter().sum();
    let right_mass: usize = mass[((x / BUCKET) as usize) + 1..].iter().sum();
    if left_mass * 3 < total_chars || right_mass * 3 < total_chars {
        return None; // badly unbalanced — not columns
    }
    Some(x)
}

/// Split a baseline cluster at large internal x-gaps (column gutters and
/// layout separators). Chars are re-sorted by x here because clustering is
/// y-tolerant and two columns with slightly different baselines arrive out
/// of x order.
///
/// The split threshold is relative, not fixed: IEEE-style papers pack columns
/// with a gutter as narrow as ~1.2 fonts, while justified text can stretch
/// word gaps to ~0.6 fonts. A gutter is an extreme outlier among the line's
/// gaps, so split where the gap exceeds both 0.8×font and 3× the line's
/// median gap.
fn split_line_segments(
    chars: &[RawChar],
    baseline_y: f32,
    page_gutter: Option<f32>,
) -> Vec<GeoLine> {
    let mut chars = chars.to_vec();
    chars.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));
    // Font for the gap threshold: median of non-degenerate sizes only
    // (pdfium reports font=1.0 for some space glyphs).
    let real_fonts: Vec<f32> = chars.iter().map(|c| c.font).filter(|&f| f >= 2.0).collect();
    let median_font = median(real_fonts).max(2.0);
    // Median internal gap (word spacing) — a layout separator must stand out
    // from it. A page-level gutter (when detected) forces splits regardless.
    let mut gaps: Vec<f32> = Vec::new();
    for w in chars.windows(2) {
        let g = w[1].x - w[0].right;
        if g > 0.1 {
            gaps.push(g);
        }
    }
    let median_gap = median(gaps);
    let gap_threshold = (median_font * 0.8).max(median_gap * 3.0).max(median_font * 0.22);
    let mut out: Vec<GeoLine> = Vec::new();
    let mut text = String::new();
    let mut x_start = 0.0f32;
    let mut x_end = 0.0f32;
    let mut fonts: Vec<f32> = Vec::new();
    let mut bold_count = 0usize;
    let mut prev_right: Option<f32> = None;

    macro_rules! flush {
        () => {
            if !text.trim().is_empty() {
                let real: Vec<f32> = fonts.iter().copied().filter(|&f| f >= 2.0).collect();
                out.push(GeoLine {
                    text: text.trim().to_string(),
                    x_start,
                    x_end,
                    baseline_y,
                    font_size: median(real),
                    bold: bold_count * 2 > fonts.len(),
                });
            }
            text.clear();
            prev_right = None;
            bold_count = 0;
        };
    }

    for ch in chars.iter() {
        let (x, right, font, c) = (ch.x, ch.right, ch.font, ch.ch);
        if let Some(pr) = prev_right {
            let crosses_gutter = page_gutter
                .map(|g| pr < g && x > g)
                .unwrap_or(false);
            if crosses_gutter || x - pr > gap_threshold {
                flush!();
            }
        }
        if text.is_empty() {
            x_start = x;
        }
        if c.is_whitespace() {
            // Whitespace glyphs are explicit word separators; the gap-based
            // inference only fires for visible glyphs (a space glyph often
            // carries no width, and would otherwise double-insert).
            if !text.ends_with(' ') && !text.is_empty() {
                text.push(' ');
            }
        } else {
            if let Some(pr) = prev_right {
                if x - pr > font * 0.22 && x - pr <= gap_threshold && !text.ends_with(' ') {
                    text.push(' ');
                }
            }
            text.push(c);
        }
        x_end = right.max(x_end);
        fonts.push(font);
        if ch.bold {
            bold_count += 1;
        }
        prev_right = Some(right);
    }
    flush!();
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

/// Detect a column gutter by measuring the empty vertical band between the
/// columns: only "narrow" lines (< 55% page width) take part, so full-width
/// titles / figures / captions can't poison the detection. The gutter is the
/// band midpoint that maximizes (right column's left edge − left column's
/// right edge). Returns None when no real empty band exists or the columns
/// would be badly unbalanced (single-column page with a figure gap).
fn find_gutter(lines: &[GeoLine], page_width: f32, body_font: f32) -> Option<f32> {
    let narrow: Vec<&GeoLine> = lines
        .iter()
        .filter(|l| l.x_end - l.x_start < page_width * 0.55)
        .collect();
    if narrow.len() < 8 {
        return None;
    }
    let band_start = page_width * 0.35;
    let band_end = page_width * 0.65;
    let steps = 40;
    let mut best: Option<(f32, f32)> = None; // (x, empty band width)
    for i in 0..=steps {
        let x = band_start + (band_end - band_start) * i as f32 / steps as f32;
        let left_edge = narrow
            .iter()
            .filter(|l| l.x_end < x)
            .map(|l| l.x_end)
            .fold(0.0f32, f32::max);
        let right_edge = narrow
            .iter()
            .filter(|l| l.x_start > x)
            .map(|l| l.x_start)
            .fold(f32::MAX, f32::min);
        if right_edge == f32::MAX || left_edge == 0.0 {
            continue;
        }
        let band = right_edge - left_edge;
        // A real gutter is at least ~half a font of empty space.
        if band < body_font * 0.5 {
            continue;
        }
        // Prefer the widest empty band; ties go to the x nearest the center.
        let center_penalty = (page_width / 2.0 - x).abs() * 0.001;
        let score = band - center_penalty;
        if best.map(|(_, s)| score > s).unwrap_or(true) {
            best = Some((x, score));
        }
    }
    let (x, _) = best?;
    // Balance check on non-crossing narrow lines: each column must hold a
    // decent share (a lone figure on one side is not a column).
    let left = narrow.iter().filter(|l| l.x_end < x).count();
    let right = narrow.iter().filter(|l| l.x_start > x).count();
    let base = left + right;
    if left >= 4 && right >= 4 && left * 4 >= base && right * 4 >= base {
        Some(x)
    } else {
        None
    }
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

/// Group geometric lines into paragraphs. Each returned paragraph is its
/// lines in reading order — callers that only need text use
/// `lines_to_text`, callers anchoring paragraphs to the page (dual-pane
/// view) use the geometry directly. `gutter_hint` is a page-level column
/// gutter precomputed from the char histogram (see `page_to_lines`); when
/// absent, the line-geometry detector `find_gutter` is used.
pub fn paragraphize(
    lines: Vec<GeoLine>,
    page_width: f32,
    page_height: f32,
    gutter_hint: Option<f32>,
) -> Vec<Vec<GeoLine>> {
    // Drop running heads / page numbers in the top and bottom margins.
    let lines: Vec<GeoLine> = lines
        .into_iter()
        .filter(|l| {
            let in_margin =
                l.baseline_y > page_height * 0.94 || l.baseline_y < page_height * 0.05;
            !(in_margin && l.text.trim().chars().count() < 60)
        })
        .collect();
    if lines.is_empty() {
        return Vec::new();
    }

    let body_font = median(lines.iter().map(|l| l.font_size).collect());
    let gutter = gutter_hint.or_else(|| find_gutter(&lines, page_width, body_font));
    let ordered = reading_order(lines, gutter);

    // Column edges, for indent / short-line tests.
    let col_of = |l: &GeoLine| -> usize {
        match gutter {
            Some(x) if (l.x_start + l.x_end) / 2.0 >= x => 1,
            _ => 0,
        }
    };
    // Column edges for indent / short-line tests. Use the 3rd-smallest /
    // 3rd-largest extreme so margin furniture (ACM-style line numbers) can't
    // stretch the band, then drop lines sitting entirely outside it.
    let mut col_left = [0.0f32; 2];
    let mut col_right = [0.0f32; 2];
    for c in 0..2 {
        let mut xs: Vec<f32> = ordered.iter().filter(|l| col_of(l) == c).map(|l| l.x_start).collect();
        xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mut xe: Vec<f32> = ordered.iter().filter(|l| col_of(l) == c).map(|l| l.x_end).collect();
        xe.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
        col_left[c] = xs.get(2).copied().or_else(|| xs.first().copied()).unwrap_or(0.0);
        col_right[c] = xe.get(2).copied().or_else(|| xe.first().copied()).unwrap_or(0.0);
    }
    let ordered: Vec<GeoLine> = ordered
        .into_iter()
        .filter(|l| {
            let c = col_of(l);
            // Outside the column band entirely (margin line numbers).
            if l.x_end < col_left[c] - body_font || l.x_start > col_right[c] + body_font {
                return false;
            }
            // 1–3 char stragglers hugging the column edges are page furniture:
            // ACM line numbers and the rotated copyright strip (one char per
            // baseline) on first pages.
            let short = l.text.trim().chars().count() <= 3;
            let at_edge = l.x_start > col_right[c] - body_font
                || l.x_end < col_left[c] + body_font;
            !(short && at_edge)
        })
        .collect();

    // Median line pitch (baseline gap) per column, body font only.
    let mut pitch_samples = Vec::new();
    for w in ordered.windows(2) {
        if col_of(&w[0]) == col_of(&w[1])
            && (w[0].font_size - body_font).abs() < body_font * 0.2
            && (w[1].font_size - body_font).abs() < body_font * 0.2
        {
            let gap = w[0].baseline_y - w[1].baseline_y;
            if gap > 0.0 {
                pitch_samples.push(gap);
            }
        }
    }
    let pitch = median(pitch_samples).max(body_font * 1.1);

    let mut paragraphs: Vec<Vec<GeoLine>> = Vec::new();
    let mut current: Vec<GeoLine> = Vec::new();
    let mut prev: Option<&GeoLine> = None;
    for line in &ordered {
        let is_heading = line.font_size > body_font * 1.15 && line.text.trim().chars().count() < 120;
        // Bold short lines are headings too (font-size-only detection misses
        // same-size bold headings — the pdf-struct-chunker approach).
        let is_heading = is_heading
            || (line.bold && line.text.trim().chars().count() < 60 && !line.text.trim().ends_with(['.', '。']));
        let mut new_para = current.is_empty();
        if let Some(p) = prev {
            if col_of(p) != col_of(line) || is_heading || line.font_size > body_font * 1.15 {
                new_para = true;
            } else {
                let gap = p.baseline_y - line.baseline_y;
                let c = col_of(line);
                // First-line indent = THIS line indented while the previous
                // one started flush at the column edge. (Without the flush
                // requirement, a uniformly indented block — e.g. a centered
                // abstract — breaks into one paragraph per line.)
                let prev_flush = p.x_start - col_left[c] <= body_font * 1.2;
                let indent = line.x_start - col_left[c];
                let prev_short = p.x_end < col_right[c] - body_font * 3.0;
                // Strong signals: extra vertical gap, first-line indent.
                // Weak signal: previous line ended short (last line of a
                // paragraph) combined with a normal-to-slightly-open gap.
                if gap > pitch * 1.45
                    || (prev_flush && indent > body_font * 1.2)
                    || (prev_short && gap > pitch * 1.1)
                    || (p.font_size > body_font * 1.15)
                {
                    new_para = true;
                }
            }
        }
        if new_para {
            if !current.is_empty() {
                paragraphs.push(std::mem::take(&mut current));
            }
        }
        current.push(line.clone());
        prev = Some(line);
    }
    if !current.is_empty() {
        paragraphs.push(current);
    }

    paragraphs
}

/// Rebuild paragraph text (paragraphs joined by "\n\n") from geometric lines.
pub fn lines_to_text(
    lines: Vec<GeoLine>,
    page_width: f32,
    page_height: f32,
    gutter_hint: Option<f32>,
) -> String {
    paragraphize(lines, page_width, page_height, gutter_hint)
        .iter()
        .map(|para| {
            para.iter()
                .map(|l| l.text.as_str())
                .fold(String::new(), |acc, l| join_lines(&acc, l))
        })
        .filter(|p| !p.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
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

    const W: f32 = 600.0;
    const H: f32 = 800.0;

    #[test]
    fn breaks_on_vertical_gap() {
        let lines = vec![
            line("First paragraph line one.", 60.0, 300.0, 700.0, 10.0),
            line("second line continues.", 60.0, 300.0, 688.0, 10.0),
            line("Second paragraph starts.", 60.0, 300.0, 668.0, 10.0), // 20pt gap vs 12pt pitch
        ];
        let text = lines_to_text(lines, W, H, None);
        assert_eq!(
            text,
            "First paragraph line one. second line continues.\n\nSecond paragraph starts."
        );
    }

    #[test]
    fn breaks_on_first_line_indent() {
        let lines = vec![
            line("A full first line of a paragraph that ends here.", 60.0, 540.0, 700.0, 10.0),
            line("New paragraph is indented.", 75.0, 300.0, 688.0, 10.0),
        ];
        let text = lines_to_text(lines, W, H, None);
        assert!(text.contains("\n\nNew paragraph is indented."));
    }

    #[test]
    fn two_columns_read_in_column_order() {
        // Left column two lines, right column two lines; y interleaved.
        let lines = vec![
            line("left one", 60.0, 250.0, 700.0, 10.0),
            line("right one", 340.0, 540.0, 700.0, 10.0),
            line("left two", 60.0, 250.0, 688.0, 10.0),
            line("right two", 340.0, 540.0, 688.0, 10.0),
            line("left three", 60.0, 250.0, 676.0, 10.0),
            line("right three", 340.0, 540.0, 676.0, 10.0),
            line("left four", 60.0, 250.0, 664.0, 10.0),
            line("right four", 340.0, 540.0, 664.0, 10.0),
        ];
        let text = lines_to_text(lines, W, H, None);
        let li = text.find("left one").unwrap();
        let ri = text.find("right one").unwrap();
        assert!(li < ri, "left column must come first: {text:?}");
        assert!(text.contains("left two"));
        assert!(text.contains("right three"));
    }

    #[test]
    fn heading_gets_own_paragraph() {
        let lines = vec![
            line("Body text before.", 60.0, 300.0, 700.0, 10.0),
            line("1. Introduction", 60.0, 200.0, 680.0, 16.0),
            line("Body text after.", 60.0, 300.0, 664.0, 10.0),
        ];
        let text = lines_to_text(lines, W, H, None);
        assert!(text.contains("\n\n1. Introduction\n\n"), "{text:?}");
    }

    #[test]
    fn hyphen_and_cjk_joins() {
        let hyph = lines_to_text(
            vec![
                line("depen-", 60.0, 540.0, 700.0, 10.0),
                line("dencies continue", 60.0, 300.0, 688.0, 10.0),
            ],
            W,
            H,
            None,
        );
        assert_eq!(hyph, "dependencies continue");

        let cjk = lines_to_text(
            vec![
                line("这是一段中文的", 60.0, 540.0, 700.0, 10.0),
                line("第二行内容", 60.0, 300.0, 688.0, 10.0),
            ],
            W,
            H,
            None,
        );
        assert_eq!(cjk, "这是一段中文的第二行内容");
    }

    #[test]
    fn full_width_title_does_not_break_two_columns() {
        // A wide title/caption line must not defeat gutter detection (the
        // demo2 failure mode: first-page figure spans both columns).
        let mut lines = vec![line(
            "A Very Long Full-Width Paper Title Spanning Most of the Page",
            80.0, 520.0, 720.0, 16.0,
        )];
        for i in 0..5 {
            lines.push(line("left column text", 60.0, 260.0, 680.0 - i as f32 * 12.0, 10.0));
            lines.push(line("right column text", 340.0, 540.0, 680.0 - i as f32 * 12.0, 10.0));
        }
        let text = lines_to_text(lines, W, H, None);
        let li = text.find("left column").unwrap();
        let ri = text.find("right column").unwrap();
        assert!(li < ri, "columns must not interleave: {text:?}");
    }

    #[test]
    fn margin_noise_dropped() {        let lines = vec![
            line("Journal Header 123", 60.0, 300.0, 790.0, 9.0), // top margin
            line("42", 290.0, 310.0, 20.0, 9.0),                 // page number
            line("Real body text.", 60.0, 300.0, 700.0, 10.0),
        ];
        let text = lines_to_text(lines, W, H, None);
        assert_eq!(text, "Real body text.");
    }

    // ---- /Rotate handling (see PageRotation) ----

    #[test]
    fn rotation_map_matches_poppler_on_a_rotated_table_page() {
        // demo0 p11: media box 595x794 with /Rotate 90 -> display 794x595.
        // The glyph 'T' of the rotated "Table 1 (continued)" sits at content
        // (43.31, 52.27); poppler (display frame, y from the top) puts the word
        // box at x=[52.3, 71.1], y_top=[34.7, 46.5].
        let rot = PageRotation::Degrees90;
        let (dw, dh) = (794.0, 595.0);
        let (x, y) = rot.map_point(43.31, 52.27, dw, dh);
        assert!((x - 52.27).abs() < 0.1, "display x = {x}");
        assert!((y - 551.7).abs() < 0.1, "display y (up) = {y}");
        assert!((dh - y - 43.3).abs() < 0.1, "y from the top = {}", dh - y);
        // 270 is the mirror case: content (x, y) -> (display_w - y, x)
        let (x, y) = PageRotation::Degrees270.map_point(43.31, 52.27, 794.0, 595.0);
        assert!((x - 741.7).abs() < 0.1, "display x = {x}");
        assert!((y - 43.31).abs() < 0.1, "display y = {y}");
        // unrotated / half-turned pages must be left alone
        assert!(!PageRotation::None.needs_frame_remap());
        assert!(!PageRotation::Degrees180.needs_frame_remap());
        assert_eq!(PageRotation::from_degrees(90), PageRotation::Degrees90);
        assert_eq!(PageRotation::from_degrees(-90), PageRotation::Degrees270);
        assert_eq!(PageRotation::from_degrees(0), PageRotation::None);
    }

    #[test]
    fn rotated_glyph_run_collapses_into_one_line() {
        // On a /Rotate 90 page the content frame delivers a whole word as
        // glyphs stacked along +y (this is what used to become one-char lines).
        let rot = PageRotation::Degrees90;
        let (dw, dh) = (794.0, 595.0);
        let chars: Vec<RawChar> = "Table 1"
            .chars()
            .enumerate()
            .map(|(i, c)| {
                let cx = 43.31;
                let cy = 52.27 + i as f32 * 4.0;
                let (x, y) = rot.map_point(cx, cy, dw, dh);
                let m = rot.map_rect([cx, cy, cx + 8.0, cy + 4.0], dw, dh);
                RawChar { x: if i == 5 { cy } else { m[0] }, y, right: m[2], font: 8.0, ch: c, bold: false }
            })
            .collect();
        let (lines, _gutter) = chars_to_lines(chars, dw);
        assert_eq!(lines.len(), 1, "rotated glyph run must become a single line");
        assert_eq!(lines[0].baseline_y, 595.0 - 43.31);
        assert!(lines[0].text.starts_with("Table"), "{:?}", lines[0].text);
    }

    #[test]
    fn gutter_found_when_only_part_of_the_page_is_two_column() {
        // demo2 p6's shape: a table spanning both columns on top, two columns
        // below. No x is empty across the whole page, so the empty-band and
        // char-histogram detectors both fail; the gap-support detector must not.
        let mut chars: Vec<RawChar> = Vec::new();
        let mut push = |text: &str, x0: f32, y: f32, chars: &mut Vec<RawChar>| {
            let adv = 6.0f32;
            for (i, c) in text.chars().enumerate() {
                let x = x0 + i as f32 * adv;
                chars.push(RawChar { x, y, right: x + adv * 0.8, font: 10.0, ch: c, bold: false });
            }
        };
        for i in 0..8 {
            push(
                "a wide table row that spans the full page width and crosses the gutter",
                60.0,
                760.0 - i as f32 * 14.0,
                &mut chars,
            );
        }
        for i in 0..6 {
            let y = 600.0 - i as f32 * 14.0;
            push("left column body text line here", 60.0, y, &mut chars);
            push("right column body text line here", 320.0, y, &mut chars);
        }
        let (lines, gutter) = chars_to_lines(chars, 600.0);
        let g = gutter.expect("a gutter must be found on a mixed-layout page");
        assert!((g - 300.0).abs() < 45.0, "gutter detected at {g}, expected ~300");
        // No BODY line may still span both columns.
        let merged = lines
            .iter()
            .filter(|l| l.baseline_y < 620.0 && l.x_start < 300.0 && l.x_end > 320.0)
            .count();
        assert_eq!(merged, 0, "body lines were left merged across the gutter");
    }

    /// The per-line slices must add up to exactly the paragraph text — the text
    /// pane renders one node and addresses lines by slicing, so a mismatch would
    /// highlight the wrong span.
    #[test]
    fn line_segments_reproduce_the_joined_text() {
        let cases: Vec<Vec<GeoLine>> = vec![
            vec![
                line("The proposed method achieves", 0.0, 100.0, 100.0, 10.0),
                line("state-of-the-art results on", 0.0, 100.0, 90.0, 10.0),
                line("three benchmarks.", 0.0, 100.0, 80.0, 10.0),
            ],
            // Line-final hyphen continued by a lowercase word.
            vec![
                line("dependencies and depen-", 0.0, 100.0, 100.0, 10.0),
                line("dencies again", 0.0, 100.0, 90.0, 10.0),
            ],
            // CJK: no inter-word space, and a hyphen is NOT a continuation.
            vec![
                line("本文提出了一种基于注意力的", 0.0, 100.0, 100.0, 10.0),
                line("语义分割方法，在多个数据集", 0.0, 100.0, 90.0, 10.0),
                line("上取得最优结果。", 0.0, 100.0, 80.0, 10.0),
            ],
            vec![
                line("A hyphen at the end of a line-", 0.0, 100.0, 100.0, 10.0),
                line("and a capitalised continuation", 0.0, 100.0, 90.0, 10.0),
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
                "每行长度之和必须等于段落文本长度: {text:?}"
            );
            assert_eq!(
                text,
                old_fold.trim_end(),
                "case {ci}: 行切片拼起来必须与原来的 join_lines 折叠结果一致\n  旧: {old_fold:?}\n  新: {text:?}"
            );
            // Slicing by the reported lengths lands on line boundaries.
            let units: Vec<u16> = text.encode_utf16().collect();
            let mut offset = 0usize;
            for (i, len) in lengths.iter().enumerate() {
                let piece = String::from_utf16_lossy(&units[offset..offset + len]);
                let head = lines[i].text.trim().chars().next().unwrap_or(' ');
                if !piece.is_empty() {
                    assert!(
                        piece.starts_with(head) || piece.starts_with(' '),
                        "第 {i} 行切片应以该行首字符开头，实际 {piece:?}"
                    );
                }
                offset += len;
            }
        }
    }
}
