//! Git hosting integration (GitHub / Gitee) for pull requests.
//!
//! Scope decisions (confirmed with the user):
//! - push is delegated to the system git binary — the user's own credential
//!   setup (credential manager / ssh agent) handles git auth;
//! - PATs are used ONLY for the platform APIs and live in device-local
//!   settings (never synced, never leave the machine);
//! - platform is auto-detected from the `origin` URL.

use serde::Serialize;
use sqlx::SqlitePool;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GitPlatform {
    GitHub,
    Gitee,
}

impl GitPlatform {
    pub fn label(&self) -> &'static str {
        match self {
            GitPlatform::GitHub => "GitHub",
            GitPlatform::Gitee => "Gitee",
        }
    }
    fn token_key(&self) -> &'static str {
        match self {
            GitPlatform::GitHub => "github.pat",
            GitPlatform::Gitee => "gitee.pat",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteInfo {
    pub platform: GitPlatform,
    pub owner: String,
    pub repo: String,
}

/// Parse an origin URL into (platform, owner, repo). Handles
/// `https://host/owner/repo(.git)` and `git@host:owner/repo(.git)`.
pub fn parse_remote_url(url: &str) -> Option<RemoteInfo> {
    let url = url.trim().trim_end_matches(".git");
    let (host, path) = if let Some(rest) = url.strip_prefix("git@") {
        let (host, path) = rest.split_once(':')?;
        (host, path)
    } else if let Some(rest) = url.strip_prefix("https://") {
        let (host, path) = rest.split_once('/')?;
        (host, path)
    } else if let Some(rest) = url.strip_prefix("http://") {
        let (host, path) = rest.split_once('/')?;
        (host, path)
    } else {
        return None;
    };
    let platform = match host {
        h if h.eq_ignore_ascii_case("github.com") => GitPlatform::GitHub,
        h if h.eq_ignore_ascii_case("gitee.com") => GitPlatform::Gitee,
        _ => return None,
    };
    let mut parts = path.split('/');
    let owner = parts.next()?.to_string();
    let repo = parts.next()?.to_string();
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some(RemoteInfo { platform, owner, repo })
}

/// PAT for a platform, if configured (device-local setting).
pub async fn get_token(db: &SqlitePool, platform: GitPlatform) -> Option<String> {
    crate::core::settings_service::get_device_setting(db, platform.token_key())
        .await
        .ok()
        .flatten()
        .filter(|t| !t.trim().is_empty())
}

/// Store/replace a platform PAT. Device-local, never synced.
pub async fn set_token(db: &SqlitePool, platform: GitPlatform, token: &str) -> Result<(), String> {
    crate::core::settings_service::set_device_setting(db, platform.token_key(), token.trim()).await
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrResult {
    pub html_url: String,
    pub number: i64,
}

/// reqwest client honoring the global network proxy (network.proxy →
/// llm.proxy fallback), same convention as link_import.
async fn http_client(db: &SqlitePool) -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder()
        .user_agent("siku-app")
        .timeout(std::time::Duration::from_secs(30));
    for key in ["network.proxy", "llm.proxy"] {
        if let Ok(Some(p)) = crate::core::settings_service::get_setting(db, key).await {
            if !p.trim().is_empty() {
                let proxy = reqwest::Proxy::all(p.trim()).map_err(|e| format!("代理配置无效: {e}"))?;
                builder = builder.proxy(proxy);
                break;
            }
        }
    }
    builder.build().map_err(|e| format!("http client: {e}"))
}

/// Create a pull request on the detected platform.
/// GitHub: POST /repos/{o}/{r}/pulls with Bearer auth.
/// Gitee:  POST /api/v5/repos/{o}/{r}/pulls with access_token in the body.
pub async fn create_pull_request(
    db: &SqlitePool,
    remote: &RemoteInfo,
    title: &str,
    body: &str,
    head: &str,
    base: &str,
) -> Result<PrResult, String> {
    let token = get_token(db, remote.platform)
        .await
        .ok_or_else(|| format!("未配置 {} 的访问令牌（PAT）", remote.platform.label()))?;
    let client = http_client(db).await?;

    let (url, resp) = match remote.platform {
        GitPlatform::GitHub => {
            let url = format!(
                "https://api.github.com/repos/{}/{}/pulls",
                remote.owner, remote.repo
            );
            let resp = client
                .post(&url)
                .bearer_auth(&token)
                .header("Accept", "application/vnd.github+json")
                .json(&serde_json::json!({
                    "title": title, "body": body, "head": head, "base": base,
                }))
                .send()
                .await
                .map_err(|e| format!("GitHub API 请求失败：{e}"))?;
            (url, resp)
        }
        GitPlatform::Gitee => {
            let url = format!(
                "https://gitee.com/api/v5/repos/{}/{}/pulls",
                remote.owner, remote.repo
            );
            let resp = client
                .post(&url)
                .json(&serde_json::json!({
                    "access_token": token, "title": title, "body": body,
                    "head": head, "base": base,
                }))
                .send()
                .await
                .map_err(|e| format!("Gitee API 请求失败：{e}"))?;
            (url, resp)
        }
    };

    let status = resp.status();
    let text = resp.text().await.map_err(|e| format!("读取响应失败：{e}"))?;
    if !status.is_success() {
        // Surface the platform's own message (e.g. "No commits between",
        // token scope errors) instead of a bare status code.
        let msg = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| {
                v.get("message")
                    .and_then(|m| m.as_str().map(String::from))
                    .or_else(|| v.get("errors").map(|e| e.to_string()))
            })
            .unwrap_or_else(|| text.chars().take(300).collect());
        return Err(format!("{} API 错误（{}）：{}", remote.platform.label(), status, msg));
    }

    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("解析响应失败：{e}"))?;
    let html_url = v
        .get("html_url")
        .and_then(|u| u.as_str())
        .map(String::from)
        .unwrap_or_default();
    let number = v.get("number").and_then(|n| n.as_i64()).unwrap_or(0);
    Ok(PrResult { html_url, number })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_github_and_gitee_urls() {
        let gh = parse_remote_url("https://github.com/owner/repo.git").unwrap();
        assert_eq!(gh.platform, GitPlatform::GitHub);
        assert_eq!((gh.owner.as_str(), gh.repo.as_str()), ("owner", "repo"));

        let gh2 = parse_remote_url("git@github.com:owner/repo.git").unwrap();
        assert_eq!(gh2.platform, GitPlatform::GitHub);

        let ge = parse_remote_url("https://gitee.com/someone/my-repo").unwrap();
        assert_eq!(ge.platform, GitPlatform::Gitee);
        assert_eq!(ge.repo, "my-repo");

        assert!(parse_remote_url("https://gitlab.com/a/b").is_none());
        assert!(parse_remote_url("not-a-url").is_none());
        assert!(parse_remote_url("https://github.com/onlyowner").is_none());
    }
}
