//! LLM client implementation using the llm crate.

use super::api_provider::Provider;
use super::error::Error;
use super::types::{
    ChatRequest, ContentBlock, Message, Role, StreamEvent, ToolCallEvent, ToolDefinition,
};
use async_trait::async_trait;
use futures::StreamExt;
use llm::builder::{FunctionBuilder, LLMBuilder, ParamBuilder};
use llm::chat::{ChatMessage, ChatRole, MessageType, StreamChunk, Tool};
use std::sync::Arc;
use tokio::sync::mpsc;

/// LLM client for making API calls.
pub struct Client {
    provider: Provider,
    api_key: String,
    base_url: Option<String>,
}

impl Client {
    /// Create a new client for the given provider.
    pub fn new(provider: Provider, api_key: impl Into<String>) -> Self {
        Self {
            provider,
            api_key: api_key.into(),
            base_url: None,
        }
    }

    /// Create client from provider, auto-detecting API key.
    pub fn from_provider(provider: Provider) -> Result<Self, Error> {
        let api_key = provider.api_key().ok_or_else(|| Error::MissingApiKey {
            backend: provider.name().to_string(),
            env_vars: provider.env_vars().iter().map(|s| s.to_string()).collect(),
        })?;
        Ok(Self::new(provider, api_key))
    }

    /// Set custom base URL (for proxies or local servers).
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }

    /// Get the provider type.
    pub fn provider(&self) -> Provider {
        self.provider
    }

    /// Build llm crate instance for a request.
    fn build_llm(
        &self,
        model: &str,
        tools: &[ToolDefinition],
    ) -> Result<Box<dyn llm::LLMProvider>, Error> {
        // OpenRouter expects full model ID (e.g., "anthropic/claude-3-opus")
        // Other providers expect just the model name
        let model_name = if self.provider == Provider::OpenRouter {
            model
        } else {
            // Strip provider prefix if present (e.g., "google/gemini-3-flash" -> "gemini-3-flash")
            model.split_once('/').map(|(_, name)| name).unwrap_or(model)
        };

        let mut builder = LLMBuilder::new()
            .backend(self.provider.to_llm())
            .model(model_name);

        if !self.api_key.is_empty() {
            builder = builder.api_key(&self.api_key);
        }

        if let Some(ref url) = self.base_url {
            builder = builder.base_url(url);
        }

        for tool in tools {
            let mut func = FunctionBuilder::new(&tool.name).description(&tool.description);

            if let Some(props) = tool.parameters.get("properties")
                && let Some(props_obj) = props.as_object()
            {
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

            if let Some(required) = tool.parameters.get("required")
                && let Some(arr) = required.as_array()
            {
                let names: Vec<String> = arr
                    .iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect();
                func = func.required(names);
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
        let llm = self.build_llm(&request.model, &request.tools)?;
        let messages = Self::convert_messages(&request.messages);
        let tools = Self::convert_tools(&request.tools);

        let tools_ref: Option<&[Tool]> = if tools.is_empty() { None } else { Some(&tools) };

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
                    let arguments = serde_json::from_str(&tool_call.function.arguments)
                        .inspect_err(|e| {
                            tracing::warn!(
                                "Malformed tool arguments for {}: {}",
                                tool_call.function.name,
                                e
                            )
                        })
                        .unwrap_or(serde_json::Value::Null);
                    let _ = tx
                        .send(StreamEvent::ToolCall(ToolCallEvent {
                            id: tool_call.id,
                            name: tool_call.function.name,
                            arguments,
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

        let tools_ref: Option<&[Tool]> = if tools.is_empty() { None } else { Some(&tools) };

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
                let arguments = serde_json::from_str(&tc.function.arguments)
                    .inspect_err(|e| {
                        tracing::warn!("Malformed tool arguments for {}: {}", tc.function.name, e)
                    })
                    .unwrap_or(serde_json::Value::Null);
                content_blocks.push(ContentBlock::ToolCall {
                    id: tc.id,
                    name: tc.function.name,
                    arguments,
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
        let client = Client::new(Provider::OpenAI, "test-key");
        assert_eq!(client.id(), "openai");
        assert_eq!(client.provider(), Provider::OpenAI);
    }

    #[test]
    fn test_client_with_base_url() {
        let client = Client::new(Provider::Ollama, "").with_base_url("http://localhost:11434");
        assert_eq!(client.provider(), Provider::Ollama);
    }

    #[test]
    fn test_from_provider_ollama() {
        // Ollama should always work (no key needed)
        let client = Client::from_provider(Provider::Ollama);
        assert!(client.is_ok());
    }
}
