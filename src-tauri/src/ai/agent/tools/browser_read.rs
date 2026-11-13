use async_trait::async_trait;
use crate::ai::agent::tool_registry::{Tool, ToolParameter};

/// Read the visible text of the active tab. This is the browser fallback for
/// `web_fetch`: it runs inside a real Chrome/Edge page, so JavaScript-rendered
/// content, logins and consent walls work. It does not solve CAPTCHA itself —
/// the model can use browser_captcha_wait or ask the user.
const READ_JS: &str = r#"(() => {
  const main = document.querySelector('article, main');
  const pick = main && ((main.innerText || '').length > 200) ? main : document.body;
  const text = pick ? (pick.innerText || pick.textContent || '') : '';
  return {
    title: document.title || '',
    url: location.href || '',
    text: text.replace(/\n{3,}/g, '\n\n').trim(),
  };
})()"#;

pub struct BrowserReadTool;

#[async_trait]
impl Tool for BrowserReadTool {
    fn name(&self) -> &str {
        "browser_read"
    }

    fn description(&self) -> &str {
        "Read the rendered text of the active browser tab. Use this after browser_connect \
         (and browser_navigate) for JavaScript-rendered pages, login-protected pages, or sites \
         that block plain HTTP fetches; web_fetch cannot run JavaScript. Optionally pass a URL to \
         navigate first. Returns page title/URL plus the main text (default 20000 chars). \
         Read-only; CAPTCHA still needs browser_captcha_wait or the user."
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![
            ToolParameter {
                name: "url".into(),
                param_type: "string".into(),
                description: "Optional URL to navigate to before reading; https:// is assumed when the scheme is missing".into(),
                required: false,
            },
            ToolParameter {
                name: "max_chars".into(),
                param_type: "integer".into(),
                description: "Maximum characters to return (default 20000, capped at 50000)".into(),
                required: false,
            },
        ]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let page = super::browser::require_page().await?;

        if let Some(raw) = args["url"]
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let url = if raw.contains("://") || raw.starts_with("about:") {
                raw.to_string()
            } else {
                format!("https://{raw}")
            };
            tokio::time::timeout(std::time::Duration::from_secs(30), page.goto(&url))
                .await
                .map_err(|_| format!("browser navigation timed out after 30s: {url}"))?
                .map_err(|e| format!("browser navigation failed: {e}"))?;
        }

        let value = page
            .evaluate(READ_JS)
            .await
            .map_err(|e| format!("browser read failed: {e}"))?
            .into_value::<serde_json::Value>()
            .map_err(|e| format!("browser read result unreadable: {e}"))?;

        let title = value["title"].as_str().unwrap_or("").trim();
        let url = value["url"].as_str().unwrap_or("").trim();
        let text = value["text"].as_str().unwrap_or("").trim();
        if text.is_empty() {
            return Err(
                "页面没有可读文本（可能仍在加载、被登录墙/验证码挡住，或内容在 iframe 中）。\
                 可先 browser_snapshot 查看页面元素，或 browser_captcha_wait 等待人工验证。"
                    .to_string(),
            );
        }

        let limit = args["max_chars"]
            .as_u64()
            .unwrap_or(20_000)
            .min(50_000) as usize;
        let total = text.chars().count();
        let truncated: String = text.chars().take(limit).collect();
        let mut out = format!("Page: {title} — {url}\n\n{truncated}");
        if total > limit {
            out.push_str(&format!(
                "\n\n[Content truncated at {limit} chars, original length: {total}]"
            ));
        }
        Ok(out)
    }
}
