//! Unified LLM provider enum.
//!
//! Single source of truth for provider detection, configuration, and LLM backend mapping.

use std::env;

/// Supported LLM providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provider {
    /// OpenRouter aggregator - access to many providers
    OpenRouter,
    /// Direct Anthropic API
    Anthropic,
    /// Direct OpenAI API
    OpenAI,
    /// Google AI Studio (Gemini)
    Google,
    /// Local Ollama instance
    Ollama,
    /// Groq cloud inference
    Groq,
}

impl Provider {
    /// All implemented providers.
    pub const ALL: &'static [Provider] = &[
        Provider::OpenRouter,
        Provider::Anthropic,
        Provider::OpenAI,
        Provider::Google,
        Provider::Ollama,
        Provider::Groq,
    ];

    /// Lowercase ID for config storage.
    pub fn id(self) -> &'static str {
        match self {
            Provider::OpenRouter => "openrouter",
            Provider::Anthropic => "anthropic",
            Provider::OpenAI => "openai",
            Provider::Google => "google",
            Provider::Ollama => "ollama",
            Provider::Groq => "groq",
        }
    }

    /// Parse provider from ID string.
    pub fn from_id(id: &str) -> Option<Self> {
        match id.to_lowercase().as_str() {
            "openrouter" => Some(Provider::OpenRouter),
            "anthropic" => Some(Provider::Anthropic),
            "openai" => Some(Provider::OpenAI),
            "google" => Some(Provider::Google),
            "ollama" => Some(Provider::Ollama),
            "groq" => Some(Provider::Groq),
            _ => None,
        }
    }

    /// Display name for the provider.
    pub fn name(self) -> &'static str {
        match self {
            Provider::OpenRouter => "OpenRouter",
            Provider::Anthropic => "Anthropic",
            Provider::OpenAI => "OpenAI",
            Provider::Google => "Google AI",
            Provider::Ollama => "Ollama",
            Provider::Groq => "Groq",
        }
    }

    /// Short description of the provider.
    pub fn description(self) -> &'static str {
        match self {
            Provider::OpenRouter => "Aggregator with 200+ models",
            Provider::Anthropic => "Claude models directly",
            Provider::OpenAI => "GPT models directly",
            Provider::Google => "Gemini via AI Studio",
            Provider::Ollama => "Local models",
            Provider::Groq => "Fast inference",
        }
    }

    /// Environment variable(s) for API key.
    pub fn env_vars(self) -> &'static [&'static str] {
        match self {
            Provider::OpenRouter => &["OPENROUTER_API_KEY"],
            Provider::Anthropic => &["ANTHROPIC_API_KEY"],
            Provider::OpenAI => &["OPENAI_API_KEY"],
            Provider::Google => &["GOOGLE_API_KEY", "GEMINI_API_KEY"],
            Provider::Ollama => &[], // No key needed
            Provider::Groq => &["GROQ_API_KEY"],
        }
    }

    /// Get API key from environment.
    pub fn api_key(self) -> Option<String> {
        for var in self.env_vars() {
            if let Ok(key) = env::var(var)
                && !key.is_empty()
            {
                return Some(key);
            }
        }
        // Ollama doesn't need a key
        if self == Provider::Ollama {
            return Some(String::new());
        }
        None
    }

    /// Check if this provider is available (has credentials or doesn't need them).
    pub fn is_available(self) -> bool {
        self.api_key().is_some()
    }

    /// Convert to llm crate backend.
    pub(crate) fn to_llm(self) -> llm::builder::LLMBackend {
        match self {
            Provider::OpenRouter => llm::builder::LLMBackend::OpenRouter,
            Provider::Anthropic => llm::builder::LLMBackend::Anthropic,
            Provider::OpenAI => llm::builder::LLMBackend::OpenAI,
            Provider::Ollama => llm::builder::LLMBackend::Ollama,
            Provider::Groq => llm::builder::LLMBackend::Groq,
            Provider::Google => llm::builder::LLMBackend::Google,
        }
    }
}

/// Provider with availability status.
#[derive(Debug, Clone)]
pub struct ProviderStatus {
    pub provider: Provider,
    pub authenticated: bool,
}

impl ProviderStatus {
    /// Detect all providers and their availability.
    pub fn detect_all() -> Vec<ProviderStatus> {
        Provider::ALL
            .iter()
            .map(|&provider| ProviderStatus {
                provider,
                authenticated: provider.is_available(),
            })
            .collect()
    }

    /// Get only available providers.
    pub fn available() -> Vec<Provider> {
        Provider::ALL
            .iter()
            .copied()
            .filter(|p| p.is_available())
            .collect()
    }

    /// Sort providers: authenticated first, then alphabetically within each group.
    pub fn sorted(mut statuses: Vec<ProviderStatus>) -> Vec<ProviderStatus> {
        statuses.sort_by(|a, b| {
            // Primary: authenticated first
            match (a.authenticated, b.authenticated) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                // Secondary: alphabetical by name
                _ => a.provider.name().cmp(b.provider.name()),
            }
        });
        statuses
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_providers_have_names() {
        for provider in Provider::ALL {
            assert!(!provider.name().is_empty());
            assert!(!provider.id().is_empty());
            assert!(!provider.description().is_empty());
        }
    }

    #[test]
    fn test_ollama_always_available() {
        // Ollama doesn't need an API key
        assert!(Provider::Ollama.api_key().is_some());
        assert!(Provider::Ollama.is_available());
    }

    #[test]
    fn test_from_id_roundtrip() {
        for provider in Provider::ALL {
            let id = provider.id();
            let parsed = Provider::from_id(id);
            assert_eq!(parsed, Some(*provider));
        }
    }

    #[test]
    fn test_detect_all_returns_all_providers() {
        let statuses = ProviderStatus::detect_all();
        assert_eq!(statuses.len(), Provider::ALL.len());
    }

    #[test]
    fn test_sorting_prioritizes_authenticated() {
        let statuses = vec![
            ProviderStatus {
                provider: Provider::Groq,
                authenticated: false,
            },
            ProviderStatus {
                provider: Provider::OpenRouter,
                authenticated: true,
            },
            ProviderStatus {
                provider: Provider::Anthropic,
                authenticated: false,
            },
        ];

        let sorted = ProviderStatus::sorted(statuses);
        assert_eq!(sorted[0].provider, Provider::OpenRouter); // Authenticated first
        assert!(!sorted[1].authenticated);
        assert!(!sorted[2].authenticated);
    }
}
