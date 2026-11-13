//! `web_fetch`: fetch a public HTTP(S) URL and return readable text.
//!
//! This is deliberately a *local* fetch, not a browser: it does not execute
//! JavaScript and cannot solve CAPTCHA/Cloudflare challenges. It is the cheap
//! path for documentation, articles, raw files and APIs. When a site returns
//! an anti-bot challenge, the tool tells the model to switch to the real
//! browser (`browser_connect` → `browser_navigate` → `browser_read`).
//!
//! Safety/robustness layer:
//! - only http/https; reject localhost, no-dot intranet names, private /
//!   loopback / link-local / CGNAT / documentation IPs;
//! - a custom reqwest DNS resolver rejects hostnames that resolve private, and
//!   pins the connection to the validated addresses (no DNS-rebinding TOCTOU);
//! - redirects are followed manually, each hop revalidated, HTTPS→HTTP refused;
//! - response size and duration are bounded;
//! - successful pages are cached in memory for 15 minutes and fetched hosts
//!   are rate-limited (10 requests / 60 s) so a loop cannot hammer a site;
//! - a transparent `Siku/<version>` UA and `Accept: text/markdown` content
//!   negotiation are used. Do NOT spoof a browser UA: TLS/HTTP2 fingerprints
//!   would not match, and it does not solve real bot walls anyway.

use async_trait::async_trait;
use once_cell::sync::Lazy;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::header::{
    HeaderMap, HeaderValue, ACCEPT, ACCEPT_LANGUAGE, CONTENT_TYPE, LOCATION,
};
use reqwest::redirect::Policy;
use reqwest::{Client, StatusCode, Url};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::warn;

use crate::ai::agent::tool_registry::{Tool, ToolParameter};

const USER_AGENT: &str = concat!("Siku/", env!("CARGO_PKG_VERSION"));
const ACCEPT_VALUE: &str = "text/markdown, text/html;q=0.9, application/xhtml+xml;q=0.8, \
                            text/plain;q=0.7, application/json;q=0.6, */*;q=0.5";
const ACCEPT_LANGUAGE_VALUE: &str = "zh-CN,zh;q=0.9,en;q=0.8";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(45);
const MAX_REDIRECTS: usize = 10;
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_CACHE_CHARS: usize = 1_000_000;

const CACHE_TTL: Duration = Duration::from_secs(15 * 60);
const CACHE_MAX_ENTRIES: usize = 64;
const RATE_WINDOW: Duration = Duration::from_secs(60);
const RATE_MAX_REQUESTS: usize = 10;

#[derive(Clone)]
struct CachedPage {
    text: String,
    extracted: bool,
    title: String,
    final_url: String,
    fetched_at: Instant,
}

struct ResponseCache {
    entries: Mutex<HashMap<String, CachedPage>>,
    ttl: Duration,
    max_entries: usize,
}

impl ResponseCache {
    fn new(ttl: Duration, max_entries: usize) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            ttl,
            max_entries,
        }
    }

    fn get(&self, key: &str) -> Option<CachedPage> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let expired = entries
            .get(key)
            .map(|page| page.fetched_at.elapsed() > self.ttl)
            .unwrap_or(false);
        if expired {
            entries.remove(key);
            return None;
        }
        entries.get(key).cloned()
    }

    fn put(&self, key: String, page: CachedPage) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.retain(|_, page| page.fetched_at.elapsed() <= self.ttl);
        if self.max_entries == 0 {
            return;
        }
        while entries.len() >= self.max_entries {
            let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, page)| page.fetched_at)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            entries.remove(&oldest);
        }
        entries.insert(key, page);
    }
}

impl Default for ResponseCache {
    fn default() -> Self {
        Self::new(CACHE_TTL, CACHE_MAX_ENTRIES)
    }
}

struct HostRateLimiter {
    hosts: Mutex<HashMap<String, Vec<Instant>>>,
    window: Duration,
    max_requests: usize,
}

impl HostRateLimiter {
    fn new(window: Duration, max_requests: usize) -> Self {
        Self {
            hosts: Mutex::new(HashMap::new()),
            window,
            max_requests,
        }
    }

    /// Record one request for `host`. `Err(wait_secs)` means the host has hit
    /// the window limit and the caller should not send the request.
    fn check(&self, host: &str) -> Result<(), u64> {
        let now = Instant::now();
        let mut hosts = self.hosts.lock().unwrap_or_else(|e| e.into_inner());
        {
            let stamps = hosts.entry(host.to_string()).or_default();
            stamps.retain(|stamp| now.duration_since(*stamp) < self.window);
            if self.max_requests == 0 || stamps.len() >= self.max_requests {
                let wait = stamps
                    .first()
                    .map(|stamp| self.window.saturating_sub(now.duration_since(*stamp)))
                    .unwrap_or(self.window);
                return Err(wait.as_secs().max(1));
            }
            stamps.push(now);
        }
        // Bound the map: an agent can touch many hosts; stale entries must not
        // accumulate for the life of the process.
        if hosts.len() > 1024 {
            hosts.retain(|_, stamps| {
                stamps.retain(|stamp| now.duration_since(*stamp) < self.window);
                !stamps.is_empty()
            });
        }
        Ok(())
    }
}

impl Default for HostRateLimiter {
    fn default() -> Self {
        Self::new(RATE_WINDOW, RATE_MAX_REQUESTS)
    }
}

static WEB_FETCH_CACHE: Lazy<ResponseCache> = Lazy::new(ResponseCache::default);
static WEB_FETCH_RATE: Lazy<HostRateLimiter> = Lazy::new(HostRateLimiter::default);

/// A DNS resolver that only returns public addresses. It also pins the
/// connection to the addresses it validated, so a second DNS answer cannot
/// turn a public name into a private one between the check and the connect.
struct PublicDnsResolver;

impl Resolve for PublicDnsResolver {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let host = name.as_str().to_string();
            let addrs = tokio::net::lookup_host((host.as_str(), 0))
                .await
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
            let addrs: Vec<SocketAddr> = addrs.collect();
            if addrs.is_empty() {
                return Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("no DNS addresses for {host}"),
                )) as Box<dyn std::error::Error + Send + Sync>);
            }
            if let Some(bad) = addrs.iter().find(|addr| !is_public_ip(addr.ip())) {
                return Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("{host} resolves to non-public address {}", bad.ip()),
                )) as Box<dyn std::error::Error + Send + Sync>);
            }
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

/// Public, globally routable address check. This intentionally rejects more
/// than `IpAddr::is_private`: loopback, link-local, CGNAT, benchmarking,
/// documentation ranges and unspecified/multicast addresses are all useless
/// (or dangerous) targets for an agent-driven fetch.
fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => {
            // Covers IPv4-mapped (::ffff:a.b.c.d) and IPv4-compatible forms.
            if let Some(v4) = v6.to_ipv4() {
                return is_public_v4(v4);
            }
            let s = v6.segments();
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // unique local fc00::/7
                || (s[0] & 0xfe00) == 0xfc00
                // link local fe80::/10
                || (s[0] & 0xffc0) == 0xfe80
                // 2001:db8::/32 documentation
                || (s[0] == 0x2001 && s[1] == 0x0db8))
        }
    }
}

fn is_public_v4(v4: Ipv4Addr) -> bool {
    let o = v4.octets();
    !(v4.is_private()
        || v4.is_loopback()
        || v4.is_link_local()
        || v4.is_broadcast()
        || v4.is_unspecified()
        || v4.is_multicast()
        // 0.0.0.0/8
        || o[0] == 0
        // 100.64.0.0/10 CGNAT
        || (o[0] == 100 && (o[1] & 0xc0) == 64)
        // 192.0.0.0/24
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)
        // 192.0.2.0/24 TEST-NET-1
        || (o[0] == 192 && o[1] == 0 && o[2] == 2)
        // 198.18.0.0/15 benchmarking
        || (o[0] == 198 && (o[1] == 18 || o[1] == 19))
        // 198.51.100.0/24 TEST-NET-2
        || (o[0] == 198 && o[1] == 51 && o[2] == 100)
        // 203.0.113.0/24 TEST-NET-3
        || (o[0] == 203 && o[1] == 0 && o[2] == 113)
        // 240.0.0.0/4 reserved
        || o[0] >= 240)
}

/// Reject non-HTTP(S), localhost, bare intranet names and private literal IPs.
/// DNS answers are additionally filtered by `PublicDnsResolver` when no proxy
/// is configured. With a proxy, DNS happens at the proxy; we still reject
/// private literals and intranet names, but cannot inspect its resolution.
fn validate_url(url: &Url) -> Result<(), String> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!(
            "unsupported URL scheme '{}' (only http/https are allowed)",
            url.scheme()
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| "URL has no host".to_string())?;
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = bare.parse::<IpAddr>() {
        if !is_public_ip(ip) {
            return Err(format!(
                "refusing to fetch {url}: {} is a private/loopback/reserved address",
                ip
            ));
        }
        return Ok(());
    }
    let domain = bare.to_ascii_lowercase();
    if domain == "localhost"
        || domain.ends_with(".localhost")
        || domain.ends_with(".local")
        || domain.ends_with(".internal")
        || !domain.contains('.')
    {
        return Err(format!(
            "refusing to fetch {url}: '{host}' is a localhost/intranet host; use browser_read for local services"
        ));
    }
    Ok(())
}

/// Detect anti-bot / JS challenge pages. A plain 403 is already suspicious;
/// a 200 challenge page is usually a short HTML document with a known marker.
fn challenge_marker(status: StatusCode, content_type: &str, body: &str) -> Option<&'static str> {
    let lower = body
        .chars()
        .take(200_000)
        .collect::<String>()
        .to_ascii_lowercase();
    const MARKERS: &[&str] = &[
        "cf-chl",
        "just a moment",
        "attention required",
        "enable javascript and cookies",
        "checking your browser",
        "verify you are human",
        "captcha",
        "access denied",
        "request blocked",
        "bot detection",
    ];
    let hit = MARKERS.iter().find(|marker| lower.contains(**marker)).copied();
    if status == StatusCode::FORBIDDEN
        || status == StatusCode::TOO_MANY_REQUESTS
        || status == StatusCode::SERVICE_UNAVAILABLE
    {
        return Some(hit.unwrap_or("site refused automated access"));
    }
    let html_like = content_type.to_ascii_lowercase().contains("html")
        || (content_type.is_empty() && looks_like_html("", body));
    if status.is_success() && html_like && body.len() < 100_000 {
        return hit;
    }
    None
}

fn challenge_error(host: &str, status: StatusCode, marker: &str) -> String {
    format!(
        "HTTP {} — {host} 被反爬/浏览器校验拦截（{marker}）。web_fetch 不执行 JavaScript，\
         无法通过此类校验；请改用 browser_connect → browser_navigate → browser_read \
         在真实浏览器中打开该页面。",
        status.as_u16()
    )
}

fn status_error(status: StatusCode, host: &str, body: &str, content_type: &str) -> String {
    if let Some(marker) = challenge_marker(status, content_type, body) {
        return challenge_error(host, status, marker);
    }
    match status {
        StatusCode::UNAUTHORIZED => {
            format!("HTTP 401 — {host} 需要登录；web_fetch 不携带登录态，请用 browser_connect 的持久化浏览器读取。")
        }
        StatusCode::NOT_FOUND => format!("HTTP 404 — 页面不存在: {host}"),
        StatusCode::TOO_MANY_REQUESTS => {
            format!("HTTP 429 — {host} 请求过于频繁，请稍后再试或改用 browser_read。")
        }
        s if s.is_server_error() => format!("HTTP {} — {host} 暂时不可用。", s.as_u16()),
        s => format!("HTTP {} — 无法抓取 {host}。", s.as_u16()),
    }
}

fn is_unsupported_binary(content_type: &str) -> bool {
    let ct = content_type.to_ascii_lowercase();
    ct.starts_with("application/pdf")
        || ct.starts_with("image/")
        || ct.starts_with("audio/")
        || ct.starts_with("video/")
        || ct.starts_with("application/zip")
        || ct.starts_with("application/octet-stream")
        || ct.starts_with("application/msword")
        || ct.starts_with("application/vnd.")
}

fn looks_like_html(content_type: &str, body: &str) -> bool {
    let ct = content_type.to_ascii_lowercase();
    if ct.contains("text/html") || ct.contains("application/xhtml") {
        return true;
    }
    if ct.starts_with("text/")
        || ct.contains("json")
        || ct.contains("markdown")
        || ct.contains("xml")
    {
        return false;
    }
    let head = body.chars().take(2048).collect::<String>().to_ascii_lowercase();
    head.contains("<html") || head.contains("<!doctype") || head.contains("<body")
}

fn remove_comments(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 4..];
        if let Some(end) = after.find("-->") {
            rest = &after[end + 3..];
        } else {
            return out;
        }
    }
    out.push_str(rest);
    out
}

/// Remove an element and its content (case-insensitive, first matching close
/// tag). Good enough for boilerplate tags like `<script>`/`<nav>`; malformed
/// markup simply leaves the start tag removed rather than eating the page.
fn remove_element_blocks(html: &str, tag: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = String::with_capacity(html.len());
    let mut i = 0usize;
    while i < html.len() {
        let Some(rel) = lower[i..].find(&open) else {
            out.push_str(&html[i..]);
            break;
        };
        let start = i + rel;
        let after = lower.as_bytes().get(start + open.len()).copied();
        let valid = matches!(
            after,
            None | Some(b'>') | Some(b'/') | Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r')
        );
        if !valid {
            out.push_str(&html[i..start + 1]);
            i = start + 1;
            continue;
        }
        out.push_str(&html[i..start]);
        if let Some(close_rel) = lower[start..].find(&close) {
            i = start + close_rel + close.len();
        } else if let Some(end) = html[start..].find('>') {
            i = start + end + 1;
        } else {
            break;
        }
    }
    out
}

fn longest_tag_content(html: &str, tag: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut best: Option<String> = None;
    let mut i = 0usize;
    while let Some(rel) = lower[i..].find(&open) {
        let start = i + rel;
        if let Some(close_rel) = lower[start..].find(&close) {
            let end = start + close_rel;
            let content = &html[start..end];
            if best.as_ref().map(|b| content.len() > b.len()).unwrap_or(true) {
                best = Some(content.to_string());
            }
            i = end + close.len();
        } else {
            break;
        }
    }
    best
}

fn decode_entity(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" | "#39" => Some('\''),
        "nbsp" => Some(' '),
        "mdash" => Some('—'),
        "ndash" => Some('–'),
        "hellip" => Some('…'),
        "lsquo" => Some('‘'),
        "rsquo" => Some('’'),
        "ldquo" => Some('“'),
        "rdquo" => Some('”'),
        "copy" => Some('©'),
        "reg" => Some('®'),
        "trade" => Some('™'),
        _ => {
            if let Some(hex) = entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
            {
                return u32::from_str_radix(hex, 16).ok().and_then(char::from_u32);
            }
            if let Some(dec) = entity.strip_prefix('#') {
                return dec.parse::<u32>().ok().and_then(char::from_u32);
            }
            None
        }
    }
}

fn decode_entities(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0usize;
    while i < input.len() {
        if bytes[i] == b'&' {
            if let Some(rel) = input[i..].find(';') {
                if rel <= 12 {
                    let entity = &input[i + 1..i + rel];
                    if let Some(ch) = decode_entity(entity) {
                        out.push(ch);
                        i += rel + 1;
                        continue;
                    }
                }
            }
        }
        let Some(ch) = input[i..].chars().next() else {
            break;
        };
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn html_to_text(html: &str) -> String {
    let chars: Vec<char> = html.chars().collect();
    let mut out = String::with_capacity(html.len() / 2);
    let mut in_tag = false;
    let mut tag_name = String::new();
    let mut i = 0usize;
    let n = chars.len();
    while i < n {
        let c = chars[i];
        if c == '<' {
            in_tag = true;
            tag_name.clear();
            i += 1;
            continue;
        }
        if c == '>' {
            let block = matches!(
                tag_name.as_str(),
                "p" | "div"
                    | "li"
                    | "br"
                    | "h1"
                    | "h2"
                    | "h3"
                    | "h4"
                    | "h5"
                    | "h6"
                    | "section"
                    | "article"
                    | "main"
                    | "ul"
                    | "ol"
                    | "pre"
                    | "table"
                    | "tr"
                    | "td"
                    | "th"
                    | "blockquote"
                    | "figcaption"
            );
            if block {
                out.push('\n');
            }
            in_tag = false;
            i += 1;
            continue;
        }
        if in_tag {
            if c.is_ascii_alphabetic() && tag_name.len() < 12 {
                tag_name.push(c.to_ascii_lowercase());
            }
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }

    let decoded = decode_entities(&out);
    decoded
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn extract_title(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let Some(start) = lower.find("<title") else {
        return String::new();
    };
    let Some(open_end) = html[start..].find('>') else {
        return String::new();
    };
    let content_start = start + open_end + 1;
    let Some(close_rel) = lower[content_start..].find("</title>") else {
        return String::new();
    };
    let raw = &html[content_start..content_start + close_rel];
    decode_entities(&html_to_text(raw))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

struct ExtractedHtml {
    text: String,
    title: String,
}

fn extract_html(html: &str) -> ExtractedHtml {
    let title = extract_title(html);
    let mut cleaned = remove_comments(html);
    for tag in [
        "script",
        "style",
        "noscript",
        "template",
        "svg",
        "iframe",
        "object",
        "embed",
        "nav",
        "footer",
        "aside",
        "form",
        "button",
        "select",
        "textarea",
    ] {
        cleaned = remove_element_blocks(&cleaned, tag);
    }

    let article = longest_tag_content(&cleaned, "article");
    let main = longest_tag_content(&cleaned, "main");
    let candidate = [article, main]
        .into_iter()
        .flatten()
        .map(|fragment| html_to_text(&fragment))
        .max_by_key(|text| text.chars().count());
    let body_text = html_to_text(&cleaned);
    let text = match candidate {
        Some(candidate) if candidate.chars().count() >= 200 => candidate,
        _ => body_text,
    };
    ExtractedHtml { text, title }
}

fn format_page(page: &CachedPage, requested: &Url, max_chars: usize) -> String {
    let kind = if page.extracted {
        "extracted main text from HTML"
    } else {
        "full response body"
    };
    let mut header = format!("[web_fetch] {kind}");
    if page.extracted && !page.title.is_empty() {
        let title = page.title.split_whitespace().collect::<Vec<_>>().join(" ");
        header.push_str(&format!(" — title: {title}"));
    }
    if page.final_url != requested.as_str() {
        header.push_str(&format!(" — final URL: {}", page.final_url));
    }
    header.push_str("\n\n");

    let budget = max_chars.max(1);
    let total = page.text.chars().count();
    let truncated: String = page.text.chars().take(budget).collect();
    let mut out = header + &truncated;
    if total > budget {
        out.push_str(&format!(
            "\n\n[Content truncated at {budget} chars, original length: {total}]"
        ));
    }
    out
}

fn build_client(proxy: Option<&str>) -> Result<Client, String> {
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static(ACCEPT_VALUE));
    headers.insert(ACCEPT_LANGUAGE, HeaderValue::from_static(ACCEPT_LANGUAGE_VALUE));

    let mut builder = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .redirect(Policy::none())
        .user_agent(USER_AGENT)
        .default_headers(headers)
        .cookie_store(true);
    if let Some(proxy) = proxy {
        // DNS runs at the proxy; URL validation still rejects private
        // literals and intranet names, but the proxy's resolution is trusted.
        builder = builder.proxy(
            reqwest::Proxy::all(proxy).map_err(|e| format!("invalid web proxy: {e}"))?,
        );
    } else {
        builder = builder.dns_resolver(Arc::new(PublicDnsResolver));
    }
    builder.build().map_err(|e| format!("client error: {e}"))
}

async fn fetch_page(client: &Client, mut url: Url) -> Result<CachedPage, String> {
    for hop in 0..=MAX_REDIRECTS {
        validate_url(&url)?;
        let host = url.host_str().unwrap_or("unknown").to_string();
        if let Err(wait) = WEB_FETCH_RATE.check(&host) {
            return Err(format!(
                "请求过于频繁：{host} 在 60 秒内最多 10 次，请 {wait} 秒后重试，\
                 或改用 browser_read 读取页面。"
            ));
        }

        let resp = client
            .get(url.clone())
            .send()
            .await
            .map_err(|e| format!("fetch error: {e}"))?;
        let status = resp.status();

        if status.is_redirection() {
            if hop == MAX_REDIRECTS {
                return Err(format!("too many redirects (> {MAX_REDIRECTS})"));
            }
            let location = resp
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| format!("HTTP {} redirect without Location", status.as_u16()))?;
            let next = url
                .join(location)
                .map_err(|e| format!("invalid redirect target: {e}"))?;
            validate_url(&next)?;
            if url.scheme() == "https" && next.scheme() == "http" {
                return Err(format!(
                    "refusing HTTPS → HTTP redirect from {url} to {next}"
                ));
            }
            warn!(from = %url, to = %next, "web_fetch following redirect");
            url = next;
            continue;
        }

        let content_type = resp
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        if is_unsupported_binary(&content_type) {
            return Err(format!(
                "unsupported content type '{content_type}' at {url}; web_fetch only returns text. \
                 Use browser_navigate + browser_read or a specialized import tool for files."
            ));
        }
        if let Some(length) = resp.content_length() {
            if length > MAX_RESPONSE_BYTES as u64 {
                return Err(format!(
                    "response too large ({length} bytes > {MAX_RESPONSE_BYTES}); use browser_read or download it directly"
                ));
            }
        }
        let body = resp.text().await.map_err(|e| format!("read error: {e}"))?;
        if body.len() > MAX_RESPONSE_BYTES {
            return Err(format!(
                "response too large ({} bytes > {MAX_RESPONSE_BYTES})",
                body.len()
            ));
        }
        if !status.is_success() {
            return Err(status_error(status, &host, &body, &content_type));
        }
        if let Some(marker) = challenge_marker(status, &content_type, &body) {
            return Err(challenge_error(&host, status, marker));
        }

        let final_url = url.to_string();
        if looks_like_html(&content_type, &body) {
            let extracted = extract_html(&body);
            if extracted.text.trim().chars().count() < 80 {
                return Err(format!(
                    "页面几乎没有可读正文（可能是 JavaScript 渲染、登录墙或反爬页面）。\
                     请改用 browser_connect → browser_navigate → browser_read 打开 {url}。"
                ));
            }
            return Ok(CachedPage {
                text: extracted.text.chars().take(MAX_CACHE_CHARS).collect(),
                extracted: true,
                title: extracted.title,
                final_url,
                fetched_at: Instant::now(),
            });
        }
        if body.trim().is_empty() {
            return Err(format!("empty response body from {url}"));
        }
        return Ok(CachedPage {
            text: body.chars().take(MAX_CACHE_CHARS).collect(),
            extracted: false,
            title: String::new(),
            final_url,
            fetched_at: Instant::now(),
        });
    }
    Err(format!("too many redirects (> {MAX_REDIRECTS})"))
}

pub struct WebFetchTool {
    db: SqlitePool,
    web_proxy: Option<String>,
}

impl WebFetchTool {
    pub fn new(db: SqlitePool, web_proxy: Option<String>) -> Self {
        Self { db, web_proxy }
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch a public HTTP(S) URL and return readable text. HTML is converted to extracted main \
         text; JavaScript is NOT executed, so challenge/JS-rendered/login pages fail with guidance \
         to use browser_connect + browser_navigate + browser_read. Rejects localhost/private hosts, \
         follows validated redirects, caches pages for 15 minutes and rate-limits each host. \
         Read-only, auto-approved."
    }

    fn readonly(&self) -> bool {
        true
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![ToolParameter {
            name: "url".into(),
            param_type: "string".into(),
            description: "Public http(s) URL to fetch".into(),
            required: true,
        }]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let raw = args["url"].as_str().ok_or("url required")?.trim();
        if raw.is_empty() {
            return Err("url required".to_string());
        }
        let mut url = Url::parse(raw).map_err(|e| format!("invalid URL: {e}"))?;
        validate_url(&url)?;
        url.set_fragment(None);

        let proxy = super::resolve_web_proxy(&self.db, self.web_proxy.as_deref()).await;
        let cache_key = format!("{}\nproxy={}", url, proxy.as_deref().unwrap_or(""));
        let max_chars = crate::core::settings_service::cached_settings()
            .tool_web_fetch_max_chars
            .max(1) as usize;

        if let Some(page) = WEB_FETCH_CACHE.get(&cache_key) {
            return Ok(format_page(&page, &url, max_chars));
        }

        let client = build_client(proxy.as_deref())?;
        let page = tokio::time::timeout(TOTAL_TIMEOUT, fetch_page(&client, url.clone()))
            .await
            .map_err(|_| format!("fetch timed out after {}s", TOTAL_TIMEOUT.as_secs()))??;
        WEB_FETCH_CACHE.put(cache_key, page.clone());
        Ok(format_page(&page, &url, max_chars))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(url: &str) -> Url {
        Url::parse(url).unwrap()
    }

    #[test]
    fn public_ip_classification() {
        for ip in [
            "8.8.8.8",
            "1.1.1.1",
            "2606:4700:4700::1111",
        ] {
            assert!(is_public_ip(ip.parse().unwrap()), "{ip} should be public");
        }
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.1.1",
            "100.64.0.1",
            "0.0.0.0",
            "192.0.2.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "240.0.0.1",
            "2001:db8::1",
            "::1",
            "fc00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(!is_public_ip(ip.parse().unwrap()), "{ip} should be blocked");
        }
    }

    #[test]
    fn url_validation_blocks_local_and_intranet_hosts() {
        assert!(validate_url(&parse("https://example.com/a")).is_ok());
        assert!(validate_url(&parse("http://example.com")).is_ok());
        for bad in [
            "file:///etc/passwd",
            "ftp://example.com/x",
            "http://localhost/",
            "http://foo.localhost/",
            "http://printer/",
            "http://intranet.local/",
            "http://127.0.0.1/",
            "http://[::1]/",
            "http://10.0.0.1/",
            "http://192.168.1.1/",
        ] {
            assert!(
                validate_url(&parse(bad)).is_err(),
                "{bad} should be rejected"
            );
        }
    }

    #[test]
    fn challenge_detection() {
        assert!(challenge_marker(
            StatusCode::FORBIDDEN,
            "text/html",
            "<html><title>Just a moment...</title></html>"
        )
        .is_some());
        assert!(challenge_marker(StatusCode::FORBIDDEN, "text/html", "nope").is_some());
        assert!(challenge_marker(
            StatusCode::OK,
            "text/html",
            "<html><div class=\"cf-chl\"></div></html>"
        )
        .is_some());
        assert!(challenge_marker(StatusCode::OK, "text/html", "<html><article>Hello</article></html>").is_none());
        assert!(challenge_marker(StatusCode::NOT_FOUND, "text/html", "captcha").is_none());
    }

    #[test]
    fn extracts_article_not_navigation() {
        let html = r#"
            <html><head><title>My Doc</title><script>bad()</script></head>
            <body>
              <nav>MENU LINK</nav>
              <article>
                <h1>Heading</h1>
                <p>Hello &amp; welcome.</p>
                <p>Second paragraph &lt;ok&gt;.</p>
              </article>
              <footer>FOOTER</footer>
            </body></html>
        "#;
        let page = extract_html(html);
        assert_eq!(page.title, "My Doc");
        assert!(page.text.contains("Hello & welcome."), "{}", page.text);
        assert!(page.text.contains("Second paragraph <ok>."), "{}", page.text);
        assert!(!page.text.contains("MENU LINK"), "{}", page.text);
        assert!(!page.text.contains("FOOTER"), "{}", page.text);
        assert!(!page.text.contains("bad()"), "{}", page.text);
    }

    #[test]
    fn falls_back_to_body_text_without_article() {
        let html = "<html><body><div><p>Alpha beta.</p><p>Gamma delta.</p></div></body></html>";
        let page = extract_html(html);
        assert!(page.text.contains("Alpha beta."), "{}", page.text);
        assert!(page.text.contains("Gamma delta."), "{}", page.text);
    }

    #[test]
    fn cache_hits_and_respects_limits() {
        let cache = ResponseCache::new(Duration::from_secs(60), 2);
        let page = |url: &str| CachedPage {
            text: "hello".to_string(),
            extracted: false,
            title: String::new(),
            final_url: url.to_string(),
            fetched_at: Instant::now(),
        };
        cache.put("a".into(), page("https://a/"));
        cache.put("b".into(), page("https://b/"));
        assert!(cache.get("a").is_some());
        cache.put("c".into(), page("https://c/"));
        let (a, b, c) = (
            cache.get("a").is_some(),
            cache.get("b").is_some(),
            cache.get("c").is_some(),
        );
        assert_eq!(
            [a, b, c].iter().filter(|present| **present).count(),
            2,
            "cache must cap entries"
        );
        assert!(c, "newest entry should survive");
    }

    /// Optional live smoke test: `SIKU_LIVE_WEB_TEST=1 cargo test --lib live_fetch_smoke`.
    /// It exercises the custom DNS resolver, rustls, redirect-free fetch and
    /// HTML extraction against a stable public page.
    #[tokio::test]
    async fn live_fetch_smoke() {
        if std::env::var("SIKU_LIVE_WEB_TEST").is_err() {
            return;
        }
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let tool = WebFetchTool::new(db, None);
        let out = tool
            .execute(serde_json::json!({"url": "http://example.com/"}))
            .await
            .expect("live fetch should succeed");
        assert!(out.contains("Example Domain"), "unexpected output: {out}");
        assert!(out.contains("extracted main text"), "unexpected output: {out}");
    }

    #[test]
    fn rate_limiter_blocks_after_limit() {
        let limiter = HostRateLimiter::new(Duration::from_secs(60), 2);
        assert!(limiter.check("example.com").is_ok());
        assert!(limiter.check("example.com").is_ok());
        let wait = limiter.check("example.com").unwrap_err();
        assert!(wait >= 1);
        assert!(limiter.check("other.example").is_ok());
    }
}
