//! Text analysis for keyword search: script-aware query terms and the CJK
//! bigram index form.
//!
//! Why this exists: the chunk index is FTS5 with the *trigram* tokenizer, which
//! cannot match anything shorter than three characters. Chinese is written
//! without spaces, so a natural-language question arrives as one long run and
//! two-character words like 方法 were unreachable — measured: the query
//! `这篇论文用了什么方法` returned **zero** hits, and `方法` was dropped by the
//! old `>= 3 chars` filter before the search even ran.
//!
//! So the CJK side of the index keeps a *bigram* form of the text
//! (`bigram_index_text`) indexed by a second FTS5 table with the `unicode61`
//! tokenizer: each CJK run becomes overlapping two-character tokens
//! (本文提出 → 本文 文提 提出), which makes two-character words matchable and
//! keeps ordinary BM25 ranking. ASCII keeps the trigram table, which is better
//! at partial-word matches.

use std::collections::HashSet;

/// CJK ideographs, kana and CJK punctuation — the scripts written without
/// word separators.
pub fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{3040}'..='\u{30ff}'   // kana
        | '\u{3400}'..='\u{4dbf}' // CJK ext A
        | '\u{4e00}'..='\u{9fff}' // CJK unified
        | '\u{f900}'..='\u{faff}' // compatibility ideographs
        | '\u{20000}'..='\u{2fa1f}')
}

/// Words that carry no retrieval signal in a question, mostly interrogatives and
/// meta vocabulary ("this paper", "used", "what"). Removed from the *query*
/// only — never from the index.
const STOPWORDS: &[&str] = &[
    // 疑问 / 语气
    "什么", "怎么", "怎样", "如何", "为什么", "为何", "哪个", "哪些", "哪里", "是否", "能否",
    "请问", "吗", "呢", "吧", "请", "告诉我", "介绍", "说明", "讲讲", "说说",
    // 元词
    "这篇", "这篇文章", "本文", "该文", "论文", "文献", "文章", "作者", "研究",
    "主要", "内容", "方面", "相关", "关于", "以及", "并且", "然后", "因此",
    // 结构助词 / 高频虚词
    "的", "了", "是", "在", "和", "与", "或", "有", "被", "把", "对", "为", "中",
    "上", "下", "我们", "他们", "这个", "那个", "一个", "可以", "能够", "进行", "使用",
    "用", "做了", "给出", "提到", "基于", "通过", "根据",
    // 英文（小写比较）
    "the", "a", "an", "of", "is", "are", "was", "were", "to", "in", "on", "for",
    "and", "or", "what", "which", "how", "why", "does", "do", "did", "this", "that",
    "paper", "study", "method", "used", "about", "with", "by", "from", "as", "it",
];

fn is_stopword(term: &str) -> bool {
    let lower = term.to_lowercase();
    STOPWORDS.contains(&lower.as_str())
}

/// Terms extracted from a query: CJK bigrams for the bigram index, ASCII words
/// for the trigram index.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct QueryTerms {
    pub cjk: Vec<String>,
    pub ascii: Vec<String>,
}

impl QueryTerms {
    pub fn is_empty(&self) -> bool {
        self.cjk.is_empty() && self.ascii.is_empty()
    }
}

/// Split a query into index-usable terms.
///
/// * CJK runs are cut into bigrams (a single leftover character is kept as is,
///   unicode61 indexes it as its own token). Stopword bigrams are dropped, which
///   is what turns `这篇论文用了什么方法` into `方法`.
/// * ASCII words are kept when they are at least 3 characters long (shorter ones
///   cannot match the trigram index), lowercased and deduplicated.
pub fn analyze_query(query: &str) -> QueryTerms {
    let mut terms = QueryTerms::default();
    let mut cjk_run = String::new();
    let mut ascii_run = String::new();

    let mut flush_cjk = |run: &mut String, out: &mut Vec<String>| {
        if run.is_empty() {
            return;
        }
        out.extend(cjk_terms(run));
        run.clear();
    };
    let mut flush_ascii = |run: &mut String, out: &mut Vec<String>| {
        if run.chars().count() >= 3 && !is_stopword(run) {
            out.push(run.to_lowercase());
        }
        run.clear();
    };

    for c in query.chars() {
        if is_cjk(c) {
            flush_ascii(&mut ascii_run, &mut terms.ascii);
            cjk_run.push(c);
        } else if c.is_alphanumeric() {
            flush_cjk(&mut cjk_run, &mut terms.cjk);
            ascii_run.push(c);
        } else {
            flush_cjk(&mut cjk_run, &mut terms.cjk);
            flush_ascii(&mut ascii_run, &mut terms.ascii);
        }
    }
    flush_cjk(&mut cjk_run, &mut terms.cjk);
    flush_ascii(&mut ascii_run, &mut terms.ascii);

    dedup(&mut terms.cjk);
    dedup(&mut terms.ascii);
    terms
}

/// Terms for one CJK run: stopwords are removed FIRST (and act as separators),
/// then the remaining segments are cut into bigrams. Cutting first and filtering
/// afterwards would produce straddling bigrams like "篇论" / "么方" from
/// `这篇论文…什么方法`, which match nothing useful and add noise to the OR.
fn cjk_terms(run: &str) -> Vec<String> {
    let chars: Vec<char> = run.chars().collect();
    let mut segments: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut i = 0;
    while i < chars.len() {
        let mut matched = 0;
        for len in (1..=4.min(chars.len() - i)).rev() {
            let cand: String = chars[i..i + len].iter().collect();
            if is_stopword(&cand) {
                matched = len;
                break;
            }
        }
        if matched > 0 {
            if !cur.is_empty() {
                segments.push(std::mem::take(&mut cur));
            }
            i += matched;
        } else {
            cur.push(chars[i]);
            i += 1;
        }
    }
    if !cur.is_empty() {
        segments.push(cur);
    }

    let mut out = Vec::new();
    for seg in segments {
        let cs: Vec<char> = seg.chars().collect();
        if cs.len() == 1 {
            out.push(seg); // unicode61 indexes a lone CJK char as its own token
        } else {
            for w in cs.windows(2) {
                out.push(w.iter().collect());
            }
        }
    }
    out
}

fn dedup(v: &mut Vec<String>) {
    let mut seen = HashSet::new();
    v.retain(|t| seen.insert(t.clone()));
}

/// Index form of a text: CJK runs expanded into space-separated bigrams, ASCII
/// runs kept as words. Feed this to the `unicode61` FTS table.
pub fn bigram_index_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    let mut cjk_run: Vec<char> = Vec::new();

    let mut flush = |run: &mut Vec<char>, out: &mut String| {
        if run.is_empty() {
            return;
        }
        if run.len() == 1 {
            out.push(run[0]);
            out.push(' ');
        } else {
            for w in run.windows(2) {
                out.extend(w.iter());
                out.push(' ');
            }
        }
        run.clear();
    };

    for c in text.chars() {
        if is_cjk(c) {
            cjk_run.push(c);
        } else if c.is_alphanumeric() {
            flush(&mut cjk_run, &mut out);
            out.push(c.to_ascii_lowercase());
        } else {
            flush(&mut cjk_run, &mut out);
            out.push(' ');
        }
    }
    flush(&mut cjk_run, &mut out);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// FTS5 MATCH expression for the ASCII terms against the trigram index:
/// quoted prefix terms joined by OR (`"prehospital"* OR "stroke"*`).
pub fn trigram_match_expr(terms: &[String]) -> Option<String> {
    if terms.is_empty() {
        return None;
    }
    Some(
        terms
            .iter()
            .map(|t| format!("\"{}\"*", t.replace('"', "")))
            .collect::<Vec<_>>()
            .join(" OR "),
    )
}

/// FTS5 MATCH expression for the bigram index. Bigrams are exact tokens there,
/// so no prefix operator is needed; OR keeps recall while BM25 still ranks
/// documents matching more of them higher.
pub fn bigram_match_expr(terms: &[String]) -> Option<String> {
    if terms.is_empty() {
        return None;
    }
    Some(
        terms
            .iter()
            .map(|t| format!("\"{}\"", t.replace('"', "")))
            .collect::<Vec<_>>()
            .join(" OR "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_question_words_and_keeps_content() {
        // The acceptance case: the old implementation sent the whole run as one
        // term and matched nothing.
        let t = analyze_query("这篇论文用了什么方法");
        assert_eq!(t.cjk, vec!["方法".to_string()], "terms: {:?}", t.cjk);
        assert!(t.ascii.is_empty());
    }

    #[test]
    fn bigrams_cover_two_char_words_and_long_terms() {
        let t = analyze_query("注意力机制与语义分割");
        assert!(t.cjk.contains(&"注意力".to_string()) || t.cjk.contains(&"意力".to_string()));
        assert!(t.cjk.contains(&"语义".to_string()), "terms: {:?}", t.cjk);
        assert!(t.cjk.contains(&"分割".to_string()), "terms: {:?}", t.cjk);
        // no term longer than two characters may survive
        assert!(t.cjk.iter().all(|w| w.chars().count() <= 2), "{:?}", t.cjk);
    }

    #[test]
    fn keeps_english_terms_and_drops_short_or_stopword_ones() {
        let t = analyze_query("What is the prehospital stroke scale used for?");
        assert_eq!(t.ascii, vec!["prehospital".to_string(), "stroke".to_string(), "scale".to_string()]);
    }

    #[test]
    fn mixed_script_query_splits_both_ways() {
        let t = analyze_query("StrokeGuard 的阶段恢复机制");
        assert!(t.ascii.contains(&"strokeguard".to_string()));
        assert!(t.cjk.contains(&"阶段".to_string()), "{:?}", t.cjk);
        assert!(t.cjk.contains(&"恢复".to_string()), "{:?}", t.cjk);
    }

    #[test]
    fn index_text_expands_cjk_runs_into_bigrams() {
        assert_eq!(bigram_index_text("本文提出方法"), "本文 文提 提出 出方 方法");
        // ASCII is kept as whole words, case-folded
        assert_eq!(bigram_index_text("BGE-M3 embedding"), "bge m3 embedding");
        // single CJK character survives as its own token
        assert_eq!(bigram_index_text("好"), "好");
    }

    #[test]
    fn match_expressions_are_built_safely() {
        assert_eq!(trigram_match_expr(&["a\"b".into()]).as_deref(), Some("\"ab\"*"));
        assert_eq!(bigram_match_expr(&["方法".into()]).as_deref(), Some("\"方法\""));
        assert_eq!(trigram_match_expr(&[]), None);
    }
}
