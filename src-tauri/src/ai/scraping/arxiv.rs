use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::instrument;

/// A paper result from arXiv search
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArxivPaper {
    pub arxiv_id: String,
    pub title: String,
    pub authors: Vec<String>,
    pub abstract_text: String,
    pub published: String,
    pub updated: String,
    pub pdf_url: String,
    pub categories: Vec<String>,
    pub doi: Option<String>,
    pub comment: Option<String>,
}

/// Fetch a single arXiv paper by its ID (e.g. "2401.12345" or "2401.12345v1").
#[instrument]
pub async fn fetch_by_id(id: &str, proxy: Option<&str>) -> Result<Option<ArxivPaper>, String> {
    let mut builder = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent("Siku/0.1");

    if let Some(p) = proxy {
        if !p.is_empty() {
            let proxy = reqwest::Proxy::all(p).map_err(|e| format!("proxy error: {e}"))?;
            builder = builder.proxy(proxy);
        }
    }

    let client = builder.build().map_err(|e| format!("client: {e}"))?;

    // Use id_list for exact ID lookup, and HTTPS to avoid redirect/network issues.
    let url = format!(
        "https://export.arxiv.org/api/query?id_list={}&max_results=1",
        id
    );

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("arXiv API error: {e}"))?;
    let text = resp.text().await.map_err(|e| format!("read: {e}"))?;

    let mut papers = parse_arxiv_response(&text)?;
    Ok(papers.pop())
}

/// Search arXiv by keywords
#[instrument]
pub async fn search(
    query: &str,
    max_results: u32,
    proxy: Option<&str>,
) -> Result<Vec<ArxivPaper>, String> {
    let mut builder = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent("Siku/0.1");

    if let Some(p) = proxy {
        if !p.is_empty() {
            let proxy = reqwest::Proxy::all(p).map_err(|e| format!("proxy error: {e}"))?;
            builder = builder.proxy(proxy);
        }
    }

    let client = builder.build().map_err(|e| format!("client: {e}"))?;

    let encoded_query = query.replace(' ', "+").replace(':', "%3A").replace('/', "%2F");
    let url = format!(
        "https://export.arxiv.org/api/query?search_query=all:{}&start=0&max_results={}&sortBy=submittedDate&sortOrder=descending",
        encoded_query,
        max_results.min(30)
    );

    let resp = client.get(&url).send().await.map_err(|e| format!("arXiv API error: {e}"))?;
    let text = resp.text().await.map_err(|e| format!("read: {e}"))?;

    parse_arxiv_response(&text)
}

/// Parse arXiv Atom XML response
fn parse_arxiv_response(xml: &str) -> Result<Vec<ArxivPaper>, String> {
    let mut papers = Vec::new();

    // Simple XML parsing for arXiv Atom feed
    let entries: Vec<&str> = xml.split("<entry>").skip(1).collect();

    for entry in entries {
        let entry = entry.split("</entry>").next().unwrap_or("");

        let raw_id = extract_tag(entry, "id");
        let arxiv_id = raw_id
            .strip_prefix("http://arxiv.org/abs/")
            .or_else(|| raw_id.strip_prefix("https://arxiv.org/abs/"))
            .unwrap_or(&raw_id)
            .to_string();

        let title = extract_tag(entry, "title").replace('\n', " ").trim().to_string();
        let abstract_text = extract_tag(entry, "summary").replace('\n', " ").trim().to_string();

        let authors: Vec<String> = entry
            .split("<author>")
            .skip(1)
            .filter_map(|a| {
                let name = extract_tag(a, "name");
                if name.is_empty() { None } else { Some(name) }
            })
            .collect();

        let published = extract_tag(entry, "published");
        let updated = extract_tag(entry, "updated");

        let categories: Vec<String> = entry
            .lines()
            .filter(|l| l.contains("category term="))
            .filter_map(|l| {
                let start = l.find('"')?;
                let end = l[start + 1..].find('"')?;
                Some(l[start + 1..start + 1 + end].to_string())
            })
            .collect();

        let doi = entry
            .lines()
            .find(|l| l.contains("doi.org"))
            .and_then(|l| {
                let start = l.find("doi.org/")?;
                let end = l[start..].find('<').unwrap_or(l[start..].len());
                Some(l[start..start + end].trim_end_matches("</").to_string())
            });

        let comment = extract_tag(entry, "arxiv:comment");

        let pdf_url = format!("https://arxiv.org/pdf/{}", arxiv_id);

        if !arxiv_id.is_empty() {
            papers.push(ArxivPaper {
                arxiv_id,
                title,
                authors,
                abstract_text,
                published,
                updated,
                pdf_url,
                categories,
                doi,
                comment: if comment.is_empty() { None } else { Some(comment) },
            });
        }
    }

    Ok(papers)
}

fn extract_tag(xml: &str, tag: &str) -> String {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);

    if let Some(start) = xml.find(&open) {
        let start_idx = start + open.len();
        if let Some(end) = xml[start_idx..].find(&close) {
            return xml[start_idx..start_idx + end].to_string();
        }
    }

    // Try self-closing or attribute variants
    if let Some(start) = xml.find(&format!("<{} ", tag)) {
        let rest = &xml[start..];
        if let Some(end) = rest.find('>') {
            let attrs = &rest[..end];
            // Try to extract value from attribute like term="..."
            for attr in ["term", "title", "name"] {
                let pattern = format!("{}=\"", attr);
                if let Some(v_start) = attrs.find(&pattern) {
                    let v_begin = v_start + pattern.len();
                    if let Some(v_end) = attrs[v_begin..].find('"') {
                        return attrs[v_begin..v_begin + v_end].to_string();
                    }
                }
            }
        }
    }

    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_tag() {
        let xml = "<title>Hello World</title>";
        assert_eq!(extract_tag(xml, "title"), "Hello World");
    }

    #[test]
    fn test_parse_empty() {
        let result = parse_arxiv_response("<feed></feed>");
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }
}
