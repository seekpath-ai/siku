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
    /// Subsection headings; empty = the section is written as one block.
    #[serde(default)]
    pub subsections: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ReviewProgress {
    /// outline | section | assemble | done | partial | cancelled | error
    pub stage: String,
    /// 1-based index of the section being / last processed (0 = not
    /// applicable). Section-level events keep this coordinate even on failure —
    /// the dialog derives its tick marks from per-section state, never from this
    /// value alone (zeroing it used to wipe every completed tick mark).
    pub section_index: usize,
    pub total_sections: usize,
    pub section_title: String,
    pub note_id: Option<String>,
    pub message: Option<String>,
    /// Streaming text fragment of the section currently being written — the
    /// dialog accumulates these into that section's own buffer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta: Option<String>,
    /// Reasoning fragment (thinking models burn reasoning tokens before any
    /// visible text — without this the preview sits on "等待模型输出" the
    /// whole time). Shown dimmed above the content in the drawer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// running | done | failed — the per-section state the UI trusts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section_status: Option<String>,
    /// Failure text for this section, kept per section so an error in one
    /// section never erases the others' state.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Retries already performed for this section (0 = first attempt).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    /// max_tokens in effect for this attempt (doubles per retry).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Sections with content so far, for the dialog's summary line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub done_sections: Option<usize>,
}

fn base_progress(stage: &str) -> ReviewProgress {
    ReviewProgress { stage: stage.to_string(), ..Default::default() }
}

/// Pipeline-level failure (nothing to retry against).
fn failure_progress(msg: String) -> ReviewProgress {
    ReviewProgress { stage: "error".into(), message: Some(msg), ..Default::default() }
}

/// A per-section state event. `index` is 1-based.
#[allow(clippy::too_many_arguments)]
fn section_progress(
    stage: &str,
    status: &str,
    index: usize,
    total: usize,
    title: &str,
    attempt: u32,
    max_tokens: u32,
    done_sections: Option<usize>,
    error: Option<String>,
) -> ReviewProgress {
    ReviewProgress {
        stage: stage.into(),
        section_index: index,
        total_sections: total,
        section_title: title.to_string(),
        section_status: Some(status.to_string()),
        attempt: Some(attempt),
        max_tokens: Some(max_tokens),
        done_sections,
        error,
        ..Default::default()
    }
}

pub fn request_cancel() {
    CANCEL.store(true, Ordering::SeqCst);
}

/// Resolve once a cancel is requested — polled so 停止 abandons an in-flight
/// section instead of waiting for a 1–3 minute model call to finish.
async fn cancelled_now() {
    loop {
        if CANCEL.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
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

/// Streaming beats from the drain tasks: a content delta OR a reasoning
/// fragment of the section/outline currently being generated. `attempt` lets
/// the dialog reset that section's buffer when a retry starts.
fn emit_stream(
    app: &AppHandle,
    stage: &str,
    section_index: usize,
    total_sections: usize,
    delta: Option<String>,
    reasoning: Option<String>,
    attempt: u32,
) {
    emit(app, &ReviewProgress {
        stage: stage.into(),
        section_index,
        total_sections,
        attempt: Some(attempt),
        delta,
        reasoning,
        ..Default::default()
    });
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

/// Hard ceiling for the per-attempt output budget. Providers cap lower than
/// this on their own; beyond it, doubling stops and the dialog tells the user to
/// switch models instead of silently retrying forever.
const MAX_OUTPUT_TOKENS: u32 = crate::core::task_prompts::OUTPUT_CAP;
/// Sentinel error for "the user asked to stop" (distinct from a real failure).
const CANCELLED: &str = "已取消";

/// Output budget a section needs for `words` characters of prose. Thinking
/// models spend reasoning out of the same budget, hence the headroom.
fn section_need(words: usize) -> u32 {
    (words as u32).saturating_mul(2).saturating_add(4000).min(MAX_OUTPUT_TOKENS)
}

/// Build a client whose max_tokens is at least `need` (the provider row itself
/// is never modified).
async fn llm_with_need(
    db: &SqlitePool,
    provider_id: Option<&str>,
    need: u32,
) -> Result<Box<dyn crate::ai::llm::LlmClient>, String> {
    // A user-set budget floor (设置 → 领域智能体) only RAISES the budget: the
    // computed need is never lowered, because a lower cap truncates prose.
    let need = crate::core::task_prompts::apply_floor(
        need,
        crate::core::task_prompts::budget_floor(db, "review").await,
    );
    let mut cfg = crate::core::llm_provider_service::load_llm_config_for(db, provider_id).await?;
    // Thinking models count reasoning toward max_tokens: a section needs its
    // prose budget PLUS reasoning headroom, or the call ends on
    // finish_reason=length with zero visible content — which is exactly how
    // a note of headings-with-empty-bodies is produced. Bump the cap (the
    // provider row stays untouched).
    if cfg.max_tokens < need {
        cfg.max_tokens = need;
    }
    crate::ai::llm::client::create_llm_client(&cfg)
}

async fn default_llm(
    db: &SqlitePool,
    provider_id: Option<&str>,
    min_max_tokens: u32,
) -> Result<Box<dyn crate::ai::llm::LlmClient>, String> {
    let need = (min_max_tokens.saturating_mul(2).saturating_add(4000)).min(MAX_OUTPUT_TOKENS);
    llm_with_need(db, provider_id, need).await
}

/// A paper in the set: [n] numbering is the input order, stable across the
/// outline call, the section calls, the linkify pass and the reference list.
#[derive(Clone)]
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

/// Parse a JSON array of {title, points, subsections?} out of the model's
/// reply, tolerating fences and surrounding prose.
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
        #[serde(default)]
        subsections: Vec<String>,
    }
    match serde_json::from_str::<Vec<RawSection>>(&trimmed[s..=e]) {
        Ok(v) if !v.is_empty() => Ok(v
            .into_iter()
            .filter(|s| !s.title.trim().is_empty())
            .map(|s| ReviewSection {
                title: s.title.trim().to_string(),
                points: s.points.trim().to_string(),
                subsections: s
                    .subsections
                    .into_iter()
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect(),
            })
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
    let llm = default_llm(db, provider_id, 1500).await?;

    let section_count = ((target_words / 800) as usize).clamp(3, 8);
    let system = crate::core::task_prompts::effective(db, crate::core::task_prompts::REVIEW_OUTLINE).await;
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
                    emit_stream(&app2, "outline", 0, 0, Some(content), None, 0);
                }
            } else if ev.event_type == "reasoning" {
                if let Some(content) = ev.content {
                    emit_stream(&app2, "outline", 0, 0, None, Some(content), 0);
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

/// One in-flight review. Kept in memory only: closing the dialog (or starting a
/// new run) discards it, which is the same guarantee the old pipeline gave —
/// without the "one bad section loses everything" part.
struct ReviewSession {
    sources: Vec<SourcePaper>,
    question: String,
    language: String,
    outline: Vec<ReviewSection>,
    section_words: Vec<usize>,
    /// Output budget the first attempt ran with; retries double it.
    base_need: u32,
    provider_id: Option<String>,
    /// Assembled markdown per section (None = not generated yet / failed).
    results: Vec<Option<String>>,
    /// Retries performed per section (0 = the pipeline's first attempt).
    attempts: Vec<u32>,
    /// Whether the pipeline already attempted this section (a section skipped by
    /// cancel has not, so its first manual attempt keeps the base budget).
    started: Vec<bool>,
    errors: Vec<Option<String>>,
    note_id: Option<String>,
}

static SESSION: std::sync::Mutex<Option<ReviewSession>> = std::sync::Mutex::new(None);

fn store_session(s: ReviewSession) {
    if let Ok(mut g) = SESSION.lock() {
        *g = Some(s);
    }
}

fn clear_session() {
    if let Ok(mut g) = SESSION.lock() {
        *g = None;
    }
}

fn with_session<R>(f: impl FnOnce(&mut ReviewSession) -> R) -> Option<R> {
    SESSION.lock().ok().and_then(|mut g| g.as_mut().map(f))
}

fn sections_done(s: &ReviewSession) -> usize {
    s.results.iter().filter(|r| r.is_some()).count()
}

fn failed_titles(s: &ReviewSession) -> Vec<String> {
    s.outline
        .iter()
        .enumerate()
        .filter(|(i, _)| s.results[*i].is_none())
        .map(|(i, sec)| format!("{}.{}", i + 1, sec.title))
        .collect()
}

/// Step 2+: run the full pipeline (spawned by the command layer).
///
/// A failing section no longer aborts the run: it is recorded (`section_status:
/// failed` + its own error) and the pipeline moves on, so one bad section costs
/// one section instead of the whole review. When every section has content the
/// note is assembled and saved as before; otherwise the session stays alive for
/// per-section retries and an explicit "save what we have".
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
            emit(&app, &failure_progress(e));
            return;
        }
    };
    clear_session();

    let sources = match load_sources(&db, &paper_ids).await {
        Ok(s) if !s.is_empty() => s,
        Ok(_) => {
            emit(&app, &failure_progress("文献集为空（文献不存在或已删除）".into()));
            return;
        }
        Err(e) => {
            emit(&app, &failure_progress(e));
            return;
        }
    };
    let total = outline.len();
    // Sections with subsections carry more prose: weight the word budget by
    // subsection count so a 4-subsection section is not squeezed into the
    // same length as a one-block introduction.
    let weights: Vec<usize> = outline.iter().map(|s| 1 + s.subsections.len()).collect();
    let weight_sum: usize = weights.iter().sum();
    let section_words: Vec<usize> = weights
        .iter()
        .map(|w| (target_words as usize * w / weight_sum.max(1)).max(300))
        .collect();
    // One client for the first pass: sized for the largest section (retries
    // rebuild it with a doubled budget for the section that failed).
    let base_need = section_words.iter().copied().map(section_need).max().unwrap_or(4000);
    let llm = match llm_with_need(&db, provider_id.as_deref(), base_need).await {
        Ok(c) => c,
        Err(e) => {
            emit(&app, &failure_progress(e));
            return;
        }
    };

    store_session(ReviewSession {
        sources: sources.clone(),
        question: question.clone(),
        language: language.clone(),
        outline: outline.clone(),
        section_words: section_words.clone(),
        base_need,
        provider_id: provider_id.clone(),
        results: vec![None; total],
        attempts: vec![0; total],
        started: vec![false; total],
        errors: vec![None; total],
        note_id: None,
    });

    for idx in 0..total {
        if CANCEL.load(Ordering::SeqCst) {
            let done = with_session(|s| sections_done(s)).unwrap_or(0);
            let mut p = base_progress("cancelled");
            p.section_index = idx + 1;
            p.total_sections = total;
            p.section_title = outline[idx].title.clone();
            p.done_sections = Some(done);
            p.message = Some(format!(
                "已取消（完成 {done}/{total} 节）——未完成的节可以单独重试，或保存已完成部分。"
            ));
            emit(&app, &p);
            // Session survives a cancel on purpose: the completed sections are
            // still worth keeping, and per-section retry can finish the rest.
            return;
        }

        with_session(|s| s.started[idx] = true);
        emit(&app, &section_progress("section", "running", idx + 1, total, &outline[idx].title, 0, base_need, None, None));
        match write_section(
            &app,
            idx + 1,
            total,
            &db,
            llm.as_ref(),
            &sources,
            &question,
            &language,
            section_words[idx],
            &outline[idx],
            0,
            base_need,
        )
        .await
        {
            Ok(md) => {
                let done = with_session(|s| {
                    s.results[idx] = Some(md);
                    s.errors[idx] = None;
                    sections_done(s)
                })
                .unwrap_or(0);
                emit(&app, &section_progress("section", "done", idx + 1, total, &outline[idx].title, 0, base_need, Some(done), None));
            }
            Err(e) => {
                if e == CANCELLED {
                    // Cancel mid-flight: no section failure to record, just stop.
                    let done = with_session(|s| sections_done(s)).unwrap_or(0);
                    let mut p = base_progress("cancelled");
                    p.section_index = idx + 1;
                    p.total_sections = total;
                    p.section_title = outline[idx].title.clone();
                    p.done_sections = Some(done);
                    p.message = Some(format!(
                        "已取消（完成 {done}/{total} 节）——未完成的节可以单独重试，或保存已完成部分。"
                    ));
                    emit(&app, &p);
                    return;
                }
                tracing::warn!(section = idx + 1, error = %e, "review section failed (continuing)");
                let msg = e;
                let done = with_session(|s| {
                    s.errors[idx] = Some(msg.clone());
                    s.results[idx] = None;
                    sections_done(s)
                })
                .unwrap_or(0);
                emit(&app, &section_progress("section", "failed", idx + 1, total, &outline[idx].title, 0, base_need, Some(done), Some(msg)));
            }
        }
    }

    finish_or_partial(&app, &db).await;
}

/// All sections have content → assemble + save; otherwise report what is
/// missing and keep the session for retries.
async fn finish_or_partial(app: &AppHandle, db: &SqlitePool) {
    let info = with_session(|s| (s.outline.len(), sections_done(s), failed_titles(s)));
    let Some((total, done, missing)) = info else {
        return;
    };
    if done == total {
        if let Err(e) = assemble_and_save(app, db, false).await {
            emit(app, &failure_progress(format!("保存笔记失败：{e}")));
        }
        return;
    }
    let mut p = base_progress("partial");
    p.total_sections = total;
    p.done_sections = Some(done);
    p.message = Some(format!(
        "{done}/{total} 节已生成。缺失：{}——可单独重试（每次重试自动加倍输出额度），或先保存已完成部分。",
        missing.join("、")
    ));
    emit(app, &p);
}

/// Retry ONE section with a doubled output budget. `section_index` is 1-based.
///
/// Every failure type doubles (uniform, predictable): a parse/network failure
/// gains nothing from extra tokens but costs nothing either, and the user does
/// not have to classify their own errors.
pub async fn retry_section(app: AppHandle, db: SqlitePool, section_index: usize) -> Result<(), String> {
    let idx = section_index.checked_sub(1).ok_or_else(|| "节号从 1 开始".to_string())?;
    let (sources, question, language, section, words, base_need, exponent, provider_id, total) = {
        let g = SESSION.lock().map_err(|_| "会话锁失败".to_string())?;
        let s = g.as_ref().ok_or_else(|| "没有可续跑的综述任务，请重新生成".to_string())?;
        let section = s.outline.get(idx).cloned().ok_or_else(|| "节号超出大纲范围".to_string())?;
        // Attempts this section has already had: the pipeline's first pass (if
        // it got there) plus every retry. The k-th attempt runs at base×2^(k-1),
        // so the first retry of a failed section doubles, while a section that
        // never ran (cancelled run) starts at the base budget.
        let exponent = s.attempts[idx] + u32::from(s.started[idx]);
        (
            s.sources.clone(),
            s.question.clone(),
            s.language.clone(),
            section,
            s.section_words[idx],
            s.base_need,
            exponent,
            s.provider_id.clone(),
            s.outline.len(),
        )
    };

    let _guard = RunGuard::try_acquire()?;
    let attempt = exponent;
    let need = base_need
        .saturating_mul(1u32 << exponent.min(6))
        .min(MAX_OUTPUT_TOKENS);
    let llm = llm_with_need(&db, provider_id.as_deref(), need).await?;

    with_session(|s| s.started[idx] = true);
    emit(&app, &section_progress("section", "running", idx + 1, total, &section.title, attempt, need, None, None));
    let outcome = write_section(
        &app, idx + 1, total, &db, llm.as_ref(), &sources, &question, &language, words, &section, attempt, need,
    )
    .await;

    match outcome {
        Ok(md) => {
            let done = with_session(|s| {
                s.results[idx] = Some(md);
                s.errors[idx] = None;
                s.attempts[idx] = attempt;
                sections_done(s)
            })
            .unwrap_or(0);
            emit(&app, &section_progress("section", "done", idx + 1, total, &section.title, attempt, need, Some(done), None));
        }
        Err(e) if e == CANCELLED => {
            let done = with_session(|s| sections_done(s)).unwrap_or(0);
            let mut p = base_progress("cancelled");
            p.section_index = idx + 1;
            p.total_sections = total;
            p.section_title = section.title.clone();
            p.done_sections = Some(done);
            p.message = Some("已取消本次重试".to_string());
            emit(&app, &p);
            return Err("已取消".to_string());
        }
        Err(e) => {
            let msg = e;
            let done = with_session(|s| {
                s.errors[idx] = Some(msg.clone());
                s.attempts[idx] = attempt;
                sections_done(s)
            })
            .unwrap_or(0);
            emit(&app, &section_progress("section", "failed", idx + 1, total, &section.title, attempt, need, Some(done), Some(msg)));
            // Still report the overall state: the dialog uses the 'partial'
            // beat to leave its busy state (a retry can fail and stop there).
            finish_or_partial(&app, &db).await;
            return Err("该节仍未成功".to_string());
        }
    }

    finish_or_partial(&app, &db).await;
    Ok(())
}

/// Assemble and save the note from whatever sections exist. `allow_partial`
/// writes an explicit placeholder for the missing ones (and says so up top), so
/// a half-finished review never looks like a complete one.
pub async fn save_review(app: AppHandle, db: SqlitePool) -> Result<String, String> {
    assemble_and_save(&app, &db, true).await
}

async fn assemble_and_save(app: &AppHandle, db: &SqlitePool, allow_partial: bool) -> Result<String, String> {
    let (sources, question, outline, results, total, existing_note) = {
        let g = SESSION.lock().map_err(|_| "会话锁失败".to_string())?;
        let s = g.as_ref().ok_or_else(|| "没有可保存的综述任务，请重新生成".to_string())?;
        (
            s.sources.clone(),
            s.question.clone(),
            s.outline.clone(),
            s.results.clone(),
            s.outline.len(),
            s.note_id.clone(),
        )
    };
    let done = results.iter().filter(|r| r.is_some()).count();
    if done == 0 {
        return Err("还没有任何一节生成成功".to_string());
    }
    if !allow_partial && done < total {
        return Err("还有小节未生成".to_string());
    }

    let mut p = base_progress("assemble");
    p.total_sections = total;
    p.done_sections = Some(done);
    emit(app, &p);

    let note_title = format!("综述：{}", question.chars().take(40).collect::<String>());
    let missing = failed_titles_of(&outline, &results);
    let mut md = format!(
        "# {note_title}\n\n> 研究问题：{question}\n> 基于 {} 篇文献由 AI 生成，引用编号可点击溯源到原文。\n",
        sources.len()
    );
    if !missing.is_empty() {
        md.push_str(&format!(
            "> ⚠️ 本综述有 {} 节未生成成功：{}（可回到生成窗口重试后重新保存）。\n",
            missing.len(),
            missing.join("、")
        ));
    }
    md.push('\n');
    for (idx, section) in outline.iter().enumerate() {
        md.push_str(&format!("## {}\n\n", section.title));
        match &results[idx] {
            Some(body) => {
                md.push_str(body);
                md.push_str("\n\n");
            }
            None => md.push_str("> 本节生成失败，未包含在本次综述中。\n\n"),
        }
    }
    md.push_str("## 参考文献\n\n");
    for (i, p) in sources.iter().enumerate() {
        md.push_str(&reference_line(i + 1, p));
        md.push('\n');
    }

    // A note saved from a partial run gets *updated* once the retries fill the
    // gaps — creating a second note would leave the user with two reviews and no
    // way to tell which is complete.
    let had_existing = existing_note.is_some();
    let note_id = match existing_note {
        Some(id) => {
            crate::core::note_service::update_note(db, &id, None, Some(&md), None, None, None, Some(true))
                .await
                .map(|_| id)
                .map_err(|e| format!("更新笔记失败：{e}"))?
        }
        None => {
            let vault_id = crate::core::vault_service::get_current_vault_id(db).await?;
            let note =
                crate::core::note_service::create_note(db, &note_title, &md, None, None, &vault_id, false).await?;
            with_session(|s| s.note_id = Some(note.id.clone()));
            note.id
        }
    };

    let mut done_event = base_progress("done");
    done_event.section_index = total;
    done_event.total_sections = total;
    done_event.done_sections = Some(done);
    done_event.note_id = Some(note_id.clone());
    done_event.message = Some(if !missing.is_empty() {
        format!("已保存（含 {} 节缺口）", missing.len())
    } else if had_existing {
        "缺失小节已补全，笔记已更新".to_string()
    } else {
        String::new()
    });
    emit(app, &done_event);
    Ok(note_id)
}

fn failed_titles_of(outline: &[ReviewSection], results: &[Option<String>]) -> Vec<String> {
    outline
        .iter()
        .enumerate()
        .filter(|(i, _)| results.get(*i).map(|r| r.is_none()).unwrap_or(true))
        .map(|(i, sec)| format!("{}.{}", i + 1, sec.title))
        .collect()
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
    attempt: u32,
    max_tokens: u32,
) -> Result<String, String> {
    let paper_ids: Vec<String> = sources.iter().map(|p| p.id.clone()).collect();
    let query = format!("{} {}", section.title, section.points);
    let mut hits = retriever::hybrid_search_scoped(db, &query, SECTION_TOP_K, false, Some(&paper_ids)).await?;
    retriever::expand_with_neighbors(db, &mut hits, false).await.ok();

    let evidence_items = build_evidence(&hits, sources);
    let evidence = evidence_block(&evidence_items, sources);
    let points_line = if section.points.is_empty() {
        String::new()
    } else {
        format!("\n本节要点：{}", section.points)
    };
    let (subsection_rule, subsection_line) = if section.subsections.is_empty() {
        (String::new(), String::new())
    } else {
        (
            "\n5. 按以下小节组织本节内容，每个小节以 `### 小节标题` 开头；不要新增或删除小节。".to_string(),
            format!("\n本节小节：{}", section.subsections.join("；")),
        )
    };
    let system = crate::core::task_prompts::render(
        &crate::core::task_prompts::effective(db, crate::core::task_prompts::REVIEW_SECTION).await,
        &[
            ("language", language),
            ("target_words", &target_words.to_string()),
            ("subsection_rule", subsection_rule.as_str()),
        ],
    );
    let user = format!("研究问题：{question}\n本节主题：{}{points_line}{subsection_line}\n\n参考段落：\n{evidence}", section.title);
    let messages = vec![chat_msg("system", system), chat_msg("user", user)];

    // Stream the section: the drain task accumulates the text and forwards
    // every delta to the dialog. When the provider call ends it drops the
    // sender, closing the channel and letting the drain finish.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<crate::ai::llm::StreamEvent>();
    let app2 = app.clone();
    let drain = tokio::spawn(async move {
        let mut text = String::new();
        let mut finish: Option<String> = None;
        while let Some(ev) = rx.recv().await {
            match ev.event_type.as_str() {
                "delta" => {
                    if let Some(content) = ev.content {
                        text.push_str(&content);
                        emit_stream(&app2, "section", section_index, total_sections, Some(content), None, attempt);
                    }
                }
                "reasoning" => {
                    if let Some(content) = ev.content {
                        emit_stream(&app2, "section", section_index, total_sections, None, Some(content), attempt);
                    }
                }
                "finish" => finish = ev.content,
                _ => {}
            }
        }
        (text, finish)
    });
    let call = tokio::select! {
        r = llm.chat_completion_stream(&messages, &[], tx) => r,
        _ = cancelled_now() => Err(CANCELLED.to_string()),
    };
    let (text, finish) = drain.await.map_err(|e| format!("stream drain: {e}"))?;
    call?;
    if text.trim().is_empty() {
        // The classic failure is a thinking model exhausting max_tokens on
        // reasoning (finish=length) — say so plainly instead of letting the
        // pipeline assemble a note of empty sections.
        tracing::warn!(section_index, finish_reason = finish.as_deref().unwrap_or("?"), attempt, max_tokens, "section produced no visible content");
        return Err(match finish.as_deref() {
            Some("length") => format!(
                "模型的思考占满了输出额度（max_tokens={max_tokens}），未产生正文。重试会自动加倍额度。"
            ),
            other => format!(
                "模型未产生正文（finish_reason={}，max_tokens={max_tokens}）。",
                other.unwrap_or("未知")
            ),
        });
    }
    Ok(linkify_citations(text.trim(), sources, &evidence_items))
}

/// One retrievable passage handed to the model, addressable as `[E<k>]`.
///
/// Citation granularity is the PASSAGE, not the paper: a sentence backed by
/// §3.2 of paper 9 links to §3.2, and a later sentence backed by §5 of the same
/// paper links to §5. Both still *display* as `[9]` (the paper number) — the
/// visible citation stays a normal reference, only the anchor gets specific.
#[derive(Debug, Clone)]
struct Evidence {
    /// 1-based number of the owning paper inside `sources`.
    paper_index: usize,
    paper_id: String,
    page: Option<i32>,
    /// Full passage text handed to the model (writing needs the whole thing).
    content: String,
    /// Short word-boundary head used only as the reader's highlight anchor.
    quote: String,
}

fn build_evidence(hits: &[SearchResult], sources: &[SourcePaper]) -> Vec<Evidence> {
    let mut out = Vec::new();
    for hit in hits {
        // Neighbours exist for context, not as citable passages.
        if hit.is_neighbor {
            continue;
        }
        let Some(paper_index) = sources.iter().position(|p| p.id == hit.paper_id).map(|i| i + 1) else {
            continue;
        };
        let quote = anchor_quote(&hit.content);
        let content = hit.content.trim().to_string();
        if quote.is_empty() || content.is_empty() {
            continue;
        }
        out.push(Evidence {
            paper_index,
            paper_id: hit.paper_id.clone(),
            page: hit.page_start,
            content,
            quote,
        });
    }
    out
}

/// The evidence block the model reads: `[E1]（《title》 p.3）` + the passage.
fn evidence_block(evidence: &[Evidence], sources: &[SourcePaper]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut used = 0usize;
    for (i, ev) in evidence.iter().enumerate() {
        let remaining = SECTION_EVIDENCE_CAP.saturating_sub(used);
        if remaining == 0 {
            break;
        }
        let title = sources
            .get(ev.paper_index - 1)
            .map(|p| p.title.clone())
            .unwrap_or_default();
        let page = ev.page.map(|p| format!(" p.{p}")).unwrap_or_default();
        let body: String = ev.content.chars().take(remaining.min(1200)).collect();
        used += body.chars().count();
        out.push(format!("[E{}]（《{}》{}）\n{}", i + 1, title, page, body));
    }
    if out.is_empty() {
        "（文献集内未检索到相关段落，请依据摘要级信息概括，并减少具体断言）".to_string()
    } else {
        out.join("\n\n")
    }
}

/// A citation marker as written by the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marker {
    /// `[E3]` — passage 3 of this section's evidence block (the new contract).
    Evidence(usize),
    /// `[9]` — paper 9 (legacy contract; still accepted so a model that ignores
    /// the instruction degrades to the old one-anchor-per-paper behaviour
    /// instead of leaking raw markers into the note).
    Paper(usize),
}

fn is_separator(c: char) -> bool {
    c.is_whitespace() || matches!(c, ',' | '，' | '、' | ';' | '；')
}

/// Parse one `[...]` group at `start`. Returns the markers and the index just
/// past the closing bracket. `None` = this bracket is not a citation group.
fn parse_citation_group(
    chars: &[char],
    start: usize,
    max_paper: usize,
    max_evidence: usize,
) -> Option<(Vec<Marker>, usize)> {
    let mut i = start + 1;
    let mut markers: Vec<Marker> = Vec::new();
    loop {
        while i < chars.len() && is_separator(chars[i]) {
            i += 1;
        }
        if i >= chars.len() {
            return None;
        }
        if chars[i] == ']' {
            return if markers.is_empty() { None } else { Some((markers, i + 1)) };
        }
        let evidence = matches!(chars[i], 'E' | 'e');
        if evidence {
            i += 1;
        }
        let mut digits = String::new();
        while i < chars.len() && chars[i].is_ascii_digit() && digits.len() < 4 {
            digits.push(chars[i]);
            i += 1;
        }
        if digits.is_empty() {
            return None;
        }
        let Ok(n) = digits.parse::<usize>() else { return None };
        // Out-of-range markers are kept so the renderer can decide: an unknown
        // [E99] is a hallucinated passage (dropped), while an unknown [99] may
        // be a figure/table number and must survive as literal text.
        if evidence {
            if max_evidence == 0 {
                return None;
            }
            markers.push(Marker::Evidence(n));
        } else {
            if max_paper == 0 {
                return None;
            }
            markers.push(Marker::Paper(n));
        }
    }
}

/// Parse one group plus any immediately adjacent groups (`[E3][E7]`,
/// `[E3], [E7]`) into a single citation run.
fn parse_citation_run(
    chars: &[char],
    start: usize,
    max_paper: usize,
    max_evidence: usize,
) -> Option<(Vec<Marker>, usize)> {
    let (mut markers, mut next) = parse_citation_group(chars, start, max_paper, max_evidence)?;
    loop {
        let mut k = next;
        while k < chars.len() && is_separator(chars[k]) {
            k += 1;
        }
        if k < chars.len() && chars[k] == '[' {
            if let Some((more, after)) = parse_citation_group(chars, k, max_paper, max_evidence) {
                markers.extend(more);
                next = after;
                continue;
            }
        }
        break;
    }
    Some((markers, next))
}

fn reader_url(paper_id: &str, page: Option<i32>, quote: &str) -> String {
    let exact = percent_encode(quote);
    match page {
        Some(p) => format!("siku-reader://{paper_id}?page={p}&exact={exact}"),
        None => format!("siku-reader://{paper_id}?exact={exact}"),
    }
}

/// Resolve one marker to (display number, link target).
///
/// The *display* number is always the paper's citation number — a citation must
/// still read like `[9]` — while the *target* is the concrete passage, so two
/// sentences citing the same paper point at their own paragraphs.
fn resolve_marker(
    marker: Marker,
    sources: &[SourcePaper],
    evidence: &[Evidence],
) -> Option<(usize, Option<String>)> {
    match marker {
        Marker::Evidence(k) => {
            let ev = evidence.get(k.checked_sub(1)?)?;
            Some((ev.paper_index, Some(reader_url(&ev.paper_id, ev.page, &ev.quote))))
        }
        Marker::Paper(n) => {
            // Range check only — the anchor comes from the evidence list.
            sources.get(n.checked_sub(1)?)?;
            // Legacy: anchor on this paper's first passage in the section.
            let anchor = evidence.iter().find(|ev| ev.paper_index == n);
            Some((
                n,
                anchor.map(|ev| reader_url(&ev.paper_id, ev.page, &ev.quote)),
            ))
        }
    }
    .filter(|(n, _)| *n >= 1 && *n <= sources.len())
}

/// Render a citation run into markdown that displays as `[9]` / `[9,13]` with
/// each number carrying its own link (verified against react-markdown: literal
/// brackets wrapped around per-number links render exactly like a paper
/// citation). `None` when nothing resolves — the caller then keeps the source
/// text verbatim.
fn render_citation_run(
    markers: &[Marker],
    sources: &[SourcePaper],
    evidence: &[Evidence],
) -> Option<String> {
    let mut items: Vec<(usize, Option<String>)> = Vec::new();
    for m in markers {
        let Some((display, url)) = resolve_marker(*m, sources, evidence) else {
            continue;
        };
        // Same paper twice in one group (two passages cited together): keep the
        // first anchor, show the number once.
        if items.iter().any(|(d, _)| *d == display) {
            continue;
        }
        items.push((display, url));
    }
    if items.is_empty() {
        return None;
    }
    if items.len() == 1 {
        let (n, url) = &items[0];
        return Some(match url {
            Some(u) => format!("[[{n}]]({u})"),
            None => format!("[{n}]"),
        });
    }
    let inner = items
        .iter()
        .map(|(n, url)| match url {
            Some(u) => format!("[{n}]({u})"),
            None => n.to_string(),
        })
        .collect::<Vec<_>>()
        .join(",");
    Some(format!("[{inner}]"))
}

/// Rewrite every citation marker in a generated section.
///
/// `[E3]` → `[9]` (link → passage 3), `[E9][E13]` / `[E9, E13]` → `[9,13]`
/// (each number its own passage), legacy `[9]` → `[9]` (link → that paper's
/// first passage). Markers that resolve to nothing are removed when they claim
/// to be evidence (a hallucinated `[E99]`), and left as literal text when they
/// look like a paper number out of range (possibly a figure/table number).
fn linkify_citations(text: &str, sources: &[SourcePaper], evidence: &[Evidence]) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 64);
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '[' {
            if let Some((markers, next)) = parse_citation_run(&chars, i, sources.len(), evidence.len()) {
                match render_citation_run(&markers, sources, evidence) {
                    Some(rendered) => {
                        out.push_str(&rendered);
                        i = next;
                        continue;
                    }
                    None => {
                        // Unresolvable group: drop evidence markers entirely
                        // (never show a citation that points nowhere), keep
                        // paper-number groups verbatim.
                        if markers.iter().all(|m| matches!(m, Marker::Evidence(_))) {
                            i = next;
                            continue;
                        }
                    }
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// The quote a deep link anchors on. Long enough to be specific, cut back to a
/// word boundary so the reader's matcher is not fed half a word.
fn anchor_quote(content: &str) -> String {
    const CAP: usize = 90;
    let flat = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut head: String = flat.chars().take(CAP).collect();
    if flat.chars().count() > CAP {
        if let Some(idx) = head.rfind(' ') {
            // Keep at least a few words even if the tail word is long.
            if idx >= 40 {
                head.truncate(idx);
            }
        }
    }
    head.trim().to_string()
}

pub(crate) fn percent_encode(s: &str) -> String {
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
    bibliography_line(
        n,
        &p.authors,
        &p.title,
        p.journal.as_deref(),
        p.year,
        p.doi.as_deref(),
        p.url.as_deref(),
    )
}

/// One numbered bibliography entry. Shared with `core::evidence_note` so a note
/// produced by the 文献分析 agent cites exactly like a 综述 note.
#[allow(clippy::too_many_arguments)]
pub(crate) fn bibliography_line(
    n: usize,
    authors_json: &str,
    title: &str,
    journal: Option<&str>,
    year: Option<i64>,
    doi: Option<&str>,
    url: Option<&str>,
) -> String {
    format!(
        "[{n}] {}",
        bibliography_entry(authors_json, title, journal, year, doi, url)
    )
}

/// The same entry without a citation number — used where the list must not
/// collide with the body's numbering (文献分析 notes number their 引用出处 per
/// citation, so a numbered bibliography would not line up).
pub(crate) fn bibliography_entry(
    authors_json: &str,
    title: &str,
    journal: Option<&str>,
    year: Option<i64>,
    doi: Option<&str>,
    url: Option<&str>,
) -> String {
    let authors = crate::core::citation_export::parse_authors(authors_json);
    let author_str = if authors.len() > 3 {
        format!("{}, 等", authors[..3].join(", "))
    } else {
        authors.join(", ")
    };
    let mut line = format!("{author_str}. {title}.");
    match (journal, year) {
        (Some(j), Some(y)) if !j.is_empty() => line.push_str(&format!(" {j}, {y}.")),
        (Some(j), None) if !j.is_empty() => line.push_str(&format!(" {j}.")),
        (None, Some(y)) => line.push_str(&format!(" {y}.")),
        _ => {}
    }
    if let Some(doi) = doi.filter(|d| !d.is_empty()) {
        line.push_str(&format!(" DOI: {doi}."));
    } else if let Some(url) = url.filter(|u| !u.is_empty()) {
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


    fn ev(paper_index: usize, paper_id: &str, page: i32, quote: &str) -> Evidence {
        Evidence {
            paper_index,
            paper_id: paper_id.into(),
            page: Some(page),
            content: quote.into(),
            quote: quote.into(),
        }
    }

    #[test]
    fn evidence_marker_displays_the_paper_number() {
        let sources = [src("p1", "甲"), src("p2", "乙")];
        let evidence = [ev(1, "p1", 5, "这是证据文本"), ev(2, "p2", 9, "第二条证据")];
        let out = linkify_citations("结论如[E1]所示。", &sources, &evidence);
        assert!(out.starts_with("结论如[[1]](siku-reader://p1?page=5&exact="), "{out}");
    }

    #[test]
    fn two_passages_of_the_same_paper_link_to_different_places() {
        // The whole point: same paper cited twice → two distinct anchors.
        let sources = [src("p1", "甲")];
        let evidence = [
            ev(1, "p1", 3, "第三章里的这段证据"),
            ev(1, "p1", 11, "第十一章里的另一段证据"),
        ];
        let out = linkify_citations("先看[E1]，再看[E2]。", &sources, &evidence);
        let links: Vec<&str> = out.split("siku-reader://").skip(1).collect();
        assert_eq!(links.len(), 2, "{out}");
        assert!(links[0].contains("page=3"), "{out}");
        assert!(links[1].contains("page=11"), "{out}");
        // Both still display as the paper's number.
        assert_eq!(out.matches("[[1]]").count(), 2, "{out}");
    }

    #[test]
    fn adjacent_evidence_markers_collapse_into_one_bracket_group() {
        let sources = [src("p1", "甲"), src("p2", "乙")];
        let evidence = [ev(1, "p1", 3, "证据甲"), ev(2, "p2", 8, "证据乙")];
        for src_text in ["见[E1][E2]。", "见[E1] [E2]。", "见[E1,E2]。", "见[E1，E2]。"] {
            let out = linkify_citations(src_text, &sources, &evidence);
            assert_eq!(
                out,
                "见[[1](siku-reader://p1?page=3&exact=%E8%AF%81%E6%8D%AE%E7%94%B2),[2](siku-reader://p2?page=8&exact=%E8%AF%81%E6%8D%AE%E4%B9%99)]。",
                "source was {src_text}"
            );
        }
    }

    #[test]
    fn same_paper_grouped_twice_shows_one_number() {
        let sources = [src("p1", "甲")];
        let evidence = [ev(1, "p1", 3, "甲"), ev(1, "p1", 11, "乙")];
        let out = linkify_citations("见[E1,E2]。", &sources, &evidence);
        assert_eq!(out.matches("[[1]]").count(), 1, "{out}");
        assert!(out.contains("page=3"), "{out}");
    }

    #[test]
    fn legacy_paper_marker_still_works() {
        let sources = [src("p1", "甲"), src("p2", "乙")];
        let evidence = [ev(2, "p2", 9, "只有乙有证据")];
        let out = linkify_citations("结论[1]与[2]。", &sources, &evidence);
        // p1 has no passage → plain number, never a dead link.
        assert!(out.contains("[1]"), "{out}");
        assert!(!out.contains("siku-reader://p1"), "{out}");
        assert!(out.contains("[[2]](siku-reader://p2?page=9&exact="), "{out}");
    }

    #[test]
    fn hallucinated_evidence_marker_is_removed_not_leaked() {
        let sources = [src("p1", "甲")];
        let evidence = [ev(1, "p1", 3, "证据")];
        let out = linkify_citations("结论[E99]成立。", &sources, &evidence);
        assert_eq!(out, "结论成立。");
    }

    #[test]
    fn out_of_range_paper_number_survives_as_literal_text() {
        // A figure/table number must not be silently deleted.
        let sources = [src("p1", "甲")];
        let evidence = [ev(1, "p1", 3, "证据")];
        let out = linkify_citations("如图[7]所示，结论[1]成立。", &sources, &evidence);
        assert!(out.contains("如图[7]所示"), "{out}");
        assert!(out.contains("[[1]]"), "{out}");
    }

    #[test]
    fn anchor_quote_cuts_on_a_word_boundary() {
        let long = "word ".repeat(40);
        let q = anchor_quote(&long);
        assert!(q.chars().count() <= 90);
        assert!(!q.ends_with(' '));
        // Never ends mid-word.
        assert!(q.ends_with("word"), "quote ended with {q:?}");
        assert_eq!(anchor_quote("  spaced   out  "), "spaced out");
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
        assert!(v[0].subsections.is_empty());
        assert!(parse_outline("没有 JSON").is_err());
    }

    #[test]
    fn parse_outline_reads_subsections_and_tolerates_missing_field() {
        let v = parse_outline(
            r#"[{"title":"方法","points":"对比","subsections":[" 监督方法 ","无监督方法",""]},{"title":"总结"}]"#,
        )
        .unwrap();
        assert_eq!(v[0].subsections, vec!["监督方法", "无监督方法"]);
        assert!(v[1].subsections.is_empty());
    }
}
