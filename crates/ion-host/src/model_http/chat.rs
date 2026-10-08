//! Chat-compatible request encoding, streamed decoding, and reasoning replay.
use super::*;

pub(super) fn chat_body(request: &ModelRequest, wire: HttpWire) -> Result<Value, ProviderError> {
    validate_request(request)?;
    let mut body = control_fields(&request.route.effective, &request.controls, wire, false)?;
    let mut messages = Vec::new();
    if let Some(instructions) = &request.instructions {
        messages.push(json!({"role":"system","content":instructions}));
    }
    messages.extend(wire_messages(request, wire)?);
    body["model"] = json!(request.route.effective.model);
    body["messages"] = json!(messages);
    body["stream"] = json!(true);
    body["stream_options"] = json!({"include_usage":true});
    if wire == HttpWire::OpenRouterChat
        && let Some(id) = &request.provider_session_id
    {
        if id.is_empty() || id.chars().count() > 256 {
            return Err(invalid(
                "OpenRouter session ID must contain 1 to 256 characters",
            ));
        }
        body["session_id"] = json!(id);
    }

    if !request.tools.is_empty() {
        body["tools"] = Value::Array(
            request
                .tools
                .iter()
                .map(|tool| {
                    json!({"type":"function","function":{
            "name":tool.name,"description":tool.description,"parameters":tool.input_schema}})
                })
                .collect(),
        );
        body["tool_choice"] = match &request.controls.tool_choice {
            ToolChoice::None => json!("none"),
            ToolChoice::Auto => json!("auto"),
            ToolChoice::Required => json!("required"),
            ToolChoice::Named(name) => json!({"type":"function","function":{"name":name}}),
        };
        body["parallel_tool_calls"] = json!(request.controls.parallel_tool_calls);
    }

    Ok(body)
}

#[derive(Default)]
struct ChatCall {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ThinkingSource {
    Plain,
    Structured,
}

pub(super) struct ChatState {
    text: String,
    thinking_source: Option<ThinkingSource>,
    // Stream one readable channel so dual fields cannot duplicate provisional
    // output. Completion projects the richer validated structured blocks.
    thinking_entry: Option<usize>,
    thinking_blocks: usize,
    reasoning_content: String,
    reasoning_details: Vec<Value>,
    wire: HttpWire,
    calls: BTreeMap<u64, ChatCall>,
    finish: Option<String>,
    model: Option<String>,
    usage: UsageState,
}
impl Default for ChatState {
    fn default() -> Self {
        Self {
            text: String::new(),
            thinking_source: None,
            thinking_entry: None,
            thinking_blocks: 0,
            reasoning_content: String::new(),
            reasoning_details: Vec::new(),
            wire: HttpWire::ChatCompletions,
            calls: BTreeMap::new(),
            finish: None,
            model: None,
            usage: UsageState::default(),
        }
    }
}
impl ChatState {
    pub(super) fn new(wire: HttpWire) -> Self {
        Self {
            wire,
            ..Self::default()
        }
    }

    pub(super) fn accept(&mut self, value: &Value) -> Result<Vec<ModelStreamEvent>, ProviderError> {
        if value.get("error").is_some_and(|v| !v.is_null()) {
            return Err(chat_stream_error(value));
        }
        if !value.is_object() {
            return Err(invalid("invalid Chat response object"));
        }
        if let Some(model) = optional_string(value, "model")? {
            if model.is_empty() {
                return Err(invalid("empty returned model"));
            }
            if self.model.as_deref().is_some_and(|old| old != model) {
                return Err(invalid("returned model changed within one response"));
            }
            self.model = Some(model.into());
        }
        let mut events = Vec::new();
        if let Some(usage) = value.get("usage").filter(|v| !v.is_null()) {
            self.usage.set_chat(usage)?;
            events.push(ModelStreamEvent::Usage(self.usage.value()));
        }
        let choices = match value.get("choices") {
            Some(choices) => choices
                .as_array()
                .ok_or_else(|| invalid("invalid response choices"))?,
            None if !events.is_empty() => return Ok(events),
            None => return Err(invalid("missing response choices")),
        };
        if choices.len() > 1 {
            return Err(invalid("multiple response choices are unsupported"));
        }
        let Some(choice) = choices.first() else {
            return Ok(events);
        };
        if !choice.is_object() {
            return Err(invalid("invalid response choice"));
        }
        if let Some(index) = choice.get("index").filter(|value| !value.is_null())
            && index.as_u64() != Some(0)
        {
            return Err(invalid("unexpected response choice index"));
        }
        let finish = optional_string(choice, "finish_reason")?;
        let delta = &choice["delta"];
        if !delta.is_null() && !delta.is_object() {
            return Err(invalid("invalid response delta"));
        }
        if let Some(role) = optional_string(delta, "role")?
            && role != "assistant"
        {
            return Err(invalid("invalid assistant delta role"));
        }
        if value.get("usage").is_none_or(Value::is_null)
            && let Some(usage) = choice.get("usage").filter(|usage| !usage.is_null())
        {
            self.usage.set_chat(usage)?;
            events.push(ModelStreamEvent::Usage(self.usage.value()));
        }
        if self.finish.is_some() {
            if finish != self.finish.as_deref() || !empty_post_finish_delta(&choice["delta"]) {
                return Err(invalid("contradictory choice after finish_reason"));
            }
            return Ok(events);
        }
        if let Some(details) = delta.get("reasoning_details").filter(|v| !v.is_null()) {
            let details = details
                .as_array()
                .ok_or_else(|| invalid("invalid reasoning_details delta"))?;
            if !details.is_empty() && self.wire != HttpWire::OpenRouterChat {
                return Err(unsupported("structured reasoning replay is unsupported"));
            }
            for detail in details {
                let index = append_openrouter_detail(&mut self.reasoning_details, detail)?;
                if openrouter_human_text(detail).is_none() {
                    events.push(ModelStreamEvent::OutputObserved);
                }
                if let Some(text) = openrouter_human_text(detail) {
                    self.thinking_source
                        .get_or_insert(ThinkingSource::Structured);
                    if self.thinking_source == Some(ThinkingSource::Structured) {
                        self.push_thinking(index, text, &mut events);
                    }
                }
            }
        }
        if let Some(reasoning) = chat_reasoning_delta(delta, self.wire)? {
            self.reasoning_content.push_str(reasoning);
            self.thinking_source.get_or_insert(ThinkingSource::Plain);
            if self.thinking_source == Some(ThinkingSource::Plain) {
                self.push_thinking(0, reasoning, &mut events);
            }
        }
        if let Some(part) = optional_string(delta, "content")?.filter(|s| !s.is_empty()) {
            self.text.push_str(part);
            events.push(ModelStreamEvent::TextDelta(part.into()));
        }
        if let Some(fragments) = delta.get("tool_calls").filter(|value| !value.is_null()) {
            let fragments = fragments
                .as_array()
                .ok_or_else(|| invalid("invalid tool_calls delta"))?;
            for fragment in fragments {
                if !fragment.is_object() {
                    return Err(invalid("invalid tool call fragment"));
                }
                let index = fragment["index"]
                    .as_u64()
                    .ok_or_else(|| invalid("tool call fragment missing index"))?;
                if let Some(kind) = optional_string(fragment, "type")?
                    && kind != "function"
                {
                    return Err(unsupported("unsupported tool call type"));
                }
                let function = &fragment["function"];
                if !function.is_null() && !function.is_object() {
                    return Err(invalid("invalid tool function delta"));
                }
                let call = self.calls.entry(index).or_default();
                merge_call_metadata(&mut call.id, optional_string(fragment, "id")?)?;
                merge_call_metadata(&mut call.name, optional_string(function, "name")?)?;
                if let Some(args) = optional_string(function, "arguments")? {
                    call.arguments.push_str(args);
                }
            }
            if !fragments.is_empty() {
                events.push(ModelStreamEvent::OutputObserved);
            }
        }
        if let Some(reason) = finish {
            self.finish = Some(reason.into());
        }
        Ok(events)
    }
    fn push_thinking(&mut self, index: usize, text: &str, events: &mut Vec<ModelStreamEvent>) {
        if self.thinking_entry != Some(index) {
            self.thinking_entry = Some(index);
            self.thinking_blocks += 1;
        }
        let block = self.thinking_blocks - 1;
        events.push(ModelStreamEvent::ThinkingDelta {
            block,
            text: text.into(),
        });
    }

    pub(super) fn complete(self, request: &ModelRequest) -> Result<ModelResponse, ProviderError> {
        if self.wire == HttpWire::OpenRouterChat
            && !self.reasoning_content.is_empty()
            && !self.reasoning_details.is_empty()
            && self.reasoning_details.iter().all(|detail| {
                detail["type"] == "reasoning.text"
                    && detail
                        .get("signature")
                        .is_none_or(|value| !has_content(value))
            })
            && self.reasoning_content
                != self
                    .reasoning_details
                    .iter()
                    .filter_map(|detail| detail["text"].as_str())
                    .collect::<String>()
        {
            return Err(invalid("OpenRouter plain reasoning and details disagree"));
        }
        let finish = self
            .finish
            .ok_or_else(|| transport("[DONE] arrived before finish_reason"))?;
        let termination = match finish.as_str() {
            "stop" | "tool_calls" => ResponseTermination::Completed,
            "length" => ResponseTermination::Incomplete(IncompleteReason::MaxOutputTokens),
            "content_filter" => ResponseTermination::Incomplete(IncompleteReason::ContentFilter),
            _ => return Err(unsupported("unsupported Chat Completions finish_reason")),
        };
        if (finish == "tool_calls" && self.calls.is_empty())
            || (finish == "stop" && !self.calls.is_empty())
        {
            return Err(invalid("finish_reason contradicts tool calls"));
        }
        let mut content = self
            .reasoning_details
            .iter()
            .filter_map(openrouter_human_text)
            .map(|text| Content::Thinking(text.to_owned()))
            .collect::<Vec<_>>();
        if content.is_empty() && !self.reasoning_content.is_empty() {
            content.push(Content::Thinking(self.reasoning_content.clone()));
        }
        if !self.text.is_empty() {
            content.push(Content::Text(self.text));
        }
        if matches!(
            termination,
            ResponseTermination::Completed
                | ResponseTermination::Incomplete(IncompleteReason::MaxOutputTokens)
        ) {
            let mut ids = BTreeSet::new();
            for (_, call) in self.calls {
                if call.id.is_empty() || !ids.insert(call.id.clone()) || !valid_name(&call.name) {
                    return Err(invalid("incomplete or duplicate streamed function call"));
                }
                let (arguments, raw_arguments) = parse_tool_arguments(call.arguments);
                content.push(Content::ToolCall(ToolCall {
                    id: call.id,
                    name: call.name,
                    arguments,
                    raw_arguments,
                }));
            }
        }
        let response = ModelResponse {
            message: Message {
                role: Role::Assistant,
                content,
                provider_replay: if !self.reasoning_details.is_empty() {
                    Some(ProviderReplay::new(
                        &request.route.effective.provider,
                        OPENROUTER_DETAILS_REPLAY,
                        Value::Array(self.reasoning_details),
                    ))
                } else if !self.reasoning_content.is_empty() {
                    Some(ProviderReplay::new(
                        &request.route.effective.provider,
                        if self.wire == HttpWire::OpenRouterChat {
                            OPENROUTER_PLAIN_REASONING_REPLAY
                        } else {
                            CHAT_REASONING_CONTENT_REPLAY
                        },
                        Value::String(self.reasoning_content),
                    ))
                } else {
                    None
                },
            },
            usage: self.usage.value(),
            termination,
            returned_model: self.model,
        };
        if response.is_complete() {
            validate_output(request, &response)?;
        }
        Ok(response)
    }
}

// Omitted/null fields are continuations, not permission to discard mistyped
// semantics. In particular, discarded arguments can turn a corrupt call into
// a valid executable call using only its earlier fragments.
fn optional_string<'a>(value: &'a Value, key: &str) -> Result<Option<&'a str>, ProviderError> {
    value
        .get(key)
        .filter(|value| !value.is_null())
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| invalid(&format!("invalid {key} delta field")))
        })
        .transpose()
}

fn merge_call_metadata(target: &mut String, part: Option<&str>) -> Result<(), ProviderError> {
    if let Some(part) = part.filter(|part| !part.is_empty()) {
        if target.is_empty() {
            target.push_str(part);
        } else if target != part {
            return Err(invalid("tool call metadata changed within one response"));
        }
    }
    Ok(())
}

pub(super) fn append_openrouter_detail(
    details: &mut Vec<Value>,
    fragment: &Value,
) -> Result<usize, ProviderError> {
    if !valid_openrouter_detail(fragment) {
        return Err(invalid("invalid OpenRouter reasoning detail"));
    }
    let kind = fragment["type"].as_str().expect("validated detail type");
    let payload = match kind {
        "reasoning.text" => "text",
        "reasoning.summary" => "summary",
        _ => {
            details.push(fragment.clone());
            return Ok(details.len() - 1);
        }
    };
    if let Some(last) = details.last_mut().filter(|last| {
        last["type"] == kind && (last.get(payload).is_some() || fragment.get(payload).is_some())
    }) {
        let last_fields = last.as_object_mut().expect("validated detail object");
        let fragment_fields = fragment.as_object().expect("validated detail object");
        let compatible = fragment_fields.iter().all(|(key, value)| {
            key == "type"
                || key == payload
                || last_fields
                    .get(key)
                    .is_none_or(|old| old.is_null() || value.is_null() || old == value)
        });
        if compatible {
            let suffix = fragment.get(payload).and_then(Value::as_str).unwrap_or("");
            if !suffix.is_empty() {
                if let Some(Value::String(text)) = last_fields.get_mut(payload) {
                    text.push_str(suffix);
                } else {
                    last_fields.insert(payload.into(), Value::String(suffix.into()));
                }
            }
            for (key, value) in fragment_fields {
                if key != "type" && key != payload && !value.is_null() {
                    let slot = last_fields.entry(key).or_insert(Value::Null);
                    if slot.is_null() {
                        *slot = value.clone();
                    }
                }
            }
            return Ok(details.len() - 1);
        }
    }
    details.push(fragment.clone());
    Ok(details.len() - 1)
}

// Called only after detail validation, at the adapter projection boundary.
fn openrouter_human_text(detail: &Value) -> Option<&str> {
    match detail["type"].as_str() {
        Some("reasoning.text") => detail["text"].as_str(),
        Some("reasoning.summary") => detail["summary"].as_str(),
        _ => None,
    }
    .filter(|text| !text.is_empty())
}

pub(super) fn chat_reasoning_delta(
    delta: &Value,
    wire: HttpWire,
) -> Result<Option<&str>, ProviderError> {
    let reasoning = optional_string(delta, "reasoning")?.filter(|s| !s.is_empty());
    let content = optional_string(delta, "reasoning_content")?.filter(|s| !s.is_empty());
    let other = optional_string(delta, "reasoning_text")?.filter(|s| !s.is_empty());
    if other.is_some() {
        return Err(unsupported("unexpected reasoning field for route"));
    }
    match wire {
        HttpWire::OpenRouterChat => {
            if reasoning.is_some() && content.is_some() && reasoning != content {
                return Err(invalid("conflicting OpenRouter reasoning aliases"));
            }
            Ok(reasoning.or(content))
        }
        HttpWire::DeepSeekChat | HttpWire::MiMoChat => {
            if reasoning.is_some() {
                return Err(unsupported("unexpected reasoning field for route"));
            }
            Ok(content)
        }
        _ if reasoning.is_some() || content.is_some() => Err(unsupported(
            "reasoning content cannot be replayed by this transport",
        )),
        _ => Ok(None),
    }
}

pub(super) fn has_content(value: &Value) -> bool {
    !value.is_null() && value != "" && value.as_array().is_none_or(|items| !items.is_empty())
}

pub(super) fn empty_post_finish_delta(delta: &Value) -> bool {
    if delta.is_null() {
        return true;
    }
    let Some(fields) = delta.as_object() else {
        return false;
    };
    fields.iter().all(|(name, value)| match name.as_str() {
        "role" => value.is_null() || value == "assistant",
        "content" => value.is_null() || value == "",
        "tool_calls" => value.is_null() || value.as_array().is_some_and(Vec::is_empty),
        "reasoning" | "reasoning_content" | "reasoning_text" => value.is_null() || value == "",
        "reasoning_details" => value.is_null() || value.as_array().is_some_and(Vec::is_empty),
        _ => false,
    })
}

#[derive(Default)]
pub(super) struct UsageState {
    input: Option<u64>,
    output: Option<u64>,
    cache_read: Option<u64>,
    cache_write: Option<u64>,
}
impl UsageState {
    pub(super) fn value(&self) -> Usage {
        Usage {
            input_tokens: self.input,
            output_tokens: self.output,
            cache_read_input_tokens: self.cache_read,
            cache_write_input_tokens: self.cache_write,
        }
    }
    pub(super) fn set_chat(&mut self, value: &Value) -> Result<(), ProviderError> {
        let fields = value
            .as_object()
            .ok_or_else(|| invalid("invalid Chat Completions usage"))?;
        if let Some(input) = fields.get("prompt_tokens") {
            self.input = Some(
                input
                    .as_u64()
                    .ok_or_else(|| invalid("invalid prompt token count"))?,
            );
        }
        if let Some(output) = fields.get("completion_tokens") {
            self.output = Some(
                output
                    .as_u64()
                    .ok_or_else(|| invalid("invalid completion token count"))?,
            );
        }
        if let Some(details) = fields
            .get("prompt_tokens_details")
            .filter(|value| !value.is_null())
        {
            let details = details
                .as_object()
                .ok_or_else(|| invalid("invalid prompt token details"))?;
            if let Some(value) = details.get("cached_tokens") {
                self.cache_read = Some(
                    value
                        .as_u64()
                        .ok_or_else(|| invalid("invalid cached token count"))?,
                );
            }
            if let Some(value) = details.get("cache_write_tokens") {
                self.cache_write = Some(
                    value
                        .as_u64()
                        .ok_or_else(|| invalid("invalid cache write token count"))?,
                );
            }
        }
        if let (Some(total), Some(read), Some(write)) =
            (self.input, self.cache_read, self.cache_write)
            && read.checked_add(write).is_none_or(|cached| cached > total)
        {
            return Err(invalid("cache token counts exceed prompt token count"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod cache_usage_tests {
    use super::*;

    #[test]
    fn prompt_cache_usage_remains_separate_from_total_input() {
        let mut usage = UsageState::default();
        usage
            .set_chat(&json!({
                "prompt_tokens": 1_000,
                "completion_tokens": 50,
                "prompt_tokens_details": {
                    "cached_tokens": 700,
                    "cache_write_tokens": 200
                }
            }))
            .unwrap();
        assert_eq!(usage.value(), Usage::known_with_cache(1_000, 50, 700, 200));
        assert_eq!(usage.value().uncached_input_tokens(), Some(100));
    }

    #[test]
    fn prompt_cache_subcounts_cannot_exceed_total_input() {
        let mut usage = UsageState::default();
        let error = usage
            .set_chat(&json!({
                "prompt_tokens": 100,
                "completion_tokens": 1,
                "prompt_tokens_details": {
                    "cached_tokens": 90,
                    "cache_write_tokens": 20
                }
            }))
            .unwrap_err();
        assert!(error.message.contains("exceed prompt token count"));
    }
}
