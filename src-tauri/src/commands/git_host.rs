use std::path::Path;

use tauri::State;
use tracing::instrument;

use crate::core::git_host::{self, GitPlatform, PrResult, RemoteInfo};
use crate::AppState;

/// Run a git command in `dir`, returning trimmed stdout; stderr goes into the
/// error so the dialog shows git's own message.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = crate::core::process::no_window(&mut std::process::Command::new("git"))
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| format!("git 执行失败（未安装？）：{e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!("git {} 失败：{}", args.join(" "), err));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Remote + branch info for a project's repo, for the create-PR dialog.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitRemoteInfo {
    pub platform: GitPlatform,
    pub owner: String,
    pub repo: String,
    pub branch: String,
    pub remote_url: String,
}

/// Inspect a project's git remote (origin) and current branch.
#[tauri::command]
#[instrument(skip(state))]
pub async fn git_remote_info(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<GitRemoteInfo, String> {
    let path = crate::core::project_service::get_path(&state.db, &project_id)
        .await?
        .ok_or_else(|| "项目不存在".to_string())?;
    let dir = Path::new(&path);
    if !dir.join(".git").exists() {
        return Err("项目目录不是 git 仓库（可在创建项目时勾选初始化 git）".to_string());
    }
    let url = git(dir, &["remote", "get-url", "origin"])
        .map_err(|_| "该仓库没有配置 origin 远程仓库".to_string())?;
    let remote: RemoteInfo = git_host::parse_remote_url(&url)
        .ok_or_else(|| format!("无法识别的远程地址（仅支持 GitHub/Gitee）：{url}"))?;
    let branch = git(dir, &["branch", "--show-current"])?;
    if branch.is_empty() {
        return Err("当前处于 detached HEAD，请先切换到一个分支".to_string());
    }
    Ok(GitRemoteInfo {
        platform: remote.platform,
        owner: remote.owner,
        repo: remote.repo,
        branch,
        remote_url: url,
    })
}

/// Push the current branch to origin (`git push -u origin <branch>`). Uses
/// the user's own git credentials (credential manager / ssh agent).
#[tauri::command]
#[instrument(skip(state))]
pub async fn git_push_branch(state: State<'_, AppState>, project_id: String) -> Result<String, String> {
    let path = crate::core::project_service::get_path(&state.db, &project_id)
        .await?
        .ok_or_else(|| "项目不存在".to_string())?;
    let dir = Path::new(&path);
    let branch = git(dir, &["branch", "--show-current"])?;
    if branch.is_empty() {
        return Err("当前处于 detached HEAD，请先切换到一个分支".to_string());
    }
    // push prints progress to stderr even on success; use output() directly.
    let out = crate::core::process::no_window(&mut std::process::Command::new("git"))
        .args(["push", "-u", "origin", &branch])
        .current_dir(dir)
        .output()
        .map_err(|e| format!("git push 执行失败：{e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!("git push 失败：{err}"));
    }
    Ok(branch)
}

/// Which platforms have a PAT configured (the token itself never leaves the
/// backend).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitTokenStatus {
    pub github: bool,
    pub gitee: bool,
}

#[tauri::command]
#[instrument(skip(state))]
pub async fn git_host_token_status(state: State<'_, AppState>) -> Result<GitTokenStatus, String> {
    Ok(GitTokenStatus {
        github: git_host::get_token(&state.db, GitPlatform::GitHub).await.is_some(),
        gitee: git_host::get_token(&state.db, GitPlatform::Gitee).await.is_some(),
    })
}

/// Store/replace a platform PAT (device-local, never synced).
#[tauri::command]
#[instrument(skip(state, token))]
pub async fn git_host_set_token(
    state: State<'_, AppState>,
    platform: GitPlatform,
    token: String,
) -> Result<(), String> {
    git_host::set_token(&state.db, platform, &token).await
}

/// Create a pull request on the platform detected from origin.
#[tauri::command]
#[instrument(skip(state))]
pub async fn pr_create(
    state: State<'_, AppState>,
    project_id: String,
    title: String,
    body: Option<String>,
    base: Option<String>,
) -> Result<PrResult, String> {
    let info = git_remote_info(state.clone(), project_id).await?;
    let remote = RemoteInfo {
        platform: info.platform,
        owner: info.owner,
        repo: info.repo,
    };
    git_host::create_pull_request(
        &state.db,
        &remote,
        title.trim(),
        body.as_deref().unwrap_or(""),
        &info.branch,
        base.as_deref().unwrap_or("main"),
    )
    .await
}
