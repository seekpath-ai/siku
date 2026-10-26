use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use sqlx::SqlitePool;
use tauri::{AppHandle, Emitter};

use crate::ai::llm::{ChatMessage, LlmClient};
use crate::core::review_sources;
use crate::core::review_store::{self, RegisteredEvidence, ReviewAudit, ReviewPaperCard, ReviewRunState, ReviewSection, ReviewSnapshot, ReviewSynthesis, StoredSource};

pub const PROGRESS_EVENT: &str = "library:review-progress";
const CANCELLED: &str = "已取消";
const MAX_OUTPUT_TOKENS: u32 = crate::core::task_prompts::OUTPUT_CAP;
const SECTION_EVIDENCE_CAP: usize = 8000;
const SYNTHESIS_BATCH: usize = 6;

static RUNNING: AtomicBool = AtomicBool::new(false);
static ACTIVE_RUN: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
static CANCEL_RUNS: once_cell::sync::Lazy<std::sync::Mutex<HashSet<String>>> =
    once_cell::sync::Lazy::new(|| std::sync::Mutex::new(HashSet::new()));

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ReviewProgress {
    pub stage: String,
    pub section_index: usize,
    pub total_sections: usize,
    pub section_title: String,
    pub note_id: Option<String>,
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section_status: Option<String>,
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub done_sections: Option<usize>,
    pub run_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paper_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub done_papers: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_papers: Option<usize>,
}

fn base_progress(run_id: &str, stage: &str) -> ReviewProgress {
    ReviewProgress { run_id: run_id.into(), stage: stage.into(), ..Default::default() }
}

fn emit(app: Option<&AppHandle>, progress: &ReviewProgress) {
    if let Some(app) = app {
        if let Err(e) = app.emit(PROGRESS_EVENT, progress) {
            tracing::warn!(error = %e, "failed to emit review progress");
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn section_progress(
    run_id: &str,
    status: &str,
    index: usize,
    total: usize,
    title: &str,
    attempt: u32,
    max_tokens: u32,
    done: usize,
    error: Option<String>,
) -> ReviewProgress {
    ReviewProgress {
        run_id: run_id.into(),
        stage: "section".into(),
        section_index: index + 1,
        total_sections: total,
        section_title: title.into(),
        section_status: Some(status.into()),
        attempt: Some(attempt.saturating_sub(1)),
        max_tokens: Some(max_tokens),
        done_sections: Some(done),
        error,
        ..Default::default()
    }
}

struct RunGuard {
    run_id: String,
}

impl RunGuard {
    fn try_acquire(run_id: &str) -> Result<Self, String> {
        let mut active = ACTIVE_RUN.lock().map_err(|_| "综述任务锁失败".to_string())?;
        if active.is_some() || RUNNING.swap(true, Ordering::SeqCst) {
            return Err("已有综述生成任务正在进行中".to_string());
        }
        *active = Some(run_id.to_string());
        Ok(Self { run_id: run_id.to_string() })
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        if let Ok(mut active) = ACTIVE_RUN.lock() {
            if active.as_deref() == Some(self.run_id.as_str()) {
                *active = None;
            }
        }
        RUNNING.store(false, Ordering::SeqCst);
    }
}

fn is_active(run_id: &str) -> bool {
    ACTIVE_RUN.lock().map(|g| g.as_deref() == Some(run_id)).unwrap_or(false)
}

async fn cancelled_now(run_id: String) {
    loop {
        if CANCEL_RUNS.lock().map(|g| g.contains(&run_id)).unwrap_or(false) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

fn clear_mem_cancel(run_id: &str) {
    if let Ok(mut guard) = CANCEL_RUNS.lock() {
        guard.remove(run_id);
    }
}

async fn check_cancel(db: &SqlitePool, run_id: &str) -> Result<(), String> {
    let memory = CANCEL_RUNS.lock().map(|g| g.contains(run_id)).unwrap_or(false);
    if memory || review_store::is_cancelled(db, run_id).await? {
        Err(CANCELLED.into())
    } else {
        Ok(())
    }
}

pub async fn request_cancel(db: &SqlitePool, run_id: &str) -> Result<(), String> {
    review_store::load_state(db, run_id).await?;
    if let Ok(mut guard) = CANCEL_RUNS.lock() {
        guard.insert(run_id.to_string());
    }
    review_store::request_cancel(db, run_id).await
}

pub(crate) fn chat_msg(role: &str, content: String) -> ChatMessage {
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

fn section_need(words: usize) -> u32 {
    (words as u32).saturating_mul(2).saturating_add(6000).min(MAX_OUTPUT_TOKENS)
}

async fn llm_with_need(
    db: &SqlitePool,
    provider_id: Option<&str>,
    need: u32,
) -> Result<Box<dyn LlmClient>, String> {
    let need = crate::core::task_prompts::apply_floor(
        need,
        crate::core::task_prompts::budget_floor(db, "review").await,
    );
    let mut cfg = crate::core::llm_provider_service::load_llm_config_for(db, provider_id).await?;
    if cfg.max_tokens < need {
        cfg.max_tokens = need;
    }
    crate::ai::llm::client::create_llm_client(&cfg)
}

async fn default_llm(
    db: &SqlitePool,
    provider_id: Option<&str>,
    min_max_tokens: u32,
) -> Result<Box<dyn LlmClient>, String> {
    let need = min_max_tokens.saturating_mul(2).saturating_add(4000).min(MAX_OUTPUT_TOKENS);
    llm_with_need(db, provider_id, need).await
}

pub async fn create(
    db: &SqlitePool,
    paper_ids: &[String],
    question: &str,
    scope: &str,
    language: &str,
    target_words: u32,
    provider_id: Option<&str>,
) -> Result<ReviewSnapshot, String> {
    if paper_ids.is_empty() {
        return Err("未选择文献".to_string());
    }
    if question.trim().is_empty() {
        return Err("请填写研究问题".to_string());
    }
    if target_words == 0 {
        return Err("目标字数必须大于 0".to_string());
    }
    let sources = review_sources::freeze_sources(db, paper_ids).await?;
    review_store::create_run(
        db,
        question.trim(),
        scope.trim(),
        if language.trim().is_empty() { "中文" } else { language.trim() },
        target_words,
        provider_id,
        sources,
    )
    .await
}

pub async fn get(db: &SqlitePool, run_id: &str) -> Result<ReviewSnapshot, String> {
    if !is_active(run_id) {
        review_store::recover_interrupted(db, run_id).await?;
    }
    review_store::load_state(db, run_id).await.map(|s| s.snapshot())
}

fn done_papers(state: &ReviewRunState) -> usize {
    state.sources.iter().filter(|s| s.source.status == "done").count()
}

fn paper_progress(run_id: &str, state: &ReviewRunState, paper_id: &str, status: &str, error: Option<String>) -> ReviewProgress {
    ReviewProgress {
        run_id: run_id.into(),
        stage: "read".into(),
        paper_id: Some(paper_id.into()),
        done_papers: Some(done_papers(state)),
        total_papers: Some(state.sources.len()),
        section_status: Some(status.into()),
        error,
        ..Default::default()
    }
}

pub async fn prepare_read_retry(db: &SqlitePool, run_id: &str, paper_id: &str) -> Result<(), String> {
    if is_active(run_id) {
        return Err("该综述任务仍在运行".to_string());
    }
    let state = review_store::load_state(db, run_id).await?;
    let source = state.source(paper_id).ok_or_else(|| "文献不属于该综述任务".to_string())?;
    if source.source.source_kind == "unusable" {
        return Err("该文献没有可重试的全文或摘要".to_string());
    }
    review_store::set_source_status(db, run_id, paper_id, "pending", None).await?;
    review_store::invalidate_downstream(db, run_id).await
}

pub async fn run_reading(app: AppHandle, db: SqlitePool, run_id: String) {
    if let Err(e) = run_reading_inner(Some(&app), &db, &run_id, None).await {
        if e != CANCELLED {
            // RunGuard 冲突说明同一 run 已有任务在执行，本次只是重复触发；
            // 不能把别人正在跑的任务标成 failed。guard 在本任务自己的 inner
            // 返回时已释放，所以此时 is_active 为真必属另一执行体。
            if is_active(&run_id) {
                return;
            }
            tracing::warn!(run_id, error = %e, "review reading failed");
            let _ = review_store::set_run_status(&db, &run_id, "failed", "read", Some(&e)).await;
            emit(Some(&app), &ReviewProgress {
                run_id,
                stage: "error".into(),
                message: Some(e.clone()),
                error: Some(e),
                ..Default::default()
            });
        }
    }
}

async fn run_reading_inner(
    app: Option<&AppHandle>,
    db: &SqlitePool,
    run_id: &str,
    injected_llm: Option<&dyn LlmClient>,
) -> Result<(), String> {
    let _guard = RunGuard::try_acquire(run_id)?;
    clear_mem_cancel(run_id);
    review_store::clear_cancel(db, run_id).await?;
    let initial = review_store::load_state(db, run_id).await?;
    review_store::set_run_status(db, run_id, "reading", "read", None).await?;
    let owned_llm;
    let llm = if let Some(llm) = injected_llm {
        llm
    } else {
        owned_llm = default_llm(db, initial.run.provider_id.as_deref(), 2500).await?;
        owned_llm.as_ref()
    };
    let card_prompt = crate::core::task_prompts::effective(db, crate::core::task_prompts::REVIEW_CARD).await;
    let pending: Vec<String> = initial
        .sources
        .iter()
        .filter(|s| s.source.status == "pending" && s.source.source_kind != "unusable")
        .map(|s| s.source.paper_id.clone())
        .collect();

    for paper_id in pending {
        if let Err(e) = check_cancel(db, run_id).await {
            if e == CANCELLED {
                return finish_cancelled_reading(app, db, run_id).await;
            }
            return Err(e);
        }
        review_store::set_source_status(db, run_id, &paper_id, "running", None).await?;
        let mut state = review_store::load_state(db, run_id).await?;
        emit(app, &paper_progress(run_id, &state, &paper_id, "running", None));
        let source = state.source(&paper_id).cloned().ok_or_else(|| "文献快照缺失".to_string())?;
        let cache_key = review_sources::card_cache_key(
            &state.run.question,
            &state.run.scope,
            &source.index_fingerprint,
            &card_prompt,
        );
        review_store::set_card_cache_key(db, run_id, &paper_id, &cache_key).await?;
        let cached = review_sources::normalize_cached_card(db, run_id, &paper_id, &cache_key, &source).await?;
        let generated = match cached {
            Some(card) => Ok(card),
            None => {
                tokio::select! {
                    result = review_sources::generate_card(db, llm, &source, &state.run.question, &state.run.scope, &card_prompt) => result,
                    _ = cancelled_now(run_id.to_string()) => Err(CANCELLED.to_string()),
                }
            }
        };
        match generated {
            Ok(card) => {
                state = review_store::load_state(db, run_id).await?;
                let source = state.source(&paper_id).cloned().ok_or_else(|| "文献快照缺失".to_string())?;
                let (card, registry) = review_sources::assign_global_evidence(
                    card,
                    &source,
                    &state.run.evidence_registry,
                    false,
                );
                review_store::save_card(db, run_id, &paper_id, &card, &registry, false).await?;
                state = review_store::load_state(db, run_id).await?;
                emit(app, &paper_progress(run_id, &state, &paper_id, "done", None));
            }
            Err(e) if e == CANCELLED => {
                review_store::set_source_status(db, run_id, &paper_id, "pending", None).await?;
                return finish_cancelled_reading(app, db, run_id).await;
            }
            Err(e) => {
                review_store::set_source_status(db, run_id, &paper_id, "failed", Some(&e)).await?;
                state = review_store::load_state(db, run_id).await?;
                emit(app, &paper_progress(run_id, &state, &paper_id, "failed", Some(e)));
            }
        }
    }

    if let Err(e) = check_cancel(db, run_id).await {
        if e == CANCELLED {
            return finish_cancelled_reading(app, db, run_id).await;
        }
        return Err(e);
    }
    let state = review_store::load_state(db, run_id).await?;
    if state.run.synthesis.is_some() {
        review_store::set_run_status(db, run_id, "read_ready", "outline", None).await?;
        return Ok(());
    }
    if state.usable_sources().is_empty() {
        return Err("没有成功生成任何证据卡；请检查阅读失败原因后重试".to_string());
    }
    review_store::set_run_status(db, run_id, "reading", "synthesize", None).await?;
    emit(app, &base_progress(run_id, "synthesize"));
    let synthesis_prompt = crate::core::task_prompts::effective(db, crate::core::task_prompts::REVIEW_SYNTHESIZE).await;
    let synthesis = tokio::select! {
        result = synthesize_cards(db, llm, &state, &synthesis_prompt) => result,
        _ = cancelled_now(run_id.to_string()) => Err(CANCELLED.to_string()),
    };
    match synthesis {
        Ok(synthesis) => {
            review_store::save_synthesis(db, run_id, &synthesis).await?;
            let mut progress = base_progress(run_id, "read_ready");
            progress.done_papers = Some(done_papers(&state));
            progress.total_papers = Some(state.sources.len());
            emit(app, &progress);
            Ok(())
        }
        Err(e) if e == CANCELLED => finish_cancelled_reading(app, db, run_id).await,
        Err(e) => Err(e),
    }
}

async fn finish_cancelled_reading(app: Option<&AppHandle>, db: &SqlitePool, run_id: &str) -> Result<(), String> {
    review_store::set_run_status(db, run_id, "cancelled", "read", None).await?;
    let state = review_store::load_state(db, run_id).await?;
    let mut progress = base_progress(run_id, "cancelled");
    progress.done_papers = Some(done_papers(&state));
    progress.total_papers = Some(state.sources.len());
    progress.message = Some("已取消；已完成的证据卡已保存，可继续阅读或单篇重试".into());
    emit(app, &progress);
    Ok(())
}

fn compressed_cards(state: &ReviewRunState) -> Vec<serde_json::Value> {
    state
        .usable_sources()
        .into_iter()
        .filter_map(|source| {
            let card = source.source.card.as_ref()?;
            Some(serde_json::json!({
                "paperId": source.source.paper_id,
                "paperIndex": source.source.paper_index,
                "title": source.source.title,
                "sourceType": card.source_type,
                "researchQuestion": card.research_question,
                "methods": card.methods,
                "dataAndSample": card.data_and_sample,
                "findings": card.findings,
                "limitations": card.limitations,
                "relevance": card.relevance,
            }))
        })
        .collect()
}

async fn synthesize_cards(
    db: &SqlitePool,
    llm: &dyn LlmClient,
    state: &ReviewRunState,
    prompt: &str,
) -> Result<ReviewSynthesis, String> {
    let cards = compressed_cards(state);
    let all_ids: HashSet<String> = state.usable_sources().iter().map(|s| s.source.paper_id.clone()).collect();
    let mut batches = Vec::new();
    for batch in cards.chunks(SYNTHESIS_BATCH) {
        let allowed: HashSet<String> = batch
            .iter()
            .filter_map(|card| card.get("paperId")?.as_str().map(str::to_string))
            .collect();
        let user = format!(
            "研究问题：{}\n范围限定：{}\n\n本批证据卡（不得遗漏任何一篇）：\n{}",
            state.run.question,
            state.run.scope,
            serde_json::to_string_pretty(batch).map_err(|e| e.to_string())?
        );
        let response = llm
            .chat_completion(&[chat_msg("system", prompt.into()), chat_msg("user", user)], &[])
            .await?;
        let mut synthesis = review_sources::parse_synthesis(&response.content)?;
        review_sources::validate_synthesis(&mut synthesis, &allowed)?;
        batches.push(synthesis);
    }
    if batches.len() == 1 {
        return batches.pop().ok_or_else(|| "没有可综合的证据卡".to_string());
    }
    let user = format!(
        "研究问题：{}\n范围限定：{}\n以下是按批次完成的综合，请合并同类项并保留所有重要差异，不得丢弃批次：\n{}\n\n有效 paperId：{}",
        state.run.question,
        state.run.scope,
        serde_json::to_string_pretty(&batches).map_err(|e| e.to_string())?,
        all_ids.iter().cloned().collect::<Vec<_>>().join(",")
    );
    let response = llm
        .chat_completion(&[chat_msg("system", prompt.into()), chat_msg("user", user)], &[])
        .await?;
    let mut merged = review_sources::parse_synthesis(&response.content)?;
    review_sources::validate_synthesis(&mut merged, &all_ids)?;
    let _ = db;
    Ok(merged)
}

pub async fn update_card(
    db: &SqlitePool,
    run_id: &str,
    paper_id: &str,
    card: ReviewPaperCard,
) -> Result<ReviewSnapshot, String> {
    if is_active(run_id) {
        return Err("任务运行中不能编辑证据卡".to_string());
    }
    let state = review_store::load_state(db, run_id).await?;
    let source = state.source(paper_id).cloned().ok_or_else(|| "文献不属于该综述任务".to_string())?;
    let card = review_sources::normalize_edited_card(card, &source)?;
    let (card, registry) = review_sources::assign_global_evidence(
        card,
        &source,
        &state.run.evidence_registry,
        true,
    );
    review_store::save_card(db, run_id, paper_id, &card, &registry, true).await?;
    review_store::set_run_status(db, run_id, "read_ready", "read", None).await?;
    review_store::load_state(db, run_id).await.map(|s| s.snapshot())
}

pub async fn generate_outline(app: &AppHandle, db: &SqlitePool, run_id: &str) -> Result<Vec<ReviewSection>, String> {
    let _guard = RunGuard::try_acquire(run_id)?;
    clear_mem_cancel(run_id);
    review_store::clear_cancel(db, run_id).await?;
    let state = review_store::load_state(db, run_id).await?;
    let llm = default_llm(db, state.run.provider_id.as_deref(), 1800).await?;
    generate_outline_with_llm(Some(app), db, llm.as_ref(), run_id).await
}

async fn generate_outline_with_llm(
    app: Option<&AppHandle>,
    db: &SqlitePool,
    llm: &dyn LlmClient,
    run_id: &str,
) -> Result<Vec<ReviewSection>, String> {
    let state = review_store::load_state(db, run_id).await?;
    let synthesis = state.run.synthesis.clone().ok_or_else(|| "请先完成逐篇阅读和跨文献综合".to_string())?;
    if state.usable_sources().is_empty() {
        return Err("没有可用于大纲的证据卡".to_string());
    }
    review_store::set_run_status(db, run_id, "outlining", "outline", None).await?;
    let prompt = crate::core::task_prompts::effective(db, crate::core::task_prompts::REVIEW_OUTLINE).await;
    let section_count = ((state.run.target_words / 800) as usize).clamp(3, 8);
    let user = format!(
        "研究问题：{}\n范围限定：{}\n写作语言：{}\n全文目标约 {} 字，请设计 {} 个章节（含引言与总结/展望）。\n\n跨文献综合：\n{}\n\n压缩证据卡（每节必须从真实 paperId 中分配文献）：\n{}",
        state.run.question,
        state.run.scope,
        state.run.language,
        state.run.target_words,
        section_count,
        serde_json::to_string_pretty(&synthesis).map_err(|e| e.to_string())?,
        serde_json::to_string_pretty(&compressed_cards(&state)).map_err(|e| e.to_string())?
    );
    let result = stream_completion(
        app,
        llm,
        &[chat_msg("system", prompt), chat_msg("user", user)],
        run_id,
        "outline",
        0,
        0,
        0,
    )
    .await;
    match result {
        Ok((text, _)) => {
            let allowed: HashSet<String> = state.usable_sources().iter().map(|s| s.source.paper_id.clone()).collect();
            match parse_outline(&text, &allowed) {
                Ok(mut outline) => {
                    outline.truncate(10);
                    review_store::save_outline(db, run_id, &outline).await?;
                    Ok(outline)
                }
                Err(e) => {
                    review_store::set_run_status(db, run_id, "read_ready", "outline", Some(&e)).await?;
                    Err(e)
                }
            }
        }
        Err(e) => {
            review_store::set_run_status(db, run_id, "read_ready", "outline", Some(&e)).await?;
            Err(e)
        }
    }
}

fn parse_outline(content: &str, allowed: &HashSet<String>) -> Result<Vec<ReviewSection>, String> {
    let trimmed = content.trim();
    let start = trimmed.find('[').ok_or_else(|| "模型未返回大纲 JSON".to_string())?;
    let end = trimmed.rfind(']').ok_or_else(|| "模型未返回大纲 JSON".to_string())?;
    if end < start {
        return Err("模型未返回有效大纲".to_string());
    }
    let mut outline: Vec<ReviewSection> = serde_json::from_str(&trimmed[start..=end])
        .map_err(|e| format!("模型未返回有效大纲：{e}"))?;
    for section in &mut outline {
        section.title = section.title.trim().to_string();
        section.points = section.points.trim().to_string();
        section.subsections = section
            .subsections
            .drain(..)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if section.paper_ids.iter().any(|id| !allowed.contains(id)) {
            return Err(format!("大纲章节「{}」包含未知或不可用 paperId", section.title));
        }
        let mut seen = HashSet::new();
        section.paper_ids.retain(|id| seen.insert(id.clone()));
    }
    outline.retain(|s| !s.title.is_empty());
    if outline.is_empty() {
        Err("模型未返回有效大纲".to_string())
    } else {
        Ok(outline)
    }
}

#[allow(clippy::too_many_arguments)]
async fn stream_completion(
    app: Option<&AppHandle>,
    llm: &dyn LlmClient,
    messages: &[ChatMessage],
    run_id: &str,
    stage: &str,
    section_index: usize,
    total_sections: usize,
    attempt: u32,
) -> Result<(String, Option<String>), String> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<crate::ai::llm::StreamEvent>();
    let app = app.cloned();
    let run_id_owned = run_id.to_string();
    let stage_owned = stage.to_string();
    let drain = tokio::spawn(async move {
        let mut text = String::new();
        let mut finish = None;
        while let Some(event) = rx.recv().await {
            match event.event_type.as_str() {
                "delta" => {
                    if let Some(content) = event.content {
                        text.push_str(&content);
                        emit(app.as_ref(), &ReviewProgress {
                            run_id: run_id_owned.clone(),
                            stage: stage_owned.clone(),
                            section_index,
                            total_sections,
                            attempt: Some(attempt),
                            delta: Some(content),
                            ..Default::default()
                        });
                    }
                }
                "reasoning" => {
                    if let Some(content) = event.content {
                        emit(app.as_ref(), &ReviewProgress {
                            run_id: run_id_owned.clone(),
                            stage: stage_owned.clone(),
                            section_index,
                            total_sections,
                            attempt: Some(attempt),
                            reasoning: Some(content),
                            ..Default::default()
                        });
                    }
                }
                "finish" => finish = event.content,
                _ => {}
            }
        }
        (text, finish)
    });
    let call = tokio::select! {
        result = llm.chat_completion_stream(messages, &[], tx) => result,
        _ = cancelled_now(run_id.to_string()) => Err(CANCELLED.to_string()),
    };
    let (text, finish) = drain.await.map_err(|e| format!("stream drain: {e}"))?;
    call?;
    Ok((text, finish))
}

pub async fn prepare_generation(db: &SqlitePool, run_id: &str, outline: &[ReviewSection]) -> Result<(), String> {
    if is_active(run_id) {
        return Err("该综述任务仍在运行".to_string());
    }
    if outline.is_empty() {
        return Err("大纲为空".to_string());
    }
    if outline.iter().any(|s| s.title.trim().is_empty()) {
        return Err("大纲包含空章节标题".to_string());
    }
    let state = review_store::load_state(db, run_id).await?;
    if state.run.synthesis.is_none() || state.usable_sources().is_empty() {
        return Err("请先完成阅读与跨文献综合".to_string());
    }
    let allowed: HashSet<String> = state.usable_sources().iter().map(|s| s.source.paper_id.clone()).collect();
    if outline.iter().flat_map(|s| &s.paper_ids).any(|id| !allowed.contains(id)) {
        return Err("大纲包含未知或不可用 paperId".to_string());
    }
    review_store::prepare_generation(db, run_id, outline).await
}

pub async fn prepare_section_retry(db: &SqlitePool, run_id: &str, section_index: usize) -> Result<(), String> {
    if is_active(run_id) {
        return Err("该综述任务仍在运行".to_string());
    }
    review_store::prepare_section_retry(db, run_id, section_index).await
}

pub async fn run_generation(app: AppHandle, db: SqlitePool, run_id: String) {
    if let Err(e) = run_generation_inner(Some(&app), &db, &run_id, None).await {
        if e != CANCELLED {
            // 与 run_reading 同理：RunGuard 冲突 = 同一 run 已有执行体在跑，
            // 静默退出，不覆盖运行中任务的状态。
            if is_active(&run_id) {
                return;
            }
            tracing::warn!(run_id, error = %e, "review generation failed");
            let _ = review_store::set_run_status(&db, &run_id, "failed", "section", Some(&e)).await;
            emit(Some(&app), &ReviewProgress {
                run_id,
                stage: "error".into(),
                error: Some(e.clone()),
                message: Some(e),
                ..Default::default()
            });
        }
    }
}

async fn run_generation_inner(
    app: Option<&AppHandle>,
    db: &SqlitePool,
    run_id: &str,
    injected_llm: Option<&dyn LlmClient>,
) -> Result<(), String> {
    let _guard = RunGuard::try_acquire(run_id)?;
    clear_mem_cancel(run_id);
    review_store::clear_cancel(db, run_id).await?;
    review_store::set_run_status(db, run_id, "generating", "section", None).await?;
    loop {
        let state = review_store::load_state(db, run_id).await?;
        let next = state.sections.iter().find(|s| s.status == "pending").map(|s| s.section_index);
        let Some(index) = next else { break };
        if index >= state.run.outline.len() {
            return Err("分节状态与大纲不一致".to_string());
        }
        check_cancel_or_finish(app, db, run_id).await?;
        let section = state.run.outline[index].clone();
        let total = state.run.outline.len();
        let weights: Vec<usize> = state.run.outline.iter().map(|s| 1 + s.subsections.len()).collect();
        let weight_sum: usize = weights.iter().sum();
        let words = (state.run.target_words as usize * weights[index] / weight_sum.max(1)).max(300);
        let attempts = review_store::set_section_running(db, run_id, index).await?;
        let exponent = attempts.saturating_sub(1).min(6);
        let need = section_need(words).saturating_mul(1u32 << exponent).min(MAX_OUTPUT_TOKENS);
        emit(app, &section_progress(run_id, "running", index, total, &section.title, attempts, need, done_sections(&state), None));
        let owned_llm;
        let llm = if let Some(llm) = injected_llm {
            llm
        } else {
            owned_llm = match llm_with_need(db, state.run.provider_id.as_deref(), need).await {
                Ok(llm) => llm,
                Err(e) => {
                    review_store::set_section_result(db, run_id, index, "failed", None, Some(&e)).await?;
                    continue;
                }
            };
            owned_llm.as_ref()
        };
        let result = write_section_with_llm(app, db, llm, run_id, index, words, attempts, need).await;
        match result {
            Ok(raw) => {
                review_store::set_section_result(db, run_id, index, "done", Some(&raw), None).await?;
                let state = review_store::load_state(db, run_id).await?;
                emit(app, &section_progress(run_id, "done", index, total, &section.title, attempts, need, done_sections(&state), None));
            }
            Err(e) if e == CANCELLED => {
                review_store::set_section_result(db, run_id, index, "pending", None, None).await?;
                return finish_cancelled_generation(app, db, run_id).await;
            }
            Err(e) => {
                review_store::set_section_result(db, run_id, index, "failed", None, Some(&e)).await?;
                let state = review_store::load_state(db, run_id).await?;
                emit(app, &section_progress(run_id, "failed", index, total, &section.title, attempts, need, done_sections(&state), Some(e)));
            }
        }
    }
    finalize_run(app, db, run_id, injected_llm).await
}

fn done_sections(state: &ReviewRunState) -> usize {
    state.sections.iter().filter(|s| s.status == "done" && !s.raw_markdown.trim().is_empty()).count()
}

async fn check_cancel_or_finish(app: Option<&AppHandle>, db: &SqlitePool, run_id: &str) -> Result<(), String> {
    match check_cancel(db, run_id).await {
        Ok(()) => Ok(()),
        Err(e) if e == CANCELLED => {
            finish_cancelled_generation(app, db, run_id).await?;
            Err(e)
        }
        Err(e) => Err(e),
    }
}

async fn finish_cancelled_generation(app: Option<&AppHandle>, db: &SqlitePool, run_id: &str) -> Result<(), String> {
    let state = review_store::load_state(db, run_id).await?;
    if done_sections(&state) > 0 {
        let raw = assemble_raw(&state, true)?;
        let audit = compute_audit(&raw, &state);
        review_store::save_final(db, run_id, &raw, &audit, "cancelled", "section").await?;
    } else {
        review_store::set_run_status(db, run_id, "cancelled", "section", None).await?;
    }
    let state = review_store::load_state(db, run_id).await?;
    let mut progress = base_progress(run_id, "cancelled");
    progress.total_sections = state.run.outline.len();
    progress.done_sections = Some(done_sections(&state));
    progress.message = Some("已取消；已完成分节已保存，可单节续跑或保存部分综述".into());
    emit(app, &progress);
    Ok(())
}

async fn section_evidence(
    db: &SqlitePool,
    state: &ReviewRunState,
    section: &ReviewSection,
) -> Result<Vec<RegisteredEvidence>, String> {
    let usable: HashSet<String> = state.usable_sources().iter().map(|s| s.source.paper_id.clone()).collect();
    let assigned: HashSet<String> = if section.paper_ids.is_empty() {
        usable.clone()
    } else {
        section.paper_ids.iter().filter(|id| usable.contains(*id)).cloned().collect()
    };
    let current_ids: HashSet<String> = state
        .usable_sources()
        .iter()
        .filter_map(|s| s.source.card.as_ref())
        .flat_map(|card| card.evidence.iter().map(|e| e.id.clone()))
        .collect();
    let scope: Vec<String> = assigned.iter().cloned().collect();
    let query = format!("{} {}", section.title, section.points);
    let hits = crate::ai::retriever::hybrid_search_scoped(db, &query, 16, false, Some(&scope)).await?;
    let rank: HashMap<String, usize> = hits
        .iter()
        .enumerate()
        .filter_map(|(i, hit)| hit.chunk_id.split_once('\0').map(|_| (hit.chunk_id.clone(), i)).or(Some((hit.chunk_id.clone(), i))))
        .collect();
    let mut primary: Vec<RegisteredEvidence> = state
        .run
        .evidence_registry
        .iter()
        .filter(|ev| assigned.contains(&ev.paper_id) && current_ids.contains(&ev.id))
        .cloned()
        .collect();
    primary.sort_by_key(|ev| {
        (
            ev.chunk_id.as_ref().and_then(|id| rank.get(id)).copied().unwrap_or(usize::MAX),
            ev.paper_index,
            evidence_number(&ev.id),
        )
    });
    let mut selected = Vec::new();
    let mut chars = 0usize;
    for ev in primary {
        if chars >= SECTION_EVIDENCE_CAP {
            break;
        }
        chars += ev.content.chars().count().min(1200);
        selected.push(ev);
    }
    if selected.len() < 3 {
        for ev in &state.run.evidence_registry {
            if chars >= SECTION_EVIDENCE_CAP {
                break;
            }
            if !usable.contains(&ev.paper_id) || !current_ids.contains(&ev.id) || selected.iter().any(|e| e.id == ev.id) {
                continue;
            }
            chars += ev.content.chars().count().min(1200);
            selected.push(ev.clone());
        }
    }
    Ok(selected)
}

fn evidence_number(id: &str) -> usize {
    id.strip_prefix('E').and_then(|n| n.parse().ok()).unwrap_or(usize::MAX)
}

fn evidence_block(evidence: &[RegisteredEvidence], state: &ReviewRunState) -> String {
    if evidence.is_empty() {
        return "（当前证据卡没有可用于本节的原文，请减少具体断言）".into();
    }
    let mut remaining = SECTION_EVIDENCE_CAP;
    let mut out = Vec::new();
    for ev in evidence {
        if remaining == 0 {
            break;
        }
        let Some(source) = state.source(&ev.paper_id) else { continue };
        let location = if ev.source_type == "abstract" {
            "摘要级证据，无页码".to_string()
        } else {
            ev.page.map(|p| format!("p.{p}")).unwrap_or_else(|| "页码未知".into())
        };
        let body: String = ev.content.chars().take(remaining.min(1200)).collect();
        remaining = remaining.saturating_sub(body.chars().count());
        out.push(format!(
            "[{}]（文献 {}《{}》，{}）\n{}",
            ev.id, source.source.paper_index, source.source.title, location, body
        ));
    }
    out.join("\n\n")
}

async fn write_section_with_llm(
    app: Option<&AppHandle>,
    db: &SqlitePool,
    llm: &dyn LlmClient,
    run_id: &str,
    section_index: usize,
    target_words: usize,
    attempts: u32,
    max_tokens: u32,
) -> Result<String, String> {
    let state = review_store::load_state(db, run_id).await?;
    let section = state.run.outline.get(section_index).cloned().ok_or_else(|| "节号超出大纲范围".to_string())?;
    let evidence = section_evidence(db, &state, &section).await?;
    let evidence_text = evidence_block(&evidence, &state);
    let points = if section.points.is_empty() { String::new() } else { format!("\n本节要点：{}", section.points) };
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
            ("language", &state.run.language),
            ("target_words", &target_words.to_string()),
            ("subsection_rule", &subsection_rule),
        ],
    );
    let user = format!(
        "研究问题：{}\n本节主题：{}{}{}\n\n跨文献综合：\n{}\n\n参考证据：\n{}",
        state.run.question,
        section.title,
        points,
        subsection_line,
        serde_json::to_string(&state.run.synthesis).map_err(|e| e.to_string())?,
        evidence_text
    );
    let (text, finish) = stream_completion(
        app,
        llm,
        &[chat_msg("system", system), chat_msg("user", user)],
        run_id,
        "section",
        section_index + 1,
        state.run.outline.len(),
        attempts.saturating_sub(1),
    )
    .await?;
    let body = text.trim();
    if body.is_empty() {
        return Err(match finish.as_deref() {
            Some("length") => format!("模型的思考占满了输出额度（max_tokens={max_tokens}），未产生正文；单节重试会提高额度"),
            other => format!("模型未产生正文（finish_reason={}，max_tokens={max_tokens}）", other.unwrap_or("未知")),
        });
    }
    let produced = body.chars().count();
    let mut final_body = body.to_string();
    if produced < target_words * 6 / 10 {
        match expand_section(llm, &section, body, target_words).await {
            Ok(expanded) if expanded.chars().count() > produced => {
                if let Err(e) = validate_revision_text(body, &expanded) {
                    let warning = format!("第 {} 节自动补写未采用：{e}；已保留原始分节稿", section_index + 1);
                    review_store::append_warning(db, run_id, &warning).await?;
                } else {
                    final_body = expanded.trim().to_string();
                }
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(run_id, section_index, error = %e, "section expansion failed"),
        }
    }
    Ok(final_body.trim().to_string())
}

async fn expand_section(
    llm: &dyn LlmClient,
    section: &ReviewSection,
    draft: &str,
    target_words: usize,
) -> Result<String, String> {
    let system = format!(
        "你是学术写作助手。下面这一节的草稿偏短，请在不改变论点、标题结构和既有引用标记的前提下把它写得更充实，使篇幅达到约 {target_words} 字的密度。必须按原顺序原样保留所有 [E数字] 引用标记，不得删除、复制、重排或新增；只输出改写后的本节正文。"
    );
    let response = llm
        .chat_completion(
            &[
                chat_msg("system", system),
                chat_msg("user", format!("本节主题：{}\n\n【草稿】\n{draft}", section.title)),
            ],
            &[],
        )
        .await?;
    if response.content.trim().is_empty() {
        Err("补写未返回内容".into())
    } else {
        Ok(response.content)
    }
}

async fn finalize_run(
    app: Option<&AppHandle>,
    db: &SqlitePool,
    run_id: &str,
    injected_llm: Option<&dyn LlmClient>,
) -> Result<(), String> {
    let state = review_store::load_state(db, run_id).await?;
    let total = state.run.outline.len();
    let done = done_sections(&state);
    if total == 0 {
        return Err("大纲为空".to_string());
    }
    if done < total {
        let raw = assemble_raw(&state, true)?;
        let audit = compute_audit(&raw, &state);
        review_store::save_final(db, run_id, &raw, &audit, "partial", "section").await?;
        let mut progress = base_progress(run_id, "partial");
        progress.total_sections = total;
        progress.done_sections = Some(done);
        progress.message = Some(format!("{done}/{total} 节已生成；可单节重试或保存已完成部分"));
        emit(app, &progress);
        return Ok(());
    }

    let original = assemble_raw(&state, false)?;
    review_store::set_run_status(db, run_id, "revising", "revision", None).await?;
    emit(app, &base_progress(run_id, "revision"));
    let mut owned_llm: Option<Box<dyn LlmClient>> = None;
    let mut llm_error = None;
    if injected_llm.is_none() {
        match default_llm(db, state.run.provider_id.as_deref(), 5000).await {
            Ok(client) => owned_llm = Some(client),
            Err(e) => llm_error = Some(e),
        }
    }
    let llm = injected_llm.or(owned_llm.as_deref());
    let prompt = crate::core::task_prompts::effective(db, crate::core::task_prompts::REVIEW_REVISION).await;
    let user = format!(
        "研究问题：{}\n范围限定：{}\n写作语言：{}\n\n跨文献综合：\n{}\n\n完整原始稿：\n{}",
        state.run.question,
        state.run.scope,
        state.run.language,
        serde_json::to_string_pretty(&state.run.synthesis).map_err(|e| e.to_string())?,
        original
    );
    let revised = if let Some(llm) = llm {
        stream_completion(
            app,
            llm,
            &[chat_msg("system", prompt), chat_msg("user", user)],
            run_id,
            "revision",
            0,
            total,
            0,
        )
        .await
    } else {
        Err(llm_error.unwrap_or_else(|| "修订模型不可用".to_string()))
    };
    let mut final_raw = original.clone();
    let mut status = "done";
    match revised {
        Ok((text, _)) => match validate_revision_text(&original, text.trim()) {
            Ok(()) => final_raw = text.trim().to_string(),
            Err(e) => {
                let warning = format!("全文修订未采用：{e}；已保留修订前原始稿");
                review_store::append_warning(db, run_id, &warning).await?;
            }
        },
        Err(e) if e == CANCELLED => {
            let warning = "全文修订已取消；已保留修订前原始稿".to_string();
            review_store::append_warning(db, run_id, &warning).await?;
            status = "cancelled";
        }
        Err(e) => {
            let warning = format!("全文修订未完成：{e}；已保留修订前原始稿");
            review_store::append_warning(db, run_id, &warning).await?;
        }
    }
    let latest = review_store::load_state(db, run_id).await?;
    let audit = compute_audit(&final_raw, &latest);
    review_store::save_final(db, run_id, &final_raw, &audit, status, "done").await?;
    let mut progress = base_progress(run_id, if status == "done" { "done" } else { "cancelled" });
    progress.total_sections = total;
    progress.done_sections = Some(total);
    emit(app, &progress);
    Ok(())
}

fn assemble_raw(state: &ReviewRunState, allow_partial: bool) -> Result<String, String> {
    let done = done_sections(state);
    if done == 0 {
        return Err("还没有任何一节生成成功".to_string());
    }
    if !allow_partial && done != state.run.outline.len() {
        return Err("还有小节未生成".to_string());
    }
    let title = format!("综述：{}", state.run.question.chars().take(40).collect::<String>());
    let mut markdown = format!(
        "# {title}\n\n> 研究问题：{}\n> 基于 {} 篇选定文献生成；正文仅保留可追溯证据引用。\n",
        state.run.question,
        state.sources.len()
    );
    let missing: Vec<String> = state
        .run
        .outline
        .iter()
        .enumerate()
        .filter(|(i, _)| {
            state.sections.get(*i).map(|s| s.status != "done" || s.raw_markdown.trim().is_empty()).unwrap_or(true)
        })
        .map(|(i, s)| format!("{}.{}", i + 1, s.title))
        .collect();
    if !missing.is_empty() {
        markdown.push_str(&format!("> 注意：本综述有 {} 节尚未完成：{}。\n", missing.len(), missing.join("、")));
    }
    markdown.push('\n');
    for (index, section) in state.run.outline.iter().enumerate() {
        markdown.push_str(&format!("## {}\n\n", section.title));
        match state.sections.get(index) {
            Some(result) if result.status == "done" && !result.raw_markdown.trim().is_empty() => {
                markdown.push_str(result.raw_markdown.trim());
                markdown.push_str("\n\n");
            }
            _ => markdown.push_str("> 本节尚未生成成功，未包含在本次综述中。\n\n"),
        }
    }
    Ok(markdown.trim().to_string())
}

#[derive(Debug, Clone)]
struct CitationGroup {
    start: usize,
    end: usize,
    raw: String,
    ids: Vec<String>,
    well_formed: bool,
}

fn parse_evidence_ids(inner: &str) -> Option<Vec<String>> {
    let mut ids = Vec::new();
    let mut token = String::new();
    let flush = |token: &mut String, ids: &mut Vec<String>| -> bool {
        let value = token.trim();
        if value.is_empty() {
            return true;
        }
        let Some(digits) = value.strip_prefix(['E', 'e']) else { return false };
        if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
            return false;
        }
        let Ok(number) = digits.parse::<usize>() else { return false };
        ids.push(format!("E{number}"));
        token.clear();
        true
    };
    for c in inner.chars().chain(std::iter::once(',')) {
        if c.is_whitespace() || matches!(c, ',' | '，' | '、' | ';' | '；') {
            if !flush(&mut token, &mut ids) {
                return None;
            }
        } else {
            token.push(c);
        }
    }
    if ids.is_empty() { None } else { Some(ids) }
}

fn citation_groups(text: &str) -> Vec<CitationGroup> {
    let chars: Vec<char> = text.chars().collect();
    let mut groups = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] != '[' {
            i += 1;
            continue;
        }
        let Some(relative_end) = chars[i + 1..].iter().position(|c| *c == ']') else {
            let inner: String = chars[i + 1..].iter().collect();
            if inner.trim_start().starts_with(['E', 'e']) {
                groups.push(CitationGroup {
                    start: i,
                    end: chars.len(),
                    raw: chars[i..].iter().collect(),
                    ids: Vec::new(),
                    well_formed: false,
                });
            }
            break;
        };
        let end = i + 1 + relative_end;
        let inner: String = chars[i + 1..end].iter().collect();
        if inner.trim_start().starts_with(['E', 'e']) {
            let parsed = parse_evidence_ids(&inner);
            groups.push(CitationGroup {
                start: i,
                end: end + 1,
                raw: chars[i..=end].iter().collect(),
                well_formed: parsed.is_some(),
                ids: parsed.unwrap_or_default(),
            });
        }
        i = end + 1;
    }
    merge_adjacent_groups(&mut groups);
    groups
}

/// 提示词让模型把同处的多条证据写成 [E3][E7]（独立括号），模型自己偶尔也写
/// [E3,E7]（单括号逗号）。两种写法语义相同，渲染必须统一成 [3,7]——把零间隙
/// 相邻的良构组合并成一个组。仅合并良构组：坏标记保持独立，审计照常报告。
fn merge_adjacent_groups(groups: &mut Vec<CitationGroup>) {
    let mut merged: Vec<CitationGroup> = Vec::with_capacity(groups.len());
    for group in groups.drain(..) {
        if let Some(prev) = merged.last_mut() {
            if prev.well_formed && group.well_formed && prev.end == group.start {
                prev.end = group.end;
                prev.raw.push_str(&group.raw);
                prev.ids.extend(group.ids);
                continue;
            }
        }
        merged.push(group);
    }
    *groups = merged;
}

fn citation_sequence(text: &str) -> Vec<String> {
    citation_groups(text)
        .into_iter()
        .flat_map(|group| {
            if group.well_formed {
                group.ids
            } else {
                vec![format!("!{}", group.raw)]
            }
        })
        .collect()
}

fn heading_structure(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let trimmed = line.trim_start();
            let hashes = trimmed.chars().take_while(|c| *c == '#').count();
            if hashes == 0 || hashes > 6 || !trimmed.chars().nth(hashes).is_some_and(|c| c.is_whitespace()) {
                return None;
            }
            Some(trimmed.trim_end().to_string())
        })
        .collect()
}

fn validate_revision_text(original: &str, revised: &str) -> Result<(), String> {
    if revised.trim().is_empty() {
        return Err("修订稿为空".into());
    }
    let before = citation_sequence(original);
    let after = citation_sequence(revised);
    if before != after {
        return Err("引用标记被删除、复制、重排或新增".into());
    }
    if heading_structure(original) != heading_structure(revised) {
        return Err("章节标题或层级结构发生变化".into());
    }
    Ok(())
}

fn compute_audit(raw: &str, state: &ReviewRunState) -> ReviewAudit {
    let registry: HashMap<&str, &RegisteredEvidence> = state.run.evidence_registry.iter().map(|e| (e.id.as_str(), e)).collect();
    let mut cited = Vec::new();
    let mut invalid = Vec::new();
    for group in citation_groups(raw) {
        if !group.well_formed {
            if !invalid.contains(&group.raw) {
                invalid.push(group.raw.clone());
            }
            continue;
        }
        for id in group.ids {
            if let Some(ev) = registry.get(id.as_str()) {
                if !cited.contains(&ev.paper_id) {
                    cited.push(ev.paper_id.clone());
                }
            } else {
                let marker = format!("[{id}]");
                if !invalid.contains(&marker) {
                    invalid.push(marker);
                }
            }
        }
    }
    let mut warnings = state.run.warnings.clone();
    for source in &state.sources {
        let title = if source.source.title.is_empty() { &source.source.paper_id } else { &source.source.title };
        match source.source.source_kind.as_str() {
            "abstract" => warnings.push(format!("《{title}》仅使用摘要级证据，具体方法、数据和结论需复核全文")),
            "unusable" => warnings.push(format!("《{title}》没有可用全文或摘要，未参与证据生成")),
            _ => {}
        }
        if source.source.status == "failed" {
            warnings.push(format!("《{title}》阅读失败：{}", source.source.error.as_deref().unwrap_or("未知错误")));
        }
        if source.source.card.is_some() && source.source.status == "done" {
            // Successful cards need no additional warning beyond their source level.
        }
    }
    let uncited: Vec<String> = state
        .sources
        .iter()
        .filter(|s| !cited.contains(&s.source.paper_id))
        .map(|s| s.source.paper_id.clone())
        .collect();
    if !uncited.is_empty() {
        warnings.push(format!("有 {} 篇选定文献未在正文实际引用，已列入覆盖报告而非参考文献", uncited.len()));
    }
    if !invalid.is_empty() {
        warnings.push(format!("发现 {} 个无效引用标记，保存时不会生成链接或参考文献", invalid.len()));
    }
    if cited.is_empty() {
        warnings.push("正文没有任何有效证据引用".into());
    }
    let mut seen = HashSet::new();
    warnings.retain(|warning| seen.insert(warning.clone()));
    ReviewAudit {
        selected_count: state.sources.len(),
        fulltext_count: state.sources.iter().filter(|s| s.source.source_kind == "fulltext").count(),
        abstract_count: state.sources.iter().filter(|s| s.source.source_kind == "abstract").count(),
        unusable_count: state.sources.iter().filter(|s| s.source.source_kind == "unusable").count(),
        cited_paper_ids: cited,
        uncited_paper_ids: uncited,
        invalid_markers: invalid,
        warnings,
    }
}

pub async fn save_review(app: &AppHandle, db: &SqlitePool, run_id: &str) -> Result<String, String> {
    save_review_inner(Some(app), db, run_id).await
}

async fn save_review_inner(app: Option<&AppHandle>, db: &SqlitePool, run_id: &str) -> Result<String, String> {
    if is_active(run_id) {
        return Err("任务运行中不能保存；请先等待完成或取消".to_string());
    }
    let mut state = review_store::load_state(db, run_id).await?;
    let raw = match state.run.final_raw_markdown.clone() {
        Some(raw) => raw,
        None => assemble_raw(&state, true)?,
    };
    let audit = compute_audit(&raw, &state);
    let status = if done_sections(&state) == state.run.outline.len() { "done" } else { "partial" };
    review_store::save_final(db, run_id, &raw, &audit, status, "done").await?;
    state = review_store::load_state(db, run_id).await?;
    emit(app, &base_progress(run_id, "assemble"));
    let markdown = render_note_markdown(&raw, &state, &audit);
    let note_title = format!("综述：{}", state.run.question.chars().take(40).collect::<String>());
    let note_id = match state.run.note_id.clone() {
        Some(id) => {
            crate::core::note_service::update_note(db, &id, None, Some(&markdown), None, None, None, Some(true))
                .await
                .map(|_| id)
                .map_err(|e| format!("更新笔记失败：{e}"))?
        }
        None => {
            let vault_id = crate::core::vault_service::get_current_vault_id(db).await?;
            let note = crate::core::note_service::create_note(db, &note_title, &markdown, None, None, &vault_id, false).await?;
            note.id
        }
    };
    review_store::set_note_id(db, run_id, &note_id).await?;
    let mut progress = base_progress(run_id, "done");
    progress.note_id = Some(note_id.clone());
    progress.done_sections = Some(done_sections(&state));
    progress.total_sections = state.run.outline.len();
    emit(app, &progress);
    Ok(note_id)
}

fn render_note_markdown(raw: &str, state: &ReviewRunState, audit: &ReviewAudit) -> String {
    let mut markdown = linkify_citations(raw, &state.run.evidence_registry);
    markdown.push_str("\n\n## 参考文献\n\n");
    let mut cited = audit.cited_paper_ids.clone();
    cited.sort_by_key(|id| state.source(id).map(|s| s.source.paper_index).unwrap_or(usize::MAX));
    for paper_id in cited {
        if let Some(source) = state.source(&paper_id) {
            markdown.push_str(&reference_line(source));
            markdown.push('\n');
        }
    }
    markdown
}

fn reference_line(source: &StoredSource) -> String {
    bibliography_line(
        source.source.paper_index,
        &source.source.authors,
        &source.source.title,
        source.journal.as_deref(),
        source.source.year,
        source.doi.as_deref(),
        source.url.as_deref(),
    )
}

fn linkify_citations(text: &str, registry: &[RegisteredEvidence]) -> String {
    let chars: Vec<char> = text.chars().collect();
    let groups = citation_groups(text);
    let evidence: HashMap<&str, &RegisteredEvidence> = registry.iter().map(|e| (e.id.as_str(), e)).collect();
    let mut output = String::with_capacity(text.len() + 128);
    let mut cursor = 0usize;
    for group in groups {
        if !group.well_formed {
            continue;
        }
        output.extend(chars[cursor..group.start].iter());
        let hint = citing_sentence(&chars, group.start);
        let mut items: Vec<(usize, Option<String>)> = Vec::new();
        for id in &group.ids {
            let Some(ev) = evidence.get(id.as_str()) else { continue };
            if items.iter().any(|(paper, _)| *paper == ev.paper_index) {
                continue;
            }
            let url = if ev.source_type == "fulltext" && ev.chunk_id.is_some() {
                let quote = anchor_for(&ev.content, &ev.quote, &hint);
                Some(reader_url(&ev.paper_id, ev.page, &quote))
            } else {
                None
            };
            items.push((ev.paper_index, url));
        }
        if items.len() == 1 {
            let (paper, url) = &items[0];
            match url {
                Some(url) => output.push_str(&format!("[[{paper}]]({url})")),
                None => output.push_str(&format!("[{paper}]")),
            }
        } else if !items.is_empty() {
            let inner = items
                .iter()
                .map(|(paper, url)| match url {
                    Some(url) => format!("[{paper}]({url})"),
                    None => paper.to_string(),
                })
                .collect::<Vec<_>>()
                .join(",");
            output.push_str(&format!("[{inner}]"));
        }
        cursor = group.end;
    }
    output.extend(chars[cursor..].iter());
    output
}

fn reader_url(paper_id: &str, page: Option<i32>, quote: &str) -> String {
    let exact = percent_encode(quote);
    match page {
        Some(page) => format!("siku-reader://{paper_id}?page={page}&exact={exact}"),
        None => format!("siku-reader://{paper_id}?exact={exact}"),
    }
}

fn citing_sentence(chars: &[char], at: usize) -> String {
    const CAP: usize = 240;
    const MIN: usize = 8;
    let mut start = at;
    let mut taken = 0usize;
    while start > 0 && taken < CAP {
        start -= 1;
        taken += 1;
        if is_sentence_break(chars[start]) && at - start >= MIN {
            start += 1;
            break;
        }
    }
    chars[start..at].iter().collect::<String>().trim().to_string()
}

fn anchor_quote_capped(content: &str, cap: usize) -> String {
    let flat = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut head: String = flat.chars().take(cap).collect();
    if flat.chars().count() > cap {
        if let Some(index) = head.rfind(' ') {
            if index >= 40 {
                head.truncate(index);
            }
        }
    }
    head.trim().to_string()
}

fn is_sentence_break(c: char) -> bool {
    matches!(c, '。' | '！' | '？' | '；' | '!' | '?' | ';' | '\n')
}

fn split_sentences(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut current = String::new();
    for (index, c) in chars.iter().enumerate() {
        current.push(*c);
        let dot_break = *c == '.' && chars.get(index + 1).map(|next| next.is_whitespace()).unwrap_or(true);
        if is_sentence_break(*c) || dot_break {
            let value = current.trim();
            if !value.is_empty() {
                out.push(value.to_string());
            }
            current.clear();
        }
    }
    let value = current.trim();
    if !value.is_empty() {
        out.push(value.to_string());
    }
    out
}

fn overlap_score(sentence: &str, hint: &str) -> usize {
    if hint.trim().is_empty() {
        return 0;
    }
    let lower = sentence.to_lowercase();
    let mut score = 0usize;
    let hint_chars: Vec<char> = hint.chars().collect();
    for pair in hint_chars.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if a.is_ascii() || b.is_ascii() || !a.is_alphanumeric() || !b.is_alphanumeric() {
            continue;
        }
        let bigram: String = [a, b].iter().collect();
        if lower.contains(&bigram) {
            score += 1;
        }
    }
    let mut word = String::new();
    let flush = |word: &mut String, score: &mut usize| {
        if word.chars().count() >= 3 && lower.contains(word.as_str()) {
            *score += 2;
        }
        word.clear();
    };
    for c in hint.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_alphanumeric() {
            word.push(c.to_ascii_lowercase());
        } else {
            flush(&mut word, &mut score);
        }
    }
    score
}

fn anchor_for(content: &str, fallback: &str, hint: &str) -> String {
    let mut best: Option<(usize, String)> = None;
    for sentence in split_sentences(content) {
        if sentence.chars().count() < 12 {
            continue;
        }
        let score = overlap_score(&sentence, hint);
        if score > 0 && best.as_ref().map(|(best, _)| score > *best).unwrap_or(true) {
            best = Some((score, sentence));
        }
    }
    match best {
        Some((_, sentence)) => anchor_quote_capped(&sentence, 120),
        None => fallback.to_string(),
    }
}

pub(crate) fn percent_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(*byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

pub(crate) fn bibliography_line(
    n: usize,
    authors_json: &str,
    title: &str,
    journal: Option<&str>,
    year: Option<i64>,
    doi: Option<&str>,
    url: Option<&str>,
) -> String {
    format!("[{n}] {}", bibliography_entry(authors_json, title, journal, year, doi, url))
}

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
        (Some(journal), Some(year)) if !journal.is_empty() => line.push_str(&format!(" {journal}, {year}.")),
        (Some(journal), None) if !journal.is_empty() => line.push_str(&format!(" {journal}.")),
        (None, Some(year)) => line.push_str(&format!(" {year}.")),
        _ => {}
    }
    if let Some(doi) = doi.filter(|value| !value.is_empty()) {
        line.push_str(&format!(" DOI: {doi}."));
    } else if let Some(url) = url.filter(|value| !value.is_empty()) {
        line.push_str(&format!(" {url}"));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::review_store::{ReviewEvidence, ReviewSource, SourceChunk};

    fn registered(id: &str, paper_id: &str, paper_index: usize, source_type: &str) -> RegisteredEvidence {
        RegisteredEvidence {
            id: id.into(),
            paper_id: paper_id.into(),
            paper_index,
            source_type: source_type.into(),
            chunk_id: (source_type == "fulltext").then(|| format!("chunk-{paper_id}")),
            page: (source_type == "fulltext").then_some(3),
            quote: "真实证据原文".into(),
            content: "这是真实证据原文。该方法在 42 个样本上把准确率提升到 91.3%。".into(),
        }
    }

    fn stored(paper_id: &str, index: usize, kind: &str) -> StoredSource {
        StoredSource {
            source: ReviewSource {
                paper_id: paper_id.into(),
                paper_index: index,
                title: format!("文献 {paper_id}"),
                authors: "[]".into(),
                year: Some(2024),
                source_kind: kind.into(),
                status: if kind == "unusable" { "done".into() } else { "failed".into() },
                chunk_count: usize::from(kind == "fulltext"),
                abstract_chars: usize::from(kind == "abstract") * 100,
                card: None,
                error: (kind != "unusable").then(|| "测试阅读失败".into()),
            },
            journal: None,
            doi: None,
            url: None,
            abstract_text: "摘要".into(),
            chunks: vec![SourceChunk {
                id: format!("chunk-{paper_id}"),
                chunk_index: 0,
                content: "真实证据".into(),
                page_start: Some(3),
                page_end: Some(3),
            }],
            cache_key: String::new(),
            index_fingerprint: String::new(),
        }
    }

    fn state_for_audit() -> ReviewRunState {
        ReviewRunState {
            run: review_store::ReviewRun {
                id: "r".into(),
                status: "done".into(),
                stage: "done".into(),
                question: "q".into(),
                scope: String::new(),
                language: "中文".into(),
                target_words: 1000,
                provider_id: None,
                paper_ids: vec!["p1".into(), "p2".into(), "p3".into()],
                synthesis: None,
                outline: vec![],
                evidence_registry: vec![registered("E1", "p1", 1, "fulltext")],
                final_raw_markdown: None,
                audit: None,
                warnings: vec!["修订已跳过".into()],
                note_id: None,
                error: None,
            },
            sources: vec![stored("p1", 1, "fulltext"), stored("p2", 2, "abstract"), stored("p3", 3, "unusable")],
            sections: vec![],
        }
    }

    #[test]
    fn revision_rejects_every_citation_sequence_change() {
        let original = "# 标题\n\n## 一节\n\n甲[E1]乙[E2]。";
        assert!(validate_revision_text(original, "# 标题\n\n## 一节\n\n改写甲[E1]乙[E2]。").is_ok());
        for revised in [
            "# 标题\n\n## 一节\n\n甲乙[E2]。",
            "# 标题\n\n## 一节\n\n甲[E1][E1]乙[E2]。",
            "# 标题\n\n## 一节\n\n甲[E2]乙[E1]。",
            "# 标题\n\n## 一节\n\n甲[E1]乙[E2]丙[E3]。",
        ] {
            assert!(validate_revision_text(original, revised).is_err(), "accepted {revised}");
        }
    }

    #[test]
    fn revision_rejects_section_structure_changes() {
        let original = "# 标题\n\n## 一节\n\n### 小节\n正文[E1]";
        let revised = "# 标题\n\n## 改名\n\n### 小节\n正文[E1]";
        assert!(validate_revision_text(original, revised).is_err());
    }

    #[test]
    fn audit_reports_coverage_invalid_markers_and_actual_citations() {
        let state = state_for_audit();
        let audit = compute_audit("结论[E1]，幻觉[E99]，坏标记[E]，残缺[E8", &state);
        assert_eq!(audit.selected_count, 3);
        assert_eq!(audit.fulltext_count, 1);
        assert_eq!(audit.abstract_count, 1);
        assert_eq!(audit.unusable_count, 1);
        assert_eq!(audit.cited_paper_ids, vec!["p1"]);
        assert_eq!(audit.uncited_paper_ids, vec!["p2", "p3"]);
        assert!(audit.invalid_markers.contains(&"[E99]".to_string()));
        assert!(audit.invalid_markers.contains(&"[E]".to_string()));
        assert!(audit.invalid_markers.contains(&"[E8".to_string()));
        assert!(audit.warnings.iter().any(|w| w.contains("摘要级")));
        assert!(audit.warnings.iter().any(|w| w.contains("修订已跳过")));
    }

    #[test]
    fn abstract_citation_is_plain_and_fulltext_citation_is_deep_linked() {
        let registry = vec![registered("E1", "p1", 1, "fulltext"), registered("E2", "p2", 2, "abstract")];
        let output = linkify_citations("全文[E1]，摘要[E2]，未知[E9]。", &registry);
        assert!(output.contains("siku-reader://p1?page=3&exact="), "{output}");
        assert!(output.contains("摘要[2]"), "{output}");
        assert!(!output.contains("siku-reader://p2"), "{output}");
        assert!(!output.contains("E9"), "{output}");
    }

    #[test]
    fn adjacent_citation_brackets_render_merged() {
        let registry = vec![registered("E1", "p1", 1, "fulltext"), registered("E2", "p2", 2, "fulltext")];
        // 提示词钦定的 [E1][E2] 独立括号写法必须与 [E1,E2] 一样合并成 [1,2]。
        let output = linkify_citations("结论[E1][E2]成立。", &registry);
        assert!(output.contains("[[1](siku-reader://p1"), "{output}");
        assert!(output.contains(",[2](siku-reader://p2"), "{output}");
        assert!(!output.contains("]]"), "{output}");
        // 有文字隔开的组不合并。
        let output = linkify_citations("甲[E1]与乙[E2]。", &registry);
        assert!(output.contains("[[1]](siku-reader://p1"), "{output}");
        assert!(output.contains("[[2]](siku-reader://p2"), "{output}");
        // 相邻但引同一文献：去重为单个编号。
        let same = vec![registered("E1", "p1", 1, "fulltext"), registered("E3", "p1", 1, "fulltext")];
        let output = linkify_citations("结论[E1][E3]。", &same);
        assert_eq!(output.matches("[[1]]").count(), 1, "{output}");
    }

    #[test]
    fn anchor_prefers_the_cited_sentence() {
        let content = "我们收集了 42 个开源项目。该方法把准确率提升到 91.3%，显著优于基线。最后讨论效度威胁。";
        let anchor = anchor_for(content, "fallback", "方法准确率提升到 91.3%");
        assert!(anchor.contains("91.3%"), "{anchor}");
        assert_eq!(anchor_quote_capped("  spaced   out  ", 90), "spaced out");
    }

    #[test]
    fn parse_outline_keeps_paper_ids_and_rejects_unknown_ids() {
        let allowed = HashSet::from(["p1".to_string()]);
        let outline = parse_outline(
            "```json\n[{\"title\":\"引言\",\"points\":\"背景\",\"paperIds\":[\"p1\"],\"subsections\":[]}]\n```",
            &allowed,
        )
        .unwrap();
        assert_eq!(outline[0].paper_ids, vec!["p1"]);
        assert!(parse_outline("[{\"title\":\"坏\",\"paperIds\":[\"other\"]}]", &allowed).is_err());
    }

    #[test]
    fn evidence_ids_default_empty_when_old_outline_json_is_read() {
        let section: ReviewSection = serde_json::from_str("{\"title\":\"旧大纲\"}").unwrap();
        assert!(section.paper_ids.is_empty());
        let _: Option<ReviewEvidence> = None;
    }

    #[derive(Default)]
    struct ScriptedLlm {
        card_calls: std::sync::atomic::AtomicUsize,
    }

    impl ScriptedLlm {
        fn reply(&self, messages: &[ChatMessage]) -> String {
            let system = &messages[0].content;
            let user = &messages[1].content;
            if system.contains("候选原文") {
                self.card_calls.fetch_add(1, Ordering::SeqCst);
                return serde_json::json!({
                    "paperId": "model-value-is-overridden",
                    "researchQuestion": " scripted 研究问题",
                    "methods": "scripted 方法",
                    "dataAndSample": "scripted 样本",
                    "findings": [{"claim": "有证据的发现", "evidenceIds": ["E1"]}],
                    "limitations": "scripted 局限",
                    "relevance": "直接相关",
                    "evidence": [{"id": "E1", "chunkId": null, "page": null, "quote": "", "content": ""}],
                    "sourceType": "fulltext",
                    "edited": false
                })
                .to_string();
            }
            if system.contains("跨文献主题") {
                return serde_json::json!({
                    "themes": [{"title": "共同主题", "detail": "两种来源的综合", "paperIds": ["p-full", "p-abstract"]}],
                    "agreements": [],
                    "conflicts": [],
                    "gaps": [{"title": "全文缺口", "detail": "摘要来源需要全文复核", "paperIds": ["p-abstract"]}]
                })
                .to_string();
            }
            if system.contains("设计文献综述目录") {
                return serde_json::json!([
                    {"title": "全文证据", "points": "讨论全文发现", "subsections": [], "paperIds": ["p-full"]},
                    {"title": "摘要证据与边界", "points": "讨论摘要发现和限制", "subsections": [], "paperIds": ["p-abstract"]}
                ])
                .to_string();
            }
            if system.contains("正在撰写文献综述的一节") {
                let evidence = user.split("参考证据：\n").nth(1).unwrap();
                let start = evidence.find("[E").unwrap();
                let end = evidence[start..].find(']').unwrap() + start;
                let marker = &evidence[start + 1..end];
                return format!("{}[{}]", "基于真实证据的跨文献论述。".repeat(24), marker);
            }
            if system.contains("全文修订助手") {
                let original = user.split("完整原始稿：\n").nth(1).unwrap();
                return original.replace("基于真实证据", "经全文修订的真实证据");
            }
            if system.contains("草稿偏短") {
                return user.split("【草稿】\n").nth(1).unwrap().to_string();
            }
            panic!("unexpected scripted prompt: {system}");
        }

        fn response(&self, messages: &[ChatMessage]) -> crate::ai::llm::LlmResponse {
            crate::ai::llm::LlmResponse {
                content: self.reply(messages),
                tool_calls: None,
                tokens_in: 1,
                tokens_out: 1,
                model: "scripted".into(),
                finish_reason: Some("stop".into()),
            }
        }
    }

    #[async_trait::async_trait]
    impl LlmClient for ScriptedLlm {
        async fn chat_completion(
            &self,
            messages: &[ChatMessage],
            _tools: &[crate::ai::llm::ToolDefinition],
        ) -> Result<crate::ai::llm::LlmResponse, String> {
            Ok(self.response(messages))
        }

        async fn chat_completion_stream(
            &self,
            messages: &[ChatMessage],
            _tools: &[crate::ai::llm::ToolDefinition],
            sender: tokio::sync::mpsc::UnboundedSender<crate::ai::llm::StreamEvent>,
        ) -> Result<(), String> {
            sender
                .send(crate::ai::llm::StreamEvent {
                    event_type: "delta".into(),
                    content: Some(self.reply(messages)),
                    tool_call: None,
                    usage: None,
                })
                .map_err(|e| e.to_string())?;
            sender
                .send(crate::ai::llm::StreamEvent {
                    event_type: "finish".into(),
                    content: Some("stop".into()),
                    tool_call: None,
                    usage: None,
                })
                .map_err(|e| e.to_string())?;
            Ok(())
        }

        async fn chat_completion_vision(
            &self,
            _system: &str,
            _user: &str,
            _images: &[crate::ai::llm::ImagePart],
        ) -> Result<crate::ai::llm::LlmResponse, String> {
            Err("scripted vision is not used".into())
        }
    }

    #[tokio::test]
    async fn scripted_llm_runs_fulltext_abstract_unusable_pipeline_through_save() {
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::raw_sql(crate::core::db::SCHEMA_INIT_SQL).execute(&db).await.unwrap();
        sqlx::raw_sql(review_store::SCHEMA_SQL).execute(&db).await.unwrap();
        crate::core::vault_service::ensure_defaults(&db).await.unwrap();
        for (id, title, abstract_text) in [
            ("p-full", "全文文献", None),
            ("p-abstract", "摘要文献", Some("摘要中的研究问题、方法、样本、发现和局限。")),
            ("p-empty", "不可用文献", None),
        ] {
            sqlx::query("INSERT INTO papers (id, title, authors, abstract, year, created_at, updated_at) VALUES (?, ?, '[]', ?, 2024, 't', 't')")
                .bind(id)
                .bind(title)
                .bind(abstract_text)
                .execute(&db)
                .await
                .unwrap();
        }
        sqlx::query(
            "INSERT INTO chunks (id, paper_id, content, chunk_index, page_start, page_end, created_at)
             VALUES ('c-full', 'p-full', ?, 0, 2, 2, 't')",
        )
        .bind("全文中的方法、数据、样本和研究发现。".repeat(20))
        .execute(&db)
        .await
        .unwrap();

        let llm = ScriptedLlm::default();
        let snapshot = create(
            &db,
            &["p-full".to_string(), "p-abstract".to_string(), "p-empty".to_string()],
            "比较研究方法",
            "仅使用已选文献",
            "中文",
            600,
            None,
        )
        .await
        .unwrap();
        assert_eq!(snapshot.sources.iter().map(|s| s.source_kind.as_str()).collect::<Vec<_>>(), vec!["fulltext", "abstract", "unusable"]);
        run_reading_inner(None, &db, &snapshot.run_id, Some(&llm)).await.unwrap();
        assert_eq!(llm.card_calls.load(Ordering::SeqCst), 2);
        let read = get(&db, &snapshot.run_id).await.unwrap();
        assert!(read.synthesis.is_some());
        assert_eq!(read.sources[0].card.as_ref().unwrap().evidence[0].id, "E1");
        assert_eq!(read.sources[1].card.as_ref().unwrap().evidence[0].id, "E2");
        assert_eq!(read.sources[1].card.as_ref().unwrap().evidence[0].page, None);
        assert_eq!(read.sources[1].card.as_ref().unwrap().evidence[0].chunk_id, None);
        assert!(read.sources[0].card.as_ref().unwrap().evidence[0].content.contains("全文中的方法"));

        let outline = generate_outline_with_llm(None, &db, &llm, &snapshot.run_id).await.unwrap();
        prepare_generation(&db, &snapshot.run_id, &outline).await.unwrap();
        run_generation_inner(None, &db, &snapshot.run_id, Some(&llm)).await.unwrap();
        let generated = get(&db, &snapshot.run_id).await.unwrap();
        assert_eq!(generated.status, "done");
        assert!(generated.sections.iter().all(|s| s.status == "done"));
        let raw = generated.final_raw_markdown.as_deref().unwrap();
        assert!(raw.contains("经全文修订的真实证据"), "{raw}");
        assert!(raw.contains("[E1]") && raw.contains("[E2]"), "{raw}");
        let audit = generated.audit.as_ref().unwrap();
        assert_eq!(audit.selected_count, 3);
        assert_eq!(audit.fulltext_count, 1);
        assert_eq!(audit.abstract_count, 1);
        assert_eq!(audit.unusable_count, 1);
        assert_eq!(audit.cited_paper_ids, vec!["p-full", "p-abstract"]);
        assert_eq!(audit.uncited_paper_ids, vec!["p-empty"]);
        assert!(audit.invalid_markers.is_empty());

        let note_id = save_review_inner(None, &db, &snapshot.run_id).await.unwrap();
        let content = sqlx::query_scalar::<_, String>("SELECT content FROM notes WHERE id = ?")
            .bind(&note_id)
            .fetch_one(&db)
            .await
            .unwrap();
        let references = content.split("## 参考文献").nth(1).unwrap();
        assert!(references.contains("全文文献"), "{references}");
        assert!(references.contains("摘要文献"), "{references}");
        assert!(!references.contains("不可用文献"), "{references}");
        assert!(content.contains("siku-reader://p-full?page=2&exact="), "{content}");
        assert!(!content.contains("siku-reader://p-abstract"), "{content}");
        assert_eq!(get(&db, &snapshot.run_id).await.unwrap().note_id.as_deref(), Some(note_id.as_str()));

        let cached_run = create(
            &db,
            &["p-full".to_string(), "p-abstract".to_string(), "p-empty".to_string()],
            "比较研究方法",
            "仅使用已选文献",
            "English",
            900,
            None,
        )
        .await
        .unwrap();
        run_reading_inner(None, &db, &cached_run.run_id, Some(&llm)).await.unwrap();
        assert_eq!(llm.card_calls.load(Ordering::SeqCst), 2, "相同问题/范围/索引/prompt 应复用卡片，语言和字数不参与阅读缓存");
        let cached = get(&db, &cached_run.run_id).await.unwrap();
        assert!(cached.synthesis.is_some());
        assert!(cached.sources.iter().take(2).all(|s| s.card.is_some()));
    }
}
