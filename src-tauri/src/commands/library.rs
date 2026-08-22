use std::path::Path;

use tauri::State;
use tracing::instrument;

use crate::core::models::{ListPapersParams, Paper, PaperInput};
use crate::core::paper_service;
use crate::file_store;
use crate::AppState;

#[tauri::command]
#[instrument(skip(state))]
pub async fn import_paper(
    state: State<'_, AppState>,
    file_path: String,
) -> Result<Paper, String> {
    paper_service::import_paper(&state.db, &state.app_data_dir, Path::new(&file_path))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn preview_paper_from_link(
    state: State<'_, AppState>,
    url: String,
) -> Result<crate::core::link_import::PaperMetadata, crate::core::link_import::LinkImportError> {
    crate::core::link_import::resolve_paper_link(&state.db, url).await
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn import_paper_from_link(
    state: State<'_, AppState>,
    url: String,
    metadata: Option<crate::core::link_import::PaperMetadata>,
) -> Result<crate::core::link_import::PaperImportResult, crate::core::link_import::LinkImportError> {
    crate::core::link_import::import_paper_from_link(&state.db, &state.app_data_dir, url, metadata)
        .await
}

/// Rebuild a paper's text index (extract → chunk → embed). Idempotent.
#[tauri::command]
#[instrument(skip(state))]
pub async fn paper_reprocess_index(
    state: State<'_, AppState>,
    id: String,
) -> Result<usize, String> {
    let paper = paper_service::get_paper(&state.db, &id)
        .await
        .map_err(|e| e.to_string())?;
    paper_service::reprocess_paper_index(&state.db, &state.app_data_dir, &paper)
        .await
        .map_err(|e| e.to_string())
}

/// Enrich a paper's bibliographic metadata from CrossRef (DOI / title).
/// Returns true when fields were filled.
#[tauri::command]
#[instrument(skip(state))]
pub async fn paper_enrich_metadata(
    state: State<'_, AppState>,
    id: String,
) -> Result<bool, String> {
    paper_service::enrich_paper_metadata(&state.db, &state.app_data_dir, &id).await
}

#[tauri::command]
pub async fn list_papers(
    state: State<'_, AppState>,
    params: ListPapersParams,
) -> Result<Vec<Paper>, String> {
    paper_service::list_papers(&state.db, params)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_paper(
    state: State<'_, AppState>,
    id: String,
) -> Result<Paper, String> {
    paper_service::get_paper(&state.db, &id)
        .await
        .map_err(|e| e.to_string())
}

/// Open a paper's PDF with the system default application.
///
/// `papers.file_path` stores a blob-relative path (e.g. `blobs/<hash>.pdf`),
/// so it MUST be resolved against the app data dir first — passing the raw
/// relative path to the OS fails with "file not found" because the app's
/// working directory is not the app data dir.
#[tauri::command]
pub async fn open_paper_in_system(
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    let paper = paper_service::get_paper(&state.db, &id)
        .await
        .map_err(|e| e.to_string())?;
    let Some(rel_path) = paper.file_path.as_deref() else {
        return Err(format!("paper {id} has no PDF file"));
    };
    let abs = file_store::resolve_blob_path(&state.app_data_dir, rel_path);
    let p = Path::new(&abs);
    if !p.exists() {
        return Err(format!("PDF file not found: {}", abs.display()));
    }
    crate::core::file_service::open_in_system(&abs.display().to_string())
}

/// Reveal a paper's PDF in the system file manager (selecting the file).
/// Same blob-relative → absolute path resolution as [`open_paper_in_system`].
#[tauri::command]
pub async fn reveal_paper_in_system(
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    let paper = paper_service::get_paper(&state.db, &id)
        .await
        .map_err(|e| e.to_string())?;
    let Some(rel_path) = paper.file_path.as_deref() else {
        return Err(format!("paper {id} has no PDF file"));
    };
    let abs = file_store::resolve_blob_path(&state.app_data_dir, rel_path);
    let p = Path::new(&abs);
    if !p.exists() {
        return Err(format!("PDF file not found: {}", abs.display()));
    }
    crate::core::file_service::reveal_in_system(&abs.display().to_string())
}

/// Find papers that are likely duplicates of the given paper (DOI exact /
/// normalized-title equality). Returns candidates plus the match reason.
#[tauri::command]
pub async fn paper_find_duplicates(
    state: State<'_, AppState>,
    id: String,
) -> Result<Vec<crate::core::paper_service::DuplicateCandidate>, String> {
    let paper = paper_service::get_paper(&state.db, &id).await?;
    paper_service::find_duplicate_papers(&state.db, &paper.title, paper.doi.as_deref(), &id)
        .await
        .map_err(|e| e.to_string())
}

/// Merge `remove_id` into `keep_id` (metadata fill + children transfer + delete).
#[tauri::command]
pub async fn paper_merge(
    state: State<'_, AppState>,
    keep_id: String,
    remove_id: String,
) -> Result<(), String> {
    paper_service::merge_papers(&state.db, &state.app_data_dir, &keep_id, &remove_id)
        .await
        .map_err(|e| e.to_string())
}

/// Restore a trashed paper (soft delete → active).
#[tauri::command]
pub async fn paper_restore(
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    paper_service::restore_paper(&state.db, &id)
        .await
        .map_err(|e| e.to_string())
}

/// Permanently delete a trashed paper and its unreferenced files.
#[tauri::command]
pub async fn paper_purge(
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    paper_service::purge_paper(&state.db, &state.app_data_dir, &id)
        .await
        .map_err(|e| e.to_string())
}

/// Export the given papers as citation text. `format` is one of
/// "bibtex" | "ris" | "csl-json".
#[tauri::command]
pub async fn paper_export(
    state: State<'_, AppState>,
    ids: Vec<String>,
    format: String,
) -> Result<String, String> {
    let mut papers = Vec::with_capacity(ids.len());
    for id in &ids {
        papers.push(paper_service::get_paper(&state.db, id).await?);
    }
    Ok(match format.as_str() {
        "ris" => papers
            .iter()
            .map(crate::core::citation_export::ris_for_paper)
            .collect(),
        "csl-json" => crate::core::citation_export::csl_json_for_papers(&papers),
        _ => papers
            .iter()
            .map(crate::core::citation_export::bibtex_for_paper)
            .collect(),
    })
}

/// Set the favorite flag of a paper.
#[tauri::command]
pub async fn paper_set_favorite(
    state: State<'_, AppState>,
    id: String,
    favorite: bool,
) -> Result<(), String> {
    paper_service::set_paper_favorite(&state.db, &id, favorite)
        .await
        .map_err(|e| e.to_string())
}

/// Set the read status of a paper: "unread" | "read" | "in_progress".
#[tauri::command]
pub async fn paper_set_read_status(
    state: State<'_, AppState>,
    id: String,
    status: String,
) -> Result<(), String> {
    paper_service::set_paper_read_status(&state.db, &id, &status)
        .await
        .map_err(|e| e.to_string())
}

/// Record that a paper was opened in the reader. Updates last_read_at and
/// promotes an "unread" paper to "in_progress".
#[tauri::command]
pub async fn paper_record_read(
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    paper_service::record_paper_read(&state.db, &id)
        .await
        .map_err(|e| e.to_string())
}

/// Link two papers bidirectionally.
#[tauri::command]
pub async fn paper_add_related(
    state: State<'_, AppState>,
    paper_id: String,
    related_id: String,
) -> Result<(), String> {
    paper_service::add_related_paper(&state.db, &paper_id, &related_id)
        .await
        .map_err(|e| e.to_string())
}

/// Remove the link between two papers.
#[tauri::command]
pub async fn paper_remove_related(
    state: State<'_, AppState>,
    paper_id: String,
    related_id: String,
) -> Result<(), String> {
    paper_service::remove_related_paper(&state.db, &paper_id, &related_id)
        .await
        .map_err(|e| e.to_string())
}

/// List papers related to the given paper.
#[tauri::command]
pub async fn paper_list_related(
    state: State<'_, AppState>,
    id: String,
) -> Result<Vec<Paper>, String> {
    paper_service::list_related_papers(&state.db, &id)
        .await
        .map_err(|e| e.to_string())
}

/// List saved searches.
#[tauri::command]
pub async fn saved_searches_list(
    state: State<'_, AppState>,
) -> Result<Vec<crate::core::models::SavedSearch>, String> {
    paper_service::list_saved_searches(&state.db)
        .await
        .map_err(|e| e.to_string())
}

/// Save the current search params under a name.
#[tauri::command]
pub async fn saved_searches_create(
    state: State<'_, AppState>,
    name: String,
    params_json: String,
) -> Result<crate::core::models::SavedSearch, String> {
    paper_service::create_saved_search(&state.db, &name, &params_json)
        .await
        .map_err(|e| e.to_string())
}

/// Delete a saved search.
#[tauri::command]
pub async fn saved_searches_delete(
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    paper_service::delete_saved_search(&state.db, &id)
        .await
        .map_err(|e| e.to_string())
}

/// Get structured creators for a paper (falls back to legacy columns).
#[tauri::command]
pub async fn paper_get_creators(
    state: State<'_, AppState>,
    id: String,
) -> Result<Vec<crate::core::paper_service::Creator>, String> {
    paper_service::get_creators(&state.db, &id)
        .await
        .map_err(|e| e.to_string())
}

/// Replace structured creators and regenerate the legacy authors/editor
/// columns (the sync transport).
#[tauri::command]
pub async fn paper_set_creators(
    state: State<'_, AppState>,
    id: String,
    creators: Vec<crate::core::paper_service::Creator>,
) -> Result<(), String> {
    paper_service::set_creators(&state.db, &id, &creators)
        .await
        .map_err(|e| e.to_string())
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AttachmentInfo {
    pub id: String,
    pub paper_id: String,
    pub file_name: String,
    /// Blob-relative path (e.g. `blobs/<hash>.ext`).
    pub file_path: String,
    pub file_type: String,
    pub created_at: String,
}

/// List a paper's attachments (the main PDF is included via its row).
#[tauri::command]
pub async fn paper_list_attachments(
    state: State<'_, AppState>,
    id: String,
) -> Result<Vec<AttachmentInfo>, String> {
    let rows = sqlx::query_as::<_, (String, String, String, String, String, String)>(
        "SELECT id, paper_id, file_name, file_path, file_type, created_at \
         FROM attachments WHERE paper_id = ? ORDER BY created_at ASC",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|(id, paper_id, file_name, file_path, file_type, created_at)| AttachmentInfo {
            id,
            paper_id,
            file_name,
            file_path,
            file_type,
            created_at,
        })
        .collect())
}

/// Add an attachment by copying an existing file into the blob store.
#[tauri::command]
pub async fn paper_add_attachment(
    state: State<'_, AppState>,
    paper_id: String,
    source_path: String,
) -> Result<AttachmentInfo, String> {
    let rel = crate::file_store::copy_file_to_blob(
        &state.app_data_dir,
        std::path::Path::new(&source_path),
    )
    .map_err(|e| e.to_string())?;
    let file_name = std::path::Path::new(&source_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "attachment".to_string());
    let file_type = std::path::Path::new(&source_path)
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_else(|| "bin".to_string());
    let id = uuid::Uuid::new_v4().to_string();
    let now = crate::core::time::now_iso();
    sqlx::query(
        "INSERT INTO attachments (id, paper_id, file_name, file_path, file_type, created_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&paper_id)
    .bind(&file_name)
    .bind(&rel)
    .bind(&file_type)
    .bind(&now)
    .execute(&state.db)
    .await
    .map_err(|e| e.to_string())?;
    Ok(AttachmentInfo {
        id,
        paper_id,
        file_name,
        file_path: rel,
        file_type,
        created_at: now,
    })
}

/// Remove an attachment row and its blob (kept when still referenced).
#[tauri::command]
pub async fn paper_remove_attachment(
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT paper_id, file_path FROM attachments WHERE id = ?")
            .bind(&id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| e.to_string())?;
    let Some((paper_id, rel_path)) = row else {
        return Ok(());
    };
    sqlx::query("DELETE FROM attachments WHERE id = ?")
        .bind(&id)
        .execute(&state.db)
        .await
        .map_err(|e| e.to_string())?;
    // Remove the blob only when nothing references it any more.
    let blob_path = crate::file_store::resolve_blob_path(&state.app_data_dir, &rel_path);
    if blob_path.exists() {
        let count: i64 = sqlx::query_scalar(
            "SELECT (SELECT COUNT(*) FROM papers WHERE file_path = ?) + \
             (SELECT COUNT(*) FROM attachments WHERE file_path = ?)",
        )
        .bind(&rel_path)
        .bind(&rel_path)
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);
        if count == 0 {
            let _ = std::fs::remove_file(&blob_path);
        }
    }
    let _ = paper_id;
    Ok(())
}

/// Open an attachment with the system default application (resolves the
/// blob-relative path to an absolute one first).
#[tauri::command]
pub async fn paper_open_attachment(
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    let rel: Option<(String,)> =
        sqlx::query_as("SELECT file_path FROM attachments WHERE id = ?")
            .bind(&id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| e.to_string())?;
    let Some((rel,)) = rel else {
        return Err("附件不存在".into());
    };
    let abs = crate::file_store::resolve_blob_path(&state.app_data_dir, &rel);
    if !abs.exists() {
        return Err(format!("附件文件不存在: {}", abs.display()));
    }
    crate::core::file_service::open_in_system(&abs.display().to_string())
}

/// Export a paper's annotations as Markdown (highlight + note + translation).
#[tauri::command]
pub async fn paper_export_annotations(
    state: State<'_, AppState>,
    id: String,
) -> Result<String, String> {
    let rows = sqlx::query_as::<_, (i64, String, Option<String>, Option<String>, Option<String>)>(
        "SELECT page, type, text, note, translation FROM annotations \
         WHERE paper_id = ? ORDER BY page ASC, created_at ASC",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| e.to_string())?;
    let mut out = String::from("# 标注\n\n");
    for (page, kind, text, note, translation) in rows {
        let kind_label = match kind.as_str() {
            "highlight" => "高亮",
            "underline" => "下划线",
            "note" => "笔记",
            _ => kind.as_str(),
        };
        if let Some(t) = text.as_deref().filter(|t| !t.is_empty()) {
            out.push_str(&format!("- 第 {page} 页（{kind_label}）：{t}\n"));
        } else if let Some(n) = note.as_deref().filter(|n| !n.is_empty()) {
            out.push_str(&format!("- 第 {page} 页（笔记）：{n}\n"));
        }
        if let Some(n) = note.as_deref().filter(|n| !n.is_empty()) {
            out.push_str(&format!("  - 笔记：{n}\n"));
        }
        if let Some(tr) = translation.as_deref().filter(|t| !t.is_empty()) {
            out.push_str(&format!("  - 翻译：{tr}\n"));
        }
    }
    if out == "# 标注\n\n" {
        out.push_str("（暂无标注）\n");
    }
    Ok(out)
}

#[tauri::command]
pub async fn update_paper(
    state: State<'_, AppState>,
    id: String,
    input: PaperInput,
) -> Result<Paper, String> {
    paper_service::update_paper(&state.db, &id, input)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_paper(
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    paper_service::delete_paper(&state.db, &state.app_data_dir, &id)
        .await
        .map_err(|e| e.to_string())
}

/// Import metadata from a BibTeX entry into a paper: parsed fields override
/// the current values, missing fields are kept, and the raw BibTeX is stored.
#[tauri::command]
#[instrument(skip(state))]
pub async fn paper_import_bibtex(
    state: State<'_, AppState>,
    paper_id: String,
    bibtex: String,
) -> Result<Paper, String> {
    use crate::core::bibtex;

    let entries = bibtex::parse_bibtex(&bibtex);
    let entry = entries
        .into_iter()
        .next()
        .ok_or_else(|| "未找到有效的 BibTeX 条目".to_string())?;

    let current = paper_service::get_paper(&state.db, &paper_id)
        .await
        .map_err(|e| e.to_string())?;

    let input = PaperInput {
        title: entry
            .field(&["title"])
            .unwrap_or(&current.title)
            .to_string(),
        authors: entry
            .field(&["author"])
            .map(bibtex::split_authors)
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| serde_json::from_str(&current.authors).unwrap_or_default()),
        year: entry
            .field(&["year"])
            .and_then(|y| y.trim().parse::<i32>().ok())
            .or(current.year),
        journal: entry
            .field(&["journal", "booktitle"])
            .map(|s| s.to_string())
            .or_else(|| current.journal.clone()),
        doi: entry
            .field(&["doi"])
            .map(|s| s.to_string())
            .or_else(|| current.doi.clone()),
        url: entry
            .field(&["url"])
            .map(|s| s.to_string())
            .or_else(|| current.url.clone()),
        abstract_text: entry
            .field(&["abstract"])
            .map(|s| s.to_string())
            .or_else(|| current.abstract_text.clone()),
        keywords: entry
            .field(&["keywords"])
            .map(|s| {
                s.split(',')
                    .map(|k| k.trim().to_string())
                    .filter(|k| !k.is_empty())
                    .collect()
            })
            .filter(|v: &Vec<String>| !v.is_empty())
            .unwrap_or_else(|| serde_json::from_str(&current.keywords).unwrap_or_default()),
        item_type: if !entry.entry_type.is_empty() {
            Some(bibtex::map_item_type(&entry.entry_type).to_string())
        } else {
            current.item_type.clone()
        },
        volume: entry
            .field(&["volume"])
            .map(|s| s.to_string())
            .or_else(|| current.volume.clone()),
        issue: entry
            .field(&["number", "issue"])
            .map(|s| s.to_string())
            .or_else(|| current.issue.clone()),
        pages: entry
            .field(&["pages"])
            .map(|s| s.to_string())
            .or_else(|| current.pages.clone()),
        conference_name: entry
            .field(&["booktitle", "conference"])
            .map(|s| s.to_string())
            .or_else(|| current.conference_name.clone()),
        publisher: entry
            .field(&["publisher"])
            .map(|s| s.to_string())
            .or_else(|| current.publisher.clone()),
        place: entry
            .field(&["address", "place"])
            .map(|s| s.to_string())
            .or_else(|| current.place.clone()),
        editor: entry
            .field(&["editor"])
            .map(bibtex::split_authors)
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| serde_json::from_str(&current.editor).unwrap_or_default()),
        series: entry
            .field(&["series"])
            .map(|s| s.to_string())
            .or_else(|| current.series.clone()),
        edition: entry
            .field(&["edition"])
            .map(|s| s.to_string())
            .or_else(|| current.edition.clone()),
        isbn: entry
            .field(&["isbn"])
            .map(|s| s.to_string())
            .or_else(|| current.isbn.clone()),
        issn: entry
            .field(&["issn"])
            .map(|s| s.to_string())
            .or_else(|| current.issn.clone()),
        language: entry
            .field(&["language"])
            .map(|s| s.to_string())
            .or_else(|| current.language.clone()),
        num_pages: entry
            .field(&["numpages"])
            .and_then(|s| s.trim().parse::<i32>().ok())
            .or(current.num_pages),
        archive_location: entry
            .field(&["archiveprefix", "archive"])
            .map(|s| s.to_string())
            .or_else(|| current.archive_location.clone()),
        call_number: entry
            .field(&["callnumber"])
            .map(|s| s.to_string())
            .or_else(|| current.call_number.clone()),
        rights: entry
            .field(&["copyright", "rights"])
            .map(|s| s.to_string())
            .or_else(|| current.rights.clone()),
    };

    let paper = paper_service::update_paper(&state.db, &paper_id, input)
        .await
        .map_err(|e| e.to_string())?;

    // Store the raw BibTeX entry for later export/viewing.
    sqlx::query("UPDATE papers SET bibtex = ?, updated_at = ? WHERE id = ?")
        .bind(&bibtex)
        .bind(crate::core::time::now_iso())
        .bind(&paper_id)
        .execute(&state.db)
        .await
        .map_err(|e| format!("db: {e}"))?;

    paper_service::get_paper(&state.db, &paper_id)
        .await
        .map_err(|e| e.to_string())
}
