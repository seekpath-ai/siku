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

/// One visual line with its geometry (PDF points; y grows upward).
#[derive(Debug, Clone)]
pub struct GeoLine {
    pub text: String,
    pub x_start: f32,
    pub x_end: f32,
    pub baseline_y: f32,
    pub font_size: f32,
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
        let mut new_para = current.is_empty();
        if let Some(p) = prev {
            if col_of(p) != col_of(line) || is_heading || line.font_size > body_font * 1.15 {
                new_para = true;
            } else {
                let gap = p.baseline_y - line.baseline_y;
                let c = col_of(line);
                let indent = line.x_start - col_left[c];
                let prev_short = p.x_end < col_right[c] - body_font * 3.0;
                // Strong signals: extra vertical gap, first-line indent.
                // Weak signal: previous line ended short (last line of a
                // paragraph) combined with a normal-to-slightly-open gap.
                if gap > pitch * 1.45
                    || indent > body_font * 1.2
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

/// Extract a page's geometric lines from its pdfium text page. Space glyphs
/// are unreliable in PDFs, so inter-word spaces are inferred from x gaps.
///
/// Baseline clustering alone would merge same-y text from DIFFERENT columns
/// into one line, so each cluster is then split at large internal x-gaps —
/// every returned line belongs to exactly one column. Returns the lines plus
/// a page-level column gutter when the char histogram shows one (used to
/// force splits across narrow gutters that per-line thresholds miss).
pub fn page_to_lines(
    text_page: &pdfium_render::prelude::PdfPageText,
    page_width: f32,
) -> (Vec<GeoLine>, Option<f32>) {
    let mut chars: Vec<(f32, f32, f32, f32, char)> = Vec::new(); // x, y, right, font, ch
    for ch in text_page.chars().iter() {
        let Some(c) = ch.unicode_char() else { continue };
        if c.is_control() {
            continue;
        }
        let (Ok(x), Ok(y)) = (ch.origin_x(), ch.origin_y()) else { continue };
        let x = x.value;
        let y = y.value;
        let font = ch.scaled_font_size().value;
        let right = ch
            .loose_bounds()
            .map(|b| b.right().value)
            .unwrap_or(x + font * 0.5);
        chars.push((x, y, right, font, c));
    }
    if chars.is_empty() {
        return (Vec::new(), None);
    }

    // Page-level gutter from the char x-histogram: a true column gutter is a
    // near-empty vertical band along the WHOLE page height, so it stands out
    // even when full-width lines (abstract, title) cross it. Per-line gap
    // thresholds can't reliably catch narrow (IEEE ~1.2 font) gutters.
    let gutter = detect_gutter_chars(&chars, page_width);

    // Cluster into baseline groups (y desc, x asc within a group).
    chars.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
    });
    let mut lines: Vec<GeoLine> = Vec::new();
    let mut group: Vec<(f32, f32, f32, char)> = Vec::new(); // x, right, font, ch
    let mut group_y = 0.0f32;
    let mut group_font = 0.0f32;
    for (x, y, right, font, c) in chars {
        // Group font is the running MAX: pdfium reports degenerate sizes
        // (font=1.0) for some space glyphs, and a first-char space would
        // otherwise collapse the clustering tolerance and split thresholds,
        // shredding the line into per-word pieces.
        let tol = (group_font.max(font) * 0.5).max(1.0);
        let same_line = !group.is_empty() && (y - group_y).abs() <= tol;
        if !same_line {
            if !group.is_empty() {
                lines.extend(split_line_segments(&group, group_y, gutter));
            }
            group.clear();
            group_y = y;
            group_font = font;
        }
        group_font = group_font.max(font);
        group.push((x, right, font, c));
    }
    if !group.is_empty() {
        lines.extend(split_line_segments(&group, group_y, gutter));
    }
    (lines, gutter)
}

/// Page-level column gutter from the char x-histogram: the middle-band
/// position with the fewest characters along the full page height, requiring
/// it to be near-empty AND both sides to hold a decent share of the text.
fn detect_gutter_chars(chars: &[(f32, f32, f32, f32, char)], page_width: f32) -> Option<f32> {
    const BUCKET: f32 = 4.0;
    let buckets = (page_width / BUCKET).ceil() as usize + 1;
    let mut hist = vec![0usize; buckets];
    let mut non_space = 0usize;
    for &(x, _, _, _, c) in chars {
        if c.is_whitespace() {
            continue;
        }
        non_space += 1;
        let b = ((x / BUCKET) as usize).min(buckets - 1);
        hist[b] += 1;
    }
    if non_space < 200 {
        return None; // too little text to speak of columns
    }
    let mut best: Option<(f32, usize)> = None;
    let mut x = page_width * 0.35;
    while x <= page_width * 0.65 {
        let b = ((x / BUCKET) as usize).min(buckets - 1);
        let count = hist[b];
        if best.map(|(_, c)| count < c).unwrap_or(true) {
            best = Some((x, count));
        }
        x += BUCKET;
    }
    let (x, count) = best?;
    // Near-empty band: crossing full-width lines (abstract/title) still leave
    // it far below the column-text density.
    if count > usize::max(4, non_space / 100) {
        return None;
    }
    let left_mass: usize = hist[..((x / BUCKET) as usize)].iter().sum();
    let right_mass: usize = hist[((x / BUCKET) as usize) + 1..].iter().sum();
    if left_mass * 3 < non_space || right_mass * 3 < non_space {
        return None; // badly unbalanced — not columns
    }
    Some(x)
}

/// Split a baseline cluster at large internal x-gaps (column gutters and
/// layout separators). `chars` are (x, right, font, char); they are re-sorted
/// by x here because clustering is y-tolerant and two columns with slightly
/// different baselines arrive out of x order.
///
/// The split threshold is relative, not fixed: IEEE-style papers pack columns
/// with a gutter as narrow as ~1.2 fonts, while justified text can stretch
/// word gaps to ~0.6 fonts. A gutter is an extreme outlier among the line's
/// gaps, so split where the gap exceeds both 0.8×font and 3× the line's
/// median gap.
fn split_line_segments(
    chars: &[(f32, f32, f32, char)],
    baseline_y: f32,
    page_gutter: Option<f32>,
) -> Vec<GeoLine> {
    let mut chars = chars.to_vec();
    chars.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    // Font for the gap threshold: median of non-degenerate sizes only
    // (pdfium reports font=1.0 for some space glyphs).
    let real_fonts: Vec<f32> = chars.iter().map(|c| c.2).filter(|&f| f >= 2.0).collect();
    let median_font = median(real_fonts).max(2.0);
    // Median internal gap (word spacing) — a layout separator must stand out
    // from it. A page-level gutter (when detected) forces splits regardless.
    let mut gaps: Vec<f32> = Vec::new();
    for w in chars.windows(2) {
        let g = w[1].0 - w[0].1;
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
                });
            }
            text.clear();
            prev_right = None;
        };
    }

    for &(x, right, font, c) in chars.iter() {
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
        prev_right = Some(right);
    }
    flush!();
    out
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
    fn margin_noise_dropped() {
        let lines = vec![
            line("Journal Header 123", 60.0, 300.0, 790.0, 9.0), // top margin
            line("42", 290.0, 310.0, 20.0, 9.0),                 // page number
            line("Real body text.", 60.0, 300.0, 700.0, 10.0),
        ];
        let text = lines_to_text(lines, W, H, None);
        assert_eq!(text, "Real body text.");
    }
}
