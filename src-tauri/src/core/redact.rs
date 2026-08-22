//! Helpers for redacting secrets in log output.

/// Mask an API key so logs show only a few leading and trailing characters.
///
/// - Long keys (>7 chars): first 3 + "..." + last 4.
/// - Short non-empty keys: first 1 + "...".
/// - Empty keys: "".
pub fn redact_api_key(key: &str) -> String {
    let len = key.chars().count();
    if len == 0 {
        return String::new();
    }
    if len > 7 {
        let head: String = key.chars().take(3).collect();
        let tail: String = key.chars().skip(len.saturating_sub(4)).collect();
        format!("{}...{}", head, tail)
    } else {
        let head: String = key.chars().take(1).collect();
        format!("{}...", head)
    }
}
