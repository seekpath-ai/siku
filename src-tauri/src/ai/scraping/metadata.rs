use reqwest::Client;
use serde::Deserialize;
use tracing::instrument;

/// CrossRef work metadata (bibliographic record used for enrichment).
#[derive(Debug, Clone, Deserialize)]
pub struct CrossrefWork {
    pub title: Option<String>,
    pub authors: Option<Vec<String>>,
    pub year: Option<i32>,
    pub journal: Option<String>,
    pub doi: Option<String>,
    pub abstract_text: Option<String>,
    pub citation_count: Option<u32>,
    pub volume: Option<String>,
    pub issue: Option<String>,
    pub pages: Option<String>,
    pub publisher: Option<String>,
    pub issn: Option<String>,
    pub url: Option<String>,
}

/// Parse a CrossRef "message" JSON object into a CrossrefWork.
fn parse_message(msg: &serde_json::Value) -> CrossrefWork {
    let title = msg["title"][0].as_str().map(|s| s.to_string());

    let authors: Option<Vec<String>> = msg["author"].as_array().map(|arr| {
        arr.iter()
            .filter_map(|a| {
                let given = a["given"].as_str().unwrap_or("");
                let family = a["family"].as_str().unwrap_or("");
                if family.is_empty() { None } else { Some(format!("{} {}", given, family)) }
            })
            .collect()
    });

    let year = msg["published-print"]["date-parts"][0][0]
        .as_i64()
        .or_else(|| msg["published-online"]["date-parts"][0][0].as_i64())
        .or_else(|| msg["created"]["date-parts"][0][0].as_i64())
        .map(|y| y as i32);

    CrossrefWork {
        title,
        authors,
        year,
        journal: msg["container-title"][0].as_str().map(|s| s.to_string()),
        doi: msg["DOI"].as_str().map(|s| s.to_string()),
        abstract_text: msg["abstract"].as_str().map(|s| s.to_string()),
        citation_count: msg["is-referenced-by-count"].as_u64().map(|c| c as u32),
        volume: msg["volume"].as_str().map(|s| s.to_string()),
        issue: msg["issue"].as_str().map(|s| s.to_string()),
        pages: msg["page"].as_str().map(|s| s.to_string()),
        publisher: msg["publisher"].as_str().map(|s| s.to_string()),
        issn: msg["ISSN"].as_array().and_then(|a| a.first()).and_then(|v| v.as_str()).map(|s| s.to_string()),
        url: msg["URL"].as_str().map(|s| s.to_string()),
    }
}

/// Fetch metadata from CrossRef by DOI
#[instrument]
pub async fn fetch_by_doi(
    doi: &str,
    proxy: Option<&str>,
) -> Result<Option<CrossrefWork>, String> {
    let mut builder = Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("Siku/0.1 (mailto:siku@example.com)");

    if let Some(p) = proxy {
        if !p.is_empty() {
            let proxy = reqwest::Proxy::all(p).map_err(|e| format!("proxy: {e}"))?;
            builder = builder.proxy(proxy);
        }
    }

    let client = builder.build().map_err(|e| format!("client: {e}"))?;

    let url = format!("https://api.crossref.org/works/{}", doi);

    let resp = client.get(&url).send().await.map_err(|e| format!("CrossRef error: {e}"))?;

    if !resp.status().is_success() {
        return Ok(None);
    }

    let json: serde_json::Value = resp.json().await.map_err(|e| format!("json: {e}"))?;
    let mut work = parse_message(&json["message"]);
    work.doi = Some(doi.to_string());
    Ok(Some(work))
}

/// Search CrossRef by title/author
#[instrument]
pub async fn search_works(
    query: &str,
    rows: u32,
    proxy: Option<&str>,
) -> Result<Vec<CrossrefWork>, String> {
    let mut builder = Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("Siku/0.1 (mailto:siku@example.com)");

    if let Some(p) = proxy {
        if !p.is_empty() {
            let proxy = reqwest::Proxy::all(p).map_err(|e| format!("proxy: {e}"))?;
            builder = builder.proxy(proxy);
        }
    }

    let client = builder.build().map_err(|e| format!("client: {e}"))?;

    let encoded_query = query.replace(' ', "+");
    let url = format!(
        "https://api.crossref.org/works?query={}&rows={}",
        encoded_query,
        rows.min(20)
    );

    let resp = client.get(&url).send().await.map_err(|e| format!("CrossRef error: {e}"))?;
    let json: serde_json::Value = resp.json().await.map_err(|e| format!("json: {e}"))?;

    let items = json["message"]["items"]
        .as_array()
        .map(|arr| arr.iter().map(parse_message).collect::<Vec<_>>())
        .unwrap_or_default();

    Ok(items)
}
