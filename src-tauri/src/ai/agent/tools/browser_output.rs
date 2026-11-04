use async_trait::async_trait;
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
        "Get a PDF out of the browser. action='print' renders the active tab to PDF (good for \
         article/preprint pages). action='download' arms the download directory and clicks the \
         given element ref (e.g. a 'Download PDF' link), waits for the file to finish, and returns \
         its path. Files land in the session write directory (downloads/). A downloaded PDF can be \
         added to the library with paper_import."
    }

    fn param_enums(&self) -> &'static [(&'static str, &'static [&'static str])] {
        &[("action", &["print", "download"])]
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![
            ToolParameter {
                name: "action".into(),
                param_type: "string".into(),
                description: "print (render page to PDF) | download (click ref and capture the file)".into(),
                required: true,
            },
            ToolParameter {
                name: "ref".into(),
                param_type: "string".into(),
                description: "Element ref to click (required for action=download)".into(),
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
            Some(other) => Err(format!("unknown action '{other}' — expected print | download")),
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
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "no completed download within {}ms after clicking [{refv}] — the click may not \
                 have triggered a download; check with browser_snapshot",
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
}
