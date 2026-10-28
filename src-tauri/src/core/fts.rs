/// FTS5 MATCH query construction from raw user input.
///
/// FTS5 treats `-`, `:`, `(`, `"` and friends as query syntax, so a title
/// like "Preference-Aware" pasted into a search box becomes
/// `Preference-Aware*` and MATCH fails with "no such column: Aware". The fix
/// is to tokenize on anything that is not a Unicode letter or digit — FTS5
/// metacharacters then never reach the expression — and to double-quote each
/// surviving token for good measure.

/// Split a user query into alphanumeric tokens (CJK counts as alphanumeric).
pub fn tokenize(query: &str) -> Vec<String> {
    query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect()
}

/// Build a MATCH expression of quoted prefix terms joined by `joiner`
/// (e.g. " OR " or " "). Returns "" for queries with no usable token;
/// callers must skip the MATCH clause in that case.
pub fn prefix_match_expr(query: &str, joiner: &str) -> String {
    tokenize(query)
        .into_iter()
        .map(|t| format!("\"{t}\"*"))
        .collect::<Vec<_>>()
        .join(joiner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hyphenated_words_become_separate_safe_terms() {
        assert_eq!(
            prefix_match_expr("Preference-Aware", " OR "),
            "\"Preference\"* OR \"Aware\"*"
        );
    }

    #[test]
    fn fts_metacharacters_are_stripped() {
        assert_eq!(
            prefix_match_expr("title:(test) \"quoted\"", " "),
            "\"title\"* \"test\"* \"quoted\"*"
        );
    }

    #[test]
    fn cjk_tokens_survive() {
        assert_eq!(prefix_match_expr("注意力 机制", " "), "\"注意力\"* \"机制\"*");
    }

    #[test]
    fn punctuation_only_query_yields_empty_expr() {
        assert_eq!(prefix_match_expr("--- :::", " OR "), "");
    }
}
