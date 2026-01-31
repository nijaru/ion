//! OpenAI-compatible API client.

use super::quirks::{ProviderQuirks, ReasoningField};
use super::request::{
    ContentPart, FunctionCall, FunctionDefinition, ImageUrl, MessageContent, OpenAIMessage,
    OpenAIRequest, OpenAITool, ProviderRouting, ToolCall,
};
use super::response::OpenAIResponse;
use super::stream::StreamChunk;
use crate::provider::api_provider::Provider;
use crate::provider::error::Error;
use crate::provider::http::{AuthConfig, HttpClient, SseParser};
use crate::provider::prefs::ProviderPrefs;
use crate::provider::types::{
    ChatRequest, ContentBlock, Message, Role, StreamEvent, ToolCallEvent, ToolDefinition, Usage,
};
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Native OpenAI-compatible API client.
pub struct OpenAICompatClient {
    http: HttpClient,
    quirks: ProviderQuirks,
    provider: Provider,
}

impl OpenAICompatClient {
    /// Create a new OpenAI-compatible client.
    pub fn new(provider: Provider, api_key: impl Into<String>) -> Result<Self, Error> {
        let quirks = ProviderQuirks::for_provider(provider);
        let api_key = api_key.into();

        // Ollama doesn't need auth
        let auth = if provider == Provider::Ollama {
            AuthConfig::Bearer(String::new())
        } else {
            AuthConfig::Bearer(api_key)
        };

        let http = HttpClient::new(quirks.base_url, auth);

        Ok(Self {
            http,
            quirks,
            provider,
        })
    }

    /// Create a client with custom base URL.
    pub fn with_base_url(
        provider: Provider,
        api_key: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Result<Self, Error> {
        let quirks = ProviderQuirks::for_provider(provider);
        let api_key = api_key.into();

        let auth = if provider == Provider::Ollama {
            AuthConfig::Bearer(String::new())
        } else {
            AuthConfig::Bearer(api_key)
        };

        let http = HttpClient::new(base_url.into(), auth);

        Ok(Self {
            http,
            quirks,
            provider,
        })
    }

    /// Make a non-streaming chat completion request.
    pub async fn complete(&self, request: ChatRequest) -> Result<Message, Error> {
        let api_request = self.build_request(&request, None, false);

        tracing::debug!(
            provider = %self.provider.id(),
            model = %api_request.model,
            messages = api_request.messages.len(),
            tools = api_request.tools.as_ref().map_or(0, |t| t.len()),
            "OpenAI-compat API request"
        );

        let response: OpenAIResponse = self
            .http
            .post_json("/chat/completions", &api_request)
            .await?;

        Ok(self.convert_response(response))
    }

    /// Stream a chat completion request.
    pub async fn stream(
        &self,
        request: ChatRequest,
        tx: mpsc::Sender<StreamEvent>,
    ) -> Result<(), Error> {
        self.stream_with_prefs(request, None, tx).await
    }

    /// Stream with provider preferences (for OpenRouter routing).
    pub async fn stream_with_prefs(
        &self,
        request: ChatRequest,
        prefs: Option<&ProviderPrefs>,
        tx: mpsc::Sender<StreamEvent>,
    ) -> Result<(), Error> {
        let api_request = self.build_request(&request, prefs, true);

        tracing::debug!(
            provider = %self.provider.id(),
            model = %api_request.model,
            messages = api_request.messages.len(),
            tools = api_request.tools.as_ref().map_or(0, |t| t.len()),
            has_routing = api_request.provider.is_some(),
            "OpenAI-compat API stream request"
        );

        let stream = self
            .http
            .post_stream("/chat/completions", &api_request)
            .await?;
        futures::pin_mut!(stream);

        let mut parser = SseParser::new();
        let mut tool_builders: HashMap<usize, ToolBuilder> = HashMap::new();

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.map_err(|e| Error::Stream(e.to_string()))?;
            let text = String::from_utf8_lossy(&chunk);

            for sse_event in parser.feed(&text) {
                if sse_event.data.is_empty() || sse_event.data == "[DONE]" {
                    continue;
                }

                match serde_json::from_str::<StreamChunk>(&sse_event.data) {
                    Ok(chunk) => {
                        self.handle_stream_chunk(chunk, &tx, &mut tool_builders)
                            .await?;
                    }
                    Err(e) => {
                        // Check if it's an error response
                        if let Some(msg) =
                            serde_json::from_str::<serde_json::Value>(&sse_event.data)
                                .ok()
                                .and_then(|v| {
                                    v.get("error")?.get("message")?.as_str().map(String::from)
                                })
                        {
                            return Err(Error::Api(msg));
                        }
                        tracing::warn!(
                            "Failed to parse OpenAI chunk: {e}\nData: {}",
                            sse_event.data
                        );
                    }
                }
            }
        }

        // Emit any remaining tool calls
        for (_, builder) in tool_builders {
            if let (Some(id), Some(name)) = (builder.id, builder.name) {
                let json_str: String = builder.argument_parts.concat();
                let arguments: serde_json::Value =
                    serde_json::from_str(&json_str).unwrap_or(serde_json::Value::Null);

                let _ = tx
                    .send(StreamEvent::ToolCall(ToolCallEvent {
                        id,
                        name,
                        arguments,
                    }))
                    .await;
            }
        }

        let _ = tx.send(StreamEvent::Done).await;
        Ok(())
    }

    /// Build an OpenAI-compatible request.
    fn build_request(
        &self,
        request: &ChatRequest,
        prefs: Option<&ProviderPrefs>,
        stream: bool,
    ) -> OpenAIRequest {
        let mut messages = Vec::new();

        for msg in request.messages.iter() {
            match msg.role {
                Role::System => {
                    let text = msg
                        .content
                        .iter()
                        .filter_map(|b| {
                            if let ContentBlock::Text { text } = b {
                                Some(text.as_str())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n");

                    if !text.is_empty() {
                        let role = if self.quirks.skip_developer_role {
                            "system"
                        } else {
                            "developer"
                        };
                        messages.push(OpenAIMessage {
                            role: role.to_string(),
                            content: MessageContent::Text(text),
                            name: None,
                            tool_calls: None,
                            tool_call_id: None,
                        });
                    }
                }
                Role::User => {
                    let content = self.convert_user_content(&msg.content);
                    messages.push(OpenAIMessage {
                        role: "user".to_string(),
                        content,
                        name: None,
                        tool_calls: None,
                        tool_call_id: None,
                    });
                }
                Role::Assistant => {
                    let (text, tool_calls) = self.extract_assistant_content(&msg.content);
                    messages.push(OpenAIMessage {
                        role: "assistant".to_string(),
                        content: MessageContent::Text(text),
                        name: None,
                        tool_calls,
                        tool_call_id: None,
                    });
                }
                Role::ToolResult => {
                    for block in msg.content.iter() {
                        if let ContentBlock::ToolResult {
                            tool_call_id,
                            content,
                            is_error,
                        } = block
                        {
                            // Encode error status in content if needed
                            let result_content = if *is_error {
                                format!("[ERROR] {content}")
                            } else {
                                content.clone()
                            };

                            messages.push(OpenAIMessage {
                                role: "tool".to_string(),
                                content: MessageContent::Text(result_content),
                                name: None,
                                tool_calls: None,
                                tool_call_id: Some(tool_call_id.clone()),
                            });
                        }
                    }
                }
            }
        }

        // Add explicit system prompt if provided
        if let Some(ref sys) = request.system {
            let role = if self.quirks.skip_developer_role {
                "system"
            } else {
                "developer"
            };
            // Insert at beginning
            messages.insert(
                0,
                OpenAIMessage {
                    role: role.to_string(),
                    content: MessageContent::Text(sys.to_string()),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                },
            );
        }

        // Convert tools
        let tools = if request.tools.is_empty() {
            None
        } else {
            Some(request.tools.iter().map(|t| self.convert_tool(t)).collect())
        };

        // Build provider routing if supported
        let provider = if self.quirks.supports_provider_routing {
            prefs.and_then(ProviderRouting::from_prefs)
        } else {
            None
        };

        let api_request = OpenAIRequest {
            model: request.model.clone(),
            messages,
            tools,
            max_tokens: request.max_tokens,
            max_completion_tokens: None,
            temperature: request.temperature,
            store: None,
            provider,
            stream,
        };

        api_request.apply_quirks(&self.quirks)
    }

    /// Convert user content blocks to OpenAI format.
    fn convert_user_content(&self, content: &[ContentBlock]) -> MessageContent {
        let mut parts = Vec::new();
        let mut has_image = false;

        for block in content {
            match block {
                ContentBlock::Text { text } => {
                    parts.push(ContentPart::Text { text: text.clone() });
                }
                ContentBlock::Image { media_type, data } => {
                    has_image = true;
                    parts.push(ContentPart::ImageUrl {
                        image_url: ImageUrl {
                            url: format!("data:{media_type};base64,{data}"),
                        },
                    });
                }
                _ => {}
            }
        }

        // Use simple text format if no images
        if !has_image
            && parts.len() == 1
            && let ContentPart::Text { text } = &parts[0]
        {
            return MessageContent::Text(text.clone());
        }

        MessageContent::Parts(parts)
    }

    /// Extract text and tool calls from assistant content.
    fn extract_assistant_content(
        &self,
        content: &[ContentBlock],
    ) -> (String, Option<Vec<ToolCall>>) {
        let mut text = String::new();
        let mut tool_calls = Vec::new();

        for block in content {
            match block {
                ContentBlock::Text { text: t } => {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(t);
                }
                ContentBlock::Thinking { thinking } => {
                    // Include thinking in text for providers that don't support it natively
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&format!("<thinking>{thinking}</thinking>"));
                }
                ContentBlock::ToolCall {
                    id,
                    name,
                    arguments,
                } => {
                    tool_calls.push(ToolCall {
                        id: id.clone(),
                        call_type: "function".to_string(),
                        function: FunctionCall {
                            name: name.clone(),
                            arguments: arguments.to_string(),
                        },
                    });
                }
                _ => {}
            }
        }

        let tool_calls = if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        };

        (text, tool_calls)
    }

    /// Convert a tool definition to OpenAI format.
    fn convert_tool(&self, tool: &ToolDefinition) -> OpenAITool {
        OpenAITool {
            tool_type: "function".to_string(),
            function: FunctionDefinition {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: tool.parameters.clone(),
            },
        }
    }

    /// Convert an API response to our common message type.
    fn convert_response(&self, response: OpenAIResponse) -> Message {
        let mut content_blocks = Vec::new();

        if let Some(choice) = response.choices.first() {
            // Handle reasoning content (DeepSeek, Kimi)
            if self.quirks.reasoning_field != ReasoningField::None
                && let Some(reasoning) = choice
                    .message
                    .reasoning_content
                    .as_ref()
                    .or(choice.message.reasoning.as_ref())
                && !reasoning.is_empty()
            {
                content_blocks.push(ContentBlock::Thinking {
                    thinking: reasoning.clone(),
                });
            }

            // Handle text content
            if let Some(text) = &choice.message.content
                && !text.is_empty()
            {
                content_blocks.push(ContentBlock::Text { text: text.clone() });
            }

            // Handle tool calls
            if let Some(tool_calls) = &choice.message.tool_calls {
                for tc in tool_calls {
                    let arguments = serde_json::from_str(&tc.function.arguments)
                        .inspect_err(|e| {
                            tracing::warn!(
                                "Malformed tool arguments for {}: {}",
                                tc.function.name,
                                e
                            );
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

        Message {
            role: Role::Assistant,
            content: Arc::new(content_blocks),
        }
    }

    /// Handle a streaming chunk.
    async fn handle_stream_chunk(
        &self,
        chunk: StreamChunk,
        tx: &mpsc::Sender<StreamEvent>,
        tool_builders: &mut HashMap<usize, ToolBuilder>,
    ) -> Result<(), Error> {
        for choice in chunk.choices {
            // Handle reasoning content (DeepSeek, Kimi)
            if self.quirks.reasoning_field != ReasoningField::None
                && let Some(reasoning) = choice
                    .delta
                    .reasoning_content
                    .as_ref()
                    .or(choice.delta.reasoning.as_ref())
                && !reasoning.is_empty()
            {
                let _ = tx.send(StreamEvent::ThinkingDelta(reasoning.clone())).await;
            }

            // Handle text content
            if let Some(text) = &choice.delta.content
                && !text.is_empty()
            {
                let _ = tx.send(StreamEvent::TextDelta(text.clone())).await;
            }

            // Handle tool calls
            if let Some(tool_calls) = &choice.delta.tool_calls {
                for tc in tool_calls {
                    let builder = tool_builders
                        .entry(tc.index)
                        .or_insert_with(ToolBuilder::new);

                    // Capture id and name when first seen
                    if let Some(ref id) = tc.id {
                        builder.id = Some(id.clone());
                    }
                    if let Some(ref func) = tc.function {
                        if let Some(ref name) = func.name {
                            builder.name = Some(name.clone());
                        }
                        if let Some(ref args) = func.arguments {
                            builder.argument_parts.push(args.clone());
                        }
                    }
                }
            }

            // Check for finish_reason = tool_calls
            if choice.finish_reason.as_deref() == Some("tool_calls") {
                // Emit all completed tool calls
                for (idx, builder) in tool_builders.drain() {
                    if let (Some(id), Some(name)) = (builder.id, builder.name) {
                        let json_str: String = builder.argument_parts.concat();
                        let arguments: serde_json::Value =
                            serde_json::from_str(&json_str).unwrap_or(serde_json::Value::Null);

                        tracing::debug!(
                            index = idx,
                            id = %id,
                            name = %name,
                            "Emitting tool call"
                        );

                        let _ = tx
                            .send(StreamEvent::ToolCall(ToolCallEvent {
                                id,
                                name,
                                arguments,
                            }))
                            .await;
                    }
                }
            }
        }

        // Handle usage at end of stream
        if let Some(usage) = chunk.usage {
            let _ = tx
                .send(StreamEvent::Usage(Usage {
                    input_tokens: usage.prompt_tokens,
                    output_tokens: usage.completion_tokens,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                }))
                .await;
        }

        Ok(())
    }
}

/// Helper for assembling tool calls from streamed deltas.
struct ToolBuilder {
    id: Option<String>,
    name: Option<String>,
    argument_parts: Vec<String>,
}

impl ToolBuilder {
    fn new() -> Self {
        Self {
            id: None,
            name: None,
            argument_parts: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_request_basic() {
        let client = OpenAICompatClient::new(Provider::OpenAI, "test-key").unwrap();

        let request = ChatRequest {
            model: "gpt-4".to_string(),
            messages: Arc::new(vec![Message {
                role: Role::User,
                content: Arc::new(vec![ContentBlock::Text {
                    text: "Hello".to_string(),
                }]),
            }]),
            system: None,
            tools: Arc::new(vec![]),
            max_tokens: Some(1024),
            temperature: None,
            thinking: None,
        };

        let api_request = client.build_request(&request, None, false);

        assert_eq!(api_request.model, "gpt-4");
        assert_eq!(api_request.messages.len(), 1);
        // OpenAI uses max_completion_tokens
        assert!(api_request.max_tokens.is_none());
        assert_eq!(api_request.max_completion_tokens, Some(1024));
    }

    #[test]
    fn test_build_request_with_system() {
        let client = OpenAICompatClient::new(Provider::OpenAI, "test-key").unwrap();

        let request = ChatRequest {
            model: "gpt-4".to_string(),
            messages: Arc::new(vec![
                Message {
                    role: Role::System,
                    content: Arc::new(vec![ContentBlock::Text {
                        text: "You are helpful".to_string(),
                    }]),
                },
                Message {
                    role: Role::User,
                    content: Arc::new(vec![ContentBlock::Text {
                        text: "Hi".to_string(),
                    }]),
                },
            ]),
            system: None,
            tools: Arc::new(vec![]),
            max_tokens: None,
            temperature: None,
            thinking: None,
        };

        let api_request = client.build_request(&request, None, false);

        // Should have developer role for OpenAI
        assert_eq!(api_request.messages[0].role, "developer");
    }

    #[test]
    fn test_build_request_groq_system_role() {
        let client = OpenAICompatClient::new(Provider::Groq, "test-key").unwrap();

        let request = ChatRequest {
            model: "llama-3.1-70b".to_string(),
            messages: Arc::new(vec![Message {
                role: Role::System,
                content: Arc::new(vec![ContentBlock::Text {
                    text: "You are helpful".to_string(),
                }]),
            }]),
            system: None,
            tools: Arc::new(vec![]),
            max_tokens: Some(1024),
            temperature: None,
            thinking: None,
        };

        let api_request = client.build_request(&request, None, false);

        // Groq should use system role, not developer
        assert_eq!(api_request.messages[0].role, "system");
        // Groq uses max_tokens
        assert_eq!(api_request.max_tokens, Some(1024));
        assert!(api_request.max_completion_tokens.is_none());
    }

    #[test]
    fn test_build_request_with_tools() {
        let client = OpenAICompatClient::new(Provider::OpenAI, "test-key").unwrap();

        let request = ChatRequest {
            model: "gpt-4".to_string(),
            messages: Arc::new(vec![Message {
                role: Role::User,
                content: Arc::new(vec![ContentBlock::Text {
                    text: "Read /etc/hosts".to_string(),
                }]),
            }]),
            system: None,
            tools: Arc::new(vec![ToolDefinition {
                name: "read_file".to_string(),
                description: "Read a file".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"}
                    }
                }),
            }]),
            max_tokens: None,
            temperature: None,
            thinking: None,
        };

        let api_request = client.build_request(&request, None, true);

        assert!(api_request.tools.is_some());
        assert_eq!(api_request.tools.as_ref().unwrap().len(), 1);
        assert!(api_request.stream);
    }

    #[test]
    fn test_build_request_with_tool_result() {
        let client = OpenAICompatClient::new(Provider::OpenAI, "test-key").unwrap();

        let request = ChatRequest {
            model: "gpt-4".to_string(),
            messages: Arc::new(vec![
                Message {
                    role: Role::Assistant,
                    content: Arc::new(vec![ContentBlock::ToolCall {
                        id: "call_123".to_string(),
                        name: "bash".to_string(),
                        arguments: serde_json::json!({"command": "ls"}),
                    }]),
                },
                Message {
                    role: Role::ToolResult,
                    content: Arc::new(vec![ContentBlock::ToolResult {
                        tool_call_id: "call_123".to_string(),
                        content: "file1.txt".to_string(),
                        is_error: false,
                    }]),
                },
            ]),
            system: None,
            tools: Arc::new(vec![]),
            max_tokens: None,
            temperature: None,
            thinking: None,
        };

        let api_request = client.build_request(&request, None, false);

        // Should have assistant message with tool_calls, then tool message
        assert_eq!(api_request.messages.len(), 2);
        assert_eq!(api_request.messages[1].role, "tool");
        assert_eq!(
            api_request.messages[1].tool_call_id.as_deref(),
            Some("call_123")
        );
    }

    #[test]
    fn test_openrouter_provider_routing() {
        let client = OpenAICompatClient::new(Provider::OpenRouter, "test-key").unwrap();

        let prefs = ProviderPrefs {
            order: Some(vec!["Anthropic".to_string()]),
            allow_fallbacks: false,
            ..Default::default()
        };

        let request = ChatRequest {
            model: "anthropic/claude-sonnet-4-20250514".to_string(),
            messages: Arc::new(vec![Message {
                role: Role::User,
                content: Arc::new(vec![ContentBlock::Text {
                    text: "Hi".to_string(),
                }]),
            }]),
            system: None,
            tools: Arc::new(vec![]),
            max_tokens: None,
            temperature: None,
            thinking: None,
        };

        let api_request = client.build_request(&request, Some(&prefs), false);

        assert!(api_request.provider.is_some());
        let routing = api_request.provider.unwrap();
        assert_eq!(routing.order, Some(vec!["Anthropic".to_string()]));
        assert_eq!(routing.allow_fallbacks, Some(false));
    }
}
