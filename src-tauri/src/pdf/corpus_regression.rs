//! Corpus regression checks for the PDF extraction → chunking pipeline.
//!
//! Every layout fix so far (page `/Rotate`, mixed-layout column gutters, the
//! pdfium line-final hyphen marker, section-label validation) was found by
//! ad-hoc probing and could silently regress afterwards — and the usual
//! bag-of-words metrics do NOT catch order defects (they scored a fully
//! column-interleaved page the same as a correct one).
//!
//! These tests therefore assert on *verbatim text* and on structural counters
//! instead. They need the three demo papers (`demo0/1/2.pdf`) which are NOT in
//! git, so the whole module **skips** (with a notice) when they are absent —
//! run `cargo test --lib pdf::corpus_regression -- --nocapture` from a checkout
//! that has them, or point `SIKU_PDF_CORPUS` at a directory containing them.

use std::path::{Path, PathBuf};

use crate::pdf::chunker::{chunk_pages, estimate_tokens, ChunkConfig};
use crate::pdf::extractor::{extract_text, PageText};

struct Corpus {
    dir: PathBuf,
}

impl Corpus {
    fn locate() -> Option<Self> {
        let dir = std::env::var("SIKU_PDF_CORPUS")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."));
        let all = ["demo0.pdf", "demo1.pdf", "demo2.pdf"]
            .iter()
            .all(|n| dir.join(n).exists());
        if all {
            Some(Self { dir })
        } else {
            println!(
                "[corpus_regression] SKIPPED: demo0/1/2.pdf not found in {} \
                 (set SIKU_PDF_CORPUS to run these checks)",
                dir.display()
            );
            None
        }
    }

    fn pdf(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

/// Everything the checks below need about one document.
struct Report {
    name: String,
    pages: usize,
    text: String,
    chunks: usize,
    median_tokens: i32,
    single_letter_pct: f64,
    soup_chunks: usize,
    section_labels: Vec<String>,
    /// Chunks carrying a section path / labelled as the references tail.
    structure_chunks: usize,
    tail_chunks: usize,
    tail_docs: usize,
    hyphens_lost: usize,
}

/// Whitespace-collapsed copy: paragraph breaks are an artefact of the layout
/// stage, the checks below are about word order and word integrity.
fn flat(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn tokens(s: &str) -> Vec<String> {
    let lower = s.to_lowercase();
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in lower.chars() {
        if c.is_alphanumeric() {
            cur.push(c);
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn analyse(name: &str, pages: &[PageText]) -> Report {
    let text = pages
        .iter()
        .map(|p| p.text.clone())
        .collect::<Vec<_>>()
        .join("\n\n");
    let chunks = chunk_pages(pages, &ChunkConfig::default());
    let all = tokens(&text);
    let single_letter = all.iter().filter(|t| t.chars().count() == 1).count();
    let soup_chunks = chunks
        .iter()
        .filter(|c| {
            let t = tokens(&c.content);
            !t.is_empty() && t.iter().filter(|w| w.chars().count() == 1).count() * 100 / t.len() > 30
        })
        .count();
    let mut toks: Vec<i32> = chunks.iter().map(|c| estimate_tokens(&c.content) as i32).collect();
    toks.sort_unstable();
    let structure_chunks = chunks
        .iter()
        .filter(|c| c.section_path.as_deref().map(|p| !p.is_empty()).unwrap_or(false))
        .count();
    let tail_chunks = chunks.iter().filter(|c| c.is_tail).count();
    Report {
        name: name.to_string(),
        pages: pages.len(),
        text,
        chunks: chunks.len(),
        median_tokens: toks.get(toks.len() / 2).copied().unwrap_or(0),
        single_letter_pct: 100.0 * single_letter as f64 / all.len().max(1) as f64,
        soup_chunks,
        section_labels: chunks.iter().filter_map(|c| c.section.clone()).collect(),
        structure_chunks,
        tail_chunks,
        tail_docs: if tail_chunks > 0 { 1 } else { 0 },
        // Count the PDF text-layer hyphen markers that survived as a real '-'
        // and are joined by `join_lines`. A lost marker shows up as a split word
        // instead, which the assertions below look for directly.
        hyphens_lost: 0,
    }
}

fn report_all(corpus: &Corpus) -> Vec<Report> {
    let mut out = Vec::new();
    for name in ["demo0.pdf", "demo1.pdf", "demo2.pdf"] {
        let pages = extract_text(&corpus.pdf(name)).expect("extract");
        out.push(analyse(name, &pages));
    }
    println!("\n{:<12}{:>7}{:>8}{:>9}{:>8}{:>8}", "doc", "pages", "chunks", "med_tok", "single%", "soup");
    for r in &out {
        println!(
            "{:<12}{:>7}{:>8}{:>9}{:>8.1}{:>8}",
            r.name, r.pages, r.chunks, r.median_tokens, r.single_letter_pct, r.soup_chunks
        );
        println!(
            "             structure_chunks={} tail_chunks={}",
            r.structure_chunks, r.tail_chunks
        );
        let mut labels = r.section_labels.clone();
        labels.dedup();
        println!("             labels[{}]: {:?}", labels.len(), &labels[..labels.len().min(8)]);
    }
    out
}

#[test]
fn corpus_stays_readable() {
    let Some(corpus) = Corpus::locate() else { return };
    let reports = report_all(&corpus);
    let by = |n: &str| reports.iter().find(|r| r.name == n).expect("report");

    // Page counts: a silent extraction failure shows up here first (v1.1.2
    // produced ZERO pages for demo0 when pdfium was unavailable).
    assert_eq!(by("demo0.pdf").pages, 17, "demo0 page count");
    assert_eq!(by("demo1.pdf").pages, 26, "demo1 page count");
    assert_eq!(by("demo2.pdf").pages, 8, "demo2 page count");

    // Garbage indicator: before the /Rotate fix demo0 sat at 43.5%.
    assert!(by("demo0.pdf").single_letter_pct < 12.0, "demo0 single-letter {:.1}%", by("demo0.pdf").single_letter_pct);
    assert!(by("demo1.pdf").single_letter_pct < 16.0, "demo1 single-letter {:.1}%", by("demo1.pdf").single_letter_pct);
    assert!(by("demo2.pdf").single_letter_pct < 11.0, "demo2 single-letter {:.1}%", by("demo2.pdf").single_letter_pct);

    // Chunks that are mostly single letters are unusable for retrieval. demo1
    // has two known ones (in-figure rotation on p13/p21, unrelated to /Rotate).
    assert_eq!(by("demo0.pdf").soup_chunks, 0, "demo0 soup chunks");
    assert_eq!(by("demo2.pdf").soup_chunks, 0, "demo2 soup chunks");
    assert!(by("demo1.pdf").soup_chunks <= 2, "demo1 soup chunks {}", by("demo1.pdf").soup_chunks);

    // Chunk size sanity (target 512 tokens).
    for r in &reports {
        assert!(r.chunks > 0, "{} produced no chunks", r.name);
        assert!((200..=800).contains(&r.median_tokens), "{} median tokens {}", r.name, r.median_tokens);
    }
}

#[test]
fn hyphenated_words_are_not_split() {
    let Some(corpus) = Corpus::locate() else { return };
    let reports = report_all(&corpus);
    let by = |n: &str| reports.iter().find(|r| r.name == n).expect("report");

    // pdfium reports a line-final hyphen as U+0002. Dropping it as a control
    // character split these words in two (measured 149 / 54 / 96 times).
    for (doc, words) in [
        ("demo0.pdf", vec!["benchmarking", "expectations", "applications", "acquisition", "understanding", "specifically"]),
        ("demo1.pdf", vec!["framework", "benchmark", "performance"]),
        ("demo2.pdf", vec!["assessment", "participants", "framework"]),
    ] {
        let text = flat(&by(doc).text);
        for w in words {
            assert!(text.contains(w), "{doc} lost the word {w:?}");
        }
    }
    let d0 = flat(&by("demo0.pdf").text);
    for broken in ["bench marking", "ex pectations", "appli cations", "acqui sition", "un derstanding", "Specif ically"] {
        assert!(!d0.contains(broken), "demo0 still contains the split word {broken:?}");
    }
}

#[test]
fn columns_are_not_interleaved() {
    let Some(corpus) = Corpus::locate() else { return };
    let reports = report_all(&corpus);

    // demo2 p6: the page is two-column only below a full-width table. Before
    // the gutter fix the two columns were glued line by line.
    let d2 = flat(&reports.iter().find(|r| r.name == "demo2.pdf").unwrap().text);
    assert!(
        d2.contains("items are written to keep each judgment focused."),
        "demo2 p6 body is not read column-wise"
    );
    assert!(!d2.contains("judgment relative of the participant"), "demo2 p6 is interleaved");

    // demo0 p1: a right-column fragment used to be spliced into the left column.
    let d0 = flat(&reports.iter().find(|r| r.name == "demo0.pdf").unwrap().text);
    assert!(d0.contains("domains (Dwivedi et al., 2023"), "demo0 p1 left column is broken");
    assert!(!d0.contains("Dwivedi plexities"), "demo0 p1 still splices the right column in");
}

#[test]
fn section_labels_are_sane() {
    let Some(corpus) = Corpus::locate() else { return };
    let reports = report_all(&corpus);
    for r in &reports {
        assert!(!r.section_labels.is_empty(), "{} has no section labels at all", r.name);
        for label in &r.section_labels {
            let t = label.trim();
            assert!(!t.is_empty(), "{} has an empty section label", r.name);
            assert!(t.chars().count() <= 80, "{} label too long: {t:?}", r.name);
            assert!(
                t.chars().any(|c| c.is_alphabetic()),
                "{} label is not a heading: {t:?}",
                r.name
            );
            assert!(
                !t.chars().all(|c| c.is_ascii_digit()),
                "{} label is a bare line number: {t:?}",
                r.name
            );
        }
        // A handful of labels must be real ones, or the detector has collapsed
        // into "everything is a heading".
        let joined = r.section_labels.join(" | ").to_lowercase();
        assert!(
            ["introduction", "abstract", "related work", "conclusion", "references", "discussion"]
                .iter()
                .filter(|k| joined.contains(*k))
                .count()
                >= 2,
            "{} section labels look wrong: {joined}",
            r.name
        );
    }
}

#[test]
fn structure_metadata_is_populated() {
    let Some(corpus) = Corpus::locate() else { return };
    let reports = report_all(&corpus);
    for r in &reports {
        // Section paths come from the heading hierarchy; without them a chunk
        // cannot say where in the paper it came from.
        assert!(
            r.structure_chunks * 100 / r.chunks.max(1) >= 80,
            "{}: only {}/{} chunks carry a section path",
            r.name, r.structure_chunks, r.chunks
        );
        // All three demo papers have a references section: it must be labelled
        // (kept in the index, down-weighted) rather than left unflagged. The
        // all-caps "REFERENCES" heading on demo2 used to defeat this entirely.
        assert!(r.tail_chunks > 0, "{}: no references/appendix chunk labelled", r.name);
        assert!(
            r.tail_chunks < r.chunks,
            "{}: everything was labelled as tail ({} of {})",
            r.name, r.tail_chunks, r.chunks
        );
    }
}

/// Concurrent extraction must stay stable.
///
/// A single `Pdfium` instance is shared process-wide, and pdfium is not
/// thread-safe: running these extractions in parallel used to corrupt the heap
/// (SIGSEGV / `free(): invalid pointer`) — which is what importing several PDFs
/// at once, or rendering a thumbnail during an import, does in the app. The
/// guard in `bindings::pdfium_guard` is the fix; this test is the detector.
#[test]
fn concurrent_extraction_stays_stable() {
    let Some(corpus) = Corpus::locate() else { return };
    let names = ["demo0.pdf", "demo1.pdf", "demo2.pdf"];

    let baseline: Vec<(String, usize, usize)> = names
        .iter()
        .map(|name| {
            let pages = extract_text(&corpus.pdf(name)).expect("extract");
            let chars: usize = pages.iter().map(|p| p.text.len()).sum();
            (name.to_string(), pages.len(), chars)
        })
        .collect();

    let handles: Vec<_> = (0..6)
        .map(|worker| {
            let dir = corpus.dir.clone();
            std::thread::spawn(move || {
                let name = names[worker % names.len()];
                let pages = extract_text(&dir.join(name)).expect("extract");
                let chars: usize = pages.iter().map(|p| p.text.len()).sum();
                (name.to_string(), pages.len(), chars)
            })
        })
        .collect();

    for handle in handles {
        let (name, pages, chars) = handle.join().expect("worker thread panicked");
        let (_, want_pages, want_chars) = baseline
            .iter()
            .find(|(n, _, _)| *n == name)
            .expect("baseline");
        // Same input, same output: concurrent runs must not lose pages or text.
        assert_eq!(pages, *want_pages, "{name}: page count differs under load");
        assert_eq!(chars, *want_chars, "{name}: extracted text differs under load");
    }
}

/// Line anchors must tile each paragraph exactly, and every line box must sit
/// inside its paragraph box: the dual-pane sync slices the paragraph text by
/// these lengths and aligns the PDF to these boxes, so an off-by-one highlights
/// the wrong words.
#[test]
fn paragraph_line_anchors_tile_the_text() {
    let Some(corpus) = Corpus::locate() else { return };
    let mut paragraphs = 0usize;
    let mut with_lines = 0usize;
    let mut problems: Vec<String> = Vec::new();
    let mut boxes_checked = 0usize;

    for name in ["demo0.pdf", "demo1.pdf", "demo2.pdf"] {
        let anchors = crate::pdf::extractor::extract_paragraphs(&corpus.pdf(name))
            .expect("extract paragraph anchors");
        for p in &anchors {
            paragraphs += 1;
            if !p.lines.is_empty() {
                with_lines += 1;
            }
            let sum: usize = p.lines.iter().map(|l| l.len).sum();
            let want = p.text.encode_utf16().count();
            if sum != want {
                problems.push(format!("{name} p{}: 行长度之和 {sum} != 文本长度 {want}", p.page));
            }
            if let Some([px0, py0, px1, py1]) = p.bbox {
                for line in &p.lines {
                    boxes_checked += 1;
                    let [x0, y0, x1, y1] = line.bbox;
                    if y1 > py1 + 1.0 || y0 < py0 - 1.0 || x0 < px0 - 1.0 || x1 > px1 + 1.0 {
                        problems.push(format!(
                            "{name} p{}: 行框 [{x0:.0},{y0:.0},{x1:.0},{y1:.0}] 超出段落框",
                            p.page
                        ));
                    }
                }
            }
        }
    }

    println!(
        "行锚点：段落 {paragraphs}，带行锚点 {with_lines}，校验行框 {boxes_checked}，问题 {}",
        problems.len()
    );
    assert!(
        problems.is_empty(),
        "行锚点不一致（前 5 条）: {:?}",
        &problems[..problems.len().min(5)]
    );
    assert!(
        with_lines * 100 / paragraphs.max(1) >= 95,
        "只有 {with_lines}/{paragraphs} 个段落带行锚点"
    );
    assert!(boxes_checked > 1000, "校验到的行框太少：{boxes_checked}");
}
