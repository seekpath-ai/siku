pub mod paper_search;
pub mod library_search;
pub mod paper_read;
pub mod paper_snapshot;
pub mod paper_import;
pub mod note_read;
pub mod note_write;
pub mod web_fetch;
pub mod web_search;
pub mod translation;
pub mod knowledge;
pub mod knowledge_write;
pub mod memory;
pub mod file_ops;
pub mod file_write;
pub mod file_edit;
pub mod file_grep;
pub mod file_glob;
pub mod known_files;
pub mod bash;
pub mod tasks;
pub mod ask_user;
pub mod skill;
pub mod read_media_file;
pub mod path;

/// Write `content` to `target` atomically: write a sibling temp file, then
/// rename it over the target.
///
/// `fs::write` truncates before writing, so any failure in between (disk full,
/// cancelled turn, app killed, permission error) destroys the original and
/// leaves a half-written file in its place. A rename either happens or it
/// doesn't. The original permission bits are carried over because an agent
/// editing a `0755` script must not silently downgrade it to `0644`.
pub(crate) fn write_atomically(target: &std::path::Path, content: &str) -> Result<(), String> {
    use std::io::Write;

    let dir = target.parent().unwrap_or_else(|| std::path::Path::new("."));
    let name = target
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file");
    // Same directory (rename is only atomic within one filesystem), and a
    // distinctive name so a crash can never be mistaken for a user file.
    let tmp = dir.join(format!(".{name}.siku-{}.tmp", uuid::Uuid::new_v4().simple()));

    let original_perms = std::fs::metadata(target).ok().map(|m| m.permissions());

    let write = || -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(content.as_bytes())?;
        // Otherwise a crash right after the rename can publish an empty file.
        f.sync_all()
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("write failed: {e}"));
    }
    if let Some(perms) = original_perms {
        let _ = std::fs::set_permissions(&tmp, perms);
    }
    if let Err(e) = std::fs::rename(&tmp, target) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("replace failed: {e}"));
    }
    Ok(())
}

/// Resolve the proxy for web tools: the per-agent proxy wins; otherwise fall
/// back to the global (default provider) proxy.
pub(crate) async fn resolve_web_proxy(
    db: &sqlx::SqlitePool,
    web_proxy: Option<&str>,
) -> Option<String> {
    if let Some(p) = web_proxy.filter(|p| !p.is_empty()) {
        return Some(p.to_string());
    }
    crate::core::settings_service::load_llm_config(db)
        .await
        .ok()
        .and_then(|c| c.proxy)
        .filter(|p| !p.is_empty())
}

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
