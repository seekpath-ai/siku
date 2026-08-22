use std::path::Path;

use tauri::State;
use tracing::{info, warn};

use crate::core::paper_service;
use crate::file_store;
use crate::AppState;

/// Copy a paper's PDF file to the user-selected target path.
#[tauri::command]
pub async fn export_pdf(
    state: State<'_, AppState>,
    paper_id: String,
    target_path: String,
) -> Result<(), String> {
    let paper = paper_service::get_paper(&state.db, &paper_id)
        .await
        .map_err(|e| e.to_string())?;

    let Some(rel_path) = paper.file_path.as_deref() else {
        return Err(format!("paper {} has no PDF file", paper_id));
    };

    let pdf_path = file_store::resolve_blob_path(&state.app_data_dir, rel_path);
    let src = Path::new(&pdf_path);

    if !src.exists() {
        warn!(paper_id = %paper_id, path = %pdf_path.display(), "PDF file not found");
        return Err(format!("PDF file not found: {}", pdf_path.display()));
    }

    std::fs::copy(src, &target_path)
        .map_err(|e| format!("failed to copy PDF: {}", e))?;

    info!(
        paper_id = %paper_id,
        src = %pdf_path.display(),
        target = %target_path,
        "export_pdf: copied PDF to target path"
    );

    Ok(())
}

/// Return the absolute filesystem path to a paper's PDF file.
///
/// The frontend converts this path to an asset-protocol URL via
/// `convertFileSrc()` from `@tauri-apps/api/core`. This avoids
/// sending binary data over IPC entirely — the webview loads the
/// PDF directly through Tauri's asset protocol, which handles
/// large files efficiently.
#[tauri::command]
pub async fn read_pdf_bytes(
    state: State<'_, AppState>,
    paper_id: String,
) -> Result<String, String> {
    let paper = paper_service::get_paper(&state.db, &paper_id)
        .await
        .map_err(|e| e.to_string())?;

    let Some(rel_path) = paper.file_path.as_deref() else {
        return Err(format!("paper {} has no PDF file", paper_id));
    };

    let pdf_path = file_store::resolve_blob_path(&state.app_data_dir, rel_path);
    let path = Path::new(&pdf_path);

    if !path.exists() {
        warn!(paper_id = %paper_id, path = %pdf_path.display(), "PDF file not found");
        return Err(format!("PDF file not found: {}", pdf_path.display()));
    }

    let metadata = std::fs::metadata(path)
        .map_err(|e| format!("failed to read PDF metadata: {}", e))?;

    info!(
        paper_id = %paper_id,
        path = %pdf_path.display(),
        file_size = metadata.len(),
        "read_pdf_bytes: returning file path for asset protocol"
    );

    Ok(pdf_path.display().to_string())
}
