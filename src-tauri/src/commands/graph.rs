use tauri::State;
use tracing::instrument;
use crate::AppState;
use crate::core::graph_service::GraphData;

#[tauri::command]
#[instrument(skip(state))]
pub async fn graph_get(state: State<'_, AppState>) -> Result<GraphData, String> {
    crate::core::graph_service::build_graph(&state.db).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn graph_get_local(
    state: State<'_, AppState>,
    center_id: String,
    depth: Option<i32>,
) -> Result<GraphData, String> {
    crate::core::graph_service::build_local_graph(&state.db, &center_id, depth.unwrap_or(1)).await
}
