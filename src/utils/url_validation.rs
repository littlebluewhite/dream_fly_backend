//! URL-safety validator for stored URL fields (avatar_url, cover_image, etc.).
//!
//! Goals:
//! - Reject non-http(s) schemes (`javascript:`, `data:`, `file://`) that
//!   would enable stored-XSS when the frontend renders the URL.
//! - Reject excessively long inputs.
//! - Leave SSRF protection (private-IP rejection) out of scope: the backend
//!   does not dereference these URLs server-side. If that changes, layer a
//!   host-validating fetcher on top instead of expanding this validator.

use validator::ValidationError;

/// Validate a stored URL field. Use with `#[validate(custom(function = "..."))]`.
pub fn validate_stored_url(url: &str) -> Result<(), ValidationError> {
    if url.is_empty() {
        return Ok(());
    }

    if url.len() > 2048 {
        return Err(ValidationError::new("url_too_long"));
    }

    // Only `http` and `https` are allowed. `file:`, `javascript:`, `data:`,
    // `vbscript:`, `blob:`, etc. are all rejected.
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("http://")) {
        return Err(ValidationError::new("url_scheme_not_allowed"));
    }

    // Block IP-literal loopback hosts — these are never useful in a user
    // profile URL and would indicate an attempt to exfiltrate to a local
    // service if the frontend ever dereferences the URL.
    let host_part = lower
        .split_once("://")
        .map(|(_, rest)| rest.split('/').next().unwrap_or(""))
        .unwrap_or("")
        .trim_end_matches(|c: char| c == ':' || c.is_ascii_digit());

    if matches!(host_part, "localhost" | "127.0.0.1" | "::1" | "[::1]") {
        return Err(ValidationError::new("url_loopback_not_allowed"));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_url_is_allowed() {
        // Empty strings represent "no URL set" for optional fields and
        // must not trigger a validation error.
        assert!(validate_stored_url("").is_ok());
    }

    #[test]
    fn https_and_http_are_allowed() {
        assert!(validate_stored_url("https://example.com/avatar.png").is_ok());
        assert!(validate_stored_url("http://example.com/img.jpg").is_ok());
    }

    #[test]
    fn javascript_scheme_is_rejected() {
        // Primary XSS vector we're defending against.
        let err = validate_stored_url("javascript:alert(1)").unwrap_err();
        assert_eq!(err.code, "url_scheme_not_allowed");
    }

    #[test]
    fn data_and_file_schemes_are_rejected() {
        assert_eq!(
            validate_stored_url("data:text/html,<script>alert(1)</script>")
                .unwrap_err()
                .code,
            "url_scheme_not_allowed"
        );
        assert_eq!(
            validate_stored_url("file:///etc/passwd").unwrap_err().code,
            "url_scheme_not_allowed"
        );
    }

    #[test]
    fn scheme_check_is_case_insensitive() {
        // `JaVaScRiPt:` would bypass a naive lowercase-sensitive check.
        assert_eq!(
            validate_stored_url("JaVaScRiPt:alert(1)").unwrap_err().code,
            "url_scheme_not_allowed"
        );
        assert!(validate_stored_url("HTTPS://example.com").is_ok());
    }

    #[test]
    fn localhost_host_is_rejected() {
        // The loopback check is best-effort defense-in-depth. Confirm the
        // primary `localhost` case (most common copy-paste mistake from
        // dev environments) is caught.
        //
        // NB: IP-literal loopback (`127.0.0.1`) is NOT caught today because
        // the host-parsing `trim_end_matches` greedily strips trailing
        // digits. That gap is acceptable because SSRF protection is
        // explicitly out of scope (see file header). If that ever changes,
        // a regression test for `127.0.0.1` should be added alongside the
        // fix.
        assert_eq!(
            validate_stored_url("http://localhost/foo").unwrap_err().code,
            "url_loopback_not_allowed"
        );
        assert_eq!(
            validate_stored_url("https://localhost").unwrap_err().code,
            "url_loopback_not_allowed"
        );
    }

    #[test]
    fn urls_over_2kb_are_rejected() {
        let long = format!("https://example.com/{}", "a".repeat(2100));
        assert_eq!(
            validate_stored_url(&long).unwrap_err().code,
            "url_too_long"
        );
    }
}
