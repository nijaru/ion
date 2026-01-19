//! LLM provider abstraction.
//!
//! This module provides a unified interface for interacting with various LLM providers
//! (OpenAI, Anthropic, Ollama, Groq, Google) using the `llm` crate.
//!
//! # Example
//!
//! ```ignore
//! use ion::provider::{Provider, Client};
//!
//! let client = Client::from_provider(Provider::OpenAI)?;
//! let response = client.complete(request).await?;
//! ```

mod api_provider;
mod client;
mod error;
mod models_dev;
mod prefs;
mod registry;
mod types;

// Re-export the clean public API
pub use api_provider::{Provider, ProviderStatus};
pub use client::{Client, LlmApi};
pub use error::Error;
pub use prefs::ProviderPrefs;
pub use registry::{ModelFilter, ModelRegistry};
pub use types::*;

/// Create a client for the given provider, auto-detecting API key from environment.
pub fn create(provider: Provider) -> Result<Client, Error> {
    Client::from_provider(provider)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_client() {
        let client = Client::new(Provider::OpenAI, "test-key");
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

    #[test]
    fn test_ollama_no_key_needed() {
        let result = Client::from_provider(Provider::Ollama);
        assert!(result.is_ok());
    }
}
