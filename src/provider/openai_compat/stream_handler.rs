//! Stream handling for OpenAI-compatible API.

use super::quirks::{ProviderQuirks, ReasoningField};
use super::response::OpenAIResponse;
use super::stream::StreamChunk;
use crate::provider::error::Error;
use crate::provider::types::{
    ContentBlock, Message, Role, StreamEvent, ToolBuilder, Usage,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Handle a streaming chunk.
pub(crate) async fn handle_stream_chunk(
    chunk: StreamChunk,
    tx: &mpsc::Sender<StreamEvent>,
    tool_builders: &mut HashMap<usize, ToolBuilder>,
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
                let builder = tool_builders
                    .entry(tc.index)
                    .or_default();

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
            for (idx, builder) in tool_builders.drain() {
                if let Some(call) = builder.finish() {
                    tracing::debug!(index = idx, id = %call.id, name = %call.name, "Emitting tool call");
                    let _ = tx.send(StreamEvent::ToolCall(call)).await;
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
