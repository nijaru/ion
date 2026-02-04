//! LLM client implementation with native HTTP backends.

use super::anthropic::AnthropicClient;
use super::api_provider::Provider;
use super::error::Error;
use super::gemini_oauth::GeminiOAuthClient;
use super::chatgpt_responses::ChatGptResponsesClient;
use super::openai_compat::OpenAICompatClient;
use super::types::{ChatRequest, Message, StreamEvent};
use crate::auth;
use async_trait::async_trait;
use tokio::sync::mpsc;

/// Backend implementation for different provider types.
enum Backend {
    /// Native Anthropic Messages API
    Anthropic(AnthropicClient),
    /// Native OpenAI-compatible API (`OpenAI`, `OpenRouter`, Groq, Kimi, Ollama, `ChatGPT`)
    OpenAICompat(OpenAICompatClient),
    /// ChatGPT subscription via Responses API
    ChatGptResponses(ChatGptResponsesClient),
    /// Native Google Generative AI (Gemini OAuth, Google)
    GeminiOAuth(GeminiOAuthClient),
}

/// LLM client for making API calls.
pub struct Client {
    provider: Provider,
    backend: Backend,
}

impl Client {
    /// Create a new client for the given provider.
    pub fn new(provider: Provider, api_key: impl Into<String>) -> Result<Self, Error> {
        let api_key = api_key.into();
        let backend = Self::create_backend(provider, &api_key, None, None)?;
        Ok(Self { provider, backend })
    }

    /// Create client from provider, auto-detecting API key or OAuth credentials.
    /// For OAuth providers, this will refresh expired tokens if possible.
    pub async fn from_provider(provider: Provider) -> Result<Self, Error> {
        // OAuth providers: get credentials from auth storage (with refresh)
        if let Some(oauth_provider) = provider.oauth_provider() {
            let creds = auth::get_credentials(oauth_provider)
                .await
                .map_err(|e| Error::Build(format!("Failed to get credentials: {e}")))?
                .ok_or_else(|| Error::MissingApiKey {
                    backend: provider.name().to_string(),
                    env_vars: vec![format!(
                        "Run 'ion login {}' to authenticate",
                        oauth_provider.storage_key()
                    )],
                })?;

            if provider == Provider::ChatGpt {
                let account_id = match &creds {
                    auth::Credentials::OAuth(tokens) => tokens.chatgpt_account_id.clone(),
                    _ => None,
                };
                let client = ChatGptResponsesClient::new(creds.token(), account_id);
                return Ok(Self {
                    provider,
                    backend: Backend::ChatGptResponses(client),
                });
            }

            let project_id = if provider == Provider::Gemini {
                match &creds {
                    auth::Credentials::OAuth(tokens) => tokens.google_project_id.clone(),
                    _ => None,
                }
            } else {
                None
            };
            let backend = Self::create_backend(provider, creds.token(), None, project_id.as_deref())?;
            return Ok(Self { provider, backend });
        }

        // Standard providers: get API key from environment
        let api_key = provider.api_key().ok_or_else(|| Error::MissingApiKey {
            backend: provider.name().to_string(),
            env_vars: provider
                .env_vars()
                .iter()
                .map(std::string::ToString::to_string)
                .collect(),
        })?;
        Self::new(provider, api_key)
    }

    /// Create client from provider synchronously (no token refresh).
    /// Use `from_provider` for OAuth providers to ensure token refresh.
    pub fn from_provider_sync(provider: Provider) -> Result<Self, Error> {
        // OAuth providers: get credentials from auth storage (no refresh)
        if let Some(oauth_provider) = provider.oauth_provider() {
            let storage = auth::AuthStorage::new()
                .map_err(|e| Error::Build(format!("Failed to access auth storage: {e}")))?;

            let creds = storage
                .load(oauth_provider)
                .map_err(|e| Error::Build(format!("Failed to load credentials: {e}")))?
                .ok_or_else(|| Error::MissingApiKey {
                    backend: provider.name().to_string(),
                    env_vars: vec![format!(
                        "Run 'ion login {}' to authenticate",
                        oauth_provider.storage_key()
                    )],
                })?;

            if provider == Provider::ChatGpt {
                let account_id = match &creds {
                    auth::Credentials::OAuth(tokens) => tokens.chatgpt_account_id.clone(),
                    _ => None,
                };
                let client = ChatGptResponsesClient::new(creds.token(), account_id);
                return Ok(Self {
                    provider,
                    backend: Backend::ChatGptResponses(client),
                });
            }

            let project_id = if provider == Provider::Gemini {
                match &creds {
                    auth::Credentials::OAuth(tokens) => tokens.google_project_id.clone(),
                    _ => None,
                }
            } else {
                None
            };
            let backend = Self::create_backend(provider, creds.token(), None, project_id.as_deref())?;
            return Ok(Self { provider, backend });
        }

        // Standard providers: get API key from environment
        let api_key = provider.api_key().ok_or_else(|| Error::MissingApiKey {
            backend: provider.name().to_string(),
            env_vars: provider
                .env_vars()
                .iter()
                .map(std::string::ToString::to_string)
                .collect(),
        })?;
        Self::new(provider, api_key)
    }

    /// Create client with custom base URL (for proxies or local servers).
    pub fn with_base_url(
        provider: Provider,
        api_key: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Result<Self, Error> {
        let api_key = api_key.into();
        let base_url = base_url.into();
        match provider {
            Provider::OpenAI
            | Provider::OpenRouter
            | Provider::Groq
            | Provider::Kimi
            | Provider::Ollama => {
                let client = OpenAICompatClient::with_base_url(provider, api_key, base_url)?;
                Ok(Self {
                    provider,
                    backend: Backend::OpenAICompat(client),
                })
            }
            _ => Err(Error::Build(
                "Custom base URL is not supported for this provider".to_string(),
            )),
        }
    }

    /// Get the provider type.
    #[must_use]
    pub fn provider(&self) -> Provider {
        self.provider
    }

    /// Create the appropriate backend for a provider.
    fn create_backend(
        provider: Provider,
        api_key: &str,
        base_url: Option<&str>,
        project_id: Option<&str>,
    ) -> Result<Backend, Error> {
        if base_url.is_some() {
            return Err(Error::Build(
                "Custom base URL is not supported for this provider".to_string(),
            ));
        }

        match provider {
            // Anthropic uses native Messages API
            Provider::Anthropic => Ok(Backend::Anthropic(AnthropicClient::new(api_key))),

            // ChatGPT subscription uses Responses API
            Provider::ChatGpt => Ok(Backend::ChatGptResponses(ChatGptResponsesClient::new(
                api_key,
                None,
            ))),

            // Google/Gemini use native Generative AI API
            Provider::Google | Provider::Gemini => {
                Ok(Backend::GeminiOAuth(GeminiOAuthClient::new(
                    api_key,
                    project_id.map(str::to_string),
                )))
            }

            // OpenAI-compatible providers
            Provider::OpenAI
            | Provider::OpenRouter
            | Provider::Groq
            | Provider::Kimi
            | Provider::Ollama => {
                let client = OpenAICompatClient::new(provider, api_key)?;
                Ok(Backend::OpenAICompat(client))
            }
        }
    }
}


/// Trait for LLM operations.
///
/// This trait is focused on LLM API calls (chat, streaming). Model discovery
/// is handled by `ModelRegistry` instead.
#[async_trait]
pub trait LlmApi: Send + Sync {
    /// Get the provider identifier.
    fn id(&self) -> &str;
    /// Stream a chat completion.
    async fn stream(
        &self,
        request: ChatRequest,
        tx: mpsc::Sender<StreamEvent>,
    ) -> Result<(), Error>;
    /// Get a non-streaming chat completion.
    async fn complete(&self, request: ChatRequest) -> Result<Message, Error>;
}

#[async_trait]
impl LlmApi for Client {
    fn id(&self) -> &str {
        self.provider.id()
    }

    async fn stream(
        &self,
        request: ChatRequest,
        tx: mpsc::Sender<StreamEvent>,
    ) -> Result<(), Error> {
        tracing::debug!(
            provider = %self.provider.id(),
            model = %request.model,
            tools = request.tools.len(),
            messages = request.messages.len(),
            "API stream request"
        );

        match &self.backend {
            Backend::Anthropic(client) => client.stream(request, tx).await,
            Backend::OpenAICompat(client) => client.stream(request, tx).await,
            Backend::ChatGptResponses(client) => client.stream(request, tx).await,
            Backend::GeminiOAuth(client) => client.stream(request, tx).await,
        }
    }

    async fn complete(&self, request: ChatRequest) -> Result<Message, Error> {
        tracing::debug!(
            provider = %self.provider.id(),
            model = %request.model,
            tools = request.tools.len(),
            messages = request.messages.len(),
            "API request"
        );

        match &self.backend {
            Backend::Anthropic(client) => client.complete(request).await,
            Backend::OpenAICompat(client) => client.complete(request).await,
            Backend::ChatGptResponses(client) => client.complete(request).await,
            Backend::GeminiOAuth(client) => client.complete(request).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_client_creation() {
        // Ollama doesn't need a key
        let client = Client::from_provider(Provider::Ollama).await;
        assert!(client.is_ok());
        assert_eq!(client.unwrap().provider(), Provider::Ollama);
    }

    #[tokio::test]
    async fn test_from_provider_ollama() {
        // Ollama should always work (no key needed)
        let client = Client::from_provider(Provider::Ollama).await;
        assert!(client.is_ok());
    }

    #[test]
    fn test_anthropic_backend() {
        let client = Client::new(Provider::Anthropic, "test-key").unwrap();
        assert_eq!(client.provider(), Provider::Anthropic);
        assert!(matches!(client.backend, Backend::Anthropic(_)));
    }

    #[test]
    fn test_openai_backend() {
        let client = Client::new(Provider::OpenAI, "test-key").unwrap();
        assert_eq!(client.provider(), Provider::OpenAI);
        assert!(matches!(client.backend, Backend::OpenAICompat(_)));
    }

    #[test]
    fn test_openrouter_backend() {
        let client = Client::new(Provider::OpenRouter, "test-key").unwrap();
        assert_eq!(client.provider(), Provider::OpenRouter);
        assert!(matches!(client.backend, Backend::OpenAICompat(_)));
    }

    #[test]
    fn test_gemini_backend() {
        let client = Client::new(Provider::Gemini, "test-token").unwrap();
        assert_eq!(client.provider(), Provider::Gemini);
        assert!(matches!(client.backend, Backend::GeminiOAuth(_)));
    }

    #[test]
    fn test_google_backend() {
        let client = Client::new(Provider::Google, "test-key").unwrap();
        assert_eq!(client.provider(), Provider::Google);
        assert!(matches!(client.backend, Backend::GeminiOAuth(_)));
    }
}
