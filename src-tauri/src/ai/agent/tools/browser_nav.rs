use async_trait::async_trait;
use crate::ai::agent::tool_registry::{Tool, ToolParameter};

/// Open a URL in the active tab, waiting for the load event.
pub struct BrowserNavigateTool;

#[async_trait]
impl Tool for BrowserNavigateTool {
    fn name(&self) -> &str {
        "browser_navigate"
    }

    fn description(&self) -> &str {
        "Navigate the active browser tab to a URL and wait for page load. Returns the final URL \
         and page title. Follow with browser_snapshot to see the page's interactive elements."
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![ToolParameter {
            name: "url".into(),
            param_type: "string".into(),
            description: "Target URL; https:// is assumed when the scheme is missing".into(),
            required: true,
        }]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let page = super::browser::require_page().await?;
        let raw = args["url"].as_str().ok_or("url required")?.trim();
        if raw.is_empty() {
            return Err("url required".to_string());
        }
        let url = if raw.contains("://") || raw.starts_with("about:") {
            raw.to_string()
        } else {
            format!("https://{raw}")
        };

        tokio::time::timeout(std::time::Duration::from_secs(30), page.goto(&url))
            .await
            .map_err(|_| format!("navigation timed out after 30s: {url}"))?
            .map_err(|e| format!("navigation failed: {e}"))?;

        let final_url = page.url().await.ok().flatten().unwrap_or(url);
        let title = page.get_title().await.ok().flatten().unwrap_or_default();
        let mut out = format!("Navigated to {final_url}\nTitle: {title}");
        let signals = crate::core::browser::captcha_signals(&page).await;
        if !signals.is_empty() {
            out.push_str(&crate::core::browser::captcha_warning(&signals));
        }
        Ok(out)
    }
}

/// (title, url) of a tab, tolerating a tab that is mid-navigation.
async fn describe(p: &chromiumoxide::Page) -> (String, String) {
    let url = p.url().await.ok().flatten().unwrap_or_default();
    let title = p.get_title().await.ok().flatten().unwrap_or_default();
    (title, url)
}

/// List / switch / close tabs.
pub struct BrowserTabsTool;

#[async_trait]
impl Tool for BrowserTabsTool {
    fn name(&self) -> &str {
        "browser_tabs"
    }

    fn description(&self) -> &str {
        "List browser tabs, switch the tab other browser tools act on, or close a tab. \
         'list' shows numbered tabs (newest last) with the active one marked; 'switch' and 'close' \
         take that 1-based index. Links opened in a new tab appear here — switch before interacting."
    }

    fn param_enums(&self) -> &'static [(&'static str, &'static [&'static str])] {
        &[("action", &["list", "switch", "close"])]
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![
            ToolParameter {
                name: "action".into(),
                param_type: "string".into(),
                description: "list | switch | close".into(),
                required: true,
            },
            ToolParameter {
                name: "index".into(),
                param_type: "integer".into(),
                description: "1-based tab number from the list output (required for switch/close)".into(),
                required: false,
            },
        ]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let mgr = crate::core::browser::global()
            .ok_or("browser manager unavailable (app not fully initialized)")?;
        let action = args["action"].as_str().ok_or("action required")?;
        let tabs = mgr.list_tabs().await?;
        if tabs.is_empty() {
            return Ok("No open tabs.".to_string());
        }

        match action {
            "list" => {
                let active = mgr.active_page().await.ok();
                let active_id = active.map(|p| p.target_id().as_ref().to_string());
                let mut out = String::from("Tabs (newest last):\n");
                for (i, (id, p)) in tabs.iter().enumerate() {
                    let (title, url) = describe(p).await;
                    let mark = if Some(id) == active_id.as_ref() { "→" } else { " " };
                    out.push_str(&format!("{mark} [{}] {title} — {url}\n", i + 1));
                }
                Ok(out.trim_end().to_string())
            }
            "switch" | "close" => {
                let idx = args["index"].as_u64().ok_or("index required")? as usize;
                let Some((id, page)) = tabs.get(idx.wrapping_sub(1)) else {
                    return Err(format!("tab index {idx} out of range (1-{})", tabs.len()));
                };
                if action == "switch" {
                    let _ = page.activate().await;
                    mgr.set_active(id).await;
                    let (title, url) = describe(page).await;
                    Ok(format!("Switched to tab [{idx}]: {title} — {url}"))
                } else {
                    let was_active = mgr.active_page().await.ok()
                        .map(|p| p.target_id().as_ref().to_string() == *id)
                        .unwrap_or(false);
                    let (title, url) = describe(page).await;
                    page.clone().close().await.map_err(|e| format!("close failed: {e}"))?;
                    if was_active {
                        mgr.clear_active().await;
                    }
                    Ok(format!("Closed tab [{idx}]: {title} — {url}"))
                }
            }
            other => Err(format!("unknown action '{other}' — expected list | switch | close")),
        }
    }
}

/// Wait for a CSS selector to appear, or sleep a fixed time.
pub struct BrowserWaitTool;

#[async_trait]
impl Tool for BrowserWaitTool {
    fn name(&self) -> &str {
        "browser_wait"
    }

    fn description(&self) -> &str {
        "Wait until a CSS selector matches an element in the active tab (polls every 250ms), or — \
         without a selector — sleep for timeout_ms. Use after navigation or clicks that load content \
         asynchronously. Read-only."
    }

    fn readonly(&self) -> bool {
        true
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![
            ToolParameter {
                name: "selector".into(),
                param_type: "string".into(),
                description: "CSS selector to wait for; omit to just sleep timeout_ms".into(),
                required: false,
            },
            ToolParameter {
                name: "timeout_ms".into(),
                param_type: "integer".into(),
                description: "Max wait (or sleep duration without selector), default 10000, max 60000".into(),
                required: false,
            },
        ]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let page = super::browser::require_page().await?;
        let timeout_ms = args["timeout_ms"].as_u64().unwrap_or(10_000).min(60_000);
        let selector = args["selector"].as_str().map(str::trim).filter(|s| !s.is_empty());

        let Some(selector) = selector else {
            tokio::time::sleep(std::time::Duration::from_millis(timeout_ms)).await;
            return Ok(format!("Waited {}ms.", timeout_ms));
        };

        // serde_json encoding keeps the selector safely inside the JS string.
        let probe = format!(
            "!!document.querySelector({})",
            serde_json::to_string(selector).map_err(|e| e.to_string())?
        );
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
        loop {
            let found = page
                .evaluate(probe.clone())
                .await
                .ok()
                .and_then(|r| r.into_value::<bool>().ok())
                .unwrap_or(false);
            if found {
                return Ok(format!("Selector now present: {selector}"));
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(format!(
                    "selector '{selector}' did not appear within {}ms — the page may have changed; \
                     try browser_snapshot to inspect it",
                    timeout_ms
                ));
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    }
}
