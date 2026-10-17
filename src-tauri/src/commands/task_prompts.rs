//! 领域智能体 settings commands: list the system prompts behind the batch
//! features together with their defaults and current overrides.

use serde::Serialize;
use tauri::State;

use crate::core::task_prompts::{self, Placeholder};
use crate::AppState;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskPromptInfo {
    pub id: &'static str,
    /// Feature the prompt belongs to ("AI 综述" / "AI 表格列" / …).
    pub group: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    /// Built-in text, shown as the textarea placeholder when not overridden.
    pub default_prompt: &'static str,
    /// What is actually used right now (override or default).
    pub effective_prompt: String,
    /// True when a settings override is in effect.
    pub overridden: bool,
    pub placeholders: Vec<Placeholder>,
    /// Rewriting this prompt's JSON contract breaks the feature.
    pub affects_parsing: bool,
}

/// Every built-in task prompt with its default and current value. The settings
/// page renders one card per entry; overrides live in `prompt.<id>` settings
/// rows (already part of the synced `settings` table).
#[tauri::command]
pub async fn task_prompts_list(state: State<'_, AppState>) -> Result<Vec<TaskPromptInfo>, String> {
    let mut out = Vec::with_capacity(task_prompts::PROMPTS.len());
    for p in task_prompts::PROMPTS {
        let override_value = crate::core::settings_service::get_setting(
            &state.db,
            &task_prompts::setting_key(p.id),
        )
        .await
        .unwrap_or(None)
        .filter(|v| !v.trim().is_empty());
        let overridden = override_value.is_some();
        out.push(TaskPromptInfo {
            id: p.id,
            group: p.group,
            name: p.name,
            description: p.description,
            default_prompt: p.default,
            effective_prompt: override_value.unwrap_or_else(|| p.default.to_string()),
            overridden,
            placeholders: p.placeholders.to_vec(),
            affects_parsing: p.affects_parsing,
        });
    }
    Ok(out)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptGroupInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    /// Budget used when the field is empty.
    pub builtin_default: u32,
    /// What the user typed (None = automatic).
    pub override_value: Option<u32>,
    /// Budget actually in effect for a typical call.
    pub effective: u32,
}

/// Per-feature output budget ("单轮输出上限") rows for the settings page.
#[tauri::command]
pub async fn task_prompt_groups(
    state: State<'_, AppState>,
) -> Result<Vec<PromptGroupInfo>, String> {
    let mut out = Vec::with_capacity(task_prompts::GROUPS.len());
    for g in task_prompts::GROUPS {
        let floor = task_prompts::budget_floor(&state.db, g.id).await;
        out.push(PromptGroupInfo {
            id: g.id,
            name: g.name,
            description: g.description,
            builtin_default: g.builtin_default,
            override_value: floor,
            effective: task_prompts::apply_floor(g.builtin_default, floor),
        });
    }
    Ok(out)
}
