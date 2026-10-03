//! Native Anthropic Messages encoding, signed replay and stream state.
use super::*;
use sha2::{Digest, Sha256};

pub(super) fn anthropic_sse_error(value: &Value) -> ProviderError {
    let detail = provider_error_detail_value(value);
    let kind = if detail.as_deref().is_some_and(is_context_error_message) {
        ProviderErrorKind::ContextLength
    } else {
        match value.pointer("/error/type").and_then(Value::as_str) {
            Some("authentication_error") => ProviderErrorKind::Authentication,
            Some("permission_error") => ProviderErrorKind::Permission,
            Some("invalid_request_error") => ProviderErrorKind::InvalidRequest,
            Some("rate_limit_error") => ProviderErrorKind::RateLimited,
            Some("overloaded_error") => ProviderErrorKind::Overloaded,
            Some("timeout_error") => ProviderErrorKind::Timeout,
            Some("api_error") => ProviderErrorKind::Server,
            _ => ProviderErrorKind::Transport,
        }
    };
    ProviderError {
        kind,
        message: detail.map_or_else(
            || "provider sent an SSE error".into(),
            |detail| format!("provider sent an SSE error: {detail}"),
        ),
        retry_after_ms: None,
    }
}

/// Replay the original assistant block array only when it still projects to
/// the committed provider-neutral message. A changed visible tool call or
/// answer cannot inherit an old signature.
pub(super) fn validated_anthropic_replay(
    data: &Value,
    content: &[Content],
    request: &ModelRequest,
    preceding: &[Value],
) -> Result<Vec<Value>, ProviderError> {
    let blocks = data["blocks"]
        .as_array()
        .filter(|blocks| !blocks.is_empty())
        .ok_or_else(|| invalid("invalid Anthropic content replay"))?;
    let digest = data["prefix_sha256"]
        .as_str()
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| invalid("invalid Anthropic replay prefix"))?;
    let mut visible = content.iter();
    for block in blocks {
        match block["type"].as_str() {
            Some("thinking") => {
                if block["thinking"].as_str().is_none()
                    || !block["signature"].as_str().is_some_and(|s| !s.is_empty())
                {
                    return Err(invalid("invalid Anthropic thinking replay"));
                }
            }
            Some("redacted_thinking") => {
                if !block["data"].as_str().is_some_and(|s| !s.is_empty()) {
                    return Err(invalid("invalid Anthropic redacted replay"));
                }
            }
            Some("text") => {
                let Some(Content::Text(expected)) = visible.next() else {
                    return Err(invalid("Anthropic replay differs from answer"));
                };
                if block["text"].as_str() != Some(expected) {
                    return Err(invalid("Anthropic replay differs from answer"));
                }
            }
            Some("tool_use") => {
                let Some(Content::ToolCall(expected)) = visible.next() else {
                    return Err(invalid("Anthropic replay differs from tool call"));
                };
                if block["id"].as_str() != Some(expected.id.as_str())
                    || block["name"].as_str() != Some(expected.name.as_str())
                    || block["input"] != expected.arguments
                {
                    return Err(invalid("Anthropic replay differs from tool call"));
                }
            }
            _ => return Err(invalid("unsupported Anthropic replay block")),
        }
    }
    if visible.next().is_some() {
        return Err(invalid("Anthropic replay omitted visible content"));
    }
    if blocks.iter().any(|block| {
        matches!(
            block["type"].as_str(),
            Some("thinking" | "redacted_thinking")
        )
    }) && anthropic_prefix_digest(request, preceding)? != digest
    {
        return Err(error(
            ProviderErrorKind::ReplayContextChanged,
            "Anthropic signed thinking prefix changed",
        ));
    }
    Ok(blocks.clone())
}

fn anthropic_context_fields(request: &ModelRequest) -> (Option<Value>, Option<Value>) {
    let system = request
        .instructions
        .as_deref()
        .filter(|instructions| !instructions.is_empty())
        .map(|instructions| json!(instructions));
    let tools = (!request.tools.is_empty()).then(|| {
        Value::Array(
            request
                .tools
                .iter()
                .map(|tool| {
                    json!({"name":tool.name,"description":tool.description,
                        "input_schema":tool.input_schema})
                })
                .collect(),
        )
    });
    (system, tools)
}

fn anthropic_prefix_digest(
    request: &ModelRequest,
    preceding: &[Value],
) -> Result<String, ProviderError> {
    let (system, tools) = anthropic_context_fields(request);
    digest_anthropic_prefix(system.as_ref(), tools.as_ref(), preceding)
}

pub(super) fn anthropic_body_prefix_digest(body: &Value) -> Result<String, ProviderError> {
    let messages = body["messages"]
        .as_array()
        .ok_or_else(|| invalid("invalid Anthropic request messages"))?;
    digest_anthropic_prefix(body.get("system"), body.get("tools"), messages)
}

fn digest_anthropic_prefix(
    system: Option<&Value>,
    tools: Option<&Value>,
    preceding: &[Value],
) -> Result<String, ProviderError> {
    let encoded = serde_json::to_vec(&json!({"system":system,"tools":tools,"messages":preceding}))
        .map_err(|_| invalid("cannot encode Anthropic replay prefix"))?;
    Ok(format!("{:x}", Sha256::digest(encoded)))
}

pub(super) fn anthropic_body(
    request: &ModelRequest,
    native_api: bool,
) -> Result<Value, ProviderError> {
    validate_request(request)?;
    if native_api
        && managed_anthropic_thinking(&request.model.model)
        && matches!(
            request.controls.tool_choice,
            ToolChoice::Required | ToolChoice::Named(_)
        )
    {
        return Err(unsupported("this Claude model cannot force tool use"));
    }
    if request.controls.reasoning != Reasoning::ProviderDefault
        || request.controls.temperature.is_some()
        || request.controls.top_p.is_some()
    {
        return Err(unsupported(
            "explicit reasoning and sampling controls are unsupported by Messages",
        ));
    }
    let messages = wire_messages(request, HttpWire::AnthropicMessages)?;
    let mut body = json!({"model":request.model.model,"messages":messages,"stream":true,
        "max_tokens":request.controls.max_output_tokens});
    if native_api && managed_anthropic_thinking(&request.model.model) {
        body["thinking"] = json!({"type":"adaptive","block_binding":{
            "prefix_mismatch_behavior":"error"}});
    }
    let (system, tools) = anthropic_context_fields(request);
    if let Some(system) = system {
        body["system"] = system;
    }
    if let Some(tools) = tools {
        body["tools"] = tools;
        let mut choice = match &request.controls.tool_choice {
            ToolChoice::None => json!({"type":"none"}),
            ToolChoice::Auto => json!({"type":"auto"}),
            ToolChoice::Required => json!({"type":"any"}),
            ToolChoice::Named(name) => json!({"type":"tool","name":name}),
        };
        if request.controls.tool_choice != ToolChoice::None {
            choice["disable_parallel_tool_use"] = json!(!request.controls.parallel_tool_calls);
        }
        body["tool_choice"] = choice;
    }
    Ok(body)
}

pub(super) fn managed_anthropic_thinking(model: &str) -> bool {
    matches!(
        model,
        "claude-fable-5-1" | "claude-opus-5-5" | "claude-sonnet-5-5"
    )
}

enum AnthropicBlock {
    Text(String),
    Thinking {
        text: String,
        signature: String,
    },
    RedactedThinking(String),
    Tool {
        id: String,
        name: String,
        initial: Value,
        fragments: Option<String>,
    },
    Closed {
        content: Option<Content>,
        wire: Value,
    },
}
#[derive(Default)]
pub(super) struct AnthropicState {
    model: Option<String>,
    pub(super) prefix_digest: Option<String>,
    blocks: Vec<AnthropicBlock>,
    /// The matching response blocks, retained verbatim apart from applying
    /// streamed field deltas. Indices follow `blocks` throughout a message.
    wire_blocks: Vec<Value>,
    ids: BTreeSet<String>,
    finish: Option<String>,
    in_message_delta: bool,
    input_transformations: Option<Value>,
    input: u64,
    output: u64,
    cache_creation: u64,
    cache_read: u64,
}

pub(super) fn anthropic_replay_notices(transformations: Option<&Value>) -> Vec<ModelStreamEvent> {
    let mut counts = BTreeMap::<(&str, &str), usize>::new();
    for item in transformations
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let (Some(action), Some(reason)) = (item["type"].as_str(), item["reason"].as_str()) else {
            continue;
        };
        if matches!(
            (action, reason),
            (
                "thinking_dropped",
                "prefix_binding_mismatch"
                    | "model_binding_mismatch"
                    | "organization_binding_mismatch"
            ) | ("thinking_mismatch_allowed", "prefix_binding_mismatch")
        ) {
            *counts.entry((action, reason)).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .map(
            |((action, reason), count)| ModelStreamEvent::ProviderReplayNotice {
                action: action.into(),
                reason: reason.into(),
                count,
            },
        )
        .collect()
}

impl AnthropicState {
    pub(super) fn new(prefix_digest: Option<String>) -> Self {
        Self {
            prefix_digest,
            ..Self::default()
        }
    }

    pub(super) fn usage(&self) -> Result<Usage, ProviderError> {
        let input = self
            .input
            .checked_add(self.cache_creation)
            .and_then(|v| v.checked_add(self.cache_read))
            .ok_or_else(|| invalid("usage token count overflow"))?;
        Ok(Usage::known_with_cache(
            input,
            self.output,
            self.cache_read,
            self.cache_creation,
        ))
    }
    fn update_usage(&mut self, value: &Value, initial: bool) -> Result<(), ProviderError> {
        if !initial && value.is_null() {
            return Ok(());
        }
        let fields = value
            .as_object()
            .ok_or_else(|| invalid("invalid Anthropic usage"))?;
        for (key, current) in [
            ("input_tokens", &mut self.input),
            ("output_tokens", &mut self.output),
            ("cache_creation_input_tokens", &mut self.cache_creation),
            ("cache_read_input_tokens", &mut self.cache_read),
        ] {
            if let Some(value) = fields.get(key) {
                let count = value
                    .as_u64()
                    .ok_or_else(|| invalid("invalid usage token count"))?;
                if count < *current {
                    return Err(invalid("cumulative usage regressed"));
                }
                *current = count;
            } else if initial && (key == "input_tokens" || key == "output_tokens") {
                return Err(invalid("missing required token usage"));
            }
        }
        self.usage()?;
        Ok(())
    }
    pub(super) fn accept(
        &mut self,
        value: &Value,
        request: &ModelRequest,
    ) -> Result<Vec<ModelStreamEvent>, ProviderError> {
        let kind = value["type"]
            .as_str()
            .ok_or_else(|| invalid("Anthropic event missing type"))?;
        if kind == "error" {
            return Err(anthropic_sse_error(value));
        }
        if kind == "ping" {
            return Ok(Vec::new());
        }
        if !matches!(
            kind,
            "message_start"
                | "content_block_start"
                | "content_block_delta"
                | "content_block_stop"
                | "message_delta"
                | "message_stop"
        ) {
            return Ok(Vec::new());
        }
        if kind == "message_start" {
            if self.model.is_some()
                || value["message"]["role"] != "assistant"
                || value["message"]["type"] != "message"
                || !value["message"]["stop_reason"].is_null()
                || !value["message"]["content"]
                    .as_array()
                    .is_some_and(Vec::is_empty)
            {
                return Err(invalid("invalid or duplicate message_start"));
            }
            let model = value["message"]["model"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| invalid("missing Anthropic model"))?;
            self.model = Some(model.into());
            self.input_transformations = value["message"]
                .get("input_transformations")
                .filter(|items| items.is_array())
                .cloned();
            self.update_usage(&value["message"]["usage"], true)?;
            return Ok(vec![ModelStreamEvent::Usage(self.usage()?)]);
        }
        if self.model.is_none() {
            return Err(invalid("Anthropic event before message_start"));
        }
        for model in [value.get("model"), value["delta"].get("model")]
            .into_iter()
            .flatten()
        {
            if model.as_str() != self.model.as_deref() {
                return Err(invalid("returned model changed within response"));
            }
        }
        let index = || {
            value["index"]
                .as_u64()
                .and_then(|v| usize::try_from(v).ok())
                .ok_or_else(|| invalid("invalid Anthropic block index"))
        };
        match kind {
            "content_block_start" => {
                if self.in_message_delta || index()? != self.blocks.len() {
                    return Err(invalid("late or noncontiguous content block"));
                }
                let block = &value["content_block"];
                if !block.is_object() {
                    return Err(invalid("invalid Anthropic content block"));
                }
                let event = match block["type"].as_str() {
                    Some("text") => {
                        if block.get("citations").is_some_and(|v| {
                            !v.is_null() && !v.as_array().is_some_and(Vec::is_empty)
                        }) {
                            return Err(unsupported("Anthropic text citations are unsupported"));
                        }
                        let text = block["text"]
                            .as_str()
                            .ok_or_else(|| invalid("invalid text block"))?;
                        self.blocks.push(AnthropicBlock::Text(text.into()));
                        if text.is_empty() {
                            None
                        } else {
                            Some(ModelStreamEvent::TextDelta(text.into()))
                        }
                    }
                    Some("thinking") => {
                        let text = block["thinking"]
                            .as_str()
                            .ok_or_else(|| invalid("invalid thinking block"))?;
                        let signature = block["signature"].as_str().unwrap_or_default();
                        self.blocks.push(AnthropicBlock::Thinking {
                            text: text.into(),
                            signature: signature.into(),
                        });
                        None
                    }
                    Some("redacted_thinking") => {
                        let data = block["data"]
                            .as_str()
                            .filter(|s| !s.is_empty())
                            .ok_or_else(|| invalid("invalid redacted thinking block"))?;
                        self.blocks
                            .push(AnthropicBlock::RedactedThinking(data.into()));
                        None
                    }
                    Some("tool_use") => {
                        if block
                            .get("caller")
                            .is_some_and(|v| !v.is_null() && v["type"] != "direct")
                            || block.get("toolset_name").is_some_and(|v| !v.is_null())
                        {
                            return Err(unsupported("provider-hosted tool calls are unsupported"));
                        }
                        let id = block["id"]
                            .as_str()
                            .filter(|s| !s.is_empty())
                            .ok_or_else(|| invalid("missing tool ID"))?;
                        let name = block["name"]
                            .as_str()
                            .filter(|s| valid_name(s))
                            .ok_or_else(|| invalid("invalid tool name"))?;
                        if !self.ids.insert(id.into()) || !block["input"].is_object() {
                            return Err(invalid("invalid or duplicate tool_use"));
                        }
                        self.blocks.push(AnthropicBlock::Tool {
                            id: id.into(),
                            name: name.into(),
                            initial: block["input"].clone(),
                            fragments: None,
                        });
                        None
                    }
                    _ => return Err(unsupported("unsupported Anthropic content block")),
                };
                self.wire_blocks.push(block.clone());
                Ok(event.into_iter().collect())
            }
            "content_block_delta" => {
                if self.in_message_delta {
                    return Err(invalid("content delta after message_delta"));
                }
                let index = index()?;
                let block = self
                    .blocks
                    .get_mut(index)
                    .ok_or_else(|| invalid("delta for unknown block"))?;
                let wire = self
                    .wire_blocks
                    .get_mut(index)
                    .ok_or_else(|| invalid("missing Anthropic replay block"))?;
                match (block, value["delta"]["type"].as_str()) {
                    (AnthropicBlock::Text(text), Some("text_delta")) => {
                        let part = value["delta"]["text"]
                            .as_str()
                            .ok_or_else(|| invalid("invalid text delta"))?;
                        text.push_str(part);
                        wire["text"] = json!(text);
                        Ok(if part.is_empty() {
                            Vec::new()
                        } else {
                            vec![ModelStreamEvent::TextDelta(part.into())]
                        })
                    }
                    (AnthropicBlock::Thinking { text, .. }, Some("thinking_delta")) => {
                        let part = value["delta"]["thinking"]
                            .as_str()
                            .ok_or_else(|| invalid("invalid thinking delta"))?;
                        text.push_str(part);
                        wire["thinking"] = json!(text);
                        Ok(Vec::new())
                    }
                    (AnthropicBlock::Thinking { signature, .. }, Some("signature_delta")) => {
                        let part = value["delta"]["signature"]
                            .as_str()
                            .filter(|s| !s.is_empty())
                            .ok_or_else(|| invalid("invalid signature delta"))?;
                        signature.push_str(part);
                        wire["signature"] = json!(signature);
                        Ok(Vec::new())
                    }
                    (
                        AnthropicBlock::Tool {
                            initial, fragments, ..
                        },
                        Some("input_json_delta"),
                    ) => {
                        if !initial.as_object().is_some_and(serde_json::Map::is_empty) {
                            return Err(invalid("ambiguous initial tool input and fragments"));
                        }
                        let part = value["delta"]["partial_json"]
                            .as_str()
                            .ok_or_else(|| invalid("invalid tool input delta"))?;
                        fragments.get_or_insert_default().push_str(part);
                        Ok(Vec::new())
                    }
                    _ => Err(invalid("mismatched content block delta")),
                }
            }
            "content_block_stop" => {
                if self.in_message_delta {
                    return Err(invalid("block stop after message_delta"));
                }
                let index = index()?;
                let block = self
                    .blocks
                    .get_mut(index)
                    .ok_or_else(|| invalid("stop for unknown block"))?;
                let wire = self
                    .wire_blocks
                    .get_mut(index)
                    .ok_or_else(|| invalid("missing Anthropic replay block"))?;
                let content = match block {
                    AnthropicBlock::Text(text) => {
                        let text = std::mem::take(text);
                        Some(Content::Text(text))
                    }
                    AnthropicBlock::Thinking { text, signature } => {
                        if signature.is_empty() {
                            return Err(invalid("thinking block missing signature"));
                        }
                        wire["thinking"] = json!(text);
                        wire["signature"] = json!(signature);
                        None
                    }
                    AnthropicBlock::RedactedThinking(data) => {
                        wire["data"] = json!(data);
                        None
                    }
                    AnthropicBlock::Tool {
                        id,
                        name,
                        initial,
                        fragments,
                    } => {
                        let (arguments, raw_arguments) = if let Some(fragments) = fragments {
                            if !initial.as_object().is_some_and(serde_json::Map::is_empty) {
                                return Err(invalid("ambiguous initial tool input and fragments"));
                            }
                            parse_tool_arguments(std::mem::take(fragments))
                        } else {
                            (std::mem::take(initial), None)
                        };
                        wire["input"] = arguments.clone();
                        Some(Content::ToolCall(ToolCall {
                            id: std::mem::take(id),
                            name: std::mem::take(name),
                            arguments,
                            raw_arguments,
                        }))
                    }
                    AnthropicBlock::Closed { .. } => {
                        return Err(invalid("duplicate content_block_stop"));
                    }
                };
                *block = AnthropicBlock::Closed {
                    content,
                    wire: std::mem::take(wire),
                };
                Ok(Vec::new())
            }
            "message_delta" => {
                if self
                    .blocks
                    .iter()
                    .any(|b| !matches!(b, AnthropicBlock::Closed { .. }))
                {
                    return Err(invalid("message_delta before content block finished"));
                }
                let delta = value["delta"]
                    .as_object()
                    .ok_or_else(|| invalid("invalid Anthropic message_delta"))?;
                if let Some(reason) = delta.get("stop_reason").filter(|reason| !reason.is_null()) {
                    let reason = reason
                        .as_str()
                        .ok_or_else(|| invalid("invalid Anthropic stop_reason"))?;
                    if !matches!(
                        reason,
                        "end_turn"
                            | "tool_use"
                            | "max_tokens"
                            | "refusal"
                            | "model_context_window_exceeded"
                    ) {
                        return Err(unsupported("unsupported Anthropic stop_reason"));
                    }
                    if self.finish.as_deref().is_some_and(|prior| prior != reason) {
                        return Err(invalid("conflicting Anthropic stop_reason"));
                    }
                    if (reason == "tool_use" && self.ids.is_empty())
                        || (reason == "end_turn" && !self.ids.is_empty())
                    {
                        return Err(invalid("stop_reason contradicts tool calls"));
                    }
                    self.finish = Some(reason.into());
                }
                self.in_message_delta = true;
                if let Some(transformations) = value
                    .get("input_transformations")
                    .filter(|items| items.is_array())
                {
                    self.input_transformations = Some(transformations.clone());
                }
                self.update_usage(&value["usage"], false)?;
                Ok(vec![ModelStreamEvent::Usage(self.usage()?)])
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
                let mut content = Vec::new();
                let mut wire_blocks = Vec::new();
                for block in std::mem::take(&mut self.blocks) {
                    let AnthropicBlock::Closed {
                        content: item,
                        wire,
                    } = block
                    else {
                        return Err(invalid("unfinished Anthropic block"));
                    };
                    if let Some(item) = item {
                        content.push(item);
                    }
                    wire_blocks.push(wire);
                }
                self.wire_blocks.clear();
                let prefix_bound = wire_blocks.iter().any(|block| {
                    matches!(
                        block["type"].as_str(),
                        Some("thinking" | "redacted_thinking")
                    )
                });
                if prefix_bound
                    && content.iter().any(|part| {
                        matches!(part, Content::ToolCall(call) if call.raw_arguments.is_some())
                    })
                {
                    return Err(invalid(
                        "signed Anthropic tool input cannot be replayed after invalid JSON",
                    ));
                }
                let response = ModelResponse {
                    message: Message {
                        role: Role::Assistant,
                        content,
                        provider_replay: Some(
                            ProviderReplay::new(
                                &request.model.provider,
                                ANTHROPIC_CONTENT_REPLAY,
                                json!({"blocks":wire_blocks,"prefix_sha256":
                                match &self.prefix_digest {
                                    Some(digest) => digest.clone(),
                                    None => anthropic_prefix_digest(
                                        request,
                                        &wire_messages(request, HttpWire::AnthropicMessages)?,
                                    )?,
                                }}),
                            )
                            .with_prefix_binding(prefix_bound),
                        ),
                    },
                    usage: self.usage()?,
                    termination,
                    returned_model: self.model.take(),
                };
                if response.is_complete() {
                    validate_output(request, &response)?;
                }
                let mut events = anthropic_replay_notices(self.input_transformations.as_ref());
                events.push(ModelStreamEvent::Completed(response));
                Ok(events)
            }
            _ => Ok(Vec::new()),
        }
    }
}

#[cfg(test)]
mod cache_usage_tests {
    use super::*;

    #[test]
    fn anthropic_usage_preserves_cache_read_and_write_buckets() {
        let mut state = AnthropicState::default();
        state
            .update_usage(
                &json!({
                    "input_tokens": 100,
                    "output_tokens": 50,
                    "cache_creation_input_tokens": 200,
                    "cache_read_input_tokens": 700
                }),
                true,
            )
            .unwrap();
        let usage = state.usage().unwrap();
        assert_eq!(usage, Usage::known_with_cache(1_000, 50, 700, 200));
        assert_eq!(usage.uncached_input_tokens(), Some(100));
    }
}
