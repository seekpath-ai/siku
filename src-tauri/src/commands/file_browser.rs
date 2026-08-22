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

/// Write text content to a file at an absolute path (e.g. note export).
#[tauri::command]
#[instrument]
pub async fn save_text_file(path: String, content: String) -> Result<(), String> {
    std::fs::write(&path, content).map_err(|e| format!("write failed: {e}"))
}
