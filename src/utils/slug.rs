/// Convert an arbitrary human-readable string into a URL-safe slug.
///
/// Rules:
/// - Lowercase everything.
/// - Keep ASCII alphanumerics as-is.
/// - Replace every other run of characters with a single `-`.
/// - Strip leading/trailing `-`.
///
/// Examples:
/// - `"Hello World!"` → `"hello-world"`
/// - `"  Foo  --  Bar "` → `"foo-bar"`
/// - `"already-slugified"` → `"already-slugified"`
///
/// Note: this is intentionally simple and ASCII-only. Non-ASCII characters
/// (e.g., Chinese, accented Latin) are collapsed into separators, which works
/// for our callers because they always allow the client to supply an explicit
/// slug when the default is unsuitable.
pub fn slugify(input: &str) -> String {
    input
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::slugify;

    #[test]
    fn basic() {
        assert_eq!(slugify("Hello World"), "hello-world");
        assert_eq!(slugify("Already-Slugified"), "already-slugified");
        assert_eq!(slugify("  multiple  spaces  "), "multiple-spaces");
        assert_eq!(slugify("Symbols!@#$%^&*()"), "symbols");
        assert_eq!(slugify("Mixed_Case 123"), "mixed-case-123");
    }
}
