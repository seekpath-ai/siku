use sqlx::SqlitePool;
use tracing::instrument;

use crate::core::models::{Project, ProjectInput};
use crate::core::time::now_iso;

const PROJECT_COLS: &str = "id, name, path, archived, created_at, updated_at";

#[instrument(skip(db))]
pub async fn list(db: &SqlitePool) -> Result<Vec<Project>, String> {
    sqlx::query_as::<_, Project>(&format!(
        "SELECT {PROJECT_COLS} FROM projects ORDER BY archived ASC, created_at"
    ))
    .fetch_all(db)
    .await
    .map_err(|e| format!("db error: {e}"))
}

/// Archive or restore a project (sidebar visibility only).
#[instrument(skip(db))]
pub async fn set_archived(db: &SqlitePool, id: &str, archived: bool) -> Result<(), String> {
    sqlx::query("UPDATE projects SET archived = ?, updated_at = ? WHERE id = ?")
        .bind(if archived { 1 } else { 0 })
        .bind(now_iso())
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| format!("db error: {e}"))?;
    Ok(())
}

#[instrument(skip(db))]
pub async fn get_by_id(db: &SqlitePool, id: &str) -> Result<Option<Project>, String> {
    sqlx::query_as::<_, Project>(&format!(
        "SELECT {PROJECT_COLS} FROM projects WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("db error: {e}"))
}

/// Path of a project, if it exists.
pub async fn get_path(db: &SqlitePool, id: &str) -> Result<Option<String>, String> {
    Ok(get_by_id(db, id).await?.map(|p| p.path))
}

fn folder_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| path.to_string())
}

/// Whether a usable system git is on PATH.
pub fn git_available() -> bool {
    std::process::Command::new("git")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Generic .gitignore written by git bootstrap (skipped when one exists).
const GITIGNORE_TEMPLATE: &str = "# Build outputs\ntarget/\nnode_modules/\ndist/\n\n# Logs & OS noise\n*.log\n.DS_Store\nThumbs.db\n\n# Local env\n.env\n.env.local\n";

/// git init + .gitignore template. No initial commit, no remote — that is
/// deliberate (see the plugins/PR roadmap); the repo starts clean.
fn git_bootstrap(dir: &std::path::Path) -> Result<(), String> {
    if !git_available() {
        return Err("未检测到 git，请先安装 git 后再初始化仓库".to_string());
    }
    let status = std::process::Command::new("git")
        .arg("init")
        .current_dir(dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .status()
        .map_err(|e| format!("git init 执行失败：{e}"))?;
    if !status.success() {
        return Err(format!("git init 退出码非零：{status}"));
    }
    let ignore = dir.join(".gitignore");
    if !ignore.exists() {
        std::fs::write(&ignore, GITIGNORE_TEMPLATE).map_err(|e| format!("写入 .gitignore 失败：{e}"))?;
    }
    Ok(())
}

#[instrument(skip(db))]
pub async fn create(db: &SqlitePool, input: ProjectInput) -> Result<Project, String> {
    let path = input.path.unwrap_or_default();
    let path = path.trim().to_string();
    if path.is_empty() {
        return Err("project path required".to_string());
    }
    // Create the directory when it does not exist ("新建项目目录" flow) —
    // there is no default/app-data project anymore, so every project dir is
    // an explicit user choice.
    if !std::path::Path::new(&path).is_dir() {
        std::fs::create_dir_all(&path).map_err(|e| format!("无法创建目录 {path}：{e}"))?;
    }

    // Optional git bootstrap: git init + a generic .gitignore. Best-effort in
    // the sense that a missing git binary produces a clear error instead of a
    // half-created project — the directory and project row are kept either way.
    if input.git_init.unwrap_or(false) {
        git_bootstrap(std::path::Path::new(&path))?;
    }

    let id = uuid::Uuid::new_v4().to_string();
    let now = now_iso();
    let name = input
        .name
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| folder_name(&path));

    sqlx::query(
        "INSERT INTO projects (id, name, path, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&name)
    .bind(&path)
    .bind(&now)
    .bind(&now)
    .execute(db)
    .await
    .map_err(|e| format!("db error: {e}"))?;

    get_by_id(db, &id)
        .await?
        .ok_or_else(|| "project not found".to_string())
}

#[instrument(skip(db))]
pub async fn update(db: &SqlitePool, id: &str, input: ProjectInput) -> Result<Project, String> {
    let now = now_iso();
    if let Some(name) = input.name.filter(|n| !n.trim().is_empty()) {
        sqlx::query("UPDATE projects SET name = ?, updated_at = ? WHERE id = ?")
            .bind(&name)
            .bind(&now)
            .bind(id)
            .execute(db)
            .await
            .map_err(|e| format!("db error: {e}"))?;
    } else {
        sqlx::query("UPDATE projects SET updated_at = ? WHERE id = ?")
            .bind(&now)
            .bind(id)
            .execute(db)
            .await
            .map_err(|e| format!("db error: {e}"))?;
    }

    get_by_id(db, id)
        .await?
        .ok_or_else(|| format!("project not found: {id}"))
}

#[instrument(skip(db))]
pub async fn delete(db: &SqlitePool, id: &str) -> Result<(), String> {
    // Sessions lose their project reference instead of being deleted.
    sqlx::query("UPDATE chat_sessions SET project_id = NULL WHERE project_id = ?")
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| format!("db error: {e}"))?;
    sqlx::query("DELETE FROM projects WHERE id = ?")
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| format!("db error: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::{tests::connect_with_crsqlite, SCHEMA_INIT_SQL};

    async fn fresh_db(dir: &std::path::Path) -> SqlitePool {
        let db = connect_with_crsqlite(&dir.join("t.db")).await.unwrap();
        sqlx::query(SCHEMA_INIT_SQL).execute(&db).await.unwrap();
        db
    }

    /// Creating a project for a path that does not exist creates the
    /// directory ("新建项目目录" flow).
    #[tokio::test]
    async fn creates_missing_directory() {
        let dir = tempfile::tempdir().unwrap();
        let db = fresh_db(dir.path()).await;
        let target = dir.path().join("new-project").join("nested");
        let p = create(
            &db,
            ProjectInput { name: None, path: Some(target.to_string_lossy().to_string()), git_init: None },
        )
        .await
        .unwrap();
        assert!(target.is_dir());
        assert_eq!(p.name, "nested");
        db.close().await;
    }

    /// git_init bootstraps a repository and writes the .gitignore template
    /// (skipped silently on machines without git — the dialog greys the
    /// option out there anyway).
    #[tokio::test]
    async fn git_init_creates_repo_and_gitignore() {
        if !git_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let db = fresh_db(dir.path()).await;
        let target = dir.path().join("repo");
        create(
            &db,
            ProjectInput { name: None, path: Some(target.to_string_lossy().to_string()), git_init: Some(true) },
        )
        .await
        .unwrap();
        assert!(target.join(".git").is_dir());
        assert!(target.join(".gitignore").is_file());
        db.close().await;
    }

    /// Regression: the frontend sends `gitInit` (camelCase); without
    /// serde rename_all the flag used to deserialize as None and git
    /// bootstrap silently never ran.
    #[test]
    fn project_input_accepts_camel_case_git_init() {
        let input: ProjectInput =
            serde_json::from_str(r#"{"path": "/tmp/x", "gitInit": true}"#).unwrap();
        assert_eq!(input.git_init, Some(true));
        let input: ProjectInput = serde_json::from_str(r#"{"path": "/tmp/x"}"#).unwrap();
        assert_eq!(input.git_init, None);
    }

    /// Archiving hides a project from the sidebar but keeps it listed
    /// (the frontend filters); restoring brings it back.
    #[tokio::test]
    async fn archive_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let db = fresh_db(dir.path()).await;
        let p = create(
            &db,
            ProjectInput { name: None, path: Some(dir.path().join("p").to_string_lossy().to_string()), git_init: None },
        )
        .await
        .unwrap();
        assert!(!p.archived);
        set_archived(&db, &p.id, true).await.unwrap();
        let p = get_by_id(&db, &p.id).await.unwrap().unwrap();
        assert!(p.archived);
        set_archived(&db, &p.id, false).await.unwrap();
        let p = get_by_id(&db, &p.id).await.unwrap().unwrap();
        assert!(!p.archived);
        db.close().await;
    }
}
