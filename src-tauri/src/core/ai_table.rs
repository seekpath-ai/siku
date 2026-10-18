//! AI 表格列: batch field extraction over a paper set.
//!
//! Each column is a user-defined extraction prompt ("样本量", "研究方法"…).
//! One LLM call per paper returns ALL columns as JSON (N papers = N calls,
//! not N×M), with the column prompt scoped-retrieved inside that paper's
//! chunks as evidence. Cells persist in `ai_table_cells` so reopening the
//! same paper set + columns shows cached values; only missing/failed cells
//! are (re)extracted, and manually edited cells are never overwritten.
//!
//! Progress/cancellation mirrors batch_import: one run at a time, cancel
//! between papers, per-paper progress events on [`PROGRESS_EVENT`].

use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tauri::{AppHandle, Emitter};

use crate::ai::llm::ChatMessage;
use crate::ai::retriever;

pub const PROGRESS_EVENT: &str = "library:ai-table-progress";

static RUNNING: AtomicBool = AtomicBool::new(false);
static CANCEL: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableColumn {
    /// Display name AND cell key (cells are keyed by name — renaming a
    /// column starts it empty).
    pub key: String,
    /// Extraction instruction; doubles as the retrieval query inside each
    /// paper. Empty prompt falls back to the key itself.
    #[serde(default)]
    pub prompt: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractProgress {
    pub total: usize,
    pub done: usize,
    pub failed: usize,
    /// Title of the paper currently being processed ("" between/after runs).
    pub current_title: String,
    /// One log line for the dialog's process panel (paper finished/failed/
    /// skipped); None on the "starting paper X" beats.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
    /// Reasoning fragment of the paper being extracted (thinking models spend
    /// most of the call here; without it the panel shows nothing for minutes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// Set when the run ends: "done" | "cancelled" | "busy".
    pub finished: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableCell {
    pub paper_id: String,
    pub column_key: String,
    pub value: String,
    /// done | failed | edited | empty (未检索到) | absent (文中无此内容)
    pub status: String,
    pub edited: bool,
    /// Where the model says the value came from ("p.21 §5").
    pub src: String,
    /// Model's self-reported confidence, 0 when unreported.
    pub conf: f64,
}

pub fn request_cancel() {
    CANCEL.store(true, Ordering::SeqCst);
}

/// Resolve once a cancel is requested. Polling (not a Notify permit) on
/// purpose: a permit left over from a previous run would make the next run
/// cancel itself immediately, and 200 ms is imperceptible to the user.
async fn cancelled_now() {
    loop {
        if CANCEL.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

/// Last progress beat, so a dialog that was closed (or a webview that was
/// reloaded) can re-attach to a run that is still going instead of showing a
/// dead spinner.
static LAST: std::sync::Mutex<Option<ExtractProgress>> = std::sync::Mutex::new(None);

pub fn status() -> (bool, Option<ExtractProgress>) {
    let last = LAST.lock().ok().and_then(|g| g.clone());
    (RUNNING.load(Ordering::SeqCst), last)
}

struct RunGuard;
impl RunGuard {
    fn try_acquire() -> Result<Self, String> {
        if RUNNING.swap(true, Ordering::SeqCst) {
            return Err("已有表格抽取任务正在进行中".to_string());
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

fn emit(app: &AppHandle, p: &ExtractProgress) {
    if let Ok(mut g) = LAST.lock() {
        *g = Some(p.clone());
    }
    if let Err(e) = app.emit(PROGRESS_EVENT, p) {
        tracing::warn!(error = %e, "failed to emit ai-table progress");
    }
}

/// How hard the run looks for evidence. Cost (LLM calls per paper) and the
/// amount of source text handed to the model are the user's choice — the code
/// never silently trades accuracy for money.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// 1 call, 3 chunks per column, ≤6k chars.
    Fast,
    /// 1 call + one follow-up pass for fields whose evidence was insufficient.
    Standard,
    /// 1 call, 8 chunks per column, section-aware coverage, ≤24k chars.
    Deep,
    /// One call PER COLUMN, each with its own retrieval — the most accurate and
    /// the most expensive.
    PerColumn,
}

impl Depth {
    pub fn parse(value: Option<&str>) -> Self {
        match value.unwrap_or("standard").trim() {
            "fast" => Depth::Fast,
            "deep" => Depth::Deep,
            "per_column" | "per-column" => Depth::PerColumn,
            _ => Depth::Standard,
        }
    }

    fn chunks_per_column(self) -> usize {
        match self {
            Depth::Fast => 3,
            Depth::Standard => 5,
            Depth::Deep => 8,
            Depth::PerColumn => 10,
        }
    }

    fn evidence_cap(self) -> usize {
        match self {
            Depth::Fast => 6000,
            Depth::Standard => 12000,
            Depth::Deep | Depth::PerColumn => 24000,
        }
    }

    /// One extra pass for fields the first call could not decide.
    fn second_pass(self) -> bool {
        matches!(self, Depth::Standard | Depth::Deep | Depth::PerColumn)
    }

    /// One call per column instead of one per paper.
    fn per_column_calls(self) -> bool {
        matches!(self, Depth::PerColumn)
    }

    pub fn label(self) -> &'static str {
        match self {
            Depth::Fast => "快速",
            Depth::Standard => "标准",
            Depth::Deep => "深入",
            Depth::PerColumn => "逐列精查",
        }
    }
}

/// One extracted field with its provenance.
#[derive(Debug, Clone, Default)]
struct FieldValue {
    value: String,
    /// Where in the paper the model says the value comes from ("p.21 §5").
    src: String,
    /// Model's self-reported confidence (0 when it did not say).
    conf: f64,
}

/// Parse one field entry: either a bare string (legacy contract) or
/// `{"v": "...", "src": "p.21", "conf": 0.8}`.
fn parse_field(entry: &serde_json::Value) -> FieldValue {
    if let Some(s) = entry.as_str() {
        return FieldValue { value: s.trim().to_string(), src: String::new(), conf: 0.0 };
    }
    let Some(obj) = entry.as_object() else {
        return FieldValue::default();
    };
    let pick = |keys: &[&str]| -> Option<&serde_json::Value> {
        keys.iter().find_map(|k| obj.get(*k))
    };
    let value = pick(&["v", "value"])
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let src = pick(&["src", "source", "evidence"])
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let conf = pick(&["conf", "confidence"])
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);
    FieldValue { value, src, conf }
}

/// Empty-value semantics. "The paper does not say this" and "we did not manage
/// to show the model the right passage" are completely different answers, and
/// collapsing them into one blank cell is what made the table untrustworthy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CellState {
    Filled,
    /// The model looked at real evidence and reported the paper has no such
    /// content.
    Absent,
    /// No usable evidence reached the model — OUR problem, worth retrying.
    Unretrieved,
}

fn cell_state(field: &FieldValue, evidence_chars: usize) -> CellState {
    if !field.value.is_empty() {
        return CellState::Filled;
    }
    if evidence_chars == 0 {
        return CellState::Unretrieved;
    }
    // Model said "nothing there" with reasonable confidence and it had material.
    if field.conf >= 0.5 {
        CellState::Absent
    } else {
        CellState::Unretrieved
    }
}

/// How verbose each cell should be. The old prompt hard-coded "不超过 50 字",
/// which is why every cell read like a label instead of an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    Brief,
    Normal,
    Rich,
}

impl Detail {
    pub fn parse(value: Option<&str>) -> Self {
        match value.unwrap_or("normal").trim() {
            "brief" => Detail::Brief,
            "rich" => Detail::Rich,
            _ => Detail::Normal,
        }
    }

    /// The per-field length rule handed to the model.
    fn length_rule(self) -> &'static str {
        match self {
            Detail::Brief => "每个字段不超过 40 字，只给结论、名称或数字，不要铺陈",
            Detail::Normal => "每个字段 1~2 句、不超过 120 字，先给结论再给关键依据",
            Detail::Rich => {
                "每个字段尽量完整（3~6 句、不超过 400 字），保留关键数字、实验设定、结论倾向与作者的原话关键词"
            }
        }
    }
}

/// Run the batch. `paper_ids` is the selected set, `columns` the column
/// defs, `provider_id` overrides the default (starred) model when set.
/// Spawning/cancellation is the command layer's job.
pub async fn run_extraction(
    app: AppHandle,
    db: SqlitePool,
    paper_ids: Vec<String>,
    columns: Vec<TableColumn>,
    provider_id: Option<String>,
    detail: Detail,
    depth: Depth,
    force: bool,
) {
    let _guard = match RunGuard::try_acquire() {
        Ok(g) => g,
        Err(e) => {
            tracing::warn!(error = %e, "ai-table extraction already running");
            // Silence here used to strand the dialog: it had already flipped to
            // "running" and no finished event ever arrived, so 停止 pointed at
            // nothing and the window could not be closed.
            emit(
                &app,
                &ExtractProgress {
                    total: 0,
                    done: 0,
                    failed: 0,
                    current_title: String::new(),
                    log: Some(format!("{e}（多半是上一次抽取还没结束，可稍后重试）")),
                    reasoning: None,
                    finished: Some("busy".into()),
                },
            );
            return;
        }
    };

    let llm = match crate::core::llm_provider_service::load_llm_config_for(&db, provider_id.as_deref()).await {
        Ok(cfg) => {
            // Thinking models count reasoning toward max_tokens: a tiny
            // provider cap can be exhausted before any JSON is emitted. 6000 is
            // the built-in floor; a user budget (设置 → 领域智能体) can raise it.
            let mut cfg = cfg;
            let floor = crate::core::task_prompts::apply_floor(
                6000,
                crate::core::task_prompts::budget_floor(&db, "ai_table").await,
            );
            if cfg.max_tokens < floor {
                cfg.max_tokens = floor;
            }
            match crate::ai::llm::client::create_llm_client(&cfg) {
                Ok(c) => c,
                Err(e) => {
                    emit(&app, &ExtractProgress { total: 0, done: 0, failed: 0, current_title: String::new(), log: Some(format!("创建模型客户端失败：{e}")), reasoning: None, finished: Some("done".into()) });
                    return;
                }
            }
        }
        Err(e) => {
            emit(&app, &ExtractProgress { total: 0, done: 0, failed: 0, current_title: String::new(), log: Some(format!("加载模型配置失败：{e}")), reasoning: None, finished: Some("done".into()) });
            return;
        }
    };

    // Search terms are prepared once per run (an AI-generated term set is cached
    // per column key), never per paper — this is what lets a Chinese column
    // definition find passages in an English paper at all.
    let pairs: Vec<(String, String)> = columns.iter().map(|c| (c.key.clone(), c.prompt.clone())).collect();
    let terms = crate::core::query_expand::prepare(&db, Some(llm.as_ref()), &pairs, true).await;

    let total = paper_ids.len();
    let mut done = 0usize;
    let mut failed = 0usize;
    let mut cancelled = false;

    // Run header: the exact configuration is written into the log so a result
    // can always be traced back to how it was produced.
    emit(
        &app,
        &ExtractProgress {
            total,
            done: 0,
            failed: 0,
            current_title: String::new(),
            log: Some(format!(
                "口径：深度={}（{} 段/列，证据≤{} 字，{}）/ 详略={:?} / 检索词={} 列已就绪",
                depth.label(),
                depth.chunks_per_column(),
                depth.evidence_cap(),
                if depth.per_column_calls() { "逐列调用" } else if depth.second_pass() { "含二次补判" } else { "单轮" },
                detail,
                terms.values().filter(|t| !t.is_empty()).count()
            )),
            reasoning: None,
            finished: None,
        },
    );

    for paper_id in &paper_ids {
        if CANCEL.load(Ordering::SeqCst) {
            cancelled = true;
            break;
        }
        let title = paper_title(&db, paper_id).await;
        emit(&app, &ExtractProgress { total, done, failed, current_title: title.clone(), log: None, reasoning: None, finished: None });

        let started = std::time::Instant::now();
        let outcome = extract_paper(&app, &db, llm.as_ref(), paper_id, &columns, detail, depth, &terms, force).await;
        match outcome {
            Ok(o) => {
                done += 1;
                let log = if o.skipped {
                    format!("— 《{title}》全部单元格已存在，跳过")
                } else {
                    match o.empty_note {
                        Some(note) => note,
                        None => format!("✓ 《{title}》抽取完成（{:.1}s）", started.elapsed().as_secs_f64()),
                    }
                };
                emit(&app, &ExtractProgress { total, done, failed, current_title: title, log: Some(log), reasoning: None, finished: None });
            }
            Err(e) => {
                if CANCEL.load(Ordering::SeqCst) {
                    cancelled = true;
                    break;
                }
                tracing::warn!(paper_id, error = %e, "ai-table extract failed for paper");
                let _ = mark_failed(&db, paper_id, &columns).await;
                failed += 1;
                let log = format!("✗ 《{title}》：{e}");
                emit(&app, &ExtractProgress { total, done, failed, current_title: title, log: Some(log), reasoning: None, finished: None });
            }
        }
    }

    emit(&app, &ExtractProgress {
        total,
        done,
        failed,
        current_title: String::new(),
        log: None,
        reasoning: None,
        finished: Some(if cancelled { "cancelled".into() } else { "done".into() }),
    });
}

async fn paper_title(db: &SqlitePool, paper_id: &str) -> String {
    sqlx::query_as::<_, (String,)>("SELECT title FROM papers WHERE id = ?")
        .bind(paper_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .map(|(t,)| t)
        .unwrap_or_else(|| paper_id.to_string())
}

/// Extract every column for one paper and upsert the cells. Cells that are
/// already `done` or were manually `edited` are left untouched (the LLM call
/// still returns all columns — upsert is selective). Returns false when no
/// LLM call was needed (all cells already present).
/// What one paper's extraction produced.
struct ExtractOutcome {
    /// Nothing was pending (all cells already filled) — not a failure.
    skipped: bool,
    /// Set when the call ran but produced no content for any column: the line
    /// the process panel shows instead of a mute "待补".
    empty_note: Option<String>,
}

fn emit_reasoning(app: &AppHandle, title: &str, delta: &str) {
    emit(
        app,
        &ExtractProgress {
            total: 0,
            done: 0,
            failed: 0,
            current_title: title.to_string(),
            log: None,
            reasoning: Some(delta.to_string()),
            finished: None,
        },
    );
}

/// Extract one paper under the chosen depth: single pass, per-column calls,
/// and/or one follow-up pass for fields whose evidence was insufficient.
#[allow(clippy::too_many_arguments)]
async fn extract_paper(
    app: &AppHandle,
    db: &SqlitePool,
    llm: &dyn crate::ai::llm::LlmClient,
    paper_id: &str,
    columns: &[TableColumn],
    detail: Detail,
    depth: Depth,
    terms: &std::collections::HashMap<String, Vec<String>>,
    force: bool,
) -> Result<ExtractOutcome, String> {
    let pending = pending_columns(db, paper_id, columns, force).await?;
    if pending.is_empty() {
        return Ok(ExtractOutcome { skipped: true, empty_note: None });
    }
    let title = paper_title(db, paper_id).await;
    let meta = paper_metadata(db, paper_id).await?;

    // (column, value, state, evidence chars that backed it)
    let mut results: Vec<(TableColumn, FieldValue, CellState, usize)> = Vec::new();

    if depth.per_column_calls() {
        // One call per column: (columns × papers) calls, the most accurate mode.
        for col in &pending {
            let one = std::slice::from_ref(col);
            let (evidence, ev_chars) = gather_evidence(db, paper_id, one, depth, terms).await;
            let mut map =
                model_call(app, db, llm, &title, &meta, &evidence, one, detail).await?;
            let field = map.remove(&col.key).unwrap_or_default();
            let state = cell_state(&field, ev_chars);
            results.push((col.clone(), field, state, ev_chars));
        }
    } else {
        let (evidence, ev_chars) = gather_evidence(db, paper_id, &pending, depth, terms).await;
        let mut map = model_call(app, db, llm, &title, &meta, &evidence, &pending, detail).await?;
        for col in &pending {
            let field = map.remove(&col.key).unwrap_or_default();
            let state = cell_state(&field, ev_chars);
            results.push((col.clone(), field, state, ev_chars));
        }
    }

    // Follow-up pass: only fields whose evidence was insufficient ("we could not
    // show the model the right passage"). Capped so a pathological paper cannot
    // turn into an unbounded call loop.
    if depth.second_pass() {
        let retry: Vec<TableColumn> = results
            .iter()
            .filter(|(_, _, state, _)| *state == CellState::Unretrieved)
            .map(|(col, _, _, _)| col.clone())
            .take(6)
            .collect();
        if !retry.is_empty() {
            let (evidence, ev_chars) = gather_evidence(db, paper_id, &retry, depth, terms).await;
            if ev_chars > 0 {
                if let Ok(mut map) = model_call(app, db, llm, &title, &meta, &evidence, &retry, detail).await {
                    for slot in results.iter_mut() {
                        if slot.2 != CellState::Unretrieved {
                            continue;
                        }
                        if let Some(field) = map.remove(&slot.0.key) {
                            let state = cell_state(&field, ev_chars);
                            if state != CellState::Unretrieved {
                                slot.1 = field;
                                slot.2 = state;
                                slot.3 = ev_chars;
                            }
                        }
                    }
                }
            }
        }
    }

    let now = crate::core::time::now_iso();
    let mut written = 0usize;
    let mut absent: Vec<String> = Vec::new();
    let mut unretrieved: Vec<String> = Vec::new();
    for (col, field, state, _) in &results {
        let status = match state {
            CellState::Filled => "done",
            // The model read real evidence and reported the paper has no such
            // content: a final answer, not a retry candidate.
            CellState::Absent => "absent",
            CellState::Unretrieved => "empty",
        };
        match state {
            CellState::Filled => written += 1,
            CellState::Absent => absent.push(col.key.clone()),
            CellState::Unretrieved => unretrieved.push(col.key.clone()),
        }
        upsert_cell(db, paper_id, &col.key, &field.value, status, false, field, &now).await?;
    }

    let empty_note = if written == 0 {
        let (chunks, abstract_len) = source_stats(db, paper_id).await;
        let mut line = format!(
            "⚠ 《{title}》没抽到内容：来源=摘要 {abstract_len} 字 / 全文索引 {chunks} 段"
        );
        if !absent.is_empty() {
            line.push_str(&format!("；模型确认文中没有：{}", absent.join("、")));
        }
        if !unretrieved.is_empty() {
            line.push_str(&format!("；证据不足（可换「深入/逐列精查」重试）：{}", unretrieved.join("、")));
        }
        if chunks == 0 && abstract_len < 100 {
            line.push_str("。⚠ 该文献没有全文索引且摘要过短——请先「重建索引」或补全摘要");
        }
        Some(line)
    } else if !absent.is_empty() || !unretrieved.is_empty() {
        let mut line = format!("✓ 《{title}》抽取完成");
        if !absent.is_empty() {
            line.push_str(&format!("；文中无此内容：{}", absent.join("、")));
        }
        if !unretrieved.is_empty() {
            line.push_str(&format!("；证据不足：{}", unretrieved.join("、")));
        }
        Some(line)
    } else {
        None
    };

    Ok(ExtractOutcome { skipped: false, empty_note })
}

/// One model call for the given columns. Streams, so the thinking model's
/// reasoning is visible instead of looking frozen.
#[allow(clippy::too_many_arguments)]
async fn model_call(
    app: &AppHandle,
    db: &SqlitePool,
    llm: &dyn crate::ai::llm::LlmClient,
    title: &str,
    meta: &str,
    evidence: &str,
    columns: &[TableColumn],
    detail: Detail,
) -> Result<std::collections::HashMap<String, FieldValue>, String> {
    let column_spec = columns
        .iter()
        .map(|c| {
            if c.prompt.trim().is_empty() {
                format!("- \"{}\": 提取「{}」", c.key, c.key)
            } else {
                format!("- \"{}\": {}", c.key, c.prompt.trim())
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    let system = crate::core::task_prompts::render(
        &crate::core::task_prompts::effective(db, crate::core::task_prompts::AI_TABLE_EXTRACT).await,
        &[("detail_rule", detail.length_rule())],
    );
    let user = format!("【文献】\n{meta}\n\n【相关段落】\n{evidence}\n\n【待抽取字段】\n{column_spec}");
    let messages = vec![
        ChatMessage { reasoning_content: None, role: "system".into(), content: system, attachments: None, tool_calls: None, tool_call_id: None, name: None },
        ChatMessage { reasoning_content: None, role: "user".into(), content: user, attachments: None, tool_calls: None, tool_call_id: None, name: None },
    ];

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<crate::ai::llm::StreamEvent>();
    let app2 = app.clone();
    let title2 = title.to_string();
    let drain = tokio::spawn(async move {
        let mut text = String::new();
        while let Some(ev) = rx.recv().await {
            match ev.event_type.as_str() {
                "delta" => {
                    if let Some(c) = ev.content {
                        text.push_str(&c);
                    }
                }
                "reasoning" => {
                    if let Some(c) = ev.content {
                        emit_reasoning(&app2, &title2, &c);
                    }
                }
                _ => {}
            }
        }
        text
    });
    let call = tokio::select! {
        r = llm.chat_completion_stream(&messages, &[], tx) => r,
        _ = cancelled_now() => Err("已取消".to_string()),
    };
    let text = drain.await.map_err(|e| format!("stream drain: {e}"))?;
    call?;

    let values = parse_values(&text)?;
    let mut out = std::collections::HashMap::new();
    for col in columns {
        if let Some(entry) = lookup_entry(&values, &col.key) {
            out.insert(col.key.clone(), parse_field(entry));
        }
    }
    Ok(out)
}

async fn pending_columns(
    db: &SqlitePool,
    paper_id: &str,
    columns: &[TableColumn],
    force: bool,
) -> Result<Vec<TableColumn>, String> {
    let mut out = Vec::new();
    for col in columns {
        let row = sqlx::query_as::<_, (String, i64, String)>(
            "SELECT status, edited, value FROM ai_table_cells WHERE paper_id = ? AND column_key = ?",
        )
        .bind(paper_id)
        .bind(&col.key)
        .fetch_optional(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
        match row {
            Some((_, edited, _)) if edited == 1 => {
                // A manual edit is the user's final word — even a forced rerun
                // leaves it alone.
            }
            // A model that found nothing left the cell empty: that is "待补",
            // not "done". `status = 'empty'` is the modern marker; a
            // done-with-empty-value row is the legacy shape of the same thing.
            Some((status, _, _)) if status == "empty" => out.push(col.clone()),
            Some((status, _, value))
                if status != "failed" && !force && status != "empty" && !value.trim().is_empty() => {}
            Some(_) => out.push(col.clone()),
            None => out.push(col.clone()),
        }
    }
    Ok(out)
}

async fn paper_metadata(db: &SqlitePool, paper_id: &str) -> Result<String, String> {
    let row = sqlx::query_as::<_, (String, String, Option<i64>, Option<String>, Option<String>)>(
        "SELECT title, authors, year, journal, abstract FROM papers WHERE id = ?",
    )
    .bind(paper_id)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("db: {e}"))?
    .ok_or_else(|| format!("paper not found: {paper_id}"))?;
    let (title, authors, year, journal, abstract_) = row;
    let mut s = format!("标题：{title}\n作者：{authors}");
    if let Some(y) = year {
        s.push_str(&format!("\n年份：{y}"));
    }
    if let Some(j) = journal.filter(|j| !j.is_empty()) {
        s.push_str(&format!("\n期刊：{j}"));
    }
    if let Some(a) = abstract_.filter(|a| !a.trim().is_empty()) {
        let abs: String = a.chars().take(1500).collect();
        s.push_str(&format!("\n摘要：{abs}"));
    }
    Ok(s)
}

/// Per column, scoped-retrieve the best chunks inside THIS paper; dedupe
/// across columns and cap the total. A paper with no index contributes an
/// empty evidence block (metadata+abstract still go to the model).
/// Build the evidence block for one call: per-column bilingual retrieval, then
/// section-aware ordering so a single long section cannot consume the whole
/// budget. Returns (block, characters used) — 0 characters means the model is
/// being asked to answer without any source text, which the caller reports as
/// "unretrieved" rather than "the paper says nothing".
async fn gather_evidence(
    db: &SqlitePool,
    paper_id: &str,
    columns: &[TableColumn],
    depth: Depth,
    terms: &std::collections::HashMap<String, Vec<String>>,
) -> (String, usize) {
    let ids = [paper_id.to_string()];
    let mut seen = std::collections::HashSet::new();
    let mut picked: Vec<retriever::SearchResult> = Vec::new();

    for col in columns {
        // The column prompt doubles as the retrieval query; the expanded terms
        // make a Chinese column reachable inside an English paper.
        let base = if col.prompt.trim().is_empty() { col.key.clone() } else { col.prompt.clone() };
        let extra = terms.get(&col.key).map(|t| t.join(" ")).unwrap_or_default();
        let query = format!("{base} {extra}").trim().to_string();
        let hits = retriever::hybrid_search_scoped(
            db,
            &query,
            depth.chunks_per_column() * 3,
            false,
            Some(&ids),
        )
        .await
        .unwrap_or_default();
        for hit in hits {
            if seen.insert(hit.chunk_id.clone()) {
                picked.push(hit);
            }
        }
    }

    if picked.is_empty() {
        return ("（该文献没有全文索引，或检索未命中任何段落）".to_string(), 0);
    }

    // Neighbours carry the sentence before/after a hit — cheap continuity.
    retriever::expand_with_neighbors(db, &mut picked, false).await.ok();
    let ordered = order_by_section(picked, depth.chunks_per_column());

    let mut used = 0usize;
    let mut blocks: Vec<String> = Vec::new();
    for hit in ordered {
        let remaining = depth.evidence_cap().saturating_sub(used);
        if remaining == 0 {
            break;
        }
        let content: String = hit.content.chars().take(remaining.min(1400)).collect();
        used += content.chars().count();
        let page = hit.page_start.map(|p| format!("p.{p}")).unwrap_or_default();
        let section = hit
            .section_path
            .clone()
            .or(hit.section.clone())
            .unwrap_or_default();
        let label = if section.is_empty() {
            page
        } else {
            format!("{page} §{section}")
        };
        blocks.push(format!("[{label}]\n{content}"));
    }
    (blocks.join("\n\n"), used)
}

/// Round-robin across sections: hit i of each section before hit i+1 of any, so
/// the evidence spans the paper (intro/method/results/limitations) instead of
/// being three chunks of the same paragraph.
fn order_by_section(
    hits: Vec<retriever::SearchResult>,
    per_section_cap: usize,
) -> Vec<retriever::SearchResult> {
    let mut groups: Vec<(String, Vec<retriever::SearchResult>)> = Vec::new();
    for hit in hits {
        let key = hit
            .section_path
            .clone()
            .or(hit.section.clone())
            .unwrap_or_default();
        match groups.iter_mut().find(|(k, _)| k == &key) {
            Some((_, list)) => list.push(hit),
            None => groups.push((key, vec![hit])),
        }
    }
    let mut out: Vec<retriever::SearchResult> = Vec::new();
    let mut round = 0usize;
    loop {
        let mut pushed = false;
        for (_, list) in groups.iter() {
            if round < list.len() && round < per_section_cap {
                out.push(list[round].clone());
                pushed = true;
            }
        }
        if !pushed {
            break;
        }
        round += 1;
    }
    out
}

fn norm_key(key: &str) -> String {
    key.chars()
        .filter(|c| !c.is_whitespace() && *c != '\u{3000}')
        .filter(|c| !matches!(c, '"' | '\'' | '`' | '“' | '”' | '「' | '」' | '『' | '』' | '【' | '】' | '[' | ']'))
        .collect()
}

/// Look a column up in the model's JSON and return the raw entry, so both the
/// legacy string form and the `{v,src,conf}` object form are handled.
fn lookup_entry<'a>(
    values: &'a serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Option<&'a serde_json::Value> {
    if let Some(v) = values.get(key) {
        return Some(v);
    }
    let target = norm_key(key);
    values.iter().find(|(k, _)| norm_key(k) == target).map(|(_, v)| v)
}

/// How much source text this paper actually has: (indexed chunk count, abstract
/// chars). All-empty results are almost always explained by these two numbers.
async fn source_stats(db: &SqlitePool, paper_id: &str) -> (i64, usize) {
    let chunks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chunks WHERE paper_id = ?")
        .bind(paper_id)
        .fetch_one(db)
        .await
        .unwrap_or(0);
    let abstract_len: usize = sqlx::query_scalar::<_, Option<String>>(
        "SELECT abstract FROM papers WHERE id = ?",
    )
    .bind(paper_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .flatten()
    .map(|a| a.trim().chars().count())
    .unwrap_or(0);
    (chunks, abstract_len)
}

/// Parse the model's JSON object, tolerating ```json fences and prose around
/// the object.
fn parse_values(content: &str) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let trimmed = content.trim();
    let unfenced = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|s| s.strip_suffix("```"))
        .unwrap_or(trimmed)
        .trim();
    let start = unfenced.find('{');
    let end = unfenced.rfind('}');
    match (start, end) {
        (Some(s), Some(e)) if e > s => match serde_json::from_str::<serde_json::Value>(&unfenced[s..=e]) {
            Ok(serde_json::Value::Object(map)) => Ok(map),
            _ => Err("模型未返回 JSON 对象".to_string()),
        },
        _ => Err("模型未返回 JSON 对象".to_string()),
    }
}

async fn upsert_cell(
    db: &SqlitePool,
    paper_id: &str,
    column_key: &str,
    value: &str,
    status: &str,
    edited: bool,
    field: &FieldValue,
    now: &str,
) -> Result<(), String> {
    let meta = serde_json::json!({ "src": field.src, "conf": field.conf }).to_string();
    sqlx::query(
        "INSERT INTO ai_table_cells (paper_id, column_key, value, status, edited, meta_json, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (paper_id, column_key) DO UPDATE SET
           value = excluded.value, status = excluded.status, edited = excluded.edited,
           meta_json = excluded.meta_json, updated_at = excluded.updated_at",
    )
    .bind(paper_id)
    .bind(column_key)
    .bind(value)
    .bind(status)
    .bind(if edited { 1 } else { 0 })
    .bind(&meta)
    .bind(now)
    .bind(now)
    .execute(db)
    .await
    .map_err(|e| format!("db: {e}"))?;
    Ok(())
}

async fn mark_failed(db: &SqlitePool, paper_id: &str, columns: &[TableColumn]) -> Result<(), String> {
    let now = crate::core::time::now_iso();
    for col in columns {
        // Only fill cells that are still missing — never clobber done/edited.
        sqlx::query(
            "INSERT INTO ai_table_cells (paper_id, column_key, value, status, edited, created_at, updated_at)
             VALUES (?, ?, '', 'failed', 0, ?, ?)
             ON CONFLICT (paper_id, column_key) DO NOTHING",
        )
        .bind(paper_id)
        .bind(&col.key)
        .bind(&now)
        .bind(&now)
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    }
    Ok(())
}

/// Cells for a (paper set × column set) grid, in input order.
pub async fn list_cells(
    db: &SqlitePool,
    paper_ids: &[String],
    column_keys: &[String],
) -> Result<Vec<TableCell>, String> {
    if paper_ids.is_empty() || column_keys.is_empty() {
        return Ok(Vec::new());
    }
    let p_ph = paper_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let c_ph = column_keys.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT paper_id, column_key, value, status, edited, meta_json FROM ai_table_cells
         WHERE paper_id IN ({p_ph}) AND column_key IN ({c_ph})"
    );
    let mut q = sqlx::query_as::<_, (String, String, String, String, i64, String)>(&sql);
    for id in paper_ids {
        q = q.bind(id);
    }
    for key in column_keys {
        q = q.bind(key);
    }
    let rows = q.fetch_all(db).await.map_err(|e| format!("db: {e}"))?;
    Ok(rows
        .into_iter()
        .map(|(paper_id, column_key, value, status, edited, meta_json)| {
            let meta: serde_json::Value = serde_json::from_str(&meta_json).unwrap_or_default();
            TableCell {
                paper_id,
                column_key,
                value,
                status,
                edited: edited != 0,
                src: meta
                    .get("src")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                conf: meta.get("conf").and_then(|v| v.as_f64()).unwrap_or(0.0),
            }
        })
        .collect())
}

/// Manual cell edit: marks the cell `edited` so later extraction runs keep it.
pub async fn set_cell(
    db: &SqlitePool,
    paper_id: &str,
    column_key: &str,
    value: &str,
) -> Result<(), String> {
    let now = crate::core::time::now_iso();
    let field = FieldValue { value: value.to_string(), src: String::new(), conf: 1.0 };
    upsert_cell(db, paper_id, column_key, value, "edited", true, &field, &now).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_values_handles_fences_and_prose() {
        let v = parse_values("```json\n{\"样本量\": \"120\", \"方法\": \"RCT\"}\n```").unwrap();
        assert_eq!(v["样本量"], "120");
        assert_eq!(v["方法"], "RCT");

        let v = parse_values("结果如下：{\"a\": \"1\"} 以上。").unwrap();
        assert_eq!(v["a"], "1");
    }

    #[test]
    fn lookup_value_tolerates_model_echoed_keys() {
        // The model often echoes the key with spaces / brackets / quotes.
        let json = serde_json::json!({
            " 研究问题 ": "问题 A",
            "【动机缺口】": "缺口 B",
            "核心洞察": "洞察 C"
        });
        let map = json.as_object().unwrap();
        assert_eq!(
            lookup_entry(map, "研究问题").and_then(|v| v.as_str()),
            Some("问题 A")
        );
        assert_eq!(
            lookup_entry(map, "动机缺口").and_then(|v| v.as_str()),
            Some("缺口 B")
        );
        // Object form ({v,src,conf}) parses too.
        let obj = serde_json::json!({ "主要发现": { "v": "提升 12%", "src": "p.9", "conf": 0.8 } });
        let field = parse_field(lookup_entry(obj.as_object().unwrap(), "主要发现").unwrap());
        assert_eq!(field.value, "提升 12%");
        assert_eq!(field.src, "p.9");
        assert!((field.conf - 0.8).abs() < 1e-6);
        // Genuinely absent stays absent (never a silent blank cell).
        assert!(lookup_entry(map, "主要结果").is_none());
    }

    #[test]
    fn depth_parsing_and_cost_shape() {
        assert_eq!(Depth::parse(None), Depth::Standard);
        assert_eq!(Depth::parse(Some("fast")), Depth::Fast);
        assert_eq!(Depth::parse(Some("deep")), Depth::Deep);
        assert_eq!(Depth::parse(Some("per_column")), Depth::PerColumn);
        // Deeper modes look at more, and cost more calls.
        assert!(Depth::Deep.chunks_per_column() > Depth::Fast.chunks_per_column());
        assert!(Depth::Deep.evidence_cap() > Depth::Standard.evidence_cap());
        assert!(!Depth::Fast.second_pass());
        assert!(Depth::Standard.second_pass());
        assert!(!Depth::Standard.per_column_calls());
        assert!(Depth::PerColumn.per_column_calls());
    }

    #[test]
    fn empty_value_distinguishes_absent_from_unretrieved() {
        let blank = FieldValue::default();
        // No source text reached the model → OUR problem, retryable.
        assert_eq!(cell_state(&blank, 0), CellState::Unretrieved);
        // Evidence was there and the model says the paper has no such content.
        let confident = FieldValue { conf: 0.9, ..Default::default() };
        assert_eq!(cell_state(&confident, 5000), CellState::Absent);
        // Evidence was there but the model could not decide.
        let unsure = FieldValue { conf: 0.0, ..Default::default() };
        assert_eq!(cell_state(&unsure, 5000), CellState::Unretrieved);
        // Anything non-empty is filled regardless of confidence.
        let filled = FieldValue { value: "120 名开发者".into(), conf: 0.1, ..Default::default() };
        assert_eq!(cell_state(&filled, 0), CellState::Filled);
    }

    #[test]
    fn order_by_section_spreads_across_sections() {
        fn hit(id: &str, section: &str) -> crate::ai::retriever::SearchResult {
            crate::ai::retriever::SearchResult {
                chunk_id: id.into(),
                paper_id: "p1".into(),
                chunk_index: 0,
                content: format!("{section} body"),
                page_start: Some(1),
                page_end: Some(1),
                section: Some(section.into()),
                section_path: Some(section.into()),
                block_type: "prose".into(),
                is_tail: false,
                is_neighbor: false,
                paper_title: String::new(),
                score: 1.0,
                source: "fts5".into(),
            }
        }
        let hits = vec![
            hit("a1", "Intro"),
            hit("a2", "Intro"),
            hit("a3", "Intro"),
            hit("b1", "Method"),
            hit("c1", "Threats"),
        ];
        let ordered = order_by_section(hits, 2);
        // First round hits three distinct sections before repeating one.
        let first_three: Vec<&str> = ordered
            .iter()
            .take(3)
            .map(|h| h.section.as_deref().unwrap_or(""))
            .collect();
        assert!(first_three.contains(&"Intro"));
        assert!(first_three.contains(&"Method"));
        assert!(first_three.contains(&"Threats"));
        // Per-section cap is respected (2 of the 3 Intro chunks).
        assert_eq!(ordered.iter().filter(|h| h.section.as_deref() == Some("Intro")).count(), 2);
    }

    #[test]
    fn parse_values_rejects_non_object() {
        assert!(parse_values("[1,2,3]").is_err());
        assert!(parse_values("没有 JSON").is_err());
    }
}
