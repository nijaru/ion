//! Provider error types.

use thiserror::Error;

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
