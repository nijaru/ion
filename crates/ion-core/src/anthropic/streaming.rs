//! Framing and semantic state for one Messages response. No incomplete tool call
//! escapes this owner; only message_stop can publish a validated response.
use std::collections::BTreeSet;

use futures_util::StreamExt;
use ion_ai::{
    Content, IncompleteReason, Message, ModelResponse, ModelStream, ModelStreamEvent,
    ProviderError, ProviderErrorKind, ResponseTermination, Role, ToolCall, Usage,
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::{error, invalid, unsupported, valid_name};

pub(super) const MAX_FRAME: usize = 256 * 1024;
pub(super) const MAX_RESPONSE: usize = 8 * 1024 * 1024;

pub(super) fn make_stream<S>(mut source: S, stop: CancellationToken) -> ModelStream
where
    S: futures_util::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Unpin + Send + 'static,
{
    Box::pin(async_stream::stream! {
        let mut frame = Vec::new();
        let mut total = 0usize;
        let mut state = ResponseState::default();
        loop {
            let next = tokio::select! {
                biased;
                _ = stop.cancelled() => { yield Err(error("stream interrupted", ProviderErrorKind::Cancelled)); return; }
                value = source.next() => value,
            };
            let chunk = match next {
                Some(Ok(chunk)) => chunk,
                Some(Err(_)) => { yield Err(error("stream transport interrupted", ProviderErrorKind::Transport)); return; }
                None => { yield Err(error("stream ended before message_stop", ProviderErrorKind::Transport)); return; }
            };
            // Incremental framing bounds storage even when a single transport chunk
            // contains many frames. Each byte is scanned once, including comments/pings.
            for byte in chunk {
                if total == MAX_RESPONSE { yield Err(invalid("response exceeded byte limit")); return; }
                total += 1;
                if frame.len() == MAX_FRAME { yield Err(invalid("SSE frame exceeded limit")); return; }
                frame.push(byte);
                if !frame.ends_with(b"\n\n") && !frame.ends_with(b"\r\n\r\n") { continue; }
                if stop.is_cancelled() { yield Err(error("stream interrupted", ProviderErrorKind::Cancelled)); return; }
                let event = match decode_frame(&frame).and_then(|value| value.map(|v| state.accept(v)).transpose()) {
                    Ok(event) => event.flatten(),
                    Err(error) => { yield Err(error); return; }
                };
                frame.clear();
                if let Some(event) = event {
                    let terminal = matches!(event, ModelStreamEvent::Completed(_));
                    if terminal {
                        // Close the owned HTTP stream before publishing completion;
                        // neither EOF nor events after this terminal belong to the attempt.
                        drop(source);
                        yield Ok(event);
                        return;
                    }
                    yield Ok(event);
                }
            }
        }
    })
}

fn decode_frame(frame: &[u8]) -> Result<Option<Value>, ProviderError> {
    let frame = std::str::from_utf8(frame).map_err(|_| invalid("SSE frame is not UTF-8"))?;
    let mut event = None;
    let mut data = String::new();
    for line in frame.lines() {
        if line.starts_with(':') || line.is_empty() {
            continue;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => {
                if event.replace(value).is_some() {
                    return Err(invalid("duplicate SSE event field"));
                }
            }
            "data" => {
                data.push_str(value);
                data.push('\n');
            }
            "id" | "retry" => {} // No reconnect or hidden retry behavior.
            _ => return Err(unsupported("unknown SSE field")),
        }
    }
    if event.is_none() && data.is_empty() {
        return Ok(None);
    }
    if event == Some("error") {
        return Err(error(
            "provider sent an SSE error",
            ProviderErrorKind::Transport,
        ));
    }
    let value: Value = serde_json::from_str(&data).map_err(|_| invalid("malformed SSE JSON"))?;
    if event.is_none() || value["type"].as_str() != event {
        return Err(invalid("SSE event and data type disagree"));
    }
    Ok(Some(value))
}

#[derive(Default)]
struct ResponseState {
    model: Option<String>,
    blocks: Vec<Block>,
    ids: BTreeSet<String>,
    usage: TokenCounts,
    in_message_delta: bool,
    finish: Option<String>,
}

enum Block {
    Text(String),
    Tool {
        id: String,
        name: String,
        initial: Value,
        fragments: Option<String>,
    },
    Closed(Content),
}

impl ResponseState {
    fn accept(&mut self, value: Value) -> Result<Option<ModelStreamEvent>, ProviderError> {
        let kind = string(&value, "type")?;
        if kind == "error" {
            return Err(error(
                "provider sent an SSE error",
                ProviderErrorKind::Transport,
            ));
        }
        if kind == "ping" {
            return Ok(None);
        }
        if kind == "message_start" {
            if self.model.is_some() {
                return Err(invalid("duplicate message_start"));
            }
            let message = &value["message"];
            if message["type"] != "message"
                || message["role"] != "assistant"
                || !message["content"].as_array().is_some_and(Vec::is_empty)
                || !message["stop_reason"].is_null()
                || !message["stop_sequence"].is_null()
            {
                return Err(invalid("invalid message_start"));
            }
            let model = string(message, "model")?;
            if model.is_empty() || string(message, "id")?.is_empty() {
                return Err(invalid("missing message identity"));
            }
            self.model = Some(model.to_owned());
            self.check_model(&value)?;
            self.usage.update(&message["usage"], true)?;
            return Ok(Some(ModelStreamEvent::Usage(self.usage.normalized()?)));
        }
        if self.model.is_none() {
            return Err(invalid("event before message_start"));
        }
        self.check_model(&value)?;
        match kind {
            "content_block_start" => {
                if self.in_message_delta || index(&value)? != self.blocks.len() {
                    return Err(invalid("late, duplicate or noncontiguous content block"));
                }
                let block = &value["content_block"];
                let (block, preview) = match string(block, "type")? {
                    "text" => {
                        if block.get("citations").is_some_and(|v| {
                            !v.is_null() && !v.as_array().is_some_and(Vec::is_empty)
                        }) {
                            return Err(unsupported("text citations are unsupported"));
                        }
                        let text = string(block, "text")?.to_owned();
                        let preview = if text.is_empty() {
                            None
                        } else {
                            Some(ModelStreamEvent::TextDelta(text.clone()))
                        };
                        (Block::Text(text), preview)
                    }
                    "tool_use" => {
                        let id = string(block, "id")?;
                        let name = string(block, "name")?;
                        if id.is_empty()
                            || !id
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                            || !valid_name(name)
                            || !block["input"].is_object()
                            || !self.ids.insert(id.to_owned())
                        {
                            return Err(invalid("invalid or duplicate tool_use"));
                        }
                        if block
                            .get("caller")
                            .is_some_and(|v| !v.is_null() && v["type"] != "direct")
                            || block.get("toolset_name").is_some_and(|v| !v.is_null())
                        {
                            return Err(unsupported("provider-hosted tool calls are unsupported"));
                        }
                        (
                            Block::Tool {
                                id: id.into(),
                                name: name.into(),
                                initial: block["input"].clone(),
                                fragments: None,
                            },
                            None,
                        )
                    }
                    _ => return Err(unsupported("unsupported content block type")),
                };
                self.blocks.push(block);
                Ok(preview)
            }
            "content_block_delta" => {
                if self.in_message_delta {
                    return Err(invalid("content delta after message_delta"));
                }
                let block = self
                    .blocks
                    .get_mut(index(&value)?)
                    .ok_or_else(|| invalid("delta for unknown block"))?;
                let delta = &value["delta"];
                match (block, string(delta, "type")?) {
                    (Block::Text(text), "text_delta") => {
                        let part = string(delta, "text")?;
                        text.push_str(part);
                        Ok(Some(ModelStreamEvent::TextDelta(part.into())))
                    }
                    (
                        Block::Tool {
                            initial, fragments, ..
                        },
                        "input_json_delta",
                    ) => {
                        if !initial.as_object().is_some_and(serde_json::Map::is_empty) {
                            return Err(invalid("ambiguous initial input and JSON fragments"));
                        }
                        fragments
                            .get_or_insert_default()
                            .push_str(string(delta, "partial_json")?);
                        Ok(None)
                    }
                    _ => Err(unsupported("unsupported or mismatched content delta")),
                }
            }
            "content_block_stop" => {
                if self.in_message_delta {
                    return Err(invalid("block stop after message_delta"));
                }
                let block = self
                    .blocks
                    .get_mut(index(&value)?)
                    .ok_or_else(|| invalid("stop for unknown block"))?;
                // Validate first, then move the owned block into its immutable result.
                let parsed = match block {
                    Block::Tool {
                        fragments: Some(json),
                        ..
                    } => {
                        let parsed: Value = serde_json::from_str(json)
                            .map_err(|_| invalid("malformed tool input JSON"))?;
                        if !parsed.is_object() {
                            return Err(invalid("tool input must be an object"));
                        }
                        Some(parsed)
                    }
                    Block::Closed(_) => return Err(invalid("duplicate content_block_stop")),
                    _ => None,
                };
                let content = match block {
                    Block::Text(text) => Content::Text(std::mem::take(text)),
                    Block::Tool {
                        id, name, initial, ..
                    } => Content::ToolCall(ToolCall {
                        id: std::mem::take(id),
                        name: std::mem::take(name),
                        arguments: parsed.unwrap_or_else(|| std::mem::take(initial)),
                    }),
                    Block::Closed(_) => unreachable!("closed blocks rejected above"),
                };
                *block = Block::Closed(content);
                Ok(None)
            }
            "message_delta" => {
                if !self.in_message_delta
                    && self.blocks.iter().any(|b| !matches!(b, Block::Closed(_)))
                {
                    return Err(invalid("unfinished content block at message_delta"));
                }
                self.in_message_delta = true;
                let delta = value["delta"]
                    .as_object()
                    .ok_or_else(|| invalid("missing message delta"))?;
                if delta
                    .keys()
                    .any(|k| !matches!(k.as_str(), "stop_reason" | "stop_sequence" | "model"))
                {
                    return Err(unsupported("unsupported message delta field"));
                }
                if !value["delta"]["stop_sequence"].is_null() {
                    return Err(unsupported("unexpected stop sequence"));
                }
                if self.finish.is_some() && delta.contains_key("stop_reason") {
                    return Err(invalid("repeated or contradictory stop_reason"));
                }
                if let Some(reason) = delta.get("stop_reason").filter(|r| !r.is_null()) {
                    let reason = reason
                        .as_str()
                        .ok_or_else(|| invalid("invalid stop_reason"))?;
                    if !matches!(
                        reason,
                        "end_turn"
                            | "tool_use"
                            | "max_tokens"
                            | "refusal"
                            | "model_context_window_exceeded"
                    ) {
                        return Err(unsupported("unsupported stop_reason"));
                    }
                    if (reason == "tool_use" && self.ids.is_empty())
                        || (reason == "end_turn" && !self.ids.is_empty())
                    {
                        return Err(invalid("stop_reason contradicts tool calls"));
                    }
                    self.finish = Some(reason.into());
                }
                self.usage.update(&value["usage"], false)?;
                Ok(Some(ModelStreamEvent::Usage(self.usage.normalized()?)))
            }
            "message_stop" => {
                if !self.in_message_delta {
                    return Err(invalid("message_stop before message_delta"));
                }
                let termination = match self.finish.as_deref() {
                    Some("end_turn" | "tool_use") => ResponseTermination::Completed,
                    Some("max_tokens") => {
                        ResponseTermination::Incomplete(IncompleteReason::MaxOutputTokens)
                    }
                    Some("refusal") => {
                        ResponseTermination::Incomplete(IncompleteReason::ContentFilter)
                    }
                    Some("model_context_window_exceeded") => {
                        ResponseTermination::Incomplete(IncompleteReason::ContextLength)
                    }
                    _ => return Err(invalid("message_stop without stop_reason")),
                };
                let mut content = Vec::with_capacity(self.blocks.len());
                for block in std::mem::take(&mut self.blocks) {
                    let Block::Closed(block) = block else {
                        return Err(invalid("unfinished content block"));
                    };
                    content.push(block);
                }
                Ok(Some(ModelStreamEvent::Completed(ModelResponse {
                    message: Message {
                        role: Role::Assistant,
                        content,
                        provider_replay: None,
                    },
                    usage: self.usage.normalized()?,
                    termination,
                    returned_model: self.model.take(),
                })))
            }
            _ => Err(unsupported("unsupported stream event type")),
        }
    }

    fn check_model(&self, event: &Value) -> Result<(), ProviderError> {
        for value in [
            event.get("model"),
            event["delta"].get("model"),
            event["message"].get("model"),
        ]
        .into_iter()
        .flatten()
        {
            if value.as_str() != self.model.as_deref() {
                return Err(invalid("returned model changed within one response"));
            }
        }
        // A stop reason outside message_delta is not a second way to settle content.
        if event.get("stop_reason").is_some_and(|v| !v.is_null())
            || (event["type"] != "message_delta"
                && event["delta"]
                    .get("stop_reason")
                    .is_some_and(|v| !v.is_null()))
        {
            return Err(invalid("stop_reason outside message_delta"));
        }
        Ok(())
    }
}

#[derive(Default)]
struct TokenCounts {
    input: u64,
    output: u64,
    creation: u64,
    read: u64,
}
impl TokenCounts {
    fn update(&mut self, value: &Value, initial: bool) -> Result<(), ProviderError> {
        let value = value.as_object().ok_or_else(|| invalid("invalid usage"))?;
        for (name, count) in [
            ("input_tokens", &mut self.input),
            ("output_tokens", &mut self.output),
            ("cache_creation_input_tokens", &mut self.creation),
            ("cache_read_input_tokens", &mut self.read),
        ] {
            if let Some(new) = value.get(name) {
                let new = new
                    .as_u64()
                    .ok_or_else(|| invalid("invalid usage token count"))?;
                if new < *count {
                    return Err(invalid("cumulative usage regressed"));
                }
                *count = new;
            } else if name == "output_tokens" || (initial && name == "input_tokens") {
                return Err(invalid("missing required token usage"));
            }
        }
        self.normalized()?;
        Ok(())
    }
    fn normalized(&self) -> Result<Usage, ProviderError> {
        // Messages input_tokens excludes cache hits/creation; Ion records total input.
        let input = self
            .input
            .checked_add(self.creation)
            .and_then(|n| n.checked_add(self.read))
            .ok_or_else(|| invalid("usage token count overflow"))?;
        Ok(Usage::known(input, self.output))
    }
}
fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str, ProviderError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("missing or invalid stream string field"))
}
fn index(value: &Value) -> Result<usize, ProviderError> {
    value["index"]
        .as_u64()
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| invalid("invalid content block index"))
}
