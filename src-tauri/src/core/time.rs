use chrono::{DateTime, Utc};

pub fn now_iso() -> String {
    // Millisecond precision: LWW conflict resolution compares these strings,
    // and second precision collapses edits made within the same second on
    // different devices into a tie. Note that mixed-precision strings do NOT
    // order correctly at a second boundary ("...:00Z" > "...:00.500Z"
    // lexicographically), so comparisons must parse the timestamps (see
    // `sync::crdt::apply_changes`).
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
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
    fn test_now_iso_has_millis_and_parses() {
        let now = now_iso();
        // RFC3339 with millis: "2026-08-31T12:34:56.789Z".
        assert!(now.contains('.'), "now_iso must carry milliseconds: {now}");
        assert!(
            DateTime::parse_from_rfc3339(&now).is_ok(),
            "now_iso must stay RFC3339-parseable: {now}"
        );
    }

    /// Lexicographic string comparison is consistent for same-precision
    /// timestamps, and both directions of a second/millis mix parse to the
    /// right chronological order (the LWW path parses before comparing —
    /// plain string order would be wrong across a second boundary).
    #[test]
    fn test_rfc3339_ordering_across_precisions() {
        let secs = "2026-06-16T10:30:00Z";
        let millis_same_second = "2026-06-16T10:30:00.500Z";
        let millis_next_second = "2026-06-16T10:30:01.000Z";

        // Same precision: lexicographic == chronological.
        assert!(millis_next_second > millis_same_second);

        // Mixed precision must be compared parsed, not as strings.
        let s = DateTime::parse_from_rfc3339(secs).unwrap();
        let m = DateTime::parse_from_rfc3339(millis_same_second).unwrap();
        assert!(m > s, "millis timestamp is later within the same second");
        assert!(
            millis_same_second < secs,
            "lexicographic order is WRONG here ('.' < 'Z'); comparisons must parse"
        );
    }

    #[test]
    fn test_iso_to_display() {
        assert_eq!(
            iso_to_display("2026-06-16T10:30:00Z"),
            "2026-06-16 10:30"
        );
    }
}
