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

/// Import Markdown notes from a folder (an Obsidian-style vault) into a vault.
/// Directories become folder notes; each `.md` file becomes a note with its
/// frontmatter (aliases/tags/created/updated) preserved. Non-markdown files
/// (attachments etc.) are skipped. Returns `{ imported, skipped }`.
#[instrument(skip(db))]
pub async fn import_vault(db: &SqlitePool, vault_id: &str, source_dir: &str) -> Result<serde_json::Value, String> {
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

    // 2. Create a folder note for each directory (rel path -> note id).
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
        let folder =
            crate::core::note_service::create_note(db, &name, "", None, parent_id.as_deref(), vault_id, true).await?;
        folder_ids.insert(rel.clone(), folder.id);
    }

    // 3. Walk all files, importing `.md` and counting skipped attachments.
    let mut files: Vec<(PathBuf, PathBuf)> = Vec::new(); // (abs, rel)
    let mut skipped = 0usize;
    fn collect_files(dir: &Path, rel: &Path, out: &mut Vec<(PathBuf, PathBuf)>, skipped: &mut usize) -> Result<(), String> {
        for entry in std::fs::read_dir(dir).map_err(|e| format!("读取目录失败: {e}"))? {
            let entry = entry.map_err(|e| format!("读取目录失败: {e}"))?;
            let name = entry.file_name().to_string_lossy().to_string();
            if SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            let path = entry.path();
            let rel_sub = rel.join(&name);
            if path.is_dir() {
                collect_files(&path, &rel_sub, out, skipped)?;
            } else if name.ends_with(".md") {
                out.push((path, rel_sub));
            } else {
                *skipped += 1;
            }
        }
        Ok(())
    }
    collect_files(root, Path::new(""), &mut files, &mut skipped)?;

    let mut imported = 0usize;
    for (abs, rel) in files {
        let content = std::fs::read_to_string(&abs).map_err(|e| format!("读取文件失败: {e}"))?;
        let (fm, body) = parse_frontmatter(&content);
        let stem = abs
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let title = if stem.is_empty() { "未命名".to_string() } else { stem };

        let parent_rel = rel.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let parent_id = if parent_rel.as_os_str().is_empty() {
            None
        } else {
            folder_ids.get(&parent_rel).cloned()
        };

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
    }

    Ok(serde_json::json!({ "imported": imported, "skipped": skipped }))
}
