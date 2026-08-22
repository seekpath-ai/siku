use async_trait::async_trait;
use sqlx::SqlitePool;
use crate::ai::agent::tool_registry::{Tool, ToolParameter};

/// Rough HTML → plain text: drops script/style blocks, strips tags, and
/// inserts newlines after block elements.
fn html_to_text(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let chars: Vec<char> = html.chars().collect();
    let lchars: Vec<char> = lower.chars().collect();
    let mut out = String::with_capacity(html.len() / 2);
    let mut in_tag = false;
    let mut skip = 0usize;
    let mut tag_name = String::new();
    let mut i = 0;
    let n = chars.len();

    while i < n {
        let c = chars[i];
        if skip > 0 {
            if c == '<' {
                let rest: String = lchars[i..].iter().take(9).collect();
                if rest.starts_with("</script") || rest.starts_with("</style") {
                    while i < n && chars[i] != '>' {
                        i += 1;
                    }
                    skip = 0;
                }
            }
            i += 1;
            continue;
        }
        if c == '<' {
            let rest: String = lchars[i..].iter().take(8).collect();
            if rest.starts_with("<script") || rest.starts_with("<style") {
                skip = 1;
                while i < n && chars[i] != '>' {
                    i += 1;
                }
                i += 1;
                continue;
            }
            in_tag = true;
            tag_name.clear();
            i += 1;
            continue;
        }
        if c == '>' {
            in_tag = false;
            if matches!(
                tag_name.as_str(),
                "p" | "div" | "li" | "br" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
                    | "tr" | "section" | "article" | "ul" | "ol" | "pre" | "table"
            ) {
                out.push('\n');
            }
            i += 1;
            continue;
        }
        if in_tag {
            if c.is_ascii_alphabetic() && tag_name.len() < 12 {
                tag_name.push(c);
            }
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }

    let out = out
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ");

    out.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

pub struct WebFetchTool {
    db: SqlitePool,
    web_proxy: Option<String>,
}

impl WebFetchTool {
    pub fn new(db: SqlitePool, web_proxy: Option<String>) -> Self {
        Self { db, web_proxy }
    }

    async fn proxy_for(&self) -> Option<String> {
        // Per-agent proxy wins; otherwise fall back to the global (default provider).
        if let Some(p) = self.web_proxy.clone().filter(|p| !p.is_empty()) {
            return Some(p);
        }
        crate::core::settings_service::load_llm_config(&self.db)
            .await
            .ok()
            .and_then(|c| c.proxy)
            .filter(|p| !p.is_empty())
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch a URL and return its text content (HTML pages are converted to plain text). Honors the configured LLM proxy. Read-only, auto-approved."
    }

    fn readonly(&self) -> bool {
        true
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![ToolParameter {
            name: "url".into(),
            param_type: "string".into(),
            description: "The URL to fetch".into(),
            required: true,
        }]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let url = args["url"].as_str().ok_or("url required")?;

        let mut builder = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent("Siku/0.1");
        if let Some(proxy) = self.proxy_for().await {
            if let Ok(p) = reqwest::Proxy::all(&proxy) {
                builder = builder.proxy(p);
            }
        }
        let client = builder.build().map_err(|e| format!("client error: {e}"))?;

        let resp = client.get(url).send().await.map_err(|e| format!("fetch error: {e}"))?;
        if !resp.status().is_success() {
            return Ok(format!("HTTP {} — could not fetch URL", resp.status()));
        }
        let body = resp.text().await.map_err(|e| format!("read error: {e}"))?;

        let is_html = body
            .get(..2048)
            .map(|h| h.to_ascii_lowercase().contains("<html") || h.contains("<!doctype") || h.contains("<body"))
            .unwrap_or(false);
        let text = if is_html { html_to_text(&body) } else { body };

        let max_chars = crate::core::settings_service::cached_settings()
            .tool_web_fetch_max_chars
            .max(1) as usize;
        let char_count = text.chars().count();
        let truncated: String = text.chars().take(max_chars).collect();
        if char_count > max_chars {
            Ok(format!(
                "{truncated}\n\n[Content truncated at {max_chars} chars, original length: {char_count}]"
            ))
        } else {
            Ok(truncated)
        }
    }
}
