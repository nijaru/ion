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
