use tauri::State;
use crate::AppState;
use crate::core::models::{ChatSession, ChatMessage};
use crate::core::time;
use tracing::instrument;

/// List chat sessions, optionally filtered by project
#[tauri::command]
#[instrument(skip(state))]
pub async fn list_chat_sessions(
    state: State<'_, AppState>,
    project_id: Option<String>,
) -> Result<Vec<ChatSession>, String> {
    const SESSION_COLS: &str = "id, title, mode, project_id, working_dir, vision_provider_id, web_proxy, agent_mode, tools_enabled, system_prompt, \
         llm_models, llm_provider_ids, approval_config, max_loops, max_tokens, context_budget, max_memory_rounds, \
         memory_file_path, memory_dir, skills_dir, is_pinned, sort_order, archived, icon, color, domain, context, selected_skills, paper_ids, created_at, updated_at";

    let sessions = if let Some(pid) = project_id {
        sqlx::query_as::<_, ChatSession>(&format!(
            "SELECT {SESSION_COLS} FROM chat_sessions WHERE project_id = ? ORDER BY updated_at DESC"
        ))
        .bind(&pid)
        .fetch_all(&state.db)
        .await
        .map_err(|e| format!("db error: {e}"))?
    } else {
        sqlx::query_as::<_, ChatSession>(&format!(
            "SELECT {SESSION_COLS} FROM chat_sessions ORDER BY updated_at DESC"
        ))
        .fetch_all(&state.db)
        .await
        .map_err(|e| format!("db error: {e}"))?
    };

    Ok(sessions)
}

/// Create a new chat session
#[tauri::command]
#[instrument(skip(state))]
pub async fn create_chat_session(
    state: State<'_, AppState>,
    title: String,
    mode: Option<String>,
    agent_mode: Option<String>,
    tools_enabled: Option<Vec<String>>,
    system_prompt: Option<String>,
    project_id: Option<String>,
) -> Result<ChatSession, String> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = time::now_iso();
    let mode = mode.unwrap_or_else(|| "qa".to_string());
    let agent_mode = agent_mode.unwrap_or_else(|| "chat".to_string());
    let tools_enabled_json = serde_json::to_string(&tools_enabled.unwrap_or_default())
        .map_err(|e| format!("json error: {e}"))?;

    sqlx::query(
        // max_tokens bound explicitly: existing DBs carry a legacy column
        // DEFAULT of 28000, which under the new semantics (per-round output
        // cap) must not leak into fresh sessions — NULL = follow the model.
        "INSERT INTO chat_sessions (id, title, mode, agent_mode, tools_enabled, system_prompt, project_id, paper_ids, max_tokens, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, '[]', NULL, ?, ?)"
    )
    .bind(&id)
    .bind(&title)
    .bind(&mode)
    .bind(&agent_mode)
    .bind(&tools_enabled_json)
    .bind(&system_prompt)
    .bind(&project_id)
    .bind(&now)
    .bind(&now)
    .execute(&state.db)
    .await
    .map_err(|e| format!("db error: {e}"))?;

    let session = sqlx::query_as::<_, ChatSession>(
        "SELECT id, title, mode, project_id, working_dir, vision_provider_id, web_proxy, agent_mode, tools_enabled, system_prompt,
                llm_models, llm_provider_ids, approval_config, max_loops, max_tokens, context_budget, max_memory_rounds,
                memory_file_path, memory_dir, skills_dir, is_pinned, sort_order, archived, icon, color, domain, context, selected_skills, paper_ids, created_at, updated_at
         FROM chat_sessions WHERE id = ?"
    )
    .bind(&id)
    .fetch_one(&state.db)
    .await
    .map_err(|e| format!("db error: {e}"))?;

    Ok(session)
}

/// Delete a chat session and its messages
#[tauri::command]
#[instrument(skip(state))]
pub async fn delete_chat_session(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<(), String> {
    sqlx::query("DELETE FROM chat_messages WHERE session_id = ?")
        .bind(&session_id)
        .execute(&state.db)
        .await
        .map_err(|e| format!("db error: {e}"))?;

    // CRR tables declare no checked FKs: cascade explicitly.
    crate::core::agent_memory_service::delete_for_session(&state.db, &session_id)
        .await
        .map_err(|e| format!("db error: {e}"))?;

    sqlx::query("DELETE FROM chat_sessions WHERE id = ?")
        .bind(&session_id)
        .execute(&state.db)
        .await
        .map_err(|e| format!("db error: {e}"))?;

    Ok(())
}

/// Get the long-term memory for an agent (session). Returns None when the
/// user has never written one.
#[tauri::command]
#[instrument(skip(state))]
pub async fn agent_memory_get(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Option<crate::core::agent_memory_service::AgentMemory>, String> {
    crate::core::agent_memory_service::get(&state.db, &session_id)
        .await
        .map_err(|e| format!("db error: {e}"))
}

/// Save the long-term memory content for an agent (upsert, keeps the
/// active flag).
#[tauri::command]
#[instrument(skip(state))]
pub async fn agent_memory_set(
    state: State<'_, AppState>,
    session_id: String,
    content: String,
) -> Result<(), String> {
    crate::core::agent_memory_service::set_content(&state.db, &session_id, &content)
        .await
        .map_err(|e| format!("db error: {e}"))
}

/// Restore an agent's long-term memory from a version snapshot (snapshots
/// reuse the note_versions table, keyed by session id). The current content
/// is snapshotted first so the restore can be undone.
#[tauri::command]
#[instrument(skip(state))]
pub async fn agent_memory_restore(
    state: State<'_, AppState>,
    session_id: String,
    version_id: String,
) -> Result<(), String> {
    crate::core::agent_memory_service::restore(&state.db, &session_id, &version_id)
        .await
        .map_err(|e| format!("db error: {e}"))
}

/// Activate or "forget" an agent's long-term memory. Forgotten memories are
/// kept but not injected into the system prompt.
#[tauri::command]
#[instrument(skip(state))]
pub async fn agent_memory_set_active(
    state: State<'_, AppState>,
    session_id: String,
    active: bool,
) -> Result<(), String> {
    crate::core::agent_memory_service::set_active(&state.db, &session_id, active)
        .await
        .map_err(|e| format!("db error: {e}"))
}

/// Get messages for a session
#[tauri::command]
#[instrument(skip(state))]
pub async fn get_chat_messages(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<ChatMessage>, String> {
    let messages = sqlx::query_as::<_, ChatMessage>(
        "SELECT id, session_id, role, content, reasoning_content, tool_calls, tool_call_id, tool_name, citations, model, tokens_used, tokens_in, tokens_in_hit, tokens_out, attachments, user_tag, created_at
         FROM chat_messages WHERE session_id = ? ORDER BY created_at ASC"
    )
    .bind(&session_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| format!("db error: {e}"))?;

    Ok(messages)
}

/// Tag a message from the bubble action bar ("打标即搬运"):
/// - "experience": content appended to the session's long-term memory
/// - "knowledge": content saved as a knowledge item (notes domain)
/// - "chitchat": tag only, nothing is copied
/// - None: clear the tag
/// Returns the updated message so the store can patch it in place.
#[tauri::command]
#[instrument(skip(state))]
pub async fn chat_message_tag(
    state: State<'_, AppState>,
    message_id: String,
    tag: Option<String>,
) -> Result<ChatMessage, String> {
    const COLS: &str = "id, session_id, role, content, reasoning_content, tool_calls, tool_call_id, tool_name, citations, model, tokens_used, tokens_in, tokens_in_hit, tokens_out, attachments, user_tag, created_at";
    let msg = sqlx::query_as::<_, ChatMessage>(&format!("SELECT {COLS} FROM chat_messages WHERE id = ?"))
        .bind(&message_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| format!("db error: {e}"))?
        .ok_or_else(|| "message not found".to_string())?;

    match tag.as_deref() {
        None => {}
        Some("experience") => {
            let entry = format!("## 经验（{}）\n\n{}", &time::now_iso()[..10], msg.content.trim());
            crate::core::agent_memory_service::append(&state.db, &msg.session_id, &entry)
                .await
                .map_err(|e| format!("memory: {e}"))?;
        }
        Some("knowledge") => {
            let domain_id: Option<(String,)> = sqlx::query_as(
                "SELECT id FROM knowledge_domains WHERE domain_type = 'notes' LIMIT 1",
            )
            .fetch_optional(&state.db)
            .await
            .map_err(|e| format!("db: {e}"))?;
            let did = domain_id.map(|(id,)| id).unwrap_or_else(|| "dom-notes".to_string());
            let title: String = msg
                .content
                .lines()
                .next()
                .map(|l| l.trim_start_matches('#').trim())
                .filter(|l| !l.is_empty())
                .map(|l| l.chars().take(30).collect::<String>())
                .unwrap_or_else(|| "对话摘录".to_string());
            let id = uuid::Uuid::new_v4().to_string();
            let now = time::now_iso();
            sqlx::query(
                "INSERT INTO knowledge_items (id, domain_id, title, content_type, content, tags, metadata, created_at, updated_at) \
                 VALUES (?, ?, ?, 'note', ?, '[]', '{}', ?, ?)",
            )
            .bind(&id)
            .bind(&did)
            .bind(&title)
            .bind(msg.content.trim())
            .bind(&now)
            .bind(&now)
            .execute(&state.db)
            .await
            .map_err(|e| format!("db: {e}"))?;
        }
        Some("chitchat") => {}
        Some(other) => return Err(format!("unknown tag: {other}")),
    }

    sqlx::query("UPDATE chat_messages SET user_tag = ? WHERE id = ?")
        .bind(tag.as_deref())
        .bind(&message_id)
        .execute(&state.db)
        .await
        .map_err(|e| format!("db error: {e}"))?;

    sqlx::query_as::<_, ChatMessage>(&format!("SELECT {COLS} FROM chat_messages WHERE id = ?"))
        .bind(&message_id)
        .fetch_one(&state.db)
        .await
        .map_err(|e| format!("db error: {e}"))
}

/// Save a message to a session
#[instrument(skip(db))]
pub async fn save_chat_message(
    db: &sqlx::SqlitePool,
    session_id: &str,
    role: &str,
    content: &str,
    model: Option<&str>,
    tokens_used: Option<i32>,
    tokens_in: Option<i32>,
    tokens_in_hit: Option<i32>,
    tokens_out: Option<i32>,
    tool_calls: Option<&str>,
    tool_call_id: Option<&str>,
    tool_name: Option<&str>,
    reasoning_content: Option<&str>,
    attachments: Option<&str>,
) -> Result<String, String> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = time::now_iso();

    sqlx::query(
        "INSERT INTO chat_messages (id, session_id, role, content, reasoning_content, tool_calls, tool_call_id, tool_name, model, tokens_used, tokens_in, tokens_in_hit, tokens_out, attachments, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
    )
    .bind(&id)
    .bind(session_id)
    .bind(role)
    .bind(content)
    .bind(reasoning_content)
    .bind(tool_calls)
    .bind(tool_call_id)
    .bind(tool_name)
    .bind(model)
    .bind(tokens_used)
    .bind(tokens_in)
    .bind(tokens_in_hit)
    .bind(tokens_out)
    .bind(attachments)
    .bind(&now)
    .execute(db)
    .await
    .map_err(|e| format!("db error: {e}"))?;

    sqlx::query("UPDATE chat_sessions SET updated_at = ? WHERE id = ?")
        .bind(&now)
        .bind(session_id)
        .execute(db)
        .await
        .map_err(|e| format!("db error: {e}"))?;

    Ok(id)
}
