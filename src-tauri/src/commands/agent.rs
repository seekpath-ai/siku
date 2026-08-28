use std::path::PathBuf;
use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager, State};
use tracing::{error, info, instrument, warn};

use crate::AppState;
use crate::ai::agent::config::{AgentConfig, ApprovalConfig, LlmConfigBlock};
use crate::ai::agent::engine::{AgentEngine, AgentEvent, ApprovalResponse};
use crate::ai::agent::memory_store::MemoryStore;
use crate::ai::agent::tool_registry::ToolRegistry;
use crate::ai::llm::{self, ChatMessage};
use crate::commands::chat;
use crate::core::models::{AgentSessionInput, AgentStep, ChatSession};
use crate::core::settings_service;
use crate::core::time;

fn now_iso() -> String {
    time::now_iso()
}

async fn ensure_agent_config(
    config: &mut AgentConfig,
    app_settings: &crate::core::models::AppSettings,
    db: &sqlx::SqlitePool,
) -> Result<(), String> {
    if config.llm_models.is_empty() {
        let mut block: Option<LlmConfigBlock> = None;
        if let Some(provider_id) = &app_settings.default_llm_provider_id {
            // The referenced provider may have been deleted since; fall back
            // to the active provider pool instead of failing the whole request.
            block = crate::core::llm_provider_service::resolve_block(db, provider_id)
                .await
                .ok();
        }
        if block.is_none() {
            block = crate::core::llm_provider_service::get_default_provider(db)
                .await
                .ok()
                .flatten()
                .map(|p| crate::core::llm_provider_service::provider_to_block(&p));
        }
        if block.is_none() {
            block = app_settings.default_llm.clone();
        }
        config.llm_models.push(block.unwrap_or_else(LlmConfigBlock::default));
    }
    if config.max_loops.is_none() {
        config.max_loops = Some(app_settings.default_max_loops);
    }
    // max_tokens (per-round output cap) stays None = follow the model config;
    // the global default applies to the context budget instead.
    if config.context_budget.is_none() {
        config.context_budget = Some(app_settings.default_max_tokens);
    }
    if config.max_memory_rounds.is_none() {
        config.max_memory_rounds = Some(app_settings.default_max_memory_rounds);
    }
    Ok(())
}

fn default_memory_dir(base: &std::path::Path) -> PathBuf {
    base.join("memory")
}

fn default_skills_dir(base: &std::path::Path) -> PathBuf {
    base.join("skills")
}

fn effective_base_dir<'a>(
    app_data_dir: &'a std::path::Path,
    data_dir: Option<&'a std::path::Path>,
) -> &'a std::path::Path {
    data_dir.unwrap_or(app_data_dir)
}

fn agent_memory_path(
    app_data_dir: &std::path::Path,
    data_dir: Option<&std::path::Path>,
    session_memory_dir: Option<&str>,
    default_memory_dir_setting: Option<&str>,
    explicit_path: Option<&str>,
    session_id: &str,
) -> PathBuf {
    if let Some(p) = explicit_path {
        return PathBuf::from(p);
    }
    let base = session_memory_dir
        .map(PathBuf::from)
        .or_else(|| default_memory_dir_setting.map(PathBuf::from))
        .unwrap_or_else(|| default_memory_dir(effective_base_dir(app_data_dir, data_dir)));
    MemoryStore::default_path(&base, session_id)
}

fn agent_skills_dir(
    app_data_dir: &std::path::Path,
    data_dir: Option<&std::path::Path>,
    session_skills_dir: Option<&str>,
    default_skills_dir_setting: Option<&str>,
) -> PathBuf {
    session_skills_dir
        .map(PathBuf::from)
        .or_else(|| default_skills_dir_setting.map(PathBuf::from))
        .unwrap_or_else(|| default_skills_dir(effective_base_dir(app_data_dir, data_dir)))
}

/// Build an AgentConfig from a ChatSession row, filling defaults from app settings.
async fn build_agent_config(
    state: &AppState,
    session: &ChatSession,
) -> Result<AgentConfig, String> {
    let app_settings = settings_service::load_app_settings(&state.db).await?;

    // NULL/unparsable → None → all tools (legacy default); "[]" → explicitly none.
    let tools: Option<Vec<String>> = serde_json::from_str(&session.tools_enabled).unwrap_or(None);

    // Prefer provider pool references; fallback to legacy embedded llm_models.
    let mut llm_models: Vec<LlmConfigBlock> = Vec::new();
    if let Some(ids_json) = &session.llm_provider_ids {
        if let Ok(ids) = serde_json::from_str::<Vec<String>>(ids_json) {
            for id in ids {
                match crate::core::llm_provider_service::resolve_block(&state.db, &id).await {
                    Ok(block) => llm_models.push(block),
                    Err(e) => tracing::warn!(provider_id=%id, error=%e, "failed to resolve llm provider"),
                }
            }
        }
    }
    if llm_models.is_empty() {
        llm_models = session
            .llm_models
            .as_ref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
    }

    let approval: ApprovalConfig = match session
        .approval_config
        .as_ref()
        .and_then(|s| serde_json::from_str(s).ok())
    {
        Some(a) => a,
        None => {
            // Pet/domain sessions fall back to the pet-level `pet.approval`
            // setting (pet panel shield) before the global default, so the
            // shield never clobbers the new-agent template default.
            let pet = match session.domain.as_deref() {
                Some(_) => settings_service::get_setting(&state.db, "pet.approval")
                    .await
                    .ok()
                    .flatten()
                    .and_then(|s| serde_json::from_str(&s).ok()),
                None => None,
            };
            pet.unwrap_or_else(|| app_settings.default_approval.clone())
        }
    };

    let mut config = AgentConfig {
        display_name: session.title.clone(),
        persona: session.system_prompt.clone(),
        system_prompt: session.system_prompt.clone(),
        llm_models,
        tools,
        approval,
        max_loops: session.max_loops,
        max_tokens: session.max_tokens,
        context_budget: session.context_budget,
        max_memory_rounds: session.max_memory_rounds,
        memory_file_path: session.memory_file_path.clone(),
        memory_dir: session.memory_dir.clone(),
        skills_dir: session.skills_dir.clone(),
    };
    ensure_agent_config(&mut config, &app_settings, &state.db).await?;
    Ok(config)
}

/// Start a new agent session
#[tauri::command]
#[instrument(skip(state, input))]
pub async fn agent_create_session(
    state: State<'_, AppState>,
    input: AgentSessionInput,
) -> Result<serde_json::Value, String> {
    let app_settings = settings_service::load_app_settings(&state.db).await?;
    let mut config = AgentConfig {
        display_name: input.title.clone(),
        persona: input.system_prompt.clone(),
        system_prompt: input.system_prompt.clone(),
        llm_models: input.llm_models,
        tools: Some(input.tools_enabled.clone()),
        approval: input.approval_config.unwrap_or_else(|| app_settings.default_approval.clone()),
        max_loops: input.max_loops.or(Some(app_settings.default_max_loops)),
        max_tokens: input.max_tokens,
        context_budget: input.context_budget.or(Some(app_settings.default_max_tokens)),
        max_memory_rounds: input.max_memory_rounds.or(Some(app_settings.default_max_memory_rounds)),
        memory_file_path: None,
        memory_dir: input.memory_dir.clone(),
        skills_dir: input.skills_dir.clone(),
    };
    ensure_agent_config(&mut config, &app_settings, &state.db).await?;

    let id = uuid::Uuid::new_v4().to_string();
    let now = now_iso();
    let project_id = input.project_id.clone();
    let vision_provider_id = input.vision_provider_id.clone();
    let web_proxy = input.web_proxy.clone();
    // Default the sandbox root to the project directory when not specified.
    let mut working_dir = input.working_dir.clone();
    if working_dir.is_none() {
        if let Some(pid) = &input.project_id {
            working_dir = crate::core::project_service::get_path(&state.db, pid).await?;
        }
    }
    let llm_json = serde_json::to_string(&config.llm_models).map_err(|e| format!("json: {e}"))?;
    let provider_ids_json = serde_json::to_string(&input.llm_provider_ids).map_err(|e| format!("json: {e}"))?;
    let approval_json = serde_json::to_string(&config.approval).map_err(|e| format!("json: {e}"))?;
    let tools_json = serde_json::to_string(&config.tools).map_err(|e| format!("json: {e}"))?;

    sqlx::query(
        "INSERT INTO chat_sessions (
            id, title, mode, agent_mode, tools_enabled, system_prompt, project_id, working_dir, vision_provider_id, web_proxy,
            llm_models, llm_provider_ids, approval_config, max_loops, max_tokens, context_budget, max_memory_rounds,
            memory_file_path, memory_dir, skills_dir,
            is_pinned, sort_order, paper_ids, created_at, updated_at
        ) VALUES (?, ?, 'qa', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, 0, '[]', ?, ?)"
    )
    .bind(&id)
    .bind(&config.display_name)
    .bind(&input.agent_mode)
    .bind(&tools_json)
    .bind(&config.system_prompt)
    .bind(&project_id)
    .bind(&working_dir)
    .bind(&vision_provider_id)
    .bind(&web_proxy)
    .bind(&llm_json)
    .bind(&provider_ids_json)
    .bind(&approval_json)
    .bind(config.max_loops)
    .bind(config.max_tokens)
    .bind(config.context_budget)
    .bind(config.max_memory_rounds)
    .bind(&config.memory_file_path)
    .bind(&config.memory_dir)
    .bind(&config.skills_dir)
    .bind(&now)
    .bind(&now)
    .execute(&state.db)
    .await
    .map_err(|e| format!("db error: {e}"))?;

    Ok(serde_json::json!({
        "id": id,
        "title": config.display_name,
        "mode": "qa",
        "agent_mode": input.agent_mode,
        "project_id": project_id,
        "working_dir": working_dir,
        "vision_provider_id": vision_provider_id,
        "web_proxy": web_proxy,
        "tools_enabled": config.tools,
        "system_prompt": config.system_prompt,
        "llm_models": config.llm_models,
        "llm_provider_ids": input.llm_provider_ids,
        "approval_config": config.approval,
        "max_loops": config.max_loops,
        "max_tokens": config.max_tokens,
        "context_budget": config.context_budget,
        "max_memory_rounds": config.max_memory_rounds,
        "memory_file_path": config.memory_file_path,
        "memory_dir": config.memory_dir,
        "skills_dir": config.skills_dir,
        "is_pinned": false,
        "sort_order": 0,
        "icon": null,
        "color": null,
        "paper_ids": "[]",
        "created_at": now,
        "updated_at": now,
    }))
}

/// Update an agent session configuration
#[tauri::command]
#[instrument(skip(state, input))]
pub async fn agent_update_session(
    state: State<'_, AppState>,
    session_id: String,
    input: AgentSessionInput,
) -> Result<serde_json::Value, String> {
    let app_settings = settings_service::load_app_settings(&state.db).await?;
    let mut config = AgentConfig {
        display_name: input.title.clone(),
        persona: input.system_prompt.clone(),
        system_prompt: input.system_prompt.clone(),
        llm_models: input.llm_models,
        tools: Some(input.tools_enabled.clone()),
        approval: input.approval_config.unwrap_or_else(|| app_settings.default_approval.clone()),
        max_loops: input.max_loops.or(Some(app_settings.default_max_loops)),
        max_tokens: input.max_tokens,
        context_budget: input.context_budget.or(Some(app_settings.default_max_tokens)),
        max_memory_rounds: input.max_memory_rounds.or(Some(app_settings.default_max_memory_rounds)),
        memory_file_path: None,
        memory_dir: input.memory_dir.clone(),
        skills_dir: input.skills_dir.clone(),
    };
    ensure_agent_config(&mut config, &app_settings, &state.db).await?;

    let now = now_iso();
    let llm_json = serde_json::to_string(&config.llm_models).map_err(|e| format!("json: {e}"))?;
    let provider_ids_json = serde_json::to_string(&input.llm_provider_ids).map_err(|e| format!("json: {e}"))?;
    let approval_json = serde_json::to_string(&config.approval).map_err(|e| format!("json: {e}"))?;
    let tools_json = serde_json::to_string(&config.tools).map_err(|e| format!("json: {e}"))?;

    sqlx::query(
        "UPDATE chat_sessions SET
            title = ?, agent_mode = ?, tools_enabled = ?, system_prompt = ?,
            working_dir = COALESCE(?, working_dir),
            vision_provider_id = COALESCE(?, vision_provider_id),
            web_proxy = COALESCE(?, web_proxy),
            llm_models = ?, llm_provider_ids = ?, approval_config = ?, max_loops = ?, max_tokens = ?,
            context_budget = ?, max_memory_rounds = ?, memory_file_path = ?, memory_dir = ?, skills_dir = ?, updated_at = ?
         WHERE id = ?"
    )
    .bind(&config.display_name)
    .bind(&input.agent_mode)
    .bind(&tools_json)
    .bind(&config.system_prompt)
    .bind(&input.working_dir)
    .bind(&input.vision_provider_id)
    .bind(&input.web_proxy)
    .bind(&llm_json)
    .bind(&provider_ids_json)
    .bind(&approval_json)
    .bind(config.max_loops)
    .bind(config.max_tokens)
    .bind(config.context_budget)
    .bind(config.max_memory_rounds)
    .bind(&config.memory_file_path)
    .bind(&config.memory_dir)
    .bind(&config.skills_dir)
    .bind(&now)
    .bind(&session_id)
    .execute(&state.db)
    .await
    .map_err(|e| format!("db error: {e}"))?;

    let session = sqlx::query_as::<_, ChatSession>(
        "SELECT id, title, mode, project_id, working_dir, vision_provider_id, web_proxy, agent_mode, tools_enabled, system_prompt,
                llm_models, llm_provider_ids, approval_config, max_loops, max_tokens, context_budget, max_memory_rounds,
                memory_file_path, memory_dir, skills_dir, is_pinned, sort_order, icon, color, domain, context, paper_ids, created_at, updated_at
         FROM chat_sessions WHERE id = ?"
    )
    .bind(&session_id)
    .fetch_one(&state.db)
    .await
    .map_err(|e| format!("db error: {e}"))?;

    let config = build_agent_config(&state, &session).await?;
    Ok(session_to_json(
        session.id,
        config,
        session.mode,
        session.agent_mode,
        session.is_pinned.unwrap_or(0) != 0,
        session.sort_order.unwrap_or(0),
        session.paper_ids,
        session.llm_provider_ids,
        session.project_id,
        session.working_dir,
        session.vision_provider_id,
        session.web_proxy,
        session.domain,
        session.context,
        session.created_at,
        session.updated_at,
    ))
}

/// Get a single agent session with full config
#[tauri::command]
#[instrument(skip(state))]
pub async fn agent_get_session(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<serde_json::Value, String> {
    let session = sqlx::query_as::<_, ChatSession>(
        "SELECT id, title, mode, project_id, working_dir, vision_provider_id, web_proxy, agent_mode, tools_enabled, system_prompt,
                llm_models, llm_provider_ids, approval_config, max_loops, max_tokens, context_budget, max_memory_rounds,
                memory_file_path, memory_dir, skills_dir, is_pinned, sort_order, icon, color, domain, context, paper_ids, created_at, updated_at
         FROM chat_sessions WHERE id = ?"
    )
    .bind(&session_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| format!("db error: {e}"))?
    .ok_or_else(|| "session not found".to_string())?;

    let config = build_agent_config(&state, &session).await?;
    Ok(session_to_json(
        session.id,
        config,
        session.mode,
        session.agent_mode,
        session.is_pinned.unwrap_or(0) != 0,
        session.sort_order.unwrap_or(0),
        session.paper_ids,
        session.llm_provider_ids,
        session.project_id,
        session.working_dir,
        session.vision_provider_id,
        session.web_proxy,
        session.domain,
        session.context,
        session.created_at,
        session.updated_at,
    ))
}

/// Pin or unpin an agent session
#[tauri::command]
#[instrument(skip(state))]
pub async fn agent_pin_session(
    state: State<'_, AppState>,
    session_id: String,
    pinned: bool,
) -> Result<(), String> {
    let now = now_iso();
    sqlx::query("UPDATE chat_sessions SET is_pinned = ?, updated_at = ? WHERE id = ?")
        .bind(if pinned { 1 } else { 0 })
        .bind(&now)
        .bind(&session_id)
        .execute(&state.db)
        .await
        .map_err(|e| format!("db error: {e}"))?;
    Ok(())
}

/// Cancel a running agent turn for a session.
/// The cancellation token is watched by the engine via `tokio::select!`,
/// allowing LLM streams and tool calls to be aborted promptly.
#[tauri::command]
#[instrument(skip(state))]
pub async fn agent_cancel(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    let tokens = state.cancel_tokens.lock().await;
    if let Some(token) = tokens.get(&session_id) {
        token.cancel();
    }
    Ok(())
}

/// Rename a session (title only — avoids clobbering the agent config).
#[tauri::command]
#[instrument(skip(state))]
pub async fn agent_rename_session(
    state: State<'_, AppState>,
    session_id: String,
    title: String,
) -> Result<(), String> {
    let now = now_iso();
    sqlx::query("UPDATE chat_sessions SET title = ?, updated_at = ? WHERE id = ?")
        .bind(&title)
        .bind(&now)
        .bind(&session_id)
        .execute(&state.db)
        .await
        .map_err(|e| format!("db error: {e}"))?;
    Ok(())
}

fn session_to_json(
    id: String,
    config: AgentConfig,
    mode: String,
    agent_mode: String,
    is_pinned: bool,
    sort_order: i32,
    paper_ids: String,
    llm_provider_ids: Option<String>,
    project_id: Option<String>,
    working_dir: Option<String>,
    vision_provider_id: Option<String>,
    web_proxy: Option<String>,
    domain: Option<String>,
    context: Option<String>,
    created_at: String,
    updated_at: String,
) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "title": config.display_name,
        "mode": mode,
        "agent_mode": agent_mode,
        "project_id": project_id,
        "working_dir": working_dir,
        "vision_provider_id": vision_provider_id,
        "web_proxy": web_proxy,
        "tools_enabled": config.tools,
        "system_prompt": config.system_prompt,
        "llm_models": config.llm_models,
        "llm_provider_ids": llm_provider_ids,
        "approval_config": config.approval,
        "max_loops": config.max_loops,
        "max_tokens": config.max_tokens,
        "context_budget": config.context_budget,
        "max_memory_rounds": config.max_memory_rounds,
        "memory_file_path": config.memory_file_path,
        "memory_dir": config.memory_dir,
        "skills_dir": config.skills_dir,
        "is_pinned": is_pinned,
        "sort_order": sort_order,
        "icon": config.display_name.chars().next().map(|c| c.to_string()),
        "color": None::<String>,
        "domain": domain,
        "context": context,
        "paper_ids": paper_ids,
        "created_at": created_at,
        "updated_at": updated_at,
    })
}

/// Execute one agent turn for a session, streaming via "agent:event".
/// Shared by user messages and scheduled (cron) prompts.
pub(crate) async fn run_agent_turn(
    state: &AppState,
    app_handle: &AppHandle,
    session_id: String,
    content: String,
    attachments: Option<Vec<crate::ai::llm::ImageAttachment>>,
) -> Result<(), String> {
    let session = sqlx::query_as::<_, ChatSession>(
        "SELECT id, title, mode, project_id, working_dir, vision_provider_id, web_proxy, agent_mode, tools_enabled, system_prompt,
                llm_models, llm_provider_ids, approval_config, max_loops, max_tokens, context_budget, max_memory_rounds,
                memory_file_path, memory_dir, skills_dir, is_pinned, sort_order, icon, color, domain, context, paper_ids, created_at, updated_at
         FROM chat_sessions WHERE id = ?"
    )
    .bind(&session_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| format!("db error: {e}"))?
    .ok_or_else(|| "session not found".to_string())?;

    let mut config = build_agent_config(&state, &session).await?;
    // Domain (pet) sessions: resolve the system prompt at RUNTIME so prompt
    // edits in settings apply to existing sessions without recreating them.
    if let Some(domain_id) = session.domain.as_deref() {
        config.system_prompt = Some(
            crate::ai::agent::domain::runtime_prompt(
                &state.db,
                domain_id,
                session.context.as_deref(),
            )
            .await,
        );
    }
    let _app_settings = settings_service::load_app_settings(&state.db).await?;
    let device_settings = settings_service::load_device_settings(&state.db).await?;

    // Resolve the session's project directory (Codex-style project context).
    let project_dir = match &session.project_id {
        Some(pid) => crate::core::project_service::get_path(&state.db, pid).await?,
        None => None,
    };

    // Runtime self-heal for the sandbox root. working_dir is a device-local
    // absolute path that can go stale: the vault was moved or deleted, or
    // the session row arrived via sync from another device (the column no
    // longer syncs, but historical rows predate that). Re-resolve through
    // the session's project and persist the fix locally. An unhealable root
    // stays in place — file tools reject with a named-path error, bash
    // degrades to the process cwd — and the model is told to inform the user.
    let mut working_dir = session.working_dir.clone();
    let mut working_dir_warning: Option<String> = None;
    if let Some(wd) = working_dir.as_deref().filter(|w| !w.trim().is_empty()) {
        if !std::path::Path::new(wd).is_dir() {
            let healed = project_dir
                .as_deref()
                .filter(|p| std::path::Path::new(p).is_dir())
                .filter(|p| *p != wd)
                .map(|p| p.to_string());
            match healed {
                Some(p) => {
                    info!(session_id=%session_id, old=%wd, new=%p, "healed stale working_dir");
                    if let Err(e) =
                        sqlx::query("UPDATE chat_sessions SET working_dir = ? WHERE id = ?")
                            .bind(&p)
                            .bind(&session_id)
                            .execute(&state.db)
                            .await
                    {
                        warn!(error=%e, "persist healed working_dir failed");
                    }
                    working_dir = Some(p);
                }
                None => {
                    warn!(session_id=%session_id, working_dir=%wd, "session working_dir does not exist");
                    working_dir_warning = Some(format!(
                        "警告：本会话的工作目录 {wd} 在本机不存在（目录可能被移动/删除，或该会话同步自其他设备）。\
                         文件类工具会报「working directory error」；bash 会降级到进程当前目录运行。\
                         请直接告知用户此情况，并建议其在会话设置中重新指定工作目录或改为完全访问。"
                    ));
                }
            }
        }
    }

    // Per-turn cancellation token. The engine and long-running tools/streams
    // watch it via `tokio::select!` so the user can abort promptly.
    let cancel_token = tokio_util::sync::CancellationToken::new();
    {
        let mut tokens = state.cancel_tokens.lock().await;
        tokens.insert(session_id.clone(), cancel_token.clone());
    }

    // Build per-agent LLM client from active block
    let mut llm_config = config.active_llm().to_llm_config();

    // If the user sent image attachments, route to a vision-capable model.
    let has_attachments = attachments.as_ref().map(|v| !v.is_empty()).unwrap_or(false);
    if has_attachments && !llm_config.is_vision {
        llm_config = match &session.vision_provider_id {
            Some(pid) => crate::core::llm_provider_service::resolve_block(&state.db, pid)
                .await
                .map(|b| b.to_llm_config())
                .map_err(|e| format!("无法解析视觉模型配置: {e}"))?,
            None => {
                return Err("当前消息包含图片，但默认模型不支持视觉且未配置视觉模型。请在会话设置中选择视觉模型。".to_string());
            }
        };
    }

    // Agent-level per-round output cap, applied AFTER any vision-model
    // switch so it governs whichever model actually serves the turn. For pet
    // domain sessions the live setting (pet.<domain>.max_tokens) and the
    // built-in domain default outrank the session row, so settings edits
    // take effect immediately. None = follow the model config.
    let output_cap = match session.domain.as_deref() {
        Some(d) => crate::ai::agent::domain::effective_max_tokens(&state.db, d)
            .await
            .or(session.max_tokens),
        None => session.max_tokens,
    };
    if let Some(cap) = output_cap.filter(|v| *v > 0) {
        llm_config.max_tokens = cap as u32;
    }

    if llm_config.api_key.is_empty() && llm_config.provider != llm::LlmProvider::Ollama {
        return Err("API key not configured. Please set it in Settings.".to_string());
    }
    let llm_client = llm::client::create_llm_client(&llm_config)
        .map_err(|e| format!("failed to create LLM client: {e}"))?;

    // Resolve the agent's vision (multimodal) model config for the read_media_file tool.
    let vision_llm = match &session.vision_provider_id {
        Some(pid) => crate::core::llm_provider_service::resolve_block(&state.db, pid)
            .await
            .ok()
            .map(|b| b.to_llm_config()),
        None => None,
    };

    // Build filtered tool registry (sandbox root = session working_dir)
    let mut registry = ToolRegistry::default_registry(
        &state.db,
        &state.app_data_dir,
        working_dir,
        state.tasks.clone(),
        Some(session.id.clone()),
        Some(app_handle.clone()),
        vision_llm,
        session.web_proxy.clone(),
    );
    registry.retain(config.tools.as_deref());

    let data_dir_path = device_settings.data_dir.as_deref().map(std::path::PathBuf::from);

    // Register inline skills from the effective skills directory (global or
    // per-agent). Always available to the agent.
    let skills_dir = agent_skills_dir(
        &state.app_data_dir,
        data_dir_path.as_deref(),
        config.skills_dir.as_deref(),
        device_settings.skills_dir.as_deref(),
    );
    registry.register_skills(&skills_dir);

    let memory_path = agent_memory_path(
        &state.app_data_dir,
        data_dir_path.as_deref(),
        config.memory_dir.as_deref(),
        device_settings.memory_dir.as_deref(),
        config.memory_file_path.as_deref(),
        &session_id,
    );
    let memory = MemoryStore::new(memory_path.clone());

    // Save user message to DB
    let attachments_json = attachments.as_ref().map(|v| serde_json::to_string(v).unwrap_or_default()).filter(|s| !s.is_empty());
    chat::save_chat_message(&state.db, &session_id, "user", &content, None, None, None, None, None, None, None, None, None, attachments_json.as_deref()).await?;

    // Load history BEFORE appending current user message, then append it.
    // The engine itself will prepend the current user message to the prompt.
    // Following ShellAgent's approach, memory only persists user and final assistant
    // messages; intermediate tool calls / tool results live in the DB (agent_steps).
    let history_records = memory.load_recent(config.effective_max_memory_rounds());
    info!(session_id=%session_id, records=%history_records.len(), "loading agent history from memory");
    let history: Vec<ChatMessage> = history_records
        .into_iter()
        .map(|r| ChatMessage {
            role: r.role,
            content: r.content,
            attachments: r.attachments,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        })
        .collect();

    memory.append("user", &content, None, None, None, attachments_json.as_deref());

    // Pet domain sessions: build a context hint for the system prompt so the
    // agent knows which object (note/paper/...) the user is talking about.
    let context_prompt = session.context.as_ref().map(|c| {
        let ctx: serde_json::Value = serde_json::from_str(c).unwrap_or(serde_json::Value::Null);
        let name = crate::ai::agent::domain::get_domain(session.domain.as_deref().unwrap_or(""))
            .map(|d| d.name)
            .unwrap_or("助手");
        let title = ctx.get("title").and_then(|t| t.as_str()).unwrap_or("");
        let oid = ctx.get("objectId").and_then(|t| t.as_str()).unwrap_or("");
        let mut extra = String::new();
        if let Some(p) = ctx.get("pageNum").and_then(|v| v.as_i64()) {
            extra.push_str(&format!("，当前在第 {p} 页"));
        }
        if let Some(s) = ctx.get("selectedText").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
            extra.push_str(&format!("，用户选中的段落文本：{s}"));
        }
        format!("当前上下文：你在「{name}」场景中，当前对象：{title}（id: {oid}）{extra}。用户的请求针对该对象，请直接处理它。")
    });

    // A stale sandbox root (heal failed) rides the same system-prompt channel
    // so the model informs the user instead of retrying blindly.
    let context_prompt = match (context_prompt, working_dir_warning) {
        (Some(a), Some(b)) => Some(format!("{a}\n\n{b}")),
        (Some(a), None) => Some(a),
        (None, b) => b,
    };

    // Long-term memory: injected into the system prompt when the user has
    // activated it for this agent (brain button in the chat input).
    let long_term_memory = crate::core::agent_memory_service::active_content(&state.db, &session_id)
        .await
        .ok()
        .flatten();

    let engine = AgentEngine::new(
        llm_client,
        registry,
        session_id.clone(),
        state.db.clone(),
        config.clone(),
        memory,
        cancel_token.clone(),
        project_dir,
        context_prompt,
        long_term_memory,
    );

    // Create event channel
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    // Cloned for the terminal done/cancelled event, which must be emitted
    // only AFTER the run's results have been persisted (the frontend reloads
    // the session history on it). Sending through the same channel keeps it
    // ordered behind the run's delta/reasoning events.
    let done_tx = event_tx.clone();

    // Create approval channel
    let (approval_tx, mut approval_rx) = tokio::sync::mpsc::unbounded_channel::<ApprovalResponse>();
    let approval_senders = state.approval_senders.clone();
    {
        let mut senders = approval_senders.lock().await;
        senders.insert(session_id.clone(), approval_tx);
    }

    // Create ask-user channel (AskUserQuestion tool)
    let (ask_tx, mut ask_rx) = tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();
    let ask_senders = state.ask_senders.clone();
    {
        let mut senders = ask_senders.lock().await;
        senders.insert(session_id.clone(), ask_tx);
    }

    // Spawn agent processing
    let db = state.db.clone();
    let sid = session_id.clone();
    let app_clone = app_handle.clone();
    let spawn_memory_path = memory_path.clone();
    let cancel_tokens = state.cancel_tokens.clone();
    let tracked = state.background_tasks.clone();

    crate::spawn_tracked(&tracked, async move {
        let engine_ref = Arc::new(engine);
        let result = engine_ref
            .process_message(&content, attachments_json.as_deref(), &history, event_tx, &mut approval_rx, &mut ask_rx)
            .await;

        {
            let mut senders = approval_senders.lock().await;
            senders.remove(&sid);
        }
        {
            let mut senders = ask_senders.lock().await;
            senders.remove(&sid);
        }
        {
            let mut tokens = cancel_tokens.lock().await;
            tokens.remove(&sid);
        }

        let mem = MemoryStore::new(spawn_memory_path);
        match result {
            Ok((final_content, steps, cancelled, total_tokens)) => {
                // Never persist an empty reply — an empty assistant row
                // renders as a blank bubble after the frontend reloads on
                // done. Fall back to a visible placeholder.
                let display_content = if final_content.trim().is_empty() {
                    "（模型未返回文本内容）".to_string()
                } else {
                    final_content.clone()
                };
                // Save final assistant message without reasoning/tool_calls (steps hold those).
                let tokens_used = if total_tokens.total() > 0 { Some(total_tokens.total() as i32) } else { None };
                let tokens_in = if total_tokens.tokens_in > 0 { Some(total_tokens.tokens_in as i32) } else { None };
                let tokens_in_hit = if total_tokens.tokens_in_hit > 0 { Some(total_tokens.tokens_in_hit as i32) } else { None };
                let tokens_out = if total_tokens.tokens_out > 0 { Some(total_tokens.tokens_out as i32) } else { None };
                let message_id = match chat::save_chat_message(
                    &db, &sid, "assistant", &display_content, None, tokens_used, tokens_in, tokens_in_hit, tokens_out, None, None, None, None, None,
                ).await {
                    Ok(id) => Some(id),
                    Err(e) => {
                        // Was swallowed with .ok() — a failed insert makes the
                        // reply vanish from history after the done reload.
                        error!(error = %e, "failed to persist assistant message");
                        None
                    }
                };

                // Persist each ReAct step linked to the final assistant message.
                if let Some(ref mid) = message_id {
                    for step in steps {
                        if let Err(e) = sqlx::query(
                            "INSERT INTO agent_steps (id, session_id, message_id, step_index, reasoning_content, tool_calls, created_at)
                             VALUES (?, ?, ?, ?, ?, ?, ?)"
                        )
                        .bind(&step.id)
                        .bind(&sid)
                        .bind(mid)
                        .bind(step.step_index)
                        .bind(&step.reasoning_content)
                        .bind(&step.tool_calls)
                        .bind(&step.created_at)
                        .execute(&db)
                        .await {
                            error!(error = %e, step_index = step.step_index, "failed to persist agent step");
                        }
                    }
                }

                mem.append("assistant", &display_content, None, None, None, None);

                // Terminal event goes last: the frontend reloads the session
                // history on done/cancelled, so it must observe committed rows.
                let _ = done_tx.send(AgentEvent {
                    event_type: if cancelled { "cancelled".into() } else { "done".into() },
                    session_id: sid.clone(),
                    step_index: None,
                    content: Some(display_content),
                    tool_call_id: None,
                    tool_name: None,
                    tool_args: None,
                    tool_result: None,
                    status: None,
                    duration_ms: None,
                    tokens_used,
                    tokens_in,
                    tokens_in_hit,
                    tokens_out,
                });
            }
            Err(e) => {
                error!("agent error: {e}");
                let _ = app_clone.emit("agent:event", serde_json::json!({
                    "type": "error",
                    "session_id": sid,
                    "content": e,
                }));
            }
        }
    });

    // Forward events to the frontend
    let app_clone2 = app_handle.clone();
    tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            let _ = app_clone2.emit("agent:event", &event);
        }
    });

    Ok(())
}

/// Send a message to the agent for processing.
/// The agent response is streamed via Tauri events on the "agent:event" channel.
#[tauri::command]
#[instrument(skip(state, app_handle))]
pub async fn agent_send_message(
    state: State<'_, AppState>,
    app_handle: AppHandle,
    session_id: String,
    content: String,
    attachments: Option<Vec<crate::ai::llm::ImageAttachment>>,
) -> Result<(), String> {
    run_agent_turn(&state, &app_handle, session_id, content, attachments).await
}

/// Answer a pending AskUserQuestion dialog for a session.
#[tauri::command]
#[instrument(skip(state))]
pub async fn agent_answer_user(
    state: State<'_, AppState>,
    session_id: String,
    answers: serde_json::Value,
) -> Result<(), String> {
    let senders = state.ask_senders.lock().await;
    if let Some(tx) = senders.get(&session_id) {
        let _ = tx.send(answers);
        Ok(())
    } else {
        Err("no pending question for this session".to_string())
    }
}

/// Create a pet (built-in domain agent) session bound to the current context.
/// Domain sessions are hidden from the regular chat list and carry their own
/// system prompt (overridable in settings) plus the page context (object id).
#[tauri::command]
#[instrument(skip(state))]
pub async fn pet_create_session(
    state: State<'_, AppState>,
    domain: String,
    context: serde_json::Value,
) -> Result<crate::core::models::ChatSession, String> {
    if !crate::ai::agent::domain::is_enabled(&state.db, &domain).await {
        return Err(format!("domain agent not enabled: {domain}"));
    }
    let Some(agent) = crate::ai::agent::domain::get_domain(&domain) else {
        return Err(format!("unknown domain agent: {domain}"));
    };
    let prompt = crate::ai::agent::domain::runtime_prompt(
        &state.db,
        &domain,
        Some(&context.to_string()),
    )
    .await;

    let title = context
        .get("title")
        .and_then(|t| t.as_str())
        .map(|t| format!("{} · {t}", agent.name))
        .unwrap_or_else(|| agent.name.to_string());
    let now = crate::core::time::now_iso();
    let id = uuid::Uuid::new_v4().to_string();
    // Per-domain tool sets: only the tools each agent actually needs.
    let tools = match domain.as_str() {
        "note_organizer" => r#"["note_read","note_write"]"#,
        "literature_analyzer" => r#"["paper_search","paper_read","translate","note_read","note_write"]"#,
        "research_tracker" => r#"["paper_search","paper_read","note_read","note_write"]"#,
        "knowledge_curator" => r#"["knowledge_query","knowledge_create","note_read","note_write"]"#,
        "chat_summarizer" => "[]",
        _ => "[]",
    };

    // Bind the current default provider so the pet always uses the live
    // provider pool instead of a stale inline default_llm snapshot.
    let default_provider = crate::core::llm_provider_service::get_default_provider(&state.db)
        .await
        .ok()
        .flatten();
    let provider_ids: Option<Vec<String>> = default_provider.as_ref().map(|p| vec![p.id.clone()]);
    let llm_models: Vec<LlmConfigBlock> = default_provider
        .as_ref()
        .map(|p| vec![crate::core::llm_provider_service::provider_to_block(p)])
        .unwrap_or_default();
    let provider_ids_json = serde_json::to_string(&provider_ids).map_err(|e| format!("json: {e}"))?;
    let llm_models_json = serde_json::to_string(&llm_models).map_err(|e| format!("json: {e}"))?;

    sqlx::query(
        // max_tokens bound explicitly as NULL: existing DBs carry a legacy
        // column DEFAULT of 28000, which under the new semantics (per-round
        // output cap) must not leak in — domain caps resolve at runtime.
        "INSERT INTO chat_sessions (id, title, mode, paper_ids, agent_mode, tools_enabled, system_prompt, domain, context, llm_provider_ids, llm_models, approval_config, max_tokens, created_at, updated_at) \
         VALUES (?, ?, 'qa', '[]', 'chat', ?, ?, ?, ?, ?, ?, ?, NULL, ?, ?)"
    )
    .bind(&id)
    .bind(&title)
    .bind(tools)
    .bind(&prompt)
    .bind(&domain)
    .bind(context.to_string())
    .bind(&provider_ids_json)
    .bind(&llm_models_json)
    // No pinned approval policy: NULL falls back to the user's global
    // default_approval at runtime (controlled from the pet panel shield).
    .bind(Option::<&str>::None)
    .bind(&now)
    .bind(&now)
    .execute(&state.db)
    .await
    .map_err(|e| format!("db: {e}"))?;
    let session: crate::core::models::ChatSession =
        sqlx::query_as("SELECT * FROM chat_sessions WHERE id = ?")
            .bind(&id)
            .fetch_one(&state.db)
            .await
            .map_err(|e| format!("db: {e}"))?;
    Ok(session)
}

/// Built-in pet domain agent info (used by the settings UI to show the
/// default system prompt as a placeholder).
#[derive(serde::Serialize)]
pub struct PetDomainInfo {
    pub id: String,
    pub name: String,
    pub default_prompt: String,
    pub default_max_tokens: Option<i32>,
}

/// List all built-in pet domain agents with their default system prompts.
#[tauri::command]
pub async fn pet_domains() -> Result<Vec<PetDomainInfo>, String> {
    Ok(crate::ai::agent::domain::builtin_domains()
        .into_iter()
        .map(|d| PetDomainInfo {
            id: d.id.to_string(),
            name: d.name.to_string(),
            default_prompt: d.default_prompt.to_string(),
            default_max_tokens: d.default_max_tokens,
        })
        .collect())
}

/// Get ReAct steps for a session.
#[tauri::command]
#[instrument(skip(state))]
pub async fn get_agent_steps(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<AgentStep>, String> {
    let steps = sqlx::query_as::<_, AgentStep>(
        "SELECT id, session_id, message_id, step_index, reasoning_content, tool_calls, created_at
         FROM agent_steps WHERE session_id = ? ORDER BY step_index ASC"
    )
    .bind(&session_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| format!("db error: {e}"))?;

    Ok(steps)
}

/// List agent sessions, optionally filtered by project
#[tauri::command]
#[instrument(skip(state))]
pub async fn agent_list_sessions(
    state: State<'_, AppState>,
    project_id: Option<String>,
) -> Result<Vec<serde_json::Value>, String> {
    const SESSION_COLS: &str = "id, title, mode, project_id, working_dir, vision_provider_id, web_proxy, agent_mode, tools_enabled, system_prompt, \
         llm_models, llm_provider_ids, approval_config, max_loops, max_tokens, context_budget, max_memory_rounds, \
         memory_file_path, memory_dir, skills_dir, is_pinned, sort_order, icon, color, domain, context, paper_ids, created_at, updated_at";

    let rows = if let Some(pid) = project_id {
        sqlx::query_as::<_, ChatSession>(&format!(
            "SELECT {SESSION_COLS} FROM chat_sessions WHERE project_id = ? \
             ORDER BY is_pinned DESC, sort_order ASC, updated_at DESC"
        ))
        .bind(&pid)
        .fetch_all(&state.db)
        .await
        .map_err(|e| format!("db error: {e}"))?
    } else {
        sqlx::query_as::<_, ChatSession>(&format!(
            "SELECT {SESSION_COLS} FROM chat_sessions \
             ORDER BY is_pinned DESC, sort_order ASC, updated_at DESC"
        ))
        .fetch_all(&state.db)
        .await
        .map_err(|e| format!("db error: {e}"))?
    };

    let mut sessions = Vec::with_capacity(rows.len());
    for session in rows {
        let config = match build_agent_config(&state, &session).await {
            Ok(c) => c,
            Err(e) => {
                error!(session_id=%session.id, error=%e, "failed to build agent config");
                continue;
            }
        };
        sessions.push(session_to_json(
            session.id,
            config,
            session.mode,
            session.agent_mode,
            session.is_pinned.unwrap_or(0) != 0,
            session.sort_order.unwrap_or(0),
            session.paper_ids,
            session.llm_provider_ids,
            session.project_id,
            session.working_dir,
            session.vision_provider_id,
            session.web_proxy,
            session.domain,
            session.context,
            session.created_at,
            session.updated_at,
        ));
    }

    Ok(sessions)
}

/// Delete an agent session
#[tauri::command]
#[instrument(skip(state))]
pub async fn agent_delete_session(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<(), String> {
    let device_settings = settings_service::load_device_settings(&state.db).await.unwrap_or_default();
    let data_dir = device_settings.data_dir.as_deref().map(std::path::Path::new);
    // Try to delete JSONL memory file using default paths. Per-agent override is not
    // available here without fetching the session; fall back to defaults.
    let memory_path = agent_memory_path(&state.app_data_dir, data_dir, None, device_settings.memory_dir.as_deref(), None, &session_id);
    let _ = std::fs::remove_file(&memory_path);
    let _ = std::fs::remove_file(memory_path.with_extension("jsonl.meta"));

    sqlx::query("DELETE FROM tool_executions WHERE session_id = ?")
        .bind(&session_id)
        .execute(&state.db).await.map_err(|e| format!("db: {e}"))?;
    sqlx::query("DELETE FROM chat_messages WHERE session_id = ?")
        .bind(&session_id)
        .execute(&state.db).await.map_err(|e| format!("db: {e}"))?;
    // CRR tables declare no checked FKs: cascade explicitly.
    crate::core::agent_memory_service::delete_for_session(&state.db, &session_id)
        .await.map_err(|e| format!("db: {e}"))?;
    sqlx::query("DELETE FROM chat_sessions WHERE id = ?")
        .bind(&session_id)
        .execute(&state.db).await.map_err(|e| format!("db: {e}"))?;
    Ok(())
}

/// Respond to a pending tool approval.
/// decision: "approve" (modified_args, when present, replaces the tool
/// arguments), "decline" (turn continues), "decline_guide" (turn continues,
/// guidance text is handed to the agent as feedback), "decline_stop"
/// (ends the whole turn).
#[tauri::command]
#[instrument(skip(state))]
pub async fn agent_approve_tool(
    state: State<'_, AppState>,
    session_id: String,
    _tool_call_id: String,
    decision: String,
    guidance: Option<String>,
    modified_args: Option<serde_json::Value>,
) -> Result<(), String> {
    let senders = state.approval_senders.lock().await;
    let tx = senders
        .get(&session_id)
        .ok_or_else(|| "no pending approval for this session".to_string())?;

    let response = match decision.as_str() {
        "approve" => match modified_args {
            Some(args) => ApprovalResponse::ModifiedArgs(args),
            None => ApprovalResponse::Approved,
        },
        "decline_guide" => ApprovalResponse::DeclinedWithGuidance(guidance.unwrap_or_default()),
        "decline_stop" => ApprovalResponse::DeclinedStop,
        _ => ApprovalResponse::Declined,
    };

    tx.send(response)
        .map_err(|e| format!("approval channel closed: {e}"))?;
    Ok(())
}

/// Update only the approval config of a session — the quick switch in the
/// chat input. Takes effect on the next turn (the running turn's engine
/// already holds its config).
#[tauri::command]
#[instrument(skip(state))]
pub async fn agent_set_approval_config(
    state: State<'_, AppState>,
    session_id: String,
    approval_config: crate::ai::agent::config::ApprovalConfig,
) -> Result<(), String> {
    let json = serde_json::to_string(&approval_config).map_err(|e| format!("json: {e}"))?;
    sqlx::query("UPDATE chat_sessions SET approval_config = ?, updated_at = ? WHERE id = ?")
        .bind(&json)
        .bind(now_iso())
        .bind(&session_id)
        .execute(&state.db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    Ok(())
}

// ── Settings ──

#[tauri::command]
#[instrument(skip(state))]
pub async fn settings_app_get(
    state: State<'_, AppState>,
) -> Result<crate::core::models::AppSettings, String> {
    settings_service::load_app_settings(&state.db).await
}

#[tauri::command]
#[instrument(skip(state, app, settings))]
pub async fn settings_app_save(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    settings: crate::core::models::AppSettings,
) -> Result<(), String> {
    settings_service::save_app_settings(&state.db, &settings).await?;

    // Sync the pet window visibility with the setting.
    if settings.show_pet {
        match app.get_webview_window("pet") {
            Some(pet) => {
                let _ = pet.show();
                let _ = pet.set_focus();
            }
            None => {
                crate::create_pet_window(&app).map_err(|e| format!("failed to create pet window: {e}"))?;
            }
        }
    } else if let Some(pet) = app.get_webview_window("pet") {
        let _ = pet.hide();
    }

    // Notify all webviews that the app settings have changed.
    let _ = app.emit("app:settings_changed", ());

    Ok(())
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn settings_get(
    state: State<'_, AppState>,
    key: String,
) -> Result<Option<String>, String> {
    settings_service::get_setting(&state.db, &key).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn settings_set(
    state: State<'_, AppState>,
    key: String,
    value: String,
) -> Result<(), String> {
    settings_service::set_setting(&state.db, &key, &value).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn settings_get_all(
    state: State<'_, AppState>,
) -> Result<Vec<crate::core::models::Setting>, String> {
    settings_service::get_all_settings(&state.db).await
}

#[tauri::command]
pub async fn settings_get_data_dir(
    state: State<'_, AppState>,
) -> Result<String, String> {
    let device_settings = settings_service::load_device_settings(&state.db).await?;
    Ok(device_settings
        .data_dir
        .map(PathBuf::from)
        .unwrap_or_else(|| state.app_data_dir.clone())
        .to_string_lossy()
        .to_string())
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn settings_set_data_dir(
    state: State<'_, AppState>,
    path: String,
) -> Result<(), String> {
    let mut device_settings = settings_service::load_device_settings(&state.db).await?;
    device_settings.data_dir = Some(path);
    settings_service::save_device_settings(&state.db, &device_settings).await
}

#[tauri::command]
pub async fn settings_validate_llm(
    provider: String,
    api_key: String,
    base_url: String,
    model: String,
    proxy: Option<String>,
) -> Result<bool, String> {
    let provider = match provider.to_lowercase().as_str() {
        "openai" => llm::LlmProvider::OpenAI,
        "anthropic" => llm::LlmProvider::Anthropic,
        "deepseek" => llm::LlmProvider::DeepSeek,
        "siliconflow" => llm::LlmProvider::SiliconFlow,
        "ollama" => llm::LlmProvider::Ollama,
        "qwen" => llm::LlmProvider::Qwen,
        "zhipu" => llm::LlmProvider::Zhipu,
        "kimi" => llm::LlmProvider::Kimi,
        "gemini" => llm::LlmProvider::Gemini,
        _ => return Err(format!("unknown provider: {provider}")),
    };

    let config = llm::LlmConfig {
        provider,
        api_key,
        base_url,
        model,
        proxy,
        max_tokens: 100,
        temperature: 0.0,
        is_vision: false,
    };

    settings_service::validate_llm_config(&config).await
}

#[tauri::command]
pub async fn settings_get_memory_dir(
    state: State<'_, AppState>,
) -> Result<String, String> {
    let device_settings = settings_service::load_device_settings(&state.db).await?;
    let base = device_settings
        .data_dir
        .as_deref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| state.app_data_dir.clone());
    Ok(device_settings.memory_dir.unwrap_or_else(|| default_memory_dir(&base).to_string_lossy().to_string()))
}

#[tauri::command]
pub async fn settings_ensure_directories(
    state: State<'_, AppState>,
) -> Result<(), String> {
    let device_settings = settings_service::load_device_settings(&state.db).await?;
    let base = device_settings
        .data_dir
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| state.app_data_dir.clone());
    let dirs = [
        Some(base.clone()),
        device_settings.memory_dir.map(PathBuf::from).or_else(|| Some(default_memory_dir(&base))),
        device_settings.skills_dir.map(PathBuf::from).or_else(|| Some(default_skills_dir(&base))),
    ];
    for dir in dirs.into_iter().flatten() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(&dir).map_err(|e| format!("create dir {}: {e}", dir.display()))?;
        }
    }
    Ok(())
}
