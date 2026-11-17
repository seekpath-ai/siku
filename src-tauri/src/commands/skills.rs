use std::collections::HashMap;

use tauri::State;
use tracing::instrument;

use crate::core::skill_review::{self, ReviewRecord, StaticReport};
use crate::core::skills::{self, DepStatus, SkillInfo, SkillSource};
use crate::AppState;

/// Default user skills directory: {app_data_dir}/skills.
fn user_skills_dir(state: &AppState) -> std::path::PathBuf {
    state.app_data_dir.join("skills")
}

fn builtin_skills_dir(state: &AppState) -> Option<&std::path::Path> {
    state.builtin_skills_dir.as_deref()
}

const IGNORED_DEPS_KEY: &str = "skills.ignored_dependencies";

async fn ignored_dependencies(db: &sqlx::SqlitePool) -> HashMap<String, Vec<String>> {
    crate::core::settings_service::get_device_setting(db, IGNORED_DEPS_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default()
}

fn apply_ignored(deps: &mut [DepStatus], ignored: &[String]) {
    for dep in deps {
        dep.ignored = ignored.iter().any(|n| n == &dep.name);
    }
}

/// List installed skills (built-in first-party + user-imported) for the
/// plugins dialog.
#[tauri::command]
#[instrument(skip(state))]
pub async fn skills_list(state: State<'_, AppState>) -> Result<Vec<SkillInfo>, String> {
    let mut list = skills::list_all(
        &state.db,
        &state.app_data_dir,
        builtin_skills_dir(&state),
        &user_skills_dir(&state),
    )
    .await;
    let ignored = ignored_dependencies(&state.db).await;
    for skill in &mut list {
        if let Some(names) = ignored.get(&skill.name) {
            apply_ignored(&mut skill.dependencies, names);
        }
    }
    Ok(list)
}

/// Full skill detail (incl. SKILL.md body + review record + probed
/// dependency status) for the detail dialog.
#[tauri::command]
#[instrument(skip(state))]
pub async fn skills_get(state: State<'_, AppState>, name: String) -> Result<SkillDetail, String> {
    let (skill, path) = skills::get(builtin_skills_dir(&state), &user_skills_dir(&state), &name)
        .ok_or(format!("技能「{name}」不存在"))?;
    let record = match skill.source {
        // 内置技能一方可信，免审查。
        SkillSource::Builtin => None,
        SkillSource::User => skill_review::get_review(&state.db, &name).await,
    };
    let review_badge = record.as_ref().map(|rec| {
        let hash = skill_review::content_hash(&skill.dir);
        skill_review::ReviewBadge {
            verdict: rec.verdict.clone(),
            stale: rec.content_hash != hash,
        }
    });
    let review_state = match skill.source {
        SkillSource::Builtin => "builtin".to_string(),
        SkillSource::User => match review_badge.as_ref() {
            None => "unreviewed".to_string(),
            Some(b) if b.stale => "stale".to_string(),
            Some(b) => b.verdict.clone(),
        },
    };
    let declared = !skill.requires.is_empty();
    let mut dependencies = if declared {
        skills::probe_dependencies_cached(&state.app_data_dir, &skill.requires, &skill.dir)
    } else {
        record
            .as_ref()
            .map(|rec| skills::probe_review_dependencies(&state.app_data_dir, rec, &skill.dir))
            .unwrap_or_default()
    };
    let ignored = ignored_dependencies(&state.db).await;
    if let Some(names) = ignored.get(&name) {
        apply_ignored(&mut dependencies, names);
    }
    let dependency_source = if declared {
        "declared".to_string()
    } else if record.is_some() {
        "review".to_string()
    } else if skill.source == SkillSource::User {
        "unreviewed".to_string()
    } else {
        "none".to_string()
    };
    Ok(SkillDetail {
        name: skill.name,
        description: skill.description,
        content: skill.content,
        path,
        review: record,
        review_state,
        dependency_source,
        source: skill.source,
        dependencies,
        tools: skill.tools,
        scenarios: skill.scenarios,
        rules: skill.rules,
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
    /// Latest review record; absent = never reviewed (or built-in).
    pub review: Option<ReviewRecord>,
    pub review_state: String,
    pub dependency_source: String,
    pub source: SkillSource,
    /// Declared dependencies with live availability (probed on open).
    pub dependencies: Vec<DepStatus>,
    /// 技能声明的工具权限（挂载后并入会话工具白名单）。
    pub tools: Vec<String>,
    pub scenarios: Vec<String>,
    pub rules: Vec<String>,
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
    let result = skills::import_folder(builtin_skills_dir(&state), &dir, std::path::Path::new(&path));
    if result.is_ok() {
        skills::clear_dependency_cache();
    }
    result
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
    let result = skills::import_zip(builtin_skills_dir(&state), &dir, std::path::Path::new(&path));
    if result.is_ok() {
        skills::clear_dependency_cache();
    }
    result
}

/// Delete an installed user skill (removes its directory). Sessions that
/// mounted it simply stop seeing the tool on the next turn. Built-in skills
/// are read-only and cannot be deleted.
#[tauri::command]
#[instrument(skip(state))]
pub async fn skills_delete(state: State<'_, AppState>, name: String) -> Result<(), String> {
    skills::delete(builtin_skills_dir(&state), &user_skills_dir(&state), &name)?;
    skills::clear_dependency_cache();
    // Drop any stored review alongside the skill.
    skill_review::drop_review(&state.db, &name).await?;
    Ok(())
}

/// 安装 pip 依赖到应用托管的隔离 conda 环境（siku-office）：环境已存在则
/// 只补装缺失包；没有环境则创建；连 conda 都没有则先下载 Miniconda 静默
/// 安装到应用数据目录（不写 PATH、不注册系统 Python）。整个过程不触碰
/// 用户本机的 Python 环境。进度经 `skills:env-install` 事件推给前端。
/// 返回给用户的简短结果说明。
#[tauri::command]
pub async fn skills_env_install(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
    packages: Vec<String>,
) -> Result<String, String> {
    if packages.is_empty() {
        return Err("没有需要安装的依赖包".to_string());
    }
    // 包名合法性白名单：拒绝任何可能拼出参数注入的字符。
    let legal = |p: &str| {
        !p.is_empty()
            && p.len() <= 64
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '[' | ']'))
    };
    if !packages.iter().all(|p| legal(p)) {
        return Err("依赖包名包含非法字符".to_string());
    }
    use tauri::Emitter;
    let cb: crate::core::skill_env::ProgressFn = Box::new(move |msg: &str| {
        let _ = app_handle.emit("skills:env-install", serde_json::json!({ "message": msg }));
    });
    let python = crate::core::skill_env::ensure_env(&state.app_data_dir, &packages, Some(&cb)).await?;
    skills::clear_dependency_cache();
    Ok(format!(
        "隔离环境已就绪（{}）：{}",
        python.display(),
        packages.join("、")
    ))
}

/// Per-package install for the plugins dialog. Unlike `skills_env_install`,
/// this returns one outcome per package so a bad inferred name cannot block
/// the whole batch.
#[tauri::command]
pub async fn skills_env_install_many(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
    packages: Vec<String>,
) -> Result<Vec<crate::core::skill_env::InstallOutcome>, String> {
    if packages.is_empty() {
        return Ok(Vec::new());
    }
    let legal = |p: &str| {
        !p.is_empty()
            && p.len() <= 64
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '[' | ']'))
    };
    if !packages.iter().all(|p| legal(p)) {
        return Err("依赖包名包含非法字符".to_string());
    }
    use tauri::Emitter;
    let cb: crate::core::skill_env::ProgressFn = Box::new(move |msg: &str| {
        let _ = app_handle.emit("skills:env-install", serde_json::json!({ "message": msg }));
    });
    let out = crate::core::skill_env::install_packages_individually(
        &state.app_data_dir,
        &packages,
        Some(&cb),
    )
    .await?;
    skills::clear_dependency_cache();
    Ok(out)
}

/// Python path of the isolated environment, for copy/manual-install escape.
#[tauri::command]
pub fn skills_env_python(state: State<'_, AppState>) -> Option<String> {
    crate::core::skill_env::resolve(&state.app_data_dir)
        .python
        .map(|p| p.to_string_lossy().to_string())
}

/// Persist a local ignore override for an inferred dependency. This is
/// device-local: it never edits the skill's SKILL.md and does not sync.
#[tauri::command]
pub async fn skills_set_dependency_ignored(
    state: State<'_, AppState>,
    skill: String,
    dependency: String,
    ignored: bool,
) -> Result<(), String> {
    let mut map = ignored_dependencies(&state.db).await;
    let names = map.entry(skill).or_default();
    if ignored {
        if !names.contains(&dependency) {
            names.push(dependency);
        }
    } else {
        names.retain(|n| n != &dependency);
    }
    let value = serde_json::to_string(&map).map_err(|e| format!("json: {e}"))?;
    crate::core::settings_service::set_device_setting(&state.db, IGNORED_DEPS_KEY, &value).await?;
    skills::clear_dependency_cache();
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
    crate::commands::agent::run_agent_turn(&state, &app_handle, session_id.clone(), prompt, None, None, None)
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

