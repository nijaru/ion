//! Conversion between ion types and Anthropic API types.

use super::request::{
    AnthropicMessage, AnthropicRequest, AnthropicTool, CacheControl, ContentBlock, SystemBlock,
};
use super::response::{AnthropicResponse, ResponseBlock};
use super::stream::{ContentBlockInfo, ContentDelta, StreamEvent as AnthropicStreamEvent};
use crate::provider::error::Error;
use crate::provider::stream::ToolCallAccumulator;
use crate::provider::types::{
    ChatRequest, ContentBlock as IonContentBlock, Message, Role, StreamEvent, ToolDefinition,
    Usage as IonUsage,
};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Build an Anthropic API request from our common request type.
#[allow(clippy::too_many_lines)]
pub(crate) fn build_request(request: &ChatRequest, stream: bool) -> AnthropicRequest {
    let mut system_blocks = Vec::new();
    let mut messages = Vec::new();

    // Extract system messages and build system blocks (cache applied to last only)
    for msg in request.messages.iter() {
        match msg.role {
            Role::System => {
                for block in msg.content.iter() {
                    if let IonContentBlock::Text { text } = block {
                        system_blocks.push(SystemBlock::text(text.clone()));
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
                                cache_control: None,
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
        system_blocks.push(SystemBlock::text(sys.to_string()));
    }

    // Convert tools and place a cache breakpoint.
    // Anthropic caches in order: system -> tools -> messages. A breakpoint on
    // the last tool creates a cache prefix covering system + all tools together.
    // When no tools are present, fall back to caching the last system block.
    let mut tools: Option<Vec<AnthropicTool>> = if request.tools.is_empty() {
        None
    } else {
        Some(request.tools.iter().map(convert_tool).collect())
    };
    if let Some(ref mut tool_vec) = tools
        && let Some(last) = tool_vec.last_mut()
    {
        last.cache_control = Some(CacheControl::ephemeral());
    } else if let Some(last) = system_blocks.last_mut() {
        last.cache_control = Some(CacheControl::ephemeral());
    }

    // Place a cache breakpoint on the second-to-last real user message
    // (skip tool result messages which also have role "user").
    // This caches the conversation prefix from prior turns so only the
    // current turn is billed at full input rate.
    let mut user_turn_count = 0;
    for msg in messages.iter_mut().rev() {
        if msg.role != "user" {
            continue;
        }
        // Real user messages contain Text/Image; tool results contain only ToolResult
        let is_user_content = msg
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::Text { .. } | ContentBlock::Image { .. }));
        if !is_user_content {
            continue;
        }
        user_turn_count += 1;
        if user_turn_count == 2 {
            if let Some(
                ContentBlock::Text { cache_control, .. }
                | ContentBlock::Image { cache_control, .. },
            ) = msg.content.last_mut()
            {
                *cache_control = Some(CacheControl::ephemeral());
            }
            break;
        }
    }

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
pub(crate) fn convert_tool(tool: &ToolDefinition) -> AnthropicTool {
    AnthropicTool {
        name: tool.name.clone(),
        description: tool.description.clone(),
        input_schema: tool.parameters.clone(),
        cache_control: None,
    }
}

/// Convert an API response to our common message type.
pub(crate) fn convert_response(response: AnthropicResponse) -> Message {
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
pub(crate) async fn handle_stream_event(
    event: AnthropicStreamEvent,
    tx: &mpsc::Sender<StreamEvent>,
    tools: &mut ToolCallAccumulator,
) -> Result<(), Error> {
    match event {
        AnthropicStreamEvent::MessageStart { message } => {
            // Send initial usage if available
            let _ = tx
                .send(StreamEvent::Usage(IonUsage {
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
                tools.insert(index, crate::provider::types::ToolBuilder::with_id_name(id, name));
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
                tools.get_or_insert(index).push(partial_json);
            }
        },
        AnthropicStreamEvent::ContentBlockStop { index } => {
            if let Some(builder) = tools.remove(index)
                && let Some(call) = builder.finish()
            {
                let _ = tx.send(StreamEvent::ToolCall(call)).await;
            }
        }
        AnthropicStreamEvent::MessageDelta { usage, .. } => {
            let _ = tx
                .send(StreamEvent::Usage(IonUsage {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_request_with_system() {
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

        let api_request = build_request(&request, false);

        assert!(api_request.system.is_some());
        let system = api_request.system.unwrap();
        assert_eq!(system.len(), 1);
        // No tools -> system block gets the cache breakpoint
        assert!(system[0].cache_control.is_some());
    }

    #[test]
    fn test_build_request_with_tools() {
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

        let api_request = build_request(&request, true);

        assert!(api_request.tools.is_some());
        let tools = api_request.tools.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "read_file");
        assert!(api_request.stream);
    }

    fn make_tool(name: &str) -> ToolDefinition {
        ToolDefinition {
            name: name.to_string(),
            description: format!("{name} tool"),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }
    }

    #[test]
    fn test_last_tool_has_cache_control() {
        let request = ChatRequest {
            model: "claude-sonnet-4-20250514".to_string(),
            messages: Arc::new(vec![Message {
                role: Role::User,
                content: Arc::new(vec![IonContentBlock::Text {
                    text: "Hi".to_string(),
                }]),
            }]),
            system: None,
            tools: Arc::new(vec![
                make_tool("read"),
                make_tool("write"),
                make_tool("bash"),
            ]),
            max_tokens: None,
            temperature: None,
            thinking: None,
        };

        let api_request = build_request(&request, false);
        let tools = api_request.tools.unwrap();
        assert!(tools[0].cache_control.is_none());
        assert!(tools[1].cache_control.is_none());
        assert!(tools[2].cache_control.is_some()); // Only last tool cached
    }

    #[test]
    fn test_system_cache_fallback_without_tools() {
        let request = ChatRequest {
            model: "claude-sonnet-4-20250514".to_string(),
            messages: Arc::new(vec![
                Message {
                    role: Role::System,
                    content: Arc::new(vec![IonContentBlock::Text {
                        text: "System block 1".to_string(),
                    }]),
                },
                Message {
                    role: Role::User,
                    content: Arc::new(vec![IonContentBlock::Text {
                        text: "Hi".to_string(),
                    }]),
                },
            ]),
            system: Some("System block 2".into()),
            tools: Arc::new(vec![]),
            max_tokens: None,
            temperature: None,
            thinking: None,
        };

        let api_request = build_request(&request, false);
        let system = api_request.system.unwrap();
        assert_eq!(system.len(), 2);
        // No tools -> last system block gets the cache breakpoint
        assert!(system[0].cache_control.is_none());
        assert!(system[1].cache_control.is_some());
    }

    #[test]
    fn test_tool_breakpoint_covers_system() {
        let request = ChatRequest {
            model: "claude-sonnet-4-20250514".to_string(),
            messages: Arc::new(vec![
                Message {
                    role: Role::System,
                    content: Arc::new(vec![IonContentBlock::Text {
                        text: "System prompt".to_string(),
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
            tools: Arc::new(vec![make_tool("bash")]),
            max_tokens: None,
            temperature: None,
            thinking: None,
        };

        let api_request = build_request(&request, false);
        let system = api_request.system.unwrap();
        // Tool breakpoint covers system, so no system breakpoint needed
        assert!(system[0].cache_control.is_none());
        // Tool has the breakpoint
        assert!(api_request.tools.unwrap()[0].cache_control.is_some());
    }

    #[test]
    fn test_history_cache_skips_tool_results() {
        let request = ChatRequest {
            model: "claude-sonnet-4-20250514".to_string(),
            messages: Arc::new(vec![
                Message {
                    role: Role::User,
                    content: Arc::new(vec![IonContentBlock::Text {
                        text: "First question".to_string(),
                    }]),
                },
                Message {
                    role: Role::Assistant,
                    content: Arc::new(vec![IonContentBlock::ToolCall {
                        id: "call_1".to_string(),
                        name: "bash".to_string(),
                        arguments: serde_json::json!({"command": "ls"}),
                    }]),
                },
                Message {
                    role: Role::ToolResult,
                    content: Arc::new(vec![IonContentBlock::ToolResult {
                        tool_call_id: "call_1".to_string(),
                        content: "file.txt".to_string(),
                        is_error: false,
                    }]),
                },
                Message {
                    role: Role::Assistant,
                    content: Arc::new(vec![IonContentBlock::Text {
                        text: "Found file.txt".to_string(),
                    }]),
                },
                Message {
                    role: Role::User,
                    content: Arc::new(vec![IonContentBlock::Text {
                        text: "Second question".to_string(),
                    }]),
                },
            ]),
            system: None,
            tools: Arc::new(vec![]),
            max_tokens: None,
            temperature: None,
            thinking: None,
        };

        let api_request = build_request(&request, false);

        // messages[0] = "First question" (user) -- should get cache breakpoint
        // messages[1] = tool call (assistant)
        // messages[2] = tool result (user role but NOT a real user message)
        // messages[3] = "Found file.txt" (assistant)
        // messages[4] = "Second question" (user) -- most recent, no cache

        // First user message should have cache_control (second-to-last real user turn)
        if let ContentBlock::Text { cache_control, .. } = &api_request.messages[0].content[0] {
            assert!(
                cache_control.is_some(),
                "First user message should be cached"
            );
        } else {
            panic!("Expected Text block");
        }

        // Tool result message should NOT have cache_control
        if let ContentBlock::ToolResult { .. } = &api_request.messages[2].content[0] {
            // ToolResult doesn't have cache_control field, so this is fine
        } else {
            panic!("Expected ToolResult block");
        }

        // Latest user message should NOT have cache_control
        if let ContentBlock::Text { cache_control, .. } = &api_request.messages[4].content[0] {
            assert!(
                cache_control.is_none(),
                "Latest user message should not be cached"
            );
        } else {
            panic!("Expected Text block");
        }
    }

    #[test]
    fn test_single_user_message_no_history_cache() {
        let request = ChatRequest {
            model: "claude-sonnet-4-20250514".to_string(),
            messages: Arc::new(vec![Message {
                role: Role::User,
                content: Arc::new(vec![IonContentBlock::Text {
                    text: "Hello".to_string(),
                }]),
            }]),
            system: None,
            tools: Arc::new(vec![]),
            max_tokens: None,
            temperature: None,
            thinking: None,
        };

        let api_request = build_request(&request, false);

        // Only one user message -- no history cache breakpoint
        if let ContentBlock::Text { cache_control, .. } = &api_request.messages[0].content[0] {
            assert!(cache_control.is_none());
        }
    }

    #[test]
    fn test_build_request_with_tool_result() {
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

        let api_request = build_request(&request, false);

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
