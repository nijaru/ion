//! Unified LLM provider wrapper using the `llm` crate.
//!
//! This wraps multiple providers (OpenAI, Anthropic, Ollama, Groq, Google)
//! behind our Provider trait using battle-tested library code.

use super::{
    ChatRequest, ContentBlock, Message, ModelInfo, Provider, ProviderError, Role, StreamEvent,
    ToolCallEvent, ToolDefinition,
};
use async_trait::async_trait;
use futures::StreamExt;
use llm::builder::{FunctionBuilder, LLMBackend, LLMBuilder, ParamBuilder};
use llm::chat::{ChatMessage, ChatRole, MessageType, StreamChunk, Tool};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Backend type for the unified provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnifiedBackend {
    OpenAI,
    Anthropic,
    Ollama,
    Groq,
    Google,
}

impl UnifiedBackend {
    fn to_llm_backend(self) -> LLMBackend {
        match self {
            UnifiedBackend::OpenAI => LLMBackend::OpenAI,
            UnifiedBackend::Anthropic => LLMBackend::Anthropic,
            UnifiedBackend::Ollama => LLMBackend::Ollama,
            UnifiedBackend::Groq => LLMBackend::Groq,
            UnifiedBackend::Google => LLMBackend::Google,
        }
    }

    pub fn id(&self) -> &'static str {
        match self {
            UnifiedBackend::OpenAI => "openai",
            UnifiedBackend::Anthropic => "anthropic",
            UnifiedBackend::Ollama => "ollama",
            UnifiedBackend::Groq => "groq",
            UnifiedBackend::Google => "google",
        }
    }
}

/// Unified provider that wraps the llm crate.
pub struct UnifiedProvider {
    backend: UnifiedBackend,
    api_key: String,
    base_url: Option<String>,
}

impl UnifiedProvider {
    pub fn new(backend: UnifiedBackend, api_key: String) -> Self {
        Self {
            backend,
            api_key,
            base_url: None,
        }
    }

    pub fn with_base_url(mut self, url: String) -> Self {
        self.base_url = Some(url);
        self
    }

    /// Build an LLM instance for a specific model with tools.
    fn build_llm(
        &self,
        model: &str,
        tools: &[ToolDefinition],
    ) -> Result<Box<dyn llm::LLMProvider>, ProviderError> {
        let mut builder = LLMBuilder::new()
            .backend(self.backend.to_llm_backend())
            .model(model);

        // Set API key (Ollama doesn't need one)
        if self.backend != UnifiedBackend::Ollama && !self.api_key.is_empty() {
            builder = builder.api_key(&self.api_key);
        }

        // Set base URL if provided
        if let Some(ref url) = self.base_url {
            builder = builder.base_url(url);
        }

        // Add tools
        for tool in tools {
            let mut func = FunctionBuilder::new(&tool.name).description(&tool.description);

            // Parse parameters from JSON schema
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

            // Set required parameters
            if let Some(required) = tool.parameters.get("required") {
                if let Some(required_arr) = required.as_array() {
                    let required_names: Vec<String> = required_arr
                        .iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect();
                    func = func.required(required_names);
                }
            }

            builder = builder.function(func);
        }

        builder
            .build()
            .map_err(|e| ProviderError::Stream(format!("Failed to build LLM: {}", e)))
    }

    /// Convert our messages to llm crate format.
    fn convert_messages(messages: &[Message]) -> Vec<ChatMessage> {
        let mut result = Vec::new();

        for msg in messages {
            match msg.role {
                Role::System => {
                    // llm crate handles system prompt via builder, but we can include as user context
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
                    // Tool results are added as user messages with context
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

    /// Convert llm tools to our format.
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

#[async_trait]
impl Provider for UnifiedProvider {
    fn id(&self) -> &str {
        self.backend.id()
    }

    fn model_info(&self, _model_id: &str) -> Option<ModelInfo> {
        None
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![]
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        // Use the llm crate's model listing if available
        // For now, return empty - we get models from registry
        Ok(vec![])
    }

    async fn stream(
        &self,
        request: ChatRequest,
        tx: mpsc::Sender<StreamEvent>,
    ) -> Result<(), ProviderError> {
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
            .map_err(|e| ProviderError::Stream(e.to_string()))?;

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
                Ok(StreamChunk::Done { .. }) => {
                    break;
                }
                Ok(_) => {
                    // ToolUseStart, ToolUseInputDelta - we wait for complete
                }
                Err(e) => {
                    let _ = tx.send(StreamEvent::Error(e.to_string())).await;
                    return Err(ProviderError::Stream(e.to_string()));
                }
            }
        }

        let _ = tx.send(StreamEvent::Done).await;
        Ok(())
    }

    async fn complete(&self, request: ChatRequest) -> Result<Message, ProviderError> {
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
            .map_err(|e| ProviderError::Stream(e.to_string()))?;

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
