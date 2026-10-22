//! Provider error identity is independent of bounded diagnostic presentation.
use super::{ProviderError, ProviderErrorKind, Value};

fn provider_code(value: &Value) -> Option<&str> {
    ["/error/code", "/error/type", "/code", "/type"]
        .into_iter()
        .filter_map(|path| value.pointer(path).and_then(Value::as_str))
        .find(|code| !code.trim().is_empty())
}

fn provider_text(value: &Value) -> [Option<&str>; 2] {
    let summary = value
        .pointer("/error/message")
        .and_then(Value::as_str)
        .or_else(|| value.get("message").and_then(Value::as_str))
        .or_else(|| value.pointer("/error/code").and_then(Value::as_str));
    // Routers can wrap the actionable upstream reason in metadata while
    // supplying only a generic message. Never interpret arbitrary metadata.
    let upstream = value
        .pointer("/error/metadata/raw")
        .or_else(|| value.pointer("/metadata/raw"))
        .and_then(Value::as_str)
        .filter(|raw| !raw.trim().is_empty() && Some(*raw) != summary);
    [summary, upstream]
}

fn is_context_error(value: &Value) -> bool {
    matches!(
        provider_code(value),
        Some(
            "context_length_exceeded"
                | "model_context_window_exceeded"
                | "context_window_exceeded"
                | "request_too_large"
        )
    ) || provider_text(value).into_iter().flatten().any(|message| {
        let lower = message.to_ascii_lowercase();
        [
            "context length",
            "context limit",
            "context window",
            "prompt is too long",
            "prompt too long",
            "available context size",
            "exceeds the maximum allowed input length of",
            "request_too_large",
        ]
        .iter()
        .any(|part| lower.contains(part))
    })
}

pub(super) fn classify_status_error(status: u16, value: Option<&Value>) -> ProviderErrorKind {
    if status == 429
        && value.is_some_and(|value| {
            matches!(
                provider_code(value),
                Some("insufficient_quota" | "billing_hard_limit_reached")
            )
        })
    {
        return ProviderErrorKind::Quota;
    }
    // Inspect the complete admitted error text, not its 500-character display
    // prefix. HTTP acquisition and SSE framing bound the input independently.
    if matches!(status, 400 | 413) && value.is_some_and(is_context_error) {
        return ProviderErrorKind::ContextLength;
    }
    match status {
        401 => ProviderErrorKind::Authentication,
        403 => ProviderErrorKind::Permission,
        408 => ProviderErrorKind::Timeout,
        429 => ProviderErrorKind::RateLimited,
        503 | 529 => ProviderErrorKind::Overloaded,
        500..=599 => ProviderErrorKind::Server,
        _ => ProviderErrorKind::InvalidRequest,
    }
}

pub(super) fn provider_error_detail_value(value: &Value) -> Option<String> {
    let [summary, upstream] = provider_text(value);
    let separator = if summary.is_some() && upstream.is_some() {
        ": "
    } else {
        ""
    };
    let detail: String = summary
        .unwrap_or("")
        .chars()
        .chain(separator.chars())
        .chain(upstream.unwrap_or("").chars())
        .filter(|ch| !ch.is_control())
        .take(500)
        .collect();
    (!detail.is_empty()).then_some(detail)
}

pub(super) fn chat_stream_error(value: &Value) -> ProviderError {
    let kind = if let Some(status) = value
        .pointer("/error/code")
        .and_then(Value::as_u64)
        .and_then(|code| u16::try_from(code).ok())
    {
        classify_status_error(status, Some(value))
    } else {
        match provider_code(value) {
            Some("server_error") => ProviderErrorKind::Server,
            Some("rate_limit_exceeded" | "rate_limited") => ProviderErrorKind::RateLimited,
            Some("insufficient_quota" | "billing_hard_limit_reached") => ProviderErrorKind::Quota,
            Some("invalid_request_error") => classify_status_error(400, Some(value)),
            None if is_context_error(value) => ProviderErrorKind::ContextLength,
            Some(
                "context_length_exceeded"
                | "model_context_window_exceeded"
                | "context_window_exceeded"
                | "request_too_large",
            ) => ProviderErrorKind::ContextLength,
            _ => ProviderErrorKind::Transport,
        }
    };
    let message = provider_error_detail_value(value).map_or_else(
        || "provider sent a stream error".to_owned(),
        |detail| format!("provider stream error: {detail}"),
    );
    ProviderError {
        kind,
        message,
        retry_after_ms: None,
    }
}

pub(super) fn anthropic_sse_error(value: &Value) -> ProviderError {
    let kind = match value.pointer("/error/type").and_then(Value::as_str) {
        Some("authentication_error") => ProviderErrorKind::Authentication,
        Some("permission_error") => ProviderErrorKind::Permission,
        Some("rate_limit_error") => ProviderErrorKind::RateLimited,
        Some("overloaded_error") => ProviderErrorKind::Overloaded,
        Some("timeout_error") => ProviderErrorKind::Timeout,
        Some("api_error") => ProviderErrorKind::Server,
        _ if is_context_error(value) => ProviderErrorKind::ContextLength,
        Some("invalid_request_error") => ProviderErrorKind::InvalidRequest,
        _ => ProviderErrorKind::Transport,
    };
    let message = provider_error_detail_value(value).map_or_else(
        || "provider sent an SSE error".into(),
        |detail| format!("provider sent an SSE error: {detail}"),
    );
    ProviderError {
        kind,
        message,
        retry_after_ms: None,
    }
}
