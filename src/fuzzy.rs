//! Shared case-insensitive subsequence fuzzy matching, used by the target
//! chooser and the file search overlay.

/// True when every character of `needle` appears in `haystack` in order
/// (classic subsequence fuzzy match), ignoring ASCII case.
pub fn fuzzy_matches(haystack: &str, needle: &str) -> bool {
    let needle = needle.trim();
    if needle.is_empty() {
        return true;
    }
    let haystack = haystack.to_ascii_lowercase();
    let needle = needle.to_ascii_lowercase();
    let mut haystack_chars = haystack.chars();
    needle.chars().all(|needle_char| {
        haystack_chars
            .by_ref()
            .any(|haystack_char| haystack_char == needle_char)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_needle_matches_everything() {
        assert!(fuzzy_matches("src/app.rs", ""));
        assert!(fuzzy_matches("", "   "));
    }

    #[test]
    fn matches_in_order_subsequences_case_insensitively() {
        assert!(fuzzy_matches("src/tui/render.rs", "turd"));
        assert!(fuzzy_matches("README.md", "readme"));
        assert!(fuzzy_matches("src/App.rs", "APP"));
    }

    #[test]
    fn rejects_out_of_order_or_missing_characters() {
        assert!(!fuzzy_matches("src/app.rs", "ppa"));
        assert!(!fuzzy_matches("src/app.rs", "xyz"));
    }
}
