use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::core::artifact_service::{artifact_uri, resolve_uri};
use crate::file_store;
use crate::AppState;

/// One row in the per-session artifact sidebar. Deliverables point at a blob
/// (`uri`); process files are local-only (`path`) and never sync.
#[derive(Debug, Clone, Serialize)]
pub struct SessionArtifactItem {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub ext: String,
    pub size: i64,
    pub path: Option<String>,
    pub uri: Option<String>,
    pub modified_at_ms: Option<i64>,
    pub created_at: String,
}

/// Resolve a device-independent `siku://artifact/<hash>.<ext>` reference to
/// this device's absolute blob path. Errors while the blob is still being
/// fetched from the peer ("产物尚未同步到本机").
#[tauri::command]
pub async fn artifact_resolve(state: State<'_, AppState>, uri: String) -> Result<String, String> {
    resolve_uri(&state.app_data_dir, &uri)
}

fn modified_ms(path: &Path) -> Option<i64> {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
}

/// List both synced deliverables and local process files for a session.
#[tauri::command]
pub async fn session_artifacts_list(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<SessionArtifactItem>, String> {
    let deliverables: Vec<(String, String, String, String, i64, String)> = sqlx::query_as(
        "SELECT id, name, hash, ext, size, created_at FROM session_artifacts \
         WHERE session_id = ?",
    )
    .bind(&session_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| format!("db error: {e}"))?;

    let mut items = Vec::with_capacity(deliverables.len());
    for (id, name, hash, ext, size, created_at) in deliverables {
        let path = file_store::blob_path(&state.app_data_dir, &hash, &ext);
        items.push(SessionArtifactItem {
            id,
            kind: "deliverable".to_string(),
            name,
            ext: ext.clone(),
            size,
            path: path.exists().then(|| path.to_string_lossy().into_owned()),
            uri: Some(artifact_uri(&hash, &ext)),
            modified_at_ms: modified_ms(&path),
            created_at,
        });
    }

    let process: Vec<(String, String, String, String, i64, i64, String)> = sqlx::query_as(
        "SELECT id, name, path, ext, size, COALESCE(modified_at_ms, 0), created_at \
         FROM session_process_files WHERE session_id = ?",
    )
    .bind(&session_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| format!("db error: {e}"))?;

    for (id, name, path, ext, size, modified_at_ms, created_at) in process {
        items.push(SessionArtifactItem {
            id,
            kind: "process".to_string(),
            name,
            ext,
            size,
            path: Some(path),
            uri: None,
            modified_at_ms: (modified_at_ms > 0).then_some(modified_at_ms),
            created_at,
        });
    }

    Ok(items)
}

/// Copy a deliverable blob to a user-chosen path from the native save dialog.
#[tauri::command]
pub async fn artifact_save_as(
    state: State<'_, AppState>,
    uri: String,
    save_path: String,
) -> Result<(), String> {
    let source = resolve_uri(&state.app_data_dir, &uri)?;
    let source = PathBuf::from(source);
    let dest = PathBuf::from(save_path);
    tokio::task::spawn_blocking(move || {
        if source == dest {
            return Ok(());
        }
        std::fs::copy(&source, &dest)
            .map(|_| ())
            .map_err(|e| format!("保存产物失败：{e}"))
    })
    .await
    .map_err(|e| format!("保存产物任务失败：{e}"))?
}

#[derive(Debug, Deserialize)]
pub struct ArtifactDeleteItem {
    pub id: String,
    pub kind: String,
}

#[derive(Debug, Default, Serialize)]
pub struct ArtifactDeleteResult {
    pub deleted: u64,
    pub errors: Vec<String>,
}

/// Move a process file out of the working directory into the app's local
/// session-file trash, so deletion is recoverable. The original path is never
/// followed through a symlink: only a regular file is moved.
fn trash_process_file(app_data_dir: &Path, source: &Path) -> Result<(), String> {
    let meta = match std::fs::symlink_metadata(source) {
        Ok(m) => m,
        Err(_) => return Ok(()), // already gone; just drop the index row
    };
    if !meta.file_type().is_file() {
        return Err(format!("不是普通文件，拒绝移动：{}", source.display()));
    }
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ").to_string();
    let trash_dir = app_data_dir.join("trash").join("session-files").join(stamp);
    std::fs::create_dir_all(&trash_dir).map_err(|e| format!("创建回收站失败：{e}"))?;
    let name = source
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_else(|| "artifact".into());
    let mut dest = trash_dir.join(name);
    if dest.exists() {
        dest = trash_dir.join(format!("{}-{}", uuid::Uuid::new_v4(), dest.file_name().unwrap().to_string_lossy()));
    }
    if let Err(rename_err) = std::fs::rename(source, &dest) {
        // The working directory may be on another volume (common on Windows:
        // repo on D:, app data on C:). Fall back to copy + remove.
        std::fs::copy(source, &dest)
            .map_err(|e| format!("移动过程文件失败：{rename_err}；复制也失败：{e}"))?;
        std::fs::remove_file(source).map_err(|e| format!("过程文件已复制但删除原文件失败：{e}"))?;
    }
    Ok(())
}

/// Remove the deleted deliverable's footer bullet from its assistant message,
/// so the chat bubble does not keep showing a link whose index row is gone.
async fn remove_artifact_footer(
    db: &sqlx::SqlitePool,
    message_id: &str,
    uri: &str,
) -> Result<(), String> {
    if message_id.is_empty() {
        return Ok(());
    }
    let row: Option<(String,)> = sqlx::query_as("SELECT content FROM chat_messages WHERE id = ?")
        .bind(message_id)
        .fetch_optional(db)
        .await
        .map_err(|e| format!("db error: {e}"))?;
    let Some((content,)) = row else {
        return Ok(());
    };
    let mut changed = false;
    let mut lines: Vec<&str> = content
        .lines()
        .filter(|line| {
            let is_target_bullet = line.trim_start().starts_with("- [") && line.contains(uri);
            if is_target_bullet {
                changed = true;
                false
            } else {
                true
            }
        })
        .collect();
    let has_other_artifact = lines.iter().any(|line| line.contains("siku://artifact/"));
    if !has_other_artifact {
        let before = lines.len();
        lines.retain(|line| !line.contains("📎 本轮产物"));
        changed |= lines.len() != before;
    }
    if !changed {
        return Ok(());
    }
    let mut new_content = lines.join("\n");
    new_content = new_content.trim_end().to_string();
    sqlx::query("UPDATE chat_messages SET content = ? WHERE id = ?")
        .bind(new_content)
        .bind(message_id)
        .execute(db)
        .await
        .map_err(|e| format!("db error: {e}"))?;
    Ok(())
}

/// Delete selected sidebar rows. Deliverables drop their synced index row
/// (the blob becomes a GC candidate); process files are moved to the app's
/// local session-file trash.
#[tauri::command]
pub async fn session_artifacts_delete(
    state: State<'_, AppState>,
    session_id: String,
    items: Vec<ArtifactDeleteItem>,
) -> Result<ArtifactDeleteResult, String> {
    let mut result = ArtifactDeleteResult::default();
    for item in items {
        if item.kind == "deliverable" {
            let row: Option<(String, String, String, String)> = sqlx::query_as(
                "SELECT message_id, hash, ext, name FROM session_artifacts \
                 WHERE id = ? AND session_id = ?",
            )
            .bind(&item.id)
            .bind(&session_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| format!("db error: {e}"))?;
            let Some((message_id, hash, ext, _name)) = row else {
                result.errors.push(format!("产物不存在：{}", item.id));
                continue;
            };
            sqlx::query("DELETE FROM session_artifacts WHERE id = ? AND session_id = ?")
                .bind(&item.id)
                .bind(&session_id)
                .execute(&state.db)
                .await
                .map_err(|e| format!("db error: {e}"))?;
            let uri = artifact_uri(&hash, &ext);
            if let Err(e) = remove_artifact_footer(&state.db, &message_id, &uri).await {
                result.errors.push(format!("更新消息产物链接失败：{e}"));
            }
            result.deleted += 1;
        } else if item.kind == "process" {
            let row: Option<(String, String)> = sqlx::query_as(
                "SELECT name, path FROM session_process_files WHERE id = ? AND session_id = ?",
            )
            .bind(&item.id)
            .bind(&session_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| format!("db error: {e}"))?;
            let Some((name, path)) = row else {
                result.errors.push(format!("过程文件不存在：{}", item.id));
                continue;
            };
            if let Err(e) = trash_process_file(&state.app_data_dir, Path::new(&path)) {
                result.errors.push(format!("{name}：{e}"));
                continue;
            }
            sqlx::query("DELETE FROM session_process_files WHERE id = ? AND session_id = ?")
                .bind(&item.id)
                .bind(&session_id)
                .execute(&state.db)
                .await
                .map_err(|e| format!("db error: {e}"))?;
            result.deleted += 1;
        } else {
            result.errors.push(format!("未知产物类型：{}", item.kind));
        }
    }
    Ok(result)
}
