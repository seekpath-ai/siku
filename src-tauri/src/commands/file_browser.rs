use tracing::instrument;
use crate::core::models::FileEntry;

#[tauri::command]
#[instrument]
pub async fn file_browser_list_dir(path: String, show_hidden: Option<bool>) -> Result<Vec<FileEntry>, String> {
    crate::core::file_service::list_dir(&path, show_hidden.unwrap_or(false))
}

#[tauri::command]
#[instrument]
pub async fn file_browser_get_info(path: String) -> Result<FileEntry, String> {
    crate::core::file_service::get_file_info(&path)
}

#[tauri::command]
#[instrument]
pub async fn file_browser_open_in_system(path: String) -> Result<(), String> {
    crate::core::file_service::open_in_system(&path)
}

#[tauri::command]
#[instrument]
pub async fn file_browser_reveal_in_system(path: String) -> Result<(), String> {
    crate::core::file_service::reveal_in_system(&path)
}

/// Read a text file's content (bounded by the file-read tool limit).
/// Used by the chat "attach file" feature to include file content as context.
#[tauri::command]
#[instrument]
pub async fn read_text_file(path: String) -> Result<String, String> {
    let content = std::fs::read_to_string(&path).map_err(|e| format!("read failed: {e}"))?;
    let limit = crate::core::settings_service::cached_settings()
        .tool_file_read_max_chars
        .max(1) as usize;
    let char_count = content.chars().count();
    let truncated: String = content.chars().take(limit).collect();
    if char_count > limit {
        Ok(format!(
            "{truncated}\n\n[...truncated at {limit} chars, original length: {char_count}]"
        ))
    } else {
        Ok(truncated)
    }
}

/// Read any document (PDF/office/text of any extension or encoding) as text
/// for chat attachment. Oversized extractions are cached under `cache_dir`
/// (the session working dir when set) so the agent can page them with
/// file_read. PDF/office run on a blocking thread — extraction is CPU-heavy.
#[tauri::command]
#[instrument(skip_all)]
pub async fn read_document_file(path: String, cache_dir: Option<String>) -> Result<String, String> {
    let path2 = std::path::PathBuf::from(&path);
    let cache = cache_dir
        .filter(|d| !d.trim().is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("siku-extracted"));
    tokio::task::spawn_blocking(move || {
        crate::core::document_text::read_document_text(&path2, &cache)
    })
    .await
    .map_err(|e| format!("task join: {e}"))?
}

/// Write text content to a file at an absolute path (e.g. note export).
#[tauri::command]
#[instrument]
pub async fn save_text_file(path: String, content: String) -> Result<(), String> {
    std::fs::write(&path, content).map_err(|e| format!("write failed: {e}"))
}
