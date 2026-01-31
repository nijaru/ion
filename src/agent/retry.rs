/// Check if an error is retryable (transient network/server issues)
pub(crate) fn is_retryable_error(err: &str) -> bool {
    let err_lower = err.to_lowercase();

    // Rate limits
    if err.contains("429") || err_lower.contains("rate limit") {
        return true;
    }

    // Timeouts
    if err_lower.contains("timeout")
        || err_lower.contains("timed out")
        || err_lower.contains("deadline exceeded")
    {
        return true;
    }

    // Network errors
    if err_lower.contains("connection")
        || err_lower.contains("network")
        || err_lower.contains("dns")
        || err_lower.contains("resolve")
    {
        return true;
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
        return true;
    }

    false
}

/// Get a human-readable category for a retryable error
pub(crate) fn categorize_error(err: &str) -> &'static str {
    let err_lower = err.to_lowercase();

    if err.contains("429") || err_lower.contains("rate limit") {
        return "Rate limited";
    }

    if err_lower.contains("timeout")
        || err_lower.contains("timed out")
        || err_lower.contains("deadline exceeded")
    {
        return "Request timed out";
    }

    if err_lower.contains("connection")
        || err_lower.contains("network")
        || err_lower.contains("dns")
        || err_lower.contains("resolve")
    {
        return "Network error";
    }

    if err.contains("500")
        || err.contains("502")
        || err.contains("503")
        || err.contains("504")
        || err_lower.contains("server error")
        || err_lower.contains("internal error")
        || err_lower.contains("service unavailable")
        || err_lower.contains("bad gateway")
    {
        return "Server error";
    }

    "Transient error"
}
