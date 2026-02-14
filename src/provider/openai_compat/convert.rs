//! Request building and stream handling for OpenAI-compatible API.

use super::quirks::{ProviderQuirks, ReasoningField};
use super::request::{
    ContentPart, FunctionCall, FunctionDefinition, ImageUrl, MessageContent, OpenAIMessage,
    OpenAIRequest, OpenAITool, ProviderRouting, ToolCall,
};
use super::response::OpenAIResponse;
use super::stream::StreamChunk;
use crate::provider::error::Error;
use crate::provider::prefs::ProviderPrefs;
use crate::provider::stream::ToolCallAccumulator;
use crate::provider::types::{
    ChatRequest, ContentBlock, Message, Role, StreamEvent, ToolDefinition, Usage,
};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Build an `OpenAI`-compatible request from a `ChatRequest`.
#[allow(clippy::too_many_lines)]
pub(crate) fn build_request(
    request: &ChatRequest,
    prefs: Option<&ProviderPrefs>,
    stream: bool,
    quirks: &ProviderQuirks,
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
                    let role = if quirks.skip_developer_role {
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
                let content = convert_user_content(&msg.content);
                messages.push(OpenAIMessage {
                    role: "user".to_string(),
                    content,
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                });
            }
            Role::Assistant => {
                let (text, tool_calls) = extract_assistant_content(&msg.content);
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
        let role = if quirks.skip_developer_role {
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
        Some(request.tools.iter().map(convert_tool).collect())
    };

    // Build provider routing if supported
    let provider = if quirks.supports_provider_routing {
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

    api_request.apply_quirks(quirks)
}

/// Convert user content blocks to `OpenAI` format.
pub(crate) fn convert_user_content(content: &[ContentBlock]) -> MessageContent {
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
pub(crate) fn extract_assistant_content(
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
                use std::fmt::Write;
                if !text.is_empty() {
                    text.push('\n');
                }
                let _ = write!(text, "<thinking>{thinking}</thinking>");
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

/// Convert a tool definition to `OpenAI` format.
pub(crate) fn convert_tool(tool: &ToolDefinition) -> OpenAITool {
    OpenAITool {
        tool_type: "function".to_string(),
        function: FunctionDefinition {
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters: tool.parameters.clone(),
        },
    }
}

/// Handle a streaming chunk.
pub(crate) async fn handle_stream_chunk(
    chunk: StreamChunk,
    tx: &mpsc::Sender<StreamEvent>,
    tools: &mut ToolCallAccumulator,
    quirks: &ProviderQuirks,
) -> Result<(), Error> {
    for choice in chunk.choices {
        // Handle reasoning content (DeepSeek, Kimi)
        if quirks.reasoning_field != ReasoningField::None
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
                let builder = tools.get_or_insert(tc.index);

                // Capture id and name when first seen
                if let Some(ref id) = tc.id {
                    builder.id = Some(id.clone());
                }
                if let Some(ref func) = tc.function {
                    if let Some(ref name) = func.name {
                        builder.name = Some(name.clone());
                    }
                    if let Some(ref args) = func.arguments {
                        builder.push(args.clone());
                    }
                }
            }
        }

        // Check for finish_reason = tool_calls
        if choice.finish_reason.as_deref() == Some("tool_calls") {
            tools.drain_finished(tx).await;
        }
    }

    // Handle usage at end of stream
    if let Some(usage) = chunk.usage {
        let cache_read_tokens = usage
            .prompt_tokens_details
            .as_ref()
            .map_or(0, |d| d.cached_tokens);
        let _ = tx
            .send(StreamEvent::Usage(Usage {
                input_tokens: usage.prompt_tokens,
                output_tokens: usage.completion_tokens,
                cache_read_tokens,
                cache_write_tokens: 0,
            }))
            .await;
    }

    Ok(())
}

/// Convert an API response to our common message type.
pub(crate) fn convert_response(response: &OpenAIResponse, quirks: &ProviderQuirks) -> Message {
    let mut content_blocks = Vec::new();

    if let Some(choice) = response.choices.first() {
        // Handle reasoning content (DeepSeek, Kimi)
        if quirks.reasoning_field != ReasoningField::None
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
                        tracing::warn!("Malformed tool arguments for {}: {}", tc.function.name, e);
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
