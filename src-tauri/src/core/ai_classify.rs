//! AI 文献分类: draft a taxonomy over a paper set, assign every paper to it,
//! then write the confirmed result back into collections / tags.
//!
//! Design follows what the mature tools converge on (see docs/ai-classify-plan.md):
//!
//! 1. **Draft → confirm → assign.** Phase 1 buckets the papers into named
//!    groups (one LLM call per batch + one merge call). The draft is stored and
//!    is the ONLY authority for the taxonomy: the user renames / merges / drops
//!    groups in the dialog, then phase 2 assigns papers against that closed set.
//!    Nothing is written to the library until [`apply_run`].
//! 2. **Closed-set assignment.** [`run_reassign`] never invents a group — it
//!    may only pick from the confirmed draft, plus an empty "待定" outcome. This
//!    is what keeps a rerun from growing a hundred near-duplicate folders.
//! 3. **Revertible.** Every row [`apply_run`] writes is logged in
//!    `ai_classify_applied` (including whether the target and the link were
//!    created by this run), so [`undo_run`] can take the whole run back out.
//! 4. **Sample first.** `sample_size` limits phase 1 to the first N papers so a
//!    long run can be judged on 10 papers before paying for the whole library.
//!
//! P1 adds: two-level categories (AI proposes parents in one extra call), multi
//! label assignment for the tag target, confidence triage (high → accepted,
//! low → pending review), a per-dimension controlled vocabulary that remembers
//! promoted terms and stops re-proposing rejected ones, and cross-run
//! incremental runs that skip papers already filed under the same dimension.
//!
//! Progress/cancellation mirrors core::ai_table (one run at a time, cancel
//! between batches, per-batch progress events).

use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tauri::{AppHandle, Emitter};

use crate::ai::llm::ChatMessage;

pub const PROGRESS_EVENT: &str = "library:ai-classify-progress";

static RUNNING: AtomicBool = AtomicBool::new(false);
static CANCEL: AtomicBool = AtomicBool::new(false);

/// Papers per LLM call while drafting the taxonomy.
const DRAFT_BATCH: usize = 24;
/// Papers per LLM call while assigning against a confirmed taxonomy.
const ASSIGN_BATCH: usize = 20;
const DRAFT_ABSTRACT_CAP: usize = 400;
const ASSIGN_ABSTRACT_CAP: usize = 700;
/// AI-created tags get one colour so they read as a group in the tag tree and
/// can be spotted (and bulk-deleted) at a glance.
const TAG_COLOR: &str = "#8b5cf6";
/// Existing categories offered to the model as a preferred vocabulary.
const VOCAB_CAP: usize = 60;
/// Tags are multi-label; collections are single-folder, so this only applies to
/// the tag target. More than three labels stops being a useful signal.
const MAX_LABELS: usize = 3;
/// Two-level drafts: how many parents the hierarchize call may propose.
const MAX_PARENTS: usize = 6;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// One node of the (user-editable) taxonomy draft. Two levels max: a group's
/// children are leaves.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassifyGroup {
    /// Display name; the last path segment.
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Existing collection/tag id this group was matched to by its full path
    /// (reuse mode). None = applying this group creates a new category.
    #[serde(default)]
    pub existing_id: Option<String>,
    #[serde(default)]
    pub children: Vec<ClassifyGroup>,
}

impl ClassifyGroup {
    pub(crate) fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self { name: name.into(), description: description.into(), existing_id: None, children: Vec::new() }
    }

    fn is_leaf(&self) -> bool {
        self.children.is_empty()
    }
}

/// Every path in the draft, parents included — the closed set an assignment may
/// pick from. `父/子` for a child, `父` for a top-level group.
fn all_paths(groups: &[ClassifyGroup]) -> Vec<String> {
    let mut out = Vec::new();
    for g in groups {
        let name = g.name.trim();
        if name.is_empty() {
            continue;
        }
        out.push(name.to_string());
        for c in &g.children {
            let child = c.name.trim();
            if !child.is_empty() {
                out.push(format!("{name}/{child}"));
            }
        }
    }
    out
}

/// Top-level names + `父/子` paths, deduped and capped — the payload the
/// dialog's process panel renders while the draft is still running.
fn group_names(groups: &[ClassifyGroup]) -> Vec<String> {
    const CAP: usize = 40;
    let mut out: Vec<String> = Vec::new();
    for g in groups {
        let name = g.name.trim();
        if name.is_empty() {
            continue;
        }
        out.push(name.to_string());
        for c in &g.children {
            let child = c.name.trim();
            if !child.is_empty() {
                out.push(format!("{name}/{child}"));
            }
        }
        if out.len() >= CAP {
            break;
        }
    }
    out.truncate(CAP);
    out
}

fn paths_of(groups: &[ClassifyGroup]) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    for g in groups {
        let name = g.name.trim();
        if name.is_empty() {
            continue;
        }
        out.push((name.to_string(), g.existing_id.clone()));
        for c in &g.children {
            let child = c.name.trim();
            if !child.is_empty() {
                out.push((format!("{name}/{child}"), c.existing_id.clone()));
            }
        }
    }
    out
}

/// Assign a path's `existing_id` from a path→id map (built from the library).
fn attach_existing(groups: &mut [ClassifyGroup], existing: &[(String, String)]) {
    for (path, id) in existing {
        let key = norm(path);
        for g in groups.iter_mut() {
            if norm(g.name.trim()) == key {
                g.existing_id = Some(id.clone());
            }
            for c in g.children.iter_mut() {
                let child_path = format!("{}/{}", g.name.trim(), c.name.trim());
                if norm(&child_path) == key {
                    c.existing_id = Some(id.clone());
                }
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassifyItem {
    pub paper_id: String,
    pub title: String,
    pub year: Option<i64>,
    /// Primary path (first label); empty = 待定.
    pub group_name: String,
    /// All assigned paths (single element for the collection target).
    pub labels: Vec<String>,
    pub confidence: f64,
    pub reason: String,
    /// accepted | pending | failed
    pub status: String,
    /// User overrode the suggestion; a rerun keeps it.
    pub edited: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassifyRun {
    pub id: String,
    pub dimension: String,
    pub target: String,
    pub scope_kind: String,
    pub scope_id: Option<String>,
    pub sample_size: Option<u32>,
    pub paper_count: usize,
    pub threshold: f64,
    pub two_level: bool,
    pub skip_existing: bool,
    pub groups: Vec<ClassifyGroup>,
    pub items: Vec<ClassifyItem>,
    pub status: String,
    pub applied_at: Option<String>,
    pub undone_at: Option<String>,
}

/// Everything the dialog collects in step 1.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassifyRequest {
    pub paper_ids: Vec<String>,
    #[serde(default)]
    pub scope_kind: String,
    #[serde(default)]
    pub scope_id: Option<String>,
    pub dimension: String,
    /// collection | tag | preview
    #[serde(default)]
    pub target: String,
    /// Draft only the first N papers (sample-first).
    #[serde(default)]
    pub sample_size: Option<u32>,
    /// Offer the library's existing categories to the model as the preferred
    /// vocabulary, and auto-match groups to them by path.
    #[serde(default)]
    pub reuse_existing: bool,
    #[serde(default)]
    pub max_groups: u32,
    /// Ask the model for a second level (one extra call after the draft).
    #[serde(default)]
    pub two_level: bool,
    /// Assignments at or above this confidence are accepted automatically.
    #[serde(default)]
    pub threshold: f64,
    /// Skip papers that a previous applied run already filed under the same
    /// dimension + target.
    #[serde(default)]
    pub skip_existing: bool,
    /// Draft engine: `batch` (LLM bucketing) or `semantic` (embedding clusters
    /// + one naming call). Semantic falls back to batch when unavailable.
    #[serde(default)]
    pub engine: String,
    #[serde(default)]
    pub provider_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassifyProgress {
    /// draft | merge | hierarchize | assign | done | cancelled | error
    pub stage: String,
    pub total: usize,
    pub done: usize,
    pub failed: usize,
    /// Paper title (assign) or batch label (draft) currently being processed.
    pub current: String,
    pub group_count: usize,
    /// Category paths proposed so far (deduped, capped). The config-step
    /// process panel shows categories appearing batch by batch instead of a
    /// mute spinner.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub groups_preview: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
    /// Set when the run ends: "done" | "cancelled" | "error".
    pub finished: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyTarget {
    /// Full path (`父/子`) of the group to write.
    pub group_name: String,
    /// Create the category when no existing one matched.
    #[serde(default)]
    pub create: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyPlan {
    pub targets: Vec<ApplyTarget>,
    /// New categories become children of this collection. None = they go under
    /// the run's own root collection (`AI 分类 · <dimension>`).
    #[serde(default)]
    pub parent_collection_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplySummary {
    pub created_targets: usize,
    pub reused_targets: usize,
    pub linked_papers: usize,
    pub skipped_papers: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoSummary {
    pub removed_links: u64,
    pub removed_targets: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaperBrief {
    pub id: String,
    pub title: String,
    pub year: Option<i64>,
    pub journal: Option<String>,
    pub abstract_text: String,
}

/// One row of the controlled vocabulary for a (dimension, target) pair.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VocabEntry {
    pub id: String,
    /// `父/子` for a child.
    pub path: String,
    /// active | staged | rejected
    pub status: String,
    /// ai | user | library
    pub source: String,
    pub created_at: String,
}

pub fn request_cancel() {
    CANCEL.store(true, Ordering::SeqCst);
}

/// Resolve once a cancel is requested — polled so an in-flight model call can be
/// abandoned instead of running to completion. (A `Notify` permit would leak
/// into the next run and cancel it instantly; 200 ms is imperceptible.)
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
            return Err("已有分类任务正在进行中".to_string());
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

fn emit(app: &AppHandle, p: &ClassifyProgress) {
    if let Err(e) = app.emit(PROGRESS_EVENT, p) {
        tracing::warn!(error = %e, "failed to emit ai-classify progress");
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
    min_max_tokens: u32,
) -> Result<Box<dyn crate::ai::llm::LlmClient>, String> {
    let mut cfg = crate::core::llm_provider_service::load_llm_config_for(db, provider_id).await?;
    // Thinking models spend max_tokens on reasoning before emitting any JSON;
    // a small provider cap otherwise ends the call with an empty body.
    let need = crate::core::task_prompts::apply_floor(
        min_max_tokens.saturating_mul(2).saturating_add(4000),
        crate::core::task_prompts::budget_floor(db, "classify").await,
    );
    if cfg.max_tokens < need {
        cfg.max_tokens = need;
    }
    crate::ai::llm::client::create_llm_client(&cfg)
}

fn norm(name: &str) -> String {
    name.trim().to_lowercase()
}

// ---------------------------------------------------------------------------
// Papers
// ---------------------------------------------------------------------------

/// id/title/year/journal/abstract for the whole set in one round trip.
pub async fn papers_info(db: &SqlitePool, paper_ids: &[String]) -> Result<Vec<PaperBrief>, String> {
    if paper_ids.is_empty() {
        return Ok(Vec::new());
    }
    let ph = paper_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT id, title, year, journal, abstract FROM papers
         WHERE id IN ({ph}) AND deleted_at IS NULL"
    );
    let mut q = sqlx::query_as::<_, (String, String, Option<i64>, Option<String>, Option<String>)>(&sql);
    for id in paper_ids {
        q = q.bind(id);
    }
    let rows = q.fetch_all(db).await.map_err(|e| format!("db: {e}"))?;
    let mut map: std::collections::HashMap<String, PaperBrief> = rows
        .into_iter()
        .map(|(id, title, year, journal, abstract_text)| {
            let brief = PaperBrief {
                id: id.clone(),
                title,
                year,
                journal,
                abstract_text: abstract_text.unwrap_or_default(),
            };
            (id, brief)
        })
        .collect();
    Ok(paper_ids.iter().filter_map(|id| map.remove(id)).collect())
}

fn paper_block(papers: &[PaperBrief], abstract_cap: usize) -> String {
    papers
        .iter()
        .map(|p| {
            let year = p.year.map(|y| y.to_string()).unwrap_or_default();
            let abs: String = p.abstract_text.chars().take(abstract_cap).collect();
            let abs = if abs.trim().is_empty() { "（无摘要）".to_string() } else { abs };
            format!("- id={} | {} ({})\n  摘要：{}", p.id, p.title, year, abs)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
// JSON parsing (tolerant: fences, prose, string-or-object paper entries)
// ---------------------------------------------------------------------------

fn json_body(content: &str) -> Option<&str> {
    let start = content.find('{')?;
    let end = content.rfind('}')?;
    if end > start {
        Some(&content[start..=end])
    } else {
        None
    }
}

#[derive(Deserialize)]
struct RawDraftGroup {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    papers: Vec<RawDraftPaper>,
}

#[derive(Deserialize)]
struct RawDraftPaperItem {
    #[serde(alias = "paper_id", alias = "id")]
    paper: String,
    #[serde(default)]
    confidence: f64,
    #[serde(default)]
    reason: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawDraftPaper {
    Detailed(RawDraftPaperItem),
    Bare(String),
}

impl RawDraftPaper {
    fn into_parts(self) -> (String, f64, String) {
        match self {
            RawDraftPaper::Detailed(d) => (d.paper, d.confidence, d.reason),
            RawDraftPaper::Bare(id) => (id, 0.0, String::new()),
        }
    }
}

#[derive(Deserialize)]
struct RawDraft {
    #[serde(default)]
    groups: Vec<RawDraftGroup>,
}

#[derive(Deserialize)]
struct RawMergeGroup {
    name: String,
    #[serde(default)]
    description: String,
    /// Draft group names this canonical group absorbs.
    #[serde(default)]
    from: Vec<String>,
}

#[derive(Deserialize)]
struct RawMerge {
    #[serde(default)]
    groups: Vec<RawMergeGroup>,
}

#[derive(Deserialize)]
struct RawParent {
    name: String,
    #[serde(default)]
    description: String,
    /// Names of existing draft groups that belong under this parent.
    #[serde(default, alias = "groups")]
    children: Vec<String>,
}

#[derive(Deserialize)]
struct RawHierarchy {
    #[serde(default)]
    parents: Vec<RawParent>,
}

#[derive(Deserialize)]
struct RawAssignItem {
    #[serde(alias = "paper_id", alias = "id")]
    paper: String,
    #[serde(default, alias = "group_name", alias = "label")]
    group: String,
    /// Multi-label output (tag target). Falls back to `group`.
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    confidence: f64,
    #[serde(default)]
    reason: String,
}

#[derive(Deserialize)]
struct RawAssign {
    #[serde(default)]
    assignments: Vec<RawAssignItem>,
}

/// One assignment as produced by either phase. `labels` is never empty for a
/// real assignment; an empty list means 待定.
struct Assign {
    paper_id: String,
    labels: Vec<String>,
    confidence: f64,
    reason: String,
}

impl Assign {
    fn primary(&self) -> String {
        self.labels.first().cloned().unwrap_or_default()
    }
}

fn parse_draft(content: &str) -> Result<(Vec<ClassifyGroup>, Vec<Assign>), String> {
    let body = json_body(content).ok_or_else(|| "模型未返回 JSON 对象".to_string())?;
    let raw: RawDraft = serde_json::from_str(body).map_err(|e| format!("解析草案失败: {e}"))?;
    let mut groups = Vec::new();
    let mut assigns = Vec::new();
    for g in raw.groups {
        let name = g.name.trim().to_string();
        if name.is_empty() {
            continue;
        }
        for p in g.papers {
            let (paper_id, confidence, reason) = p.into_parts();
            let paper_id = paper_id.trim().to_string();
            if paper_id.is_empty() {
                continue;
            }
            assigns.push(Assign {
                paper_id,
                labels: vec![name.clone()],
                confidence,
                reason: reason.trim().to_string(),
            });
        }
        if !groups.iter().any(|x: &ClassifyGroup| norm(&x.name) == norm(&name)) {
            groups.push(ClassifyGroup::new(name, g.description.trim()));
        }
    }
    if groups.is_empty() {
        return Err("模型未返回有效分类".to_string());
    }
    Ok((groups, assigns))
}

fn parse_merge(content: &str) -> Result<Vec<RawMergeGroup>, String> {
    let body = json_body(content).ok_or_else(|| "模型未返回 JSON 对象".to_string())?;
    let raw: RawMerge = serde_json::from_str(body).map_err(|e| format!("解析归并结果失败: {e}"))?;
    let groups: Vec<RawMergeGroup> = raw
        .groups
        .into_iter()
        .filter(|g| !g.name.trim().is_empty())
        .collect();
    if groups.is_empty() {
        return Err("模型未返回有效类目".to_string());
    }
    Ok(groups)
}

fn parse_hierarchy(content: &str) -> Result<Vec<RawParent>, String> {
    let body = json_body(content).ok_or_else(|| "模型未返回 JSON 对象".to_string())?;
    let raw: RawHierarchy =
        serde_json::from_str(body).map_err(|e| format!("解析层级失败: {e}"))?;
    let parents: Vec<RawParent> = raw
        .parents
        .into_iter()
        .filter(|p| !p.name.trim().is_empty() && !p.children.is_empty())
        .collect();
    if parents.is_empty() {
        return Err("模型未返回层级".to_string());
    }
    Ok(parents)
}

fn parse_assign(content: &str) -> Result<Vec<Assign>, String> {
    let body = json_body(content).ok_or_else(|| "模型未返回 JSON 对象".to_string())?;
    let raw: RawAssign = serde_json::from_str(body).map_err(|e| format!("解析判定结果失败: {e}"))?;
    Ok(raw
        .assignments
        .into_iter()
        .map(|a| {
            let mut labels: Vec<String> = a
                .labels
                .into_iter()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect();
            let single = a.group.trim().to_string();
            if labels.is_empty() && !single.is_empty() {
                labels.push(single);
            }
            let mut seen = std::collections::HashSet::new();
            labels.retain(|l| seen.insert(norm(l)));
            labels.truncate(MAX_LABELS);
            Assign {
                paper_id: a.paper.trim().to_string(),
                labels,
                confidence: a.confidence,
                reason: a.reason.trim().to_string(),
            }
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Controlled vocabulary (per dimension + target)
// ---------------------------------------------------------------------------

/// Library categories as `(path, id)` pairs, path-shaped so nesting matches the
/// draft (`父/子`). For a collection run the scope collection's children are
/// offered (so new groups land next to their siblings); without a scope the
/// library's roots are. For tags the `#`-separated name is the path.
async fn existing_targets(
    db: &SqlitePool,
    target: &str,
    scope_id: Option<&str>,
) -> Result<Vec<(String, String)>, String> {
    if target == "tag" {
        let rows = sqlx::query_as::<_, (String, String)>(
            "SELECT id, name FROM tags ORDER BY name LIMIT ?",
        )
        .bind(VOCAB_CAP as i64)
        .fetch_all(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
        return Ok(rows
            .into_iter()
            .map(|(id, name)| (name.split('#').map(str::trim).collect::<Vec<_>>().join("/"), id))
            .collect());
    }

    let rows = match scope_id {
        Some(id) => {
            sqlx::query_as::<_, (String, String)>(
                "SELECT id, name FROM collections WHERE parent_id = ? ORDER BY sort_order, name LIMIT ?",
            )
            .bind(id)
            .bind(VOCAB_CAP as i64)
            .fetch_all(db)
            .await
        }
        None => {
            sqlx::query_as::<_, (String, String)>(
                "SELECT id, name FROM collections WHERE parent_id IS NULL ORDER BY sort_order, name LIMIT ?",
            )
            .bind(VOCAB_CAP as i64)
            .fetch_all(db)
            .await
        }
    }
    .map_err(|e| format!("db: {e}"))?;
    Ok(rows)
}

/// Active + staged vocabulary entries for this dimension/target, as paths.
async fn vocab_paths(
    db: &SqlitePool,
    dimension: &str,
    target: &str,
) -> Result<(Vec<String>, Vec<String>), String> {
    let rows = sqlx::query_as::<_, (String, String)>(
        "SELECT path, status FROM ai_classify_vocab
         WHERE dimension = ? AND target = ? AND status IN ('active', 'rejected')
         ORDER BY path",
    )
    .bind(dimension)
    .bind(target)
    .fetch_all(db)
    .await
    .map_err(|e| format!("db: {e}"))?;
    let mut active = Vec::new();
    let mut rejected = Vec::new();
    for (path, status) in rows {
        if status == "rejected" {
            rejected.push(path);
        } else {
            active.push(path);
        }
    }
    Ok((active, rejected))
}

async fn vocab_upsert(
    db: &SqlitePool,
    dimension: &str,
    target: &str,
    path: &str,
    status: &str,
    source: &str,
) -> Result<(), String> {
    let now = crate::core::time::now_iso();
    sqlx::query(
        "INSERT INTO ai_classify_vocab
           (id, dimension, target, path, status, source, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (dimension, target, path) DO UPDATE SET
           status = CASE WHEN ai_classify_vocab.status = 'rejected' THEN ai_classify_vocab.status
                         ELSE excluded.status END,
           source = excluded.source,
           updated_at = excluded.updated_at",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(dimension)
    .bind(target)
    .bind(path)
    .bind(status)
    .bind(source)
    .bind(&now)
    .bind(&now)
    .execute(db)
    .await
    .map_err(|e| format!("db: {e}"))?;
    Ok(())
}

pub async fn vocab_list(
    db: &SqlitePool,
    dimension: &str,
    target: &str,
) -> Result<Vec<VocabEntry>, String> {
    let rows = sqlx::query_as::<_, (String, String, String, String, String)>(
        "SELECT id, path, status, source, created_at FROM ai_classify_vocab
         WHERE dimension = ? AND target = ?
         ORDER BY status = 'active' DESC, path",
    )
    .bind(dimension)
    .bind(target)
    .fetch_all(db)
    .await
    .map_err(|e| format!("db: {e}"))?;
    Ok(rows
        .into_iter()
        .map(|(id, path, status, source, created_at)| VocabEntry { id, path, status, source, created_at })
        .collect())
}

/// Promote (accept) or reject vocabulary entries. Rejecting a term is how the
/// user tells the model "never propose this again".
pub async fn vocab_set_status(db: &SqlitePool, ids: &[String], status: &str) -> Result<(), String> {
    if !matches!(status, "active" | "staged" | "rejected") {
        return Err(format!("未知的标签状态: {status}"));
    }
    let now = crate::core::time::now_iso();
    for id in ids {
        sqlx::query("UPDATE ai_classify_vocab SET status = ?, updated_at = ? WHERE id = ?")
            .bind(status)
            .bind(&now)
            .bind(id)
            .execute(db)
            .await
            .map_err(|e| format!("db: {e}"))?;
    }
    Ok(())
}

fn vocab_block(active: &[String], library: &[(String, String)]) -> String {
    let mut names: Vec<String> = active.to_vec();
    for (path, _) in library {
        if !names.iter().any(|n| norm(n) == norm(path)) {
            names.push(path.clone());
        }
    }
    if names.is_empty() {
        return String::new();
    }
    names.truncate(VOCAB_CAP);
    let listing = names
        .iter()
        .map(|n| format!("「{n}」"))
        .collect::<Vec<_>>()
        .join("、");
    format!(
        "\n\n【词表（已有分类）】{listing}\n若某个已有分类完全适合某一组文献，请直接使用它的原名（一字不差），不要另造近义词。"
    )
}

// ---------------------------------------------------------------------------
// Persistence helpers
// ---------------------------------------------------------------------------

async fn set_status(db: &SqlitePool, run_id: &str, status: &str) -> Result<(), String> {
    sqlx::query("UPDATE ai_classify_runs SET status = ? WHERE id = ?")
        .bind(status)
        .bind(run_id)
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    Ok(())
}

async fn save_groups(db: &SqlitePool, run_id: &str, groups: &[ClassifyGroup]) -> Result<(), String> {
    let json = serde_json::to_string(groups).map_err(|e| format!("json: {e}"))?;
    sqlx::query("UPDATE ai_classify_runs SET groups_json = ? WHERE id = ?")
        .bind(&json)
        .bind(run_id)
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    Ok(())
}

pub async fn groups_set(
    db: &SqlitePool,
    run_id: &str,
    groups: &[ClassifyGroup],
) -> Result<(), String> {
    if groups.is_empty() {
        return Err("分类体系为空".to_string());
    }
    save_groups(db, run_id, groups).await
}

/// Upsert one assignment. `edited` marks it as user-confirmed so a later rerun
/// does not overwrite it; `None` means "leave the existing flag alone" — the
/// bulk sync before a run must not silently promote every row to manual.
async fn upsert_item_flag(
    db: &SqlitePool,
    run_id: &str,
    assign: &Assign,
    status: &str,
    edited: Option<bool>,
) -> Result<(), String> {
    let now = crate::core::time::now_iso();
    let inserted: i64 = if edited.unwrap_or(false) { 1 } else { 0 };
    let preserve: Option<i64> = edited.map(|e| if e { 1 } else { 0 });
    let labels_json = serde_json::to_string(&assign.labels).map_err(|e| format!("json: {e}"))?;
    sqlx::query(
        "INSERT INTO ai_classify_items
           (run_id, paper_id, group_name, labels_json, confidence, reason, status, edited, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (run_id, paper_id) DO UPDATE SET
           group_name = excluded.group_name,
           labels_json = excluded.labels_json,
           confidence = excluded.confidence,
           reason = excluded.reason,
           status = excluded.status,
           edited = COALESCE(?, ai_classify_items.edited)",
    )
    .bind(run_id)
    .bind(&assign.paper_id)
    .bind(assign.primary())
    .bind(&labels_json)
    .bind(assign.confidence)
    .bind(&assign.reason)
    .bind(status)
    .bind(inserted)
    .bind(&now)
    .bind(preserve)
    .execute(db)
    .await
    .map_err(|e| format!("db: {e}"))?;
    Ok(())
}

async fn upsert_item(
    db: &SqlitePool,
    run_id: &str,
    assign: &Assign,
    status: &str,
    edited: bool,
) -> Result<(), String> {
    upsert_item_flag(db, run_id, assign, status, Some(edited)).await
}

/// Assignment status from the confidence threshold (P1 triage).
fn triage_status(confidence: f64, threshold: f64) -> &'static str {
    if confidence >= threshold {
        "accepted"
    } else {
        "pending"
    }
}

/// Manual per-paper override from the dialog (empty labels = 待定).
pub async fn item_set(
    db: &SqlitePool,
    run_id: &str,
    paper_id: &str,
    labels: &[String],
) -> Result<(), String> {
    let cleaned: Vec<String> = labels
        .iter()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .take(MAX_LABELS)
        .collect();
    let assign = Assign {
        paper_id: paper_id.to_string(),
        labels: cleaned,
        confidence: 1.0,
        reason: "手动指定".to_string(),
    };
    upsert_item(db, run_id, &assign, "accepted", true).await
}

/// A manual move, used both for a single override and for the bulk remap that
/// follows renaming a group in the draft (the stored assignments are keyed by
/// group path, so a rename must carry them along).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemEdit {
    pub paper_id: String,
    pub labels: Vec<String>,
    /// Some(true) = a real user edit (protected from later reruns and applied
    /// as accepted); None = pure sync, keep whatever the row already had.
    #[serde(default)]
    pub edited: Option<bool>,
}

pub async fn items_set(db: &SqlitePool, run_id: &str, edits: &[ItemEdit]) -> Result<(), String> {
    for e in edits {
        let labels: Vec<String> = e
            .labels
            .iter()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .take(MAX_LABELS)
            .collect();
        match e.edited {
            // Pure sync (e.g. right before a run): only the labels move.
            // Overwriting confidence/reason here would blank out the evidence
            // the dialog is showing.
            None => {
                let labels_json = serde_json::to_string(&labels).map_err(|x| format!("json: {x}"))?;
                let res = sqlx::query(
                    "UPDATE ai_classify_items
                     SET group_name = ?, labels_json = ?
                     WHERE run_id = ? AND paper_id = ?",
                )
                .bind(labels.first().cloned().unwrap_or_default())
                .bind(&labels_json)
                .bind(run_id)
                .bind(&e.paper_id)
                .execute(db)
                .await
                .map_err(|x| format!("db: {x}"))?;
                if res.rows_affected() == 0 {
                    upsert_item_flag(
                        db,
                        run_id,
                        &Assign {
                            paper_id: e.paper_id.clone(),
                            labels,
                            confidence: 0.0,
                            reason: String::new(),
                        },
                        "pending",
                        Some(false),
                    )
                    .await?;
                }
            }
            Some(flag) => {
                let assign = Assign {
                    paper_id: e.paper_id.clone(),
                    labels,
                    confidence: if flag { 1.0 } else { 0.0 },
                    reason: if flag { "手动指定".to_string() } else { String::new() },
                };
                let status = if flag { "accepted" } else { "pending" };
                upsert_item_flag(db, run_id, &assign, status, Some(flag)).await?;
            }
        }
    }
    Ok(())
}

async fn load_run_row(
    db: &SqlitePool,
    run_id: &str,
) -> Result<(String, String, String, Option<u32>, f64, Vec<String>, Vec<ClassifyGroup>), String> {
    let row = sqlx::query_as::<_, (String, String, String, Option<i64>, f64, String, String)>(
        "SELECT dimension, target, status, sample_size, threshold, paper_ids_json, groups_json
         FROM ai_classify_runs WHERE id = ?",
    )
    .bind(run_id)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("db: {e}"))?
    .ok_or_else(|| format!("分类任务不存在: {run_id}"))?;
    let (dimension, target, status, sample_size, threshold, paper_ids_json, groups_json) = row;
    let paper_ids: Vec<String> = serde_json::from_str(&paper_ids_json).unwrap_or_default();
    let groups: Vec<ClassifyGroup> = serde_json::from_str(&groups_json).unwrap_or_default();
    Ok((
        dimension,
        target,
        status,
        sample_size.map(|v| v as u32),
        threshold,
        paper_ids,
        groups,
    ))
}

pub async fn get_run(db: &SqlitePool, run_id: &str) -> Result<ClassifyRun, String> {
    let row = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            String,
            Option<String>,
            Option<i64>,
            f64,
            i64,
            i64,
            String,
            String,
            Option<String>,
            Option<String>,
        ),
    >(
        "SELECT id, dimension, target, scope_kind, scope_id, sample_size, threshold,
                two_level, skip_existing, groups_json, status, applied_at, undone_at
         FROM ai_classify_runs WHERE id = ?",
    )
    .bind(run_id)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("db: {e}"))?
    .ok_or_else(|| format!("分类任务不存在: {run_id}"))?;

    let (
        id,
        dimension,
        target,
        scope_kind,
        scope_id,
        sample_size,
        threshold,
        two_level,
        skip_existing,
        groups_json,
        status,
        applied_at,
        undone_at,
    ) = row;
    let groups: Vec<ClassifyGroup> = serde_json::from_str(&groups_json).unwrap_or_default();

    let rows = sqlx::query_as::<
        _,
        (String, String, String, f64, String, String, i64, Option<String>, Option<i64>),
    >(
        "SELECT i.paper_id, i.group_name, i.labels_json, i.confidence, i.reason, i.status,
                i.edited, p.title, p.year
         FROM ai_classify_items i
         LEFT JOIN papers p ON p.id = i.paper_id
         WHERE i.run_id = ?
         ORDER BY i.group_name = '' , i.group_name, p.title",
    )
    .bind(run_id)
    .fetch_all(db)
    .await
    .map_err(|e| format!("db: {e}"))?;

    let items = rows
        .into_iter()
        .map(
            |(paper_id, group_name, labels_json, confidence, reason, status, edited, title, year)| {
                let mut labels: Vec<String> = serde_json::from_str(&labels_json).unwrap_or_default();
                if labels.is_empty() && !group_name.trim().is_empty() {
                    labels.push(group_name.clone());
                }
                ClassifyItem {
                    paper_id,
                    title: title.unwrap_or_else(|| "（文献已删除）".to_string()),
                    year,
                    group_name,
                    labels,
                    confidence,
                    reason,
                    status,
                    edited: edited != 0,
                }
            },
        )
        .collect::<Vec<_>>();

    let paper_count = sqlx::query_scalar::<_, String>(
        "SELECT paper_ids_json FROM ai_classify_runs WHERE id = ?",
    )
    .bind(run_id)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("db: {e}"))?
    .and_then(|json| serde_json::from_str::<Vec<String>>(&json).ok())
    .map(|v| v.len())
    .unwrap_or(items.len());

    Ok(ClassifyRun {
        id,
        dimension,
        target,
        scope_kind,
        scope_id,
        sample_size: sample_size.map(|v| v as u32),
        paper_count,
        threshold,
        two_level: two_level != 0,
        skip_existing: skip_existing != 0,
        groups,
        items,
        status,
        applied_at,
        undone_at,
    })
}

/// Papers already filed under this dimension+target by an applied run.
pub async fn previously_classified(
    db: &SqlitePool,
    dimension: &str,
    target: &str,
) -> Result<std::collections::HashSet<String>, String> {
    let rows = sqlx::query_as::<_, (String,)>(
        "SELECT DISTINCT a.paper_id FROM ai_classify_applied a
         JOIN ai_classify_runs r ON r.id = a.run_id
         WHERE r.dimension = ? AND r.target = ? AND r.status = 'applied'
           AND a.link_created = 1 AND a.paper_id <> ''",
    )
    .bind(dimension)
    .bind(target)
    .fetch_all(db)
    .await
    .map_err(|e| format!("db: {e}"))?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

// ---------------------------------------------------------------------------
// Phase 1: draft the taxonomy (batch bucketing + merge + optional hierarchy)
// ---------------------------------------------------------------------------

pub async fn run_draft(app: AppHandle, db: SqlitePool, run_id: String, req: ClassifyRequest) {
    let _guard = match RunGuard::try_acquire() {
        Ok(g) => g,
        Err(e) => {
            tracing::warn!(error = %e, "ai-classify already running");
            let _ = set_status(&db, &run_id, "error").await;
            // Never leave the dialog in a silent "running" state.
            emit(
                &app,
                &ClassifyProgress {
                    stage: "error".into(),
                    total: 0,
                    done: 0,
                    failed: 0,
                    current: String::new(),
                    group_count: 0,
                    groups_preview: None,
                    log: Some(format!("{e}（多半是上一次分类还没结束，可稍后重试）")),
                    finished: Some("error".into()),
                },
            );
            return;
        }
    };

    let started = std::time::Instant::now();
    match draft_inner(&app, &db, &run_id, &req).await {
        Ok(()) => {
            tracing::info!(run_id, elapsed = ?started.elapsed(), "ai-classify draft done");
        }
        Err(e) => {
            tracing::warn!(run_id, error = %e, "ai-classify draft failed");
            let _ = set_status(&db, &run_id, "error").await;
            emit(
                &app,
                &ClassifyProgress {
                    stage: "error".into(),
                    total: 0,
                    done: 0,
                    failed: 0,
                    current: String::new(),
                    group_count: 0,
                    groups_preview: None,
                    log: Some(format!("分类失败：{e}")),
                    finished: Some("error".into()),
                },
            );
        }
    }
}

async fn draft_inner(
    app: &AppHandle,
    db: &SqlitePool,
    run_id: &str,
    req: &ClassifyRequest,
) -> Result<(), String> {
    let dimension = if req.dimension.trim().is_empty() {
        "研究主题".to_string()
    } else {
        req.dimension.trim().to_string()
    };
    let target = if req.target.trim().is_empty() { "collection".to_string() } else { req.target.clone() };
    let max_groups = if req.max_groups == 0 { 12 } else { req.max_groups.min(30) };
    let threshold = if req.threshold <= 0.0 || req.threshold > 1.0 { 0.75 } else { req.threshold };
    let multi_label = target == "tag";

    // Sample-first: draft on the first N papers, assign the rest later.
    let limit = req
        .sample_size
        .filter(|n| *n > 0)
        .map(|n| n as usize)
        .unwrap_or(req.paper_ids.len());
    let draft_ids: Vec<String> = req.paper_ids.iter().take(limit).cloned().collect();
    let papers = papers_info(db, &draft_ids).await?;
    if papers.is_empty() {
        return Err("所选文献集为空".to_string());
    }

    let library = if req.reuse_existing {
        existing_targets(db, &target, req.scope_id.as_deref()).await?
    } else {
        Vec::new()
    };
    let (active_vocab, rejected_vocab) = vocab_paths(db, &dimension, &target).await?;

    let llm = default_llm(db, req.provider_id.as_deref(), 4000).await?;
    let total_batches = papers.chunks(DRAFT_BATCH).count();
    let mut all_groups: Vec<ClassifyGroup> = Vec::new();
    let mut assigns: Vec<Assign> = Vec::new();
    let mut failed_batches = 0usize;

    // Optional semantic engine: cluster in vector space, name the clusters once.
    let mut engine = if req.engine.trim().is_empty() {
        "batch".to_string()
    } else {
        req.engine.trim().to_string()
    };
    if engine == "semantic" {
        let started = std::time::Instant::now();
        match semantic_draft(db, &*llm, &dimension, &draft_ids, &papers, max_groups).await {
            Ok((groups, list)) => {
                all_groups = groups;
                assigns = list;
                emit(
                    app,
                    &ClassifyProgress {
                        stage: "draft".into(),
                        total: total_batches,
                        done: total_batches,
                        failed: 0,
                        current: String::new(),
                        group_count: all_groups.len(),
                        groups_preview: Some(group_names(&all_groups)),
                        log: Some(format!(
                            "语义聚类完成：{} 个簇，{} 篇已归类（{:.1}s，仅 1 次模型调用）",
                            all_groups.len(),
                            assigns.len(),
                            started.elapsed().as_secs_f64()
                        )),
                        finished: None,
                    },
                );
            }
            Err(e) => {
                // A gated nicety must never block the feature: fall back and say so.
                engine = "batch".to_string();
                emit(
                    app,
                    &ClassifyProgress {
                        stage: "draft".into(),
                        total: total_batches,
                        done: 0,
                        failed: 0,
                        current: String::new(),
                        group_count: 0,
                        groups_preview: None,
                        log: Some(format!("语义聚类不可用（{e}），已回退到批量起草")),
                        finished: None,
                    },
                );
            }
        }
    }

    if engine == "batch" {
    for (i, batch) in papers.chunks(DRAFT_BATCH).enumerate() {
        if CANCEL.load(Ordering::SeqCst) {
            let _ = set_status(db, run_id, "cancelled").await;
            emit(
                app,
                &ClassifyProgress {
                    stage: "cancelled".into(),
                    total: total_batches,
                    done: i,
                    failed: failed_batches,
                    current: String::new(),
                    group_count: all_groups.len(),
                    groups_preview: Some(group_names(&all_groups)),
                    log: Some("已取消".into()),
                    finished: Some("cancelled".into()),
                },
            );
            return Ok(());
        }

        emit(
            app,
            &ClassifyProgress {
                stage: "draft".into(),
                total: total_batches,
                done: i,
                failed: failed_batches,
                current: format!("第 {}/{} 批（{} 篇）", i + 1, total_batches, batch.len()),
                group_count: all_groups.len(),
                groups_preview: Some(group_names(&all_groups)),
                log: None,
                finished: None,
            },
        );

        let system = crate::core::task_prompts::render(
            &crate::core::task_prompts::effective(db, crate::core::task_prompts::CLASSIFY_DRAFT).await,
            &[("dimension", dimension.as_str()), ("max_groups", &max_groups.to_string())],
        );
        let user = format!(
            "分类维度：{dimension}{}\n\n【待分类文献】\n{}",
            vocab_block(&active_vocab, &library),
            paper_block(batch, DRAFT_ABSTRACT_CAP)
        );

        let messages = vec![chat_msg("system", system), chat_msg("user", user)];
        let draft_call = tokio::select! {
            r = llm.chat_completion(&messages, &[]) => r,
            _ = cancelled_now() => Err("已取消".to_string()),
        };
        match draft_call {
            Ok(resp) => match parse_draft(&resp.content) {
                Ok((groups, mut batch_assigns)) => {
                    // Keep only ids that were actually in this batch: a model
                    // that hallucinates an id must not create a phantom row.
                    let allowed: std::collections::HashSet<&str> =
                        batch.iter().map(|p| p.id.as_str()).collect();
                    batch_assigns.retain(|a| allowed.contains(a.paper_id.as_str()));
                    for g in groups {
                        if !all_groups.iter().any(|x| norm(&x.name) == norm(&g.name)) {
                            all_groups.push(g);
                        }
                    }
                    assigns.extend(batch_assigns);
                }
                Err(e) => {
                    failed_batches += 1;
                    emit(
                        app,
                        &ClassifyProgress {
                            stage: "draft".into(),
                            total: total_batches,
                            done: i + 1,
                            failed: failed_batches,
                            current: String::new(),
                            group_count: all_groups.len(),
                            groups_preview: Some(group_names(&all_groups)),
                            log: Some(format!("第 {} 批解析失败：{e}", i + 1)),
                            finished: None,
                        },
                    );
                }
            },
            Err(e) => {
                failed_batches += 1;
                emit(
                    app,
                    &ClassifyProgress {
                        stage: "draft".into(),
                        total: total_batches,
                        done: i + 1,
                        failed: failed_batches,
                        current: String::new(),
                        group_count: all_groups.len(),
                        groups_preview: Some(group_names(&all_groups)),
                        log: Some(format!("第 {} 批调用失败：{e}", i + 1)),
                        finished: None,
                    },
                );
            }
        }
    }

    }

    if all_groups.is_empty() {
        return Err("没有产出任何分类（模型调用或解析全部失败）".to_string());
    }

    // Merge near-duplicate group names produced by different batches.
    if all_groups.len() > max_groups as usize {
        emit(
            app,
            &ClassifyProgress {
                stage: "merge".into(),
                total: total_batches,
                done: total_batches,
                failed: failed_batches,
                current: "合并近义类目".into(),
                group_count: all_groups.len(),
                groups_preview: Some(group_names(&all_groups)),
                log: None,
                finished: None,
            },
        );
        if let Ok(merged) = merge_groups(db, &*llm, &all_groups, max_groups).await {
            let mut next: Vec<ClassifyGroup> = Vec::new();
            let mut map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
            for g in &merged {
                next.push(ClassifyGroup::new(g.name.trim(), g.description.trim()));
                for from in &g.from {
                    map.insert(norm(from), g.name.trim().to_string());
                }
            }
            if !next.is_empty() {
                for a in assigns.iter_mut() {
                    for label in a.labels.iter_mut() {
                        if let Some(canonical) = map.get(&norm(label)) {
                            *label = canonical.clone();
                        }
                    }
                }
                all_groups = next;
            }
        }
    }

    // Optional second level: one call that groups the flat draft under parents.
    let mut two_level = req.two_level;
    if two_level && all_groups.len() >= 4 {
        emit(
            app,
            &ClassifyProgress {
                stage: "hierarchize".into(),
                total: total_batches,
                done: total_batches,
                failed: failed_batches,
                current: "生成二级类目".into(),
                group_count: all_groups.len(),
                groups_preview: Some(group_names(&all_groups)),
                log: None,
                finished: None,
            },
        );
        match hierarchize_groups(db, &*llm, &all_groups).await {
            Ok(parents) => {
                if let Some((tree, mapping)) = apply_hierarchy(&all_groups, &parents) {
                    all_groups = tree;
                    for a in assigns.iter_mut() {
                        for label in a.labels.iter_mut() {
                            if let Some(next) = mapping.get(&norm(label)) {
                                *label = next.clone();
                            }
                        }
                    }
                    emit(
                        app,
                        &ClassifyProgress {
                            stage: "hierarchize".into(),
                            total: total_batches,
                            done: total_batches,
                            failed: failed_batches,
                            current: String::new(),
                            group_count: all_paths(&all_groups).len(),
                            groups_preview: Some(group_names(&all_groups)),
                            log: Some(format!("已生成 {} 个二级父类", parents.len())),
                            finished: None,
                        },
                    );
                }
            }
            Err(e) => {
                // Hierarchy is a nicety: fall back to a flat draft, never fail.
                two_level = false;
                emit(
                    app,
                    &ClassifyProgress {
                        stage: "hierarchize".into(),
                        total: total_batches,
                        done: total_batches,
                        failed: failed_batches,
                        current: String::new(),
                        group_count: all_groups.len(),
                        groups_preview: Some(group_names(&all_groups)),
                        log: Some(format!("二级类目生成失败，保留单层：{e}")),
                        finished: None,
                    },
                );
            }
        }
    }

    // Negative vocabulary memory: a term the user rejected never comes back.
    let mut dropped = 0usize;
    if !rejected_vocab.is_empty() {
        let rejected: std::collections::HashSet<String> =
            rejected_vocab.iter().map(|p| norm(p)).collect();
        let before = all_groups.len();
        all_groups.retain(|g| !rejected.contains(&norm(&g.name)));
        assigns.retain(|a| {
            !a.labels
                .iter()
                .all(|l| rejected.contains(&norm(l)) || l.trim().is_empty())
        });
        // Also drop rejected children.
        for g in all_groups.iter_mut() {
            g.children.retain(|c| !rejected.contains(&norm(&format!("{}/{}", g.name.trim(), c.name.trim()))));
        }
        dropped = before.saturating_sub(all_groups.len());
    }
    if all_groups.is_empty() {
        return Err("草案里的类目都被词表标记为「丢弃」了，请调整维度或检查词表".to_string());
    }

    // Groups that survived keep only the papers whose group still exists.
    let names: std::collections::HashSet<String> =
        all_paths(&all_groups).iter().map(|p| norm(p)).collect();
    for a in assigns.iter_mut() {
        a.labels.retain(|l| names.contains(&norm(l)));
    }
    assigns.retain(|a| !a.labels.is_empty());

    if req.reuse_existing {
        attach_existing(&mut all_groups, &library);
    }
    save_groups(db, run_id, &all_groups).await?;
    for a in &assigns {
        let status = triage_status(a.confidence, threshold);
        if multi_label {
            // The draft assigns one bucket per paper; multi-label refinement
            // belongs to the closed-set pass, which the dialog offers.
            upsert_item(db, run_id, a, status, false).await?;
        } else {
            upsert_item(db, run_id, a, status, false).await?;
        }
    }

    // Vocabulary bookkeeping: every drafted path becomes a staged candidate
    // (unless it is already known), so the user can promote or reject it.
    for path in all_paths(&all_groups) {
        let known = active_vocab.iter().any(|p| norm(p) == norm(&path));
        let in_library = library.iter().any(|(p, _)| norm(p) == norm(&path));
        let status = if known || in_library { "active" } else { "staged" };
        let source = if in_library { "library" } else { "ai" };
        vocab_upsert(db, &dimension, &target, &path, status, source).await?;
    }

    // Papers the model dropped are recorded as 待定 so the dialog can offer
    // "assign the rest" instead of showing them as missing.
    let assigned: std::collections::HashSet<String> =
        assigns.iter().map(|a| a.paper_id.clone()).collect();
    for p in &papers {
        if !assigned.contains(&p.id) {
            upsert_item(
                db,
                run_id,
                &Assign {
                    paper_id: p.id.clone(),
                    labels: Vec::new(),
                    confidence: 0.0,
                    reason: "模型未归类".to_string(),
                },
                "pending",
                false,
            )
            .await?;
        }
    }

    sqlx::query("UPDATE ai_classify_runs SET two_level = ?, engine = ? WHERE id = ?")
        .bind(if two_level { 1 } else { 0 })
        .bind(&engine)
        .bind(run_id)
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    set_status(db, run_id, "done").await?;
    let paths = all_paths(&all_groups).len();
    emit(
        app,
        &ClassifyProgress {
            stage: "done".into(),
            total: total_batches,
            done: total_batches,
            failed: failed_batches,
            current: String::new(),
            group_count: paths,
            groups_preview: Some(group_names(&all_groups)),
            log: Some(format!(
                "草案完成：{} 个类目，{} 篇已归类（{} 批，失败 {} 批{}）",
                paths,
                assigns.len(),
                total_batches,
                failed_batches,
                if dropped > 0 { format!("，按词表丢弃 {dropped} 个") } else { String::new() }
            )),
            finished: Some("done".into()),
        },
    );
    Ok(())
}

/// Run the embedding-cluster draft and flatten its result into `Assign` rows.
async fn semantic_draft(
    db: &SqlitePool,
    llm: &dyn crate::ai::llm::LlmClient,
    dimension: &str,
    paper_ids: &[String],
    papers: &[PaperBrief],
    max_groups: u32,
) -> Result<(Vec<ClassifyGroup>, Vec<Assign>), String> {
    let briefs: std::collections::HashMap<String, String> = papers
        .iter()
        .map(|p| {
            let year = p.year.map(|y| format!("（{y}）")).unwrap_or_default();
            (p.id.clone(), format!("{}{}", p.title, year))
        })
        .collect();
    let (groups, list) = crate::core::ai_classify_semantic::draft(
        db,
        llm,
        dimension,
        paper_ids,
        max_groups as usize,
        &briefs,
    )
    .await?;
    let mut assigns: Vec<Assign> = Vec::new();
    for (name, members, reason) in list {
        for (paper_id, confidence) in members {
            assigns.push(Assign {
                paper_id,
                labels: vec![name.clone()],
                confidence,
                reason: reason.clone(),
            });
        }
    }
    Ok((groups, assigns))
}

async fn merge_groups(
    db: &SqlitePool,
    llm: &dyn crate::ai::llm::LlmClient,
    groups: &[ClassifyGroup],
    max_groups: u32,
) -> Result<Vec<RawMergeGroup>, String> {
    let listing = groups
        .iter()
        .map(|g| format!("- 「{}」：{}", g.name, g.description))
        .collect::<Vec<_>>()
        .join("\n");
    let system = crate::core::task_prompts::render(
        &crate::core::task_prompts::effective(db, crate::core::task_prompts::CLASSIFY_MERGE).await,
        &[("max_groups", &max_groups.to_string())],
    );
    let messages = vec![
        chat_msg("system", system),
        chat_msg("user", format!("【待归并类目】\n{listing}")),
    ];
    let resp = tokio::select! {
        r = llm.chat_completion(&messages, &[]) => r?,
        _ = cancelled_now() => return Err("已取消".to_string()),
    };
    parse_merge(&resp.content)
}

async fn hierarchize_groups(
    db: &SqlitePool,
    llm: &dyn crate::ai::llm::LlmClient,
    groups: &[ClassifyGroup],
) -> Result<Vec<RawParent>, String> {
    let listing = groups
        .iter()
        .map(|g| format!("- 「{}」：{}", g.name, g.description))
        .collect::<Vec<_>>()
        .join("\n");
    let system = crate::core::task_prompts::render(
        &crate::core::task_prompts::effective(db, crate::core::task_prompts::CLASSIFY_HIERARCHIZE)
            .await,
        &[("max_parents", &MAX_PARENTS.to_string())],
    );
    let messages = vec![
        chat_msg("system", system),
        chat_msg("user", format!("【待归纳类目】\n{listing}")),
    ];
    let resp = tokio::select! {
        r = llm.chat_completion(&messages, &[]) => r?,
        _ = cancelled_now() => return Err("已取消".to_string()),
    };
    parse_hierarchy(&resp.content)
}

/// Rebuild the draft as a two-level tree. Returns None when the model's answer
/// names categories that do not exist (we then keep the flat draft).
fn apply_hierarchy(
    flat: &[ClassifyGroup],
    parents: &[RawParent],
) -> Option<(Vec<ClassifyGroup>, std::collections::HashMap<String, String>)> {
    let by_name: std::collections::HashMap<String, &ClassifyGroup> =
        flat.iter().map(|g| (norm(&g.name), g)).collect();
    let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut tree: Vec<ClassifyGroup> = Vec::new();
    let mut mapping: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    for p in parents {
        let mut children: Vec<ClassifyGroup> = Vec::new();
        for c in &p.children {
            let key = norm(c);
            // Skip unknown or already-consumed names; the child keeps its own
            // description and existing_id from the flat draft.
            if used.contains(&key) {
                continue;
            }
            if let Some(group) = by_name.get(&key) {
                let mut node = (*group).clone();
                node.children.clear();
                children.push(node);
                used.insert(key);
            }
        }
        if children.len() < 2 {
            // A "parent" with one child adds a level for nothing.
            for c in children {
                used.remove(&norm(&c.name));
            }
            continue;
        }
        let parent_name = p.name.trim().to_string();
        for c in &children {
            mapping.insert(norm(&c.name), format!("{parent_name}/{}", c.name.trim()));
        }
        tree.push(ClassifyGroup {
            name: parent_name,
            description: p.description.trim().to_string(),
            existing_id: None,
            children,
        });
    }

    // Whatever the model did not place stays at the top level.
    for g in flat {
        if !used.contains(&norm(&g.name)) {
            let mut node = g.clone();
            node.children.clear();
            tree.push(node);
        }
    }

    if tree.iter().all(|g| g.is_leaf()) {
        return None;
    }
    Some((tree, mapping))
}

// ---------------------------------------------------------------------------
// Phase 2: closed-set assignment against the confirmed taxonomy
// ---------------------------------------------------------------------------

pub async fn run_reassign(
    app: AppHandle,
    db: SqlitePool,
    run_id: String,
    provider_id: Option<String>,
    only_missing: bool,
) {
    let _guard = match RunGuard::try_acquire() {
        Ok(g) => g,
        Err(e) => {
            tracing::warn!(error = %e, "ai-classify already running");
            emit(
                &app,
                &ClassifyProgress {
                    stage: "error".into(),
                    total: 0,
                    done: 0,
                    failed: 0,
                    current: String::new(),
                    group_count: 0,
                    groups_preview: None,
                    log: Some(format!("{e}（多半是上一次分类还没结束，可稍后重试）")),
                    finished: Some("error".into()),
                },
            );
            return;
        }
    };
    if let Err(e) = reassign_inner(&app, &db, &run_id, provider_id.as_deref(), only_missing).await {
        let _ = set_status(&db, &run_id, "error").await;
        emit(
            &app,
            &ClassifyProgress {
                stage: "error".into(),
                total: 0,
                done: 0,
                failed: 0,
                current: String::new(),
                group_count: 0,
                groups_preview: None,
                log: Some(format!("判定失败：{e}")),
                finished: Some("error".into()),
            },
        );
    }
}

async fn reassign_inner(
    app: &AppHandle,
    db: &SqlitePool,
    run_id: &str,
    provider_id: Option<&str>,
    only_missing: bool,
) -> Result<(), String> {
    let (dimension, target, _status, _sample, threshold, paper_ids, groups) =
        load_run_row(db, run_id).await?;
    if groups.is_empty() {
        return Err("分类体系为空，请先完成草案".to_string());
    }
    let paths = all_paths(&groups);
    if paths.is_empty() {
        return Err("分类体系为空，请先完成草案".to_string());
    }
    let multi_label = target == "tag";

    // Papers to (re)assign.
    let targets: Vec<String> = if only_missing {
        let rows = sqlx::query_as::<_, (String, String, i64)>(
            "SELECT paper_id, group_name, edited FROM ai_classify_items WHERE run_id = ?",
        )
        .bind(run_id)
        .fetch_all(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
        let mut have: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (paper_id, group_name, edited) in rows {
            if !group_name.trim().is_empty() || edited != 0 {
                have.insert(paper_id);
            }
        }
        paper_ids.iter().filter(|id| !have.contains(*id)).cloned().collect()
    } else {
        // Untouched manual overrides survive a full rerun.
        let rows = sqlx::query_as::<_, (String, i64)>(
            "SELECT paper_id, edited FROM ai_classify_items WHERE run_id = ?",
        )
        .bind(run_id)
        .fetch_all(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
        let edited: std::collections::HashSet<String> = rows
            .into_iter()
            .filter(|(_, e)| *e != 0)
            .map(|(id, _)| id)
            .collect();
        paper_ids.iter().filter(|id| !edited.contains(*id)).cloned().collect()
    };

    if targets.is_empty() {
        emit(
            app,
            &ClassifyProgress {
                stage: "done".into(),
                total: 0,
                done: 0,
                failed: 0,
                current: String::new(),
                group_count: paths.len(),
                groups_preview: Some(paths.clone()),
                log: Some("没有需要判定的文献".into()),
                finished: Some("done".into()),
            },
        );
        return Ok(());
    }

    let llm = default_llm(db, provider_id, 4000).await?;
    let taxonomy = paths
        .iter()
        .enumerate()
        .map(|(i, n)| format!("{}.「{n}」", i + 1))
        .collect::<Vec<_>>()
        .join("\n");
    let total = targets.len();
    let mut done = 0usize;
    let mut failed = 0usize;
    let batches: Vec<Vec<String>> = targets.chunks(ASSIGN_BATCH).map(|c| c.to_vec()).collect();
    let total_batches = batches.len();

    let label_spec = if multi_label {
        format!(
            "每篇文献给出 1~{MAX_LABELS} 个类目（按主次排序，labels 为类目名数组），\
             都不合适时给空数组"
        )
    } else {
        "每篇文献只归入**一个**类目（group 填类目名），完全不合适时把 group 留空字符串".to_string()
    };
    let output_spec = if multi_label {
        "{\"assignments\":[{\"paper\":\"文献 id\",\"labels\":[\"类目名1\",\"类目名2\"],\"confidence\":0.0~1.0,\"reason\":\"归类依据（不超过 20 字）\"}]}"
    } else {
        "{\"assignments\":[{\"paper\":\"文献 id\",\"group\":\"类目名\",\"confidence\":0.0~1.0,\"reason\":\"归类依据（不超过 20 字）\"}]}"
    };

    for (bi, batch) in batches.iter().enumerate() {
        if CANCEL.load(Ordering::SeqCst) {
            set_status(db, run_id, "cancelled").await?;
            emit(
                app,
                &ClassifyProgress {
                    stage: "cancelled".into(),
                    total,
                    done,
                    failed,
                    current: String::new(),
                    group_count: paths.len(),
                    groups_preview: Some(paths.clone()),
                    log: Some("已取消".into()),
                    finished: Some("cancelled".into()),
                },
            );
            return Ok(());
        }
        emit(
            app,
            &ClassifyProgress {
                stage: "assign".into(),
                total,
                done,
                failed,
                current: format!("第 {}/{} 批（{} 篇）", bi + 1, total_batches, batch.len()),
                group_count: paths.len(),
                groups_preview: Some(paths.clone()),
                log: None,
                finished: None,
            },
        );

        let papers = papers_info(db, batch).await?;
        if papers.is_empty() {
            failed += batch.len();
            continue;
        }
        let system = crate::core::task_prompts::render(
            &crate::core::task_prompts::effective(db, crate::core::task_prompts::CLASSIFY_ASSIGN).await,
            &[
                ("label_spec", label_spec.as_str()),
                ("output_spec", output_spec),
                ("dimension", dimension.as_str()),
                ("taxonomy", taxonomy.as_str()),
            ],
        );
        let user = format!("【待判定文献】\n{}", paper_block(&papers, ASSIGN_ABSTRACT_CAP));
        let messages = vec![chat_msg("system", system), chat_msg("user", user)];

        let assign_call = tokio::select! {
            r = llm.chat_completion(&messages, &[]) => r,
            _ = cancelled_now() => Err("已取消".to_string()),
        };
        match assign_call {
            Ok(resp) => match parse_assign(&resp.content) {
                Ok(list) => {
                    let allowed_ids: std::collections::HashSet<&str> =
                        papers.iter().map(|p| p.id.as_str()).collect();
                    let allowed_groups: std::collections::HashSet<String> =
                        paths.iter().map(|n| norm(n)).collect();
                    let mut seen = std::collections::HashSet::new();
                    for mut a in list {
                        if !allowed_ids.contains(a.paper_id.as_str()) {
                            continue;
                        }
                        // Closed set: anything outside the confirmed taxonomy
                        // degrades to 待定 instead of creating a new category.
                        let before = a.labels.len();
                        a.labels.retain(|l| allowed_groups.contains(&norm(l)));
                        if before > 0 && a.labels.is_empty() {
                            a.reason = "模型给出的类目不在清单内".to_string();
                        }
                        if seen.insert(a.paper_id.clone()) {
                            let status = triage_status(a.confidence, threshold);
                            upsert_item(db, run_id, &a, status, false).await?;
                            done += 1;
                        }
                    }
                    // Papers the model skipped become explicit 待定 rows.
                    for p in &papers {
                        if !seen.contains(&p.id) {
                            upsert_item(
                                db,
                                run_id,
                                &Assign {
                                    paper_id: p.id.clone(),
                                    labels: Vec::new(),
                                    confidence: 0.0,
                                    reason: "模型未返回判定".to_string(),
                                },
                                "pending",
                                false,
                            )
                            .await?;
                            done += 1;
                        }
                    }
                }
                Err(e) => {
                    failed += papers.len();
                    emit(
                        app,
                        &ClassifyProgress {
                            stage: "assign".into(),
                            total,
                            done,
                            failed,
                            current: String::new(),
                            group_count: paths.len(),
                            groups_preview: Some(paths.clone()),
                            log: Some(format!("第 {} 批解析失败：{e}", bi + 1)),
                            finished: None,
                        },
                    );
                }
            },
            Err(e) => {
                failed += papers.len();
                emit(
                    app,
                    &ClassifyProgress {
                        stage: "assign".into(),
                        total,
                        done,
                        failed,
                        current: String::new(),
                        group_count: paths.len(),
                        groups_preview: Some(paths.clone()),
                        log: Some(format!("第 {} 批调用失败：{e}", bi + 1)),
                        finished: None,
                    },
                );
            }
        }
    }

    set_status(db, run_id, if failed == total { "error" } else { "done" }).await?;
    emit(
        app,
        &ClassifyProgress {
            stage: "done".into(),
            total,
            done,
            failed,
            current: String::new(),
            group_count: paths.len(),
            groups_preview: Some(paths.clone()),
            log: Some(format!("判定完成：{done} 篇，失败 {failed} 篇")),
            finished: Some("done".into()),
        },
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Apply / undo
// ---------------------------------------------------------------------------

/// Find or create the collection to hold a group segment.
async fn ensure_collection(
    db: &SqlitePool,
    name: &str,
    parent_id: Option<&str>,
) -> Result<(String, bool), String> {
    let existing = match parent_id {
        Some(pid) => {
            sqlx::query_scalar::<_, String>(
                "SELECT id FROM collections WHERE name = ? AND parent_id = ? LIMIT 1",
            )
            .bind(name)
            .bind(pid)
            .fetch_optional(db)
            .await
        }
        None => {
            sqlx::query_scalar::<_, String>(
                "SELECT id FROM collections WHERE name = ? AND parent_id IS NULL LIMIT 1",
            )
            .bind(name)
            .fetch_optional(db)
            .await
        }
    }
    .map_err(|e| format!("db: {e}"))?;
    if let Some(id) = existing {
        return Ok((id, false));
    }
    let created = crate::core::collection_service::create_collection(db, name, parent_id).await?;
    Ok((created.id, true))
}

async fn ensure_tag(db: &SqlitePool, name: &str) -> Result<(String, bool), String> {
    let existing = sqlx::query_scalar::<_, String>("SELECT id FROM tags WHERE name = ? LIMIT 1")
        .bind(name)
        .fetch_optional(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    if let Some(id) = existing {
        return Ok((id, false));
    }
    let tag = crate::core::tag_service::create_tag(db, name, Some(TAG_COLOR)).await?;
    Ok((tag.id, true))
}

/// `父/子` → the two segments (a path never has more than two levels).
fn split_path(path: &str) -> Vec<String> {
    path.split('/')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

async fn record_applied(
    db: &SqlitePool,
    run_id: &str,
    kind: &str,
    target_id: &str,
    target_name: &str,
    paper_id: &str,
    created_new: bool,
    link_created: bool,
) -> Result<(), String> {
    sqlx::query(
        "INSERT OR REPLACE INTO ai_classify_applied
           (run_id, kind, target_id, target_name, paper_id, created_new, link_created)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(run_id)
    .bind(kind)
    .bind(target_id)
    .bind(target_name)
    .bind(paper_id)
    .bind(if created_new { 1 } else { 0 })
    .bind(if link_created { 1 } else { 0 })
    .execute(db)
    .await
    .map_err(|e| format!("db: {e}"))?;
    Ok(())
}

/// Write the confirmed draft into the library. Only papers whose stored
/// assignment is `accepted` (or manually edited) are touched — `pending` rows
/// are what the review queue is for.
pub async fn apply_run(
    db: &SqlitePool,
    run_id: &str,
    plan: &ApplyPlan,
) -> Result<ApplySummary, String> {
    let (dimension, target, status, _sample, _threshold, _paper_ids, groups) =
        load_run_row(db, run_id).await?;
    if status == "undone" {
        return Err("该分类已撤销，请重新运行后再应用".to_string());
    }
    let kind = if target == "tag" { "tag" } else { "collection" };

    // Container for new top-level collections: the caller's parent, else our
    // own root collection so AI output never scatters across the library root.
    let base_parent = if kind == "collection" {
        match plan.parent_collection_id.clone().filter(|s| !s.is_empty()) {
            Some(pid) => Some(pid),
            None => {
                let root_name = format!("AI 分类 · {dimension}");
                let (id, created) = ensure_collection(db, &root_name, None).await?;
                if created {
                    record_applied(db, run_id, "collection", &id, &root_name, "", true, false).await?;
                }
                Some(id)
            }
        }
    } else {
        None
    };

    // Group paths that still exist in the draft → their existing_id, so a
    // deleted-in-the-meantime library category is not reused blindly.
    let draft: std::collections::HashMap<String, Option<String>> =
        paths_of(&groups).into_iter().map(|(p, id)| (norm(&p), id)).collect();

    // paper → labels, restricted to rows this run is allowed to write.
    let rows = sqlx::query_as::<_, (String, String, i64, String)>(
        "SELECT paper_id, labels_json, edited, status FROM ai_classify_items WHERE run_id = ?",
    )
    .bind(run_id)
    .fetch_all(db)
    .await
    .map_err(|e| format!("db: {e}"))?;
    let mut by_path: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    let mut skipped_papers = 0usize;
    for (paper_id, labels_json, edited, item_status) in rows {
        let mut labels: Vec<String> = serde_json::from_str(&labels_json).unwrap_or_default();
        labels.retain(|l| !l.trim().is_empty());
        if labels.is_empty() {
            skipped_papers += 1;
            continue;
        }
        let writable = edited != 0 || item_status == "accepted";
        if !writable {
            skipped_papers += 1;
            continue;
        }
        for label in labels {
            by_path.entry(label).or_default().push(paper_id.clone());
        }
    }

    let mut created_targets = 0usize;
    let mut reused_targets = 0usize;
    let mut linked_papers = 0usize;
    let mut applied_paths: Vec<String> = Vec::new();

    for t in &plan.targets {
        let path = t.group_name.trim().to_string();
        if path.is_empty() {
            continue;
        }
        let paper_ids = match by_path.get(&path) {
            Some(ids) if !ids.is_empty() => ids.clone(),
            _ => continue,
        };
        let existing_id = draft.get(&norm(&path)).cloned().flatten();
        let segments = split_path(&path);
        if segments.is_empty() {
            continue;
        }

        // Resolve the target id: an existing match wins; otherwise create the
        // chain (parent segment then leaf) for collections, one tag row for tags.
        let (target_id, created) = if kind == "tag" {
            let tag_name = format!("{dimension}#{}", segments.join("#"));
            match existing_id {
                Some(id) if exists(db, "tags", &id).await? => (id, false),
                _ => {
                    if !t.create {
                        continue;
                    }
                    let (id, made) = ensure_tag(db, &tag_name).await?;
                    (id, made)
                }
            }
        } else {
            match existing_id {
                Some(id) if exists(db, "collections", &id).await? => (id, false),
                _ => {
                    if !t.create {
                        continue;
                    }
                    let mut parent = base_parent.clone();
                    let mut leaf_id = String::new();
                    let mut leaf_created = false;
                    for (i, segment) in segments.iter().enumerate() {
                        let (id, made) = ensure_collection(db, segment, parent.as_deref()).await?;
                        if made {
                            // Record every created level so undo can peel the
                            // chain back from the deepest node.
                            record_applied(
                                db,
                                run_id,
                                "collection",
                                &id,
                                segment,
                                "",
                                true,
                                false,
                            )
                            .await?;
                            created_targets += 1;
                        }
                        let is_leaf = i + 1 == segments.len();
                        if is_leaf {
                            leaf_id = id;
                            leaf_created = made;
                        } else {
                            parent = Some(id);
                        }
                    }
                    (leaf_id, leaf_created)
                }
            }
        };

        if created {
            // `created_targets` already counted the chain above for collections;
            // tags are counted here.
            if kind == "tag" {
                created_targets += 1;
                record_applied(db, run_id, "tag", &target_id, &path, "", true, false).await?;
            }
        } else {
            reused_targets += 1;
        }

        for paper_id in &paper_ids {
            let sql = if kind == "tag" {
                "INSERT OR IGNORE INTO paper_tags (paper_id, tag_id) VALUES (?, ?)"
            } else {
                "INSERT OR IGNORE INTO paper_collections (paper_id, collection_id) VALUES (?, ?)"
            };
            let res = sqlx::query(sql)
                .bind(paper_id)
                .bind(&target_id)
                .execute(db)
                .await
                .map_err(|e| format!("db: {e}"))?;
            let link_created = res.rows_affected() > 0;
            if link_created {
                linked_papers += 1;
            }
            record_applied(
                db,
                run_id,
                kind,
                &target_id,
                segments.last().cloned().unwrap_or_default().as_str(),
                paper_id,
                false,
                link_created,
            )
            .await?;
        }
        applied_paths.push(path);
    }

    // Applying a term is the strongest signal that it belongs in the
    // vocabulary: promote it.
    for path in &applied_paths {
        vocab_upsert(db, &dimension, &target, path, "active", "user").await?;
    }

    let now = crate::core::time::now_iso();
    sqlx::query("UPDATE ai_classify_runs SET status = 'applied', applied_at = ? WHERE id = ?")
        .bind(&now)
        .bind(run_id)
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;

    Ok(ApplySummary {
        created_targets,
        reused_targets,
        linked_papers,
        skipped_papers,
    })
}

async fn exists(db: &SqlitePool, table: &str, id: &str) -> Result<bool, String> {
    let sql = format!("SELECT COUNT(*) FROM {table} WHERE id = ?");
    let count: i64 = sqlx::query_scalar(&sql)
        .bind(id)
        .fetch_one(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    Ok(count > 0)
}

/// Take a run back out: drop the links it created, then any category that only
/// this run created and that is now empty (deepest first, so a created parent
/// chain peels back level by level).
pub async fn undo_run(db: &SqlitePool, run_id: &str) -> Result<UndoSummary, String> {
    let rows = sqlx::query_as::<_, (String, String, String, i64, i64)>(
        "SELECT kind, target_id, paper_id, created_new, link_created
         FROM ai_classify_applied WHERE run_id = ?",
    )
    .bind(run_id)
    .fetch_all(db)
    .await
    .map_err(|e| format!("db: {e}"))?;
    if rows.is_empty() {
        return Err("这次分类没有可撤销的写入".to_string());
    }

    let mut removed_links = 0u64;
    // Links first, so the emptiness check below sees the post-undo state.
    for (kind, target_id, paper_id, _created_new, link_created) in &rows {
        if *link_created == 0 || paper_id.is_empty() {
            continue;
        }
        let sql = if kind == "tag" {
            "DELETE FROM paper_tags WHERE paper_id = ? AND tag_id = ?"
        } else {
            "DELETE FROM paper_collections WHERE paper_id = ? AND collection_id = ?"
        };
        let res = sqlx::query(sql)
            .bind(paper_id)
            .bind(target_id)
            .execute(db)
            .await
            .map_err(|e| format!("db: {e}"))?;
        removed_links += res.rows_affected();
    }

    // created targets, deepest collection last. Sorting by whether the target
    // still has children gives a single pass that works for parent chains.
    let created: Vec<(String, String)> = rows
        .iter()
        .filter(|(_, _, paper_id, created_new, _)| *created_new != 0 && paper_id.is_empty())
        .map(|(kind, target_id, _, _, _)| (kind.clone(), target_id.clone()))
        .collect();
    let mut removed_targets = 0u64;
    for _ in 0..2 {
        // Two passes cover a two-level chain (leaf, then its parent).
        for (kind, target_id) in created.clone() {
            let count: i64 = if kind == "tag" {
                sqlx::query_scalar("SELECT COUNT(*) FROM paper_tags WHERE tag_id = ?")
                    .bind(&target_id)
                    .fetch_one(db)
                    .await
                    .map_err(|e| format!("db: {e}"))?
            } else {
                sqlx::query_scalar("SELECT COUNT(*) FROM paper_collections WHERE collection_id = ?")
                    .bind(&target_id)
                    .fetch_one(db)
                    .await
                    .map_err(|e| format!("db: {e}"))?
            };
            if count > 0 {
                continue;
            }
            // Reuse the vetted service paths: they also orphan child tags /
            // unlink mapped folder notes / clear the junction rows.
            if kind == "tag" {
                if exists(db, "tags", &target_id).await? {
                    crate::core::tag_service::delete_tag(db, &target_id).await?;
                    removed_targets += 1;
                }
            } else {
                let children: i64 =
                    sqlx::query_scalar("SELECT COUNT(*) FROM collections WHERE parent_id = ?")
                        .bind(&target_id)
                        .fetch_one(db)
                        .await
                        .map_err(|e| format!("db: {e}"))?;
                if children > 0 || !exists(db, "collections", &target_id).await? {
                    continue;
                }
                crate::core::collection_service::delete_collection(db, &target_id).await?;
                removed_targets += 1;
            }
        }
    }

    let now = crate::core::time::now_iso();
    sqlx::query("UPDATE ai_classify_runs SET status = 'undone', undone_at = ? WHERE id = ?")
        .bind(&now)
        .bind(run_id)
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;

    Ok(UndoSummary { removed_links, removed_targets })
}

// ---------------------------------------------------------------------------
// P2: unattended rules
// ---------------------------------------------------------------------------

/// An unattended classification rule: re-runs the pipeline on a cron schedule
/// for papers it has not classified yet, then applies the high-confidence
/// assignments. The review queue is deliberately NOT automated — a rule only
/// ever writes what the threshold already accepted.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassifyRule {
    pub id: String,
    pub name: String,
    pub dimension: String,
    pub target: String,
    pub scope_kind: String,
    pub scope_id: Option<String>,
    pub max_groups: u32,
    pub threshold: f64,
    pub two_level: bool,
    pub reuse_existing: bool,
    pub cron: String,
    pub enabled: bool,
    pub provider_id: Option<String>,
    pub last_run_at: Option<String>,
    pub last_result: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassifyRuleInput {
    pub name: String,
    pub dimension: String,
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub scope_kind: String,
    #[serde(default)]
    pub scope_id: Option<String>,
    #[serde(default)]
    pub max_groups: u32,
    #[serde(default)]
    pub threshold: f64,
    #[serde(default = "default_true")]
    pub two_level: bool,
    #[serde(default = "default_true")]
    pub reuse_existing: bool,
    pub cron: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub provider_id: Option<String>,
}

/// Raw row for the rules table (17 columns — more than sqlx supports as a
/// tuple, so it is a derived struct).
#[derive(sqlx::FromRow)]
struct RuleRow {
    id: String,
    name: String,
    dimension: String,
    target: String,
    scope_kind: String,
    scope_id: Option<String>,
    max_groups: i64,
    threshold: f64,
    two_level: i64,
    reuse_existing: i64,
    cron: String,
    enabled: i64,
    provider_id: Option<String>,
    last_run_at: Option<String>,
    last_result: Option<String>,
    created_at: String,
    updated_at: String,
}

fn rule_from_row(r: RuleRow) -> ClassifyRule {
    ClassifyRule {
        id: r.id,
        name: r.name,
        dimension: r.dimension,
        target: r.target,
        scope_kind: r.scope_kind,
        scope_id: r.scope_id,
        max_groups: r.max_groups as u32,
        threshold: r.threshold,
        two_level: r.two_level != 0,
        reuse_existing: r.reuse_existing != 0,
        cron: r.cron,
        enabled: r.enabled != 0,
        provider_id: r.provider_id,
        last_run_at: r.last_run_at,
        last_result: r.last_result,
        created_at: r.created_at,
        updated_at: r.updated_at,
    }
}

pub async fn rules_list(db: &SqlitePool) -> Result<Vec<ClassifyRule>, String> {
    let sql = "SELECT * FROM ai_classify_rules ORDER BY created_at DESC";
    let rows = sqlx::query_as::<_, RuleRow>(sql)
        .fetch_all(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    Ok(rows.into_iter().map(rule_from_row).collect())
}

pub async fn rule_get(db: &SqlitePool, id: &str) -> Result<ClassifyRule, String> {
    let sql = "SELECT * FROM ai_classify_rules WHERE id = ?";
    let row = sqlx::query_as::<_, RuleRow>(sql)
        .bind(id)
        .fetch_optional(db)
        .await
        .map_err(|e| format!("db: {e}"))?
        .ok_or_else(|| format!("规则不存在: {id}"))?;
    Ok(rule_from_row(row))
}

/// Enabled rules — what the scheduler polls.
pub async fn rules_due(db: &SqlitePool) -> Result<Vec<ClassifyRule>, String> {
    let sql = "SELECT * FROM ai_classify_rules WHERE enabled = 1";
    let rows = sqlx::query_as::<_, RuleRow>(sql)
        .fetch_all(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    Ok(rows.into_iter().map(rule_from_row).collect())
}

pub async fn rule_save(
    db: &SqlitePool,
    id: Option<&str>,
    input: &ClassifyRuleInput,
) -> Result<ClassifyRule, String> {
    let dimension = input.dimension.trim();
    if dimension.is_empty() {
        return Err("请填写分类维度".to_string());
    }
    crate::core::cron_scheduler::validate(&input.cron)?;
    let target = if input.target.trim().is_empty() { "collection" } else { input.target.trim() };
    if target == "preview" {
        return Err("自动规则不能只预览，请选择写入集合或标签".to_string());
    }
    let scope_kind = if input.scope_kind.trim().is_empty() { "all" } else { input.scope_kind.trim() };
    let name = if input.name.trim().is_empty() {
        format!("{dimension} 自动归类")
    } else {
        input.name.trim().to_string()
    };
    let threshold = if input.threshold <= 0.0 || input.threshold > 1.0 {
        0.85
    } else {
        input.threshold
    };
    let max_groups = if input.max_groups == 0 { 8 } else { input.max_groups.min(30) };
    let now = crate::core::time::now_iso();
    let id = id.map(|s| s.to_string()).filter(|s| !s.is_empty());
    match id {
        Some(id) => {
            sqlx::query(
                "UPDATE ai_classify_rules SET name = ?, dimension = ?, target = ?, scope_kind = ?,
                   scope_id = ?, max_groups = ?, threshold = ?, two_level = ?, reuse_existing = ?,
                   cron = ?, enabled = ?, provider_id = ?, updated_at = ?
                 WHERE id = ?",
            )
            .bind(&name)
            .bind(dimension)
            .bind(target)
            .bind(scope_kind)
            .bind(&input.scope_id)
            .bind(max_groups as i64)
            .bind(threshold)
            .bind(if input.two_level { 1 } else { 0 })
            .bind(if input.reuse_existing { 1 } else { 0 })
            .bind(input.cron.trim())
            .bind(if input.enabled { 1 } else { 0 })
            .bind(&input.provider_id)
            .bind(&now)
            .bind(&id)
            .execute(db)
            .await
            .map_err(|e| format!("db: {e}"))?;
            rule_get(db, &id).await
        }
        None => {
            let id = uuid::Uuid::new_v4().to_string();
            sqlx::query(
                "INSERT INTO ai_classify_rules
                   (id, name, dimension, target, scope_kind, scope_id, max_groups, threshold,
                    two_level, reuse_existing, cron, enabled, provider_id, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(&name)
            .bind(dimension)
            .bind(target)
            .bind(scope_kind)
            .bind(&input.scope_id)
            .bind(max_groups as i64)
            .bind(threshold)
            .bind(if input.two_level { 1 } else { 0 })
            .bind(if input.reuse_existing { 1 } else { 0 })
            .bind(input.cron.trim())
            .bind(if input.enabled { 1 } else { 0 })
            .bind(&input.provider_id)
            .bind(&now)
            .bind(&now)
            .execute(db)
            .await
            .map_err(|e| format!("db: {e}"))?;
            rule_get(db, &id).await
        }
    }
}

pub async fn rule_set_enabled(db: &SqlitePool, id: &str, enabled: bool) -> Result<(), String> {
    let now = crate::core::time::now_iso();
    sqlx::query("UPDATE ai_classify_rules SET enabled = ?, updated_at = ? WHERE id = ?")
        .bind(if enabled { 1 } else { 0 })
        .bind(&now)
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    Ok(())
}

pub async fn rule_delete(db: &SqlitePool, id: &str) -> Result<(), String> {
    sqlx::query("DELETE FROM ai_classify_rules WHERE id = ?")
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    Ok(())
}

/// Papers a rule may consider. `all` is capped: an unattended run must not
/// silently start classifying a 20k-paper library.
const RULE_SCOPE_CAP: i64 = 500;

async fn scope_paper_ids(
    db: &SqlitePool,
    scope_kind: &str,
    scope_id: Option<&str>,
) -> Result<Vec<String>, String> {
    let rows = match (scope_kind, scope_id) {
        ("collection", Some(cid)) => {
            sqlx::query_as::<_, (String,)>(
                "SELECT p.id FROM papers p
                 JOIN paper_collections pc ON pc.paper_id = p.id
                 WHERE pc.collection_id = ? AND p.deleted_at IS NULL
                 ORDER BY p.imported_at DESC LIMIT ?",
            )
            .bind(cid)
            .bind(RULE_SCOPE_CAP)
            .fetch_all(db)
            .await
        }
        _ => {
            sqlx::query_as::<_, (String,)>(
                "SELECT id FROM papers WHERE deleted_at IS NULL
                 ORDER BY imported_at DESC LIMIT ?",
            )
            .bind(RULE_SCOPE_CAP)
            .fetch_all(db)
            .await
        }
    };
    Ok(rows.map_err(|e| format!("db: {e}"))?.into_iter().map(|(id,)| id).collect())
}

/// Fire one rule now. Safe to call from the scheduler or from the UI's
/// 「立即运行」: the pipeline's own RunGuard rejects overlaps.
pub async fn run_rule(app: AppHandle, db: SqlitePool, rule_id: String) {
    let outcome = rule_inner(&app, &db, &rule_id).await;
    let (text, failed) = match outcome {
        Ok(s) => (s, false),
        Err(e) => (format!("失败：{e}"), true),
    };
    if failed {
        tracing::warn!(rule_id, error = %text, "ai-classify rule failed");
    }
    let now = crate::core::time::now_iso();
    let _ = sqlx::query(
        "UPDATE ai_classify_rules SET last_run_at = ?, last_result = ?, updated_at = ? WHERE id = ?",
    )
    .bind(&now)
    .bind(&text)
    .bind(&now)
    .bind(&rule_id)
    .execute(&db)
    .await;
}

async fn rule_inner(app: &AppHandle, db: &SqlitePool, rule_id: &str) -> Result<String, String> {
    let rule = rule_get(db, rule_id).await?;
    if !rule.enabled {
        return Ok("规则已停用，跳过".to_string());
    }
    let target = if rule.target.trim().is_empty() { "collection".to_string() } else { rule.target.clone() };

    let mut ids = scope_paper_ids(db, &rule.scope_kind, rule.scope_id.as_deref()).await?;
    let in_scope = ids.len();
    let done = previously_classified(db, &rule.dimension, &target).await?;
    ids.retain(|id| !done.contains(id));
    if ids.is_empty() {
        return Ok(format!(
            "范围 {} 篇都已按「{}」分类过，无新文献",
            in_scope, rule.dimension
        ));
    }
    let fresh = ids.len();

    let run_id = uuid::Uuid::new_v4().to_string();
    let now = crate::core::time::now_iso();
    let paper_ids_json = serde_json::to_string(&ids).map_err(|e| format!("json: {e}"))?;
    sqlx::query(
        "INSERT INTO ai_classify_runs
           (id, scope_kind, scope_id, dimension, target, engine, sample_size,
            paper_ids_json, groups_json, provider_id, status, threshold,
            two_level, skip_existing, created_at)
         VALUES (?, ?, ?, ?, ?, 'batch', NULL, ?, '[]', ?, 'running', ?, ?, 1, ?)",
    )
    .bind(&run_id)
    .bind(&rule.scope_kind)
    .bind(&rule.scope_id)
    .bind(&rule.dimension)
    .bind(&target)
    .bind(&paper_ids_json)
    .bind(&rule.provider_id)
    .bind(rule.threshold)
    .bind(if rule.two_level { 1 } else { 0 })
    .bind(&now)
    .execute(db)
    .await
    .map_err(|e| format!("db: {e}"))?;

    let req = ClassifyRequest {
        paper_ids: ids,
        scope_kind: rule.scope_kind.clone(),
        scope_id: rule.scope_id.clone(),
        dimension: rule.dimension.clone(),
        target: target.clone(),
        sample_size: None,
        reuse_existing: rule.reuse_existing,
        max_groups: rule.max_groups,
        two_level: rule.two_level,
        threshold: rule.threshold,
        skip_existing: false,
        engine: "batch".to_string(),
        provider_id: rule.provider_id.clone(),
    };
    // run_draft takes the pipeline guard itself, so a concurrent interactive run
    // makes this a no-op rather than two interleaved runs.
    run_draft(app.clone(), db.clone(), run_id.clone(), req).await;

    let (_, _, status, _, _, _, groups) = load_run_row(db, &run_id).await?;
    if status != "done" {
        return Err(format!("起草未完成（{status}），未写入任何内容"));
    }
    if groups.is_empty() {
        return Err("没有产出任何分类".to_string());
    }

    let plan = ApplyPlan {
        targets: paths_of(&groups)
            .into_iter()
            .filter(|(p, _)| !p.trim().is_empty())
            .map(|(p, existing)| ApplyTarget { group_name: p, create: existing.is_none() })
            .collect(),
        parent_collection_id: if target == "collection" && rule.scope_kind == "collection" {
            rule.scope_id.clone()
        } else {
            None
        },
    };
    let summary = apply_run(db, &run_id, &plan).await?;
    Ok(format!(
        "「{}」新文献 {} 篇：新建 {} 个类目、复用 {}、写入 {} 条归属、{} 篇待审",
        rule.dimension,
        fresh,
        summary.created_targets,
        summary.reused_targets,
        summary.linked_papers,
        summary.skipped_papers
    ))
}

/// Semantic engine availability for the dialog (gated on a real embeddings
/// endpoint: the built-in lexical placeholder cannot be clustered).
pub fn semantic_available() -> bool {
    crate::core::ai_classify_semantic::available()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(name: &str, children: &[&str]) -> ClassifyGroup {
        ClassifyGroup {
            name: name.into(),
            description: String::new(),
            existing_id: None,
            children: children
                .iter()
                .map(|c| ClassifyGroup::new(*c, ""))
                .collect(),
        }
    }

    #[test]
    fn parse_draft_tolerates_fences_and_bare_ids() {
        let content = r#"```json
        {"groups":[{"name":"方法类","description":"讲方法","papers":[
          {"paper":"p1","confidence":0.9,"reason":"明确讲方法"},
          "p2"
        ]}]}
        ```"#;
        let (groups, assigns) = parse_draft(content).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "方法类");
        assert_eq!(assigns.len(), 2);
        assert_eq!(assigns[1].paper_id, "p2");
        assert_eq!(assigns[1].confidence, 0.0);
        assert_eq!(assigns[1].labels, vec!["方法类".to_string()]);
    }

    #[test]
    fn parse_draft_rejects_empty() {
        assert!(parse_draft("{\"groups\":[]}").is_err());
        assert!(parse_draft("没有 JSON").is_err());
    }

    #[test]
    fn parse_assign_reads_aliases_and_multi_labels() {
        let single = parse_assign(
            r#"{"assignments":[{"paper_id":"p1","group_name":"A","confidence":0.5}]}"#,
        )
        .unwrap();
        assert_eq!(single[0].paper_id, "p1");
        assert_eq!(single[0].labels, vec!["A".to_string()]);

        let multi = parse_assign(
            r#"{"assignments":[{"paper":"p2","labels":["A","B","A","C","D"],"confidence":0.9}]}"#,
        )
        .unwrap();
        // Deduped and capped at MAX_LABELS.
        assert_eq!(multi[0].labels, vec!["A".to_string(), "B".to_string(), "C".to_string()]);
    }

    #[test]
    fn parse_assign_accepts_empty_labels() {
        let list = parse_assign(r#"{"assignments":[{"paper":"p1","labels":[]}]}"#).unwrap();
        assert!(list[0].labels.is_empty());
        assert_eq!(list[0].primary(), "");
    }

    #[test]
    fn parse_merge_reads_from_lists() {
        let content = r#"{"groups":[{"name":"统一名","from":["甲","乙"]}]}"#;
        let merged = parse_merge(content).unwrap();
        assert_eq!(merged[0].name, "统一名");
        assert_eq!(merged[0].from, vec!["甲".to_string(), "乙".to_string()]);
    }

    #[test]
    fn parse_hierarchy_requires_children() {
        let ok = parse_hierarchy(
            r#"{"parents":[{"name":"算法","children":["分类","聚类"]}]}"#,
        )
        .unwrap();
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].children.len(), 2);
        // Empty children list = not a parent; the whole answer is then invalid.
        assert!(parse_hierarchy(r#"{"parents":[{"name":"算法","children":[]}]}"#).is_err());
    }

    #[test]
    fn apply_hierarchy_builds_tree_and_maps_paths() {
        let flat = vec![group("分类", &[]), group("聚类", &[]), group("综述", &[])];
        let parents = vec![RawParent {
            name: "算法".into(),
            description: "带监督的学习方式".into(),
            children: vec!["分类".into(), "聚类".into()],
        }];
        let (tree, mapping) = apply_hierarchy(&flat, &parents).unwrap();
        assert_eq!(tree.len(), 2); // 算法 + 未归位的综述
        assert_eq!(tree[0].name, "算法");
        assert_eq!(tree[0].children.len(), 2);
        assert_eq!(mapping.get(&norm("分类")).unwrap(), "算法/分类");
        assert!(tree[1].is_leaf());
    }

    #[test]
    fn apply_hierarchy_ignores_unknown_names_and_single_child_parents() {
        let flat = vec![group("分类", &[])];
        let unknown = vec![RawParent {
            name: "算法".into(),
            description: String::new(),
            children: vec!["不存在的类目".into()],
        }];
        assert!(apply_hierarchy(&flat, &unknown).is_none());

        let single = vec![RawParent {
            name: "算法".into(),
            description: String::new(),
            children: vec!["分类".into()],
        }];
        // One child → no point adding a level.
        assert!(apply_hierarchy(&flat, &single).is_none());
    }

    #[test]
    fn all_paths_flattens_two_levels() {
        let groups = vec![group("算法", &["分类", "聚类"]), group("综述", &[])];
        assert_eq!(
            all_paths(&groups),
            vec![
                "算法".to_string(),
                "算法/分类".to_string(),
                "算法/聚类".to_string(),
                "综述".to_string()
            ]
        );
    }

    #[test]
    fn split_path_handles_nesting_and_blanks() {
        assert_eq!(split_path("算法/分类"), vec!["算法".to_string(), "分类".to_string()]);
        assert_eq!(split_path("算法"), vec!["算法".to_string()]);
        assert_eq!(split_path(" / "), Vec::<String>::new());
    }

    #[test]
    fn triage_marks_low_confidence_as_pending() {
        assert_eq!(triage_status(0.9, 0.75), "accepted");
        assert_eq!(triage_status(0.75, 0.75), "accepted");
        assert_eq!(triage_status(0.4, 0.75), "pending");
    }

    #[test]
    fn norm_matches_case_and_space_insensitively() {
        assert_eq!(norm("  Deep Learning "), norm("deep learning"));
    }
}
