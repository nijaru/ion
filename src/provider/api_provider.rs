//! Unified LLM provider enum.
//!
//! Single source of truth for provider detection, configuration, and LLM backend mapping.

use crate::auth::{self, OAuthProvider};
use std::env;

/// Supported LLM providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provider {
    /// `OpenRouter` aggregator - access to many providers
    OpenRouter,
    /// Direct Anthropic API
    Anthropic,
    /// Direct `OpenAI` API
    OpenAI,
    /// Google AI Studio (Gemini)
    Google,
    /// Local Ollama instance
    Ollama,
    /// Groq cloud inference
    Groq,
    /// Moonshot AI Kimi
    Kimi,
    /// ChatGPT via OAuth (Plus/Pro subscription)
    ChatGpt,
    /// Gemini via OAuth (consumer subscription)
    Gemini,
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
        Provider::Kimi,
        Provider::ChatGpt,
        Provider::Gemini,
    ];

    /// Lowercase ID for config storage.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Provider::OpenRouter => "openrouter",
            Provider::Anthropic => "anthropic",
            Provider::OpenAI => "openai",
            Provider::Google => "google",
            Provider::Ollama => "ollama",
            Provider::Groq => "groq",
            Provider::Kimi => "kimi",
            Provider::ChatGpt => "chatgpt",
            Provider::Gemini => "gemini",
        }
    }

    /// Parse provider from ID string.
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        match id.to_lowercase().as_str() {
            "openrouter" => Some(Provider::OpenRouter),
            "anthropic" => Some(Provider::Anthropic),
            "openai" => Some(Provider::OpenAI),
            "google" => Some(Provider::Google),
            "ollama" => Some(Provider::Ollama),
            "groq" => Some(Provider::Groq),
            "kimi" | "moonshot" => Some(Provider::Kimi),
            "chatgpt" => Some(Provider::ChatGpt),
            "gemini" => Some(Provider::Gemini),
            _ => None,
        }
    }

    /// Display name for the provider.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Provider::OpenRouter => "OpenRouter",
            Provider::Anthropic => "Anthropic",
            Provider::OpenAI => "OpenAI",
            Provider::Google => "Google AI",
            Provider::Ollama => "Ollama",
            Provider::Groq => "Groq",
            Provider::Kimi => "Kimi",
            Provider::ChatGpt => "ChatGPT",
            Provider::Gemini => "Gemini",
        }
    }

    /// Short description of the provider.
    #[must_use]
    pub fn description(self) -> &'static str {
        match self {
            Provider::OpenRouter => "Aggregator with 200+ models",
            Provider::Anthropic => "Claude models directly",
            Provider::OpenAI => "GPT models directly",
            Provider::Google => "Gemini via AI Studio",
            Provider::Ollama => "Local models",
            Provider::Groq => "Fast inference",
            Provider::Kimi => "Moonshot K2 models",
            Provider::ChatGpt => "Sign in with ChatGPT",
            Provider::Gemini => "Sign in with Google",
        }
    }

    /// Environment variable(s) for API key.
    #[must_use]
    pub fn env_vars(self) -> &'static [&'static str] {
        match self {
            Provider::OpenRouter => &["OPENROUTER_API_KEY"],
            Provider::Anthropic => &["ANTHROPIC_API_KEY"],
            Provider::OpenAI => &["OPENAI_API_KEY"],
            Provider::Google => &["GOOGLE_API_KEY", "GEMINI_API_KEY"],
            Provider::Ollama => &[], // No key needed
            Provider::Groq => &["GROQ_API_KEY"],
            Provider::Kimi => &["MOONSHOT_API_KEY", "KIMI_API_KEY"],
            Provider::ChatGpt => &[], // OAuth only
            Provider::Gemini => &[],  // OAuth only
        }
    }

    /// Check if this is an OAuth-based provider.
    #[must_use]
    pub fn is_oauth(self) -> bool {
        matches!(self, Provider::ChatGpt | Provider::Gemini)
    }

    /// Get the corresponding OAuth provider, if any.
    #[must_use]
    pub fn oauth_provider(self) -> Option<OAuthProvider> {
        match self {
            Provider::ChatGpt => Some(OAuthProvider::OpenAI),
            Provider::Gemini => Some(OAuthProvider::Google),
            _ => None,
        }
    }

    /// Get API key from environment.
    #[must_use]
    pub fn api_key(self) -> Option<String> {
        // OAuth providers don't use env vars
        if self.is_oauth() {
            return None;
        }

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
    #[must_use]
    pub fn is_available(self) -> bool {
        // OAuth providers check OAuth credentials
        if let Some(oauth_provider) = self.oauth_provider() {
            return auth::is_logged_in(oauth_provider);
        }
        self.api_key().is_some()
    }

    /// Get authentication hint for display in provider picker.
    ///
    /// Returns the CLI command for OAuth providers, env var for API providers,
    /// or empty string for providers that don't need auth.
    #[must_use]
    pub fn auth_hint(self) -> &'static str {
        match self {
            // OAuth providers: CLI command
            Provider::ChatGpt => "ion login chatgpt",
            Provider::Gemini => "ion login gemini",
            // Ollama: no auth needed
            Provider::Ollama => "",
            // API providers: first env var
            Provider::OpenRouter => "OPENROUTER_API_KEY",
            Provider::Anthropic => "ANTHROPIC_API_KEY",
            Provider::OpenAI => "OPENAI_API_KEY",
            Provider::Google => "GOOGLE_API_KEY",
            Provider::Groq => "GROQ_API_KEY",
            Provider::Kimi => "MOONSHOT_API_KEY",
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
    #[must_use]
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
    #[must_use]
    pub fn available() -> Vec<Provider> {
        Provider::ALL
            .iter()
            .copied()
            .filter(|p| p.is_available())
            .collect()
    }

    /// Sort providers: authenticated first, then alphabetically within each group.
    #[must_use]
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

    #[test]
    fn test_oauth_providers() {
        assert!(Provider::ChatGpt.is_oauth());
        assert!(Provider::Gemini.is_oauth());
        assert!(!Provider::OpenAI.is_oauth());
        assert!(!Provider::Google.is_oauth());

        // OAuth providers have no env vars
        assert!(Provider::ChatGpt.env_vars().is_empty());
        assert!(Provider::Gemini.env_vars().is_empty());
    }

    #[test]
    fn test_oauth_provider_mapping() {
        assert_eq!(
            Provider::ChatGpt.oauth_provider(),
            Some(OAuthProvider::OpenAI)
        );
        assert_eq!(
            Provider::Gemini.oauth_provider(),
            Some(OAuthProvider::Google)
        );
        assert_eq!(Provider::OpenAI.oauth_provider(), None);
    }
}
