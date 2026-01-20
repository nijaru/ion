mod anthropic;
mod api_provider;
mod models_dev;
mod openai;
mod openrouter;
mod prefs;
mod registry;

pub use anthropic::AnthropicProvider;
pub use api_provider::{ApiProvider, ProviderStatus};
pub use openai::OpenAIProvider;
pub use openrouter::OpenRouterProvider;
pub use prefs::ProviderPrefs;
pub use registry::{ModelFilter, ModelRegistry};

/// Create a provider instance based on the ApiProvider enum.
pub fn create_provider(
    api_provider: ApiProvider,
    api_key: String,
    prefs: ProviderPrefs,
) -> Arc<dyn Provider> {
    match api_provider {
        ApiProvider::OpenRouter => Arc::new(OpenRouterProvider::with_prefs(api_key, prefs)),
        ApiProvider::Anthropic => Arc::new(AnthropicProvider::new(api_key)),
        ApiProvider::OpenAI => Arc::new(OpenAIProvider::new(api_key)),
        _ => {
            // Fallback to OpenRouter or panic if not implemented but marked as such
            Arc::new(OpenRouterProvider::with_prefs(api_key, prefs))
        }
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
        );
        assert_eq!(provider.id(), "openrouter");
    }

    #[test]
    fn test_create_provider_anthropic() {
        let provider = create_provider(
            ApiProvider::Anthropic,
            "test_key".into(),
            ProviderPrefs::default(),
        );
        assert_eq!(provider.id(), "anthropic");
    }

    #[test]
    fn test_create_provider_openai() {
        let provider = create_provider(
            ApiProvider::OpenAI,
            "test_key".into(),
            ProviderPrefs::default(),
        );
        assert_eq!(provider.id(), "openai");
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
