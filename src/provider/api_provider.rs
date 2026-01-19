//! API Provider detection and management.
//!
//! Handles detection of available API providers based on environment variables
//! and future OAuth tokens.

use std::env;

/// Supported API providers (backends, not model providers within OpenRouter).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ApiProvider {
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

impl ApiProvider {
    /// All implemented API providers.
    pub const ALL: &'static [ApiProvider] = &[
        ApiProvider::OpenRouter,
        ApiProvider::Anthropic,
        ApiProvider::OpenAI,
        ApiProvider::Google,
        ApiProvider::Ollama,
        ApiProvider::Groq,
    ];

    /// Lowercase ID for config storage and model string prefixes.
    pub fn id(&self) -> &'static str {
        match self {
            ApiProvider::OpenRouter => "openrouter",
            ApiProvider::Anthropic => "anthropic",
            ApiProvider::OpenAI => "openai",
            ApiProvider::Google => "google",
            ApiProvider::Ollama => "ollama",
            ApiProvider::Groq => "groq",
        }
    }

    /// Parse provider from ID string.
    pub fn from_id(id: &str) -> Option<Self> {
        match id.to_lowercase().as_str() {
            "openrouter" => Some(ApiProvider::OpenRouter),
            "anthropic" => Some(ApiProvider::Anthropic),
            "openai" => Some(ApiProvider::OpenAI),
            "google" => Some(ApiProvider::Google),
            "ollama" => Some(ApiProvider::Ollama),
            "groq" => Some(ApiProvider::Groq),
            _ => None,
        }
    }

    /// Display name for the provider.
    pub fn name(&self) -> &'static str {
        match self {
            ApiProvider::OpenRouter => "OpenRouter",
            ApiProvider::Anthropic => "Anthropic",
            ApiProvider::OpenAI => "OpenAI",
            ApiProvider::Google => "Google AI",
            ApiProvider::Ollama => "Ollama",
            ApiProvider::Groq => "Groq",
        }
    }

    /// Short description of the provider.
    pub fn description(&self) -> &'static str {
        match self {
            ApiProvider::OpenRouter => "Aggregator with 200+ models",
            ApiProvider::Anthropic => "Claude models directly",
            ApiProvider::OpenAI => "GPT models directly",
            ApiProvider::Google => "Gemini via AI Studio",
            ApiProvider::Ollama => "Local models",
            ApiProvider::Groq => "Fast inference",
        }
    }

    /// Environment variable(s) that indicate authentication.
    pub fn env_vars(&self) -> &'static [&'static str] {
        match self {
            ApiProvider::OpenRouter => &["OPENROUTER_API_KEY"],
            ApiProvider::Anthropic => &["ANTHROPIC_API_KEY"],
            ApiProvider::OpenAI => &["OPENAI_API_KEY"],
            ApiProvider::Google => &["GOOGLE_API_KEY", "GEMINI_API_KEY"],
            ApiProvider::Ollama => &["OLLAMA_HOST"], // Ollama doesn't need auth, but host can be configured
            ApiProvider::Groq => &["GROQ_API_KEY"],
        }
    }

    /// Check if the provider is authenticated (has required env vars).
    pub fn is_authenticated(&self) -> bool {
        // Ollama is special - it's "authenticated" if reachable (no key needed)
        if *self == ApiProvider::Ollama {
            // For now, assume Ollama is available if OLLAMA_HOST is set or default localhost
            return env::var("OLLAMA_HOST").is_ok() || Self::ollama_default_available();
        }

        self.env_vars()
            .iter()
            .any(|var| env::var(var).map(|v| !v.is_empty()).unwrap_or(false))
    }

    /// Get the API key if authenticated.
    pub fn api_key(&self) -> Option<String> {
        for var in self.env_vars() {
            if let Ok(key) = env::var(var) {
                if !key.is_empty() {
                    return Some(key);
                }
            }
        }
        None
    }

    /// Check if default Ollama is likely available (localhost:11434).
    fn ollama_default_available() -> bool {
        // Quick check - just see if the env suggests local dev environment
        // Real check would need async HTTP call
        cfg!(debug_assertions) // Assume available in debug builds
    }

    /// Whether this provider requires OAuth (vs just API key).
    pub fn requires_oauth(&self) -> bool {
        // Future: some providers may support OAuth for subscription access
        false
    }

    /// Convert to Backend enum for LLM operations.
    pub fn to_backend(self) -> super::Backend {
        match self {
            ApiProvider::OpenRouter => super::Backend::OpenRouter,
            ApiProvider::Anthropic => super::Backend::Anthropic,
            ApiProvider::OpenAI => super::Backend::OpenAI,
            ApiProvider::Ollama => super::Backend::Ollama,
            ApiProvider::Groq => super::Backend::Groq,
            ApiProvider::Google => super::Backend::Google,
        }
    }
}

/// Information about detected API providers.
#[derive(Debug, Clone)]
pub struct ProviderStatus {
    pub provider: ApiProvider,
    pub authenticated: bool,
}

impl ProviderStatus {
    /// Get status for all providers.
    pub fn detect_all() -> Vec<ProviderStatus> {
        ApiProvider::ALL
            .iter()
            .map(|&provider| ProviderStatus {
                provider,
                authenticated: provider.is_authenticated(),
            })
            .collect()
    }

    /// Get only authenticated providers.
    pub fn available() -> Vec<ProviderStatus> {
        Self::detect_all()
            .into_iter()
            .filter(|s| s.authenticated)
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
        for provider in ApiProvider::ALL {
            assert!(!provider.name().is_empty());
            assert!(!provider.description().is_empty());
        }
    }

    #[test]
    fn test_env_vars_not_empty() {
        for provider in ApiProvider::ALL {
            // Ollama is special case, others need at least one env var
            if *provider != ApiProvider::Ollama {
                assert!(!provider.env_vars().is_empty());
            }
        }
    }

    #[test]
    fn test_detect_all_returns_all_providers() {
        let statuses = ProviderStatus::detect_all();
        assert_eq!(statuses.len(), ApiProvider::ALL.len());
    }

    #[test]
    fn test_sorting_prioritizes_authenticated() {
        let statuses = vec![
            ProviderStatus {
                provider: ApiProvider::Groq,
                authenticated: false,
            },
            ProviderStatus {
                provider: ApiProvider::OpenRouter,
                authenticated: true,
            },
            ProviderStatus {
                provider: ApiProvider::Anthropic,
                authenticated: false,
            },
        ];

        let sorted = ProviderStatus::sorted(statuses);
        assert_eq!(sorted[0].provider, ApiProvider::OpenRouter); // Authenticated first
        assert!(!sorted[1].authenticated); // Then not authenticated
        assert!(!sorted[2].authenticated);
    }
}
