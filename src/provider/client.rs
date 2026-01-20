//! LLM client implementation using llm-connector.

use super::api_provider::Provider;
use super::error::Error;
use super::types::{
    ChatRequest, ContentBlock, Message, Role, StreamEvent, ToolCallEvent, ToolDefinition,
};
use async_trait::async_trait;
use futures::StreamExt;
use llm_connector::LlmClient;
use std::sync::Arc;
use tokio::sync::mpsc;

/// LLM client for making API calls.
pub struct Client {
    provider: Provider,
    client: LlmClient,
}

impl Client {
    /// Create a new client for the given provider.
    pub fn new(provider: Provider, api_key: impl Into<String>) -> Result<Self, Error> {
        let api_key = api_key.into();
        let client = Self::create_llm_client(provider, &api_key, None)?;
        Ok(Self { provider, client })
    }

    /// Create client from provider, auto-detecting API key.
    pub fn from_provider(provider: Provider) -> Result<Self, Error> {
        let api_key = provider.api_key().ok_or_else(|| Error::MissingApiKey {
            backend: provider.name().to_string(),
            env_vars: provider.env_vars().iter().map(|s| s.to_string()).collect(),
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
        let client = Self::create_llm_client(provider, &api_key, Some(&base_url))?;
        Ok(Self { provider, client })
    }

    /// Get the provider type.
    pub fn provider(&self) -> Provider {
        self.provider
    }

    /// Create the appropriate llm-connector client for a provider.
    fn create_llm_client(
        provider: Provider,
        api_key: &str,
        base_url: Option<&str>,
    ) -> Result<LlmClient, Error> {
        let client = match provider {
            Provider::Anthropic => LlmClient::anthropic(api_key),
            Provider::OpenAI => {
                if let Some(url) = base_url {
                    LlmClient::openai_with_base_url(api_key, url)
                } else {
                    LlmClient::openai(api_key)
                }
            }
            Provider::Google => LlmClient::google(api_key),
            Provider::Ollama => {
                if let Some(url) = base_url {
                    LlmClient::ollama_with_base_url(url)
                } else {
                    LlmClient::ollama()
                }
            }
            Provider::OpenRouter => LlmClient::openai_compatible(
                api_key,
                "https://openrouter.ai/api/v1",
                "openrouter",
            ),
            Provider::Groq => {
                LlmClient::openai_compatible(api_key, "https://api.groq.com/openai/v1", "groq")
            }
        };
        client.map_err(|e| Error::Build(e.to_string()))
    }

    /// Convert our messages to llm-connector format.
    fn convert_messages(messages: &[Message]) -> Vec<llm_connector::Message> {
        let mut result = Vec::new();

        for msg in messages {
            match msg.role {
                Role::System => {
                    for block in msg.content.as_ref() {
                        if let ContentBlock::Text { text } = block {
                            result.push(llm_connector::Message::system(text));
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
                        result.push(llm_connector::Message::user(&text));
                    }
                }
                Role::Assistant => {
                    let mut text = String::new();
                    let mut tool_calls = Vec::new();
                    for block in msg.content.as_ref() {
                        match block {
                            ContentBlock::Text { text: t } => text.push_str(t),
                            ContentBlock::Thinking { thinking } => {
                                text.push_str(&format!("<thinking>{}</thinking>", thinking));
                            }
                            ContentBlock::ToolCall {
                                id,
                                name,
                                arguments,
                            } => {
                                tool_calls.push(llm_connector::types::ToolCall {
                                    id: id.clone(),
                                    call_type: "function".to_string(),
                                    index: None,
                                    function: llm_connector::types::FunctionCall {
                                        name: name.clone(),
                                        arguments: arguments.to_string(),
                                    },
                                });
                            }
                            _ => {}
                        }
                    }
                    if !text.is_empty() || !tool_calls.is_empty() {
                        let mut msg = llm_connector::Message::assistant(&text);
                        if !tool_calls.is_empty() {
                            msg.tool_calls = Some(tool_calls);
                        }
                        result.push(msg);
                    }
                }
                Role::ToolResult => {
                    for block in msg.content.as_ref() {
                        if let ContentBlock::ToolResult {
                            tool_call_id,
                            content,
                            ..
                        } = block
                        {
                            tracing::debug!("Tool result: id={}, content_len={}", tool_call_id, content.len());
                            // Note: llm-connector expects (content, tool_call_id) order
                            result.push(llm_connector::Message::tool(content, tool_call_id));
                        }
                    }
                }
            }
        }

        result
    }

    /// Convert our tool definitions to llm-connector format.
    fn convert_tools(tools: &[ToolDefinition]) -> Vec<llm_connector::types::Tool> {
        tools
            .iter()
            .map(|t| llm_connector::types::Tool {
                tool_type: "function".to_string(),
                function: llm_connector::types::Function {
                    name: t.name.clone(),
                    description: Some(t.description.clone()),
                    parameters: t.parameters.clone(),
                },
            })
            .collect()
    }

    /// Build a ChatRequest for llm-connector.
    fn build_request(request: &ChatRequest) -> llm_connector::ChatRequest {
        let messages = Self::convert_messages(&request.messages);
        let tools = Self::convert_tools(&request.tools);

        tracing::debug!(
            "Building request: model='{}', input_tools={}, converted_tools={}",
            request.model,
            request.tools.len(),
            tools.len()
        );

        llm_connector::ChatRequest {
            model: request.model.clone(),
            messages,
            tools: if tools.is_empty() { None } else { Some(tools) },
            ..Default::default()
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
        let llm_request = Self::build_request(&request);

        let mut stream = self
            .client
            .chat_stream(&llm_request)
            .await
            .map_err(|e| {
                tracing::error!("Stream error from {}: {:?}", self.provider.id(), e);
                Error::Stream(format!("{} ({})", e, self.provider.id()))
            })?;

        while let Some(chunk_result) = stream.next().await {
            match chunk_result {
                Ok(chunk) => {
                    // Handle text content
                    if let Some(content) = chunk.get_content() {
                        if !content.is_empty() {
                            let _ = tx.send(StreamEvent::TextDelta(content.to_string())).await;
                        }
                    }

                    // Handle tool calls from choices
                    for choice in &chunk.choices {
                        if let Some(tool_calls) = &choice.delta.tool_calls {
                            for tc in tool_calls {
                                // Only emit complete tool calls (with id and name)
                                if !tc.id.is_empty() && !tc.function.name.is_empty() {
                                    let arguments: serde_json::Value =
                                        serde_json::from_str(&tc.function.arguments)
                                            .unwrap_or(serde_json::Value::Null);

                                    let _ = tx
                                        .send(StreamEvent::ToolCall(ToolCallEvent {
                                            id: tc.id.clone(),
                                            name: tc.function.name.clone(),
                                            arguments,
                                        }))
                                        .await;
                                }
                            }
                        }

                        // Check for completion
                        if choice.finish_reason.as_deref() == Some("stop")
                            || choice.finish_reason.as_deref() == Some("tool_calls")
                        {
                            break;
                        }
                    }
                }
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
        let llm_request = Self::build_request(&request);

        tracing::debug!(
            "LLM request: provider={}, model={}, tools={}, messages={}",
            self.provider.id(),
            llm_request.model,
            llm_request.tools.as_ref().map(|t| t.len()).unwrap_or(0),
            llm_request.messages.len()
        );
        if let Some(tools) = &llm_request.tools {
            for tool in tools {
                tracing::trace!("  Tool: {}", tool.function.name);
            }
        }

        let response = self
            .client
            .chat(&llm_request)
            .await
            .map_err(|e| {
                // Log full error details for debugging
                tracing::error!(
                    "API error: provider={}, model={}, error={:?}",
                    self.provider.id(),
                    llm_request.model,
                    e
                );
                // Format error with helpful context
                Error::Api(format!(
                    "{}\n  Provider: {}\n  Model: {}",
                    e,
                    self.provider.id(),
                    llm_request.model
                ))
            })?;

        let mut content_blocks = Vec::new();

        // Extract content from first choice
        if let Some(choice) = response.choices.first() {
            // Extract text content from message blocks
            for block in &choice.message.content {
                match block {
                    llm_connector::types::MessageBlock::Text { text } => {
                        content_blocks.push(ContentBlock::Text { text: text.clone() });
                    }
                    _ => {} // Ignore image blocks for now
                }
            }

            // Extract tool calls
            if let Some(tool_calls) = &choice.message.tool_calls {
                for tc in tool_calls {
                    let arguments = serde_json::from_str(&tc.function.arguments)
                        .inspect_err(|e| {
                            tracing::warn!("Malformed tool arguments for {}: {}", tc.function.name, e)
                        })
                        .unwrap_or(serde_json::Value::Null);
                    content_blocks.push(ContentBlock::ToolCall {
                        id: tc.id.clone(),
                        name: tc.function.name.clone(),
                        arguments,
                    });
                }
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
        // Ollama doesn't need a key
        let client = Client::from_provider(Provider::Ollama);
        assert!(client.is_ok());
        assert_eq!(client.unwrap().provider(), Provider::Ollama);
    }

    #[test]
    fn test_from_provider_ollama() {
        // Ollama should always work (no key needed)
        let client = Client::from_provider(Provider::Ollama);
        assert!(client.is_ok());
    }
}
