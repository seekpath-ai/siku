use async_trait::async_trait;
use crate::ai::agent::tool_registry::{Tool, ToolParameter};

/// Block until the human solves an on-page verification challenge.
///
/// The browser is a headed window on the user's desktop, so the solving
/// happens there — not in chat. This tool notifies the desktop, brings the
/// tab to front, and polls until the challenge signals disappear.
pub struct BrowserCaptchaWaitTool {
    app_handle: Option<tauri::AppHandle>,
}

impl BrowserCaptchaWaitTool {
    pub fn new(app_handle: Option<tauri::AppHandle>) -> Self {
        Self { app_handle }
    }
}

#[async_trait]
impl Tool for BrowserCaptchaWaitTool {
    fn name(&self) -> &str {
        "browser_captcha_wait"
    }

    fn description(&self) -> &str {
        "Wait for a human to solve a CAPTCHA / security verification shown in the browser. \
         Detects reCAPTCHA, hCaptcha, Cloudflare Turnstile/challenge, GeeTest, Arkose. \
         Sends a desktop notification, focuses the browser tab, then polls until the challenge \
         clears. Never try to solve or bypass a challenge yourself — always use this. \
         On timeout, call again to keep waiting. Read-only (only observes the page)."
    }

    fn readonly(&self) -> bool {
        true
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![ToolParameter {
            name: "timeout_ms".into(),
            param_type: "integer".into(),
            description: "Max wait before reporting back, default 300000 (5 min), max 600000".into(),
            required: false,
        }]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let page = super::browser::require_page().await?;
        let timeout_ms = args["timeout_ms"].as_u64().unwrap_or(300_000).min(600_000);

        let signals = crate::core::browser::captcha_signals(&page).await;
        if signals.is_empty() {
            return Ok("No human-verification challenge detected on the page — continue normally.".to_string());
        }

        // Alert whoever is near the machine: desktop notification + the tab
        // itself in front. Both are best-effort (headless notification
        // daemons, locked focus-stealing prevention, …).
        if let Some(app) = &self.app_handle {
            use tauri_plugin_notification::NotificationExt;
            let _ = app
                .notification()
                .builder()
                .title("思库：需要人工验证")
                .body("浏览器自动化遇到人机验证，请在浏览器窗口中完成，任务会自动继续。")
                .show();
        }
        let _ = page.bring_to_front().await;

        let started = std::time::Instant::now();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
            let signals = crate::core::browser::captcha_signals(&page).await;
            if signals.is_empty() {
                return Ok(format!(
                    "✓ Verification cleared after {}s. Call browser_snapshot to map the page and continue.",
                    started.elapsed().as_secs()
                ));
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(format!(
                    "verification still present after {}s ({}). Call browser_captcha_wait again to keep \
                     waiting, or abort the browser task if nobody is available to solve it.",
                    timeout_ms / 1000,
                    signals.join("; ")
                ));
            }
        }
    }
}
