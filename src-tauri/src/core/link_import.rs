use std::path::{Path, PathBuf};

use futures::StreamExt;
use reqwest::Client;
use sqlx::SqlitePool;
use tracing::{info, instrument, warn};
use uuid::Uuid;

use crate::core::models::Paper;
use crate::core::paper_service::{finalize_paper_import, ImportMetadata};
use crate::core::time::now_iso;
use crate::file_store;

const PDF_DOWNLOAD_TIMEOUT_SECS: u64 = 30;
const MAX_PDF_BYTES: usize = 200 * 1024 * 1024;

/// Platform-agnostic metadata extracted from a link.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PaperMetadata {
    pub title: String,
    pub authors: Vec<String>,
    pub year: Option<i32>,
    pub journal: Option<String>,
    pub doi: Option<String>,
    pub url: Option<String>,
    pub abstract_text: Option<String>,
    pub keywords: Vec<String>,
    pub pdf_url: Option<String>,
    pub page_count: Option<i32>,
    pub isbn: Option<String>,
}

/// A structured error returned to the frontend so it can show localized messages.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LinkImportError {
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for LinkImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for LinkImportError {}

impl LinkImportError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

/// What kind of academic link the user pasted.
#[derive(Debug, Clone)]
pub enum ParsedLink {
    Doi(String),
    Arxiv(String),
    PubMed(String),
    SemanticScholar(String),
    Isbn(String),
    DirectPdf(String),
    Unsupported,
}

/// Parse a pasted URL into a known academic link type.
pub fn parse_link(url: &str) -> ParsedLink {
    let trimmed = url.trim();
    let lower = trimmed.to_lowercase();

    // DOI: either a doi.org URL or a bare 10.xxxx/... string.
    if let Some(doi) = extract_doi(trimmed) {
        return ParsedLink::Doi(doi);
    }

    // arXiv: abs or pdf URLs. Check before direct PDF so that arxiv.org/pdf/xxx.pdf
    // still gets its metadata from the arXiv API.
    if let Some(id) = extract_arxiv_id(trimmed) {
        return ParsedLink::Arxiv(id);
    }

    // PubMed.
    if let Some(id) = extract_pubmed_id(trimmed) {
        return ParsedLink::PubMed(id);
    }

    // ISBN: 10 or 13 digits (hyphens/spaces ignored, ISBN-10 may end in X).
    if let Some(isbn) = extract_isbn(trimmed) {
        return ParsedLink::Isbn(isbn);
    }

    // Direct PDF: URL ends with .pdf (allow query strings or fragments after).
    let path_and_query = lower
        .split('#')
        .next()
        .unwrap_or("")
        .split('?')
        .next()
        .unwrap_or("");
    if path_and_query.ends_with(".pdf") {
        return ParsedLink::DirectPdf(trimmed.to_string());
    }

    // Semantic Scholar: extract the last path segment if it looks like an S2 ID.
    if lower.contains("semanticscholar.org") {
        if let Some(id) = extract_semantic_scholar_id(trimmed) {
            return ParsedLink::SemanticScholar(id);
        }
    }

    ParsedLink::Unsupported
}

/// Normalize and validate an ISBN (10 or 13 digits; hyphens/spaces ignored;
/// ISBN-10 may end with X). Returns the uppercase normalized digits.
fn extract_isbn(input: &str) -> Option<String> {
    let digits: String = input
        .chars()
        .filter(|c| c.is_ascii_digit() || matches!(c, 'x' | 'X'))
        .map(|c| c.to_ascii_uppercase())
        .collect();
    let len = digits.len();
    if len == 10 {
        Some(digits)
    } else if len == 13 && (digits.starts_with("978") || digits.starts_with("979")) {
        Some(digits)
    } else {
        None
    }
}

fn extract_doi(url: &str) -> Option<String> {
    let lower = url.to_lowercase();
    // doi.org/10.xxxx/... or dx.doi.org/10.xxxx/...
    if let Some(idx) = lower.find("doi.org/") {
        let doi = &url[idx + "doi.org/".len()..];
        let doi = doi.split(&['?', '#'] as &[char]).next().unwrap_or(doi);
        let doi = doi.trim_end_matches('/');
        if looks_like_doi(doi) {
            return Some(doi.to_string());
        }
    }
    // Bare DOI: starts with 10. and contains '/'.
    let bare = url.split(&['?', '#'] as &[char]).next().unwrap_or(url).trim_end_matches('/');
    if looks_like_doi(bare) {
        return Some(bare.to_string());
    }
    None
}

fn looks_like_doi(s: &str) -> bool {
    // DOI prefix is "10." followed by at least 4 digits, then "/...".
    s.starts_with("10.")
        && s.len() > 6
        && s.contains('/')
        && s[3..].chars().take_while(|c| c.is_ascii_digit()).count() >= 4
}

fn extract_arxiv_id(url: &str) -> Option<String> {
    let lower = url.to_lowercase();
    for prefix in ["arxiv.org/abs/", "arxiv.org/pdf/"] {
        if let Some(idx) = lower.find(prefix) {
            let rest = &url[idx + prefix.len()..];
            let id = rest.split('/').next().unwrap_or(rest);
            let id = id.split('?').next().unwrap_or(id);
            let id = id.trim_end_matches(".pdf");
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
    }
    None
}

fn extract_pubmed_id(url: &str) -> Option<String> {
    let lower = url.to_lowercase();
    if let Some(idx) = lower.find("pubmed.ncbi.nlm.nih.gov/") {
        let rest = &url[idx + "pubmed.ncbi.nlm.nih.gov/".len()..];
        let id = rest.split('/').next().unwrap_or(rest);
        let id = id.split(&['?', '#'] as &[char]).next().unwrap_or(id);
        if id.chars().all(|c| c.is_ascii_digit()) && !id.is_empty() {
            return Some(id.to_string());
        }
    }
    None
}

fn extract_semantic_scholar_id(url: &str) -> Option<String> {
    url.rsplit('/')
        .next()
        .map(|s| s.split(&['?', '#'] as &[char]).next().unwrap_or(s))
        .filter(|s| !s.is_empty() && s.len() >= 16 && s.chars().all(|c| c.is_ascii_alphanumeric()))
        .map(|s| s.to_string())
}

/// Read the global network proxy if one has been configured.
/// Falls back from `network.proxy` to the legacy `llm.proxy` setting.
async fn get_proxy(db: &SqlitePool) -> Option<String> {
    for key in ["network.proxy", "llm.proxy"] {
        if let Ok(Some(value)) = crate::core::settings_service::get_setting(db, key).await {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn http_client(proxy: Option<&str>) -> Result<Client, LinkImportError> {
    http_client_with_compression(proxy, true)
}

fn http_client_with_compression(
    proxy: Option<&str>,
    allow_compression: bool,
) -> Result<Client, LinkImportError> {
    let mut builder = Client::builder()
        .timeout(std::time::Duration::from_secs(PDF_DOWNLOAD_TIMEOUT_SECS))
        .user_agent("Siku/0.1 (mailto:siku@example.com)")
        .gzip(allow_compression)
        .brotli(allow_compression)
        .deflate(allow_compression);

    if let Some(p) = proxy {
        let proxy = reqwest::Proxy::all(p).map_err(|e| {
            LinkImportError::new("network_error", format!("代理配置无效: {e}"))
        })?;
        builder = builder.proxy(proxy);
    }

    builder
        .build()
        .map_err(|e| LinkImportError::new("network_error", format!("HTTP 客户端创建失败: {e}")))
}

/// Fetch metadata from CrossRef by DOI.
async fn fetch_crossref(
    doi: &str,
    proxy: Option<&str>,
) -> Result<Option<PaperMetadata>, LinkImportError> {
    let client = http_client(proxy)?;
    let url = format!("https://api.crossref.org/works/{}", doi);
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| LinkImportError::new("network_error", format!("CrossRef: {e}")))?;
    if !resp.status().is_success() {
        return Ok(None);
    }
    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| LinkImportError::new("network_error", format!("CrossRef JSON: {e}")))?;
    let msg = &json["message"];

    let title = msg["title"][0].as_str().map(|s| s.to_string());
    let authors: Vec<String> = msg["author"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|a| {
                    let given = a["given"].as_str().unwrap_or("");
                    let family = a["family"].as_str().unwrap_or("");
                    if family.is_empty() {
                        None
                    } else {
                        Some(format!("{} {}", given, family).trim().to_string())
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    let year = msg["published-print"]["date-parts"][0][0]
        .as_i64()
        .or_else(|| msg["published-online"]["date-parts"][0][0].as_i64())
        .or_else(|| msg["created"]["date-parts"][0][0].as_i64())
        .map(|y| y as i32);

    let Some(title) = title else {
        return Ok(None);
    };

    Ok(Some(PaperMetadata {
        title,
        authors,
        year,
        journal: msg["container-title"][0].as_str().map(|s| s.to_string()),
        doi: Some(doi.to_string()),
        url: msg["URL"].as_str().map(|s| s.to_string()),
        abstract_text: msg["abstract"]
            .as_str()
            .map(strip_xml_tags)
            .filter(|s| !s.is_empty()),
        keywords: vec![],
        pdf_url: None,
        page_count: None,
        isbn: None,
    }))
}

/// Fetch metadata from OpenAlex by DOI as a fallback.
async fn fetch_openalex(
    doi: &str,
    proxy: Option<&str>,
) -> Result<Option<PaperMetadata>, LinkImportError> {
    let client = http_client(proxy)?;
    let url = format!("https://api.openalex.org/works/doi:{}", doi);
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| LinkImportError::new("network_error", format!("OpenAlex: {e}")))?;
    if !resp.status().is_success() {
        return Ok(None);
    }
    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| LinkImportError::new("network_error", format!("OpenAlex JSON: {e}")))?;

    let title = json["display_name"].as_str().map(|s| s.to_string());
    let authors: Vec<String> = json["authorships"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|a| a["author"]["display_name"].as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let year = json["publication_year"].as_i64().map(|y| y as i32);
    let journal = json["primary_location"]["source"]["display_name"]
        .as_str()
        .map(|s| s.to_string());
    let pdf_url = json["open_access"]["oa_url"].as_str().map(|s| s.to_string());
    let url = json["primary_location"]["landing_page_url"]
        .as_str()
        .map(|s| s.to_string());

    let Some(title) = title else {
        return Ok(None);
    };

    let abstract_text = reconstruct_openalex_abstract(&json["abstract_inverted_index"]);

    Ok(Some(PaperMetadata {
        title,
        authors,
        year,
        journal,
        doi: Some(doi.to_string()),
        url: url.or_else(|| Some(format!("https://doi.org/{}", doi))),
        abstract_text,
        keywords: vec![],
        pdf_url,
        page_count: None,
        isbn: None,
    }))
}

/// Reconstruct a plain-text abstract from OpenAlex's inverted index object.
fn reconstruct_openalex_abstract(value: &serde_json::Value) -> Option<String> {
    let obj = value.as_object()?;
    let mut tokens: Vec<(usize, &str)> = Vec::new();
    for (word, positions) in obj {
        if let Some(arr) = positions.as_array() {
            for pos in arr.iter().filter_map(|v| v.as_u64()) {
                tokens.push((pos as usize, word.as_str()));
            }
        }
    }
    if tokens.is_empty() {
        return None;
    }
    tokens.sort_by_key(|(pos, _)| *pos);
    let words: Vec<&str> = tokens.into_iter().map(|(_, word)| word).collect();
    Some(words.join(" "))
}

/// Fetch metadata from arXiv by ID.
/// If the arXiv API is rate-limited or otherwise unreachable, fall back to a
/// sparse record so the user can still import the PDF directly.
async fn fetch_arxiv(
    id: &str,
    proxy: Option<&str>,
) -> Result<Option<PaperMetadata>, LinkImportError> {
    match crate::ai::scraping::arxiv::fetch_by_id(id, proxy).await {
        Ok(Some(p)) => Ok(Some(PaperMetadata {
            title: p.title,
            authors: p.authors,
            year: p.published.split('-').next().and_then(|s| s.parse().ok()),
            journal: None,
            doi: p.doi.clone(),
            url: Some(format!("https://arxiv.org/abs/{}", p.arxiv_id)),
            abstract_text: Some(p.abstract_text),
            keywords: p.categories,
            pdf_url: Some(format!("https://arxiv.org/pdf/{}.pdf", p.arxiv_id)),
            page_count: None,
            isbn: None,
        })),
        Ok(None) => Ok(None),
        Err(e) => {
            warn!(
                arxiv_id = %id,
                error = %e,
                "arXiv API metadata fetch failed, falling back to direct PDF"
            );
            Ok(Some(arxiv_fallback_metadata(id)))
        }
    }
}

/// Minimal metadata for an arXiv paper when the API is unavailable.
/// Lets the import proceed by downloading the PDF directly and extracting
/// whatever metadata is embedded in the file.
fn arxiv_fallback_metadata(id: &str) -> PaperMetadata {
    PaperMetadata {
        title: format!("arXiv: {}", id),
        authors: vec![],
        year: None,
        journal: None,
        doi: None,
        url: Some(format!("https://arxiv.org/abs/{}", id)),
        abstract_text: None,
        keywords: vec![],
        pdf_url: Some(format!("https://arxiv.org/pdf/{}.pdf", id)),
        page_count: None,
        isbn: None,
    }
}

/// Fetch metadata from PubMed by ID.
async fn fetch_pubmed(
    id: &str,
    proxy: Option<&str>,
) -> Result<Option<PaperMetadata>, LinkImportError> {
    let client = http_client(proxy)?;

    // First get summary.
    let summary_url = format!(
        "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/esummary.fcgi?db=pubmed&id={}&retmode=json",
        id
    );
    let summary_resp = client
        .get(&summary_url)
        .send()
        .await
        .map_err(|e| LinkImportError::new("network_error", format!("PubMed summary: {e}")))?;
    if !summary_resp.status().is_success() {
        return Ok(None);
    }
    let summary: serde_json::Value = summary_resp
        .json()
        .await
        .map_err(|e| LinkImportError::new("network_error", format!("PubMed JSON: {e}")))?;
    let doc = summary["result"][id]
        .as_object()
        .ok_or_else(|| LinkImportError::new("metadata_not_found", "PubMed response missing result"))?;

    let title = doc["title"].as_str().map(|s| s.to_string());
    let authors: Vec<String> = doc["authors"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|a| a["name"].as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let year = doc["pubdate"]
        .as_str()
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.parse().ok());

    // Then fetch abstract via efetch.
    let abstract_text = fetch_pubmed_abstract(id, proxy).await.ok();

    let Some(title) = title else {
        return Ok(None);
    };

    Ok(Some(PaperMetadata {
        title,
        authors,
        year,
        journal: doc["fulljournalname"].as_str().map(|s| s.to_string()),
        doi: doc["elocationid"].as_str().map(|s| s.to_string()),
        url: Some(format!("https://pubmed.ncbi.nlm.nih.gov/{}/", id)),
        abstract_text,
        keywords: vec![],
        pdf_url: None,
        page_count: None,
        isbn: None,
    }))
}

async fn fetch_pubmed_abstract(id: &str, proxy: Option<&str>) -> Result<String, LinkImportError> {
    let client = http_client(proxy)?;
    let url = format!(
        "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/efetch.fcgi?db=pubmed&id={}&rettype=abstract",
        id
    );
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| LinkImportError::new("network_error", format!("PubMed efetch: {e}")))?;
    let text = resp
        .text()
        .await
        .map_err(|e| LinkImportError::new("network_error", format!("PubMed text: {e}")))?;

    parse_pubmed_abstract(&text)
}

/// Parse all <AbstractText> contents from a PubMed efetch XML response.
fn parse_pubmed_abstract(xml: &str) -> Result<String, LinkImportError> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut buf = Vec::new();
    let mut in_abstract_text = false;
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                if e.name().as_ref() == b"AbstractText" {
                    in_abstract_text = true;
                    current.clear();
                }
            }
            Ok(Event::Text(e)) => {
                if in_abstract_text {
                    if let Ok(s) = e.unescape() {
                        current.push_str(&s);
                    }
                }
            }
            Ok(Event::End(e)) => {
                if e.name().as_ref() == b"AbstractText" {
                    in_abstract_text = false;
                    let trimmed = current.trim();
                    if !trimmed.is_empty() {
                        parts.push(trimmed.to_string());
                    }
                    current.clear();
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(LinkImportError::new(
                    "metadata_not_found",
                    format!("PubMed XML parse error: {e}"),
                ));
            }
            _ => {}
        }
        buf.clear();
    }

    if parts.is_empty() {
        return Err(LinkImportError::new(
            "metadata_not_found",
            "no abstract found",
        ));
    }
    Ok(parts.join(" "))
}

/// Fetch metadata from Semantic Scholar by paper ID.
async fn fetch_semantic_scholar(
    id: &str,
    proxy: Option<&str>,
) -> Result<Option<PaperMetadata>, LinkImportError> {
    let client = http_client(proxy)?;
    let url = format!(
        "https://api.semanticscholar.org/graph/v1/paper/{}?fields=title,authors,year,venue,doi,abstract,openAccessPdf",
        id
    );
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| LinkImportError::new("network_error", format!("Semantic Scholar: {e}")))?;
    if !resp.status().is_success() {
        return Ok(None);
    }
    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| LinkImportError::new("network_error", format!("S2 JSON: {e}")))?;

    let title = json["title"].as_str().map(|s| s.to_string());
    let authors: Vec<String> = json["authors"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|a| a["name"].as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let pdf_url = json["openAccessPdf"]["url"].as_str().map(|s| s.to_string());

    let Some(title) = title else {
        return Ok(None);
    };

    Ok(Some(PaperMetadata {
        title,
        authors,
        year: json["year"].as_i64().map(|y| y as i32),
        journal: json["venue"].as_str().map(|s| s.to_string()),
        doi: json["doi"].as_str().map(|s| s.to_string()),
        url: Some(format!("https://semanticscholar.org/paper/{}", id)),
        abstract_text: json["abstract"].as_str().map(|s| s.to_string()),
        keywords: vec![],
        pdf_url,
        page_count: None,
        isbn: None,
    }))
}

/// Try to find an OA PDF URL for a DOI via Unpaywall.
async fn resolve_pdf_via_unpaywall(
    doi: &str,
    proxy: Option<&str>,
) -> Result<Option<String>, LinkImportError> {
    let client = http_client(proxy)?;
    let url = format!("https://api.unpaywall.org/v2/{doi}?email=siku@example.com");
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| LinkImportError::new("network_error", format!("Unpaywall: {e}")))?;
    if !resp.status().is_success() {
        return Ok(None);
    }
    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| LinkImportError::new("network_error", format!("Unpaywall JSON: {e}")))?;
    Ok(json["best_oa_location"]["url_for_pdf"]
        .as_str()
        .map(|s| s.to_string()))
}

/// Download a PDF to the managed paper store with retries, streaming, and basic validation.
async fn download_pdf(
    url: &str,
    app_data_dir: &Path,
    paper_id: &str,
    proxy: Option<&str>,
) -> Result<PathBuf, LinkImportError> {
    let mut last_err = LinkImportError::new("download_failed", "unknown error");

    for attempt in 0..3 {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(2 * attempt as u64)).await;
        }

        match try_download_pdf_stream(url, app_data_dir, paper_id, proxy, false).await {
            Ok(path) => return Ok(path),
            Err(e) => {
                warn!(
                    paper_id = %paper_id,
                    attempt = attempt + 1,
                    error = %e,
                    "PDF download attempt failed"
                );
                last_err = e;
            }
        }
    }

    // Some proxies/servers advertise a Content-Encoding that the body does not actually use,
    // causing reqwest's automatic decoder to fail. Fall back to a raw identity request.
    if last_err.message.contains("decoding") {
        warn!(
            paper_id = %paper_id,
            url = %url,
            "PDF download body decoding failed, falling back to identity (no compression)"
        );
        for attempt in 0..2 {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(2 * (attempt + 1) as u64)).await;
            }
            match try_download_pdf_stream(url, app_data_dir, paper_id, proxy, true).await {
                Ok(path) => return Ok(path),
                Err(e) => {
                    warn!(
                        paper_id = %paper_id,
                        attempt = attempt + 1,
                        error = %e,
                        "raw PDF download attempt failed"
                    );
                    last_err = LinkImportError::new(
                        "download_failed",
                        format!("{}（无压缩回退失败：{}）", last_err.message, e.message),
                    );
                }
            }
        }
    }

    Err(LinkImportError::new(
        "download_failed",
        format!("PDF 下载失败（已重试）: {}", last_err.message),
    ))
}

async fn try_download_pdf_stream(
    url: &str,
    app_data_dir: &Path,
    paper_id: &str,
    proxy: Option<&str>,
    accept_identity: bool,
) -> Result<PathBuf, LinkImportError> {
    let client = http_client_with_compression(proxy, !accept_identity)?;
    let mut req = client.get(url);
    if accept_identity {
        req = req.header("Accept-Encoding", "identity");
    }
    let resp = req
        .send()
        .await
        .map_err(|e| LinkImportError::new("download_failed", format!("request failed: {e}")))?;

    let final_url = resp.url().clone();
    let status = resp.status();
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_lowercase());

    if !status.is_success() {
        return Err(LinkImportError::new(
            "download_failed",
            format!("HTTP {} from {}", status, final_url),
        ));
    }

    file_store::ensure_paper_dir(app_data_dir, paper_id)
        .map_err(|e| LinkImportError::new("download_failed", format!("create paper dir: {e}")))?;
    let dest = file_store::original_pdf_path(app_data_dir, paper_id);

    let mut file = tokio::fs::File::create(&dest)
        .await
        .map_err(|e| LinkImportError::new("download_failed", format!("create pdf: {e}")))?;

    let mut stream = resp.bytes_stream();
    let mut total: usize = 0;
    let mut first_chunk_validated = false;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| {
            LinkImportError::new("download_failed", format!("read stream: {e}"))
        })?;

        if !first_chunk_validated {
            if !chunk.starts_with(b"%PDF") {
                let preview = String::from_utf8_lossy(&chunk[..chunk.len().min(120)]);
                warn!(
                    paper_id = %paper_id,
                    requested_url = %url,
                    final_url = %final_url,
                    status = %status,
                    content_type = ?content_type,
                    preview = %preview,
                    "downloaded content is not a PDF"
                );
                return Err(LinkImportError::new(
                    "download_failed",
                    format!(
                        "下载内容不是 PDF（status={}，content-type={:?}，最终 URL={}，前 120 字节：{}）",
                        status, content_type, final_url, preview
                    ),
                ));
            }
            first_chunk_validated = true;
        }

        total += chunk.len();
        if total > MAX_PDF_BYTES {
            let _ = tokio::fs::remove_file(&dest).await;
            return Err(LinkImportError::new(
                "download_failed",
                format!("PDF 超过大小上限 {}MB", MAX_PDF_BYTES / 1024 / 1024),
            ));
        }

        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
            .await
            .map_err(|e| LinkImportError::new("download_failed", format!("write pdf: {e}")))?;
    }

    if !first_chunk_validated {
        let _ = tokio::fs::remove_file(&dest).await;
        return Err(LinkImportError::new("download_failed", "PDF 内容为空"));
    }

    Ok(dest)
}

/// Resolve the final PDF URL from metadata.
async fn resolve_pdf_url(
    meta: &PaperMetadata,
    proxy: Option<&str>,
) -> Result<Option<String>, LinkImportError> {
    // Direct PDF URL from metadata is the strongest signal.
    if let Some(url) = meta.pdf_url.as_deref() {
        return Ok(Some(url.to_string()));
    }

    // Try Unpaywall for DOI.
    if let Some(doi) = meta.doi.as_deref() {
        if let Some(url) = resolve_pdf_via_unpaywall(doi, proxy).await? {
            return Ok(Some(url));
        }
    }

    Ok(None)
}

/// Fetch book metadata for an ISBN via the OpenLibrary API.
async fn fetch_isbn(
    isbn: &str,
    proxy: Option<&str>,
) -> Result<Option<PaperMetadata>, LinkImportError> {
    let url = format!(
        "https://openlibrary.org/api/books?bibkeys=ISBN:{}&format=json&jscmd=data",
        isbn
    );
    let client = http_client(proxy)?;
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| LinkImportError::new("network_error", format!("OpenLibrary 请求失败: {e}")))?;
    if !resp.status().is_success() {
        return Ok(None);
    }
    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| LinkImportError::new("network_error", format!("解析 OpenLibrary 响应失败: {e}")))?;
    let key = format!("ISBN:{isbn}");
    let Some(data) = json.get(&key) else {
        return Ok(None);
    };
    let title = data
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if title.is_empty() {
        return Ok(None);
    }
    let authors: Vec<String> = data
        .get("authors")
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|a| a.get("name").and_then(|n| n.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let year = data
        .get("publish_date")
        .and_then(|d| d.as_str())
        .and_then(|s| s.split_whitespace().filter_map(|t| t.parse::<i32>().ok()).last());
    let url = data.get("url").and_then(|v| v.as_str()).map(String::from);
    Ok(Some(PaperMetadata {
        title,
        authors,
        year,
        url,
        isbn: Some(isbn.to_string()),
        ..Default::default()
    }))
}

/// Fetch metadata for a parsed link, trying fallbacks where appropriate.
async fn fetch_metadata(
    parsed: &ParsedLink,
    proxy: Option<&str>,
) -> Result<PaperMetadata, LinkImportError> {
    match parsed {
        ParsedLink::Doi(doi) => {
            let mut last_err: Option<LinkImportError> = None;

            match fetch_crossref(doi, proxy).await {
                Ok(Some(meta)) => return Ok(meta),
                Ok(None) => {}
                Err(e) => {
                    if e.code != "network_error" {
                        return Err(e);
                    }
                    last_err = Some(e);
                }
            }

            match fetch_openalex(doi, proxy).await {
                Ok(Some(meta)) => return Ok(meta),
                Ok(None) => {}
                Err(e) => {
                    if e.code != "network_error" {
                        return Err(e);
                    }
                    last_err = Some(e);
                }
            }

            if let Some(e) = last_err {
                return Err(LinkImportError::new(
                    "metadata_not_found",
                    format!("无法通过 DOI 获取元数据: {} ({}", doi, e.message),
                ));
            }
            Err(LinkImportError::new(
                "metadata_not_found",
                format!("无法通过 DOI 获取元数据: {}", doi),
            ))
        }
        ParsedLink::Arxiv(id) => fetch_arxiv(id, proxy)
            .await?
            .ok_or_else(|| LinkImportError::new("metadata_not_found", format!("无法获取 arXiv 论文: {}", id))),
        ParsedLink::PubMed(id) => fetch_pubmed(id, proxy)
            .await?
            .ok_or_else(|| LinkImportError::new("metadata_not_found", format!("无法获取 PubMed 论文: {}", id))),
        ParsedLink::SemanticScholar(id) => fetch_semantic_scholar(id, proxy)
            .await?
            .ok_or_else(|| LinkImportError::new("metadata_not_found", format!("无法获取 Semantic Scholar 论文: {}", id))),
        ParsedLink::Isbn(id) => fetch_isbn(id, proxy)
            .await?
            .ok_or_else(|| LinkImportError::new("metadata_not_found", format!("无法获取 ISBN {} 的图书信息", id))),
        ParsedLink::DirectPdf(url) => Ok(PaperMetadata {
            url: Some(url.clone()),
            pdf_url: Some(url.clone()),
            ..Default::default()
        }),
        ParsedLink::Unsupported => Err(LinkImportError::new(
            "unsupported_url",
            "不支持的链接格式",
        )),
    }
}

/// Best-effort removal of XML/JATS tags from abstracts.
fn strip_xml_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for c in text.chars() {
        if c == '<' {
            in_tag = true;
        } else if c == '>' {
            in_tag = false;
        } else if !in_tag {
            out.push(c);
        }
    }
    out
}

/// Fill in a title for direct-PDF imports when no metadata source is available.
fn title_from_url(url: &str) -> String {
    url.rsplit('/')
        .next()
        .map(|s| s.split(&['?', '#'] as &[char]).next().unwrap_or(s))
        .map(|s| s.trim_end_matches(".pdf"))
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "Imported PDF".to_string())
}

/// Resolve metadata for a link without importing or downloading anything.
#[instrument(skip(db))]
pub async fn resolve_paper_link(
    db: &SqlitePool,
    url: String,
) -> Result<PaperMetadata, LinkImportError> {
    let parsed = parse_link(&url);
    if matches!(parsed, ParsedLink::Unsupported) {
        return Err(LinkImportError::new("unsupported_url", "不支持的链接格式"));
    }

    let proxy = get_proxy(db).await;
    let mut meta = fetch_metadata(&parsed, proxy.as_deref()).await?;
    if meta.title.is_empty() {
        meta.title = title_from_url(&url);
    }
    if meta.url.is_none() {
        meta.url = Some(url.clone());
    }

    // Compute whether a PDF is likely available, but do not download yet.
    if meta.pdf_url.is_none() {
        if let Some(doi) = meta.doi.as_deref() {
            meta.pdf_url = resolve_pdf_via_unpaywall(doi, proxy.as_deref()).await?;
        }
    }

    Ok(meta)
}

/// Result of importing a paper from a link.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PaperImportResult {
    pub paper: Paper,
    pub warning: Option<String>,
}

/// Main entry point: import a paper from a URL.
#[instrument(skip(db, app_data_dir, preview_metadata), fields(paper_id))]
pub async fn import_paper_from_link(
    db: &SqlitePool,
    app_data_dir: &Path,
    url: String,
    preview_metadata: Option<PaperMetadata>,
) -> Result<PaperImportResult, LinkImportError> {
    let parsed = parse_link(&url);
    if matches!(parsed, ParsedLink::Unsupported) {
        return Err(LinkImportError::new("unsupported_url", "不支持的链接格式"));
    }

    let proxy = get_proxy(db).await;

    // Track this import attempt in the imports table.
    let import_id = Uuid::new_v4().to_string();
    let now = now_iso();
    sqlx::query(
        "INSERT INTO imports (id, file_path, source_url, status, created_at) VALUES (?, NULL, ?, 'processing', ?)",
    )
    .bind(&import_id)
    .bind(&url)
    .bind(&now)
    .execute(db)
    .await
    .map_err(|e| LinkImportError::new("finalize_failed", format!("imports insert: {e}")))?;

    let do_import: Result<(Paper, Option<String>), LinkImportError> = async {
        let mut meta = if let Some(pm) = preview_metadata {
            pm
        } else {
            fetch_metadata(&parsed, proxy.as_deref()).await?
        };

        // For direct PDFs we have no title yet; derive one from the URL.
        if meta.title.is_empty() {
            meta.title = title_from_url(&url);
        }

        // Always keep the original URL as a fallback.
        if meta.url.is_none() {
            meta.url = Some(url.clone());
        }

        let paper_id = Uuid::new_v4().to_string();
        tracing::Span::current().record("paper_id", &paper_id);

        // Try to download a PDF if available.
        let mut warning: Option<String> = None;
        let pdf_path = if let Some(pdf_url) = resolve_pdf_url(&meta, proxy.as_deref()).await? {
            info!(paper_id = %paper_id, pdf_url = %pdf_url, "downloading PDF");
            match download_pdf(&pdf_url, app_data_dir, &paper_id, proxy.as_deref()).await {
                Ok(path) => Some(path),
                Err(e) => {
                    warning = Some(format!("PDF 下载失败，已仅导入元数据：{}", e.message));
                    warn!(paper_id = %paper_id, error = %e, "PDF download failed, importing metadata only");
                    None
                }
            }
        } else {
            None
        };

        // For direct PDFs (or any downloaded PDF with sparse metadata), try to enrich
        // from the PDF's own /Info dictionary.
        if let Some(ref path) = pdf_path {
            if let Ok(extracted) = crate::pdf::parser::extract_metadata(Path::new(path)) {
                if matches!(parsed, ParsedLink::DirectPdf(_)) || meta.title.is_empty() {
                    if let Some(t) = extracted.title {
                        meta.title = t;
                    }
                }
                if meta.authors.is_empty() {
                    meta.authors = extracted.authors;
                }
                if meta.abstract_text.is_none() {
                    meta.abstract_text = extracted.subject;
                }
                if meta.keywords.is_empty() {
                    meta.keywords = extracted.keywords;
                }
                if meta.page_count.is_none() && extracted.page_count > 0 {
                    meta.page_count = Some(extracted.page_count as i32);
                }
            }
        }

        // If the arXiv API was rate-limited and the PDF didn't carry a title,
        // surface a friendly warning. The paper is still imported; the user can
        // enrich metadata later.
        if matches!(parsed, ParsedLink::Arxiv(_))
            && meta.title.starts_with("arXiv: ")
            && meta.authors.is_empty()
        {
            warning.get_or_insert_with(|| {
                "arXiv 元数据接口暂时不可用，已直接下载 PDF；可在导入后通过「富化元数据」补齐信息。".to_string()
            });
        }

        let (file_path, file_size, file_name) = if let Some(ref path) = pdf_path {
            let size = std::fs::metadata(path).map(|m| m.len() as i64).unwrap_or(0);
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "original.pdf".to_string());
            let blob_rel_path = file_store::copy_file_to_blob(app_data_dir, path)
                .map_err(|e| LinkImportError::new("blob_failed", format!("copy pdf to blob: {e}")))?;
            let _ = std::fs::remove_file(path);
            (Some(blob_rel_path), Some(size), name)
        } else {
            (None, None, "original.pdf".to_string())
        };

        let import_meta = ImportMetadata {
            title: meta.title,
            authors: meta.authors,
            year: meta.year,
            journal: meta.journal,
            doi: meta.doi,
            url: meta.url,
            abstract_text: meta.abstract_text,
            keywords: meta.keywords,
            file_path,
            file_size,
            page_count: meta.page_count,
            language: None,
            isbn: meta.isbn,
        };

        let paper = finalize_paper_import(db, app_data_dir, paper_id, &file_name, import_meta)
            .await
            .map_err(|e| LinkImportError::new("finalize_failed", format!("{e}")))?;

        Ok((paper, warning))
    }
    .await;

    match do_import {
        Ok((paper, warning)) => {
            sqlx::query(
                "UPDATE imports SET status = 'completed', paper_id = ?, completed_at = ? WHERE id = ?",
            )
            .bind(&paper.id)
            .bind(&now)
            .bind(&import_id)
            .execute(db)
            .await
            .ok();
            Ok(PaperImportResult { paper, warning })
        }
        Err(e) => {
            sqlx::query(
                "UPDATE imports SET status = 'failed', error = ?, completed_at = ? WHERE id = ?",
            )
            .bind(e.to_string())
            .bind(&now)
            .bind(&import_id)
            .execute(db)
            .await
            .ok();
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_link_doi_url() {
        assert!(
            matches!(
                parse_link("https://doi.org/10.1145/276675.276685"),
                ParsedLink::Doi(d) if d == "10.1145/276675.276685"
            )
        );
    }

    #[test]
    fn test_parse_link_bare_doi() {
        assert!(
            matches!(
                parse_link("10.1145/276675.276685"),
                ParsedLink::Doi(d) if d == "10.1145/276675.276685"
            )
        );
    }

    #[test]
    fn test_parse_link_arxiv_abs() {
        assert!(
            matches!(
                parse_link("https://arxiv.org/abs/2401.12345"),
                ParsedLink::Arxiv(id) if id == "2401.12345"
            )
        );
    }

    #[test]
    fn test_parse_link_arxiv_pdf() {
        assert!(
            matches!(
                parse_link("https://arxiv.org/pdf/2401.12345.pdf"),
                ParsedLink::Arxiv(id) if id == "2401.12345"
            )
        );
    }

    #[test]
    fn test_parse_link_pubmed() {
        assert!(
            matches!(
                parse_link("https://pubmed.ncbi.nlm.nih.gov/12345678/"),
                ParsedLink::PubMed(id) if id == "12345678"
            )
        );
    }

    #[test]
    fn test_parse_link_direct_pdf() {
        assert!(
            matches!(
                parse_link("https://example.com/paper.pdf?download=1"),
                ParsedLink::DirectPdf(url) if url == "https://example.com/paper.pdf?download=1"
            )
        );
        assert!(
            matches!(
                parse_link("https://example.com/paper.pdf#page=1"),
                ParsedLink::DirectPdf(url) if url == "https://example.com/paper.pdf#page=1"
            )
        );
    }

    #[test]
    fn test_parse_link_arxiv_pdf_treated_as_arxiv() {
        // arxiv.org/pdf/xxx.pdf should fetch metadata from arXiv, not be treated as a raw PDF.
        assert!(
            matches!(
                parse_link("https://arxiv.org/pdf/2401.12345.pdf"),
                ParsedLink::Arxiv(id) if id == "2401.12345"
            )
        );
    }

    #[test]
    fn test_parse_link_unsupported() {
        assert!(matches!(parse_link("https://example.com"), ParsedLink::Unsupported));
    }

    #[test]
    fn test_invalid_bare_doi_rejected() {
        // Looks like an IP address, not a DOI.
        assert!(matches!(parse_link("10.0.0.1/path"), ParsedLink::Unsupported));
        assert!(matches!(
            parse_link("978-3-16-148410-0"),
            ParsedLink::Isbn(_)
        ));
        assert!(matches!(parse_link("0306406152"), ParsedLink::Isbn(_)));
        assert!(matches!(parse_link("openlibrary.org/isbn/9783161484100"), ParsedLink::Isbn(_)));
    }

    #[test]
    fn test_reconstruct_openalex_abstract() {
        let json = serde_json::json!({
            "abstract_inverted_index": {
                "Hello": [0, 2],
                "world": [1]
            }
        });
        let abs = reconstruct_openalex_abstract(&json["abstract_inverted_index"]).unwrap();
        assert_eq!(abs, "Hello world Hello");
    }

    #[test]
    fn test_parse_pubmed_abstract_simple() {
        let xml = r#"<?xml version="1.0"?>
        <PubmedArticleSet>
            <PubmedArticle>
                <MedlineCitation>
                    <Article>
                        <Abstract>
                            <AbstractText>First part.</AbstractText>
                            <AbstractText>Second part.</AbstractText>
                        </Abstract>
                    </Article>
                </MedlineCitation>
            </PubmedArticle>
        </PubmedArticleSet>"#;
        let abs = parse_pubmed_abstract(xml).unwrap();
        assert_eq!(abs, "First part. Second part.");
    }

    #[test]
    fn test_parse_pubmed_abstract_with_attributes() {
        let xml = r#"<?xml version="1.0"?>
        <PubmedArticleSet>
            <PubmedArticle>
                <MedlineCitation>
                    <Article>
                        <Abstract>
                            <AbstractText Label="BACKGROUND" NlmCategory="BACKGROUND">Some background.</AbstractText>
                            <AbstractText Label="CONCLUSIONS" NlmCategory="CONCLUSIONS">A conclusion.</AbstractText>
                        </Abstract>
                    </Article>
                </MedlineCitation>
            </PubmedArticle>
        </PubmedArticleSet>"#;
        let abs = parse_pubmed_abstract(xml).unwrap();
        assert_eq!(abs, "Some background. A conclusion.");
    }

    #[test]
    fn test_strip_xml_tags() {
        assert_eq!(
            strip_xml_tags("<jats:p>Hello <i>world</i>.</jats:p>"),
            "Hello world."
        );
    }
}
