use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tracing::{info, instrument, warn};
use uuid::Uuid;

use crate::core::error::{Result, SikuError};
use crate::core::models::{ListPapersParams, Paper, PaperInput};
use crate::core::time::now_iso;
use crate::file_store;
use crate::pdf;
use crate::pdf::chunker::ChunkConfig;

/// Metadata used to create or finalize a paper import, whether the source is a
/// local PDF, a downloaded PDF, or a metadata-only link.
#[derive(Debug, Clone, Default)]
pub struct ImportMetadata {
    pub title: String,
    pub authors: Vec<String>,
    pub year: Option<i32>,
    pub journal: Option<String>,
    pub doi: Option<String>,
    pub url: Option<String>,
    pub abstract_text: Option<String>,
    pub keywords: Vec<String>,
    pub file_path: Option<String>,
    pub file_size: Option<i64>,
    pub page_count: Option<i32>,
    pub language: Option<String>,
    pub isbn: Option<String>,
}

/// Common finalization step for all import paths: insert the paper record,
/// create the attachment (if a PDF is available), generate thumbnail, extract
/// text/chunks, and spawn metadata enrichment.
#[instrument(skip(db, app_data_dir, metadata), fields(paper_id = %paper_id))]
pub async fn finalize_paper_import(
    db: &SqlitePool,
    app_data_dir: &Path,
    paper_id: String,
    file_name: &str,
    metadata: ImportMetadata,
) -> Result<Paper> {
    let now = now_iso();
    let authors_json = serde_json::to_string(&metadata.authors).unwrap_or_else(|_| "[]".to_string());
    let keywords_json = serde_json::to_string(&metadata.keywords).unwrap_or_else(|_| "[]".to_string());
    let language = metadata.language.unwrap_or_else(|| "en".to_string());

    // Insert paper record
    let paper: Paper = sqlx::query_as(
        r#"INSERT INTO papers (
            id, title, authors, year, journal, doi, url, abstract, keywords,
            file_path, file_size, page_count, language, isbn,
            created_at, updated_at, imported_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?,
            ?, ?, ?, ?, ?,
            ?, ?, ?)
        RETURNING id, title, authors, year, journal, doi, url, abstract, keywords,
            citation_key, bibtex, file_path, file_size, page_count, language,
            item_type, volume, issue, pages, conference_name, publisher, place, editor,
            series, edition, isbn, issn, num_pages, archive_location, call_number, rights,
            deleted_at, is_favorite, read_status, last_read_at, created_at, updated_at, imported_at"#,
    )
    .bind(&paper_id)
    .bind(&metadata.title)
    .bind(&authors_json)
    .bind(metadata.year)
    .bind(&metadata.journal)
    .bind(&metadata.doi)
    .bind(&metadata.url)
    .bind(&metadata.abstract_text)
    .bind(&keywords_json)
    .bind(&metadata.file_path)
    .bind(metadata.file_size)
    .bind(metadata.page_count)
    .bind(&language)
    .bind(&metadata.isbn)
    .bind(&now)
    .bind(&now)
    .bind(&now)
    .fetch_one(db)
    .await?;

    // Create attachment record only when a PDF is present.
    if let Some(ref path) = metadata.file_path {
        let attachment_id = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO attachments (id, paper_id, file_name, file_path, file_type, created_at) VALUES (?, ?, ?, ?, 'pdf', ?)",
        )
        .bind(&attachment_id)
        .bind(&paper_id)
        .bind(file_name)
        .bind(path)
        .bind(&now)
        .execute(db)
        .await?;
    }

    // Auto-create knowledge_item linking to the research domain
    let ki_id = Uuid::new_v4().to_string();
    let _ = sqlx::query(
        "INSERT OR IGNORE INTO knowledge_items (id, domain_id, title, content_type, content, source_type, source_id, tags, metadata, created_at, updated_at)
         VALUES (?, 'dom-research', ?, 'paper_ref', NULL, 'paper', ?, '[]', '{}', ?, ?)"
    )
    .bind(&ki_id)
    .bind(&metadata.title)
    .bind(&paper_id)
    .bind(&now)
    .bind(&now)
    .execute(db)
    .await;

    // Deep PDF parsing, only when a PDF is available.
    if let Some(ref pdf_rel_path) = metadata.file_path {
        let pdf_path = file_store::resolve_blob_path(app_data_dir, pdf_rel_path);

        // Generate thumbnail (best-effort)
        let thumbnail_path = file_store::thumbnail_path(app_data_dir, &paper_id);
        if let Err(e) = pdf::renderer::render_thumbnail(&pdf_path, &thumbnail_path) {
            warn!(
                paper_id = %paper_id,
                error = %e,
                "failed to generate thumbnail"
            );
        }

        // Extract full text and chunk
        match pdf::extractor::extract_text(&pdf_path) {
            Ok(pages) => {
                if !pages.is_empty() {
                    info!(
                        paper_id = %paper_id,
                        page_count = pages.len(),
                        "text extraction completed"
                    );

                    // Update page count if metadata didn't have it
                    if metadata.page_count.is_none() {
                        let _ = sqlx::query("UPDATE papers SET page_count = ?, updated_at = ? WHERE id = ?")
                            .bind(pages.len() as i32)
                            .bind(crate::core::time::now_iso())
                            .bind(&paper_id)
                            .execute(db)
                            .await;
                    }

                    match insert_chunks_for_paper(db, &paper_id, &pages).await {
                        Ok(n) => info!(paper_id = %paper_id, chunk_count = n, "text chunking completed"),
                        Err(e) => warn!(paper_id = %paper_id, error = %e, "chunking failed"),
                    }
                }
            }
            Err(e) => {
                warn!(
                    paper_id = %paper_id,
                    error = %e,
                    "text extraction failed, skipping chunking"
                );
            }
        }
    }

    // Enrich bibliographic metadata from CrossRef in the background
    let db_clone = db.clone();
    let app_data_dir = app_data_dir.to_path_buf();
    let pid = paper_id.clone();
    tokio::spawn(async move {
        match enrich_paper_metadata(&db_clone, &app_data_dir, &pid).await {
            Ok(true) => info!(paper_id = %pid, "metadata enriched from CrossRef"),
            Ok(false) => info!(paper_id = %pid, "no metadata enrichment available"),
            Err(e) => warn!(paper_id = %pid, error = %e, "metadata enrichment failed"),
        }
    });

    info!(
        paper_id = %paper_id,
        title = %metadata.title,
        pages = ?metadata.page_count,
        "paper import completed"
    );

    Ok(paper)
}

/// Full import pipeline: copy file, extract metadata, create records.
#[instrument(skip(db, app_data_dir, source_path), fields(paper_id))]
pub async fn import_paper(
    db: &SqlitePool,
    app_data_dir: &Path,
    source_path: &Path,
) -> Result<Paper> {
    let paper_id = Uuid::new_v4().to_string();
    let import_id = Uuid::new_v4().to_string();
    let now = now_iso();

    // Validate source exists
    if !source_path.exists() {
        return Err(SikuError::NotFound(format!(
            "source file not found: {}",
            source_path.display()
        )));
    }

    let file_name = source_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown.pdf".to_string());

    // Create import record (status: processing)
    sqlx::query(
        "INSERT INTO imports (id, file_path, source_url, status, created_at) VALUES (?, ?, NULL, 'processing', ?)",
    )
    .bind(&import_id)
    .bind(source_path.to_string_lossy().to_string())
    .bind(&now)
    .execute(db)
    .await?;

    // Copy PDF to content-addressed blob storage
    let blob_rel_path = file_store::copy_file_to_blob(app_data_dir, source_path)?;
    let file_size = std::fs::metadata(source_path)
        .map(|m| m.len() as i64)
        .unwrap_or(0);

    info!(
        paper_id = %paper_id,
        file_size = file_size,
        "PDF copied to managed storage"
    );

    // Extract metadata
    let extracted = pdf::parser::extract_metadata(source_path).unwrap_or_else(|e| {
        warn!("metadata extraction failed, using defaults: {}", e);
        pdf::parser::PdfMetadata {
            title: Some(file_name.trim_end_matches(".pdf").to_string()),
            authors: vec![],
            subject: None,
            keywords: vec![],
            page_count: 0,
        }
    });

    let title = extracted.title.unwrap_or_else(|| {
        file_name
            .trim_end_matches(".pdf")
            .trim_end_matches(".PDF")
            .to_string()
    });

    let metadata = ImportMetadata {
        title,
        authors: extracted.authors,
        year: None,
        journal: None,
        doi: None,
        url: None,
        abstract_text: extracted.subject,
        keywords: extracted.keywords,
        file_path: Some(blob_rel_path),
        file_size: Some(file_size),
        page_count: if extracted.page_count > 0 {
            Some(extracted.page_count as i32)
        } else {
            None
        },
        language: None,
        isbn: None,
    };

    let paper = finalize_paper_import(db, app_data_dir, paper_id, &file_name, metadata).await?;

    // Update import record to completed
    sqlx::query("UPDATE imports SET status = 'completed', paper_id = ?, completed_at = ? WHERE id = ?")
        .bind(&paper.id)
        .bind(&now)
        .bind(&import_id)
        .execute(db)
        .await?;

    Ok(paper)
}

/// Insert chunk rows inside an existing transaction.
async fn insert_chunks_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    paper_id: &str,
    chunks: &[pdf::chunker::ChunkData],
    now: &str,
) -> Result<()> {
    for chunk in chunks {
        let chunk_id = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO chunks (id, paper_id, content, page_start, page_end, section, chunk_index, token_count, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&chunk_id)
        .bind(paper_id)
        .bind(&chunk.content)
        .bind(chunk.page_start)
        .bind(chunk.page_end)
        .bind(&chunk.section)
        .bind(chunk.chunk_index)
        .bind(chunk.token_count)
        .bind(now)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// Kick off embedding generation in the background. The embedder picks up
/// whatever chunks of the paper still lack vectors, so it is safe to call
/// this after any chunk rewrite.
fn spawn_embedding_task(db: &SqlitePool, paper_id: &str) {
    let db_clone = db.clone();
    let pid = paper_id.to_string();
    tokio::spawn(async move {
        match crate::ai::embedder::generate_embeddings_for_paper(&db_clone, &pid).await {
            Ok(n) => info!(paper_id = %pid, count = n, "embeddings generated"),
            Err(e) => warn!(paper_id = %pid, error = %e, "embedding generation failed"),
        }
    });
}

/// Chunk extracted pages, persist the chunks (in one transaction), and kick
/// off embedding generation in the background. Returns the number of chunks
/// written. Does NOT delete existing chunks — callers handle cleanup as
/// needed.
#[instrument(skip(db))]
pub async fn insert_chunks_for_paper(
    db: &SqlitePool,
    paper_id: &str,
    pages: &[crate::pdf::extractor::PageText],
) -> Result<usize> {
    let config = ChunkConfig::default();
    let chunks = pdf::chunker::chunk_pages(pages, &config);
    if chunks.is_empty() {
        return Ok(0);
    }

    let now = now_iso();
    let mut tx = db.begin().await?;
    insert_chunks_tx(&mut tx, paper_id, &chunks, &now).await?;
    tx.commit().await?;

    spawn_embedding_task(db, paper_id);

    Ok(chunks.len())
}

/// Rebuild a paper's text index from its stored PDF: re-extract and re-chunk
/// FIRST (a parse failure leaves the existing index intact), then atomically
/// swap old chunks for new ones (embeddings cascade-delete, FTS stays in
/// sync via triggers), and finally re-embed in the background. Returns the
/// new chunk count.
#[instrument(skip(db))]
pub async fn reprocess_paper_index(
    db: &SqlitePool,
    app_data_dir: &Path,
    paper: &Paper,
) -> Result<usize> {
    // Serialize rebuilds: concurrent DELETE+INSERT cycles (double-click,
    // multiple windows) would interleave and produce duplicate chunks.
    static REPROCESS_LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    let _guard = REPROCESS_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;

    let Some(rel_path) = paper.file_path.as_deref() else {
        return Err(SikuError::NotFound("该文献没有关联的 PDF 文件".into()));
    };
    let path = file_store::resolve_blob_path(app_data_dir, rel_path);
    if !path.exists() {
        return Err(SikuError::NotFound(format!("PDF 文件不存在: {path:?}")));
    }

    // Extract + chunk BEFORE touching the database.
    let pages = pdf::extractor::extract_text(&path)?;
    let config = ChunkConfig::default();
    let chunks = pdf::chunker::chunk_pages(&pages, &config);

    // Atomic swap: old chunks out (embeddings cascade via FK, chunks_fts via
    // triggers), new chunks in, page count refreshed.
    let now = now_iso();
    let mut tx = db.begin().await?;
    sqlx::query("DELETE FROM chunks WHERE paper_id = ?")
        .bind(&paper.id)
        .execute(&mut *tx)
        .await?;
    insert_chunks_tx(&mut tx, &paper.id, &chunks, &now).await?;
    sqlx::query("UPDATE papers SET page_count = ?, updated_at = ? WHERE id = ?")
        .bind(pages.len() as i32)
        .bind(&now)
        .bind(&paper.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    if !chunks.is_empty() {
        spawn_embedding_task(db, &paper.id);
    }

    Ok(chunks.len())
}

/// Best-effort bibliographic enrichment (Zotero-style):
/// resolve a DOI (paper field → PDF Info/XMP/text), query CrossRef, and fill
/// the blank metadata fields (year/journal/volume/issue/pages/publisher/ISSN/
/// abstract/authors...). Falls back to a title search when no DOI is found.
/// Returns true when at least one field was filled.
#[instrument(skip(db))]
pub async fn enrich_paper_metadata(
    db: &SqlitePool,
    app_data_dir: &Path,
    paper_id: &str,
) -> std::result::Result<bool, String> {
    let paper = get_paper(db, paper_id).await?;
    let proxy = crate::core::settings_service::get_setting(db, "llm.proxy").await.ok().flatten();

    // 1. Resolve a DOI.
    let doi = paper
        .doi
        .clone()
        .filter(|d| !d.trim().is_empty())
        .or_else(|| {
            paper.file_path.as_deref().and_then(|p| {
                crate::pdf::parser::extract_doi(&file_store::resolve_blob_path(app_data_dir, p))
            })
        });

    let work = if let Some(d) = &doi {
        crate::ai::scraping::metadata::fetch_by_doi(d, proxy.as_deref())
            .await
            .unwrap_or(None)
    } else {
        // 2. No DOI → try a title search, accepting only a close match.
        let results = crate::ai::scraping::metadata::search_works(&paper.title, 5, proxy.as_deref())
            .await
            .unwrap_or_default();
        results.into_iter().find(|w| {
            w.title.as_deref().map(|t| title_similar(&paper.title, t)).unwrap_or(false)
        })
    };

    let Some(work) = work else {
        return Ok(false);
    };

    let current_authors: Vec<String> = serde_json::from_str(&paper.authors).unwrap_or_default();
    let work_authors = work.authors.clone().filter(|v| !v.is_empty());
    let authors = if !current_authors.is_empty() {
        current_authors
    } else {
        work_authors.unwrap_or_default()
    };

    let mut changed = false;
    let input = crate::core::models::PaperInput {
        title: paper.title.clone(),
        authors,
        year: paper.year.or(work.year),
        journal: paper.journal.clone().or_else(|| work.journal.clone()),
        doi: paper.doi.clone().or_else(|| work.doi.clone()),
        url: paper.url.clone().or_else(|| work.url.clone()),
        abstract_text: paper.abstract_text.clone().or_else(|| work.abstract_text.clone()),
        keywords: serde_json::from_str(&paper.keywords).unwrap_or_default(),
        item_type: paper.item_type.clone(),
        volume: paper.volume.clone().or_else(|| work.volume.clone()),
        issue: paper.issue.clone().or_else(|| work.issue.clone()),
        pages: paper.pages.clone().or_else(|| work.pages.clone()),
        conference_name: paper.conference_name.clone(),
        publisher: paper.publisher.clone().or_else(|| work.publisher.clone()),
        place: paper.place.clone(),
        editor: serde_json::from_str(&paper.editor).unwrap_or_default(),
        series: paper.series.clone(),
        edition: paper.edition.clone(),
        isbn: paper.isbn.clone(),
        issn: paper.issn.clone().or_else(|| work.issn.clone()),
        language: paper.language.clone(),
        num_pages: paper.num_pages,
        archive_location: paper.archive_location.clone(),
        call_number: paper.call_number.clone(),
        rights: paper.rights.clone(),
    };
    if work.year.is_some() && paper.year.is_none() { changed = true; }
    if work.journal.is_some() && paper.journal.is_none() { changed = true; }
    if work.volume.is_some() && paper.volume.is_none() { changed = true; }
    if work.issue.is_some() && paper.issue.is_none() { changed = true; }
    if work.pages.is_some() && paper.pages.is_none() { changed = true; }
    if work.abstract_text.is_some() && paper.abstract_text.is_none() { changed = true; }
    if work.issn.is_some() && paper.issn.is_none() { changed = true; }
    if work.doi.is_some() && paper.doi.is_none() { changed = true; }
    if work.url.is_some() && paper.url.is_none() { changed = true; }
    if work.authors.as_ref().is_some_and(|a| !a.is_empty()) && serde_json::from_str::<Vec<String>>(&paper.authors).unwrap_or_default().is_empty() {
        changed = true;
    }

    if changed {
        update_paper(db, paper_id, input).await?;
    }
    Ok(changed)
}

/// Loose title similarity: share ≥50% of significant tokens.
fn title_similar(a: &str, b: &str) -> bool {
    let norm = |s: &str| -> Vec<String> {
        s.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() > 2)
            .map(|w| w.to_string())
            .collect()
    };
    let ta = norm(a);
    let tb = norm(b);
    if ta.is_empty() || tb.is_empty() {
        return false;
    }
    let overlap = ta.iter().filter(|w| tb.contains(w)).count();
    overlap as f32 / ta.len().max(tb.len()) as f32 >= 0.5
}

/// List papers with optional search, filtering, sorting, and pagination.
#[instrument(skip(db))]
pub async fn list_papers(db: &SqlitePool, params: ListPapersParams) -> Result<Vec<Paper>> {
    let mut sql = String::from(
        "SELECT id, title, authors, year, journal, doi, url, abstract, keywords, \
         citation_key, bibtex, file_path, file_size, page_count, language, \
         item_type, volume, issue, pages, conference_name, publisher, place, editor, \
         series, edition, isbn, issn, num_pages, archive_location, call_number, rights, \
         deleted_at, is_favorite, read_status, last_read_at, created_at, updated_at, imported_at FROM papers WHERE 1=1",
    );

    let mut bind_values: Vec<String> = Vec::new();

    // Trash filter: normal lists exclude soft-deleted papers; the trash view
    // (include_deleted=true) shows only them.
    if params.include_deleted.unwrap_or(false) {
        sql.push_str(" AND deleted_at IS NOT NULL");
    } else {
        sql.push_str(" AND deleted_at IS NULL");
    }

    // Favorite / read-status filters.
    if params.is_favorite.unwrap_or(false) {
        sql.push_str(" AND is_favorite = 1");
    }
    if let Some(status) = params.read_status.as_deref().filter(|s| !s.is_empty()) {
        sql.push_str(" AND read_status = ?");
    }

    // Year range filter (inclusive).
    if let Some(y) = params.year_from {
        sql.push_str(" AND year >= ?");
    }
    if let Some(y) = params.year_to {
        sql.push_str(" AND year <= ?");
    }

    // Journal filter (case-insensitive substring).
    if let Some(j) = params.journal.as_deref().filter(|j| !j.is_empty()) {
        sql.push_str(" AND journal LIKE ?");
    }

    // Related-to filter: papers linked to the given paper id.
    if let Some(id) = params.related_to.as_deref().filter(|i| !i.is_empty()) {
        sql.push_str(
            " AND id IN (SELECT paper_id FROM related_papers WHERE related_id = ?              UNION SELECT related_id FROM related_papers WHERE paper_id = ?)",
        );
    }

    // Search filter: metadata (title/authors) + PDF full text (chunks_fts).
    // chunks_fts uses the trigram tokenizer (CJK-friendly); terms shorter
    // than 3 chars are skipped, and a query with no qualifying terms must not
    // build an empty MATCH expression (FTS5 syntax error).
    let search_pattern: Option<String> = params
        .search
        .as_ref()
        .filter(|s| !s.is_empty())
        .map(|s| format!("%{}%", s));
    let fts_query = params
        .search
        .as_ref()
        .map(|s| {
            s.split_whitespace()
                .filter(|w| w.chars().count() >= 3)
                .map(|w| format!("{w}*"))
                .collect::<Vec<_>>()
                .join(" OR ")
        })
        .filter(|q| !q.is_empty());
    if search_pattern.is_some() {
        sql.push_str(" AND (title LIKE ? OR authors LIKE ?");
        if fts_query.is_some() {
            sql.push_str(
                " OR id IN (SELECT DISTINCT c.paper_id FROM chunks_fts fts \
                 JOIN chunks c ON fts.rowid = c.rowid \
                 WHERE chunks_fts MATCH ? LIMIT 300)",
            );
        }
        sql.push_str(")");
    }

    // Collection filter
    let collection_id_filter: Option<String> = params.collection_id.clone();
    if collection_id_filter.is_some() {
        sql.push_str(
            " AND id IN (SELECT paper_id FROM paper_collections WHERE collection_id = ?)",
        );
    }

    // Tag filter (multiple tags, AND/OR logic)
    let tag_ids: Vec<String> = params.tag_ids.clone().unwrap_or_default();
    let tag_logic = params.tag_logic.as_deref().unwrap_or("or");
    if !tag_ids.is_empty() {
        let placeholders = vec!["?"; tag_ids.len()].join(", ");
        if tag_logic.eq_ignore_ascii_case("and") {
            // Papers that have ALL selected tags.
            sql.push_str(&format!(
                " AND id IN (SELECT paper_id FROM paper_tags WHERE tag_id IN ({placeholders}) \
                 GROUP BY paper_id HAVING COUNT(DISTINCT tag_id) = {})",
                tag_ids.len()
            ));
        } else {
            // Papers that have ANY selected tag.
            sql.push_str(&format!(
                " AND id IN (SELECT paper_id FROM paper_tags WHERE tag_id IN ({placeholders}))"
            ));
        }
    }

    // Valid sort fields. last_read_at only makes sense for papers that have
    // actually been opened, so exclude NULLs in that case.
    let sort_by = match params.sort_by.as_deref() {
        Some("title") => "title",
        Some("year") => "year",
        Some("last_read_at") => {
            sql.push_str(" AND last_read_at IS NOT NULL");
            "last_read_at"
        }
        Some("imported_at") => "imported_at",
        _ => "imported_at",
    };
    let sort_order = match params.sort_order.as_deref() {
        Some("asc") => "ASC",
        _ => "DESC",
    };

    sql.push_str(&format!(" ORDER BY {} {}", sort_by, sort_order));

    if let Some(limit) = params.limit {
        sql.push_str(&format!(" LIMIT {}", limit));
    }
    if let Some(offset) = params.offset {
        sql.push_str(&format!(" OFFSET {}", offset));
    }

    // Bind values in the same order as placeholders.
    if let Some(ref pattern) = search_pattern {
        bind_values.push(pattern.clone());
        bind_values.push(pattern.clone());
    }
    if let Some(ref fts) = fts_query {
        bind_values.push(fts.clone());
    }
    if let Some(ref coll_id) = collection_id_filter {
        bind_values.push(coll_id.clone());
    }
    for tid in &tag_ids {
        bind_values.push(tid.clone());
    }
    if let Some(status) = params.read_status.as_deref().filter(|s| !s.is_empty()) {
        bind_values.push(status.to_string());
    }
    if let Some(y) = params.year_from {
        bind_values.push(y.to_string());
    }
    if let Some(y) = params.year_to {
        bind_values.push(y.to_string());
    }
    if let Some(j) = params.journal.as_deref().filter(|j| !j.is_empty()) {
        bind_values.push(format!("%{}%", j));
    }
    if let Some(id) = params.related_to.as_deref().filter(|i| !i.is_empty()) {
        bind_values.push(id.to_string());
        bind_values.push(id.to_string());
    }

    let mut q = sqlx::query_as::<_, Paper>(&sql);
    for v in &bind_values {
        q = q.bind(v);
    }

    let papers = q.fetch_all(db).await?;
    Ok(papers)
}

/// Get a single paper by ID.
#[instrument(skip(db))]
pub async fn get_paper(db: &SqlitePool, id: &str) -> Result<Paper> {
    let paper = sqlx::query_as::<_, Paper>(
        "SELECT id, title, authors, year, journal, doi, url, abstract, keywords, \
         citation_key, bibtex, file_path, file_size, page_count, language, \
         item_type, volume, issue, pages, conference_name, publisher, place, editor, \
         series, edition, isbn, issn, num_pages, archive_location, call_number, rights, \
         deleted_at, is_favorite, read_status, last_read_at, created_at, updated_at, imported_at FROM papers WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| SikuError::NotFound(format!("paper {} not found", id)))?;

    Ok(paper)
}

/// Update paper metadata. Only overwrites fields present in the input.
#[instrument(skip(db))]
pub async fn update_paper(db: &SqlitePool, id: &str, input: PaperInput) -> Result<Paper> {
    // Verify paper exists
    get_paper(db, id).await?;

    let now = now_iso();
    let authors_json = serde_json::to_string(&input.authors).unwrap_or_else(|_| "[]".to_string());
    let keywords_json =
        serde_json::to_string(&input.keywords).unwrap_or_else(|_| "[]".to_string());
    let editor_json = serde_json::to_string(&input.editor).unwrap_or_else(|_| "[]".to_string());

    let paper: Paper = sqlx::query_as(
        r#"UPDATE papers SET
            title = ?, authors = ?, year = ?, journal = ?, doi = ?, url = ?,
            abstract = ?, keywords = ?, item_type = ?, volume = ?, issue = ?, pages = ?,
            conference_name = ?, publisher = ?, place = ?, editor = ?, series = ?, edition = ?,
            isbn = ?, issn = ?, language = ?, num_pages = ?, archive_location = ?,
            call_number = ?, rights = ?, updated_at = ?
        WHERE id = ?
        RETURNING id, title, authors, year, journal, doi, url, abstract, keywords,
            citation_key, bibtex, file_path, file_size, page_count, language,
            item_type, volume, issue, pages, conference_name, publisher, place, editor,
            series, edition, isbn, issn, num_pages, archive_location, call_number, rights,
            deleted_at, is_favorite, read_status, last_read_at, created_at, updated_at, imported_at"#,
    )
    .bind(&input.title)
    .bind(&authors_json)
    .bind(input.year)
    .bind(&input.journal)
    .bind(&input.doi)
    .bind(&input.url)
    .bind(&input.abstract_text)
    .bind(&keywords_json)
    .bind(&input.item_type)
    .bind(&input.volume)
    .bind(&input.issue)
    .bind(&input.pages)
    .bind(&input.conference_name)
    .bind(&input.publisher)
    .bind(&input.place)
    .bind(&editor_json)
    .bind(&input.series)
    .bind(&input.edition)
    .bind(&input.isbn)
    .bind(&input.issn)
    .bind(&input.language)
    .bind(input.num_pages)
    .bind(&input.archive_location)
    .bind(&input.call_number)
    .bind(&input.rights)
    .bind(&now)
    .bind(id)
    .fetch_one(db)
    .await?;

    info!(paper_id = %id, "paper updated");
    Ok(paper)
}

/// Move a paper to the trash (soft delete): set `deleted_at`, keep all rows
/// and files so it can be restored. Use [`purge_paper`] to permanently remove
/// the row and its unreferenced files.
#[instrument(skip(db, _app_data_dir))]
pub async fn delete_paper(db: &SqlitePool, _app_data_dir: &Path, id: &str) -> Result<()> {
    let n = sqlx::query("UPDATE papers SET deleted_at = ? WHERE id = ? AND deleted_at IS NULL")
        .bind(now_iso())
        .bind(id)
        .execute(db)
        .await?;
    if n.rows_affected() == 0 {
        return Err(SikuError::NotFound(format!("paper {id} not found")));
    }
    info!(paper_id = %id, "paper moved to trash");
    Ok(())
}

/// Restore a trashed paper (clear `deleted_at`).
#[instrument(skip(db))]
pub async fn restore_paper(db: &SqlitePool, id: &str) -> Result<()> {
    let n = sqlx::query("UPDATE papers SET deleted_at = NULL WHERE id = ?")
        .bind(id)
        .execute(db)
        .await?;
    if n.rows_affected() == 0 {
        return Err(SikuError::NotFound(format!("paper {id} not found")));
    }
    info!(paper_id = %id, "paper restored from trash");
    Ok(())
}

/// Permanently delete a paper and remove its managed files (cascades to
/// attachments, annotations, chunks; blob removed only when unreferenced).
#[instrument(skip(db, app_data_dir))]
pub async fn purge_paper(db: &SqlitePool, app_data_dir: &Path, id: &str) -> Result<()> {
    // Verify paper exists and capture its file path before deletion.
    let paper = get_paper(db, id).await?;

    // Clean up linked knowledge items
    let _ = crate::core::knowledge::remove_by_source(db, "paper", id).await;

    // Break import records to avoid foreign-key constraint errors while keeping import history.
    sqlx::query("UPDATE imports SET paper_id = NULL WHERE paper_id = ?")
        .bind(id)
        .execute(db)
        .await?;

    // Junction/org tables have no FK cascades (CR-SQLite forbids checked FKs
    // on CRRs) — delete children explicitly so the deletions are tracked as
    // CRDT changes and propagate to other devices. This also removes
    // annotations, which never had a cascade and were previously orphaned.
    sqlx::query("DELETE FROM paper_tags WHERE paper_id = ?")
        .bind(id)
        .execute(db)
        .await?;
    sqlx::query("DELETE FROM paper_collections WHERE paper_id = ?")
        .bind(id)
        .execute(db)
        .await?;
    sqlx::query("DELETE FROM related_papers WHERE paper_id = ? OR related_id = ?")
        .bind(id)
        .bind(id)
        .execute(db)
        .await?;
    sqlx::query("DELETE FROM annotations WHERE paper_id = ?")
        .bind(id)
        .execute(db)
        .await?;

    // Delete from database (cascades to attachments, chunks, creators, etc.)
    sqlx::query("DELETE FROM papers WHERE id = ?")
        .bind(id)
        .execute(db)
        .await?;

    // Try to clean up the PDF blob if no other paper or attachment references it.
    if let Some(rel_path) = paper.file_path.as_deref() {
        let blob_path = file_store::resolve_blob_path(app_data_dir, rel_path);
        if blob_path.exists() {
            let count_papers: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM papers WHERE file_path = ? AND id != ?"
            )
            .bind(rel_path)
            .bind(id)
            .fetch_one(db)
            .await
            .unwrap_or(0);
            let count_attachments: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM attachments WHERE file_path = ?"
            )
            .bind(rel_path)
            .bind(id)
            .fetch_one(db)
            .await
            .unwrap_or(0);
            if count_papers == 0 && count_attachments == 0 {
                if let Err(e) = std::fs::remove_file(&blob_path) {
                    warn!(
                        paper_id = %id,
                        path = %blob_path.display(),
                        error = %e,
                        "failed to remove unreferenced blob"
                    );
                }
            }
        }
    }

    info!(paper_id = %id, "paper permanently deleted");
    Ok(())
}

// ── Duplicate detection & merge ────────────────────────────────────────────

/// Normalize a title for duplicate comparison: keep only alphanumeric chars,
/// lowercase (CJK-safe). Makes "Attention Is All You Need." and
/// "attention is all you need" compare equal.
fn normalize_title_for_dedup(title: &str) -> String {
    title
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// A paper likely identical to the one being checked, plus why it matched.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateCandidate {
    pub id: String,
    pub title: String,
    pub year: Option<i32>,
    pub journal: Option<String>,
    pub doi: Option<String>,
    /// "doi" (exact match) or "title" (normalized equality).
    pub match_reason: String,
}

const PAPER_COLS: &str = "id, title, authors, year, journal, doi, url, abstract, keywords, \
     citation_key, bibtex, file_path, file_size, page_count, language, \
     item_type, volume, issue, pages, conference_name, publisher, place, editor, \
     series, edition, isbn, issn, num_pages, archive_location, call_number, rights, \
     deleted_at, is_favorite, read_status, last_read_at, created_at, updated_at, imported_at";

/// Find papers that are likely duplicates of `title`/`doi`, excluding
/// `exclude_id`. Exact (case/whitespace-insensitive) DOI match wins; when
/// there is no DOI match, normalized-title equality is used.
#[instrument(skip(db))]
pub async fn find_duplicate_papers(
    db: &SqlitePool,
    title: &str,
    doi: Option<&str>,
    exclude_id: &str,
) -> Result<Vec<DuplicateCandidate>> {
    let mut out = Vec::new();

    if let Some(doi) = doi.map(str::trim).filter(|d| !d.is_empty()) {
        let rows = sqlx::query_as::<_, Paper>(&format!(
            "SELECT {PAPER_COLS} FROM papers WHERE TRIM(LOWER(doi)) = LOWER(?) AND id != ?"
        ))
        .bind(doi)
        .bind(exclude_id)
        .fetch_all(db)
        .await
        ?;
        for p in rows {
            out.push(DuplicateCandidate {
                id: p.id,
                title: p.title,
                year: p.year,
                journal: p.journal,
                doi: p.doi,
                match_reason: "doi".into(),
            });
        }
        if !out.is_empty() {
            return Ok(out);
        }
    }

    let norm = normalize_title_for_dedup(title);
    if !norm.is_empty() {
        let all = sqlx::query_as::<_, Paper>(&format!(
            "SELECT {PAPER_COLS} FROM papers WHERE id != ?"
        ))
        .bind(exclude_id)
        .fetch_all(db)
        .await
        ?;
        for p in all {
            let pn = normalize_title_for_dedup(&p.title);
            if !pn.is_empty() && pn == norm {
                out.push(DuplicateCandidate {
                    id: p.id,
                    title: p.title,
                    year: p.year,
                    journal: p.journal,
                    doi: p.doi,
                    match_reason: "title".into(),
                });
            }
        }
    }
    Ok(out)
}

/// Merge `remove_id` into `keep_id`: fill empty metadata fields in `keep`
/// from `remove`, transfer all children (attachments, notes, annotations,
/// chunks, imports, tags, collections), then delete the removed paper.
/// Blob-aware: a PDF now shared with `keep` survives because `delete_paper`
/// only removes a blob when nothing references it anymore.
#[instrument(skip(db, app_data_dir))]
pub async fn merge_papers(
    db: &SqlitePool,
    app_data_dir: &Path,
    keep_id: &str,
    remove_id: &str,
) -> Result<()> {
    if keep_id == remove_id {
        return Ok(());
    }
    let keep = get_paper(db, keep_id).await?;
    let remove = get_paper(db, remove_id).await?;

    // Pick a non-empty field from `keep`, falling back to `remove`.
    fn pick<'a>(a: &'a Option<String>, b: &'a Option<String>) -> Option<&'a str> {
        a.as_deref()
            .filter(|s| !s.is_empty())
            .or_else(|| b.as_deref().filter(|s| !s.is_empty()))
    }

    let title = if keep.title.trim().is_empty() { &remove.title } else { &keep.title };
    let authors = if keep.authors.trim().is_empty() || keep.authors == "[]" {
        &remove.authors
    } else {
        &keep.authors
    };
    let editor = if keep.editor.trim().is_empty() || keep.editor == "[]" {
        &remove.editor
    } else {
        &keep.editor
    };
    let keywords = if keep.keywords.trim().is_empty() || keep.keywords == "[]" {
        &remove.keywords
    } else {
        &keep.keywords
    };

    sqlx::query(
        "UPDATE papers SET title=?, authors=?, year=?, journal=?, doi=?, url=?, abstract=?, \
         keywords=?, citation_key=?, bibtex=?, file_path=?, file_size=?, page_count=?, language=?, \
         item_type=?, volume=?, issue=?, pages=?, conference_name=?, publisher=?, place=?, editor=?, \
         series=?, edition=?, isbn=?, issn=?, num_pages=?, archive_location=?, call_number=?, rights=?, \
         updated_at=? WHERE id=?",
    )
    .bind(title)
    .bind(authors)
    .bind(keep.year.or(remove.year))
    .bind(pick(&keep.journal, &remove.journal))
    .bind(pick(&keep.doi, &remove.doi))
    .bind(pick(&keep.url, &remove.url))
    .bind(pick(&keep.abstract_text, &remove.abstract_text))
    .bind(keywords)
    .bind(pick(&keep.citation_key, &remove.citation_key))
    .bind(pick(&keep.bibtex, &remove.bibtex))
    .bind(pick(&keep.file_path, &remove.file_path))
    .bind(keep.file_size.or(remove.file_size))
    .bind(keep.page_count.or(remove.page_count))
    .bind(pick(&keep.language, &remove.language))
    .bind(pick(&keep.item_type, &remove.item_type))
    .bind(pick(&keep.volume, &remove.volume))
    .bind(pick(&keep.issue, &remove.issue))
    .bind(pick(&keep.pages, &remove.pages))
    .bind(pick(&keep.conference_name, &remove.conference_name))
    .bind(pick(&keep.publisher, &remove.publisher))
    .bind(pick(&keep.place, &remove.place))
    .bind(editor)
    .bind(pick(&keep.series, &remove.series))
    .bind(pick(&keep.edition, &remove.edition))
    .bind(pick(&keep.isbn, &remove.isbn))
    .bind(pick(&keep.issn, &remove.issn))
    .bind(keep.num_pages.or(remove.num_pages))
    .bind(pick(&keep.archive_location, &remove.archive_location))
    .bind(pick(&keep.call_number, &remove.call_number))
    .bind(pick(&keep.rights, &remove.rights))
    .bind(now_iso())
    .bind(keep_id)
    .execute(db)
    .await
    ?;

    // Transfer children inside one transaction.
    let mut tx = db.begin().await?;
    for table in ["attachments", "notes", "annotations", "chunks", "imports"] {
        let sql = format!("UPDATE {table} SET paper_id = ? WHERE paper_id = ?");
        sqlx::query(&sql)
            .bind(keep_id)
            .bind(remove_id)
            .execute(&mut *tx)
            .await
            ?;
    }
    sqlx::query(
        "INSERT OR IGNORE INTO paper_tags (paper_id, tag_id) \
         SELECT ?, tag_id FROM paper_tags WHERE paper_id = ?",
    )
    .bind(keep_id)
    .bind(remove_id)
    .execute(&mut *tx)
    .await
    ?;
    sqlx::query(
        "INSERT OR IGNORE INTO paper_collections (paper_id, collection_id) \
         SELECT ?, collection_id FROM paper_collections WHERE paper_id = ?",
    )
    .bind(keep_id)
    .bind(remove_id)
    .execute(&mut *tx)
    .await
    ?;
    tx.commit().await?;

    // Children were transferred above, so the removed paper is purged
    // permanently (not moved to trash) — nothing of it remains to restore.
    purge_paper(db, app_data_dir, remove_id).await?;
    info!(keep = %keep_id, removed = %remove_id, "papers merged");
    Ok(())
}

// ── Read status & favorites ────────────────────────────────────────────────

/// Set the favorite flag (1/0).
pub async fn set_paper_favorite(db: &SqlitePool, id: &str, favorite: bool) -> Result<()> {
    let n = sqlx::query("UPDATE papers SET is_favorite = ? WHERE id = ?")
        .bind(if favorite { 1 } else { 0 })
        .bind(id)
        .execute(db)
        .await?;
    if n.rows_affected() == 0 {
        return Err(SikuError::NotFound(format!("paper {id} not found")));
    }
    Ok(())
}

/// Set the read status: "unread" | "read" | "in_progress".
pub async fn set_paper_read_status(db: &SqlitePool, id: &str, status: &str) -> Result<()> {
    let n = sqlx::query("UPDATE papers SET read_status = ? WHERE id = ?")
        .bind(status)
        .bind(id)
        .execute(db)
        .await?;
    if n.rows_affected() == 0 {
        return Err(SikuError::NotFound(format!("paper {id} not found")));
    }
    Ok(())
}

/// Record that a paper was opened in the reader. Updates last_read_at and
/// promotes read_status from "unread" to "in_progress" without overwriting
/// an explicit "read" mark.
pub async fn record_paper_read(db: &SqlitePool, id: &str) -> Result<()> {
    let now = now_iso();
    let n = sqlx::query(
        "UPDATE papers SET last_read_at = ?, read_status = CASE \
         WHEN read_status = 'unread' THEN 'in_progress' ELSE read_status END \
         WHERE id = ?",
    )
    .bind(&now)
    .bind(id)
    .execute(db)
    .await?;
    if n.rows_affected() == 0 {
        return Err(SikuError::NotFound(format!("paper {id} not found")));
    }
    Ok(())
}

// ── Related papers ─────────────────────────────────────────────────────────

/// Link two papers (bidirectional; stored once, queried either direction).
pub async fn add_related_paper(
    db: &SqlitePool,
    paper_id: &str,
    related_id: &str,
) -> Result<()> {
    if paper_id == related_id {
        return Ok(());
    }
    sqlx::query(
        "INSERT OR IGNORE INTO related_papers (paper_id, related_id, created_at) VALUES (?, ?, ?)",
    )
    .bind(paper_id)
    .bind(related_id)
    .bind(now_iso())
    .execute(db)
    .await?;
    Ok(())
}

/// Remove the link between two papers (either direction).
pub async fn remove_related_paper(
    db: &SqlitePool,
    paper_id: &str,
    related_id: &str,
) -> Result<()> {
    sqlx::query(
        "DELETE FROM related_papers WHERE (paper_id = ? AND related_id = ?)          OR (paper_id = ? AND related_id = ?)",
    )
    .bind(paper_id)
    .bind(related_id)
    .bind(related_id)
    .bind(paper_id)
    .execute(db)
    .await?;
    Ok(())
}

/// List papers related to `paper_id` (either direction), ordered by recency.
pub async fn list_related_papers(db: &SqlitePool, paper_id: &str) -> Result<Vec<Paper>> {
    let rows = sqlx::query_as::<_, Paper>(&format!(
        "SELECT {PAPER_COLS} FROM papers WHERE id IN (         SELECT related_id FROM related_papers WHERE paper_id = ?          UNION SELECT paper_id FROM related_papers WHERE related_id = ?)          AND deleted_at IS NULL ORDER BY updated_at DESC"
    ))
    .bind(paper_id)
    .bind(paper_id)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

// ── Saved searches (device-local) ──────────────────────────────────────────

/// List saved searches, newest first.
pub async fn list_saved_searches(db: &SqlitePool) -> Result<Vec<crate::core::models::SavedSearch>> {
    Ok(sqlx::query_as::<_, crate::core::models::SavedSearch>(
        "SELECT id, name, params_json, created_at FROM saved_searches ORDER BY created_at DESC",
    )
    .fetch_all(db)
    .await?)
}

/// Save the current search params under a name.
pub async fn create_saved_search(
    db: &SqlitePool,
    name: &str,
    params_json: &str,
) -> Result<crate::core::models::SavedSearch> {
    let id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO saved_searches (id, name, params_json, created_at) VALUES (?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(name)
    .bind(params_json)
    .bind(now_iso())
    .execute(db)
    .await?;
    Ok(crate::core::models::SavedSearch {
        id,
        name: name.to_string(),
        params_json: params_json.to_string(),
        created_at: now_iso(),
    })
}

/// Delete a saved search.
pub async fn delete_saved_search(db: &SqlitePool, id: &str) -> Result<()> {
    sqlx::query("DELETE FROM saved_searches WHERE id = ?")
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

// ── Structured creators ────────────────────────────────────────────────────

/// A structured creator (role + name), stored in the device-local `creators`
/// table. The legacy `authors`/`editor` JSON columns remain the sync
/// transport: every write here also regenerates them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Creator {
    pub role: String,
    pub last_name: String,
    pub first_name: String,
    pub name: String,
}

/// Insert creators rows for one paper from its authors/editor JSON columns.
/// Caller owns the transaction and any prior cleanup of existing rows.
async fn insert_creators_from_json(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    paper_id: &str,
    authors_json: &str,
    editor_json: &str,
) -> Result<()> {
    let authors: Vec<String> = serde_json::from_str(authors_json).unwrap_or_default();
    let editors: Vec<String> = serde_json::from_str(editor_json).unwrap_or_default();
    let mut sort = 0i64;
    for name in authors.iter().chain(editors.iter()) {
        let role = if sort < authors.len() as i64 { "author" } else { "editor" };
        sqlx::query(
            "INSERT INTO creators (id, paper_id, role, last_name, first_name, name, sort_order) \
             VALUES (?, ?, ?, '', '', ?, ?)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(paper_id)
        .bind(role)
        .bind(name)
        .bind(sort)
        .execute(&mut **tx)
        .await?;
        sort += 1;
    }
    Ok(())
}

/// Backfill `creators` rows from the legacy authors/editor JSON columns.
/// Names are kept whole (no last/first splitting) — parsing is deferred until
/// the structured editor actually edits them.
pub async fn backfill_creators(db: &SqlitePool) -> Result<()> {
    let papers = sqlx::query_as::<_, (String, String, String)>(
        "SELECT id, authors, editor FROM papers",
    )
    .fetch_all(db)
    .await?;
    let total = papers.len();
    let mut tx = db.begin().await?;
    for (id, authors_json, editor_json) in papers {
        insert_creators_from_json(&mut tx, &id, &authors_json, &editor_json).await?;
    }
    tx.commit().await?;
    info!("backfilled creators for {} papers", total);
    Ok(())
}

/// Rebuild `creators` rows for specific papers from the legacy authors/editor
/// JSON columns. `creators` is a device-local overlay that is NOT synced
/// itself; call this after sync applies papers rows so the overlay matches
/// the synced source of truth. `delete_ids` removes rows whose paper was
/// deleted by the changeset (FK cascades don't fire while FK enforcement is
/// disabled during apply).
pub async fn rebuild_creators_for_papers(
    db: &SqlitePool,
    upsert_ids: &[String],
    delete_ids: &[String],
) -> Result<()> {
    let mut tx = db.begin().await?;
    for id in delete_ids {
        sqlx::query("DELETE FROM creators WHERE paper_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    for id in upsert_ids {
        let row: Option<(String, String)> =
            sqlx::query_as("SELECT authors, editor FROM papers WHERE id = ?")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        let Some((authors_json, editor_json)) = row else {
            continue;
        };
        sqlx::query("DELETE FROM creators WHERE paper_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        insert_creators_from_json(&mut tx, id, &authors_json, &editor_json).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Fill `creators` for papers that have no rows yet (fresh arrivals from a
/// full snapshot). Existing rows are left untouched.
pub async fn rebuild_creators_where_missing(db: &SqlitePool) -> Result<()> {
    let papers = sqlx::query_as::<_, (String, String, String)>(
        "SELECT id, authors, editor FROM papers \
         WHERE id NOT IN (SELECT DISTINCT paper_id FROM creators)",
    )
    .fetch_all(db)
    .await?;
    if papers.is_empty() {
        return Ok(());
    }
    let total = papers.len();
    let mut tx = db.begin().await?;
    for (id, authors_json, editor_json) in papers {
        insert_creators_from_json(&mut tx, &id, &authors_json, &editor_json).await?;
    }
    tx.commit().await?;
    info!("rebuilt creators for {} synced papers", total);
    Ok(())
}

/// Get structured creators for a paper. Falls back to the legacy
/// authors/editor columns when no structured rows exist yet.
pub async fn get_creators(db: &SqlitePool, paper_id: &str) -> Result<Vec<Creator>> {
    let rows = sqlx::query_as::<_, (String, String, String, String, i64)>(
        "SELECT role, last_name, first_name, name, sort_order FROM creators \
         WHERE paper_id = ? ORDER BY sort_order",
    )
    .bind(paper_id)
    .fetch_all(db)
    .await?;
    if !rows.is_empty() {
        return Ok(rows
            .into_iter()
            .map(|(role, last_name, first_name, name, _)| Creator {
                role,
                last_name,
                first_name,
                name,
            })
            .collect());
    }
    // Fallback: derive from the legacy columns.
    let paper = get_paper(db, paper_id).await?;
    let mut out = Vec::new();
    for name in crate::core::citation_export::parse_authors(&paper.authors) {
        out.push(Creator { role: "author".into(), last_name: String::new(), first_name: String::new(), name });
    }
    for name in crate::core::citation_export::parse_authors(&paper.editor) {
        out.push(Creator { role: "editor".into(), last_name: String::new(), first_name: String::new(), name });
    }
    Ok(out)
}

/// Replace the structured creators of a paper and regenerate the legacy
/// authors/editor columns (the sync transport) from them.
pub async fn set_creators(
    db: &SqlitePool,
    paper_id: &str,
    creators: &[Creator],
) -> Result<()> {
    fn display_name(c: &Creator) -> String {
        if !c.name.is_empty() {
            return c.name.clone();
        }
        [c.first_name.as_str(), c.last_name.as_str()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }
    let authors: Vec<String> = creators
        .iter()
        .filter(|c| c.role == "author")
        .map(display_name)
        .filter(|n| !n.is_empty())
        .collect();
    let editors: Vec<String> = creators
        .iter()
        .filter(|c| c.role == "editor")
        .map(display_name)
        .filter(|n| !n.is_empty())
        .collect();
    let authors_json = serde_json::to_string(&authors).unwrap_or_else(|_| "[]".to_string());
    let editors_json = serde_json::to_string(&editors).unwrap_or_else(|_| "[]".to_string());

    let mut tx = db.begin().await?;
    sqlx::query("DELETE FROM creators WHERE paper_id = ?")
        .bind(paper_id)
        .execute(&mut *tx)
        .await?;
    for (i, c) in creators.iter().enumerate() {
        sqlx::query(
            "INSERT INTO creators (id, paper_id, role, last_name, first_name, name, sort_order) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(paper_id)
        .bind(&c.role)
        .bind(&c.last_name)
        .bind(&c.first_name)
        .bind(&c.name)
        .bind(i as i64)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query("UPDATE papers SET authors = ?, editor = ?, updated_at = ? WHERE id = ?")
        .bind(&authors_json)
        .bind(&editors_json)
        .bind(now_iso())
        .bind(paper_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}
