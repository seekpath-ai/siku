use async_trait::async_trait;
use crate::ai::agent::tool_registry::{Tool, ToolParameter};
use crate::core::browser::{self, ConnectMode};

/// Launch or attach to a real Chrome/Edge instance (phase 1 of the browser
/// automation toolset). Follow-up tools (navigate, snapshot, click…) reuse
/// this connection, so this is the entry point of any browser task.
pub struct BrowserConnectTool;

#[async_trait]
impl Tool for BrowserConnectTool {
    fn name(&self) -> &str {
        "browser_connect"
    }

    fn description(&self) -> &str {
        "Launch or attach to a real Chrome/Edge browser for web automation. \
         Modes: 'persistent' (default) uses a dedicated long-lived profile so site logins survive restarts — \
         use it for sites needing login; 'temporary' uses a clean throwaway profile — a stronger web_fetch \
         for JS-rendered pages; 'attach' connects to a browser the user already started with \
         --remote-debugging-port. Idempotent: a healthy existing connection is reused. \
         Requires an unlocked, logged-in desktop (headed window)."
    }

    fn param_enums(&self) -> &'static [(&'static str, &'static [&'static str])] {
        &[("mode", &["persistent", "temporary", "attach"])]
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![
            ToolParameter {
                name: "mode".into(),
                param_type: "string".into(),
                description: "persistent | temporary | attach. Default: the configured default mode (persistent unless changed in Settings)".into(),
                required: false,
            },
            ToolParameter {
                name: "port".into(),
                param_type: "integer".into(),
                description: "Debug port for mode=attach (default 9222)".into(),
                required: false,
            },
        ]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let mgr = browser::global().ok_or("browser manager unavailable (app not fully initialized)")?;

        let mode = match args["mode"].as_str() {
            None => {
                let configured = crate::core::settings_service::cached_settings().browser_default_mode;
                match configured.as_deref() {
                    Some("temporary") => ConnectMode::Temporary,
                    Some("attach") => ConnectMode::Attach {
                        port: args["port"].as_u64().unwrap_or(9222) as u16,
                    },
                    _ => ConnectMode::Persistent,
                }
            }
            Some("persistent") => ConnectMode::Persistent,
            Some("temporary") => ConnectMode::Temporary,
            Some("attach") => ConnectMode::Attach {
                port: args["port"].as_u64().unwrap_or(9222) as u16,
            },
            Some(other) => {
                return Err(format!("unknown mode '{other}' — expected persistent | temporary | attach"))
            }
        };

        let outcome = mgr.connect(mode).await?;

        let mut details = vec![format!("mode: {}", outcome.mode.label())];
        if let Some(exe) = &outcome.executable {
            details.push(format!("executable: {}", exe.display()));
        }
        if let Some(dir) = &outcome.profile_dir {
            details.push(format!("profile: {}", dir.display()));
        }
        details.push(format!("{} tab(s) open", outcome.tabs));

        if outcome.reused {
            Ok(format!(
                "Already connected — reusing {} ({}).",
                outcome.product,
                details.join(", ")
            ))
        } else {
            Ok(format!(
                "Browser connected: {} ({}).",
                outcome.product,
                details.join(", ")
            ))
        }
    }
}

/// Close the automation browser connection. Attached browsers are only
/// disconnected — never closed.
pub struct BrowserCloseTool;

#[async_trait]
impl Tool for BrowserCloseTool {
    fn name(&self) -> &str {
        "browser_close"
    }

    fn description(&self) -> &str {
        "Close the browser automation connection. A browser launched by siku is closed (profile is kept \
         for persistent mode, deleted for temporary mode); an attached browser is only disconnected, never closed. \
         Use when a browser task is finished and the window is no longer needed."
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![]
    }

    async fn execute(&self, _args: serde_json::Value) -> Result<String, String> {
        let mgr = browser::global().ok_or("browser manager unavailable (app not fully initialized)")?;
        if mgr.disconnect().await {
            Ok("Browser connection closed.".to_string())
        } else {
            Ok("No browser connection was open.".to_string())
        }
    }
}
