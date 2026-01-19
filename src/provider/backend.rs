//! LLM backend definitions and metadata.

use std::env;

/// Supported LLM backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Backend {
    OpenRouter,
    Anthropic,
    OpenAI,
    Ollama,
    Groq,
    Google,
}

impl Backend {
    /// All available backends.
    pub const ALL: &'static [Backend] = &[
        Backend::OpenRouter,
        Backend::Anthropic,
        Backend::OpenAI,
        Backend::Ollama,
        Backend::Groq,
        Backend::Google,
    ];

    /// Display name.
    pub fn name(self) -> &'static str {
        match self {
            Backend::OpenRouter => "OpenRouter",
            Backend::Anthropic => "Anthropic",
            Backend::OpenAI => "OpenAI",
            Backend::Ollama => "Ollama",
            Backend::Groq => "Groq",
            Backend::Google => "Google",
        }
    }

    /// Short identifier.
    pub fn id(self) -> &'static str {
        match self {
            Backend::OpenRouter => "openrouter",
            Backend::Anthropic => "anthropic",
            Backend::OpenAI => "openai",
            Backend::Ollama => "ollama",
            Backend::Groq => "groq",
            Backend::Google => "google",
        }
    }

    /// Environment variables for API keys.
    pub fn env_vars(self) -> &'static [&'static str] {
        match self {
            Backend::OpenRouter => &["OPENROUTER_API_KEY"],
            Backend::Anthropic => &["ANTHROPIC_API_KEY"],
            Backend::OpenAI => &["OPENAI_API_KEY"],
            Backend::Ollama => &[], // No key needed
            Backend::Groq => &["GROQ_API_KEY"],
            Backend::Google => &["GOOGLE_API_KEY", "GEMINI_API_KEY"],
        }
    }

    /// Get API key from environment.
    pub fn api_key(self) -> Option<String> {
        for var in self.env_vars() {
            if let Ok(key) = env::var(var) {
                if !key.is_empty() {
                    return Some(key);
                }
            }
        }
        // Ollama doesn't need a key
        if self == Backend::Ollama {
            return Some(String::new());
        }
        None
    }

    /// Check if this backend is available (has credentials).
    pub fn is_available(self) -> bool {
        self.api_key().is_some()
    }

    /// Convert to llm crate backend.
    pub(crate) fn to_llm(self) -> llm::builder::LLMBackend {
        match self {
            Backend::OpenRouter => llm::builder::LLMBackend::OpenRouter,
            Backend::Anthropic => llm::builder::LLMBackend::Anthropic,
            Backend::OpenAI => llm::builder::LLMBackend::OpenAI,
            Backend::Ollama => llm::builder::LLMBackend::Ollama,
            Backend::Groq => llm::builder::LLMBackend::Groq,
            Backend::Google => llm::builder::LLMBackend::Google,
        }
    }
}

/// Backend with availability status.
#[derive(Debug, Clone)]
pub struct BackendStatus {
    pub backend: Backend,
    pub available: bool,
}

impl BackendStatus {
    /// Detect all backends and their availability.
    pub fn detect_all() -> Vec<BackendStatus> {
        Backend::ALL
            .iter()
            .map(|&backend| BackendStatus {
                backend,
                available: backend.is_available(),
            })
            .collect()
    }

    /// Get only available backends.
    pub fn available() -> Vec<Backend> {
        Backend::ALL
            .iter()
            .copied()
            .filter(|b| b.is_available())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_backends_have_names() {
        for backend in Backend::ALL {
            assert!(!backend.name().is_empty());
            assert!(!backend.id().is_empty());
        }
    }

    #[test]
    fn test_ollama_always_available() {
        // Ollama doesn't need an API key
        assert!(Backend::Ollama.api_key().is_some());
    }
}
