pub mod paper_search;
pub mod paper_read;
pub mod paper_import;
pub mod note_read;
pub mod note_write;
pub mod web_fetch;
pub mod web_search;
pub mod translation;
pub mod knowledge;
pub mod knowledge_write;
pub mod file_ops;
pub mod file_write;
pub mod file_edit;
pub mod file_grep;
pub mod file_glob;
pub mod bash;
pub mod tasks;
pub mod ask_user;
pub mod skill;
pub mod read_media_file;
pub mod path;
pub mod system;

/// Format the papers.authors / editor JSON-array column for display:
/// `["A","B"]` becomes "A, B". Falls back to the raw string when the
/// column is not a JSON array, and to "N/A" when the list is empty.
pub(crate) fn format_author_list(raw: &str) -> String {
    match serde_json::from_str::<Vec<String>>(raw) {
        Ok(list) if !list.is_empty() => list.join(", "),
        Ok(_) => "N/A".to_string(),
        Err(_) => raw.to_string(),
    }
}
