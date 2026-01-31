//! Provider error types.

use thiserror::Error;

/// Format an API error for display, extracting message from JSON if present.
///
/// Handles common patterns:
/// - `"HTTP 403: {"error": {"message": "..."}}"` → extracts message
/// - `"HTTP 429: {"error": {"message": "Rate limit"}}"` → extracts message
/// - Plain text errors → returns as-is
#[must_use]
pub fn format_api_error(error: &str) -> String {
    // Try to find JSON in the error message (after "HTTP XXX: " prefix)
    if let Some(json_start) = error.find('{') {
        let json_str = &error[json_start..];

        // Try to parse and extract meaningful message
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(json_str)
            && let Some(msg) = extract_error_message(&json)
        {
            // Preserve HTTP status prefix if present
            let prefix = &error[..json_start].trim();
            if prefix.is_empty() {
                return msg;
            }
            return format!("{prefix} {msg}");
        }
    }

    // No JSON or couldn't extract - return original
    error.to_string()
}

/// Extract user-friendly message from JSON error response.
fn extract_error_message(json: &serde_json::Value) -> Option<String> {
    // Common patterns:
    // {"error": {"message": "...", "code": "..."}}  (OpenAI, Anthropic)
    // {"error": {"message": "...", "status": "..."}} (Google)
    // {"message": "..."}
    // {"error": "..."}

    // Try nested error.message first
    if let Some(error_obj) = json.get("error") {
        if let Some(msg) = error_obj.get("message").and_then(|v| v.as_str()) {
            // Include code/status if present for context
            let mut result = msg.to_string();

            if let Some(code) = error_obj.get("code").and_then(|v| v.as_str()) {
                result = format!("{result} (code: {code})");
            } else if let Some(status) = error_obj.get("status").and_then(|v| v.as_str()) {
                result = format!("{result} (status: {status})");
            }

            return Some(result);
        }

        // error might be a string directly
        if let Some(msg) = error_obj.as_str() {
            return Some(msg.to_string());
        }
    }

    // Try top-level message
    if let Some(msg) = json.get("message").and_then(|v| v.as_str()) {
        return Some(msg.to_string());
    }

    None
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Missing API key for {backend}. Set one of: {}", env_vars.join(", "))]
    MissingApiKey {
        backend: String,
        env_vars: Vec<String>,
    },

    #[error("Failed to build LLM client: {0}")]
    Build(String),

    #[error("API error: {0}")]
    Api(String),

    #[error("Stream error: {0}")]
    Stream(String),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Rate limited, retry after {retry_after:?}s")]
    RateLimited { retry_after: Option<u64> },

    #[error("Context overflow: {used} > {limit}")]
    ContextOverflow { used: u32, limit: u32 },

    #[error("Cancelled")]
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_openai_error() {
        let error = r#"HTTP 429: {"error":{"message":"Rate limit exceeded","type":"rate_limit_error","code":"rate_limit_exceeded"}}"#;
        let formatted = format_api_error(error);
        assert_eq!(
            formatted,
            "HTTP 429: Rate limit exceeded (code: rate_limit_exceeded)"
        );
    }

    #[test]
    fn test_format_google_error() {
        let error = r#"HTTP 403: {"error":{"message":"Request had insufficient authentication scopes.","status":"PERMISSION_DENIED"}}"#;
        let formatted = format_api_error(error);
        assert_eq!(
            formatted,
            "HTTP 403: Request had insufficient authentication scopes. (status: PERMISSION_DENIED)"
        );
    }

    #[test]
    fn test_format_simple_error() {
        let error = r#"{"error":"Invalid API key"}"#;
        let formatted = format_api_error(error);
        assert_eq!(formatted, "Invalid API key");
    }

    #[test]
    fn test_format_top_level_message() {
        let error = r#"{"message":"Something went wrong"}"#;
        let formatted = format_api_error(error);
        assert_eq!(formatted, "Something went wrong");
    }

    #[test]
    fn test_format_plain_text() {
        let error = "Connection refused";
        let formatted = format_api_error(error);
        assert_eq!(formatted, "Connection refused");
    }

    #[test]
    fn test_format_unparseable_json() {
        let error = "HTTP 500: {invalid json}";
        let formatted = format_api_error(error);
        assert_eq!(formatted, "HTTP 500: {invalid json}");
    }
}
