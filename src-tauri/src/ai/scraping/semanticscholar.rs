//! Semantic Scholar Graph API — the third research-discovery source
//! (alongside arXiv and Crossref). Free anonymous tier; requests are
//! rate-limited, so each scan issues exactly one search call per topic.

use reqwest::Client;
use serde::Deserialize;
use tracing::instrument;

/// A paper result from Semantic Scholar search.
#[derive(Debug, Clone)]
pub struct S2Paper {
    pub s2_id: String,
    pub title: String,
    pub authors: Vec<String>,
    pub abstract_text: Option<String>,
    pub year: Option<i64>,
    pub citation_count: Option<i64>,
    pub doi: Option<String>,
    pub arxiv_id: Option<String>,
    /// Open-access PDF when one exists; otherwise the S2 landing page.
    pub pdf_url: Option<String>,
    pub url: String,
}

#[derive(Deserialize)]
struct S2SearchResponse {
    #[serde(default)]
    data: Vec<S2PaperRaw>,
}

#[derive(Deserialize)]
struct S2PaperRaw {
    #[serde(rename = "paperId")]
    paper_id: String,
    title: Option<String>,
    #[serde(default)]
    authors: Vec<S2Author>,
    #[serde(rename = "abstract")]
    abstract_text: Option<String>,
    year: Option<i64>,
    #[serde(rename = "citationCount")]
    citation_count: Option<i64>,
    #[serde(rename = "externalIds", default)]
    external_ids: Option<S2ExternalIds>,
    #[serde(rename = "openAccessPdf")]
    open_access_pdf: Option<S2OpenAccessPdf>,
    url: Option<String>,
}

#[derive(Deserialize)]
struct S2Author {
    name: Option<String>,
}

#[derive(Deserialize)]
struct S2ExternalIds {
    #[serde(rename = "DOI")]
    doi: Option<String>,
    #[serde(rename = "ArXiv")]
    arxiv: Option<String>,
}

#[derive(Deserialize)]
struct S2OpenAccessPdf {
    url: Option<String>,
}

/// Search Semantic Scholar by keywords. Returns up to `max_results` papers.
/// A 429 (rate limit) or any network failure is an Err — the caller treats a
/// failed source as non-fatal and moves on.
#[instrument]
pub async fn search(
    query: &str,
    max_results: u32,
    proxy: Option<&str>,
) -> Result<Vec<S2Paper>, String> {
    let mut builder = Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .user_agent("Siku/0.1");
    if let Some(p) = proxy {
        if !p.is_empty() {
            let proxy = reqwest::Proxy::all(p).map_err(|e| format!("proxy error: {e}"))?;
            builder = builder.proxy(proxy);
        }
    }
    let client = builder.build().map_err(|e| format!("client: {e}"))?;

    let url = format!(
        "https://api.semanticscholar.org/graph/v1/paper/search?query={}&limit={}&fields=title,authors,abstract,year,externalIds,citationCount,openAccessPdf,url",
        urlencoding(query),
        max_results.clamp(1, 50),
    );
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("Semantic Scholar API error: {e}"))?;
    if resp.status().as_u16() == 429 {
        return Err("Semantic Scholar rate limit (429)".to_string());
    }
    if !resp.status().is_success() {
        return Err(format!("Semantic Scholar HTTP {}", resp.status()));
    }
    let body: S2SearchResponse = resp.json().await.map_err(|e| format!("json: {e}"))?;

    Ok(body
        .data
        .into_iter()
        .map(|p| {
            let (doi, arxiv_id) = match p.external_ids {
                Some(ids) => (ids.doi, ids.arxiv),
                None => (None, None),
            };
            let landing = p
                .url
                .unwrap_or_else(|| format!("https://www.semanticscholar.org/paper/{}", p.paper_id));
            S2Paper {
                s2_id: p.paper_id,
                title: p.title.unwrap_or_else(|| "未命名文献".into()),
                authors: p.authors.into_iter().filter_map(|a| a.name).collect(),
                abstract_text: p.abstract_text,
                year: p.year,
                citation_count: p.citation_count,
                doi,
                arxiv_id,
                pdf_url: p.open_access_pdf.and_then(|o| o.url),
                url: landing,
            }
        })
        .collect())
}

/// Minimal percent-encoding for query strings (avoid a new dependency for
/// one call site).
fn urlencoding(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
