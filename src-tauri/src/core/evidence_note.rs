//! Convert an agent reply's footnote citations into the same citation form the
//! AI 综述 uses: **inline `[n]` links + a `## 参考文献` list**.
//!
//! The 文献分析 agent answers with `[^1]` markers plus a trailing
//! ```evidence JSON block (see `LITERATURE_ANALYZER_PROMPT`). That is a chat
//! format; a note must not carry it. Instead of making the model hand-build
//! URLs (fragile, and it cannot know other papers' ids), the conversion happens
//! here — deterministically, with database access for the bibliography.
//!
//! Rules:
//! * one citation number per **paper**, ordered by first appearance — the same
//!   semantics as a 综述 note, so a note looks the same however it was produced;
//! * each marker links to its OWN passage (page + quote), so two sentences
//!   citing the same paper still point at their own paragraphs;
//! * a marker with no evidence entry is dropped — no fake citations;
//! * when no paper can be resolved at all (no context paper, no `paper` fields)
//!   the markers survive as plain `[n]` text and no bibliography is written.

use serde::Deserialize;
use sqlx::SqlitePool;

/// One `[^n]` citation's evidence, as emitted by the agent.
#[derive(Debug, Clone, Deserialize)]
struct RawItem {
    n: usize,
    #[serde(default)]
    page: Option<PageField>,
    #[serde(default)]
    exact: String,
    /// Paper id — required when the citation is not about the context paper.
    #[serde(default, alias = "paper_id")]
    paper: Option<String>,
}

/// Models emit the page as a number or a string ("3").
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum PageField {
    Num(i32),
    Text(String),
}

impl PageField {
    fn value(&self) -> Option<i32> {
        match self {
            PageField::Num(n) => Some(*n),
            PageField::Text(t) => t.trim().parse().ok(),
        }
    }
}

#[derive(Debug, Clone)]
struct Item {
    n: usize,
    page: Option<i32>,
    exact: String,
    paper: Option<String>,
}

/// Escape control characters that sit INSIDE JSON strings — models emit raw
/// newlines/tabs in `exact` quotes even though JSON forbids them.
fn sanitize_json(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    let mut escaped = false;
    for c in text.chars() {
        if in_string {
            if escaped {
                out.push(c);
                escaped = false;
                continue;
            }
            match c {
                '\\' => {
                    out.push(c);
                    escaped = true;
                }
                '"' => {
                    out.push(c);
                    in_string = false;
                }
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                _ => out.push(c),
            }
        } else {
            if c == '"' {
                in_string = true;
            }
            out.push(c);
        }
    }
    out
}

/// Split the trailing ```evidence block off the reply. An unterminated block
/// (still streaming) is stripped too: by contract it is metadata and always
/// comes last.
fn parse_evidence_block(content: &str) -> (String, Vec<Item>) {
    let Some(start) = content.find("```evidence") else {
        return (content.to_string(), Vec::new());
    };
    let body = content[..start].trim_end().to_string();
    let after_fence = start + "```evidence".len();
    // JSON starts on the line after the opening fence.
    let Some(nl) = content[after_fence..].find('\n') else {
        return (body, Vec::new());
    };
    let json_start = after_fence + nl + 1;
    let json = match content[json_start..].find("```") {
        Some(i) => &content[json_start..json_start + i],
        None => &content[json_start..],
    };
    let parsed: Vec<RawItem> = serde_json::from_str(&sanitize_json(json)).unwrap_or_default();
    let items = parsed
        .into_iter()
        .filter(|r| !r.exact.trim().is_empty())
        .map(|r| Item {
            n: r.n,
            page: r.page.as_ref().and_then(|p| p.value()),
            exact: r.exact.trim().to_string(),
            paper: r.paper.map(|p| p.trim().to_string()).filter(|p| !p.is_empty()),
        })
        .collect();
    (body, items)
}

/// strip GFM footnote definitions the model may have written by hand
/// (`[^1]: [第 3 页](…) · 「原文」`) — they are replaced by the bibliography.
fn strip_footnote_definitions(body: &str) -> String {
    body.lines()
        .filter(|line| {
            let t = line.trim_start();
            let Some(rest) = t.strip_prefix("[^") else {
                return true;
            };
            let Some(colon) = rest.find("]:") else {
                return true;
            };
            let label = &rest[..colon];
            // `[^3]: value` is a definition; `[^3] text` is not.
            !(!label.is_empty() && label.chars().all(|c| c.is_ascii_digit()))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn heading_text(line: &str) -> Option<&str> {
    let t = line.trim();
    if !t.starts_with('#') {
        return None;
    }
    Some(t.trim_start_matches('#').trim())
}

/// Drop a model-written 参考文献 section before the generated one is appended.
///
/// The prompt may tell the model not to write its own bibliography, but prompts
/// are not a guarantee: the converter appends one unconditionally, so a
/// compliant-looking reply that also lists references would end up with two
/// sections. Only a trailing section is stripped — a 参考文献 heading followed by
/// another heading is the user's own content and is left alone.
fn strip_existing_reference_section(body: &str) -> String {
    let lines: Vec<&str> = body.lines().collect();
    let Some(start) = lines
        .iter()
        .rposition(|l| heading_text(l) == Some("参考文献"))
    else {
        return body.to_string();
    };
    if lines[start + 1..].iter().any(|l| heading_text(l).is_some()) {
        return body.to_string();
    }
    lines[..start].join("\n").trim_end().to_string()
}

struct Linked {
    /// Reply body with citations rewritten to inline links.
    body: String,
    /// Cited passages in number order — the 引用出处 list.
    citations: Vec<(usize, Item)>,
    /// Distinct paper ids in first-citation order — the 参考文献 list.
    papers: Vec<String>,
}

fn reader_url(paper_id: &str, page: Option<i32>, exact: &str) -> String {
    let encoded = crate::core::review_gen::percent_encode(exact);
    match page {
        Some(p) => format!("siku-reader://{paper_id}?page={p}&exact={encoded}"),
        None => format!("siku-reader://{paper_id}?exact={encoded}"),
    }
}

/// Rewrite `[^n]` markers into `[[k]](siku-reader://…)` where `k` is the
/// paper's citation number.
fn linkify_reply(body: &str, items: &[Item], default_paper: Option<&str>) -> Linked {
    // Numbering is PER CITATION, not per paper: a single-paper analysis cites
    // one source 20+ times, and re-numbering by paper collapsed all of them to
    // `[1]` — the reader lost which passage each marker meant. The marker's own
    // `n` is already the identity of the evidence entry, so it is kept.
    let mut citations: Vec<(usize, Item)> = Vec::new();
    let mut papers: Vec<String> = Vec::new();
    let chars: Vec<char> = body.chars().collect();
    let mut out = String::with_capacity(body.len() + 64);
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '[' && i + 1 < chars.len() && chars[i + 1] == '^' {
            let mut j = i + 2;
            let mut digits = String::new();
            while j < chars.len() && chars[j].is_ascii_digit() && digits.len() < 4 {
                digits.push(chars[j]);
                j += 1;
            }
            if !digits.is_empty() && j < chars.len() && chars[j] == ']' {
                if let Ok(n) = digits.parse::<usize>() {
                    let Some(item) = items.iter().find(|it| it.n == n) else {
                        // No evidence at all: drop the marker (never a fake citation).
                        i = j + 1;
                        continue;
                    };
                    let paper = item
                        .paper
                        .clone()
                        .or_else(|| default_paper.map(|p| p.to_string()));
                    match &paper {
                        Some(pid) => {
                            if !papers.iter().any(|p| p == pid) {
                                papers.push(pid.clone());
                            }
                            let url = reader_url(pid, item.page, &item.exact);
                            out.push_str(&format!("[[{}]]({})", n, url));
                        }
                        // Cited but no paper to point at: keep the number only.
                        None => out.push_str(&format!("[{n}]")),
                    }
                    if !citations.iter().any(|(x, _)| *x == n) {
                        citations.push((n, item.clone()));
                    }
                    i = j + 1;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    citations.sort_by_key(|(n, _)| *n);
    Linked { body: out.trim_end().to_string(), citations, papers }
}

/// Rows fetched for the bibliography.
struct Meta {
    id: String,
    title: String,
    authors: String,
    year: Option<i64>,
    journal: Option<String>,
    doi: Option<String>,
    url: Option<String>,
}

async fn load_meta(db: &SqlitePool, ids: &[String]) -> Result<Vec<Meta>, String> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let ph = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT id, title, authors, year, journal, doi, url FROM papers WHERE id IN ({ph})"
    );
    let mut q = sqlx::query_as::<_, (String, String, String, Option<i64>, Option<String>, Option<String>, Option<String>)>(&sql);
    for id in ids {
        q = q.bind(id);
    }
    let rows = q.fetch_all(db).await.map_err(|e| format!("db: {e}"))?;
    let mut map: std::collections::HashMap<String, Meta> = rows
        .into_iter()
        .map(|(id, title, authors, year, journal, doi, url)| {
            (
                id.clone(),
                Meta { id, title, authors, year, journal, doi, url },
            )
        })
        .collect();
    Ok(ids.iter().filter_map(|id| map.remove(id)).collect())
}

/// Convert an agent reply into note Markdown: inline `[n]` citations that jump
/// straight to the evidence passage, plus a `## 参考文献` list.
pub async fn convert(
    db: &SqlitePool,
    content: &str,
    default_paper: Option<&str>,
) -> Result<String, String> {
    let (body, items) = parse_evidence_block(content);
    let body = strip_footnote_definitions(&body);
    let body = strip_existing_reference_section(&body);
    if items.is_empty() {
        return Ok(body.trim_end().to_string());
    }
    let linked = linkify_reply(&body, &items, default_paper);
    if linked.citations.is_empty() {
        return Ok(linked.body);
    }

    let cited: Vec<String> = linked.papers.clone();
    let metas = load_meta(db, &cited).await?;
    let title_of = |pid: &str| -> String {
        metas
            .iter()
            .find(|m| m.id == pid)
            .map(|m| m.title.clone())
            .unwrap_or_else(|| "（未知文献）".to_string())
    };

    let mut out = linked.body;

    // 引用出处: one line per citation, numbered exactly like the body so `[17]`
    // still means "the 17th piece of evidence" in a single-source note.
    out.push_str("\n\n## 引用出处\n\n");
    for (n, item) in &linked.citations {
        let page = item.page.map(|p| format!("第 {p} 页 · ")).unwrap_or_default();
        let paper = item
            .paper
            .clone()
            .or_else(|| default_paper.map(|p| p.to_string()))
            .map(|pid| format!("《{}》 · ", title_of(&pid)))
            .unwrap_or_default();
        out.push_str(&format!("[{n}] {page}{paper}「{}」\n\n", item.exact));
    }

    // 参考文献: the distinct sources, deliberately unnumbered — a numbered list
    // here would suggest `[1]`..`[23]` are 23 different papers.
    if !cited.is_empty() {
        out.push_str("## 参考文献\n\n");
        for pid in &cited {
            let line = match metas.iter().find(|m| &m.id == pid) {
                Some(m) => crate::core::review_gen::bibliography_entry(
                    &m.authors,
                    &m.title,
                    m.journal.as_deref(),
                    m.year,
                    m.doi.as_deref(),
                    m.url.as_deref(),
                ),
                // The paper row is gone: keep the note honest.
                None => "（文献已删除）".to_string(),
            };
            out.push_str(&format!("- {line}\n"));
        }
    }
    Ok(out.trim_end().to_string())
}
#[cfg(test)]
mod tests {
    use super::*;

    fn item(n: usize, page: i32, exact: &str, paper: Option<&str>) -> Item {
        Item {
            n,
            page: Some(page),
            exact: exact.into(),
            paper: paper.map(|p| p.into()),
        }
    }

    #[test]
    fn parses_a_fenced_block_and_strips_it() {
        let content = "结论成立[^1]。\n\n```evidence\n[{\"n\":1,\"page\":3,\"exact\":\"原文片段\"}]\n```\n";
        let (body, items) = parse_evidence_block(content);
        assert!(!body.contains("evidence"), "{body}");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].page, Some(3));
        assert_eq!(items[0].exact, "原文片段");
    }

    #[test]
    fn tolerates_raw_newlines_inside_the_quote() {
        // Models emit literal newlines inside JSON strings; strict parsers die.
        let content = "结论[^1]。\n```evidence\n[{\"n\":1,\"page\":\"5\",\"exact\":\"第一行\n第二行\"}]\n```";
        let (_, items) = parse_evidence_block(content);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].page, Some(5));
        assert!(items[0].exact.contains("第一行"), "{:?}", items[0].exact);
    }

    #[test]
    fn markers_become_inline_links_numbered_per_paper() {
        let body = "甲说[^1]。乙说[^2]。又说[^1]。";
        let items = [
            item(1, 3, "证据一", Some("p1")),
            item(2, 9, "证据二", Some("p2")),
        ];
        let linked = linkify_reply(body, &items, None);
        assert_eq!(linked.papers, vec!["p1".to_string(), "p2".to_string()]);
        assert_eq!(linked.body.matches("[[1]]").count(), 2, "{}", linked.body);
        assert_eq!(linked.body.matches("[[2]]").count(), 1, "{}", linked.body);
        // Different passages of the same paper keep their own page.
        assert!(linked.body.contains("page=3"), "{}", linked.body);
        assert!(linked.body.contains("page=9"), "{}", linked.body);
    }

    #[test]
    fn falls_back_to_the_context_paper() {
        let body = "结论[^1][^2]。";
        let items = [item(1, 4, "片段一", None), item(2, 7, "片段二", None)];
        let linked = linkify_reply(body, &items, Some("ctx"));
        assert_eq!(linked.papers, vec!["ctx".to_string()]);
        // Same paper twice → one number, two different anchors.
        assert!(linked.body.contains("page=4"), "{}", linked.body);
        assert!(linked.body.contains("page=7"), "{}", linked.body);
        assert!(linked.body.starts_with("结论[[1]](siku-reader://ctx?page=4"), "{}", linked.body);
    }

    #[test]
    fn marker_without_evidence_is_dropped() {
        let body = "结论[^9]成立。";
        let items = [item(1, 3, "证据", Some("p1"))];
        let linked = linkify_reply(body, &items, None);
        assert_eq!(linked.body, "结论成立。");
    }

    #[test]
    fn unknown_paper_keeps_a_plain_number() {
        let body = "结论[^1]。";
        let items = [item(1, 3, "证据", None)];
        let linked = linkify_reply(body, &items, None);
        assert_eq!(linked.body, "结论[1]。");
        // No paper id → nothing to list in 参考文献.
        assert!(linked.papers.is_empty());
    }

    #[test]
    fn hand_written_footnote_definitions_are_removed() {
        let body = "结论[^1]。\n\n[^1]: [第 3 页](siku-reader://p1?page=3&exact=x) · 「原文」\n[^2]: 别的定义";
        let out = strip_footnote_definitions(body);
        assert!(!out.contains("[^1]:"), "{out}");
        assert!(!out.contains("[^2]:"), "{out}");
        assert!(out.contains("结论[^1]。"), "{out}");
    }

    #[test]
    fn model_written_bibliography_is_replaced_not_duplicated() {
        // The old prompt told the model to hand-write footnote definitions, so a
        // reply may still carry its own reference block. It must not survive next
        // to the generated one.
        let body = "结论[^1]。\n\n## 参考文献\n\n[1] 张三. 某研究. 学报, 2020.";
        let out = strip_existing_reference_section(body);
        assert!(!out.contains("参考文献"), "{out}");
        assert!(out.contains("结论[^1]。"), "{out}");
    }

    #[test]
    fn mid_document_reference_heading_is_user_content() {
        let body = "## 参考文献\n\n我自己写的备注\n\n## 后续一节\n\n正文";
        assert_eq!(strip_existing_reference_section(body), body);
    }

    #[test]
    fn body_without_reference_section_is_untouched() {
        let body = "## 摘要\n\n正文[^1]。";
        assert_eq!(strip_existing_reference_section(body), body);
    }

    #[test]
    fn inline_marker_text_is_not_treated_as_a_definition() {
        let body = "结论[^1] 后面还有正文。";
        assert_eq!(strip_footnote_definitions(body), body);
    }
}
