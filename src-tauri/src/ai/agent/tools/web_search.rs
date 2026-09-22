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
fn parse_ddg_results(html: &str) -> Vec<(String, String, String)> {
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

/// A search hit: (title, url, snippet).
type Hit = (String, String, String);

/// Strip HTML tags (for snippets with inline <strong> etc.).
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// Parse Bing's HTML results (`<li class="b_algo">` blocks).
fn parse_bing_results(html: &str) -> Vec<Hit> {
    let mut results: Vec<Hit> = Vec::new();
    let mut rest = html;
    const ITEM: &str = "<li class=\"b_algo\"";
    while results.len() < MAX_RESULTS {
        let Some(pos) = rest.find(ITEM) else { break };
        let after = &rest[pos + ITEM.len()..];
        let end = after.find(ITEM).unwrap_or(after.len());
        let block = &after[..end];
        rest = &after[end..];

        let Some(h2) = block.find("<h2>") else { continue };
        let Some(a) = block[h2..].find("<a href=\"") else { continue };
        let a_chunk = &block[h2 + a + 9..];
        let Some(q) = a_chunk.find('"') else { continue };
        let url = a_chunk[..q].to_string();
        let title = match a_chunk[q..].find("</a>") {
            Some(e) => {
                let t = &a_chunk[q..q + e];
                let t = t.rsplit('>').next().unwrap_or(t);
                decode_entities(t)
            }
            None => continue,
        };
        if url.is_empty() || title.is_empty() {
            continue;
        }
        let snippet = match block.find("<p") {
            Some(p) => match block[p..].find('>') {
                Some(g) => {
                    let body = &block[p + g + 1..];
                    match body.find("</p>") {
                        Some(e) => decode_entities(&strip_tags(&body[..e])),
                        None => String::new(),
                    }
                }
                None => String::new(),
            },
            None => String::new(),
        };
        results.push((title, url, snippet));
    }
    results
}

/// Parse a JSON search API's results array with the given field names.
fn parse_json_hits(body: &str, results_path: &[&str], title_key: &str, url_key: &str, content_key: &str) -> Vec<Hit> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else { return Vec::new() };
    let mut node = &v;
    for key in results_path {
        let Some(next) = node.get(*key) else { return Vec::new() };
        node = next;
    }
    let Some(arr) = node.as_array() else { return Vec::new() };
    arr.iter()
        .filter_map(|r| {
            let title = r.get(title_key)?.as_str()?.to_string();
            let url = r.get(url_key)?.as_str()?.to_string();
            let snippet = r.get(content_key).and_then(|c| c.as_str()).unwrap_or("").to_string();
            Some((title, url, snippet))
        })
        .take(MAX_RESULTS)
        .collect()
}

/// Execute one engine; Ok(vec![]) = reachable but no parseable results
/// (block page, markup change, …) — the caller falls through to the next.
async fn run_engine(
    client: &reqwest::Client,
    engine: &crate::core::models::SearchEngineConfig,
    query: &str,
) -> Result<Vec<Hit>, String> {
    match engine.id.as_str() {
        "bing" => {
            let url = format!("https://www.bing.com/search?q={}&mkt=zh-CN", percent_encode_query(query));
            let body = client.get(&url).send().await.map_err(|e| e.to_string())?
                .text().await.map_err(|e| e.to_string())?;
            Ok(parse_bing_results(&body))
        }
        "duckduckgo" => {
            let url = format!("https://html.duckduckgo.com/html/?q={}", percent_encode_query(query));
            let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("HTTP {}", resp.status()));
            }
            let body = resp.text().await.map_err(|e| e.to_string())?;
            Ok(parse_ddg_results(&body))
        }
        "tavily" => {
            let key = engine.api_key.as_deref().filter(|k| !k.trim().is_empty())
                .ok_or("未配置 API Key")?;
            let resp = client
                .post("https://api.tavily.com/search")
                .json(&serde_json::json!({"api_key": key, "query": query, "max_results": MAX_RESULTS}))
                .send().await.map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("HTTP {}", resp.status()));
            }
            let body = resp.text().await.map_err(|e| e.to_string())?;
            Ok(parse_json_hits(&body, &["results"], "title", "url", "content"))
        }
        "brave" => {
            let key = engine.api_key.as_deref().filter(|k| !k.trim().is_empty())
                .ok_or("未配置 API Key")?;
            let url = format!("https://api.search.brave.com/res/v1/web/search?q={}&count={}", percent_encode_query(query), MAX_RESULTS);
            let resp = client.get(&url).header("X-Subscription-Token", key)
                .send().await.map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("HTTP {}", resp.status()));
            }
            let body = resp.text().await.map_err(|e| e.to_string())?;
            Ok(parse_json_hits(&body, &["web", "results"], "title", "url", "description"))
        }
        "searxng" => {
            let base = engine.base_url.as_deref().filter(|u| !u.trim().is_empty())
                .ok_or("未配置实例地址")?;
            let url = format!("{}/search?q={}&format=json", base.trim_end_matches('/'), percent_encode_query(query));
            let body = client.get(&url).send().await.map_err(|e| e.to_string())?
                .text().await.map_err(|e| e.to_string())?;
            Ok(parse_json_hits(&body, &["results"], "title", "url", "content"))
        }
        other => Err(format!("未知引擎: {other}")),
    }
}

fn engine_label(id: &str) -> &'static str {
    match id {
        "bing" => "Bing",
        "duckduckgo" => "DuckDuckGo",
        "tavily" => "Tavily",
        "brave" => "Brave",
        "searxng" => "SearXNG",
        _ => "未知引擎",
    }
}

/// Web search over the user's configured engines, in order: the first engine
/// returning results wins — one tool call, no wasted LLM retry rounds.
pub struct WebSearchTool {
    db: SqlitePool,
    web_proxy: Option<String>,
}

impl WebSearchTool {
    pub fn new(db: SqlitePool, web_proxy: Option<String>) -> Self {
        Self { db, web_proxy }
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
        "Search the web for information. Tries the user's configured search engines in order (设置 → 网络搜索) until one returns results. Returns up to 10 results with title, URL, and snippet. Read-only, auto-approved."
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

        let engines: Vec<_> = crate::core::settings_service::load_app_settings(&self.db)
            .await
            .map(|s| s.search_engines)
            .unwrap_or_default()
            .into_iter()
            .filter(|e| e.enabled)
            .collect();
        if engines.is_empty() {
            return Err("未启用任何搜索引擎（请在 设置 → 网络搜索 中配置）".to_string());
        }

        let mut builder = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36");
        if let Some(proxy) = super::resolve_web_proxy(&self.db, self.web_proxy.as_deref()).await {
            if let Ok(p) = reqwest::Proxy::all(&proxy) {
                builder = builder.proxy(p);
            }
        }
        let client = builder.build().map_err(|e| format!("client error: {e}"))?;

        let mut errors: Vec<String> = Vec::new();
        let mut saw_empty = false;
        for engine in &engines {
            match run_engine(&client, engine, query).await {
                Ok(results) if !results.is_empty() => {
                    let shown: Vec<_> = results.into_iter().take(max).collect();
                    let lines: Vec<String> = shown
                        .iter()
                        .map(|(title, url, snippet)| {
                            let mut line = format!("- **{title}**\n  {url}");
                            if !snippet.is_empty() {
                                line.push_str(&format!("\n  {snippet}"));
                            }
                            line
                        })
                        .collect();
                    return Ok(format!(
                        "{}\n\n共返回 {} 条（引擎：{}）",
                        lines.join("\n\n"),
                        shown.len(),
                        engine_label(&engine.id)
                    ));
                }
                Ok(_) => saw_empty = true,
                Err(e) => errors.push(format!("{}: {e}", engine_label(&engine.id))),
            }
        }
        if saw_empty {
            return Ok(format!("No results found for '{query}'."));
        }
        Err(format!("所有可用搜索引擎均失败 —— {}", errors.join("；")))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bing_parser_extracts_hits() {
        let html = r#"<ol id="b_results">
          <li class="b_algo"><h2><a href="https://a.com/x">标题一</a></h2><div class="b_caption"><p>摘要<strong>加粗</strong>一</p></div></li>
          <li class="b_algo"><h2><a href="https://b.com/y">标题二</a></h2></li>
        </ol>"#;
        let hits = parse_bing_results(html);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].0, "标题一");
        assert_eq!(hits[0].1, "https://a.com/x");
        assert_eq!(hits[0].2, "摘要加粗一");
        assert_eq!(hits[1].2, "");
    }

    #[test]
    fn json_hits_parser_walks_paths() {
        let body = r#"{"results": [{"title": "T", "url": "https://x", "content": "C"}, {"title": "T2", "url": "https://y"}]}"#;
        let hits = parse_json_hits(body, &["results"], "title", "url", "content");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[1].2, "");
        // Brave nests under web.results
        let brave = r#"{"web": {"results": [{"title": "B", "url": "https://z", "description": "D"}]}}"#;
        assert_eq!(parse_json_hits(brave, &["web", "results"], "title", "url", "description").len(), 1);
        assert!(parse_json_hits("not json", &["results"], "title", "url", "content").is_empty());
    }

    #[test]
    fn strip_tags_removes_inline_markup() {
        assert_eq!(strip_tags("a<b>x</b>c"), "axc");
        assert_eq!(strip_tags("<strong>加粗</strong>文本"), "加粗文本");
    }
}
