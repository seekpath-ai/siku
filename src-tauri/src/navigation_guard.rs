//! Keep every webview on the app itself.
//!
//! A link inside a paper used to navigate the main webview in place: the whole
//! UI was replaced by the web page, with no way back but a restart. The frontend
//! now hands links to the system browser, and this guard is the backstop for
//! anything it misses — a link we never anticipated, a dropped URL, a PDF
//! JavaScript action, a form post.
//!
//! Registered as an app-level plugin so it also covers config-declared windows
//! (main, pet, splashscreen), which are created by Tauri and therefore cannot
//! take a `WebviewWindowBuilder::on_navigation` handler.

use tauri::plugin::{Builder, TauriPlugin};
use tauri::{Runtime, Url, Webview};
use tauri_plugin_shell::ShellExt;
use tracing::warn;

/// Is this host the machine we are running on?
///
/// The bundled frontend is served over http on Windows and Android
/// (`http://tauri.localhost`), over `tauri://localhost` on macOS/Linux, and from
/// the Vite dev server (a loopback address) while developing. Getting this wrong
/// is not a cosmetic bug: the app's own first navigation would be cancelled and
/// handed to the browser, and the window would never load.
fn is_local_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host == "localhost"
        || host.ends_with(".localhost")
        || host.parse::<std::net::IpAddr>().map(|ip| ip.is_loopback()).unwrap_or(false)
}

/// URLs that are the app itself — plus the blank/blob/data URLs WebKit creates
/// on its own for internal frames and attachment previews.
fn is_app_url(url: &Url) -> bool {
    match url.scheme() {
        "tauri" | "about" | "data" | "blob" => true,
        "http" | "https" => url.host_str().map(is_local_host).unwrap_or(false),
        _ => false,
    }
}

/// Schemes the OS knows how to hand to another application.
fn is_external(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https" | "mailto" | "tel")
}

pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("navigation-guard")
        .on_navigation(|webview: &Webview<R>, url: &Url| {
            if is_app_url(url) {
                return true;
            }
            // Anything on the webview's own origin is the app navigating its own
            // routes, whatever scheme the platform happens to serve it from.
            // This is the platform-independent half of the check above: it is
            // what keeps a misjudged "local" host from cancelling the app's own
            // startup navigation and leaving the window blank.
            let current = webview.url().ok();
            if current
                .as_ref()
                .map(|u| u.origin() == url.origin())
                .unwrap_or(false)
            {
                return true;
            }
            // A webview that has not loaded anything yet (or whose URL we cannot
            // read) has no app to protect and everything to lose: blocking its
            // first navigation leaves an empty window that can never recover.
            // From the first real load onwards the origin rule above applies.
            if current
                .as_ref()
                .map(|u| u.as_str() == "about:blank")
                .unwrap_or(true)
            {
                return true;
            }

            // Cancel the navigation either way; an external address goes to the
            // browser, anything else (file:, javascript:, an unknown scheme) is
            // simply refused.
            warn!(
                webview = webview.label(),
                %url,
                "navigation outside the app blocked"
            );
            if is_external(url) {
                // `Webview` is a manager, so the shell plugin's state (and its
                // cross-platform `open`) is reachable without an AppHandle.
                // (Shell::open is deprecated in favour of tauri-plugin-opener;
                // migrating is a separate change, and the frontend already
                // routes links itself with the same shell command.)
                #[allow(deprecated)]
                let opened = webview.shell().open(url.as_str(), None);
                let _ = opened;
            }
            false
        })
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).expect("test url")
    }

    #[test]
    fn app_urls_are_allowed() {
        for s in [
            // The bundled frontend: Windows/Android serve it over http, and this
            // exact URL was once cancelled and sent to the browser, which left
            // the app unable to start at all.
            "http://tauri.localhost/",
            "http://tauri.localhost/splashscreen.html",
            "http://tauri.localhost/reader/12?tab=3",
            "tauri://localhost/index.html",
            // Development server and any other loopback address.
            "http://localhost:1420/",
            "http://127.0.0.1:3080/reader",
            "http://[::1]:1420/",
            "about:blank",
            "blob:tauri://localhost/abc",
            "data:image/png;base64,AAAA",
        ] {
            assert!(is_app_url(&url(s)), "{s} should be treated as the app");
        }
    }

    #[test]
    fn external_urls_are_never_the_app() {
        for s in [
            "https://doi.org/10.1016/j.compind.2023.103999",
            "http://example.com/paper.pdf",
            "https://localhost.evil.com/",
            "https://tauri.localhost.evil.com/",
        ] {
            let u = url(s);
            assert!(!is_app_url(&u), "{s} must not be treated as the app");
            assert!(is_external(&u), "{s} should be handed to the browser");
        }
    }

    #[test]
    fn local_host_matching_is_exact() {
        for h in ["localhost", "tauri.localhost", "127.0.0.1", "::1", "[::1]", "dev.localhost"] {
            assert!(is_local_host(h), "{h} is local");
        }
        for h in [
            "localhost.evil.com",
            "tauri.localhost.evil.com",
            "notlocalhost",
            "example.com",
            // A domain that merely starts with a loopback address.
            "127.0.0.1.evil.com",
        ] {
            assert!(!is_local_host(h), "{h} is not local");
        }
    }

    #[test]
    fn unknown_schemes_are_blocked_without_a_handoff() {
        for s in ["file:///etc/passwd", "javascript:alert(1)", "ftp://host/x"] {
            let u = url(s);
            assert!(!is_app_url(&u));
            assert!(!is_external(&u));
        }
    }
}
