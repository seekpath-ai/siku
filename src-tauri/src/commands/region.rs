use tauri::State;
use tracing::instrument;

use crate::ai::region_detection::service::{self, DetectedRegionOutput, RegionDetectionRequest};
use crate::AppState;

/// Detect structural regions on a PDF page using LLM layout analysis.
#[tauri::command]
#[instrument(skip(state))]
pub async fn detect_regions_llm(
    state: State<'_, AppState>,
    request: RegionDetectionRequest,
) -> Result<Vec<DetectedRegionOutput>, String> {
    service::detect_regions(&state.db, request).await
}
