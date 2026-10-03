use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderErrorKind {
    Authentication,
    Permission,
    InvalidRequest,
    /// The adapter rejected saved opaque replay before dispatch because its
    /// producing context no longer matches this request.
    ReplayContextChanged,
    ContextLength,
    RateLimited,
    Quota,
    Unsupported,
    Safety,
    Transport,
    Timeout,
    Overloaded,
    Server,
    Cancelled,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Error)]
#[error("{kind:?}: {message}")]
pub struct ProviderError {
    pub kind: ProviderErrorKind,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
}
