//! Request building helpers for OpenAI-compatible API.

use super::quirks::ProviderQuirks;
use super::request::{
    ContentPart, FunctionCall, FunctionDefinition, ImageUrl, MessageContent, OpenAIMessage,
    OpenAIRequest, OpenAITool, ProviderRouting, ToolCall,
};
use crate::provider::prefs::ProviderPrefs;
use crate::provider::types::{ChatRequest, ContentBlock, Role, ToolDefinition};

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
