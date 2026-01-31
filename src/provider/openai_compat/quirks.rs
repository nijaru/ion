//! Provider-specific quirks for OpenAI-compatible APIs.
//!
//! Different providers have subtle differences in their API compatibility.

use crate::provider::api_provider::Provider;

/// How providers handle reasoning/thinking content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ReasoningField {
    /// No reasoning support.
    None,
    /// Uses `reasoning_content` field in delta (`DeepSeek`, Kimi).
    ReasoningContent,
    /// Uses `reasoning` field in message (some custom deployments).
    Reasoning,
}

/// Provider-specific quirks for OpenAI-compatible APIs.
#[derive(Debug, Clone)]
#[allow(dead_code, clippy::struct_excessive_bools)]
pub struct ProviderQuirks {
    /// Use `max_tokens` instead of `max_completion_tokens`.
    pub use_max_tokens: bool,
    /// Skip the `store` field (causes errors on some providers).
    pub skip_store: bool,
    /// Skip the `developer` role (use `system` instead).
    pub skip_developer_role: bool,
    /// How reasoning/thinking is returned.
    pub reasoning_field: ReasoningField,
    /// Supports `provider` field for routing (`OpenRouter`).
    pub supports_provider_routing: bool,
    /// Base URL for the provider.
    pub base_url: &'static str,
    /// Auth header name (most use Authorization: Bearer, some use custom).
    pub auth_header: Option<&'static str>,
}

impl ProviderQuirks {
    /// Get quirks for a specific provider.
    pub fn for_provider(provider: Provider) -> Self {
        match provider {
            Provider::OpenRouter => Self::openrouter(),
            Provider::Groq => Self::groq(),
            Provider::Kimi => Self::kimi(),
            Provider::Ollama => Self::ollama(),
            // OpenAI-compatible or fallback for non-compatible providers
            Provider::OpenAI
            | Provider::ChatGpt
            | Provider::Anthropic
            | Provider::Google
            | Provider::Gemini => Self::openai(),
        }
    }

    /// `OpenAI` (standard behavior).
    fn openai() -> Self {
        Self {
            use_max_tokens: false,
            skip_store: false,
            skip_developer_role: false,
            reasoning_field: ReasoningField::None,
            supports_provider_routing: false,
            base_url: "https://api.openai.com/v1",
            auth_header: None, // Standard Bearer auth
        }
    }

    /// `OpenRouter` (supports provider routing).
    fn openrouter() -> Self {
        Self {
            use_max_tokens: false,
            skip_store: false,
            skip_developer_role: false,
            reasoning_field: ReasoningField::ReasoningContent,
            supports_provider_routing: true,
            base_url: "https://openrouter.ai/api/v1",
            auth_header: None,
        }
    }

    /// Groq (fast inference).
    fn groq() -> Self {
        Self {
            use_max_tokens: true,
            skip_store: true,
            skip_developer_role: true,
            reasoning_field: ReasoningField::None,
            supports_provider_routing: false,
            base_url: "https://api.groq.com/openai/v1",
            auth_header: None,
        }
    }

    /// Kimi/Moonshot (Chinese LLM with reasoning).
    fn kimi() -> Self {
        Self {
            use_max_tokens: true,
            skip_store: true,
            skip_developer_role: false,
            reasoning_field: ReasoningField::ReasoningContent,
            supports_provider_routing: false,
            base_url: "https://api.moonshot.ai/v1",
            auth_header: None,
        }
    }

    /// Ollama (local inference).
    fn ollama() -> Self {
        Self {
            use_max_tokens: true,
            skip_store: true,
            skip_developer_role: true,
            reasoning_field: ReasoningField::None,
            supports_provider_routing: false,
            base_url: "http://localhost:11434/v1",
            auth_header: None, // No auth needed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_openai_quirks() {
        let quirks = ProviderQuirks::for_provider(Provider::OpenAI);
        assert!(!quirks.use_max_tokens);
        assert!(!quirks.skip_store);
        assert!(!quirks.supports_provider_routing);
    }

    #[test]
    fn test_openrouter_quirks() {
        let quirks = ProviderQuirks::for_provider(Provider::OpenRouter);
        assert!(quirks.supports_provider_routing);
        assert_eq!(quirks.reasoning_field, ReasoningField::ReasoningContent);
    }

    #[test]
    fn test_groq_quirks() {
        let quirks = ProviderQuirks::for_provider(Provider::Groq);
        assert!(quirks.use_max_tokens);
        assert!(quirks.skip_store);
        assert!(quirks.skip_developer_role);
    }

    #[test]
    fn test_kimi_quirks() {
        let quirks = ProviderQuirks::for_provider(Provider::Kimi);
        assert!(quirks.use_max_tokens);
        assert_eq!(quirks.reasoning_field, ReasoningField::ReasoningContent);
    }
}
