use chrono::{DateTime, Utc};

pub fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub fn iso_to_display(iso: &str) -> String {
    DateTime::parse_from_rfc3339(iso)
        .map(|dt| dt.with_timezone(&Utc).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|_| iso.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_now_iso_ends_with_z() {
        let now = now_iso();
        assert!(now.ends_with('Z'));
    }

    #[test]
    fn test_iso_to_display() {
        assert_eq!(
            iso_to_display("2026-06-16T10:30:00Z"),
            "2026-06-16 10:30"
        );
    }
}
