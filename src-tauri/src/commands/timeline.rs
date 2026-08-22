use tauri::State;
use tracing::instrument;

use crate::AppState;
use crate::core::timeline_service::{self, TimelineItem};

/// List recent cross-module activities, newest first.
#[tauri::command]
#[instrument(skip(state))]
pub async fn timeline_list(
    state: State<'_, AppState>,
    limit: Option<i64>,
    offset: Option<i64>,
    module: Option<String>,
) -> Result<Vec<TimelineItem>, String> {
    timeline_service::list_timeline(
        &state.db,
        limit.unwrap_or(50),
        offset.unwrap_or(0),
        module,
    )
    .await
}
