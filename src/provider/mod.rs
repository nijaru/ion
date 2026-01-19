//! LLM provider abstraction.
//!
//! This module provides a unified interface for interacting with various LLM backends
//! (OpenAI, Anthropic, Ollama, Groq, Google) using the battle-tested `llm` crate.
//!
//! # Example
//!
//! ```ignore
//! use ion::provider::{Backend, Client};
//!
//! let client = Client::from_backend(Backend::OpenAI)?;
//! let response = client.complete(request).await?;
//! ```

mod backend;
mod client;
mod error;
mod models_dev;
mod prefs;
mod registry;
mod types;

// Re-export the clean public API
pub use backend::{Backend, BackendStatus};
pub use client::{Client, LlmApi};
pub use error::Error;
pub use prefs::ProviderPrefs;
pub use registry::{ModelFilter, ModelRegistry};
pub use types::*;

/// Create a client for the given backend, auto-detecting API key from environment.
pub fn create(backend: Backend) -> Result<Client, Error> {
    Client::from_backend(backend)
}

// API provider enum for TUI selection (not for actual LLM calls)
mod api_provider;
pub use api_provider::{ApiProvider, ProviderStatus};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_client() {
        let client = Client::new(Backend::OpenAI, "test-key");
        assert_eq!(client.backend(), Backend::OpenAI);
    }

    #[test]
    fn test_backend_ids() {
        assert_eq!(Backend::OpenAI.id(), "openai");
        assert_eq!(Backend::Anthropic.id(), "anthropic");
        assert_eq!(Backend::Ollama.id(), "ollama");
        assert_eq!(Backend::Groq.id(), "groq");
        assert_eq!(Backend::Google.id(), "google");
    }

    #[test]
    fn test_ollama_no_key_needed() {
        let result = Client::from_backend(Backend::Ollama);
        assert!(result.is_ok());
    }
}
