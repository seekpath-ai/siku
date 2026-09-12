use crate::pdf::extractor::PageText;

/// What kind of content a chunk holds. Lets retrieval prefer prose, keep
/// captions/tables addressable, and label the references tail instead of
/// silently dropping it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockType {
    Prose,
    Heading,
    Caption,
    Reference,
}

impl BlockType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prose => "prose",
            Self::Heading => "heading",
            Self::Caption => "caption",
            Self::Reference => "reference",
        }
    }
}

/// A text chunk for RAG storage.
#[derive(Debug, Clone)]
pub struct ChunkData {
    pub content: String,
    pub page_start: Option<i32>,
    pub page_end: Option<i32>,
    /// Detected section heading this chunk belongs to (e.g. "Methods").
    pub section: Option<String>,
    /// Ancestor chain of the section, " > " separated (e.g.
    /// "Data and system requirements > Technology deployment process").
    pub section_path: Option<String>,
    pub block_type: BlockType,
    /// The chunk belongs to the references/appendix tail. Kept in the index
    /// (it answers "what does this paper cite?") but down-weighted by default.
    pub is_tail: bool,
    pub chunk_index: i32,
    pub token_count: Option<i32>,
}

/// Chunking configuration.
pub struct ChunkConfig {
    /// Target token count per chunk
    pub target_tokens: usize,
    /// Overlap tokens between consecutive chunks
    pub overlap_tokens: usize,
}

impl Default for ChunkConfig {
    fn default() -> Self {
        Self {
            target_tokens: 512,
            overlap_tokens: 64,
        }
    }
}

/// Estimate token count from text.
/// Rough heuristic: English ~4 chars/token, CJK ~1.5 chars/token.
pub fn estimate_tokens(text: &str) -> usize {
    let mut tokens: f32 = 0.0;
    for c in text.chars() {
        if c.is_ascii_alphabetic() || c.is_ascii_digit() {
            // English letters/numbers: ~4 chars per token
            tokens += 0.25;
        } else if c.is_whitespace() || c.is_ascii_punctuation() {
            tokens += 0.1;
        } else {
            // CJK and other wide chars: ~1.5 chars per token
            tokens += 0.67;
        }
    }
    tokens.ceil() as usize
}

/// Split text into paragraphs by double newlines.
fn split_paragraphs(text: &str) -> Vec<String> {
    text.split("\n\n")
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

/// Academic section heading keywords (exact or as prefix), EN + ZH.
const HEADING_KEYWORDS: &[&str] = &[
    "abstract", "introduction", "methods", "methodology", "results", "discussion",
    "conclusion", "conclusions", "references", "related work", "background",
    "experiments", "evaluation", "acknowledgments", "acknowledgements", "appendix",
    "system model", "design", "implementation", "motivation", "problem statement",
    "limitations", "future work", "threat model",
    "摘要", "引言", "方法", "结果", "讨论", "结论", "参考文献", "相关工作", "背景",
    "实验", "评估", "致谢", "附录", "系统模型", "设计", "实现", "局限性",
];

/// Collapse letter-spaced small caps: "A B S T R A C T" → "ABSTRACT",
/// "I NTRODUCTION" → "INTRODUCTION".
///
/// PDF text layers keep small caps as separate glyph runs, so heading keywords
/// used to be missed entirely — and the raw form then leaked into the chunk
/// label and the FTS index. Returns the collapsed string plus whether it took a
/// "hard" collapse (3+ single-letter groups), which callers use to avoid
/// turning arbitrary letter-spaced furniture into headings.
fn collapse_small_caps(s: &str) -> (String, bool) {
    let tokens: Vec<&str> = s.split_whitespace().collect();
    if tokens.len() < 2 {
        return (s.to_string(), false);
    }
    let singles = tokens.iter().filter(|t| t.chars().count() == 1).count();
    // A single letter followed by an uppercase run ("I NTRODUCTION") is the same
    // artifact, and joins the two tokens.
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let cur = tokens[i];
        // Only join when the next token is ALL CAPS: "I NTRODUCTION" and
        // "R EFERENCES" are small-caps artefacts, whereas "A Multi-Agent …" is a
        // normal title whose leading article must stay a separate word.
        let next_upper_run = tokens
            .get(i + 1)
            .map(|n| {
                n.chars().count() >= 2
                    && n.chars().all(|c| c.is_uppercase() || !c.is_alphabetic())
                    && n.chars().any(|c| c.is_alphabetic())
            })
            .unwrap_or(false);
        if cur.chars().count() == 1 && next_upper_run {
            out.push(format!("{cur}{}", tokens[i + 1]));
            i += 2;
            continue;
        }
        out.push(cur.to_string());
        i += 1;
    }
    let hard = singles >= 3;
    (out.join(if hard { "" } else { " " }), hard)
}

/// Heading-level prefix ("3.2 ", "IV. ", "A. ") → the heading body after it.
fn strip_number_prefix(s: &str) -> Option<&str> {
    let (raw_head, rest) = s.split_once(char::is_whitespace)?;
    let rest = rest.trim_start();
    if rest.is_empty() || rest.chars().count() < 2 {
        return None;
    }
    let head = raw_head.trim_end_matches(['.', ')', ':', '：']);
    if head.is_empty() {
        return None;
    }
    // "3." / "3.2" — the digit run itself may be a single digit, so test the
    // raw prefix (before stripping the separator) for the dotted form.
    // "3." / "3.2" / "3" (dotted numbering)
    let is_dotted = raw_head.chars().all(|c| c.is_ascii_digit() || c == '.')
        && raw_head.chars().any(|c| c.is_ascii_digit())
        && (raw_head.contains('.') || raw_head.len() <= 2);
    // "2C." / "3A." (digit + letter, Elsevier style)
    let is_digit_letter = (2..=3).contains(&head.chars().count())
        && head.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false)
        && head.chars().last().map(|c| c.is_ascii_uppercase()).unwrap_or(false)
        && head.chars().all(|c| c.is_ascii_alphanumeric());
    // "IV." (roman numerals, IEEE style)
    let is_roman = (2..=6).contains(&head.chars().count())
        && head.chars().all(|c| matches!(c, 'I' | 'V' | 'X' | 'L' | 'C'));
    // "A." (lettered subsections)
    let is_letter = head.chars().count() == 1 && head.chars().all(|c| c.is_ascii_uppercase());
    if is_dotted || is_digit_letter || is_roman || is_letter {
        // The body must look like a title, not a sentence.
        if rest.chars().next().map(|c| c.is_alphabetic()).unwrap_or(false) {
            return Some(rest);
        }
    }
    None
}

/// Text that is page furniture rather than a heading: line numbers, captions,
/// reference entries, sentence fragments.
fn is_heading_reject(t: &str) -> bool {
    if !t.chars().any(|c| c.is_alphabetic()) {
        return true; // "10", "- 12 -", "|||"
    }
    // Figure/table/equation captions are content, not structure.
    const CAPTION_PREFIXES: &[&str] = &[
        "fig", "figure", "table", "tab", "eq", "equation", "algorithm", "listing", "appendix a.",
    ];
    let lower = t.to_lowercase();
    for p in CAPTION_PREFIXES {
        if let Some(rest) = lower.strip_prefix(p) {
            let rest = rest.trim_start_matches(['.', ')', ':', ' ', '\u{a0}']);
            if lower.starts_with(p) && (rest.starts_with(|c: char| c.is_ascii_digit()) || rest.is_empty() || *p == "figure" || *p == "table") {
                return true;
            }
        }
    }
    if t.starts_with('(') {
        return true; // "(a) A failure scenario ..." figure sub-labels
    }
    if t.ends_with(['.', '。', '!', '！', '?', '？']) || t.contains('。') || t.contains(". ") {
        return true; // prose, or a numbered sentence
    }
    if t.starts_with('[') || t.contains("et al.") || t.contains("(19") || t.contains("(20") {
        return true; // reference entry
    }
    false
}

/// Title case, ALL CAPS, or a single capitalised word — the shape a heading has
/// and a sentence fragment does not.
fn looks_like_title(t: &str) -> bool {
    const FUNCTION_WORDS: &[&str] = &[
        "of", "the", "and", "for", "in", "on", "to", "a", "an", "at", "by", "with", "from", "as", "or", "vs",
    ];
    let words: Vec<&str> = t.split_whitespace().collect();
    if words.is_empty() {
        return false;
    }
    let letters: String = t.chars().filter(|c| c.is_alphabetic()).collect();
    if letters.chars().count() >= 2 && letters.chars().all(|c| c.is_uppercase()) {
        return true; // ALL CAPS heading
    }
    if words.len() == 1 {
        return words[0].chars().next().map(|c| c.is_uppercase()).unwrap_or(false);
    }
    words.iter().all(|w| {
        let first = w.chars().next().unwrap_or(' ');
        first.is_uppercase()
            || FUNCTION_WORDS.contains(&w.to_lowercase().as_str())
            || !first.is_alphabetic()
    })
}

/// Detect whether a paragraph is a section heading and return its heading text.
///
/// Every rule is gated by `is_heading_reject` + `looks_like_title`: without them
/// "any short line" qualified, so ACM line numbers ("10", "11"), figure
/// sub-labels and caption fragments all became section labels (measured: 18 of
/// 50 chunks on demo1).
fn detect_section(text: &str) -> Option<String> {
    let t = text.trim();
    if t.is_empty() || t.chars().count() > 80 {
        return None;
    }
    let (collapsed, hard_collapse) = collapse_small_caps(t);

    // 1. Numbered headings first: their ". " belongs to the number, so the
    //    "looks like prose" rejection must not run before them.
    if let Some(rest) = strip_number_prefix(&collapsed) {
        if !is_heading_reject(rest) {
            return Some(rest.to_string());
        }
    }
    if is_heading_reject(&collapsed) {
        return None;
    }

    // 2. Explicit heading keywords (exact / prefix), only for heading-shaped text.
    let lower = collapsed.to_lowercase();
    let short = collapsed.chars().count() <= 60;
    for kw in HEADING_KEYWORDS {
        let hit = lower == *kw
            || lower.starts_with(&format!("{kw} "))
            || lower.starts_with(&format!("{kw}:"))
            || lower.starts_with(&format!("{kw}："));
        if hit && short && (collapsed == *kw || looks_like_title(&collapsed) || lower == *kw) {
            return Some(collapsed);
        }
    }
    // A hard collapse (3+ letter-spaced glyph groups) is only trusted when a
    // keyword matched above — otherwise it is letter-spaced furniture such as
    // "A R T I C L E I N F O".
    if hard_collapse {
        return None;
    }

    // 3. Standalone short line. Title case ("Tool Usage", "Related Work") or a
    //    short sentence-case heading ("Computational toxicology", "Data and
    //    system requirements"); longer lowercase-heavy lines are prose.
    let words: Vec<&str> = collapsed.split_whitespace().collect();
    let first_upper = collapsed.chars().next().map(|c| c.is_uppercase()).unwrap_or(false);
    let caps = words
        .iter()
        .filter(|w| w.chars().next().map(|c| c.is_uppercase()).unwrap_or(false))
        .count();
    let caps_ratio = caps as f32 / words.len().max(1) as f32;
    if collapsed.chars().count() <= 60
        && first_upper
        && words.len() <= 12
        && (caps_ratio >= 0.5 || words.len() <= 5)
    {
        return Some(collapsed);
    }
    None
}

/// Trailing tokens whose period is NOT a sentence boundary (citations and
/// common scholarly abbreviations). Matched lowercase against the word
/// immediately before a '.'.
const ABBREVIATIONS: &[&str] = &[
    "al", "fig", "figs", "eq", "eqs", "i.e", "e.g", "cf", "vs", "dr", "prof",
    "mr", "mrs", "ms", "st", "no", "vol", "pp", "sec", "dept", "approx",
];

/// Decide whether the '.' at `chars[i]` really ends a sentence. Academic
/// text is full of false positives: "et al. 2025" (abbreviation + citation
/// year), "Fig. 3", "i.e. ...". A real sentence almost always ends before
/// an uppercase letter or a CJK char, so a following lowercase letter or
/// digit means the period belongs to an abbreviation.
fn is_sentence_period(chars: &[char], i: usize) -> bool {
    // Word immediately before the period (may contain inner dots: "i.e").
    let mut start = i;
    while start > 0 && (chars[start - 1].is_ascii_alphabetic() || chars[start - 1] == '.') {
        start -= 1;
    }
    let word: String = chars[start..i].iter().collect::<String>().to_lowercase();
    if ABBREVIATIONS.contains(&word.as_str()) {
        return false;
    }
    // Next real character after the period (skip whitespace/closing quotes).
    let mut j = i + 1;
    while j < chars.len()
        && (chars[j].is_whitespace() || matches!(chars[j], ')' | '"' | '\'' | '」' | '』'))
    {
        j += 1;
    }
    match chars.get(j) {
        // Digit (citation year) or lowercase letter → abbreviation, not a boundary.
        Some(c) if c.is_ascii_digit() || c.is_lowercase() => false,
        // End of text after a period still closes the sentence.
        _ => true,
    }
}

/// Split a paragraph into sentences by common sentence-ending punctuation.
fn split_sentences(text: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        current.push(chars[i]);

        // Sentence endings: .!?。！？ followed by space or end
        if matches!(chars[i], '.' | '!' | '?' | '。' | '！' | '？') {
            let is_boundary = if chars[i] == '.' {
                is_sentence_period(&chars, i)
            } else {
                true
            };
            if is_boundary
                && (i + 1 >= chars.len()
                    || chars[i + 1].is_whitespace()
                    || matches!(chars[i + 1], ')' | '"' | '」' | '』'))
            {
                let trimmed = current.trim().to_string();
                if !trimmed.is_empty() {
                    sentences.push(trimmed);
                }
                current = String::new();
                // Skip whitespace after sentence ending
                while i + 1 < chars.len() && chars[i + 1].is_whitespace() {
                    i += 1;
                }
            }
        }

        i += 1;
    }

    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        sentences.push(trimmed);
    }

    sentences
}

/// Join hyphenated line breaks from PDF extraction
/// ("depen-\ndencies" → "dependencies"). Only joins when the next
/// character is a lowercase letter, so genuine hyphens at line ends
/// (e.g. before a capitalized name) are preserved.
fn dehyphenate(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '-'
            && i + 2 < chars.len()
            && chars[i + 1] == '\n'
            && chars[i + 2].is_lowercase()
        {
            i += 2; // skip "-\n", the lowercase char is emitted next round
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Nesting depth of a heading, from its numbering prefix: "3.2" → 2,
/// "3." → 1, "IV." → 1, "A." → 2, "2C." → 1, no prefix → 1.
fn section_level(text: &str) -> u8 {
    let Some((head, _)) = text.split_once(char::is_whitespace) else {
        return 1;
    };
    let head = head.trim_end_matches(['.', ')', ':', '：']);
    if head.is_empty() {
        return 1;
    }
    if head.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return head.split('.').filter(|p| !p.is_empty()).count().clamp(1, 4) as u8;
    }
    // "2C." — dot-less top level in Elsevier's scheme.
    if (2..=3).contains(&head.chars().count())
        && head.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false)
    {
        return 1;
    }
    if head.chars().count() == 1 && head.chars().all(|c| c.is_ascii_uppercase()) {
        return 2; // "A." subsection under a numbered/roman parent
    }
    1 // roman numerals and plain titles
}

/// A figure/table caption paragraph (kept as its own block type so retrieval
/// can treat it differently from prose).
fn is_caption(text: &str) -> bool {
    let lower = text.trim().to_lowercase();
    let mut head = lower.split_whitespace();
    let Some(first) = head.next() else { return false };
    let first = first.trim_end_matches(['.', ':', ')']);
    if !matches!(first, "fig" | "figure" | "table" | "tab" | "algorithm" | "listing") {
        return false;
    }
    head.next()
        .map(|n| n.trim_end_matches(['.', ':']).chars().next().map(|c| c.is_ascii_digit() || "ivx".contains(c)).unwrap_or(false))
        .unwrap_or(false)
}

/// Find the chunk where the references/appendix tail begins, using the same
/// rule that labels chunks at indexing time.
///
/// PDF text extraction reflows the layout — headings do NOT sit on their own
/// lines (verified against the real chunk store), so this matches an inline
/// heading (`References`, `REFERENCES`, the "R EFERENCES" small-caps artifact,
/// `参考文献`) that is IMMEDIATELY followed by the first reference entry:
/// `[1]` (numeric style), `Adlakha, …` or `Anthropic. 2024` (author-year).
/// Prose mentions ("meme references", "see Appendix B") and TOC lines
/// ("References ......... 59") never match because of the entry requirement.
/// The heading itself is case-insensitive while the entry patterns are not —
/// the all-caps `REFERENCES` heading on demo2 was missed for months because of
/// a case-sensitive match.
pub fn detect_body_end<'a>(chunks: impl Iterator<Item = (i32, &'a str)>) -> Option<i32> {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        const NAME: &str = r"(?:[A-Z][A-Za-z\-']+\s+){0,3}[A-Z][A-Za-z\-']+";
        regex::Regex::new(&format!(
            r"(?:\d{{1,2}}\s+)?(?i:references|bibliography|r eferences|参考文献)\s*(?:\[1\]|{NAME},|{NAME}\.\s*(?:19|20)\d{{2}})"
        ))
        .expect("tail heading regex")
    });
    // Skip chunk 0 (title page) — a false positive there would hide the body.
    for (index, content) in chunks {
        if index > 0 && re.is_match(content) {
            return Some(index);
        }
    }
    None
}

/// Chunk extracted pages into ChunkData for RAG storage.
///
/// Strategy:
/// 1. Join all page texts
/// 2. Split into paragraphs
/// 3. Group paragraphs to approach target_tokens, with overlap_tokens overlap
/// 4. Track page ranges for each chunk
pub fn chunk_pages(pages: &[PageText], config: &ChunkConfig) -> Vec<ChunkData> {
    if pages.is_empty() {
        return Vec::new();
    }

    // Build paragraph list with page tracking + section context.
    struct ParaInfo {
        text: String,
        page: u16,
        section: Option<String>,
        section_path: Option<String>,
        block_type: BlockType,
    }

    let mut paragraphs: Vec<ParaInfo> = Vec::new();
    // Ancestor chain of the current section, by nesting level.
    let mut heading_stack: Vec<(u8, String)> = Vec::new();
    for page_text in pages {
        let paras = split_paragraphs(&dehyphenate(&page_text.text));
        for para in paras {
            if let Some(sec) = detect_section(&para) {
                let level = section_level(&para);
                heading_stack.retain(|(l, _)| *l < level);
                heading_stack.push((level, sec.clone()));
                // The heading paragraph itself is kept as content.
                paragraphs.push(ParaInfo {
                    text: para,
                    page: page_text.page,
                    section: Some(sec),
                    section_path: Some(
                        heading_stack.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join(" > "),
                    ),
                    block_type: BlockType::Heading,
                });
            } else {
                paragraphs.push(ParaInfo {
                    text: para.clone(),
                    page: page_text.page,
                    section: heading_stack.last().map(|(_, t)| t.clone()),
                    section_path: if heading_stack.is_empty() {
                        None
                    } else {
                        Some(heading_stack.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join(" > "))
                    },
                    block_type: if is_caption(&para) { BlockType::Caption } else { BlockType::Prose },
                });
            }
        }
    }

    if paragraphs.is_empty() {
        return Vec::new();
    }

    // Pre-split oversized paragraphs into sentence-packed pieces that each
    // fit the token target. A paragraph that alone exceeds the target is
    // split by sentences and packed greedily — no text is dropped; a single
    // sentence still over the target becomes its own piece (nothing better
    // we can do without cutting mid-sentence). Pieces born from a sentence
    // split are re-joined with a single space so the original paragraph
    // flow is preserved inside a chunk.
    struct Piece {
        text: String,
        page: u16,
        section: Option<String>,
        section_path: Option<String>,
        block_type: BlockType,
        join_with_space: bool,
    }

    let mut pieces: Vec<Piece> = Vec::new();
    for para in &paragraphs {
        if estimate_tokens(&para.text) <= config.target_tokens {
            pieces.push(Piece {
                text: para.text.clone(),
                page: para.page,
                section: para.section.clone(),
                section_path: para.section_path.clone(),
                block_type: para.block_type,
                join_with_space: false,
            });
            continue;
        }
        let mut current = String::new();
        let mut current_tokens = 0usize;
        for sent in split_sentences(&para.text) {
            let sent_tokens = estimate_tokens(&sent);
            if current_tokens > 0 && current_tokens + sent_tokens > config.target_tokens {
                pieces.push(Piece {
                    text: std::mem::take(&mut current),
                    page: para.page,
                    section: para.section.clone(),
                    section_path: para.section_path.clone(),
                    block_type: para.block_type,
                    join_with_space: true,
                });
                current_tokens = 0;
            }
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(&sent);
            current_tokens += sent_tokens;
        }
        if !current.is_empty() {
            pieces.push(Piece {
                text: current,
                page: para.page,
                section: para.section.clone(),
                section_path: para.section_path.clone(),
                block_type: para.block_type,
                join_with_space: true,
            });
        }
    }

    // Group pieces into chunks
    let mut chunks: Vec<ChunkData> = Vec::new();
    let mut chunk_index = 0i32;
    let mut i = 0usize;
    let mut prev_section: Option<String> = None;

    while i < pieces.len() {
        let mut chunk_text = String::new();
        let page_start = pieces[i].page;
        let mut page_end = pieces[i].page;
        let mut token_count = 0usize;
        let mut chunk_section: Option<String> = None;
        let mut chunk_path: Option<String> = None;
        let mut chunk_block = BlockType::Prose;
        let mut j = i;

        while j < pieces.len() {
            let piece_tokens = estimate_tokens(&pieces[j].text);
            if token_count > 0 && token_count + piece_tokens > config.target_tokens {
                break;
            }
            if chunk_section.is_none() {
                chunk_section = pieces[j].section.clone();
            }
            if chunk_path.is_none() {
                chunk_path = pieces[j].section_path.clone();
            }
            // A chunk is a heading chunk only while it is made of headings.
            if j == i {
                chunk_block = pieces[j].block_type;
            } else if chunk_block != pieces[j].block_type {
                chunk_block = BlockType::Prose;
            }
            if !chunk_text.is_empty() {
                chunk_text.push_str(if pieces[j].join_with_space { " " } else { "\n\n" });
            }
            chunk_text.push_str(&pieces[j].text);
            token_count += piece_tokens;
            page_end = pieces[j].page;
            j += 1;
        }

        // Prepend the section title when the chunk enters a new section so the
        // FTS index and the embedding carry section context. Plain text (no
        // "##" marker): the marker used to leak into the indexed content.
        if let Some(sec) = &chunk_section {
            if prev_section.as_deref() != Some(sec.as_str()) && chunk_block != BlockType::Heading {
                chunk_text = format!("{sec}\n\n{}", chunk_text.trim());
            }
        }
        prev_section = chunk_section.clone();

        chunks.push(ChunkData {
            content: chunk_text.trim().to_string(),
            page_start: Some(page_start as i32),
            page_end: Some(page_end as i32),
            section: chunk_section,
            section_path: chunk_path,
            block_type: chunk_block,
            is_tail: false,
            chunk_index,
            token_count: Some(token_count as i32),
        });

        chunk_index += 1;

        if j <= i {
            // No progress made (shouldn't happen but guard against infinite loop)
            break;
        }

        // Rewind for overlap: walk back from the end of this chunk while the
        // pieces fit within overlap_tokens. `start` is the first piece of
        // the overlap run; when nothing fits, advance to `j` (no overlap).
        // The `start > i` guard also guarantees forward progress.
        if j < pieces.len() && config.overlap_tokens > 0 {
            let mut overlap = 0usize;
            let mut start = j;
            while start > i {
                let t = estimate_tokens(&pieces[start - 1].text);
                if overlap + t > config.overlap_tokens {
                    break;
                }
                overlap += t;
                start -= 1;
            }
            i = if start > i { start } else { j };
        } else {
            i = j;
        }
    }

    // Label — never drop — the references/appendix tail. A wrong boundary then
    // costs a down-weight instead of making 60% of a paper unreadable, which is
    // what the previous "truncate by chunk_index" behaviour did.
    if let Some(end) = detect_body_end(chunks.iter().map(|c| (c.chunk_index, c.content.as_str()))) {
        for chunk in chunks.iter_mut() {
            if chunk.chunk_index >= end {
                chunk.is_tail = true;
                if chunk.block_type == BlockType::Prose {
                    chunk.block_type = BlockType::Reference;
                }
            }
        }
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_tokens_english() {
        // "Hello world" is ~10 chars, English ~4 chars/token => ~3 tokens
        let tokens = estimate_tokens("Hello world");
        assert!(tokens >= 2 && tokens <= 5);
    }

    #[test]
    fn test_estimate_tokens_chinese() {
        // 10 Chinese chars, ~1.5 chars/token => ~7 tokens
        let tokens = estimate_tokens("你好世界你好世界你好世界");
        assert!(tokens >= 5 && tokens <= 10);
    }

    #[test]
    fn test_estimate_tokens_empty() {
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn test_split_paragraphs() {
        let text = "Para 1\n\nPara 2\n\nPara 3";
        let paras = split_paragraphs(text);
        assert_eq!(paras.len(), 3);
        assert_eq!(paras[0], "Para 1");
    }

    #[test]
    fn test_split_sentences() {
        let text = "Hello world. This is a test! Another sentence?";
        let sentences = split_sentences(text);
        assert!(sentences.len() >= 3);
    }

    #[test]
    fn test_split_sentences_citation_abbreviations() {
        // "et al. 2025" / "Fig. 3" must NOT split — the period belongs to
        // the abbreviation, and the following digit gives the citation year.
        let text = "As shown by Smith et al. 2025 and Fig. 3 the method works. This is real. New sentence here.";
        let sentences = split_sentences(text);
        assert_eq!(sentences.len(), 3);
        assert!(sentences[0].contains("et al. 2025"));
        assert!(sentences[0].contains("Fig. 3"));
    }

    #[test]
    fn test_dehyphenate() {
        assert_eq!(dehyphenate("cross-page depen-\ndencies"), "cross-page dependencies");
        // Uppercase after the break is left alone (genuine hyphen).
        assert_eq!(dehyphenate("Self-\nAttention"), "Self-\nAttention");
    }

    #[test]
    fn test_chunk_pages_small() {
        let pages = vec![PageText {
            page: 1,
            text: "Short text.".to_string(),
        }];
        let config = ChunkConfig::default();
        let chunks = chunk_pages(&pages, &config);
        assert!(!chunks.is_empty());
        assert_eq!(chunks[0].page_start, Some(1));
        assert_eq!(chunks[0].chunk_index, 0);
    }

    #[test]
    fn test_chunk_pages_empty() {
        let chunks = chunk_pages(&[], &ChunkConfig::default());
        assert!(chunks.is_empty());
    }

    #[test]
    fn test_chunk_pages_huge_paragraph_no_hang() {
        // A single paragraph far exceeding the token target must be split
        // into sentence-sized pieces and produce multiple chunks.
        let sentences: Vec<String> = (0..500).map(|n| format!("Sentence number {n} here.")).collect();
        let pages = vec![PageText {
            page: 1,
            text: sentences.join(" "),
        }];
        let chunks = chunk_pages(&pages, &ChunkConfig::default());
        assert!(chunks.len() > 1, "oversized paragraph should be split into multiple chunks");
        let all = chunks.iter().map(|c| c.content.as_str()).collect::<Vec<_>>().join(" ");
        assert!(all.contains("Sentence number 0 here."));
        assert!(all.contains("Sentence number 499 here."));
        // All pages must have been consumed.
        assert_eq!(chunks.iter().map(|c| c.page_end.unwrap_or(0)).max(), Some(1));
    }

    #[test]
    fn test_chunk_pages_oversized_paragraph_loses_no_text() {
        // Regression: sentence-splitting an oversized paragraph used to drop
        // every sentence past the token budget. Every sentence must survive.
        let sentences: Vec<String> = (0..200).map(|n| format!("Unique sentence {n} ends here.")).collect();
        let pages = vec![PageText {
            page: 1,
            text: sentences.join(" "),
        }];
        let chunks = chunk_pages(&pages, &ChunkConfig::default());
        let all = chunks.iter().map(|c| c.content.as_str()).collect::<Vec<_>>().join(" ");
        for n in 0..200 {
            assert!(
                all.contains(&format!("Unique sentence {n} ends here.")),
                "missing sentence {n}"
            );
        }
    }

    #[test]
    fn detects_numbered_roman_and_lettered_headings() {
        assert_eq!(detect_section("3.2 Methods").as_deref(), Some("Methods"));
        assert_eq!(detect_section("3. Results").as_deref(), Some("Results"));
        // IEEE roman numerals — the previous rule rejected them via ". ".
        assert_eq!(detect_section("I. I NTRODUCTION").as_deref(), Some("INTRODUCTION"));
        assert_eq!(detect_section("IV. Conclusion").as_deref(), Some("Conclusion"));
        assert_eq!(detect_section("A. Dataset").as_deref(), Some("Dataset"));
        assert_eq!(detect_section("2C. Accessing competencies").as_deref(), Some("Accessing competencies"));
    }

    #[test]
    fn collapses_letter_spaced_headings() {
        // Small caps arrive as separate glyph runs; without collapsing, the
        // keyword never matched and the raw form leaked into the label.
        assert_eq!(detect_section("A B S T R A C T").as_deref(), Some("ABSTRACT"));
        assert_eq!(detect_section("R EFERENCES").as_deref(), Some("REFERENCES"));
        // ... but letter-spaced furniture that matches no keyword must NOT
        // become a heading.
        assert_eq!(detect_section("A R T I C L E I N F O"), None);
    }

    #[test]
    fn rejects_page_furniture_and_captions() {
        for bad in [
            "10", "11", "18",            // ACM line numbers (18/50 chunks on demo1)
            "- 12 -", "|||",             // page numbers / rules
            "Fig. 3. Agent trajectories of two systems",   // caption
            "TABLE I\nAGENT ROLES AND OUTPUTS",            // table caption
            "(a) A failure scenario where a microservice misses an",
            "AI needs to be fed with adequate data (quality/volume); there are",
            "[24] L. Zhou, J. Bao, and B. Parmanto",
            "as shown in Smith et al. 2025 and Fig. 3 the method works.",
        ] {
            assert_eq!(detect_section(bad), None, "should not be a heading: {bad:?}");
        }
    }

    #[test]
    fn keeps_real_subsection_titles() {
        for good in ["Tool Usage", "Related Work", "Computational toxicology", "Discussion and Conclusion"] {
            assert!(detect_section(good).is_some(), "should be a heading: {good:?}");
        }
    }

    fn body_end_of(contents: &[&str]) -> Option<i32> {
        let owned: Vec<(i32, String)> = contents
            .iter()
            .enumerate()
            .map(|(i, c)| (i as i32, c.to_string()))
            .collect();
        detect_body_end(owned.iter().map(|(i, c)| (*i, c.as_str())))
    }

    #[test]
    fn body_end_numeric_style() {
        assert_eq!(
            body_end_of(&[
                "body text",
                "supported in part by NSF CNS-2145295. References [1] Chaos mesh: A powerful chaos engineering platform",
                "[2] Claude Code by Anthropic",
            ]),
            Some(1)
        );
    }

    #[test]
    fn body_end_author_year_style() {
        assert_eq!(
            body_end_of(&["body", "References Vaibhav Adlakha, Parishad BehnamGhader, Xing Han Lu", "tail"]),
            Some(1)
        );
        assert_eq!(
            body_end_of(&["body", "correspondence to: panlu@stanford.edu. References Anthropic. 2024. Claude 3.5 haiku", "tail"]),
            Some(1)
        );
    }

    #[test]
    fn body_end_matches_all_caps_heading() {
        // Regression: the demo2 paper prints "REFERENCES"; the old pattern was
        // case-sensitive and reported "no references section" for the whole
        // document, so the reference list was read as body text.
        assert_eq!(
            body_end_of(&["body", "future work. REFERENCES [1] R. U. Kothari, A. Pancioli, T. Liu, T. Brott", "more"]),
            Some(1)
        );
    }

    #[test]
    fn body_end_rejects_prose_and_toc() {
        assert_eq!(
            body_end_of(&[
                "Contents: References ......... 59",
                "find reliable sources or references that explain the conversion process",
                "we refer the reader to Appendix A for details",
            ]),
            None
        );
        assert_eq!(body_end_of(&["References [1] bogus match on the title chunk", "real body"]), None);
    }

    #[test]
    fn tail_is_labelled_not_dropped() {
        // The tail stays in the index: a wrong boundary must cost a down-weight,
        // not make the rest of a paper unreachable (the old behaviour truncated
        // pagination at the boundary).
        let pages = vec![PageText {
            page: 1,
            text: format!(
                "{}\n\n5. Conclusion\n\nWe conclude that the system works.\n\nReferences\n\n[1] Smith, J. 2020. A paper about things.\n\n[2] Doe, A. 2021. Another paper.",
                "A long body paragraph about the system and its evaluation. ".repeat(60)
            ),
        }];
        let chunks = chunk_pages(&pages, &ChunkConfig::default());
        assert!(chunks.len() > 1, "expected several chunks, got {}", chunks.len());
        let tail: Vec<&ChunkData> = chunks.iter().filter(|c| c.is_tail).collect();
        assert!(!tail.is_empty(), "references tail must be labelled");
        assert!(
            tail.iter().any(|c| c.content.contains("[2] Doe")),
            "tail content must stay in the index"
        );
        assert!(tail.iter().all(|c| c.block_type == BlockType::Reference));
        assert!(chunks.iter().any(|c| !c.is_tail), "body must not be flagged as tail");
    }
}
