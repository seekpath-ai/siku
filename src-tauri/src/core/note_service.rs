use sqlx::SqlitePool;
use tracing::instrument;

use crate::core::models::Note;
use crate::core::time;

/// Create a note
#[instrument(skip(db))]
pub async fn create_note(
    db: &SqlitePool,
    title: &str,
    content: &str,
    paper_id: Option<&str>,
    parent_id: Option<&str>,
    vault_id: &str,
    is_folder: bool,
) -> Result<Note, String> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = time::now_iso();
    let content_plain = strip_markdown(content);
    let tags = parse_tags(content);

    sqlx::query(
        "INSERT INTO notes (id, vault_id, paper_id, title, content, content_plain, tags, aliases, is_folder, parent_id, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, '[]', ?, ?, ?, ?)"
    ).bind(&id).bind(vault_id).bind(paper_id).bind(title).bind(content).bind(&content_plain)
     .bind(&tags).bind(is_folder).bind(parent_id).bind(&now).bind(&now)
     .execute(db).await.map_err(|e| format!("db: {e}"))?;

    // Parse and create wiki links
    let links = parse_wiki_links(content);
    update_note_links(db, &id, &links).await?;

    get_note(db, &id).await
}

/// Get a note by ID
pub async fn get_note(db: &SqlitePool, id: &str) -> Result<Note, String> {
    sqlx::query_as::<_, Note>("SELECT * FROM notes WHERE id = ?")
        .bind(id).fetch_optional(db).await
        .map_err(|e| format!("db: {e}"))?
        .ok_or_else(|| format!("note not found: {id}"))
}

/// Update a note. When `touch` is `Some(false)` the `updated_at` timestamp is
/// left untouched so the note keeps its current position in the tree.
#[instrument(skip(db))]
pub async fn update_note(
    db: &SqlitePool,
    id: &str,
    title: Option<&str>,
    content: Option<&str>,
    paper_id: Option<&str>,
    aliases: Option<&str>,
    is_favorite: Option<i32>,
    touch: Option<bool>,
) -> Result<Note, String> {
    let touch = touch.unwrap_or(true);
    let now = time::now_iso();

    // System folders (e.g. the "我的图书馆" library root) cannot be renamed.
    if let Some(t) = title {
        let sys: Option<(i32,)> = sqlx::query_as("SELECT is_system FROM notes WHERE id = ?")
            .bind(id)
            .fetch_optional(db)
            .await
            .map_err(|e| format!("db: {e}"))?;
        if let Some((1,)) = sys {
            return Err("不能重命名系统目录「我的图书馆」".to_string());
        }
        if touch {
            sqlx::query("UPDATE notes SET title = ?, updated_at = ? WHERE id = ?")
                .bind(t).bind(&now).bind(id).execute(db).await.map_err(|e| format!("db: {e}"))?;
        } else {
            sqlx::query("UPDATE notes SET title = ? WHERE id = ?")
                .bind(t).bind(id).execute(db).await.map_err(|e| format!("db: {e}"))?;
        }
    }
    if let Some(c) = content {
        let plain = strip_markdown(c);
        let tags = parse_tags(c);
        if touch {
            sqlx::query("UPDATE notes SET content = ?, content_plain = ?, tags = ?, updated_at = ? WHERE id = ?")
                .bind(c).bind(&plain).bind(&tags).bind(&now).bind(id).execute(db).await.map_err(|e| format!("db: {e}"))?;
        } else {
            sqlx::query("UPDATE notes SET content = ?, content_plain = ?, tags = ? WHERE id = ?")
                .bind(c).bind(&plain).bind(&tags).bind(id).execute(db).await.map_err(|e| format!("db: {e}"))?;
        }

        let links = parse_wiki_links(c);
        update_note_links(db, id, &links).await?;
    }
    if let Some(pid) = paper_id {
        if touch {
            sqlx::query("UPDATE notes SET paper_id = ?, updated_at = ? WHERE id = ?")
                .bind(pid).bind(&now).bind(id).execute(db).await.map_err(|e| format!("db: {e}"))?;
        } else {
            sqlx::query("UPDATE notes SET paper_id = ? WHERE id = ?")
                .bind(pid).bind(id).execute(db).await.map_err(|e| format!("db: {e}"))?;
        }
    }
    if let Some(a) = aliases {
        if touch {
            sqlx::query("UPDATE notes SET aliases = ?, updated_at = ? WHERE id = ?")
                .bind(a).bind(&now).bind(id).execute(db).await.map_err(|e| format!("db: {e}"))?;
        } else {
            sqlx::query("UPDATE notes SET aliases = ? WHERE id = ?")
                .bind(a).bind(id).execute(db).await.map_err(|e| format!("db: {e}"))?;
        }
    }
    if let Some(f) = is_favorite {
        if touch {
            sqlx::query("UPDATE notes SET is_favorite = ?, updated_at = ? WHERE id = ?")
                .bind(f).bind(&now).bind(id).execute(db).await.map_err(|e| format!("db: {e}"))?;
        } else {
            sqlx::query("UPDATE notes SET is_favorite = ? WHERE id = ?")
                .bind(f).bind(id).execute(db).await.map_err(|e| format!("db: {e}"))?;
        }
    }

    get_note(db, id).await
}

/// Delete a note together with its whole subtree. When the target is a folder,
/// all descendant notes and subfolders are deleted too (file-manager/Obsidian
/// semantics) instead of being re-parented to the root.
pub async fn delete_note(db: &SqlitePool, id: &str) -> Result<(), String> {
    // System folders (e.g. the "我的图书馆" library root) are protected.
    let sys: Option<(i32,)> = sqlx::query_as("SELECT is_system FROM notes WHERE id = ?")
        .bind(id)
        .fetch_optional(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    if let Some((1,)) = sys {
        return Err("不能删除系统目录「我的图书馆」".to_string());
    }

    // Collect the note itself plus every descendant (child notes/folders).
    let ids: Vec<(String,)> = sqlx::query_as(
        "WITH RECURSIVE subtree(id) AS (
             SELECT ?1
             UNION
             SELECT n.id FROM notes n JOIN subtree s ON n.parent_id = s.id
         )
         SELECT id FROM subtree",
    )
    .bind(id)
    .fetch_all(db)
    .await
    .map_err(|e| format!("db: {e}"))?;

    // No ON DELETE CASCADE anymore (CRR tables may not have checked FKs) —
    // remove links, version history and managed files explicitly so the
    // deletions propagate via CRDT.
    for (nid,) in &ids {
        sqlx::query("DELETE FROM note_links WHERE source_id = ? OR target_id = ?")
            .bind(nid).bind(nid).execute(db).await.map_err(|e| format!("db: {e}"))?;
        sqlx::query("DELETE FROM note_versions WHERE note_id = ?")
            .bind(nid).execute(db).await.map_err(|e| format!("db: {e}"))?;
        sqlx::query("DELETE FROM files WHERE parent_id = ?")
            .bind(nid).execute(db).await.map_err(|e| format!("db: {e}"))?;
        sqlx::query("DELETE FROM notes WHERE id = ?")
            .bind(nid).execute(db).await.map_err(|e| format!("db: {e}"))?;
    }
    Ok(())
}

/// List all notes regardless of parent, ordered by updated_at DESC
#[instrument(skip(db))]
pub async fn list_all_notes(db: &SqlitePool, vault_id: &str) -> Result<Vec<Note>, String> {
    sqlx::query_as::<_, Note>("SELECT * FROM notes WHERE vault_id = ? ORDER BY updated_at DESC")
        .bind(vault_id)
        .fetch_all(db)
        .await
        .map_err(|e| format!("db: {e}"))
}

/// Move a note: update parent_id and/or sort_order.
/// `parent_id = None` clears the parent (moves the note to the root level).
#[instrument(skip(db))]
pub async fn move_note(
    db: &SqlitePool,
    id: &str,
    parent_id: Option<&str>,
    sort_order: Option<i32>,
) -> Result<Note, String> {
    let now = time::now_iso();

    // System folders cannot be moved (they stay at the tree root).
    if parent_id.is_some() {
        let sys: Option<(i32,)> = sqlx::query_as("SELECT is_system FROM notes WHERE id = ?")
            .bind(id)
            .fetch_optional(db)
            .await
            .map_err(|e| format!("db: {e}"))?;
        if let Some((1,)) = sys {
            return Err("不能移动系统目录「我的图书馆」".to_string());
        }
    }

    // Always apply parent_id (None binds SQL NULL → move to root).
    sqlx::query("UPDATE notes SET parent_id = ?, updated_at = ? WHERE id = ?")
        .bind(parent_id)
        .bind(&now)
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    if let Some(so) = sort_order {
        sqlx::query("UPDATE notes SET sort_order = ?, updated_at = ? WHERE id = ?")
            .bind(so)
            .bind(&now)
            .bind(id)
            .execute(db)
            .await
            .map_err(|e| format!("db: {e}"))?;
    }

    get_note(db, id).await
}

/// List notes, optionally filtered by paper
pub async fn list_notes(
    db: &SqlitePool,
    paper_id: Option<&str>,
    search: Option<&str>,
    parent_id: Option<&str>,
) -> Result<Vec<Note>, String> {
    let mut sql = String::from("SELECT * FROM notes WHERE 1=1");
    let mut params: Vec<String> = Vec::new();

    if let Some(pid) = paper_id {
        sql.push_str(" AND paper_id = ?");
        params.push(pid.to_string());
    }
    if let Some(s) = search {
        sql.push_str(" AND (title LIKE ? OR content_plain LIKE ?)");
        let p = format!("%{}%", s);
        params.push(p.clone()); params.push(p);
    }
    if let Some(pid) = parent_id {
        sql.push_str(" AND parent_id = ?");
        params.push(pid.to_string());
    } else if paper_id.is_none() {
        // No parent scope and no paper scope → root-level only (notes pane).
        // When filtering by paper, include notes at any depth (e.g. notes
        // created under the paper's collection folder tree).
        sql.push_str(" AND parent_id IS NULL");
    }

    sql.push_str(" ORDER BY updated_at DESC LIMIT 50");

    let mut q = sqlx::query_as::<_, Note>(&sql);
    for p in &params { q = q.bind(p); }

    q.fetch_all(db).await.map_err(|e| format!("db: {e}"))
}

/// Full-text search across all notes (FTS5 with LIKE fallback).
/// Returns ranked results with a snippet of the matched content.
#[instrument(skip(db))]
pub async fn search_notes(
    db: &SqlitePool,
    query: &str,
    limit: i64,
    vault_id: &str,
) -> Result<Vec<serde_json::Value>, String> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let match_expr = query
        .split_whitespace()
        .map(|t| format!("\"{}\"*", t.trim_matches('"')))
        .collect::<Vec<_>>()
        .join(" ");

    let rows: Vec<(String, String, String, String)> = match sqlx::query_as(
        "SELECT n.id, n.title, n.content, n.updated_at FROM notes_fts f JOIN notes n ON n.rowid = f.rowid \
         WHERE notes_fts MATCH ? AND n.vault_id = ? ORDER BY bm25(notes_fts) LIMIT ?"
    )
    .bind(&match_expr)
    .bind(vault_id)
    .bind(limit)
    .fetch_all(db)
    .await
    {
        Ok(rows) => rows,
        Err(_) => {
            let pattern = format!("%{}%", query);
            sqlx::query_as::<_, (String, String, String, String)>(
                "SELECT id, title, content, updated_at FROM notes \
                 WHERE vault_id = ? AND (title LIKE ? OR content_plain LIKE ?) ORDER BY updated_at DESC LIMIT ?"
            )
            .bind(vault_id)
            .bind(&pattern)
            .bind(&pattern)
            .bind(limit)
            .fetch_all(db)
            .await
            .map_err(|e| format!("db: {e}"))?
        }
    };

    Ok(rows
        .into_iter()
        .map(|(id, title, content, updated_at)| {
            let snippet = make_snippet(&content, query, 160);
            serde_json::json!({ "id": id, "title": title, "snippet": snippet, "updated_at": updated_at })
        })
        .collect())
}

/// Round a byte offset down to the nearest char boundary (CJK-safe).
fn floor_char_boundary(s: &str, mut idx: usize) -> usize {
    idx = idx.min(s.len());
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

/// Build a short snippet around the first query match in content.
fn make_snippet(content: &str, query: &str, max_len: usize) -> String {
    let plain = strip_markdown(content);
    let lower = plain.to_lowercase();
    let q = query.to_lowercase();
    let pos = lower.find(&q).unwrap_or(0);
    let start = floor_char_boundary(&plain, pos.saturating_sub(40));
    // Ensure the snippet always covers the matched keyword.
    let end = floor_char_boundary(&plain, (start + max_len).max(pos + q.len()).min(plain.len()));
    let mut snippet = plain[start..end].to_string();
    if start > 0 {
        snippet.insert_str(0, "…");
    }
    if end < plain.len() {
        snippet.push('…');
    }
    snippet
}

/// Get backlinks (notes that link to this note) — restricted to the same vault.
pub async fn get_backlinks(db: &SqlitePool, note_id: &str, vault_id: &str) -> Result<Vec<(Note, String)>, String> {
    let rows = sqlx::query_as::<_, (String, String)>(
        "SELECT nl.source_id, nl.context FROM note_links nl \
         JOIN notes src ON src.id = nl.source_id \
         WHERE nl.target_id = ? AND src.vault_id = ?"
    ).bind(note_id).bind(vault_id).fetch_all(db).await.map_err(|e| format!("db: {e}"))?;

    let mut results = Vec::new();
    for (source_id, context) in rows {
        if let Ok(note) = get_note(db, &source_id).await {
            results.push((note, context));
        }
    }
    Ok(results)
}

/// Parse [[wiki-links]] from Markdown content. Byte-based scan so indices
/// always land on char boundaries (safe for CJK content).
fn parse_wiki_links(content: &str) -> Vec<String> {
    let bytes = content.as_bytes();
    let mut links = Vec::new();
    let mut i = 0;
    while i + 3 <= bytes.len() {
        if bytes[i] == b'[' && bytes[i + 1] == b'[' {
            let start = i + 2;
            let mut j = start;
            let mut found = false;
            while j + 1 < bytes.len() {
                if bytes[j] == b']' && bytes[j + 1] == b']' {
                    found = true;
                    break;
                }
                j += 1;
            }
            if found {
                let link = content[start..j].trim().to_string();
                if !link.is_empty() && !links.contains(&link) {
                    links.push(link);
                }
                i = j + 2;
                continue;
            }
        }
        i += 1;
    }
    links
}

/// Update note_links for a note
async fn update_note_links(db: &SqlitePool, source_id: &str, targets: &[String]) -> Result<(), String> {
    // Remove old links
    sqlx::query("DELETE FROM note_links WHERE source_id = ?")
        .bind(source_id).execute(db).await.map_err(|e| format!("db: {e}"))?;

    // Find target note IDs by title
    for target in targets {
        // Try exact title match first
        let target_note: Option<(String,)> = sqlx::query_as(
            "SELECT id FROM notes WHERE title = ? LIMIT 1"
        ).bind(target).fetch_optional(db).await.map_err(|e| format!("db: {e}"))?;

        let now = time::now_iso();
        if let Some((target_id,)) = target_note {
            sqlx::query(
                "INSERT OR IGNORE INTO note_links (source_id, target_id, context, created_at) VALUES (?, ?, ?, ?)"
            ).bind(source_id).bind(&target_id).bind(target).bind(&now)
             .execute(db).await.map_err(|e| format!("db: {e}"))?;
        }
    }
    Ok(())
}

/// Strip Markdown syntax to plain text
fn strip_markdown(content: &str) -> String {
    // Simple: just remove common markdown characters
    content
        .replace("**", "").replace("__", "").replace('*', "").replace('_', "")
        .replace("`", "").replace('#', "").replace("> ", "")
        .replace("[", "").replace("]", "").replace("(", "").replace(")", "")
}

/// Parse `#tags` from content. A tag is a `#` preceded by start-of-line or
/// whitespace, followed by non-space/non-`#` characters. Fenced code blocks
/// are skipped so `#tag` inside code is not collected.
fn parse_tags(content: &str) -> String {
    let mut tags: Vec<String> = Vec::new();
    let mut in_code = false;
    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            continue;
        }
        // Scan for `#tag` at word boundaries.
        let bytes = line.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'#' {
                let preceded_ok = i == 0 || bytes[i - 1].is_ascii_whitespace();
                let mut j = i + 1;
                while j < bytes.len()
                    && !bytes[j].is_ascii_whitespace()
                    && bytes[j] != b'#'
                    && bytes[j] != b'('
                    && bytes[j] != b')'
                    && bytes[j] != b','
                    && bytes[j] != b']'
                    && bytes[j] != b'['
                {
                    j += 1;
                }
                if preceded_ok && j > i + 1 {
                    let tag = line[i + 1..j].to_string();
                    if !tags.contains(&tag) {
                        tags.push(tag);
                    }
                }
                i = j;
            } else {
                i += 1;
            }
        }
    }
    serde_json::to_string(&tags).unwrap_or_else(|_| "[]".to_string())
}

/// Reserved name for the system library folder (paper-mapped note tree root).
pub const SYSTEM_LIBRARY_NAME: &str = "我的图书馆";

/// Find or create the system "我的图书馆" folder note for a vault.
pub async fn ensure_system_library_folder(db: &SqlitePool, vault_id: &str) -> Result<String, String> {
    let existing: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM notes WHERE vault_id = ? AND is_system = 1 AND is_folder = 1 LIMIT 1"
    )
    .bind(vault_id)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("db: {e}"))?;
    if let Some((id,)) = existing {
        return Ok(id);
    }
    let folder = create_note(db, SYSTEM_LIBRARY_NAME, "", None, None, vault_id, true).await?;
    sqlx::query("UPDATE notes SET is_system = 1, updated_at = ? WHERE id = ?")
        .bind(crate::core::time::now_iso())
        .bind(&folder.id)
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    Ok(folder.id)
}

/// Find or create a collection-mapped folder note under a parent, linked to the
/// collection so renames/moves can be synced later.
async fn ensure_collection_folder(
    db: &SqlitePool,
    vault_id: &str,
    parent_id: Option<String>,
    name: &str,
    collection_id: &str,
) -> Result<String, String> {
    let existing: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM notes WHERE vault_id = ? AND is_folder = 1 \
         AND source_collection_id = ? AND parent_id IS ?"
    )
    .bind(vault_id)
    .bind(collection_id)
    .bind(&parent_id)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("db: {e}"))?;
    if let Some((id,)) = existing {
        return Ok(id);
    }
    let folder = create_note(db, name, "", None, parent_id.as_deref(), vault_id, true).await?;
    sqlx::query("UPDATE notes SET source_collection_id = ?, updated_at = ? WHERE id = ?")
        .bind(collection_id)
        .bind(crate::core::time::now_iso())
        .bind(&folder.id)
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    Ok(folder.id)
}

/// Resolve the deepest collection folder note for a paper, creating the
/// system library root and collection folders as needed. `None` = root.
pub async fn collection_parent_for_paper(
    db: &SqlitePool,
    paper_id: &str,
    vault_id: &str,
) -> Result<Option<String>, String> {
    // First collection of the paper (if any).
    let col: Option<(String, Option<String>, String)> = sqlx::query_as(
        "SELECT c.id, c.parent_id, c.name FROM paper_collections pc \
         JOIN collections c ON c.id = pc.collection_id \
         WHERE pc.paper_id = ? ORDER BY c.created_at LIMIT 1"
    )
    .bind(paper_id)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("db: {e}"))?;

    let Some((col_id, col_parent, col_name)) = col else {
        // Even papers without a collection live under the system library root
        // so literature notes are grouped together instead of scattered at the
        // vault root.
        let sys = ensure_system_library_folder(db, vault_id).await?;
        return Ok(Some(sys));
    };

    // Build the collection path from root to leaf.
    let mut chain = vec![(col_id.clone(), col_name, col_parent.clone())];
    let mut cur = col_parent;
    let mut guard = 0;
    while let Some(pid) = cur {
        let row: Option<(String, Option<String>, String)> =
            sqlx::query_as("SELECT id, parent_id, name FROM collections WHERE id = ?")
                .bind(&pid)
                .fetch_optional(db)
                .await
                .map_err(|e| format!("db: {e}"))?;
        match row {
            Some((id, p, name)) => {
                chain.push((id, name, p.clone()));
                cur = p;
            }
            None => break,
        }
        guard += 1;
        if guard > 30 {
            break;
        }
    }
    chain.reverse();

    // Map each collection to a folder under the system library root.
    let sys = ensure_system_library_folder(db, vault_id).await?;
    let mut parent = Some(sys);
    for (cid, name, _) in chain {
        // The library root itself is already the system folder — skip it.
        if name == SYSTEM_LIBRARY_NAME {
            continue;
        }
        parent = Some(ensure_collection_folder(db, vault_id, parent, &name, &cid).await?);
    }

    Ok(parent)
}

/// Create a note under the paper's collection folder tree:
/// `我的图书馆/<集合1>/<集合2>/.../<笔记>`. Without a collection it goes to root.
pub async fn create_note_under_paper(
    db: &SqlitePool,
    paper_id: &str,
    title: &str,
    content: &str,
    vault_id: &str,
) -> Result<Note, String> {
    let parent = collection_parent_for_paper(db, paper_id, vault_id).await?;
    create_note(db, title, content, Some(paper_id), parent.as_deref(), vault_id, false).await
}

/// Append an excerpt to the paper's excerpt-collection note (titled with the
/// paper's title), creating it on first use. Multiple excerpts accumulate.
pub async fn add_excerpt_to_paper(
    db: &SqlitePool,
    paper_id: &str,
    excerpt: &str,
    vault_id: &str,
) -> Result<Note, String> {
    let paper_title: Option<(String,)> = sqlx::query_as("SELECT title FROM papers WHERE id = ?")
        .bind(paper_id)
        .fetch_optional(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    let title = paper_title
        .map(|(t,)| t)
        .unwrap_or_else(|| "文献摘录".to_string());

    let parent = collection_parent_for_paper(db, paper_id, vault_id).await?;
    let existing: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM notes WHERE paper_id = ? AND is_excerpt = 1 AND parent_id IS ?"
    )
    .bind(paper_id)
    .bind(&parent)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("db: {e}"))?;

    match existing {
        Some((id,)) => {
            let note = get_note(db, &id).await?;
            let new_content = format!("{}\n\n---\n\n{}", note.content, excerpt);
            update_note(db, &id, None, Some(&new_content), None, None, None, None).await
        }
        None => {
            let note = create_note(db, &title, excerpt, Some(paper_id), parent.as_deref(), vault_id, false).await?;
            sqlx::query("UPDATE notes SET is_excerpt = 1, updated_at = ? WHERE id = ?")
                .bind(crate::core::time::now_iso())
                .bind(&note.id)
                .execute(db)
                .await
                .map_err(|e| format!("db: {e}"))?;
            Ok(note)
        }
    }
}

/// Merge a standalone note's content into the paper's excerpt note
/// (`is_excerpt = 1`), then delete the standalone note.
#[instrument(skip(db))]
pub async fn merge_note_into_paper_note(
    db: &SqlitePool,
    note_id: &str,
    paper_id: &str,
    vault_id: &str,
) -> Result<Note, String> {
    let note = get_note(db, note_id).await?;
    if note.is_excerpt == 1 {
        return Err("该笔记已是摘录笔记".to_string());
    }
    if note.paper_id.as_deref() != Some(paper_id) {
        return Err("笔记与文献不匹配".to_string());
    }
    if note.content.trim().is_empty() {
        return Err("笔记内容为空，无需合并".to_string());
    }
    let target = add_excerpt_to_paper(db, paper_id, &note.content, vault_id).await?;
    delete_note(db, note_id).await?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_wiki_links_cjk() {
        let content = "开头[[中文链接]]中间[[英文 link]]结尾[[子目录/深层 链接]]。";
        let links = parse_wiki_links(content);
        assert_eq!(links, vec!["中文链接", "英文 link", "子目录/深层 链接"]);
    }

    #[test]
    fn parse_wiki_links_no_panic_on_plain_cjk() {
        // 无链接的纯中文内容，字符索引与字节索引不一致也不能 panic
        let content = "这是一段完全没有任何链接标记的中文内容，用于验证字节边界安全。";
        let links = parse_wiki_links(content);
        assert!(links.is_empty());
    }

    #[test]
    fn make_snippet_cjk() {
        let content = "这是一段很长的中文内容，其中包含一个需要搜索的关键词，后面还有更多文字用来拉长内容以便截断产生省略号前后缀。";
        let s = make_snippet(content, "关键词", 40);
        assert!(s.contains("关键词"));
    }

    #[tokio::test]
    async fn delete_note_removes_whole_subtree() {
        let dir = std::env::temp_dir().join(format!(
            "siku-note-del-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = crate::core::db::tests::connect_with_crsqlite(&dir.join("m.db"))
            .await
            .unwrap();

        sqlx::query("CREATE TABLE notes (id TEXT PRIMARY KEY, parent_id TEXT, is_system INTEGER NOT NULL DEFAULT 0)")
            .execute(&db).await.unwrap();
        sqlx::query("CREATE TABLE note_links (source_id TEXT, target_id TEXT)")
            .execute(&db).await.unwrap();
        sqlx::query("CREATE TABLE note_versions (note_id TEXT)")
            .execute(&db).await.unwrap();

        // Tree: folder → child → grandchild; sibling stays at the root level.
        sqlx::query("INSERT INTO notes (id, parent_id) VALUES ('folder', NULL), ('child', 'folder'), ('grandchild', 'child'), ('sibling', NULL)")
            .execute(&db).await.unwrap();
        sqlx::query("INSERT INTO note_links (source_id, target_id) VALUES ('child', 'sibling'), ('sibling', 'grandchild')")
            .execute(&db).await.unwrap();
        sqlx::query("INSERT INTO note_versions (note_id) VALUES ('child'), ('grandchild'), ('sibling')")
            .execute(&db).await.unwrap();

        delete_note(&db, "folder").await.unwrap();

        let remaining: Vec<(String,)> = sqlx::query_as("SELECT id FROM notes")
            .fetch_all(&db).await.unwrap();
        assert_eq!(remaining, vec![("sibling".to_string(),)]);
        let links: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM note_links")
            .fetch_one(&db).await.unwrap();
        assert_eq!(links, 0, "links touching any deleted note must be gone");
        let versions: Vec<(String,)> = sqlx::query_as("SELECT note_id FROM note_versions")
            .fetch_all(&db).await.unwrap();
        assert_eq!(versions, vec![("sibling".to_string(),)]);

        // System folders stay protected.
        sqlx::query("INSERT INTO notes (id, parent_id, is_system) VALUES ('sysroot', NULL, 1)")
            .execute(&db).await.unwrap();
        assert!(delete_note(&db, "sysroot").await.is_err());
        let sys_alive: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notes WHERE id = 'sysroot'")
            .fetch_one(&db).await.unwrap();
        assert_eq!(sys_alive, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
