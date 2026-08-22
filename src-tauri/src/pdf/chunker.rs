use crate::pdf::extractor::PageText;

/// A text chunk for RAG storage.
#[derive(Debug, Clone)]
pub struct ChunkData {
    pub content: String,
    pub page_start: Option<i32>,
    pub page_end: Option<i32>,
    /// Detected section heading this chunk belongs to (e.g. "3.2 Methods").
    pub section: Option<String>,
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
    "摘要", "引言", "方法", "结果", "讨论", "结论", "参考文献", "相关工作", "背景",
    "实验", "评估", "致谢", "附录",
];

/// Detect whether a paragraph is a section heading and return its heading text.
fn detect_section(text: &str) -> Option<String> {
    let t = text.trim();
    if t.is_empty() || t.chars().count() > 80 {
        return None;
    }
    let lower = t.to_lowercase();
    // 1. Explicit heading keywords (exact / prefix).
    for kw in HEADING_KEYWORDS {
        if lower == *kw
            || lower.starts_with(&format!("{kw} "))
            || lower.starts_with(&format!("{kw}:"))
            || lower.starts_with(&format!("{kw}："))
        {
            return Some(t.to_string());
        }
    }
    // 2. Numbered headings with a dotted prefix: "1.2 Methods", "3. Results".
    let rest = t.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.');
    let prefix_len = t.len() - rest.len();
    if prefix_len > 0 && t[..prefix_len].contains('.') && !rest.trim().is_empty() {
        return Some(rest.trim().to_string());
    }
    // 3. Standalone short title-like line (no sentence-ending punctuation).
    if t.chars().count() <= 60
        && !t.ends_with(['.', '。', '!', '！', '?', '？'])
        && !t.contains('。')
        && !t.contains(". ")
    {
        return Some(t.to_string());
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
    }

    let mut paragraphs: Vec<ParaInfo> = Vec::new();
    let mut current_section: Option<String> = None;
    for page_text in pages {
        let paras = split_paragraphs(&dehyphenate(&page_text.text));
        for para in paras {
            if let Some(sec) = detect_section(&para) {
                current_section = Some(sec);
                // The heading paragraph itself is kept as content.
                paragraphs.push(ParaInfo {
                    text: para,
                    page: page_text.page,
                    section: None,
                });
            } else {
                paragraphs.push(ParaInfo {
                    text: para,
                    page: page_text.page,
                    section: current_section.clone(),
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
        join_with_space: bool,
    }

    let mut pieces: Vec<Piece> = Vec::new();
    for para in &paragraphs {
        if estimate_tokens(&para.text) <= config.target_tokens {
            pieces.push(Piece {
                text: para.text.clone(),
                page: para.page,
                section: para.section.clone(),
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
        let mut j = i;

        while j < pieces.len() {
            let piece_tokens = estimate_tokens(&pieces[j].text);
            if token_count > 0 && token_count + piece_tokens > config.target_tokens {
                break;
            }
            if chunk_section.is_none() {
                chunk_section = pieces[j].section.clone();
            }
            if !chunk_text.is_empty() {
                chunk_text.push_str(if pieces[j].join_with_space { " " } else { "\n\n" });
            }
            chunk_text.push_str(&pieces[j].text);
            token_count += piece_tokens;
            page_end = pieces[j].page;
            j += 1;
        }

        // Prepend the section heading when the chunk enters a new section,
        // so the retrieved text (and the FTS index) carry section context.
        if let Some(sec) = &chunk_section {
            if prev_section.as_deref() != Some(sec.as_str()) {
                chunk_text = format!("## {sec}\n\n{}", chunk_text.trim());
            }
        }
        prev_section = chunk_section.clone();

        chunks.push(ChunkData {
            content: chunk_text.trim().to_string(),
            page_start: Some(page_start as i32),
            page_end: Some(page_end as i32),
            section: chunk_section,
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
}
