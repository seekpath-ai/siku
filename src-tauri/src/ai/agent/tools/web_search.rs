use async_trait::async_trait;
use sqlx::SqlitePool;
use crate::ai::agent::tool_registry::{Tool, ToolParameter};

const MAX_RESULTS: usize = 10;
const DEFAULT_RESULTS: usize = 6;

/// Minimal percent-decoding (also converts `+` to space).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .trim()
        .to_string()
}

/// DuckDuckGo result pages redirect through `uddg=`; extract the real URL.
fn extract_real_url(href: &str) -> String {
    if let Some(pos) = href.find("uddg=") {
        let rest = &href[pos + 5..];
        let end = rest.find('&').unwrap_or(rest.len());
        return percent_decode(&rest[..end]);
    }
    href.to_string()
}

/// Parse DuckDuckGo's HTML results into (title, url, snippet) triples.
fn parse_results(html: &str) -> Vec<(String, String, String)> {
    let mut results = Vec::new();
    let marker = "<a rel=\"nofollow\" class=\"result__a\" href=\"";
    let mut rest = html;
    while results.len() < MAX_RESULTS {
        let Some(pos) = rest.find(marker) else { break };
        let chunk = &rest[pos + marker.len()..];
        let Some(quote) = chunk.find('"') else { break };
        let href = &chunk[..quote];
        let title = {
            let after = &chunk[quote + 1..];
            match after.find("</a>") {
                Some(e) => {
                    let t = &after[..e];
                    let t = t.rsplit('>').next().unwrap_or(t);
                    decode_entities(t)
                }
                None => break,
            }
        };
        // Snippet: next occurrence of class="result__snippet"
        let snippet = match chunk.find("class=\"result__snippet\"") {
            Some(sp) => {
                let after = &chunk[sp..];
                match after.find('>') {
                    Some(g) => {
                        let body = &after[g + 1..];
                        match body.find("</a>") {
                            Some(e) => decode_entities(&body[..e]),
                            None => String::new(),
                        }
                    }
                    None => String::new(),
                }
            }
            None => String::new(),
        };
        if !title.is_empty() && !href.is_empty() {
            results.push((title, extract_real_url(href), snippet));
        }
        // Advance past this result.
        rest = &chunk[quote + 1..];
    }
    results
}

/// Search the web via DuckDuckGo's HTML endpoint (free, no API key).
pub struct WebSearchTool {
    db: SqlitePool,
    web_proxy: Option<String>,
}

impl WebSearchTool {
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
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn readonly(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Search the web for information (DuckDuckGo). Returns up to 10 results with title, URL, and snippet. Read-only, auto-approved."
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![
            ToolParameter {
                name: "query".into(),
                param_type: "string".into(),
                description: "Search query".into(),
                required: true,
            },
            ToolParameter {
                name: "max_results".into(),
                param_type: "integer".into(),
                description: "Maximum results (default 6, max 10)".into(),
                required: false,
            },
        ]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let query = args["query"].as_str().unwrap_or("");
        if query.trim().is_empty() {
            return Err("query required".to_string());
        }
        let max = args["max_results"].as_u64().unwrap_or(DEFAULT_RESULTS as u64).min(MAX_RESULTS as u64) as usize;

        let mut builder = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36");
        if let Some(proxy) = self.proxy_for().await {
            if let Ok(p) = reqwest::Proxy::all(&proxy) {
                builder = builder.proxy(p);
            }
        }
        let client = builder.build().map_err(|e| format!("client error: {e}"))?;

        let url = format!(
            "https://html.duckduckgo.com/html/?q={}",
            percent_encode_query(query)
        );
        let resp = client.get(&url).send().await.map_err(|e| format!("search error: {e}"))?;
        if !resp.status().is_success() {
            return Ok(format!("Search failed with HTTP {}", resp.status()));
        }
        let body = resp.text().await.map_err(|e| format!("read error: {e}"))?;

        let results = parse_results(&body);
        if results.is_empty() {
            return Ok(format!("No results found for '{query}'."));
        }

        let shown = results.into_iter().take(max);
        let lines: Vec<String> = shown
            .map(|(title, url, snippet)| {
                let mut line = format!("- **{title}**\n  {url}");
                if !snippet.is_empty() {
                    line.push_str(&format!("\n  {snippet}"));
                }
                line
            })
            .collect();
        Ok(lines.join("\n\n"))
    }
}

/// URL-encode a query for the DuckDuckGo endpoint (RFC 3986 unreserved kept).
fn percent_encode_query(query: &str) -> String {
    let mut out = String::new();
    for b in query.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
