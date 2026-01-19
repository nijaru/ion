//! LLM client implementation using the llm crate.

use super::backend::Backend;
use super::error::Error;
use super::types::{
    ChatRequest, ContentBlock, Message, ModelInfo, ModelPricing, Role, StreamEvent, ToolCallEvent,
    ToolDefinition,
};
use async_trait::async_trait;
use futures::StreamExt;
use llm::builder::{FunctionBuilder, LLMBuilder, ParamBuilder};
use llm::chat::{ChatMessage, ChatRole, MessageType, StreamChunk, Tool};
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::mpsc;

/// LLM client for making API calls.
pub struct Client {
    backend: Backend,
    api_key: String,
    base_url: Option<String>,
}

impl Client {
    /// Create a new client for the given backend.
    pub fn new(backend: Backend, api_key: impl Into<String>) -> Self {
        Self {
            backend,
            api_key: api_key.into(),
            base_url: None,
        }
    }

    /// Create client from backend, auto-detecting API key.
    pub fn from_backend(backend: Backend) -> Result<Self, Error> {
        let api_key = backend.api_key().ok_or_else(|| Error::MissingApiKey {
            backend: backend.name().to_string(),
            env_vars: backend
                .env_vars()
                .iter()
                .map(|s| s.to_string())
                .collect(),
        })?;
        Ok(Self::new(backend, api_key))
    }

    /// Set custom base URL (for proxies or local servers).
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }

    /// Get the backend type.
    pub fn backend(&self) -> Backend {
        self.backend
    }

    /// Build llm crate instance for a request.
    fn build_llm(&self, model: &str, tools: &[ToolDefinition]) -> Result<Box<dyn llm::LLMProvider>, Error> {
        let mut builder = LLMBuilder::new()
            .backend(self.backend.to_llm())
            .model(model);

        if !self.api_key.is_empty() {
            builder = builder.api_key(&self.api_key);
        }

        if let Some(ref url) = self.base_url {
            builder = builder.base_url(url);
        }

        for tool in tools {
            let mut func = FunctionBuilder::new(&tool.name).description(&tool.description);

            if let Some(props) = tool.parameters.get("properties") {
                if let Some(props_obj) = props.as_object() {
                    for (name, schema) in props_obj {
                        let type_str = schema
                            .get("type")
                            .and_then(|t| t.as_str())
                            .unwrap_or("string");
                        let desc = schema
                            .get("description")
                            .and_then(|d| d.as_str())
                            .unwrap_or("");
                        func = func.param(ParamBuilder::new(name).type_of(type_str).description(desc));
                    }
                }
            }

            if let Some(required) = tool.parameters.get("required") {
                if let Some(arr) = required.as_array() {
                    let names: Vec<String> = arr
                        .iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect();
                    func = func.required(names);
                }
            }

            builder = builder.function(func);
        }

        builder.build().map_err(|e| Error::Build(e.to_string()))
    }

    fn convert_messages(messages: &[Message]) -> Vec<ChatMessage> {
        let mut result = Vec::new();

        for msg in messages {
            match msg.role {
                Role::System => {
                    for block in msg.content.as_ref() {
                        if let ContentBlock::Text { text } = block {
                            result.push(ChatMessage {
                                role: ChatRole::User,
                                message_type: MessageType::Text,
                                content: format!("[System]: {}", text),
                            });
                        }
                    }
                }
                Role::User => {
                    let mut text = String::new();
                    for block in msg.content.as_ref() {
                        if let ContentBlock::Text { text: t } = block {
                            text.push_str(t);
                        }
                    }
                    if !text.is_empty() {
                        result.push(ChatMessage {
                            role: ChatRole::User,
                            message_type: MessageType::Text,
                            content: text,
                        });
                    }
                }
                Role::Assistant => {
                    let mut text = String::new();
                    for block in msg.content.as_ref() {
                        match block {
                            ContentBlock::Text { text: t } => text.push_str(t),
                            ContentBlock::Thinking { thinking } => {
                                text.push_str(&format!("<thinking>{}</thinking>", thinking));
                            }
                            _ => {}
                        }
                    }
                    if !text.is_empty() {
                        result.push(ChatMessage {
                            role: ChatRole::Assistant,
                            message_type: MessageType::Text,
                            content: text,
                        });
                    }
                }
                Role::ToolResult => {
                    for block in msg.content.as_ref() {
                        if let ContentBlock::ToolResult {
                            tool_call_id,
                            content,
                            is_error,
                        } = block
                        {
                            let prefix = if *is_error { "[Error]" } else { "[Result]" };
                            result.push(ChatMessage {
                                role: ChatRole::User,
                                message_type: MessageType::Text,
                                content: format!("{} Tool {}: {}", prefix, tool_call_id, content),
                            });
                        }
                    }
                }
            }
        }

        result
    }

    fn convert_tools(tools: &[ToolDefinition]) -> Vec<Tool> {
        tools
            .iter()
            .map(|t| Tool {
                tool_type: "function".to_string(),
                function: llm::chat::FunctionTool {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: t.parameters.clone(),
                },
            })
            .collect()
    }

    /// Fetch models from OpenRouter API.
    async fn list_openrouter_models(&self) -> Result<Vec<ModelInfo>, Error> {
        let base_url = self
            .base_url
            .as_deref()
            .unwrap_or("https://openrouter.ai/api/v1");

        let client = reqwest::Client::new();
        let response = client
            .get(format!("{}/models", base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await
            .map_err(|e| Error::Api(format!("OpenRouter API error: {}", e)))?;

        if !response.status().is_success() {
            return Err(Error::Api(format!(
                "OpenRouter returned status {}",
                response.status()
            )));
        }

        let data: OpenRouterModelsResponse = response
            .json()
            .await
            .map_err(|e| Error::Api(format!("Failed to parse OpenRouter response: {}", e)))?;

        Ok(data
            .data
            .into_iter()
            .map(|m| {
                let supports_cache = m.pricing.cache_read.is_some_and(|p| p > 0.0);
                let supports_vision = m
                    .architecture
                    .as_ref()
                    .and_then(|a| a.modality.as_ref())
                    .is_some_and(|modality| modality.contains("image"));
                let provider = m.id.split('/').next().unwrap_or("unknown").to_string();
                let supports_tools = m
                    .architecture
                    .as_ref()
                    .and_then(|a| a.instruct_type.as_ref())
                    .is_some();

                ModelInfo {
                    id: m.id,
                    name: m.name,
                    provider,
                    context_window: m.context_length,
                    supports_tools,
                    supports_vision,
                    supports_thinking: false,
                    supports_cache,
                    pricing: ModelPricing {
                        input: m.pricing.prompt * 1_000_000.0,
                        output: m.pricing.completion * 1_000_000.0,
                        cache_read: m.pricing.cache_read.map(|p| p * 1_000_000.0),
                        cache_write: m.pricing.cache_write.map(|p| p * 1_000_000.0),
                    },
                    created: 0,
                }
            })
            .collect())
    }

    /// Fetch models from Ollama API.
    async fn list_ollama_models(&self) -> Result<Vec<ModelInfo>, Error> {
        let base_url = self
            .base_url
            .as_deref()
            .unwrap_or("http://localhost:11434");

        let client = reqwest::Client::new();
        let response = client
            .get(format!("{}/api/tags", base_url))
            .send()
            .await
            .map_err(|e| Error::Api(format!("Ollama API error: {}", e)))?;

        if !response.status().is_success() {
            return Err(Error::Api(format!(
                "Ollama returned status {}",
                response.status()
            )));
        }

        let data: OllamaTagsResponse = response
            .json()
            .await
            .map_err(|e| Error::Api(format!("Failed to parse Ollama response: {}", e)))?;

        Ok(data
            .models
            .into_iter()
            .map(|m| ModelInfo {
                id: m.name.clone(),
                name: m.name,
                provider: "ollama".to_string(),
                context_window: 128_000, // Default, Ollama doesn't report this
                supports_tools: true,    // Most modern models support tools
                supports_vision: false,
                supports_thinking: false,
                supports_cache: false,
                pricing: ModelPricing::default(),
                created: 0,
            })
            .collect())
    }
}

/// Ollama API response for /api/tags.
#[derive(Debug, Deserialize)]
struct OllamaTagsResponse {
    models: Vec<OllamaModel>,
}

#[derive(Debug, Deserialize)]
struct OllamaModel {
    name: String,
    #[allow(dead_code)]
    model: String,
    #[allow(dead_code)]
    size: u64,
}

/// OpenRouter API response for /models.
#[derive(Debug, Deserialize)]
struct OpenRouterModelsResponse {
    data: Vec<OpenRouterModel>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterModel {
    id: String,
    name: String,
    context_length: u32,
    pricing: OpenRouterPricing,
    #[serde(default)]
    architecture: Option<OpenRouterArchitecture>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterPricing {
    #[serde(default, deserialize_with = "parse_price")]
    prompt: f64,
    #[serde(default, deserialize_with = "parse_price")]
    completion: f64,
    #[serde(default, deserialize_with = "parse_optional_price")]
    cache_read: Option<f64>,
    #[serde(default, deserialize_with = "parse_optional_price")]
    cache_write: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterArchitecture {
    modality: Option<String>,
    #[serde(default)]
    instruct_type: Option<String>,
}

fn parse_price<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s: String = Deserialize::deserialize(deserializer)?;
    Ok(s.parse().unwrap_or(0.0))
}

fn parse_optional_price<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Deserialize::deserialize(deserializer)?;
    Ok(opt.and_then(|s| s.parse().ok()))
}

/// Trait for LLM operations.
#[async_trait]
pub trait LlmApi: Send + Sync {
    fn id(&self) -> &str;
    fn model_info(&self, model_id: &str) -> Option<ModelInfo>;
    fn models(&self) -> Vec<ModelInfo>;
    async fn list_models(&self) -> Result<Vec<ModelInfo>, Error>;
    async fn stream(&self, request: ChatRequest, tx: mpsc::Sender<StreamEvent>) -> Result<(), Error>;
    async fn complete(&self, request: ChatRequest) -> Result<Message, Error>;
}

#[async_trait]
impl LlmApi for Client {
    fn id(&self) -> &str {
        self.backend.id()
    }

    fn model_info(&self, _model_id: &str) -> Option<ModelInfo> {
        None
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![]
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, Error> {
        match self.backend {
            Backend::OpenRouter => self.list_openrouter_models().await,
            Backend::Ollama => self.list_ollama_models().await,
            // Other backends don't have dynamic model listing via API
            // They use static lists or registry fetching
            _ => Ok(vec![]),
        }
    }

    async fn stream(&self, request: ChatRequest, tx: mpsc::Sender<StreamEvent>) -> Result<(), Error> {
        let llm = self.build_llm(&request.model, &request.tools)?;
        let messages = Self::convert_messages(&request.messages);
        let tools = Self::convert_tools(&request.tools);

        let tools_ref: Option<&[Tool]> = if tools.is_empty() {
            None
        } else {
            Some(&tools)
        };

        let mut stream = llm
            .chat_stream_with_tools(&messages, tools_ref)
            .await
            .map_err(|e| Error::Stream(e.to_string()))?;

        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(StreamChunk::Text(text)) => {
                    let _ = tx.send(StreamEvent::TextDelta(text)).await;
                }
                Ok(StreamChunk::ToolUseComplete { tool_call, .. }) => {
                    let _ = tx
                        .send(StreamEvent::ToolCall(ToolCallEvent {
                            id: tool_call.id,
                            name: tool_call.function.name,
                            arguments: serde_json::from_str(&tool_call.function.arguments)
                                .unwrap_or(serde_json::Value::Null),
                        }))
                        .await;
                }
                Ok(StreamChunk::Done { .. }) => break,
                Ok(_) => {}
                Err(e) => {
                    let _ = tx.send(StreamEvent::Error(e.to_string())).await;
                    return Err(Error::Stream(e.to_string()));
                }
            }
        }

        let _ = tx.send(StreamEvent::Done).await;
        Ok(())
    }

    async fn complete(&self, request: ChatRequest) -> Result<Message, Error> {
        let llm = self.build_llm(&request.model, &request.tools)?;
        let messages = Self::convert_messages(&request.messages);
        let tools = Self::convert_tools(&request.tools);

        let tools_ref: Option<&[Tool]> = if tools.is_empty() {
            None
        } else {
            Some(&tools)
        };

        let response = llm
            .chat_with_tools(&messages, tools_ref)
            .await
            .map_err(|e| Error::Api(e.to_string()))?;

        let mut content_blocks = Vec::new();

        if let Some(text) = response.text() {
            content_blocks.push(ContentBlock::Text { text });
        }

        if let Some(tool_calls) = response.tool_calls() {
            for tc in tool_calls {
                content_blocks.push(ContentBlock::ToolCall {
                    id: tc.id,
                    name: tc.function.name,
                    arguments: serde_json::from_str(&tc.function.arguments)
                        .unwrap_or(serde_json::Value::Null),
                });
            }
        }

        Ok(Message {
            role: Role::Assistant,
            content: Arc::new(content_blocks),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_creation() {
        let client = Client::new(Backend::OpenAI, "test-key");
        assert_eq!(client.id(), "openai");
        assert_eq!(client.backend(), Backend::OpenAI);
    }

    #[test]
    fn test_client_with_base_url() {
        let client = Client::new(Backend::Ollama, "")
            .with_base_url("http://localhost:11434");
        assert_eq!(client.backend(), Backend::Ollama);
    }

    #[test]
    fn test_from_backend_ollama() {
        // Ollama should always work (no key needed)
        let client = Client::from_backend(Backend::Ollama);
        assert!(client.is_ok());
    }

    #[tokio::test]
    async fn test_ollama_list_models() {
        // Skip if Ollama isn't running
        let client = reqwest::Client::new();
        if client
            .get("http://localhost:11434/api/tags")
            .send()
            .await
            .is_err()
        {
            eprintln!("Skipping test: Ollama not running");
            return;
        }

        let ollama = Client::new(Backend::Ollama, "");
        let models = ollama.list_models().await;
        assert!(models.is_ok(), "list_models should succeed");
        let models = models.unwrap();
        // Ollama should return at least one model if running
        assert!(!models.is_empty(), "Ollama should have at least one model");
        // Each model should have basic fields
        for model in &models {
            assert!(!model.id.is_empty());
            assert!(!model.name.is_empty());
            assert_eq!(model.provider, "ollama");
        }
    }
}
