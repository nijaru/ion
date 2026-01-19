mod anthropic;
mod api_provider;
mod llm_provider;
mod models_dev;
mod ollama;
mod openai;
mod openrouter;
mod prefs;
mod registry;

pub use anthropic::AnthropicProvider;
pub use api_provider::{ApiProvider, ProviderStatus};
pub use llm_provider::{UnifiedBackend, UnifiedProvider};
pub use ollama::OllamaProvider;
pub use openai::OpenAIProvider;
pub use openrouter::OpenRouterProvider;
pub use prefs::ProviderPrefs;
pub use registry::{ModelFilter, ModelRegistry};

/// Create a provider instance based on the ApiProvider enum.
///
/// Returns an error if the provider is not implemented.
pub fn create_provider(
    api_provider: ApiProvider,
    api_key: String,
    prefs: ProviderPrefs,
) -> Result<Arc<dyn Provider>, ProviderError> {
    match api_provider {
        ApiProvider::OpenRouter => Ok(Arc::new(OpenRouterProvider::with_prefs(api_key, prefs))),
        ApiProvider::Anthropic => Ok(Arc::new(AnthropicProvider::new(api_key))),
        ApiProvider::OpenAI => Ok(Arc::new(OpenAIProvider::new(api_key))),
        ApiProvider::Ollama => Ok(Arc::new(OllamaProvider::new())),
        _ => Err(ProviderError::Api {
            code: "NOT_IMPLEMENTED".to_string(),
            message: format!("Provider {:?} is not implemented", api_provider),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_provider_openrouter() {
        let provider = create_provider(
            ApiProvider::OpenRouter,
            "test_key".into(),
            ProviderPrefs::default(),
        )
        .unwrap();
        assert_eq!(provider.id(), "openrouter");
    }

    #[test]
    fn test_create_provider_anthropic() {
        let provider = create_provider(
            ApiProvider::Anthropic,
            "test_key".into(),
            ProviderPrefs::default(),
        )
        .unwrap();
        assert_eq!(provider.id(), "anthropic");
    }

    #[test]
    fn test_create_provider_openai() {
        let provider = create_provider(
            ApiProvider::OpenAI,
            "test_key".into(),
            ProviderPrefs::default(),
        )
        .unwrap();
        assert_eq!(provider.id(), "openai");
    }

    #[test]
    fn test_create_provider_ollama() {
        let provider = create_provider(
            ApiProvider::Ollama,
            String::new(), // Ollama doesn't need an API key
            ProviderPrefs::default(),
        )
        .unwrap();
        assert_eq!(provider.id(), "ollama");
    }

    #[test]
    fn test_create_provider_unimplemented() {
        let result = create_provider(
            ApiProvider::Google,
            "test_key".into(),
            ProviderPrefs::default(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_unified_provider_backends() {
        // Test that UnifiedProvider can be created for each backend
        let openai = UnifiedProvider::new(UnifiedBackend::OpenAI, "test".into());
        assert_eq!(openai.id(), "openai");

        let anthropic = UnifiedProvider::new(UnifiedBackend::Anthropic, "test".into());
        assert_eq!(anthropic.id(), "anthropic");

        let ollama = UnifiedProvider::new(UnifiedBackend::Ollama, String::new());
        assert_eq!(ollama.id(), "ollama");

        let groq = UnifiedProvider::new(UnifiedBackend::Groq, "test".into());
        assert_eq!(groq.id(), "groq");

        let google = UnifiedProvider::new(UnifiedBackend::Google, "test".into());
        assert_eq!(google.id(), "google");
    }
}

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub enum StreamEvent {
    TextDelta(String),
    ThinkingDelta(String),
    ToolCall(ToolCallEvent),
    Usage(Usage),
    Done,
    Error(String),
}

#[derive(Debug, Clone)]
pub struct ToolCallEvent {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
}

/// Pricing per million tokens.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelPricing {
    pub input: f64,
    pub output: f64,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub context_window: u32,
    pub supports_tools: bool,
    pub supports_vision: bool,
    pub supports_thinking: bool,
    pub supports_cache: bool,
    pub pricing: ModelPricing,
    /// Unix timestamp when model was added (for sorting newest first)
    pub created: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Arc<Vec<ContentBlock>>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    ToolResult,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking { thinking: String },
    #[serde(rename = "tool_call")]
    ToolCall {
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_call_id: String,
        content: String,
        is_error: bool,
    },
    #[serde(rename = "image")]
    Image { media_type: String, data: String },
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Arc<Vec<Message>>,
    pub system: Option<Cow<'static, str>>,
    pub tools: Arc<Vec<ToolDefinition>>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub thinking: Option<ThinkingConfig>,
}

#[derive(Debug, Clone)]
pub struct ThinkingConfig {
    pub enabled: bool,
    pub budget_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> &str;
    fn model_info(&self, model_id: &str) -> Option<ModelInfo>;
    fn models(&self) -> Vec<ModelInfo>;

    /// Fetch available models from the provider API.
    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError>;

    async fn stream(
        &self,
        request: ChatRequest,
        tx: mpsc::Sender<StreamEvent>,
    ) -> Result<(), ProviderError>;

    async fn complete(&self, request: ChatRequest) -> Result<Message, ProviderError>;
}

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Stream error: {0}")]
    Stream(String),

    #[error("API error: {code} - {message}")]
    Api { code: String, message: String },

    #[error("Rate limited, retry after {retry_after:?}s")]
    RateLimited { retry_after: Option<u64> },

    #[error("Context overflow: {used} > {limit}")]
    ContextOverflow { used: u32, limit: u32 },

    #[error("Cancelled")]
    Cancelled,
}
