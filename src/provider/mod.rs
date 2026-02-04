//! LLM provider abstraction.
//!
//! This module provides a unified interface for interacting with various LLM providers
//! (`OpenAI`, Anthropic, Ollama, Groq, Google) using the `llm` crate.
//!
//! # Example
//!
//! ```ignore
//! use ion::provider::{Provider, Client};
//!
//! let client = Client::from_provider(Provider::OpenAI)?;
//! let response = client.complete(request).await?;
//! ```

mod anthropic;
mod api_provider;
mod client;
mod error;
mod gemini_oauth;
mod chatgpt_responses;
mod http;
mod models_dev;
mod openai_compat;
mod prefs;
mod registry;
mod types;

use std::time::Duration;

// Re-export the clean public API
pub use api_provider::{Provider, ProviderStatus};
pub use client::{Client, LlmApi};
pub use error::{Error, format_api_error};
pub use prefs::ProviderPrefs;
pub use registry::{ModelFilter, ModelRegistry};
pub use types::*;

/// Default timeout for HTTP requests.
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
/// Default connect timeout for HTTP requests.
pub const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Create an HTTP client with standard timeouts.
#[must_use]
pub fn create_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .connect_timeout(HTTP_CONNECT_TIMEOUT)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// Create a client for the given provider, auto-detecting API key from environment.
/// For OAuth providers, this will refresh expired tokens if possible.
pub async fn create(provider: Provider) -> Result<Client, Error> {
    Client::from_provider(provider).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_client() {
        let client = Client::new(Provider::OpenAI, "test-key").expect("failed to create client");
        assert_eq!(client.provider(), Provider::OpenAI);
    }

    #[test]
    fn test_provider_ids() {
        assert_eq!(Provider::OpenAI.id(), "openai");
        assert_eq!(Provider::Anthropic.id(), "anthropic");
        assert_eq!(Provider::Ollama.id(), "ollama");
        assert_eq!(Provider::Groq.id(), "groq");
        assert_eq!(Provider::Google.id(), "google");
    }

    #[tokio::test]
    async fn test_ollama_no_key_needed() {
        let result = Client::from_provider(Provider::Ollama).await;
        assert!(result.is_ok());
    }
}
