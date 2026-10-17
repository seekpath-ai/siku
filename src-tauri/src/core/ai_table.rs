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
    /// done | failed | edited (edited = user overrode the extracted value).
    pub status: String,
    pub edited: bool,
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

    /// Evidence budget per paper: a richer answer needs more source text.
    fn evidence_cap(self) -> usize {
        match self {
            Detail::Brief => 5000,
            Detail::Normal => 6000,
            Detail::Rich => 9000,
        }
    }

    fn chunks_per_column(self) -> usize {
        match self {
            Detail::Brief | Detail::Normal => 3,
            Detail::Rich => 5,
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

    let total = paper_ids.len();
    let mut done = 0usize;
    let mut failed = 0usize;
    let mut cancelled = false;

    for paper_id in &paper_ids {
        if CANCEL.load(Ordering::SeqCst) {
            cancelled = true;
            break;
        }
        let title = paper_title(&db, paper_id).await;
        emit(&app, &ExtractProgress { total, done, failed, current_title: title.clone(), log: None, reasoning: None, finished: None });

        let started = std::time::Instant::now();
        // Race the model call against the cancel flag: 停止 must take effect
        // while a slow local model is still generating, not minutes later.
        // extract_one races its own call against the cancel flag, so a plain
        // await is enough here.
        let outcome = extract_one(&app, &db, llm.as_ref(), paper_id, &columns, detail, force).await;
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
                    // Cancelled mid-flight: leave the cells as they were so a
                    // later run resumes them instead of seeing "failed".
                    cancelled = true;
                    break;
                }
                tracing::warn!(paper_id, error = %e, "ai-table extract failed for paper");
                // Mark every missing cell as failed so the UI can offer a
                // per-cell retry instead of a silently blank grid.
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

async fn extract_one(
    app: &AppHandle,
    db: &SqlitePool,
    llm: &dyn crate::ai::llm::LlmClient,
    paper_id: &str,
    columns: &[TableColumn],
    detail: Detail,
    force: bool,
) -> Result<ExtractOutcome, String> {
    let pending = pending_columns(db, paper_id, columns, force).await?;
    if pending.is_empty() {
        return Ok(ExtractOutcome { skipped: true, empty_note: None });
    }

    let meta = paper_metadata(db, paper_id).await?;
    let evidence = gather_evidence(db, paper_id, &pending, detail).await?;

    let column_spec = pending
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

    // The length rule scales with the requested detail level; an explicit
    // instruction in the column prompt always wins over it.
    let system = crate::core::task_prompts::render(
        &crate::core::task_prompts::effective(db, crate::core::task_prompts::AI_TABLE_EXTRACT).await,
        &[("detail_rule", detail.length_rule())],
    );
    let user = format!(
        "【文献】\n{meta}\n\n【相关段落】\n{evidence}\n\n【待抽取字段】\n{column_spec}"
    );

    let messages = vec![
        ChatMessage { reasoning_content: None, role: "system".into(), content: system, attachments: None, tool_calls: None, tool_call_id: None, name: None },
        ChatMessage { reasoning_content: None, role: "user".into(), content: user, attachments: None, tool_calls: None, tool_call_id: None, name: None },
    ];

    // Stream the call: a thinking model spends most of its time reasoning, and
    // a non-streaming request threw that away — the panel showed nothing for
    // minutes, which read as "hang". Reasoning deltas go straight to the panel.
    let title = paper_title(db, paper_id).await;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<crate::ai::llm::StreamEvent>();
    let app2 = app.clone();
    let title2 = title.clone();
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

    let now = crate::core::time::now_iso();
    let mut written = 0usize;
    let mut blanks: Vec<String> = Vec::new();
    for col in &pending {
        let value = lookup_value(&values, &col.key)
            .unwrap_or_default()
            .trim()
            .to_string();
        if value.is_empty() {
            blanks.push(col.key.clone());
        } else {
            written += 1;
        }
        // An empty result is NOT a finished cell: storing it as `done` used to
        // make the cell permanently un-refillable ("重新抽取缺失" skipped it).
        let status = if value.is_empty() { "empty" } else { "done" };
        upsert_cell(db, paper_id, &col.key, &value, status, false, &now).await?;
    }

    // Everything came back empty: say WHY instead of leaving a mute 待补. The
    // two things that actually explain it are the source size (no index / no
    // abstract) and the keys the model returned (usually a naming mismatch).
    let empty_note = if written == 0 {
        let (chunks, abstract_len) = source_stats(db, paper_id).await;
        let got: Vec<String> = values.keys().cloned().collect();
        let want: Vec<String> = pending.iter().map(|c| c.key.clone()).collect();
        let snippet: String = text.chars().take(160).collect();
        let mut line = format!(
            "⚠ 《{title}》没抽到内容：来源=摘要 {abstract_len} 字 / 全文索引 {chunks} 段；模型返回键 {got:?}，期望键 {want:?}"
        );
        if chunks == 0 && abstract_len < 100 {
            line.push_str("。⚠ 该文献没有全文索引且摘要过短，模型无从抽取——请先「重建索引」或补全摘要");
        }
        if !got.is_empty() && got.len() != want.len() {
            line.push_str("。键名不一致是常见原因（已按去空格/去引号匹配，仍失败则是模型改写了字段名）");
        }
        line.push_str(&format!("。原始返回：{snippet}"));
        Some(line)
    } else if !blanks.is_empty() {
        Some(format!("· 《{title}》部分字段为空：{}", blanks.join("、")))
    } else {
        None
    };

    Ok(ExtractOutcome { skipped: false, empty_note })
}

/// Columns whose cell is missing or failed — the ones a run must produce.
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
async fn gather_evidence(
    db: &SqlitePool,
    paper_id: &str,
    columns: &[TableColumn],
    detail: Detail,
) -> Result<String, String> {
    let mut seen = std::collections::HashSet::new();
    let mut blocks: Vec<String> = Vec::new();
    let mut used = 0usize;
    let ids = [paper_id.to_string()];
    for col in columns {
        let query = if col.prompt.trim().is_empty() { &col.key } else { &col.prompt };
        let hits = retriever::hybrid_search_scoped(db, query, detail.chunks_per_column(), false, Some(&ids))
            .await
            .unwrap_or_default();
        for hit in hits {
            if !seen.insert(hit.chunk_id.clone()) {
                continue;
            }
            let remaining = detail.evidence_cap().saturating_sub(used);
            if remaining == 0 {
                break;
            }
            let content: String = hit.content.chars().take(remaining.min(1200)).collect();
            used += content.chars().count();
            let page = hit.page_start.map(|p| format!("p.{p}")).unwrap_or_default();
            blocks.push(format!("[{page} {}]\n{content}", hit.section_path.unwrap_or_default()));
        }
    }
    Ok(if blocks.is_empty() {
        "（该文献未建立全文索引，仅依据元数据与摘要抽取）".to_string()
    } else {
        blocks.join("\n\n")
    })
}

/// Column keys as the model may echo them: surrounding quotes/brackets and
/// whitespace (incl. full-width) removed.
fn norm_key(key: &str) -> String {
    key.chars()
        .filter(|c| !c.is_whitespace() && *c != '\u{3000}')
        .filter(|c| !matches!(c, '"' | '\'' | '`' | '“' | '”' | '「' | '」' | '『' | '』' | '【' | '】' | '[' | ']'))
        .collect()
}

/// Look a column up in the model's JSON: exact key first, then a normalised
/// match (models echo `"研究问题 "` or `【研究问题】` often enough that an exact
/// match silently produced an empty cell).
fn lookup_value(values: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
    if let Some(v) = values.get(key).and_then(|v| v.as_str()) {
        return Some(v.to_string());
    }
    let target = norm_key(key);
    values
        .iter()
        .find(|(k, _)| norm_key(k) == target)
        .and_then(|(_, v)| v.as_str())
        .map(|v| v.to_string())
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
    now: &str,
) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO ai_table_cells (paper_id, column_key, value, status, edited, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (paper_id, column_key) DO UPDATE SET
           value = excluded.value, status = excluded.status, edited = excluded.edited,
           updated_at = excluded.updated_at",
    )
    .bind(paper_id)
    .bind(column_key)
    .bind(value)
    .bind(status)
    .bind(if edited { 1 } else { 0 })
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
        "SELECT paper_id, column_key, value, status, edited FROM ai_table_cells
         WHERE paper_id IN ({p_ph}) AND column_key IN ({c_ph})"
    );
    let mut q = sqlx::query_as::<_, (String, String, String, String, i64)>(&sql);
    for id in paper_ids {
        q = q.bind(id);
    }
    for key in column_keys {
        q = q.bind(key);
    }
    let rows = q.fetch_all(db).await.map_err(|e| format!("db: {e}"))?;
    Ok(rows
        .into_iter()
        .map(|(paper_id, column_key, value, status, edited)| TableCell {
            paper_id,
            column_key,
            value,
            status,
            edited: edited != 0,
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
    upsert_cell(db, paper_id, column_key, value, "edited", true, &now).await
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
        assert_eq!(lookup_value(map, "研究问题").as_deref(), Some("问题 A"));
        assert_eq!(lookup_value(map, "动机缺口").as_deref(), Some("缺口 B"));
        assert_eq!(lookup_value(map, "核心洞察").as_deref(), Some("洞察 C"));
        // Genuinely absent stays absent (never a silent blank cell).
        assert_eq!(lookup_value(map, "主要结果"), None);
    }

    #[test]
    fn parse_values_rejects_non_object() {
        assert!(parse_values("[1,2,3]").is_err());
        assert!(parse_values("没有 JSON").is_err());
    }
}
