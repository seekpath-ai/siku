use async_trait::async_trait;
use base64::Engine as _;
use chromiumoxide::cdp::browser_protocol::browser::{
    SetDownloadBehaviorBehavior, SetDownloadBehaviorParams,
};
use chromiumoxide::cdp::browser_protocol::page::PrintToPdfParams;
use chromiumoxide::page::ScreenshotParams;
use crate::ai::agent::tool_registry::{Tool, ToolParameter};

/// The session write directory every browser artifact lands in.
fn write_dir(args: &serde_json::Value, sub: &str) -> Result<std::path::PathBuf, String> {
    let wd = args["_working_dir"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or("no session write directory available")?;
    let dir = std::path::Path::new(wd).join(sub);
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    Ok(dir)
}

/// A model-supplied filename must be a plain file name, never a path.
fn sanitize_filename(name: &str) -> Result<String, String> {
    let name = name.trim();
    let ok = !name.is_empty()
        && name.len() <= 120
        && !name.contains(['/', '\\'])
        && !name.starts_with('.')
        && name.chars().all(|c| c != '\0' && c != ':' && c != '*' && c != '?' && c != '"' && c != '<' && c != '>' && c != '|');
    if ok {
        Ok(name.to_string())
    } else {
        Err(format!("invalid filename '{name}' — plain file name only, no path separators"))
    }
}

/// Screenshot the active tab to a PNG in the session write directory.
pub struct BrowserScreenshotTool;

#[async_trait]
impl Tool for BrowserScreenshotTool {
    fn name(&self) -> &str {
        "browser_screenshot"
    }

    fn description(&self) -> &str {
        "Screenshot the active browser tab and save the PNG into the session write directory \
         (screenshots/). Pass the returned path to read_media_file when visual inspection is needed \
         (e.g. charts, CAPTCHA checks). browser_snapshot is the cheaper way to read a page — use \
         this only when layout or visuals matter. Read-only."
    }

    fn readonly(&self) -> bool {
        true
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![
            ToolParameter {
                name: "full_page".into(),
                param_type: "boolean".into(),
                description: "Capture the full scrollable page instead of just the viewport (default false)".into(),
                required: false,
            },
            ToolParameter {
                name: "filename".into(),
                param_type: "string".into(),
                description: "Output file name (default: auto page-<timestamp>.png)".into(),
                required: false,
            },
        ]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let page = super::browser::require_page().await?;
        let full_page = args["full_page"].as_bool().unwrap_or(false);

        let bytes = page
            .screenshot(
                ScreenshotParams::builder()
                    .full_page(full_page)
                    .capture_beyond_viewport(full_page)
                    .build(),
            )
            .await
            .map_err(|e| format!("screenshot failed: {e}"))?;

        let filename = match args["filename"].as_str() {
            Some(f) => {
                let f = sanitize_filename(f)?;
                if f.ends_with(".png") { f } else { format!("{f}.png") }
            }
            None => format!("page-{}.png", chrono::Local::now().format("%Y%m%d-%H%M%S")),
        };
        let path = write_dir(&args, "screenshots")?.join(&filename);
        std::fs::write(&path, &bytes).map_err(|e| format!("write failed: {e}"))?;

        let scope = if full_page { "full page" } else { "viewport" };
        Ok(format!(
            "Screenshot saved ({scope}, {:.1} KB): {}\nPass this path to read_media_file to inspect it.",
            bytes.len() as f64 / 1024.0,
            path.display()
        ))
    }
}

/// Save the page as PDF, or capture a file download triggered by a click.
pub struct BrowserDownloadPdfTool;

#[async_trait]
impl Tool for BrowserDownloadPdfTool {
    fn name(&self) -> &str {
        "browser_download_pdf"
    }

    fn description(&self) -> &str {
        "Get a PDF out of the browser. action='fetch' saves the document currently open in the \
         tab (typically a PDF shown in Chrome's built-in viewer) by capturing its original bytes \
         through the browser's own network stack — use it when clicking a PDF link opened the \
         viewer instead of downloading, or when direct downloads fail behind proxies/certificate \
         inspection. action='download' arms the download directory and clicks the given element \
         ref (e.g. a 'Download PDF' link), waits for the file to finish, and returns its path. \
         action='print' renders the active tab to PDF — for HTML article pages only; never use it \
         on Chrome's built-in PDF viewer page (it captures only the viewer UI). Files land in the \
         session write directory (downloads/). A saved PDF can be added to the library with \
         paper_import."
    }

    fn param_enums(&self) -> &'static [(&'static str, &'static [&'static str])] {
        &[("action", &["print", "download", "fetch"])]
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![
            ToolParameter {
                name: "action".into(),
                param_type: "string".into(),
                description: "fetch (save the document open in the tab via the browser's network stack) | download (click ref and capture the file) | print (render an HTML page to PDF)".into(),
                required: true,
            },
            ToolParameter {
                name: "ref".into(),
                param_type: "string".into(),
                description: "Element ref to click (required for action=download)".into(),
                required: false,
            },
            ToolParameter {
                name: "url".into(),
                param_type: "string".into(),
                description: "Document URL for action=fetch; default: the active tab's current URL. Pass it when the PDF is not open in the tab yet.".into(),
                required: false,
            },
            ToolParameter {
                name: "filename".into(),
                param_type: "string".into(),
                description: "Output file name; default: page title (print) or server-provided name (download)".into(),
                required: false,
            },
            ToolParameter {
                name: "timeout_ms".into(),
                param_type: "integer".into(),
                description: "Max wait for a download to finish, default 60000, max 300000".into(),
                required: false,
            },
        ]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        match args["action"].as_str() {
            Some("print") => print_pdf(&args).await,
            Some("download") => capture_download(&args).await,
            Some("fetch") => fetch_document(&args).await,
            Some(other) => Err(format!("unknown action '{other}' — expected print | download | fetch")),
            None => Err("action required".to_string()),
        }
    }
}

async fn print_pdf(args: &serde_json::Value) -> Result<String, String> {
    let page = super::browser::require_page().await?;
    let bytes = page
        .pdf(PrintToPdfParams::builder().print_background(true).build())
        .await
        .map_err(|e| format!("print to PDF failed: {e}"))?;

    let filename = match args["filename"].as_str() {
        Some(f) => {
            let f = sanitize_filename(f)?;
            if f.ends_with(".pdf") { f } else { format!("{f}.pdf") }
        }
        None => {
            let title = page.get_title().await.ok().flatten().unwrap_or_default();
            let stem: String = title
                .chars()
                .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
                .take(60)
                .collect();
            let stem = stem.trim_matches('_');
            format!(
                "{}-{}.pdf",
                if stem.is_empty() { "page" } else { stem },
                chrono::Local::now().format("%H%M%S")
            )
        }
    };
    let path = write_dir(args, "downloads")?.join(&filename);
    std::fs::write(&path, &bytes).map_err(|e| format!("write failed: {e}"))?;
    Ok(format!(
        "Page saved as PDF ({:.1} KB): {}\nTo add it to the library, call paper_import with this path.",
        bytes.len() as f64 / 1024.0,
        path.display()
    ))
}

/// Does `url` point at a PDF document (by path extension)?
fn url_looks_like_pdf(url: &str) -> bool {
    url.split(['?', '#'])
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase()
        .ends_with(".pdf")
}

/// In-page re-request of the current document, returned as base64. Runs in
/// the top-level document — for a PDF opened in Chrome's built-in viewer
/// that document's origin is the PDF URL itself, so the fetch is same-origin
/// and rides the browser's own network stack (proxy, cookies, corporate CA).
/// Chunked fromCharCode avoids argument-count limits on larger files.
const FETCH_DOCUMENT_JS: &str = r#"(async () => {
  const r = await fetch(location.href, { credentials: 'include' });
  if (!r.ok) return { error: 'HTTP ' + r.status, url: location.href };
  const bytes = new Uint8Array(await r.arrayBuffer());
  if (bytes.length > 45 * 1024 * 1024) {
    return { error: 'document too large for in-browser capture (>45 MB)', url: location.href };
  }
  let binary = '';
  const CHUNK = 32768;
  for (let i = 0; i < bytes.length; i += CHUNK) {
    binary += String.fromCharCode.apply(null, bytes.subarray(i, i + CHUNK));
  }
  return { b64: btoa(binary), url: location.href };
})()"#;

/// action=fetch: capture the original bytes of the document open in the
/// active tab (typically a PDF in Chrome's built-in viewer). The browser
/// itself re-requests the document from within the page, so proxies, cookies
/// and corporate CA setups behave exactly as they did when the page was
/// first opened.
async fn fetch_document(args: &serde_json::Value) -> Result<String, String> {
    let page = super::browser::require_page().await?;
    let timeout_ms = args["timeout_ms"].as_u64().unwrap_or(60_000).min(300_000);
    let explicit_url = args["url"].as_str().map(str::trim).filter(|u| !u.is_empty());

    if let Some(url) = explicit_url {
        tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), page.goto(url))
            .await
            .map_err(|_| format!("navigate to {url} timed out after {timeout_ms}ms"))?
            .map_err(|e| format!("navigate to {url} failed: {e}"))?;
    }

    // The top-level document of a PDF navigation reports contentType
    // application/pdf — authoritative even for extension-less URLs like
    // arxiv.org/pdf/<id>. The .pdf suffix is a fallback signal for setups
    // where the built-in viewer is disabled.
    let content_type = page
        .evaluate("document.contentType")
        .await
        .ok()
        .and_then(|r| r.into_value::<String>().ok())
        .unwrap_or_default();
    let current_url = page.url().await.ok().flatten().unwrap_or_default();
    if !content_type.eq_ignore_ascii_case("application/pdf") && !url_looks_like_pdf(&current_url)
    {
        return Err(format!(
            "the active tab is not a PDF document (content-type: '{}', url: {}). action=fetch \
             is for documents already open in the browser — pass url=... to navigate first, or \
             use action=print for HTML pages.",
            if content_type.is_empty() { "unknown" } else { content_type.as_str() },
            current_url
        ));
    }

    let result = page
        .evaluate(FETCH_DOCUMENT_JS)
        .await
        .map_err(|e| format!("in-page fetch failed: {e}"))?
        .into_value::<serde_json::Value>()
        .map_err(|e| format!("in-page fetch returned an unreadable result: {e}"))?;
    if let Some(err) = result["error"].as_str() {
        return Err(format!(
            "the browser could not re-request {}: {err}",
            result["url"].as_str().unwrap_or(&current_url)
        ));
    }
    let b64 = result["b64"]
        .as_str()
        .ok_or("in-page fetch returned no data")?;
    let final_url = result["url"].as_str().unwrap_or(&current_url).to_string();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| format!("cannot decode the document bytes: {e}"))?;

    let filename = match args["filename"].as_str() {
        Some(f) => {
            let f = sanitize_filename(f)?;
            if f.ends_with(".pdf") { f } else { format!("{f}.pdf") }
        }
        None => {
            let stem: String = final_url
                .split(['?', '#'])
                .next()
                .unwrap_or(&final_url)
                .rsplit('/')
                .next()
                .unwrap_or("document")
                .trim_end_matches(".pdf")
                .chars()
                .map(|c| if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' })
                .take(60)
                .collect();
            let stem = stem.trim_matches('_');
            format!("{}.pdf", if stem.is_empty() { "document" } else { stem })
        }
    };
    let path = write_dir(args, "downloads")?.join(&filename);
    std::fs::write(&path, &bytes).map_err(|e| format!("write failed: {e}"))?;
    let mut out = format!(
        "Document saved ({:.1} KB): {}\nSource: {final_url}",
        bytes.len() as f64 / 1024.0,
        path.display()
    );
    if bytes.starts_with(b"%PDF") {
        out.push_str("\nTo add it to the library, call paper_import with this path.");
    } else {
        out.push_str(
            "\n⚠ the captured document has no %PDF header — it may be an error page; verify before importing.",
        );
    }
    Ok(out)
}

/// Completed files in `dir` (excluding in-progress `.crdownload`).
fn completed_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file() && p.extension().is_none_or(|x| x != "crdownload"))
                .collect()
        })
        .unwrap_or_default()
}

async fn capture_download(args: &serde_json::Value) -> Result<String, String> {
    let refv = super::browser::validate_ref(args["ref"].as_str().ok_or("ref required for action=download")?)?;
    let timeout_ms = args["timeout_ms"].as_u64().unwrap_or(60_000).min(300_000);
    let dir = write_dir(args, "downloads")?;

    // Route the browser's downloads at our directory (browser-level CDP).
    let browser = crate::core::browser::global()
        .ok_or("browser manager unavailable")?
        .live_browser()
        .await
        .ok_or("No browser connection — call browser_connect first")?;
    let params = SetDownloadBehaviorParams::builder()
        .behavior(SetDownloadBehaviorBehavior::Allow)
        .download_path(dir.to_string_lossy().to_string())
        .build()
        .map_err(|e| format!("invalid download config: {e}"))?;
    browser
        .execute(params)
        .await
        .map_err(|e| format!("cannot set download directory: {e}"))?;

    let before: std::collections::HashSet<std::path::PathBuf> =
        completed_files(&dir).into_iter().collect();

    let page = super::browser::require_page().await?;
    let el = super::browser_interact::find_by_ref(&page, &refv).await?;
    let url_before = page.url().await.ok().flatten().unwrap_or_default();
    el.click().await.map_err(|e| format!("click on [{refv}] failed: {e}"))?;

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let done = completed_files(&dir);
        let in_progress = std::fs::read_dir(&dir)
            .map(|rd| rd.flatten().any(|e| e.path().extension().is_some_and(|x| x == "crdownload")))
            .unwrap_or(false);
        if let Some(new_file) = done.into_iter().find(|p| !before.contains(p)) {
            if !in_progress {
                let final_path = match args["filename"].as_str() {
                    Some(f) => {
                        let f = sanitize_filename(f)?;
                        let target = dir.join(&f);
                        std::fs::rename(&new_file, &target)
                            .map_err(|e| format!("rename failed: {e}"))?;
                        target
                    }
                    None => new_file,
                };
                let size = std::fs::metadata(&final_path).map(|m| m.len()).unwrap_or(0);
                let mut out = format!(
                    "Download finished ({:.1} KB): {}",
                    size as f64 / 1024.0,
                    final_path.display()
                );
                if final_path.extension().is_some_and(|x| x == "pdf") {
                    out.push_str("\nTo add it to the library, call paper_import with this path.");
                }
                return Ok(out);
            }
        }
        // Fast-fail: the click navigated the tab to a PDF, i.e. Chrome opened
        // its built-in viewer and no download will ever fire. Point at the
        // action that handles exactly this case. document.contentType is the
        // authoritative signal — arXiv-style URLs carry no .pdf suffix.
        if !in_progress {
            let ct = page
                .evaluate("document.contentType")
                .await
                .ok()
                .and_then(|r| r.into_value::<String>().ok())
                .unwrap_or_default();
            let url_now = page.url().await.ok().flatten().unwrap_or_default();
            if ct.eq_ignore_ascii_case("application/pdf")
                || (url_now != url_before && url_looks_like_pdf(&url_now))
            {
                return Err(format!(
                    "clicking [{refv}] opened the PDF in Chrome's built-in viewer ({url_now}) — \
                     no download was triggered. Use browser_download_pdf with action='fetch' to \
                     save the opened document directly."
                ));
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "no completed download within {}ms after clicking [{refv}] — the click may not \
                 have triggered a download. If it opened the PDF in a browser tab instead, use \
                 browser_download_pdf with action='fetch' on that tab.",
                timeout_ms
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_rules() {
        assert_eq!(sanitize_filename("a b.pdf").unwrap(), "a b.pdf");
        assert!(sanitize_filename("../x").is_err());
        assert!(sanitize_filename("a/b").is_err());
        assert!(sanitize_filename("a\\b").is_err());
        assert!(sanitize_filename(".hidden").is_err());
        assert!(sanitize_filename("").is_err());
    }

    #[test]
    fn pdf_url_detection() {
        assert!(url_looks_like_pdf("https://arxiv.org/pdf/2412.20927v1.pdf"));
        assert!(url_looks_like_pdf("https://x.test/A.PDF?dl=1#view"));
        assert!(!url_looks_like_pdf("https://arxiv.org/abs/2412.20927"));
        assert!(!url_looks_like_pdf("https://x.test/pdf-viewer"));
        assert!(!url_looks_like_pdf(""));
    }
}
