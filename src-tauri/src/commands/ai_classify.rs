//! AI 文献分类 commands: draft lifecycle, closed-set assignment, apply/undo,
//! and the per-dimension controlled vocabulary.

use tauri::{AppHandle, State};

use crate::core::ai_classify::{
    self, ApplyPlan, ApplySummary, ClassifyGroup, ClassifyRequest, ClassifyRule, ClassifyRuleInput,
    ClassifyRun, ItemEdit, PaperBrief, UndoSummary, VocabEntry,
};
use crate::AppState;

/// Create the run row, then spawn the draft phase (progress on
/// `library:ai-classify-progress`). Returns the new run id so the dialog can
/// fetch the result once the progress event says `finished`.
///
/// `skip_existing` drops papers a previous applied run already filed under the
/// same dimension + target (cross-run incremental).
#[tauri::command]
pub async fn ai_classify_start(
    app: AppHandle,
    state: State<'_, AppState>,
    req: ClassifyRequest,
) -> Result<String, String> {
    if req.paper_ids.is_empty() {
        return Err("未选择文献".to_string());
    }
    if req.dimension.trim().is_empty() {
        return Err("请填写分类维度".to_string());
    }
    let dimension = req.dimension.trim().to_string();
    let target = if req.target.trim().is_empty() {
        "collection".to_string()
    } else {
        req.target.clone()
    };

    let mut paper_ids = req.paper_ids.clone();
    if req.skip_existing {
        let done = ai_classify::previously_classified(&state.db, &dimension, &target).await?;
        paper_ids.retain(|id| !done.contains(id));
        if paper_ids.is_empty() {
            return Err("所选文献都已按该维度分类过（可关闭「跳过已分类」重跑）".to_string());
        }
    }

    let run_id = uuid::Uuid::new_v4().to_string();
    let now = crate::core::time::now_iso();
    let paper_ids_json = serde_json::to_string(&paper_ids).map_err(|e| format!("json: {e}"))?;
    let scope_kind = if req.scope_kind.trim().is_empty() {
        "selection".to_string()
    } else {
        req.scope_kind.clone()
    };
    let threshold = if req.threshold <= 0.0 || req.threshold > 1.0 {
        0.75
    } else {
        req.threshold
    };
    sqlx::query(
        "INSERT INTO ai_classify_runs
           (id, scope_kind, scope_id, dimension, target, engine, sample_size,
            paper_ids_json, groups_json, provider_id, status, threshold,
            two_level, skip_existing, created_at)
         VALUES (?, ?, ?, ?, ?, 'batch', ?, ?, '[]', ?, 'running', ?, ?, ?, ?)",
    )
    .bind(&run_id)
    .bind(&scope_kind)
    .bind(&req.scope_id)
    .bind(&dimension)
    .bind(&target)
    .bind(req.sample_size.map(|v| v as i64))
    .bind(&paper_ids_json)
    .bind(&req.provider_id)
    .bind(threshold)
    .bind(if req.two_level { 1 } else { 0 })
    .bind(if req.skip_existing { 1 } else { 0 })
    .bind(&now)
    .execute(&state.db)
    .await
    .map_err(|e| format!("db: {e}"))?;

    let db = state.db.clone();
    let spawn_id = run_id.clone();
    let mut req = req;
    req.paper_ids = paper_ids;
    req.dimension = dimension;
    req.target = target;
    req.threshold = threshold;
    tauri::async_runtime::spawn(async move {
        ai_classify::run_draft(app, db, spawn_id, req).await;
    });
    Ok(run_id)
}

#[tauri::command]
pub async fn ai_classify_cancel() -> Result<(), String> {
    ai_classify::request_cancel();
    Ok(())
}

#[tauri::command]
pub async fn ai_classify_run_get(
    state: State<'_, AppState>,
    run_id: String,
) -> Result<ClassifyRun, String> {
    ai_classify::get_run(&state.db, &run_id).await
}

/// Persist the (edited) draft taxonomy. This is the single authority for what
/// the closed-set assignment and the apply step may use.
#[tauri::command]
pub async fn ai_classify_groups_set(
    state: State<'_, AppState>,
    run_id: String,
    groups: Vec<ClassifyGroup>,
) -> Result<(), String> {
    ai_classify::groups_set(&state.db, &run_id, &groups).await
}

/// Per-paper manual override (empty labels = 待定).
#[tauri::command]
pub async fn ai_classify_item_set(
    state: State<'_, AppState>,
    run_id: String,
    paper_id: String,
    labels: Vec<String>,
) -> Result<(), String> {
    ai_classify::item_set(&state.db, &run_id, &paper_id, &labels).await
}

/// Bulk override. `edited: true` marks a real user edit (protected from later
/// reruns and applied as accepted); omitting it is a pure sync that keeps the
/// existing flag, confidence and reason.
#[tauri::command]
pub async fn ai_classify_items_set(
    state: State<'_, AppState>,
    run_id: String,
    items: Vec<ItemEdit>,
) -> Result<(), String> {
    ai_classify::items_set(&state.db, &run_id, &items).await
}

/// Closed-set assignment over the run's paper set. `only_missing` fills only
/// papers without a category (sample-then-full, or after adding a new group).
#[tauri::command]
pub async fn ai_classify_reassign(
    app: AppHandle,
    state: State<'_, AppState>,
    run_id: String,
    provider_id: Option<String>,
    only_missing: bool,
) -> Result<(), String> {
    let db = state.db.clone();
    tauri::async_runtime::spawn(async move {
        ai_classify::run_reassign(app, db, run_id, provider_id, only_missing).await;
    });
    Ok(())
}

#[tauri::command]
pub async fn ai_classify_apply(
    state: State<'_, AppState>,
    run_id: String,
    plan: ApplyPlan,
) -> Result<ApplySummary, String> {
    ai_classify::apply_run(&state.db, &run_id, &plan).await
}

#[tauri::command]
pub async fn ai_classify_undo(
    state: State<'_, AppState>,
    run_id: String,
) -> Result<UndoSummary, String> {
    ai_classify::undo_run(&state.db, &run_id).await
}

/// id/title/year/journal/abstract for the dialog's paper table and the
/// frontend's cost estimate (one round trip for the whole set).
#[tauri::command]
pub async fn ai_classify_papers_info(
    state: State<'_, AppState>,
    paper_ids: Vec<String>,
) -> Result<Vec<PaperBrief>, String> {
    ai_classify::papers_info(&state.db, &paper_ids).await
}

/// Controlled vocabulary rows for a dimension + target (active first).
#[tauri::command]
pub async fn ai_classify_vocab_list(
    state: State<'_, AppState>,
    dimension: String,
    target: String,
) -> Result<Vec<VocabEntry>, String> {
    ai_classify::vocab_list(&state.db, &dimension, &target).await
}

/// Promote (`active`), un-promote (`staged`) or reject (`rejected`) vocabulary
/// entries. A rejected term is never proposed again for that dimension.
#[tauri::command]
pub async fn ai_classify_vocab_set_status(
    state: State<'_, AppState>,
    ids: Vec<String>,
    status: String,
) -> Result<(), String> {
    ai_classify::vocab_set_status(&state.db, &ids, &status).await
}

/// What the dialog may offer. Semantic drafting needs a real embeddings
/// endpoint (the built-in lexical placeholder cannot be clustered).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassifyCapabilities {
    pub semantic: bool,
}

#[tauri::command]
pub async fn ai_classify_capabilities() -> Result<ClassifyCapabilities, String> {
    Ok(ClassifyCapabilities { semantic: ai_classify::semantic_available() })
}

#[tauri::command]
pub async fn ai_classify_rules_list(
    state: State<'_, AppState>,
) -> Result<Vec<ClassifyRule>, String> {
    ai_classify::rules_list(&state.db).await
}

/// Create (id = None) or update a rule. The schedule is validated here so a bad
/// cron expression never reaches the scheduler.
#[tauri::command]
pub async fn ai_classify_rule_save(
    state: State<'_, AppState>,
    id: Option<String>,
    input: ClassifyRuleInput,
) -> Result<ClassifyRule, String> {
    ai_classify::rule_save(&state.db, id.as_deref(), &input).await
}

#[tauri::command]
pub async fn ai_classify_rule_set_enabled(
    state: State<'_, AppState>,
    id: String,
    enabled: bool,
) -> Result<(), String> {
    ai_classify::rule_set_enabled(&state.db, &id, enabled).await
}

#[tauri::command]
pub async fn ai_classify_rule_delete(
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    ai_classify::rule_delete(&state.db, &id).await
}

/// Fire a rule immediately (same code path the scheduler uses).
#[tauri::command]
pub async fn ai_classify_rule_run(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    let db = state.db.clone();
    tauri::async_runtime::spawn(async move {
        ai_classify::run_rule(app, db, id).await;
    });
    Ok(())
}
