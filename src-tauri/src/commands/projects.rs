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

/// Delete a project (its sessions lose the project reference)
#[tauri::command]
#[instrument(skip(state))]
pub async fn project_delete(state: State<'_, AppState>, id: String) -> Result<(), String> {
    crate::core::project_service::delete(&state.db, &id).await
}
