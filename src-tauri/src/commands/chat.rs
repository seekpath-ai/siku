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
         llm_models, llm_provider_ids, approval_config, max_loops, max_tokens, max_memory_rounds, \
         memory_file_path, memory_dir, skills_dir, is_pinned, sort_order, icon, color, domain, context, paper_ids, created_at, updated_at";

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
        "INSERT INTO chat_sessions (id, title, mode, agent_mode, tools_enabled, system_prompt, project_id, paper_ids, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, '[]', ?, ?)"
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
                llm_models, llm_provider_ids, approval_config, max_loops, max_tokens, max_memory_rounds,
                memory_file_path, memory_dir, skills_dir, is_pinned, sort_order, icon, color, domain, context, paper_ids, created_at, updated_at
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

    sqlx::query("DELETE FROM chat_sessions WHERE id = ?")
        .bind(&session_id)
        .execute(&state.db)
        .await
        .map_err(|e| format!("db error: {e}"))?;

    Ok(())
}

/// Get messages for a session
#[tauri::command]
#[instrument(skip(state))]
pub async fn get_chat_messages(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<ChatMessage>, String> {
    let messages = sqlx::query_as::<_, ChatMessage>(
        "SELECT id, session_id, role, content, reasoning_content, tool_calls, tool_call_id, tool_name, citations, model, tokens_used, tokens_in, tokens_in_hit, tokens_out, created_at
         FROM chat_messages WHERE session_id = ? ORDER BY created_at ASC"
    )
    .bind(&session_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| format!("db error: {e}"))?;

    Ok(messages)
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
) -> Result<String, String> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = time::now_iso();

    sqlx::query(
        "INSERT INTO chat_messages (id, session_id, role, content, reasoning_content, tool_calls, tool_call_id, tool_name, model, tokens_used, tokens_in, tokens_in_hit, tokens_out, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
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
