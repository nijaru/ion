//! Anthropic Messages API client.

use super::request::{
    AnthropicMessage, AnthropicRequest, AnthropicTool, ContentBlock, SystemBlock,
};
use super::response::{AnthropicResponse, ResponseBlock};
use super::stream::{ContentBlockInfo, ContentDelta, StreamEvent as AnthropicStreamEvent};
use crate::provider::error::Error;
use crate::provider::http::{AuthConfig, HttpClient, SseParser};
use crate::provider::types::{
    ChatRequest, ContentBlock as IonContentBlock, Message, Role, StreamEvent, ToolBuilder,
    ToolDefinition, Usage,
};
use futures::StreamExt;
use std::sync::Arc;
use tokio::sync::mpsc;

const BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";

/// Native Anthropic Messages API client.
pub struct AnthropicClient {
    http: HttpClient,
}

impl AnthropicClient {
    /// Create a new Anthropic client.
    pub fn new(api_key: impl Into<String>) -> Self {
        let api_key = api_key.into();
        let mut http = HttpClient::new(
            BASE_URL,
            AuthConfig::ApiKey {
                header: "x-api-key".to_string(),
                key: api_key,
            },
        );

        // Add Anthropic-specific headers
        let mut headers = reqwest::header::HeaderMap::new();
        if let Ok(version) = reqwest::header::HeaderValue::from_str(API_VERSION) {
            headers.insert("anthropic-version", version);
        }
        http = http.with_extra_headers(headers);

        Self { http }
    }

    /// Make a non-streaming chat completion request.
    pub async fn complete(&self, request: ChatRequest) -> Result<Message, Error> {
        let api_request = self.build_request(&request, false);

        tracing::debug!(
            model = %api_request.model,
            messages = api_request.messages.len(),
            tools = api_request.tools.as_ref().map_or(0, std::vec::Vec::len),
            "Anthropic API request"
        );

        let response: AnthropicResponse = self.http.post_json("/v1/messages", &api_request).await?;

        Ok(self.convert_response(response))
    }

    /// Stream a chat completion request.
    pub async fn stream(
        &self,
        request: ChatRequest,
        tx: mpsc::Sender<StreamEvent>,
    ) -> Result<(), Error> {
        let api_request = self.build_request(&request, true);

        tracing::debug!(
            model = %api_request.model,
            messages = api_request.messages.len(),
            tools = api_request.tools.as_ref().map_or(0, std::vec::Vec::len),
            "Anthropic API stream request"
        );

        let stream = self.http.post_stream("/v1/messages", &api_request).await?;
        futures::pin_mut!(stream);

        let mut parser = SseParser::new();
        let mut tool_builders: std::collections::HashMap<usize, ToolBuilder> =
            std::collections::HashMap::new();

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.map_err(|e| Error::Stream(e.to_string()))?;
            let text = String::from_utf8_lossy(&chunk);

            for sse_event in parser.feed(&text) {
                if sse_event.data.is_empty() || sse_event.data == "[DONE]" {
                    continue;
                }

                match serde_json::from_str::<AnthropicStreamEvent>(&sse_event.data) {
                    Ok(event) => {
                        if let Err(e) = self
                            .handle_stream_event(event, &tx, &mut tool_builders)
                            .await
                        {
                            let _ = tx.send(StreamEvent::Error(e.to_string())).await;
                            return Err(e);
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to parse Anthropic event: {e}\nData: {}",
                            sse_event.data
                        );
                    }
                }
            }
        }

        let _ = tx.send(StreamEvent::Done).await;
        Ok(())
    }

    /// Build an Anthropic API request from our common request type.
    #[allow(clippy::too_many_lines)]
    fn build_request(&self, request: &ChatRequest, stream: bool) -> AnthropicRequest {
        let mut system_blocks = Vec::new();
        let mut messages = Vec::new();

        // Extract system messages and build system blocks with cache control
        for msg in request.messages.iter() {
            match msg.role {
                Role::System => {
                    for block in msg.content.iter() {
                        if let IonContentBlock::Text { text } = block {
                            // Mark system prompts for caching
                            system_blocks.push(SystemBlock::text(text.clone()).with_cache());
                        }
                    }
                }
                Role::User => {
                    let content: Vec<ContentBlock> = msg
                        .content
                        .iter()
                        .filter_map(|b| match b {
                            IonContentBlock::Text { text } => Some(ContentBlock::Text {
                                text: text.clone(),
                                cache_control: None,
                            }),
                            IonContentBlock::Image { media_type, data } => {
                                Some(ContentBlock::Image {
                                    source: super::request::ImageSource {
                                        source_type: "base64".to_string(),
                                        media_type: media_type.clone(),
                                        data: data.clone(),
                                    },
                                })
                            }
                            _ => None,
                        })
                        .collect();

                    if !content.is_empty() {
                        messages.push(AnthropicMessage {
                            role: "user".to_string(),
                            content,
                        });
                    }
                }
                Role::Assistant => {
                    let content: Vec<ContentBlock> = msg
                        .content
                        .iter()
                        .filter_map(|b| match b {
                            IonContentBlock::Text { text } => Some(ContentBlock::Text {
                                text: text.clone(),
                                cache_control: None,
                            }),
                            IonContentBlock::ToolCall {
                                id,
                                name,
                                arguments,
                            } => Some(ContentBlock::ToolUse {
                                id: id.clone(),
                                name: name.clone(),
                                input: arguments.clone(),
                            }),
                            _ => None,
                        })
                        .collect();

                    if !content.is_empty() {
                        messages.push(AnthropicMessage {
                            role: "assistant".to_string(),
                            content,
                        });
                    }
                }
                Role::ToolResult => {
                    let content: Vec<ContentBlock> = msg
                        .content
                        .iter()
                        .filter_map(|b| {
                            if let IonContentBlock::ToolResult {
                                tool_call_id,
                                content,
                                is_error,
                            } = b
                            {
                                Some(ContentBlock::ToolResult {
                                    tool_use_id: tool_call_id.clone(),
                                    content: content.clone(),
                                    is_error: *is_error,
                                })
                            } else {
                                None
                            }
                        })
                        .collect();

                    if !content.is_empty() {
                        messages.push(AnthropicMessage {
                            role: "user".to_string(),
                            content,
                        });
                    }
                }
            }
        }

        // Also include explicit system prompt if provided
        if let Some(ref sys) = request.system {
            system_blocks.push(SystemBlock::text(sys.to_string()).with_cache());
        }

        // Convert tools
        let tools = if request.tools.is_empty() {
            None
        } else {
            Some(request.tools.iter().map(|t| self.convert_tool(t)).collect())
        };

        AnthropicRequest {
            model: request.model.clone(),
            max_tokens: request.max_tokens.unwrap_or(8192),
            system: if system_blocks.is_empty() {
                None
            } else {
                Some(system_blocks)
            },
            messages,
            tools,
            temperature: request.temperature,
            stream,
        }
    }

    /// Convert a tool definition to Anthropic format.
    #[allow(clippy::unused_self)]
    fn convert_tool(&self, tool: &ToolDefinition) -> AnthropicTool {
        AnthropicTool {
            name: tool.name.clone(),
            description: tool.description.clone(),
            input_schema: tool.parameters.clone(),
        }
    }

    /// Convert an API response to our common message type.
    #[allow(clippy::unused_self)]
    fn convert_response(&self, response: AnthropicResponse) -> Message {
        let content_blocks: Vec<IonContentBlock> = response
            .content
            .into_iter()
            .map(|block| match block {
                ResponseBlock::Text { text } => IonContentBlock::Text { text },
                ResponseBlock::Thinking { thinking } => IonContentBlock::Thinking { thinking },
                ResponseBlock::ToolUse { id, name, input } => IonContentBlock::ToolCall {
                    id,
                    name,
                    arguments: input,
                },
            })
            .collect();

        Message {
            role: Role::Assistant,
            content: Arc::new(content_blocks),
        }
    }

    /// Handle a single stream event.
    async fn handle_stream_event(
        &self,
        event: AnthropicStreamEvent,
        tx: &mpsc::Sender<StreamEvent>,
        tool_builders: &mut std::collections::HashMap<usize, ToolBuilder>,
    ) -> Result<(), Error> {
        match event {
            AnthropicStreamEvent::MessageStart { message } => {
                // Send initial usage if available
                let _ = tx
                    .send(StreamEvent::Usage(Usage {
                        input_tokens: message.usage.input_tokens,
                        output_tokens: message.usage.output_tokens,
                        cache_read_tokens: message.usage.cache_read_input_tokens,
                        cache_write_tokens: message.usage.cache_creation_input_tokens,
                    }))
                    .await;
            }
            AnthropicStreamEvent::ContentBlockStart {
                index,
                content_block,
            } => {
                // Track tool use blocks for later assembly
                if let ContentBlockInfo::ToolUse { id, name } = content_block {
                    tool_builders.insert(index, ToolBuilder::with_id_name(id, name));
                }
            }
            AnthropicStreamEvent::ContentBlockDelta { index, delta } => match delta {
                ContentDelta::Text { text } => {
                    let _ = tx.send(StreamEvent::TextDelta(text)).await;
                }
                ContentDelta::Thinking { thinking } => {
                    let _ = tx.send(StreamEvent::ThinkingDelta(thinking)).await;
                }
                ContentDelta::InputJson { partial_json } => {
                    if let Some(builder) = tool_builders.get_mut(&index) {
                        builder.push(partial_json);
                    }
                }
            },
            AnthropicStreamEvent::ContentBlockStop { index } => {
                if let Some(builder) = tool_builders.remove(&index)
                    && let Some(call) = builder.finish()
                {
                    let _ = tx.send(StreamEvent::ToolCall(call)).await;
                }
            }
            AnthropicStreamEvent::MessageDelta { usage, .. } => {
                let _ = tx
                    .send(StreamEvent::Usage(Usage {
                        input_tokens: usage.input_tokens,
                        output_tokens: usage.output_tokens,
                        cache_read_tokens: usage.cache_read_input_tokens,
                        cache_write_tokens: usage.cache_creation_input_tokens,
                    }))
                    .await;
            }
            AnthropicStreamEvent::MessageStop | AnthropicStreamEvent::Ping => {
                // MessageStop: stream end handled by caller
                // Ping: keepalive, ignore
            }
            AnthropicStreamEvent::Error { error } => {
                return Err(Error::Api(format!(
                    "{}: {}",
                    error.error_type, error.message
                )));
            }
        }
        Ok(())
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_request_with_system() {
        let client = AnthropicClient::new("test-key");

        let request = ChatRequest {
            model: "claude-sonnet-4-20250514".to_string(),
            messages: Arc::new(vec![
                Message {
                    role: Role::System,
                    content: Arc::new(vec![IonContentBlock::Text {
                        text: "You are helpful".to_string(),
                    }]),
                },
                Message {
                    role: Role::User,
                    content: Arc::new(vec![IonContentBlock::Text {
                        text: "Hi".to_string(),
                    }]),
                },
            ]),
            system: None,
            tools: Arc::new(vec![]),
            max_tokens: Some(1024),
            temperature: None,
            thinking: None,
        };

        let api_request = client.build_request(&request, false);

        assert!(api_request.system.is_some());
        let system = api_request.system.unwrap();
        assert_eq!(system.len(), 1);
        assert!(system[0].cache_control.is_some()); // Should have cache_control
    }

    #[test]
    fn test_build_request_with_tools() {
        let client = AnthropicClient::new("test-key");

        let request = ChatRequest {
            model: "claude-sonnet-4-20250514".to_string(),
            messages: Arc::new(vec![Message {
                role: Role::User,
                content: Arc::new(vec![IonContentBlock::Text {
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
                    },
                    "required": ["path"]
                }),
            }]),
            max_tokens: None,
            temperature: None,
            thinking: None,
        };

        let api_request = client.build_request(&request, true);

        assert!(api_request.tools.is_some());
        let tools = api_request.tools.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "read_file");
        assert!(api_request.stream);
    }

    #[test]
    fn test_build_request_with_tool_result() {
        let client = AnthropicClient::new("test-key");

        let request = ChatRequest {
            model: "claude-sonnet-4-20250514".to_string(),
            messages: Arc::new(vec![
                Message {
                    role: Role::User,
                    content: Arc::new(vec![IonContentBlock::Text {
                        text: "Hi".to_string(),
                    }]),
                },
                Message {
                    role: Role::Assistant,
                    content: Arc::new(vec![IonContentBlock::ToolCall {
                        id: "call_123".to_string(),
                        name: "bash".to_string(),
                        arguments: serde_json::json!({"command": "ls"}),
                    }]),
                },
                Message {
                    role: Role::ToolResult,
                    content: Arc::new(vec![IonContentBlock::ToolResult {
                        tool_call_id: "call_123".to_string(),
                        content: "file1.txt\nfile2.txt".to_string(),
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

        let api_request = client.build_request(&request, false);

        // Should have 3 messages: user, assistant (with tool_use), user (with tool_result)
        assert_eq!(api_request.messages.len(), 3);

        // Check tool result is in user message
        let tool_result_msg = &api_request.messages[2];
        assert_eq!(tool_result_msg.role, "user");
        if let ContentBlock::ToolResult {
            tool_use_id,
            is_error,
            ..
        } = &tool_result_msg.content[0]
        {
            assert_eq!(tool_use_id, "call_123");
            assert!(!is_error);
        } else {
            panic!("Expected ToolResult content block");
        }
    }
}
