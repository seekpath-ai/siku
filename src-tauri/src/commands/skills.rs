use tauri::State;
use tracing::instrument;

use crate::core::skills::{self, SkillInfo};
use crate::AppState;

/// Default user skills directory: {app_data_dir}/skills.
fn user_skills_dir(state: &AppState) -> std::path::PathBuf {
    state.app_data_dir.join("skills")
}

/// List installed external skills for the plugins dialog.
#[tauri::command]
#[instrument(skip(state))]
pub async fn skills_list(state: State<'_, AppState>) -> Result<Vec<SkillInfo>, String> {
    Ok(skills::list_all(&user_skills_dir(&state)))
}

/// Full skill detail (incl. SKILL.md body) for the detail dialog.
#[tauri::command]
#[instrument(skip(state))]
pub async fn skills_get(state: State<'_, AppState>, name: String) -> Result<SkillDetail, String> {
    let (skill, path) =
        skills::get(&user_skills_dir(&state), &name).ok_or(format!("技能「{name}」不存在"))?;
    Ok(SkillDetail {
        name: skill.name,
        description: skill.description,
        content: skill.content,
        path,
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
    skills::delete(&user_skills_dir(&state), &name)
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

