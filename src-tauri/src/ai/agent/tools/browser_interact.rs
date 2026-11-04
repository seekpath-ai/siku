use async_trait::async_trait;
use crate::ai::agent::tool_registry::{Tool, ToolParameter};

/// Find an element by its snapshot ref, or explain the refs went stale.
pub(crate) async fn find_by_ref(page: &chromiumoxide::Page, refv: &str) -> Result<chromiumoxide::Element, String> {
    page.find_element(super::browser::ref_selector(refv))
        .await
        .map_err(|_| {
            format!(
                "element [{refv}] not found — the snapshot is stale or the page changed; call browser_snapshot again"
            )
        })
}

/// Click a snapshot-ref'd element (real mouse events at the element's center).
pub struct BrowserClickTool;

#[async_trait]
impl Tool for BrowserClickTool {
    fn name(&self) -> &str {
        "browser_click"
    }

    fn description(&self) -> &str {
        "Click an element by its ref from the latest browser_snapshot (e.g. \"3-12\"). Dispatches \
         real mouse input, so JS handlers fire. Reports URL changes and newly opened tabs afterwards; \
         re-snapshot before the next interaction if the page changed."
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![ToolParameter {
            name: "ref".into(),
            param_type: "string".into(),
            description: "Element ref from browser_snapshot, format \"<gen>-<n>\"".into(),
            required: true,
        }]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let refv = super::browser::validate_ref(args["ref"].as_str().ok_or("ref required")?)?;
        let page = super::browser::require_page().await?;

        let url_before = page.url().await.ok().flatten();
        let tabs_before = match crate::core::browser::global() {
            Some(m) => m.list_tabs().await.ok().map(|t| t.len()),
            None => None,
        };

        let el = find_by_ref(&page, &refv).await?;
        el.click().await.map_err(|e| format!("click on [{refv}] failed: {e}"))?;

        // Give sync handlers / same-page navigation a moment to settle.
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;

        let mut out = format!("Clicked [{refv}].");
        let url_after = page.url().await.ok().flatten();
        if url_after != url_before {
            if let Some(u) = &url_after {
                out.push_str(&format!("\nURL is now: {u}"));
            }
        }
        if let (Some(mgr), Some(before)) = (crate::core::browser::global(), tabs_before) {
            if let Ok(tabs) = mgr.list_tabs().await {
                if tabs.len() > before {
                    out.push_str(&format!(
                        "\nA new tab opened ({} tabs total) — use browser_tabs switch to act on it.",
                        tabs.len()
                    ));
                }
            }
        }
        let signals = crate::core::browser::captcha_signals(&page).await;
        if !signals.is_empty() {
            out.push_str(&crate::core::browser::captcha_warning(&signals));
        }
        out.push_str("\nRe-run browser_snapshot before interacting further if the page changed.");
        Ok(out)
    }
}

/// Type text into an input/textarea/contenteditable by ref.
pub struct BrowserTypeTool;

#[async_trait]
impl Tool for BrowserTypeTool {
    fn name(&self) -> &str {
        "browser_type"
    }

    fn description(&self) -> &str {
        "Type text into an input field (or textarea / contenteditable) by its snapshot ref. \
         Sends real keystrokes, so autocomplete and validation handlers fire. \
         clear=true empties the field first; press_enter=true submits with Enter afterwards."
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![
            ToolParameter {
                name: "ref".into(),
                param_type: "string".into(),
                description: "Element ref from browser_snapshot".into(),
                required: true,
            },
            ToolParameter {
                name: "text".into(),
                param_type: "string".into(),
                description: "Text to type".into(),
                required: true,
            },
            ToolParameter {
                name: "clear".into(),
                param_type: "boolean".into(),
                description: "Empty the field before typing (default false)".into(),
                required: false,
            },
            ToolParameter {
                name: "press_enter".into(),
                param_type: "boolean".into(),
                description: "Press Enter after typing (default false)".into(),
                required: false,
            },
        ]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let refv = super::browser::validate_ref(args["ref"].as_str().ok_or("ref required")?)?;
        let text = args["text"].as_str().ok_or("text required")?;
        let clear = args["clear"].as_bool().unwrap_or(false);
        let press_enter = args["press_enter"].as_bool().unwrap_or(false);
        let page = super::browser::require_page().await?;

        let el = find_by_ref(&page, &refv).await?;
        el.focus().await.map_err(|e| format!("focus on [{refv}] failed: {e}"))?;

        if clear {
            // Clearing must fire input/change or frameworks (React & co.) keep
            // their internal state and resurrect the old value.
            let js = format!(
                "(() => {{ const el = document.querySelector({}); if (!el) return false; \
                 el.value = ''; el.dispatchEvent(new Event('input', {{bubbles: true}})); \
                 el.dispatchEvent(new Event('change', {{bubbles: true}})); return true; }})()",
                serde_json::to_string(&super::browser::ref_selector(&refv)).map_err(|e| e.to_string())?
            );
            page.evaluate(js).await.map_err(|e| format!("clear on [{refv}] failed: {e}"))?;
        }

        el.type_str(text).await.map_err(|e| format!("typing into [{refv}] failed: {e}"))?;
        let mut out = format!("Typed {} chars into [{refv}].", text.chars().count());

        if press_enter {
            el.press_key("Enter").await.map_err(|e| format!("Enter on [{refv}] failed: {e}"))?;
            out.push_str(" Pressed Enter.");
            tokio::time::sleep(std::time::Duration::from_millis(600)).await;
            if let Ok(Some(u)) = page.url().await {
                out.push_str(&format!("\nURL is now: {u}"));
            }
        }
        Ok(out)
    }
}

/// Scroll the page, or bring a ref'd element into view.
pub struct BrowserScrollTool;

#[async_trait]
impl Tool for BrowserScrollTool {
    fn name(&self) -> &str {
        "browser_scroll"
    }

    fn description(&self) -> &str {
        "Scroll the active tab up/down/to top/to bottom, or scroll a snapshot-ref'd element into \
         view (pass ref instead of direction). Reports the new scroll position and whether more \
         content lies below. Read-only (no page state is modified beyond scroll)."
    }

    fn readonly(&self) -> bool {
        true
    }

    fn param_enums(&self) -> &'static [(&'static str, &'static [&'static str])] {
        &[("direction", &["up", "down", "top", "bottom"])]
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![
            ToolParameter {
                name: "direction".into(),
                param_type: "string".into(),
                description: "up | down | top | bottom (default down); ignored when ref is given".into(),
                required: false,
            },
            ToolParameter {
                name: "ref".into(),
                param_type: "string".into(),
                description: "Element ref to scroll into view (takes precedence over direction)".into(),
                required: false,
            },
            ToolParameter {
                name: "amount".into(),
                param_type: "integer".into(),
                description: "Pixels for up/down (default 700)".into(),
                required: false,
            },
        ]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let page = super::browser::require_page().await?;

        if let Some(refv) = args["ref"].as_str() {
            let refv = super::browser::validate_ref(refv)?;
            let el = find_by_ref(&page, &refv).await?;
            el.scroll_into_view()
                .await
                .map_err(|e| format!("scroll [{refv}] into view failed: {e}"))?;
            return Ok(format!("Scrolled [{refv}] into view. Re-run browser_snapshot to map what is now visible."));
        }

        let direction = args["direction"].as_str().unwrap_or("down");
        let amount = args["amount"].as_u64().unwrap_or(700).min(10_000);
        let action = match direction {
            "up" => format!("scrollBy(0, -{amount});"),
            "down" => format!("scrollBy(0, {amount});"),
            "top" => "scrollTo(0, 0);".to_string(),
            "bottom" => "scrollTo(0, document.body.scrollHeight);".to_string(),
            other => return Err(format!("unknown direction '{other}' — expected up | down | top | bottom")),
        };
        let js = format!(
            "(() => {{ {action} const max = Math.max(document.body.scrollHeight, \
             document.documentElement.scrollHeight) - innerHeight; \
             return {{ y: Math.round(scrollY), max: Math.round(Math.max(max, 0)) }}; }})()"
        );
        let pos = page
            .evaluate(js)
            .await
            .map_err(|e| format!("scroll failed: {e}"))?
            .into_value::<serde_json::Value>()
            .map_err(|e| format!("scroll result unreadable: {e}"))?;
        let y = pos["y"].as_u64().unwrap_or(0);
        let max = pos["max"].as_u64().unwrap_or(0);
        let hint = if y >= max { " (reached bottom)" } else { " (more below)" };
        Ok(format!("Scrolled {direction} — now at {y}/{max}px{hint}"))
    }
}
