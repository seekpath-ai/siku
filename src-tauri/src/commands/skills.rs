use tauri::State;
use tracing::instrument;

use crate::core::skill_review::{self, ReviewRecord, StaticReport};
use crate::core::skills::{self, SkillInfo};
use crate::AppState;

/// Default user skills directory: {app_data_dir}/skills.
fn user_skills_dir(state: &AppState) -> std::path::PathBuf {
    state.app_data_dir.join("skills")
}

/// List installed external skills for the plugins dialog (with review badges).
#[tauri::command]
#[instrument(skip(state))]
pub async fn skills_list(state: State<'_, AppState>) -> Result<Vec<SkillInfo>, String> {
    Ok(skills::list_all(&state.db, &user_skills_dir(&state)).await)
}

/// Full skill detail (incl. SKILL.md body + review record) for the detail dialog.
#[tauri::command]
#[instrument(skip(state))]
pub async fn skills_get(state: State<'_, AppState>, name: String) -> Result<SkillDetail, String> {
    let (skill, path) =
        skills::get(&user_skills_dir(&state), &name).ok_or(format!("技能「{name}」不存在"))?;
    let review = skill_review::get_review(&state.db, &name).await;
    Ok(SkillDetail {
        name: skill.name,
        description: skill.description,
        content: skill.content,
        path,
        review,
    })
}

/// Detail payload for the plugins dialog's second-level view.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillDetail {
    pub name: String,
    pub description: String,
    /// SKILL.md body (the instructions injected when the skill tool runs).
    pub content: String,
    pub path: String,
    /// Latest review record; absent = never reviewed.
    pub review: Option<ReviewRecord>,
}

/// Import a skill from a folder containing SKILL.md.
#[tauri::command]
#[instrument(skip(state))]
pub async fn skills_import_folder(
    state: State<'_, AppState>,
    path: String,
) -> Result<SkillInfo, String> {
    let dir = user_skills_dir(&state);
    std::fs::create_dir_all(&dir).map_err(|e| format!("无法创建插件目录：{e}"))?;
    skills::import_folder(&dir, std::path::Path::new(&path))
}

/// Import a skill from a zip archive.
#[tauri::command]
#[instrument(skip(state))]
pub async fn skills_import_zip(
    state: State<'_, AppState>,
    path: String,
) -> Result<SkillInfo, String> {
    let dir = user_skills_dir(&state);
    std::fs::create_dir_all(&dir).map_err(|e| format!("无法创建插件目录：{e}"))?;
    skills::import_zip(&dir, std::path::Path::new(&path))
}

/// Delete an installed skill (removes its directory). Sessions that mounted
/// it simply stop seeing the tool on the next turn.
#[tauri::command]
#[instrument(skip(state))]
pub async fn skills_delete(state: State<'_, AppState>, name: String) -> Result<(), String> {
    skills::delete(&user_skills_dir(&state), &name)?;
    // Drop any stored review alongside the skill.
    skill_review::drop_review(&state.db, &name).await?;
    Ok(())
}

/// Result of `skills_review_start`: the reviewer session to display plus the
/// deterministic scan report (shown in the detail dialog immediately).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillReviewStart {
    pub session_id: String,
    pub report: StaticReport,
}

/// Start an AI review for a skill: run the deterministic static scan, create
/// a no-tool 安全审查 domain session, and fire the review turn. The frontend
/// displays the session in a pet-chat window and calls
/// `skills_review_collect` when the turn completes.
#[tauri::command]
#[instrument(skip(state, app_handle))]
pub async fn skills_review_start(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
    name: String,
) -> Result<SkillReviewStart, String> {
    let dir = user_skills_dir(&state).join(&name);
    if !dir.is_dir() {
        return Err(format!("技能「{name}」不存在"));
    }
    let report = skill_review::static_scan(&dir);

    // Reviewer session: domain agent with NO tools — content under review
    // must never be executable by the reviewer itself.
    let session = crate::commands::agent::pet_create_session(
        state.clone(),
        "skill_reviewer".to_string(),
        serde_json::json!({ "title": name }),
    )
    .await?;
    let session_id = session
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or("创建审查会话失败")?
        .to_string();

    let prompt = skill_review::build_review_prompt(&name, &dir, &report);
    crate::commands::agent::run_agent_turn(&state, &app_handle, session_id.clone(), prompt, None)
        .await?;

    Ok(SkillReviewStart { session_id, report })
}

/// Collect the verdict after the review turn completes: parse the reviewer
/// agent's ```verdict block, merge with the static verdict (static wins
/// ties), and persist the record keyed by content hash.
#[tauri::command]
#[instrument(skip(state))]
pub async fn skills_review_collect(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<ReviewRecord, String> {
    // The skill name travels in the session's pet context ({"title": name}).
    let context: Option<String> =
        sqlx::query_scalar("SELECT context FROM chat_sessions WHERE id = ?")
            .bind(&session_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| format!("db error: {e}"))?
            .flatten();
    let name = context
        .as_deref()
        .and_then(|c| serde_json::from_str::<serde_json::Value>(c).ok())
        .and_then(|v| v.get("title").and_then(|t| t.as_str()).map(|s| s.to_string()))
        .ok_or("审查会话缺少技能名上下文")?;

    let dir = user_skills_dir(&state).join(&name);
    if !dir.is_dir() {
        return Err(format!("技能「{name}」已被删除"));
    }
    // Re-scan so the record anchors to the content as it stands NOW.
    let report = skill_review::static_scan(&dir);

    let llm_content: Option<String> = sqlx::query_scalar(
        "SELECT content FROM chat_messages WHERE session_id = ? AND role = 'assistant' \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(&session_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| format!("db error: {e}"))?
    .flatten();
    let llm = llm_content.as_deref().and_then(skill_review::parse_verdict);

    let record = ReviewRecord {
        verdict: skill_review::merge_verdicts(&report.verdict, llm.as_ref().map(|v| v.0.as_str())),
        static_verdict: report.verdict.clone(),
        summary: llm
            .as_ref()
            .map(|v| v.1.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| match report.verdict.as_str() {
                "risk" => format!("静态扫描发现 {} 处风险模式", report.findings.len()),
                "warn" => format!("静态扫描发现 {} 处需注意的模式", report.findings.len()),
                _ => "静态扫描未发现风险模式".to_string(),
            }),
        llm_verdict: llm.map(|v| v.0),
        findings: report.findings.into_iter().take(100).collect(),
        dependencies: report.dependencies,
        content_hash: report.content_hash,
        reviewed_at: crate::core::time::now_iso(),
    };
    skill_review::save_review(&state.db, &name, record.clone()).await?;
    Ok(record)
}

/// Open the user skills directory in the OS file manager (created when
/// missing). External skills live there as `<name>/SKILL.md`.
#[tauri::command]
#[instrument(skip(state))]
pub async fn skills_open_directory(state: State<'_, AppState>) -> Result<(), String> {
    let dir = user_skills_dir(&state);
    std::fs::create_dir_all(&dir).map_err(|e| format!("无法创建插件目录：{e}"))?;
    crate::core::file_service::reveal_in_system(&dir.to_string_lossy())
}

