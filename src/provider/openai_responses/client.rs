//! OpenAI Responses API client.

use super::convert::{build_request, extract_output, extract_usage, parse_response_event};
use super::types::ParsedEvent;
use crate::provider::error::Error;
use crate::provider::http::{AuthConfig, HttpClient, SseParser};
use crate::provider::types::{
    ChatRequest, CompletionResponse, Message, Role, StreamEvent, ToolBuilder,
};
use futures::StreamExt;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::mpsc;

const OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

/// OpenAI Responses API client.
pub struct OpenAIResponsesClient {
    http: HttpClient,
}

impl OpenAIResponsesClient {
    /// Create a new OpenAI Responses client.
    pub fn new(api_key: impl Into<String>) -> Self {
        let http = HttpClient::new(OPENAI_BASE_URL, AuthConfig::Bearer(api_key.into()));
        Self { http }
    }

    /// Make a non-streaming responses request.
    pub async fn complete(&self, request: ChatRequest) -> Result<CompletionResponse, Error> {
        let body = build_request(&request, false);
        let value: serde_json::Value = self.http.post_json("/responses", &body).await?;

        let content = extract_output(&value);
        let usage = value
            .get("usage")
            .map(extract_usage)
            .unwrap_or_default();

        Ok(CompletionResponse {
            message: Message {
                role: Role::Assistant,
                content: Arc::new(content),
            },
            usage,
        })
    }

    /// Stream a responses request.
    pub async fn stream(
        &self,
        request: ChatRequest,
        tx: mpsc::Sender<StreamEvent>,
    ) -> Result<(), Error> {
        let body = build_request(&request, true);
        let stream = self.http.post_stream("/responses", &body).await?;
        futures::pin_mut!(stream);

        let mut parser = SseParser::new();
        // Track in-progress tool calls by item_id for incremental accumulation
        let mut tool_builders: HashMap<String, ToolBuilder> = HashMap::new();
        // Track tool calls already emitted to avoid double-emission
        // (arguments.done fires before output_item.done for the same call)
        let mut emitted_tool_ids: HashSet<String> = HashSet::new();

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.map_err(|e| Error::Stream(format!("Stream error: {e}")))?;
            let text = String::from_utf8_lossy(&chunk);

            for event in parser.feed(&text) {
                if event.data.is_empty() {
                    continue;
                }

                let parsed = parse_response_event(&event.data, event.event.as_deref());
                tracing::trace!(
                    event_type = ?event.event,
                    parsed = ?parsed.as_ref().map(std::mem::discriminant),
                    "SSE event"
                );
                let Some(parsed) = parsed else {
                    continue;
                };

                match parsed {
                    ParsedEvent::TextDelta(delta) => {
                        let _ = tx.send(StreamEvent::TextDelta(delta)).await;
                    }
                    ParsedEvent::ThinkingDelta(delta) => {
                        let _ = tx.send(StreamEvent::ThinkingDelta(delta)).await;
                    }
                    ParsedEvent::ToolCallDelta { call_id, delta } => {
                        let builder = tool_builders.entry(call_id.clone()).or_default();
                        if builder.id.is_none() {
                            builder.id = Some(call_id);
                        }
                        builder.push(delta);
                    }
                    ParsedEvent::ToolCallDone {
                        call_id,
                        name,
                        arguments,
                    } => {
                        // Prefer the accumulated builder if present, otherwise parse directly
                        if let Some(mut builder) = tool_builders.remove(&call_id) {
                            // Ensure name is set from the done event (deltas don't carry it)
                            if builder.name.is_none() && !name.is_empty() {
                                builder.name = Some(name.clone());
                            }
                            if let Some(call) = builder.finish() {
                                emitted_tool_ids.insert(call.id.clone());
                                let _ = tx.send(StreamEvent::ToolCall(call)).await;
                            }
                        } else {
                            let arguments = serde_json::from_str(&arguments)
                                .unwrap_or(serde_json::Value::Null);
                            emitted_tool_ids.insert(call_id.clone());
                            let _ = tx
                                .send(StreamEvent::ToolCall(
                                    crate::provider::types::ToolCallEvent {
                                        id: call_id,
                                        name,
                                        arguments,
                                    },
                                ))
                                .await;
                        }
                    }
                    ParsedEvent::ToolCall(call) => {
                        // From output_item.done — skip if already emitted via arguments.done
                        if !emitted_tool_ids.contains(&call.id) {
                            emitted_tool_ids.insert(call.id.clone());
                            let _ = tx.send(StreamEvent::ToolCall(call)).await;
                        }
                    }
                    ParsedEvent::Usage(usage) => {
                        let _ = tx.send(StreamEvent::Usage(usage)).await;
                        let _ = tx.send(StreamEvent::Done).await;
                        return Ok(());
                    }
                    ParsedEvent::Done => {
                        let _ = tx.send(StreamEvent::Done).await;
                        return Ok(());
                    }
                    ParsedEvent::Error(message) => {
                        return Err(Error::Stream(message));
                    }
                }
            }
        }

        // Drain any remaining tool builders
        for (_, builder) in tool_builders {
            if let Some(call) = builder.finish() {
                let _ = tx.send(StreamEvent::ToolCall(call)).await;
            }
        }

        let _ = tx.send(StreamEvent::Done).await;
        Ok(())
    }
}
