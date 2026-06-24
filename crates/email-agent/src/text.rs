//! Small text helpers shared across the email agent.

/// Truncate `s` to at most `max_bytes`, snapping the cut **down** to the nearest
/// UTF-8 char boundary so we never slice through a multi-byte character.
///
/// **Why**: `&s[..n]` panics with "byte index N is not a char boundary" whenever
/// `n` lands inside a multi-byte char — which is common in German text (umlauts
/// like `ü`, `ä`, `ö`, `ß`). A single such email previously crash-looped the whole
/// backend process (panic in the email-processor task aborted the binary, which
/// then restarted and re-fetched the same unread mail). Always go through this.
///
/// Returns the whole string unchanged when it is already within `max_bytes`.
pub(crate) fn truncate_on_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::truncate_on_char_boundary;

    #[test]
    fn shorter_than_max_is_unchanged() {
        assert_eq!(truncate_on_char_boundary("hallo", 300), "hallo");
    }

    #[test]
    fn ascii_truncates_exactly() {
        assert_eq!(truncate_on_char_boundary("abcdef", 3), "abc");
    }

    #[test]
    fn never_splits_a_multibyte_char() {
        // "Schränke": the 'ä' is two bytes. Cutting at a byte index inside it
        // must snap back to before the 'ä', never panic.
        let s = "Schränke";
        let a_index = s.find('ä').unwrap(); // byte offset of 'ä' (= 4)
        // Ask for a cut one byte into the 'ä' (a non-char-boundary index).
        let out = truncate_on_char_boundary(s, a_index + 1);
        assert_eq!(out, "Schr");
        assert!(s.starts_with(out));
    }

    #[test]
    fn reproduces_the_original_crash_index() {
        // Mirrors the production panic: a long German body cut at byte 300 that
        // landed inside a 'ü'. Must not panic and must return valid UTF-8.
        let body = "ä".repeat(200); // 400 bytes, boundaries on even indices
        let out = truncate_on_char_boundary(&body, 299); // odd → inside a char
        assert_eq!(out.len(), 298);
        assert!(out.chars().all(|c| c == 'ä'));
    }
}
