use tauri::State;
use tracing::instrument;
use crate::AppState;
use crate::core::models::{Annotation, AnnotationInput, AnnotationRect, AnnotationSegment};
use crate::core::annotation_service;

#[tauri::command]
#[instrument(skip(state))]
pub async fn annotation_list(state: State<'_, AppState>, paper_id: String) -> Result<Vec<Annotation>, String> {
    annotation_service::list_by_paper(&state.db, &paper_id).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn annotation_create(
    state: State<'_, AppState>,
    id: Option<String>,
    paper_id: String,
    page: i64,
    x_ratio: f64,
    y_ratio: f64,
    width_ratio: f64,
    height_ratio: f64,
    text: Option<String>,
    note: Option<String>,
    tags: Vec<String>,
    rects: Option<String>, // optional JSON array of {x,y,w,h} per-line rects
    segments: Option<String>, // optional JSON array of AnnotationSegment (multi-range selection)
) -> Result<Annotation, String> {
    let parsed_rects = match rects {
        Some(json) if !json.trim().is_empty() => {
            let list: Vec<AnnotationRect> =
                serde_json::from_str(&json).map_err(|e| format!("rects json: {e}"))?;
            Some(list)
        }
        _ => None,
    };
    let parsed_segments = match segments {
        Some(json) if !json.trim().is_empty() => {
            let list: Vec<AnnotationSegment> =
                serde_json::from_str(&json).map_err(|e| format!("segments json: {e}"))?;
            Some(list)
        }
        _ => None,
    };
    let input = AnnotationInput {
        id,
        paper_id,
        page,
        rect: AnnotationRect {
            x: x_ratio,
            y: y_ratio,
            w: width_ratio,
            h: height_ratio,
            rects: parsed_rects,
            segments: parsed_segments,
        },
        text,
        note,
        tags,
    };
    annotation_service::create(&state.db, &input).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn annotation_update_note(state: State<'_, AppState>, id: String, note: String) -> Result<Annotation, String> {
    annotation_service::update_note(&state.db, &id, &note).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn annotation_update_tags(state: State<'_, AppState>, id: String, tags: Vec<String>) -> Result<Annotation, String> {
    annotation_service::update_tags(&state.db, &id, &tags).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn annotation_update_translation(state: State<'_, AppState>, id: String, translation: String) -> Result<Annotation, String> {
    annotation_service::update_translation(&state.db, &id, &translation).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn annotation_delete(state: State<'_, AppState>, id: String) -> Result<(), String> {
    annotation_service::delete(&state.db, &id).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn annotation_clear_paper(state: State<'_, AppState>, paper_id: String) -> Result<(), String> {
    annotation_service::clear_paper(&state.db, &paper_id).await
}
