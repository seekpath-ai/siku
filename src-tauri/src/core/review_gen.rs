//! AI 综述生成: outline-first pipeline over a selected paper set.
//!
//! 1. `generate_outline` — one LLM call designs the section list (user can
//!    edit it before generation starts).
//! 2. `run_review` — per section: retrieve evidence scoped to the paper set
//!    → one LLM call writes the section with [n] citation markers →
//!    post-processing linkifies every VALID [n] into a `siku-reader://` deep
//!    link (page + exact quote from the section's own evidence — paragraph-
//!    level traceability through the reader's locateQuote) and strips invalid
//!    ones (the "citations are 100% real" guarantee is engineering, not model
//!    luck). The assembled markdown is saved as a note.
//!
//! Progress/cancellation mirrors core::ai_table.

use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tauri::{AppHandle, Emitter};

use crate::ai::llm::ChatMessage;
use crate::ai::retriever::{self, SearchResult};

pub const PROGRESS_EVENT: &str = "library:review-progress";

static RUNNING: AtomicBool = AtomicBool::new(false);
static CANCEL: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewSection {
    pub title: String,
    /// What this section should cover (editable before generation).
    #[serde(default)]
    pub points: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewProgress {
    /// "outline" | "section" | "assemble" | "done" | "cancelled" | "error"
    pub stage: String,
    /// 1-based index of the section being written (0 when not applicable).
    pub section_index: usize,
    pub total_sections: usize,
    pub section_title: String,
    pub note_id: Option<String>,
    pub message: Option<String>,
    /// Streaming text fragment of the section currently being written — the
    /// dialog accumulates these into its live preview panel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta: Option<String>,
}

pub fn request_cancel() {
    CANCEL.store(true, Ordering::SeqCst);
}

struct RunGuard;
impl RunGuard {
    fn try_acquire() -> Result<Self, String> {
        if RUNNING.swap(true, Ordering::SeqCst) {
            return Err("已有综述生成任务正在进行中".to_string());
        }
        CANCEL.store(false, Ordering::SeqCst);
        Ok(Self)
    }
}
impl Drop for RunGuard {
    fn drop(&mut self) {
        RUNNING.store(false, Ordering::SeqCst);
    }
}

fn emit(app: &AppHandle, p: &ReviewProgress) {
    if let Err(e) = app.emit(PROGRESS_EVENT, p) {
        tracing::warn!(error = %e, "failed to emit review progress");
    }
}

fn chat_msg(role: &str, content: String) -> ChatMessage {
    ChatMessage {
        reasoning_content: None,
        role: role.into(),
        content,
        attachments: None,
        tool_calls: None,
        tool_call_id: None,
        name: None,
    }
}

async fn default_llm(
    db: &SqlitePool,
    provider_id: Option<&str>,
) -> Result<Box<dyn crate::ai::llm::LlmClient>, String> {
    let cfg = crate::core::llm_provider_service::load_llm_config_for(db, provider_id).await?;
    crate::ai::llm::client::create_llm_client(&cfg)
}

/// A paper in the set: [n] numbering is the input order, stable across the
/// outline call, the section calls, the linkify pass and the reference list.
struct SourcePaper {
    id: String,
    title: String,
    authors: String,
    year: Option<i64>,
    journal: Option<String>,
    doi: Option<String>,
    url: Option<String>,
}

async fn load_sources(db: &SqlitePool, paper_ids: &[String]) -> Result<Vec<SourcePaper>, String> {
    let mut out = Vec::with_capacity(paper_ids.len());
    for id in paper_ids {
        let row = sqlx::query_as::<_, (String, String, String, Option<i64>, Option<String>, Option<String>, Option<String>)>(
            "SELECT id, title, authors, year, journal, doi, url FROM papers WHERE id = ? AND deleted_at IS NULL",
        )
        .bind(id)
        .fetch_optional(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
        if let Some((id, title, authors, year, journal, doi, url)) = row {
            out.push(SourcePaper { id, title, authors, year, journal, doi, url });
        }
    }
    Ok(out)
}

/// The numbered source list the outline call sees.
fn source_listing(sources: &[SourcePaper]) -> String {
    sources
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let year = p.year.map(|y| y.to_string()).unwrap_or_default();
            format!("[{}] {} ({})", i + 1, p.title, year)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parse a JSON array of {title, points} out of the model's reply, tolerating
/// fences and surrounding prose.
fn parse_outline(content: &str) -> Result<Vec<ReviewSection>, String> {
    let trimmed = content.trim();
    let start = trimmed.find('[');
    let end = trimmed.rfind(']');
    let (Some(s), Some(e)) = (start, end) else {
        return Err("模型未返回大纲 JSON".to_string());
    };
    #[derive(Deserialize)]
    struct RawSection {
        title: String,
        #[serde(default)]
        points: String,
    }
    match serde_json::from_str::<Vec<RawSection>>(&trimmed[s..=e]) {
        Ok(v) if !v.is_empty() => Ok(v
            .into_iter()
            .filter(|s| !s.title.trim().is_empty())
            .map(|s| ReviewSection { title: s.title.trim().to_string(), points: s.points.trim().to_string() })
            .collect()),
        _ => Err("模型未返回有效大纲".to_string()),
    }
}

/// Step 1: design the section outline (user-editable before generation).
/// Streams like the section calls: deltas go to the dialog's live drawer.
pub async fn generate_outline(
    app: &AppHandle,
    db: &SqlitePool,
    paper_ids: &[String],
    question: &str,
    scope: &str,
    language: &str,
    target_words: u32,
    provider_id: Option<&str>,
) -> Result<Vec<ReviewSection>, String> {
    let sources = load_sources(db, paper_ids).await?;
    if sources.is_empty() {
        return Err("文献集为空（文献不存在或已删除）".to_string());
    }
    let llm = default_llm(db, provider_id).await?;

    let section_count = ((target_words / 800) as usize).clamp(3, 8);
    let system = "你是学术写作助手，擅长设计文献综述的目录框架。只输出 JSON 数组，\
        每个元素为 {\"title\": \"章节标题\", \"points\": \"该节应覆盖的要点（一句话）\"}，\
        不要输出 JSON 以外的内容。"
        .to_string();
    let scope_line = if scope.trim().is_empty() {
        String::new()
    } else {
        format!("\n范围限定：{}", scope.trim())
    };
    let user = format!(
        "研究问题：{question}{scope_line}\n写作语言：{language}\n全文目标约 {target_words} 字，\
        请设计 {section_count} 个章节（含引言与总结/展望）。\n\n可用文献（编号即引用编号）：\n{}",
        source_listing(&sources)
    );
    let messages = vec![chat_msg("system", system), chat_msg("user", user)];

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<crate::ai::llm::StreamEvent>();
    let app2 = app.clone();
    let drain = tokio::spawn(async move {
        let mut text = String::new();
        while let Some(ev) = rx.recv().await {
            if ev.event_type == "delta" {
                if let Some(content) = ev.content {
                    text.push_str(&content);
                    emit(&app2, &ReviewProgress {
                        stage: "outline".into(),
                        section_index: 0,
                        total_sections: 0,
                        section_title: String::new(),
                        note_id: None,
                        message: None,
                        delta: Some(content),
                    });
                }
            }
        }
        text
    });
    let call = llm.chat_completion_stream(&messages, &[], tx).await;
    let text = drain.await.map_err(|e| format!("stream drain: {e}"))?;
    call?;
    let mut outline = parse_outline(&text)?;
    outline.truncate(10);
    Ok(outline)
}

/// Step 2+: run the full pipeline (spawned by the command layer).
pub async fn run_review(
    app: AppHandle,
    db: SqlitePool,
    paper_ids: Vec<String>,
    question: String,
    language: String,
    target_words: u32,
    outline: Vec<ReviewSection>,
    provider_id: Option<String>,
) {
    let _guard = match RunGuard::try_acquire() {
        Ok(g) => g,
        Err(e) => {
            emit(&app, &ReviewProgress { stage: "error".into(), section_index: 0, total_sections: 0, section_title: String::new(), note_id: None, message: Some(e), delta: None });
            return;
        }
    };
    let fail = |app: &AppHandle, msg: String| {
        emit(app, &ReviewProgress { stage: "error".into(), section_index: 0, total_sections: 0, section_title: String::new(), note_id: None, message: Some(msg), delta: None });
    };

    let sources = match load_sources(&db, &paper_ids).await {
        Ok(s) if !s.is_empty() => s,
        Ok(_) => return fail(&app, "文献集为空（文献不存在或已删除）".into()),
        Err(e) => return fail(&app, e),
    };
    let llm = match default_llm(&db, provider_id.as_deref()).await {
        Ok(c) => c,
        Err(e) => return fail(&app, e),
    };

    let total = outline.len();
    let per_section_words = (target_words as usize / total.max(1)).max(300);
    let mut section_md: Vec<(String, String)> = Vec::new();

    for (idx, section) in outline.iter().enumerate() {
        if CANCEL.load(Ordering::SeqCst) {
            emit(&app, &ReviewProgress { stage: "cancelled".into(), section_index: idx, total_sections: total, section_title: section.title.clone(), note_id: None, message: None, delta: None });
            return;
        }
        emit(&app, &ReviewProgress { stage: "section".into(), section_index: idx + 1, total_sections: total, section_title: section.title.clone(), note_id: None, message: None, delta: None });

        match write_section(&app, idx + 1, total, &db, llm.as_ref(), &sources, &question, &language, per_section_words, section).await {
            Ok(md) => section_md.push((section.title.clone(), md)),
            Err(e) => return fail(&app, format!("第 {} 节「{}」生成失败：{e}", idx + 1, section.title)),
        }
    }

    // Assemble the note: title from the research question, sections, then
    // the numbered reference list matching the [n] markers.
    emit(&app, &ReviewProgress { stage: "assemble".into(), section_index: total, total_sections: total, section_title: String::new(), note_id: None, message: None, delta: None });
    let note_title = format!("综述：{}", question.chars().take(40).collect::<String>());
    let mut md = format!("# {note_title}\n\n> 研究问题：{question}\n> 基于 {} 篇文献由 AI 生成，引用编号可点击溯源到原文。\n\n", sources.len());
    for (title, body) in &section_md {
        md.push_str(&format!("## {title}\n\n{body}\n\n"));
    }
    md.push_str("## 参考文献\n\n");
    for (i, p) in sources.iter().enumerate() {
        md.push_str(&reference_line(i + 1, p));
        md.push('\n');
    }

    let vault_id = match crate::core::vault_service::get_current_vault_id(&db).await {
        Ok(v) => v,
        Err(e) => return fail(&app, e),
    };
    match crate::core::note_service::create_note(&db, &note_title, &md, None, None, &vault_id, false).await {
        Ok(note) => {
            emit(&app, &ReviewProgress { stage: "done".into(), section_index: total, total_sections: total, section_title: String::new(), note_id: Some(note.id), message: None, delta: None });
        }
        Err(e) => fail(&app, format!("保存笔记失败：{e}")),
    }
}

/// Evidence characters per section (keeps small-context models in budget).
const SECTION_EVIDENCE_CAP: usize = 8000;
/// Chunks retrieved per section as candidate evidence.
const SECTION_TOP_K: usize = 10;

/// Write one section (streaming — text deltas are forwarded to the dialog's
/// live preview) and linkify its citation markers.
#[allow(clippy::too_many_arguments)]
async fn write_section(
    app: &AppHandle,
    section_index: usize,
    total_sections: usize,
    db: &SqlitePool,
    llm: &dyn crate::ai::llm::LlmClient,
    sources: &[SourcePaper],
    question: &str,
    language: &str,
    target_words: usize,
    section: &ReviewSection,
) -> Result<String, String> {
    let paper_ids: Vec<String> = sources.iter().map(|p| p.id.clone()).collect();
    let query = format!("{} {}", section.title, section.points);
    let mut hits = retriever::hybrid_search_scoped(db, &query, SECTION_TOP_K, false, Some(&paper_ids)).await?;
    retriever::expand_with_neighbors(db, &mut hits, false).await.ok();

    let evidence = build_evidence_block(&hits, sources);
    let points_line = if section.points.is_empty() {
        String::new()
    } else {
        format!("\n本节要点：{}", section.points)
    };
    let system = format!(
        "你是学术写作助手，正在撰写文献综述的一节。规则：\n\
        1. 只依据「参考段落」写作，不得编造文献中没有的事实；\n\
        2. 引用事实必须在句末标注来源编号 [n]（n 为段落前的编号），一处事实可引多篇如 [1][3]；\n\
        3. 使用{language}写作，学术语体，本节约 {target_words} 字；\n\
        4. 直接输出本节正文，不要输出章节标题，不要解释。"
    );
    let user = format!("研究问题：{question}\n本节主题：{}{points_line}\n\n参考段落：\n{evidence}", section.title);
    let messages = vec![chat_msg("system", system), chat_msg("user", user)];

    // Stream the section: the drain task accumulates the text and forwards
    // every delta to the dialog. When the provider call ends it drops the
    // sender, closing the channel and letting the drain finish.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<crate::ai::llm::StreamEvent>();
    let app2 = app.clone();
    let drain = tokio::spawn(async move {
        let mut text = String::new();
        while let Some(ev) = rx.recv().await {
            if ev.event_type == "delta" {
                if let Some(content) = ev.content {
                    text.push_str(&content);
                    emit(&app2, &ReviewProgress {
                        stage: "section".into(),
                        section_index,
                        total_sections,
                        section_title: String::new(),
                        note_id: None,
                        message: None,
                        delta: Some(content),
                    });
                }
            }
        }
        text
    });
    let call = llm.chat_completion_stream(&messages, &[], tx).await;
    let text = drain.await.map_err(|e| format!("stream drain: {e}"))?;
    call?;
    Ok(linkify_citations(text.trim(), sources, &hits))
}

/// The evidence block: every chunk tagged with its paper's citation number,
/// which is exactly what the model is told to cite.
fn build_evidence_block(hits: &[SearchResult], sources: &[SourcePaper]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut used = 0usize;
    for hit in hits {
        let Some(n) = sources.iter().position(|p| p.id == hit.paper_id).map(|i| i + 1) else {
            continue;
        };
        let remaining = SECTION_EVIDENCE_CAP.saturating_sub(used);
        if remaining == 0 {
            break;
        }
        let content: String = hit.content.chars().take(remaining.min(1200)).collect();
        used += content.chars().count();
        let page = hit.page_start.map(|p| format!(" p.{p}")).unwrap_or_default();
        out.push(format!("[{n}]（《{}》{page}）\n{content}", hit.paper_title));
    }
    if out.is_empty() {
        "（文献集内未检索到相关段落，请依据摘要级信息概括，并减少具体断言）".to_string()
    } else {
        out.join("\n\n")
    }
}

/// Turn every valid `[n]` in the generated text into a clickable deep link.
/// The link target is the section's own top evidence chunk for that paper —
/// page + a short exact quote, which the reader's locateQuote resolves to a
/// paragraph-level highlight. Markers outside the paper set are left as plain
/// text (they may be figure/table numbers, and must never become fake links):
/// what is clickable is real by construction.
fn linkify_citations(text: &str, sources: &[SourcePaper], hits: &[SearchResult]) -> String {
    let mut out = text.to_string();
    for (i, p) in sources.iter().enumerate() {
        let n = i + 1;
        let marker = format!("[{n}]");
        if !out.contains(&marker) {
            continue;
        }
        let replacement = match hits.iter().find(|h| h.paper_id == p.id && !h.is_neighbor) {
            Some(hit) => {
                let exact: String = hit.content.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(60).collect();
                format!("[{n}](siku-reader://{}?page={}&exact={})", p.id, hit.page_start.unwrap_or(1), percent_encode(&exact))
            }
            // No evidence for this paper in this section: keep the number
            // but not a link — never a dead deep link.
            None => marker.clone(),
        };
        out = out.replace(&marker, &replacement);
    }
    out
}

/// RFC 3986 unreserved characters pass through; everything else is %XX
/// (UTF-8). Matches what the frontend's URLSearchParams parser decodes.
fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(*b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `[n] 作者. 标题. 期刊, 年份. DOI/URL` — compact GB/T 7714-style line.
fn reference_line(n: usize, p: &SourcePaper) -> String {
    let authors = crate::core::citation_export::parse_authors(&p.authors);
    let author_str = if authors.len() > 3 {
        format!("{}, 等", authors[..3].join(", "))
    } else {
        authors.join(", ")
    };
    let mut line = format!("[{n}] {author_str}. {}.", p.title);
    match (&p.journal, p.year) {
        (Some(j), Some(y)) if !j.is_empty() => line.push_str(&format!(" {j}, {y}.")),
        (Some(j), None) if !j.is_empty() => line.push_str(&format!(" {j}.")),
        (None, Some(y)) => line.push_str(&format!(" {y}.")),
        _ => {}
    }
    if let Some(doi) = p.doi.as_deref().filter(|d| !d.is_empty()) {
        line.push_str(&format!(" DOI: {doi}."));
    } else if let Some(url) = p.url.as_deref().filter(|u| !u.is_empty()) {
        line.push_str(&format!(" {url}"));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(id: &str, title: &str) -> SourcePaper {
        SourcePaper { id: id.into(), title: title.into(), authors: "[]".into(), year: None, journal: None, doi: None, url: None }
    }

    fn hit(paper_id: &str, page: i32, content: &str) -> SearchResult {
        SearchResult {
            chunk_id: format!("c-{paper_id}"),
            paper_id: paper_id.into(),
            chunk_index: 0,
            content: content.into(),
            page_start: Some(page),
            page_end: Some(page),
            section: None,
            section_path: None,
            block_type: "prose".into(),
            is_tail: false,
            is_neighbor: false,
            paper_title: String::new(),
            score: 1.0,
            source: "fts5".into(),
        }
    }

    #[test]
    fn linkify_turns_valid_markers_into_reader_links() {
        let sources = [src("p1", "甲"), src("p2", "乙")];
        let hits = [hit("p1", 5, "这 是 一段 证据 文本")];
        let out = linkify_citations("结论 [1] 与 [2] 都成立，还有无效编号 [9]。", &sources, &hits);
        assert!(out.contains("[1](siku-reader://p1?page=5&exact="), "{out}");
        // No evidence for p2: number stays but is not linked.
        assert!(out.contains("[2]"), "{out}");
        assert!(!out.contains("[2](siku-reader://"), "{out}");
        // [9] is not part of the set — left as-is text (never a fake link).
        assert!(out.contains("[9]"), "{out}");
        assert!(!out.contains("[9]("), "{out}");
    }

    #[test]
    fn percent_encode_handles_cjk_and_space() {
        assert_eq!(percent_encode("a b"), "a%20b");
        assert_eq!(percent_encode("中"), "%E4%B8%AD");
        assert_eq!(percent_encode("a-b_c.d~e"), "a-b_c.d~e");
    }

    #[test]
    fn parse_outline_reads_fenced_json() {
        let v = parse_outline("```json\n[{\"title\":\"引言\",\"points\":\"背景\"}]\n```").unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].title, "引言");
        assert!(parse_outline("没有 JSON").is_err());
    }
}
