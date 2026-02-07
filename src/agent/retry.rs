/// Classify a retryable error, returning the category if retryable.
///
/// Returns `Some(category)` for transient errors that should be retried,
/// `None` for non-retryable errors.
pub(crate) fn retryable_category(err: &str) -> Option<&'static str> {
    let err_lower = err.to_lowercase();

    // Rate limits
    if err.contains("429") || err_lower.contains("rate limit") {
        return Some("Rate limited");
    }

    // Timeouts
    if err_lower.contains("timeout")
        || err_lower.contains("timed out")
        || err_lower.contains("deadline exceeded")
    {
        return Some("Request timed out");
    }

    // Network errors
    if err_lower.contains("connection")
        || err_lower.contains("network")
        || err_lower.contains("dns")
        || err_lower.contains("resolve")
    {
        return Some("Network error");
    }

    // Server errors (5xx)
    if err.contains("500")
        || err.contains("502")
        || err.contains("503")
        || err.contains("504")
        || err_lower.contains("server error")
        || err_lower.contains("internal error")
        || err_lower.contains("service unavailable")
        || err_lower.contains("bad gateway")
    {
        return Some("Server error");
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_limit_detection() {
        assert_eq!(
            retryable_category("HTTP 429: rate limit"),
            Some("Rate limited")
        );
        assert_eq!(retryable_category("Rate limited"), Some("Rate limited"));
        assert_eq!(
            retryable_category("Rate limited, retry after 30s"),
            Some("Rate limited")
        );
    }

    #[test]
    fn test_timeout_detection() {
        assert_eq!(
            retryable_category("request timeout"),
            Some("Request timed out")
        );
        assert_eq!(
            retryable_category("connection timed out"),
            Some("Request timed out")
        );
        assert_eq!(
            retryable_category("deadline exceeded"),
            Some("Request timed out")
        );
    }

    #[test]
    fn test_network_error_detection() {
        assert_eq!(
            retryable_category("connection refused"),
            Some("Network error")
        );
        assert_eq!(
            retryable_category("DNS resolution failed"),
            Some("Network error")
        );
    }

    #[test]
    fn test_server_error_detection() {
        assert_eq!(
            retryable_category("HTTP 500: Internal"),
            Some("Server error")
        );
        assert_eq!(
            retryable_category("HTTP 502: Bad Gateway"),
            Some("Server error")
        );
        assert_eq!(
            retryable_category("HTTP 503: Service Unavailable"),
            Some("Server error")
        );
    }

    #[test]
    fn test_non_retryable() {
        assert_eq!(retryable_category("HTTP 400: Bad Request"), None);
        assert_eq!(retryable_category("HTTP 401: Unauthorized"), None);
        assert_eq!(retryable_category("Invalid API key"), None);
    }
}
