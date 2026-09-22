use tauri::State;
use tracing::instrument;

use crate::AppState;
use crate::core::models::{Project, ProjectInput};

/// List all projects
#[tauri::command]
#[instrument(skip(state))]
pub async fn projects_list(state: State<'_, AppState>) -> Result<Vec<Project>, String> {
    crate::core::project_service::list(&state.db).await
}

/// Create a project from a local folder path
#[tauri::command]
#[instrument(skip(state))]
pub async fn project_create(
    state: State<'_, AppState>,
    input: ProjectInput,
) -> Result<Project, String> {
    crate::core::project_service::create(&state.db, input).await
}

/// Rename a project
#[tauri::command]
#[instrument(skip(state))]
pub async fn project_update(
    state: State<'_, AppState>,
    id: String,
    input: ProjectInput,
) -> Result<Project, String> {
    crate::core::project_service::update(&state.db, &id, input).await
}

/// Archive or restore a project. Archived projects hide from the sidebar;
/// their sessions are untouched.
#[tauri::command]
#[instrument(skip(state))]
pub async fn project_set_archived(
    state: State<'_, AppState>,
    id: String,
    archived: bool,
) -> Result<(), String> {
    crate::core::project_service::set_archived(&state.db, &id, archived).await
}

/// Delete a project (its sessions lose the project reference)
#[tauri::command]
#[instrument(skip(state))]
pub async fn project_delete(state: State<'_, AppState>, id: String) -> Result<(), String> {
    crate::core::project_service::delete(&state.db, &id).await
}

/// Whether the system git binary is available (drives the new-project
/// dialog's "初始化 git 仓库" checkbox state).
#[tauri::command]
#[instrument(skip_all)]
pub fn git_available() -> bool {
    crate::core::project_service::git_available()
}
