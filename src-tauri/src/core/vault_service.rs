use sqlx::SqlitePool;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::instrument;

use crate::core::db::DEFAULT_VAULT_ID;
use crate::core::models::{Note, Vault};
use crate::core::settings_service::{get_setting, set_setting};
use crate::core::time;

pub const CURRENT_VAULT_KEY: &str = "notes.current_vault_id";
pub const DEFAULT_VAULT_NAME: &str = "cognitive-archive";

/// Seed the default vault and the current-vault setting on first run.
/// The default vault uses a fixed uuid so every device converges on one row.
pub async fn ensure_defaults(db: &SqlitePool) -> Result<(), String> {
    let count: Option<(i64,)> = sqlx::query_as("SELECT COUNT(*) FROM vaults")
        .fetch_optional(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    if count.map(|c| c.0).unwrap_or(0) == 0 {
        let now = time::now_iso();
        sqlx::query("INSERT INTO vaults (id, name, created_at, updated_at) VALUES (?, ?, ?, ?)")
            .bind(DEFAULT_VAULT_ID)
            .bind(DEFAULT_VAULT_NAME)
            .bind(&now)
            .bind(&now)
            .execute(db)
            .await
            .map_err(|e| format!("db: {e}"))?;
    }

    if get_setting(db, CURRENT_VAULT_KEY).await?.is_none() {
        set_setting(db, CURRENT_VAULT_KEY, DEFAULT_VAULT_ID).await?;
    }
    Ok(())
}

#[instrument(skip(db))]
pub async fn list_vaults(db: &SqlitePool) -> Result<Vec<Vault>, String> {
    sqlx::query_as::<_, Vault>("SELECT * FROM vaults ORDER BY created_at ASC, id ASC")
        .fetch_all(db)
        .await
        .map_err(|e| format!("db: {e}"))
}

#[instrument(skip(db))]
pub async fn get_vault(db: &SqlitePool, id: &str) -> Result<Vault, String> {
    sqlx::query_as::<_, Vault>("SELECT * FROM vaults WHERE id = ?")
        .bind(id)
        .fetch_optional(db)
        .await
        .map_err(|e| format!("db: {e}"))?
        .ok_or_else(|| format!("vault not found: {id}"))
}

/// The currently active vault id (persisted in the settings table).
pub async fn get_current_vault_id(db: &SqlitePool) -> Result<String, String> {
    let v = get_setting(db, CURRENT_VAULT_KEY).await?;
    Ok(v.unwrap_or_else(|| DEFAULT_VAULT_ID.to_string()))
}

pub async fn set_current_vault_id(db: &SqlitePool, id: &str) -> Result<(), String> {
    get_vault(db, id).await?; // validate existence
    set_setting(db, CURRENT_VAULT_KEY, id).await
}

#[instrument(skip(db))]
pub async fn create_vault(db: &SqlitePool, name: &str) -> Result<Vault, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("库名称不能为空".to_string());
    }
    // No UNIQUE(name) constraint anymore (CRDT-incompatible) — check here.
    let conflict: Option<(String,)> = sqlx::query_as("SELECT id FROM vaults WHERE name = ?")
        .bind(name)
        .fetch_optional(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    if conflict.is_some() {
        return Err(format!("已存在同名库「{name}」"));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let now = time::now_iso();
    sqlx::query("INSERT INTO vaults (id, name, created_at, updated_at) VALUES (?, ?, ?, ?)")
        .bind(&id)
        .bind(name)
        .bind(&now)
        .bind(&now)
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    get_vault(db, &id).await
}

#[instrument(skip(db))]
pub async fn rename_vault(db: &SqlitePool, id: &str, name: &str) -> Result<Vault, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("库名称不能为空".to_string());
    }
    let now = time::now_iso();
    sqlx::query("UPDATE vaults SET name = ?, updated_at = ? WHERE id = ?")
        .bind(name)
        .bind(&now)
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    get_vault(db, id).await
}

/// Delete a vault together with all its notes (Obsidian removes the vault folder).
#[instrument(skip(db))]
pub async fn delete_vault(db: &SqlitePool, id: &str) -> Result<(), String> {
    // Keep the default vault around — you cannot delete the last vault.
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM vaults")
        .fetch_one(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    if count <= 1 {
        return Err("不能删除最后一个库".to_string());
    }

    // Remove links pointing to/from notes inside this vault, then the notes.
    sqlx::query(
        "DELETE FROM note_links WHERE source_id IN (SELECT id FROM notes WHERE vault_id = ?) \
         OR target_id IN (SELECT id FROM notes WHERE vault_id = ?)"
    )
    .bind(id)
    .bind(id)
    .execute(db)
    .await
    .map_err(|e| format!("db: {e}"))?;
    // No ON DELETE CASCADE on note_versions anymore (CRR tables may not have
    // checked FKs) — remove version history explicitly.
    sqlx::query(
        "DELETE FROM note_versions WHERE note_id IN (SELECT id FROM notes WHERE vault_id = ?)"
    )
    .bind(id)
    .execute(db)
    .await
    .map_err(|e| format!("db: {e}"))?;
    sqlx::query("DELETE FROM notes WHERE vault_id = ?")
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    // Managed files belong to the vault too (no checked FKs on CRR tables).
    sqlx::query("DELETE FROM files WHERE vault_id = ?")
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    sqlx::query("DELETE FROM vaults WHERE id = ?")
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;

    // If the deleted vault was current, fall back to the first remaining one.
    if get_current_vault_id(db).await? == id {
        let first: String = sqlx::query_scalar("SELECT id FROM vaults ORDER BY id ASC LIMIT 1")
            .fetch_one(db)
            .await
            .map_err(|e| format!("db: {e}"))?;
        set_current_vault_id(db, &first).await?;
    }
    Ok(())
}

// ── Markdown export / import ─────────────────────────────────────────

const SKIP_DIRS: [&str; 4] = [".obsidian", ".trash", ".git", "node_modules"];

fn sanitize_filename(name: &str) -> String {
    let invalid = ['\\', '/', ':', '*', '?', '"', '<', '>', '|'];
    let cleaned: String = name
        .chars()
        .map(|c| if invalid.contains(&c) { '_' } else { c })
        .collect();
    let cleaned = cleaned.trim().trim_end_matches('.');
    if cleaned.is_empty() {
        "未命名".to_string()
    } else {
        cleaned.to_string()
    }
}

fn unique_path(dir: &Path, base: &str, ext: &str) -> PathBuf {
    let mut p = dir.join(format!("{base}.{ext}"));
    let mut n = 2;
    while p.exists() {
        p = dir.join(format!("{base} ({n}).{ext}"));
        n += 1;
    }
    p
}

/// Export a vault's notes as Markdown files (folders mirror the parent
/// hierarchy). Each note becomes `<parent-title>/<title>.md` with YAML
/// frontmatter (aliases/tags/created/updated) so it can be re-imported or
/// opened directly in Obsidian.
#[instrument(skip(db))]
pub async fn export_vault(db: &SqlitePool, vault_id: &str, target_dir: &str) -> Result<usize, String> {
    let notes: Vec<Note> = sqlx::query_as::<_, Note>("SELECT * FROM notes WHERE vault_id = ?")
        .bind(vault_id)
        .fetch_all(db)
        .await
        .map_err(|e| format!("db: {e}"))?;

    let mut children: HashMap<Option<&str>, Vec<&Note>> = HashMap::new();
    for n in &notes {
        children.entry(n.parent_id.as_deref()).or_default().push(n);
    }

    let root = Path::new(target_dir);
    std::fs::create_dir_all(root).map_err(|e| format!("创建目录失败: {e}"))?;

    fn write_tree<'a>(
        nodes: Vec<&'a Note>,
        dir: &Path,
        children: &mut HashMap<Option<&'a str>, Vec<&'a Note>>,
        count: &mut usize,
    ) -> Result<(), String> {
        for note in nodes {
            let file = unique_path(dir, &sanitize_filename(&note.title), "md");
            let mut fm = String::new();
            fm.push_str("---\n");
            fm.push_str(&format!("created: {}\n", note.created_at));
            fm.push_str(&format!("updated: {}\n", note.updated_at));
            if let Ok(aliases) = serde_json::from_str::<Vec<String>>(&note.aliases) {
                if !aliases.is_empty() {
                    fm.push_str("aliases:\n");
                    for a in aliases {
                        fm.push_str(&format!("  - {a}\n"));
                    }
                }
            }
            if let Ok(tags) = serde_json::from_str::<Vec<String>>(&note.tags) {
                if !tags.is_empty() {
                    fm.push_str("tags:\n");
                    for t in tags {
                        fm.push_str(&format!("  - {t}\n"));
                    }
                }
            }
            fm.push_str("---\n\n");
            std::fs::write(&file, format!("{fm}{}", note.content))
                .map_err(|e| format!("写入文件失败: {e}"))?;
            *count += 1;

            // Sub-notes go into a folder named after this note's title.
            let sub = children.remove(&Some(note.id.as_str())).unwrap_or_default();
            if !sub.is_empty() {
                let sub_dir = dir.join(sanitize_filename(&note.title));
                std::fs::create_dir_all(&sub_dir).map_err(|e| format!("创建目录失败: {e}"))?;
                write_tree(sub, &sub_dir, children, count)?;
            }
        }
        Ok(())
    }

    let mut count = 0usize;
    write_tree(children.remove(&None).unwrap_or_default(), root, &mut children, &mut count)?;
    Ok(count)
}

struct Frontmatter {
    aliases: Vec<String>,
    tags: Vec<String>,
    created_at: Option<String>,
    updated_at: Option<String>,
}

fn split_csv(s: &str) -> Vec<String> {
    s.split(',')
        .map(|x| {
            x.trim()
                .trim_matches('#')
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_string()
        })
        .filter(|x| !x.is_empty())
        .collect()
}

/// Extract a leading YAML frontmatter block (`---` ... `---`). Returns the
/// parsed fields and the body text (frontmatter stripped).
fn parse_frontmatter(content: &str) -> (Option<Frontmatter>, &str) {
    if !content.starts_with("---\n") {
        return (None, content);
    }
    let after = &content[4..];
    let Some(rel_end) = after.find("\n---") else {
        return (None, content);
    };
    let fm = &after[..rel_end];
    let rest = &after[rel_end + 4..];
    let body = rest.strip_prefix('\n').unwrap_or(rest);

    let mut aliases: Vec<String> = Vec::new();
    let mut tags: Vec<String> = Vec::new();
    let mut created_at: Option<String> = None;
    let mut updated_at: Option<String> = None;
    let mut in_aliases = false;
    let mut in_tags = false;

    for line in fm.lines() {
        let t = line.trim();
        if let Some(item) = t.strip_prefix("- ") {
            let val = item.trim().trim_matches('"').trim_matches('\'').to_string();
            if in_aliases {
                aliases.push(val.clone());
            }
            if in_tags {
                tags.push(val);
            }
            continue;
        }
        if let Some((k, v)) = t.split_once(':') {
            let key = k.trim().to_lowercase();
            let raw = v.trim();
            let val = raw.trim_matches('"').trim_matches('\'').trim();
            in_aliases = key == "aliases";
            in_tags = key == "tags";
            match key.as_str() {
                "created" | "date" => created_at = Some(val.to_string()),
                "updated" | "modified" => updated_at = Some(val.to_string()),
                "aliases" => {
                    if !val.is_empty() {
                        aliases.extend(split_csv(val));
                    }
                }
                "tags" => {
                    if !val.is_empty() {
                        tags.extend(split_csv(val));
                    }
                }
                _ => {}
            }
        }
    }
    (
        Some(Frontmatter {
            aliases,
            tags,
            created_at,
            updated_at,
        }),
        body,
    )
}

/// Import a folder (an Obsidian-style vault) into a vault. Directories become
/// folder notes; each `.md` file becomes a note with its frontmatter
/// (aliases/tags/created/updated) preserved; every other file (PDF/Word/Excel/
/// images/...) is imported as a vault-managed file (blob store) under the
/// folder it came from. Re-importing the same directory is idempotent:
/// folders are reused, unchanged notes/files are skipped, changed files update
/// their blob in place, and changed notes are imported as new copies (local
/// edits are never overwritten). `on_progress(current, total, name)` is
/// called as each item is processed. Returns `{ imported, files_imported,
/// unchanged, skipped }` where `skipped` counts files that failed to import.
#[instrument(skip(db, on_progress))]
pub async fn import_vault(
    db: &SqlitePool,
    app_data_dir: &Path,
    vault_id: &str,
    source_dir: &str,
    on_progress: &(dyn Fn(usize, usize, &str) + Sync),
) -> Result<serde_json::Value, String> {
    let root = Path::new(source_dir);
    if !root.is_dir() {
        return Err("目录不存在".to_string());
    }

    // 1. Collect relative directory paths, shallowest first, so parents are
    //    created before their children.
    fn collect_dirs(dir: &Path, rel: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
        for entry in std::fs::read_dir(dir).map_err(|e| format!("读取目录失败: {e}"))? {
            let entry = entry.map_err(|e| format!("读取目录失败: {e}"))?;
            let name = entry.file_name().to_string_lossy().to_string();
            if SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                let rel_sub = rel.join(&name);
                out.push(rel_sub.clone());
                collect_dirs(&path, &rel_sub, out)?;
            }
        }
        Ok(())
    }
    let mut dirs: Vec<PathBuf> = Vec::new();
    collect_dirs(root, Path::new(""), &mut dirs)?;
    dirs.sort_by_key(|p| p.components().count());

    // 2. Walk all files, splitting markdown notes from managed-file assets.
    let mut md_files: Vec<(PathBuf, PathBuf)> = Vec::new(); // (abs, rel)
    let mut asset_files: Vec<(PathBuf, PathBuf)> = Vec::new();
    fn collect_files(dir: &Path, rel: &Path, md: &mut Vec<(PathBuf, PathBuf)>, assets: &mut Vec<(PathBuf, PathBuf)>) -> Result<(), String> {
        for entry in std::fs::read_dir(dir).map_err(|e| format!("读取目录失败: {e}"))? {
            let entry = entry.map_err(|e| format!("读取目录失败: {e}"))?;
            let name = entry.file_name().to_string_lossy().to_string();
            if SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            let path = entry.path();
            let rel_sub = rel.join(&name);
            if path.is_dir() {
                collect_files(&path, &rel_sub, md, assets)?;
            } else if name.ends_with(".md") {
                md.push((path, rel_sub));
            } else {
                assets.push((path, rel_sub));
            }
        }
        Ok(())
    }
    collect_files(root, Path::new(""), &mut md_files, &mut asset_files)?;

    let total = dirs.len() + md_files.len() + asset_files.len();
    let mut current = 0usize;

    // Idempotent re-import support: index existing items by (parent, name) so
    // re-importing the same directory reuses folders, skips unchanged notes
    // and files, and updates changed files in place (stable ids) instead of
    // duplicating everything.
    let mut existing_folders: HashMap<(Option<String>, String), String> = HashMap::new();
    {
        let rows: Vec<(String, Option<String>, String)> = sqlx::query_as(
            "SELECT id, parent_id, title FROM notes WHERE vault_id = ? AND is_folder = 1",
        )
        .bind(vault_id)
        .fetch_all(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
        for (id, pid, title) in rows {
            existing_folders.insert((pid, title), id);
        }
    }
    let mut existing_notes: HashMap<(Option<String>, String), String> = HashMap::new();
    {
        let rows: Vec<(Option<String>, String, String)> = sqlx::query_as(
            "SELECT parent_id, title, content FROM notes WHERE vault_id = ? AND is_folder = 0",
        )
        .bind(vault_id)
        .fetch_all(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
        for (pid, title, content) in rows {
            existing_notes.insert((pid, title), content);
        }
    }
    let mut existing_files: HashMap<(Option<String>, String), (String, String)> = HashMap::new();
    {
        let rows: Vec<(String, Option<String>, String, String)> = sqlx::query_as(
            "SELECT id, parent_id, name, blob_path FROM files WHERE vault_id = ?",
        )
        .bind(vault_id)
        .fetch_all(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
        for (id, pid, name, blob) in rows {
            existing_files.insert((pid, name), (id, blob));
        }
    }

    // 3. Create a folder note for each directory (rel path -> note id).
    let mut folder_ids: HashMap<PathBuf, String> = HashMap::new();
    for rel in &dirs {
        let parent_rel = rel.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let parent_id = if parent_rel.as_os_str().is_empty() {
            None
        } else {
            folder_ids.get(&parent_rel).cloned()
        };
        let name = rel
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        // Re-import: reuse an existing folder with the same name and parent.
        if let Some(existing_id) = existing_folders.get(&(parent_id.clone(), name.clone())) {
            folder_ids.insert(rel.clone(), existing_id.clone());
            current += 1;
            on_progress(current, total, &name);
            continue;
        }
        let folder =
            crate::core::note_service::create_note(db, &name, "", None, parent_id.as_deref(), vault_id, true).await?;
        folder_ids.insert(rel.clone(), folder.id);
        current += 1;
        on_progress(current, total, &name);
    }

    let folder_of = |rel: &Path| -> Option<String> {
        let parent_rel = rel.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        if parent_rel.as_os_str().is_empty() {
            None
        } else {
            folder_ids.get(&parent_rel).cloned()
        }
    };

    // 4. Import markdown notes. Re-import: a note with the same title, parent
    //    and identical content is skipped; changed content creates a new note
    //    (never overwrite local edits made in Siku).
    let mut imported = 0usize;
    let mut unchanged = 0usize;
    for (abs, rel) in md_files {
        let content = std::fs::read_to_string(&abs).map_err(|e| format!("读取文件失败: {e}"))?;
        let (fm, body) = parse_frontmatter(&content);
        let stem = abs
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let title = if stem.is_empty() { "未命名".to_string() } else { stem };

        let parent_id = folder_of(&rel);
        if existing_notes
            .get(&(parent_id.clone(), title.clone()))
            .map(|c| c.as_str() == body)
            .unwrap_or(false)
        {
            unchanged += 1;
            current += 1;
            on_progress(current, total, &title);
            continue;
        }

        let note =
            crate::core::note_service::create_note(db, &title, body, None, parent_id.as_deref(), vault_id, false).await?;

        // Preserve frontmatter metadata (fall back to the freshly created note).
        let now = time::now_iso();
        sqlx::query("UPDATE notes SET created_at = ?, updated_at = ? WHERE id = ?")
            .bind(fm.as_ref().and_then(|f| f.created_at.clone()).unwrap_or_else(|| now.clone()))
            .bind(fm.as_ref().and_then(|f| f.updated_at.clone()).unwrap_or(now))
            .bind(&note.id)
            .execute(db)
            .await
            .map_err(|e| format!("db: {e}"))?;
        if let Some(f) = &fm {
            if !f.aliases.is_empty() {
                let aliases = serde_json::to_string(&f.aliases).map_err(|e| format!("json: {e}"))?;
                sqlx::query("UPDATE notes SET aliases = ?, updated_at = ? WHERE id = ?")
                    .bind(aliases)
                    .bind(crate::core::time::now_iso())
                    .bind(&note.id)
                    .execute(db)
                    .await
                    .map_err(|e| format!("db: {e}"))?;
            }
            if !f.tags.is_empty() {
                let tags = serde_json::to_string(&f.tags).map_err(|e| format!("json: {e}"))?;
                sqlx::query("UPDATE notes SET tags = ? WHERE id = ?")
                    .bind(tags)
                    .bind(&note.id)
                    .execute(db)
                    .await
                    .map_err(|e| format!("db: {e}"))?;
            }
        }

        imported += 1;
        current += 1;
        on_progress(current, total, &title);
    }

    // 5. Import every other file as a vault-managed file (blob store) under
    //    the folder it came from. Re-import: same name + same parent compares
    //    content hashes — identical files are skipped, changed files update
    //    their blob in place (stable id). Failures skip the file without
    //    aborting.
    let mut files_imported = 0usize;
    let mut skipped = 0usize;
    for (abs, rel) in asset_files {
        let name = abs
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let parent_id = folder_of(&rel);
        // Ok(false) = unchanged (already imported with identical content).
        let outcome: Result<bool, String> = 'blk: {
            if let Some((fid, old_blob)) = existing_files.get(&(parent_id.clone(), name.clone())) {
                let bytes = match std::fs::read(&abs) {
                    Ok(b) => b,
                    Err(e) => break 'blk Err(format!("读取文件失败: {e}")),
                };
                let new_hash = crate::file_store::sha256_hex(&bytes);
                let old_hash = crate::file_store::parse_blob_path(old_blob)
                    .map(|(h, _)| h)
                    .unwrap_or_default();
                if new_hash == old_hash {
                    break 'blk Ok(false);
                }
                let ext = abs.extension().and_then(|e| e.to_str()).unwrap_or("bin");
                let blob = match crate::file_store::write_blob(app_data_dir, &bytes, ext) {
                    Ok(b) => b,
                    Err(e) => break 'blk Err(format!("copy to blob store: {e}")),
                };
                if let Err(e) = sqlx::query(
                    "UPDATE files SET blob_path = ?, size = ?, mime_type = ?, updated_at = ? WHERE id = ?",
                )
                .bind(&blob)
                .bind(bytes.len() as i64)
                .bind(crate::core::file_service::mime_guess(&name))
                .bind(time::now_iso())
                .bind(fid)
                .execute(db)
                .await
                {
                    break 'blk Err(format!("db: {e}"));
                }
                break 'blk Ok(true);
            }
            match crate::core::file_item_service::import_file(
                db,
                app_data_dir,
                vault_id,
                parent_id.as_deref(),
                &abs.to_string_lossy(),
            )
            .await
            {
                Ok(_) => Ok(true),
                Err(e) => Err(e),
            }
        };
        match outcome {
            Ok(true) => files_imported += 1,
            Ok(false) => unchanged += 1,
            Err(e) => {
                skipped += 1;
                tracing::warn!(file = %abs.display(), error = %e, "vault import: skipping file");
            }
        }
        current += 1;
        on_progress(current, total, &name);
    }

    Ok(serde_json::json!({
        "imported": imported,
        "files_imported": files_imported,
        "unchanged": unchanged,
        "skipped": skipped,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Re-importing the same directory must not duplicate folders, notes or
    /// files: unchanged items are skipped and changed files update in place.
    #[tokio::test]
    async fn import_vault_is_idempotent() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!("siku-import-test-{}", uuid::Uuid::new_v4()));
        let src = dir.join("src");
        let data = dir.join("data");
        std::fs::create_dir_all(src.join("sub"))?;
        std::fs::create_dir_all(&data)?;
        std::fs::write(src.join("a.md"), "---\ntags:\n  - t1\n---\n\nhello [[a]]")?;
        std::fs::write(src.join("sub").join("b.md"), "world")?;
        std::fs::write(src.join("sub").join("doc.pdf"), b"%PDF-fake")?;

        let db = crate::core::db::tests::connect_with_crsqlite(&dir.join("t.db")).await?;
        sqlx::query(crate::core::db::SCHEMA_INIT_SQL).execute(&db).await?;
        let vault = create_vault(&db, "test").await.map_err(|e| anyhow::anyhow!(e))?;

        let noop = |_: usize, _: usize, _: &str| {};
        let src_str = src.to_string_lossy().to_string();

        // First import: everything is new.
        let r1 = import_vault(&db, &data, &vault.id, &src_str, &noop)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        assert_eq!(r1["imported"], 2);
        assert_eq!(r1["files_imported"], 1);
        assert_eq!(r1["unchanged"], 0);

        // Second import of the unchanged directory: nothing new, nothing duplicated.
        let r2 = import_vault(&db, &data, &vault.id, &src_str, &noop)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        assert_eq!(r2["imported"], 0);
        assert_eq!(r2["files_imported"], 0);
        assert_eq!(r2["unchanged"], 3); // 2 notes + 1 file

        let note_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notes WHERE vault_id = ?")
            .bind(&vault.id)
            .fetch_one(&db)
            .await?;
        assert_eq!(note_count, 3); // 1 folder + 2 notes
        let file_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM files WHERE vault_id = ?")
            .bind(&vault.id)
            .fetch_one(&db)
            .await?;
        assert_eq!(file_count, 1);

        // Changed file content updates the blob in place (stable file id).
        std::fs::write(src.join("sub").join("doc.pdf"), b"%PDF-v2")?;
        let r3 = import_vault(&db, &data, &vault.id, &src_str, &noop)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        assert_eq!(r3["files_imported"], 1);
        assert_eq!(r3["unchanged"], 2); // 2 notes unchanged
        let file_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM files WHERE vault_id = ?")
            .bind(&vault.id)
            .fetch_one(&db)
            .await?;
        assert_eq!(file_count, 1);

        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}
