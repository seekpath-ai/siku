use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::core::artifact_service::{artifact_uri, resolve_uri};
use crate::file_store;
use crate::AppState;

const MAX_SYNC_BYTES: u64 = 512 * 1024 * 1024;

/// One row in the per-session artifact sidebar. Local rows point at the file
/// in the workspace; once synced they also carry a `siku://artifact` URI.
#[derive(Debug, Clone, Serialize)]
pub struct SessionArtifactItem {
    pub id: String,
    pub kind: String,
    pub synced: bool,
    pub message_id: String,
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

async fn local_items(
    db: &sqlx::SqlitePool,
    app_data_dir: &Path,
    session_id: &str,
    message_id: Option<&str>,
) -> Result<Vec<SessionArtifactItem>, String> {
        let message_filter = if message_id.is_some() {
            " AND sf.message_id = ?"
        } else {
            ""
        };
        let sql = format!(
            "SELECT sf.id, sf.message_id, sf.name, sf.path, sf.ext, sf.size, \
                    COALESCE(sf.modified_at_ms, 0), sf.created_at, \
                    sa.id, sa.hash, sa.ext \
             FROM session_process_files sf \
             LEFT JOIN session_artifacts sa ON sa.id = sf.artifact_id \
             WHERE sf.session_id = ?{message_filter}"
        );
        let mut query = sqlx::query_as::<
            _,
            (
                String,
                String,
                String,
                String,
                String,
                i64,
                i64,
                String,
                Option<String>,
                Option<String>,
                Option<String>,
            ),
        >(&sql)
        .bind(session_id);
        if let Some(mid) = message_id {
            query = query.bind(mid);
        }
        let rows = query
            .fetch_all(db)
            .await
            .map_err(|e| format!("db error: {e}"))?;

        let mut items = Vec::with_capacity(rows.len());
        for (
            id,
            message_id,
            name,
            path,
            ext,
            size,
            modified_at_ms,
            created_at,
            artifact_id,
            hash,
            artifact_ext,
        ) in rows
        {
            let synced = artifact_id.is_some() && hash.is_some();
            let synced_ext = artifact_ext.clone().unwrap_or_else(|| ext.clone());
            items.push(SessionArtifactItem {
                id,
                kind: "local".to_string(),
                synced,
                message_id,
                name,
                ext,
                size,
                path: Some(path),
                uri: hash
                    .as_deref()
                    .map(|hash| artifact_uri(hash, &synced_ext)),
                modified_at_ms: (modified_at_ms > 0).then_some(modified_at_ms),
                created_at,
            });
        }

        // Synced artifacts from a peer have no local row on this device.
        let message_filter = if message_id.is_some() {
            " AND sa.message_id = ?"
        } else {
            ""
        };
        let sql = format!(
            "SELECT sa.id, sa.message_id, sa.name, sa.hash, sa.ext, sa.size, sa.created_at \
             FROM session_artifacts sa \
             WHERE sa.session_id = ?{message_filter} \
               AND NOT EXISTS ( \
                 SELECT 1 FROM session_process_files sf \
                 WHERE sf.session_id = sa.session_id AND sf.artifact_id = sa.id \
               )"
        );
        let mut query = sqlx::query_as::<_, (String, String, String, String, String, i64, String)>(&sql)
            .bind(session_id);
        if let Some(mid) = message_id {
            query = query.bind(mid);
        }
        let synced_only = query
            .fetch_all(db)
            .await
            .map_err(|e| format!("db error: {e}"))?;
        for (id, message_id, name, hash, ext, size, created_at) in synced_only {
            let blob = file_store::blob_path(app_data_dir, &hash, &ext);
            items.push(SessionArtifactItem {
                id,
                kind: "synced".to_string(),
                synced: true,
                message_id,
                name,
                ext: ext.clone(),
                size,
                path: blob.exists().then(|| blob.to_string_lossy().into_owned()),
                uri: Some(artifact_uri(&hash, &ext)),
                modified_at_ms: modified_ms(&blob),
                created_at,
            });
        }

    Ok(items)
}

/// List local files and synced artifacts for a session.
#[tauri::command]
pub async fn session_artifacts_list(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<SessionArtifactItem>, String> {
    local_items(&state.db, &state.app_data_dir, &session_id, None).await
}

/// All files belonging to one assistant turn, including files synced from a
/// peer. Used by the in-message artifact section.
#[tauri::command]
pub async fn session_turn_files(
    state: State<'_, AppState>,
    session_id: String,
    message_id: String,
) -> Result<Vec<SessionArtifactItem>, String> {
    local_items(
        &state.db,
        &state.app_data_dir,
        &session_id,
        Some(&message_id),
    )
    .await
}

#[derive(Debug, Default, Serialize)]
pub struct ArtifactSyncResult {
    pub synced: u64,
    pub bytes: u64,
    pub errors: Vec<String>,
}

/// Copy selected local files into the blob store and add them to the synced
/// artifact index. This is the only path that creates cross-device artifacts.
#[tauri::command]
pub async fn session_artifacts_sync(
    state: State<'_, AppState>,
    session_id: String,
    ids: Vec<String>,
) -> Result<ArtifactSyncResult, String> {
    let mut result = ArtifactSyncResult::default();
    for id in ids {
        let row: Option<(String, String, String, i64, String)> = sqlx::query_as(
            "SELECT name, path, ext, size, message_id FROM session_process_files \
             WHERE id = ? AND session_id = ?",
        )
        .bind(&id)
        .bind(&session_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| format!("db error: {e}"))?;
        let Some((name, path, ext, size, message_id)) = row else {
            result.errors.push(format!("产物不存在：{id}"));
            continue;
        };
        if size as u64 > MAX_SYNC_BYTES {
            result.errors.push(format!(
                "{name} 超过同步上限（{} MB）",
                MAX_SYNC_BYTES / 1024 / 1024
            ));
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            result.errors.push(format!("无法读取文件：{path}"));
            continue;
        };
        let rel = match file_store::write_blob(&state.app_data_dir, &bytes, &ext) {
            Ok(rel) => rel,
            Err(e) => {
                result.errors.push(format!("写入 blob 失败：{e}"));
                continue;
            }
        };
        let Some((hash, ext)) = file_store::parse_blob_path(&rel) else {
            result.errors.push("无效的 blob 路径".to_string());
            continue;
        };

        let existing: Option<(String,)> = sqlx::query_as(
            "SELECT id FROM session_artifacts WHERE session_id = ? AND hash = ? LIMIT 1",
        )
        .bind(&session_id)
        .bind(&hash)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| format!("db error: {e}"))?;
        let artifact_id = if let Some((id,)) = existing {
            id
        } else {
            let artifact_id = uuid::Uuid::new_v4().to_string();
            sqlx::query(
                "INSERT INTO session_artifacts \
                 (id, session_id, message_id, hash, ext, name, size, created_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&artifact_id)
            .bind(&session_id)
            .bind(&message_id)
            .bind(&hash)
            .bind(&ext)
            .bind(&name)
            .bind(size)
            .bind(crate::core::time::now_iso())
            .execute(&state.db)
            .await
            .map_err(|e| format!("db error: {e}"))?;
            artifact_id
        };

        sqlx::query("UPDATE session_process_files SET artifact_id = ? WHERE id = ?")
            .bind(&artifact_id)
            .bind(&id)
            .execute(&state.db)
            .await
            .map_err(|e| format!("db error: {e}"))?;
        crate::sync::attachments::mark_blob_pending_push(
            &state.db,
            &state.app_data_dir,
            &hash,
            &ext,
        )
        .await;
        result.synced += 1;
        result.bytes += size as u64;
    }
    Ok(result)
}

/// Copy a deliverable blob to a user-chosen path from the native save dialog.
#[tauri::command]
pub async fn artifact_save_as(
    state: State<'_, AppState>,
    uri: String,
    save_path: String,
) -> Result<(), String> {
    save_copy(PathBuf::from(resolve_uri(&state.app_data_dir, &uri)?), PathBuf::from(save_path)).await
}

/// Copy a local (not yet synced) artifact file to a user-chosen path.
#[tauri::command]
pub async fn local_artifact_save_as(
    state: State<'_, AppState>,
    session_id: String,
    id: String,
    save_path: String,
) -> Result<(), String> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT path FROM session_process_files WHERE id = ? AND session_id = ?",
    )
    .bind(&id)
    .bind(&session_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| format!("db error: {e}"))?;
    let Some((path,)) = row else {
        return Err("产物不存在".to_string());
    };
    save_copy(PathBuf::from(path), PathBuf::from(save_path)).await
}

async fn save_copy(source: PathBuf, dest: PathBuf) -> Result<(), String> {
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
/// session-file trash, so deletion is recoverable.
fn trash_process_file(app_data_dir: &Path, source: &Path) -> Result<(), String> {
    let meta = match std::fs::symlink_metadata(source) {
        Ok(m) => m,
        Err(_) => return Ok(()),
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
        std::fs::copy(source, &dest)
            .map_err(|e| format!("移动过程文件失败：{rename_err}；复制也失败：{e}"))?;
        std::fs::remove_file(source).map_err(|e| format!("过程文件已复制但删除原文件失败：{e}"))?;
    }
    Ok(())
}

async fn remove_artifact_footer(db: &sqlx::SqlitePool, message_id: &str, uri: &str) -> Result<(), String> {
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
            let target = line.trim_start().starts_with("- [") && line.contains(uri);
            if target { changed = true; !target } else { true }
        })
        .collect();
    if !lines.iter().any(|line| line.contains("siku://artifact/")) {
        let before = lines.len();
        lines.retain(|line| !line.contains("📎 本轮产物"));
        changed |= lines.len() != before;
    }
    if !changed {
        return Ok(());
    }
    let new_content = lines.join("\n").trim_end().to_string();
    sqlx::query("UPDATE chat_messages SET content = ? WHERE id = ?")
        .bind(new_content)
        .bind(message_id)
        .execute(db)
        .await
        .map_err(|e| format!("db error: {e}"))?;
    Ok(())
}

/// Delete selected sidebar rows. Local files go to the app trash; synced rows
/// drop their synced index (the blob becomes a GC candidate).
#[tauri::command]
pub async fn session_artifacts_delete(
    state: State<'_, AppState>,
    session_id: String,
    items: Vec<ArtifactDeleteItem>,
) -> Result<ArtifactDeleteResult, String> {
    let mut result = ArtifactDeleteResult::default();
    for item in items {
        if item.kind == "local" {
            let row: Option<(String, String, String)> = sqlx::query_as(
                "SELECT name, path, artifact_id FROM session_process_files WHERE id = ? AND session_id = ?",
            )
            .bind(&item.id)
            .bind(&session_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| format!("db error: {e}"))?;
            let Some((name, path, artifact_id)) = row else {
                result.errors.push(format!("产物不存在：{}", item.id));
                continue;
            };
            if !artifact_id.is_empty() {
                if let Some((hash, ext, message_id)) = sqlx::query_as::<_, (String, String, String)>(
                    "SELECT hash, ext, message_id FROM session_artifacts WHERE id = ?",
                )
                .bind(&artifact_id)
                .fetch_optional(&state.db)
                .await
                .map_err(|e| format!("db error: {e}"))?
                {
                    sqlx::query("DELETE FROM session_artifacts WHERE id = ?")
                        .bind(&artifact_id)
                        .execute(&state.db)
                        .await
                        .map_err(|e| format!("db error: {e}"))?;
                    let _ = remove_artifact_footer(&state.db, &message_id, &artifact_uri(&hash, &ext)).await;
                }
            }
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
        } else if item.kind == "synced" {
            let row: Option<(String, String, String)> = sqlx::query_as(
                "SELECT hash, ext, message_id FROM session_artifacts WHERE id = ? AND session_id = ?",
            )
            .bind(&item.id)
            .bind(&session_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| format!("db error: {e}"))?;
            let Some((hash, ext, message_id)) = row else {
                result.errors.push(format!("产物不存在：{}", item.id));
                continue;
            };
            sqlx::query("DELETE FROM session_artifacts WHERE id = ? AND session_id = ?")
                .bind(&item.id)
                .bind(&session_id)
                .execute(&state.db)
                .await
                .map_err(|e| format!("db error: {e}"))?;
            let _ = remove_artifact_footer(&state.db, &message_id, &artifact_uri(&hash, &ext)).await;
            result.deleted += 1;
        } else {
            result.errors.push(format!("未知产物类型：{}", item.kind));
        }
    }
    Ok(result)
}
