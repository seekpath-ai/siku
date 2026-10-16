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
    /// Set when the run ends: "done" | "cancelled".
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
    if let Err(e) = app.emit(PROGRESS_EVENT, p) {
        tracing::warn!(error = %e, "failed to emit ai-table progress");
    }
}

/// Total evidence characters fed to the model per paper (metadata+abstract
/// come on top). Keeps small-context models inside their budget.
const EVIDENCE_CHAR_CAP: usize = 6000;
/// Chunks retrieved per column as extraction evidence.
const CHUNKS_PER_COLUMN: usize = 3;

/// Run the batch. `paper_ids` is the selected set, `columns` the column
/// defs. Spawning/cancellation is the command layer's job.
pub async fn run_extraction(
    app: AppHandle,
    db: SqlitePool,
    paper_ids: Vec<String>,
    columns: Vec<TableColumn>,
) {
    let _guard = match RunGuard::try_acquire() {
        Ok(g) => g,
        Err(e) => {
            tracing::warn!(error = %e, "ai-table extraction already running");
            return;
        }
    };

    let llm = match crate::core::llm_provider_service::load_default_llm_config(&db).await {
        Ok(cfg) => match crate::ai::llm::client::create_llm_client(&cfg) {
            Ok(c) => c,
            Err(e) => {
                emit(&app, &ExtractProgress { total: 0, done: 0, failed: 0, current_title: e, finished: Some("done".into()) });
                return;
            }
        },
        Err(e) => {
            emit(&app, &ExtractProgress { total: 0, done: 0, failed: 0, current_title: e, finished: Some("done".into()) });
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
        emit(&app, &ExtractProgress { total, done, failed, current_title: title.clone(), finished: None });

        match extract_one(&db, llm.as_ref(), paper_id, &columns).await {
            Ok(()) => done += 1,
            Err(e) => {
                tracing::warn!(paper_id, error = %e, "ai-table extract failed for paper");
                // Mark every missing cell as failed so the UI can offer a
                // per-cell retry instead of a silently blank grid.
                let _ = mark_failed(&db, paper_id, &columns).await;
                failed += 1;
            }
        }
        emit(&app, &ExtractProgress { total, done, failed, current_title: title, finished: None });
    }

    emit(&app, &ExtractProgress {
        total,
        done,
        failed,
        current_title: String::new(),
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
/// still returns all columns — upsert is selective).
async fn extract_one(
    db: &SqlitePool,
    llm: &dyn crate::ai::llm::LlmClient,
    paper_id: &str,
    columns: &[TableColumn],
) -> Result<(), String> {
    let pending = pending_columns(db, paper_id, columns).await?;
    if pending.is_empty() {
        return Ok(());
    }

    let meta = paper_metadata(db, paper_id).await?;
    let evidence = gather_evidence(db, paper_id, &pending).await?;

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

    let system = "你是学术文献信息抽取引擎。根据给定的文献信息与段落，按用户列出的字段逐一抽取，\
        只输出一个 JSON 对象：键为字段名，值为简短字符串（一般不超过 50 字）；\
        文献中未提及的字段值用空字符串。不要输出 JSON 以外的任何内容。"
        .to_string();
    let user = format!(
        "【文献】\n{meta}\n\n【相关段落】\n{evidence}\n\n【待抽取字段】\n{column_spec}"
    );

    let messages = vec![
        ChatMessage { reasoning_content: None, role: "system".into(), content: system, attachments: None, tool_calls: None, tool_call_id: None, name: None },
        ChatMessage { reasoning_content: None, role: "user".into(), content: user, attachments: None, tool_calls: None, tool_call_id: None, name: None },
    ];
    let resp = llm.chat_completion(&messages, &[]).await?;
    let values = parse_values(&resp.content)?;

    let now = crate::core::time::now_iso();
    for col in &pending {
        let value = values
            .get(&col.key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        upsert_cell(db, paper_id, &col.key, &value, "done", false, &now).await?;
    }
    Ok(())
}

/// Columns whose cell is missing or failed — the ones a run must produce.
async fn pending_columns(
    db: &SqlitePool,
    paper_id: &str,
    columns: &[TableColumn],
) -> Result<Vec<TableColumn>, String> {
    let mut out = Vec::new();
    for col in columns {
        let row = sqlx::query_as::<_, (String, i64)>(
            "SELECT status, edited FROM ai_table_cells WHERE paper_id = ? AND column_key = ?",
        )
        .bind(paper_id)
        .bind(&col.key)
        .fetch_optional(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
        match row {
            // Manually edited or already extracted: keep.
            Some((status, edited)) if edited == 1 || status == "done" => {}
            _ => out.push(col.clone()),
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
) -> Result<String, String> {
    let mut seen = std::collections::HashSet::new();
    let mut blocks: Vec<String> = Vec::new();
    let mut used = 0usize;
    let ids = [paper_id.to_string()];
    for col in columns {
        let query = if col.prompt.trim().is_empty() { &col.key } else { &col.prompt };
        let hits = retriever::hybrid_search_scoped(db, query, CHUNKS_PER_COLUMN, false, Some(&ids))
            .await
            .unwrap_or_default();
        for hit in hits {
            if !seen.insert(hit.chunk_id.clone()) {
                continue;
            }
            let remaining = EVIDENCE_CHAR_CAP.saturating_sub(used);
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
    fn parse_values_rejects_non_object() {
        assert!(parse_values("[1,2,3]").is_err());
        assert!(parse_values("没有 JSON").is_err());
    }
}
