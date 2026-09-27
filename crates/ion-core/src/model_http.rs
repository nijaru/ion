//! HTTP model transports. Provider wire formats stay outside Session state.
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use async_stream::try_stream;
use futures_util::StreamExt;
use ion_ai::{
    BoxFuture, Content, IncompleteReason, Message, ModelRequest, ModelResponse, ModelService,
    ModelStream, ModelStreamEvent, ProviderError, ProviderErrorKind, Reasoning,
    ResponseTermination, Role, ToolCall, ToolChoice, Usage,
};
use reqwest::{
    Client, Url,
    header::{AUTHORIZATION, HeaderValue},
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::CredentialResolver;

const MAX_FRAME: usize = 256 * 1024;
const MAX_RESPONSE: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpWire {
    ChatCompletions,
    DeepSeekChat,
    MiMoChat,
    OpenRouterNoReasoning,
    AnthropicMessages,
}

impl HttpWire {
    fn is_chat(self) -> bool {
        self != Self::AnthropicMessages
    }
}

pub struct HttpModelService {
    client: Client,
    endpoint: Url,
    wire: HttpWire,
    credentials: Arc<dyn CredentialResolver>,
}

impl HttpModelService {
    pub fn new(
        endpoint: &str,
        wire: HttpWire,
        credentials: Arc<dyn CredentialResolver>,
    ) -> Result<Self, ProviderError> {
        let endpoint = Url::parse(endpoint).map_err(|_| invalid("invalid provider endpoint"))?;
        if endpoint.scheme() != "https"
            && !(endpoint.scheme() == "http" && endpoint.host_str().is_some_and(is_loopback))
        {
            return Err(invalid(
                "provider endpoint must use HTTPS or literal loopback HTTP",
            ));
        }
        if !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(invalid(
                "provider endpoint must not contain credentials, query or fragment",
            ));
        }
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .map_err(|_| transport("HTTP client setup failed"))?;
        Ok(Self {
            client,
            endpoint,
            wire,
            credentials,
        })
    }
}

impl ModelService for HttpModelService {
    fn stream<'a>(
        &'a self,
        request: ModelRequest,
    ) -> BoxFuture<'a, Result<ModelStream, ProviderError>> {
        Box::pin(async move {
            request.controls.validate()?;
            let body = if self.wire.is_chat() {
                chat_body(&request, self.wire)?
            } else {
                anthropic_body(&request)?
            };
            // Dropping this future drops credential lookup. The caller owns cancellation;
            // no provider request is spawned into an independent task.
            let key = self
                .credentials
                .resolve(CancellationToken::new())
                .await
                .map_err(|_| {
                    error(
                        ProviderErrorKind::Authentication,
                        "credential resolution failed",
                    )
                })?;
            if key.is_none() && self.endpoint.scheme() != "http" {
                return Err(error(
                    ProviderErrorKind::Authentication,
                    "provider credential unavailable",
                ));
            }
            let mut post = self
                .client
                .post(self.endpoint.clone())
                .header("accept", "text/event-stream")
                .json(&body);
            if let Some(key) = key {
                if key.is_empty() {
                    return Err(error(
                        ProviderErrorKind::Authentication,
                        "invalid provider credential",
                    ));
                }
                let header_text = if self.wire.is_chat() {
                    format!("Bearer {key}")
                } else {
                    key
                };
                let mut header = HeaderValue::from_str(&header_text).map_err(|_| {
                    error(
                        ProviderErrorKind::Authentication,
                        "invalid provider credential",
                    )
                })?;
                header.set_sensitive(true);
                post = if self.wire.is_chat() {
                    post.header(AUTHORIZATION, header)
                } else {
                    post.header("x-api-key", header)
                };
            }
            if self.wire == HttpWire::AnthropicMessages {
                post = post.header("anthropic-version", "2023-06-01");
            }
            let response = post
                .send()
                .await
                .map_err(|_| transport("provider HTTP request failed"))?;
            if !response.status().is_success() {
                let status = response.status().as_u16();
                let kind = match status {
                    401 => ProviderErrorKind::Authentication,
                    403 => ProviderErrorKind::Permission,
                    408 => ProviderErrorKind::Timeout,
                    429 => ProviderErrorKind::RateLimited,
                    503 | 529 => ProviderErrorKind::Overloaded,
                    500..=599 => ProviderErrorKind::Server,
                    _ => ProviderErrorKind::InvalidRequest,
                };
                return Err(error(kind, &format!("provider returned HTTP {status}")));
            }
            let is_sse = response
                .headers()
                .get("content-type")
                .and_then(|h| h.to_str().ok())
                .and_then(|h| h.split(';').next())
                .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/event-stream"));
            if !is_sse {
                return Err(invalid("provider response is not text/event-stream"));
            }
            let wire = self.wire;
            let stream: ModelStream = Box::pin(try_stream! {
                let mut bytes = response.bytes_stream();
                let mut frame = Vec::new();
                let mut total = 0usize;
                let mut decoder = if wire.is_chat() { Decoder::Chat(ChatState::default()) } else { Decoder::Anthropic(AnthropicState::default()) };
                while let Some(next) = bytes.next().await {
                    let chunk = next.map_err(|_| transport("provider stream transport failed"))?;
                    for byte in chunk {
                        if total == MAX_RESPONSE { Err(invalid("provider stream exceeded byte limit"))?; }
                        total += 1;
                        if frame.len() == MAX_FRAME { Err(invalid("provider SSE frame exceeded byte limit"))?; }
                        frame.push(byte);
                        if !frame.ends_with(b"\n\n") && !frame.ends_with(b"\r\n\r\n") { continue; }
                        let parsed = parse_frame(&frame)?;
                        frame.clear();
                        let Some(parsed) = parsed else { continue; };
                        if wire.is_chat() && parsed.data == "[DONE]" {
                            let response = decoder.complete(&request)?;
                            yield ModelStreamEvent::Completed(response);
                            return;
                        }
                        let value: Value = serde_json::from_str(&parsed.data)
                            .map_err(|_| invalid("provider sent invalid SSE JSON"))?;
                        if wire == HttpWire::AnthropicMessages
                            && parsed.event.as_deref() != value["type"].as_str()
                        {
                            Err(invalid("Anthropic SSE event and data type disagree"))?;
                        }
                        for event in decoder.accept(&value, &request)? {
                            let terminal = matches!(event, ModelStreamEvent::Completed(_));
                            yield event;
                            if terminal { return; }
                        }
                    }
                }
                Err(transport("provider stream ended before completion"))?;
            });
            Ok(stream)
        })
    }
}

fn is_loopback(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1")
}
fn error(kind: ProviderErrorKind, message: &str) -> ProviderError {
    ProviderError {
        kind,
        message: message.into(),
    }
}
fn invalid(message: &str) -> ProviderError {
    error(ProviderErrorKind::InvalidRequest, message)
}
fn unsupported(message: &str) -> ProviderError {
    error(ProviderErrorKind::Unsupported, message)
}
fn transport(message: &str) -> ProviderError {
    error(ProviderErrorKind::Transport, message)
}

struct SseFrame {
    event: Option<String>,
    data: String,
}
fn parse_frame(bytes: &[u8]) -> Result<Option<SseFrame>, ProviderError> {
    let text = std::str::from_utf8(bytes).map_err(|_| invalid("provider sent non-UTF-8 SSE"))?;
    let mut event = None;
    let mut data = String::new();
    let mut has_data = false;
    for line in text.lines() {
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => {
                if event.replace(value.to_owned()).is_some() {
                    return Err(invalid("duplicate SSE event field"));
                }
            }
            "data" => {
                if has_data {
                    data.push('\n');
                }
                data.push_str(value);
                has_data = true;
            }
            "id" | "retry" => {}
            _ => {} // SSE permits extension fields.
        }
    }
    if !has_data {
        if event.is_some() {
            return Err(invalid("SSE event has no data"));
        }
        return Ok(None);
    }
    Ok(Some(SseFrame { event, data }))
}

fn validate_request(request: &ModelRequest) -> Result<(), ProviderError> {
    if request.model.model.is_empty() || request.messages.is_empty() {
        return Err(invalid("model and conversation must be nonempty"));
    }
    if request.messages.iter().any(|m| m.provider_replay.is_some()) {
        return Err(unsupported(
            "opaque provider replay is unsupported by this transport",
        ));
    }
    if request.tools.is_empty()
        && matches!(
            request.controls.tool_choice,
            ToolChoice::Required | ToolChoice::Named(_)
        )
    {
        return Err(invalid("required tool choice has no tools"));
    }
    let mut names = BTreeSet::new();
    for tool in &request.tools {
        if !valid_name(&tool.name)
            || !names.insert(&tool.name)
            || tool.input_schema.get("type").and_then(Value::as_str) != Some("object")
        {
            return Err(invalid("invalid or duplicate tool specification"));
        }
    }
    if let ToolChoice::Named(name) = &request.controls.tool_choice
        && !names.contains(name)
    {
        return Err(invalid("named tool is not in the loadout"));
    }
    Ok(())
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Remap provider call IDs for every request. Their original IDs are only
/// meaningful inside the response which produced them and may collide later.
fn wire_messages(request: &ModelRequest, anthropic: bool) -> Result<Vec<Value>, ProviderError> {
    let mut messages = Vec::new();
    let mut pending = BTreeMap::<String, (String, String)>::new();
    let mut next_call = 0usize;
    for message in &request.messages {
        if message.content.is_empty() {
            return Err(invalid("empty transcript message"));
        }
        if message.role != Role::Tool && !pending.is_empty() {
            return Err(invalid("unanswered tool calls"));
        }
        let mut text = String::new();
        let mut blocks = Vec::new();
        let mut calls = Vec::new();
        let mut results = Vec::new();
        for item in &message.content {
            match (message.role, item) {
                (Role::User | Role::Assistant, Content::Text(part)) => {
                    text.push_str(part);
                    if anthropic && !part.is_empty() {
                        blocks.push(json!({"type":"text","text":part}));
                    }
                }
                (Role::Assistant, Content::ToolCall(call)) => {
                    if !valid_name(&call.name)
                        || !call.arguments.is_object()
                        || call.id.is_empty()
                        || pending.contains_key(&call.id)
                    {
                        return Err(invalid("invalid or duplicate tool call"));
                    }
                    let wire_id = format!("ion_call_{next_call}");
                    next_call += 1;
                    pending.insert(call.id.clone(), (wire_id.clone(), call.name.clone()));
                    if anthropic {
                        blocks.push(json!({"type":"tool_use","id":wire_id,"name":call.name,"input":call.arguments}));
                    } else {
                        calls.push(json!({"id":wire_id,"type":"function","function":{"name":call.name,"arguments":call.arguments.to_string()}}));
                    }
                }
                (Role::Tool, Content::ToolResult(result)) => {
                    let Some((wire_id, name)) = pending.remove(&result.call_id) else {
                        return Err(invalid("orphan or duplicate tool result"));
                    };
                    if result.name != name {
                        return Err(invalid("tool result name does not match call"));
                    }
                    if anthropic {
                        blocks.push(json!({"type":"tool_result","tool_use_id":wire_id,"content":result.result.to_string()}));
                    } else {
                        results.push(json!({"role":"tool","tool_call_id":wire_id,"content":result.result.to_string()}));
                    }
                }
                _ => return Err(invalid("content does not match transcript role")),
            }
        }
        if anthropic {
            if blocks.is_empty() {
                return Err(invalid("empty Anthropic message"));
            }
            let role = if message.role == Role::Assistant {
                "assistant"
            } else {
                "user"
            };
            if let Some(previous) = messages
                .last_mut()
                .filter(|m: &&mut Value| m["role"] == role)
            {
                previous["content"]
                    .as_array_mut()
                    .expect("constructed content array")
                    .extend(blocks);
            } else {
                messages.push(json!({"role":role,"content":blocks}));
            }
        } else if message.role == Role::Tool {
            messages.extend(results);
        } else {
            let role = if message.role == Role::Assistant {
                "assistant"
            } else {
                "user"
            };
            let mut value = json!({"role":role,"content":if text.is_empty() { Value::Null } else { json!(text) }});
            if !calls.is_empty() {
                value["tool_calls"] = Value::Array(calls);
            }
            messages.push(value);
        }
    }
    if !pending.is_empty() {
        return Err(invalid("unanswered tool calls"));
    }
    if anthropic
        && (messages[0]["role"] != "user" || messages.last().is_some_and(|m| m["role"] != "user"))
    {
        return Err(unsupported("Anthropic assistant prefill is unsupported"));
    }
    Ok(messages)
}

fn chat_body(request: &ModelRequest, wire: HttpWire) -> Result<Value, ProviderError> {
    validate_request(request)?;
    if matches!(request.controls.reasoning, Reasoning::BudgetTokens(_)) {
        return Err(unsupported(
            "exact reasoning-token budgets are unsupported by Chat Completions",
        ));
    }
    let mut messages = Vec::new();
    if let Some(instructions) = &request.instructions {
        messages.push(json!({"role":"system","content":instructions}));
    }
    messages.extend(wire_messages(request, false)?);
    let mut body = json!({"model":request.model.model,"messages":messages,"stream":true,
        "stream_options":{"include_usage":true},"max_completion_tokens":request.controls.max_output_tokens});
    if let Some(temperature) = request.controls.temperature {
        body["temperature"] = json!(temperature);
    }
    if let Some(top_p) = request.controls.top_p {
        body["top_p"] = json!(top_p);
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
    match wire {
        HttpWire::ChatCompletions => match request.controls.reasoning {
            Reasoning::ProviderDefault => {}
            Reasoning::Off => body["reasoning_effort"] = json!("none"),
            Reasoning::Low => body["reasoning_effort"] = json!("low"),
            Reasoning::Medium => body["reasoning_effort"] = json!("medium"),
            Reasoning::High => body["reasoning_effort"] = json!("high"),
            Reasoning::BudgetTokens(_) => unreachable!("rejected above"),
        },
        HttpWire::DeepSeekChat => {
            if !matches!(
                request.controls.reasoning,
                Reasoning::ProviderDefault | Reasoning::Off
            ) {
                return Err(unsupported(
                    "DeepSeek thinking needs reasoning-content replay",
                ));
            }
            body.as_object_mut()
                .expect("constructed object")
                .remove("max_completion_tokens");
            body["max_tokens"] = json!(request.controls.max_output_tokens);
            body["reasoning_effort"] = json!("none");
        }
        HttpWire::MiMoChat => {
            if !matches!(
                request.controls.reasoning,
                Reasoning::ProviderDefault | Reasoning::Off
            ) {
                return Err(unsupported("MiMo thinking needs reasoning-content replay"));
            }
            body["thinking"] = json!({"type":"disabled"});
        }
        HttpWire::OpenRouterNoReasoning => {
            if !matches!(
                request.controls.reasoning,
                Reasoning::ProviderDefault | Reasoning::Off
            ) {
                return Err(unsupported("OpenRouter thinking needs reasoning replay"));
            }
            body["reasoning"] = json!({"enabled":false});
        }
        HttpWire::AnthropicMessages => unreachable!("Anthropic uses its own encoder"),
    }
    Ok(body)
}

fn anthropic_body(request: &ModelRequest) -> Result<Value, ProviderError> {
    validate_request(request)?;
    if request.controls.reasoning != Reasoning::ProviderDefault
        || request.controls.temperature.is_some()
        || request.controls.top_p.is_some()
    {
        return Err(unsupported(
            "explicit reasoning and sampling controls are unsupported by Messages",
        ));
    }
    let messages = wire_messages(request, true)?;
    let mut body = json!({"model":request.model.model,"messages":messages,"stream":true,
        "max_tokens":request.controls.max_output_tokens});
    if let Some(instructions) = &request.instructions
        && !instructions.is_empty()
    {
        body["system"] = json!(instructions);
    }
    if !request.tools.is_empty() {
        body["tools"] = Value::Array(
            request
                .tools
                .iter()
                .map(|tool| {
                    json!({
            "name":tool.name,"description":tool.description,"input_schema":tool.input_schema})
                })
                .collect(),
        );
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

fn validate_output(request: &ModelRequest, response: &ModelResponse) -> Result<(), ProviderError> {
    let names = response
        .message
        .content
        .iter()
        .filter_map(|item| match item {
            Content::ToolCall(call) => Some(call.name.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if !request.controls.permits_tool_calls(names.iter().copied()) {
        return Err(invalid(
            "provider violated requested tool choice or parallel limit",
        ));
    }
    if names
        .iter()
        .any(|name| !request.tools.iter().any(|tool| tool.name == *name))
    {
        return Err(invalid("provider returned a tool outside the loadout"));
    }
    Ok(())
}

enum Decoder {
    Chat(ChatState),
    Anthropic(AnthropicState),
}
impl Decoder {
    fn accept(
        &mut self,
        value: &Value,
        request: &ModelRequest,
    ) -> Result<Vec<ModelStreamEvent>, ProviderError> {
        match self {
            Self::Chat(state) => state.accept(value),
            Self::Anthropic(state) => state.accept(value, request),
        }
    }
    fn complete(self, request: &ModelRequest) -> Result<ModelResponse, ProviderError> {
        match self {
            Self::Chat(state) => state.complete(request),
            Self::Anthropic(_) => Err(invalid("[DONE] is not an Anthropic completion")),
        }
    }
}

#[derive(Default)]
struct ChatCall {
    id: String,
    name: String,
    arguments: String,
}
#[derive(Default)]
struct ChatState {
    text: String,
    calls: BTreeMap<u64, ChatCall>,
    finish: Option<String>,
    model: Option<String>,
    usage: UsageState,
}
impl ChatState {
    fn accept(&mut self, value: &Value) -> Result<Vec<ModelStreamEvent>, ProviderError> {
        if value.get("error").is_some_and(|v| !v.is_null()) {
            return Err(transport("provider sent a stream error"));
        }
        if let Some(model) = value.get("model").and_then(Value::as_str) {
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
        let choices = value["choices"]
            .as_array()
            .ok_or_else(|| invalid("missing response choices"))?;
        if choices.len() > 1 {
            return Err(invalid("multiple response choices are unsupported"));
        }
        let Some(choice) = choices.first() else {
            return Ok(events);
        };
        if choice["index"].as_u64().is_some_and(|index| index != 0) {
            return Err(invalid("unexpected response choice index"));
        }
        if self.finish.is_some() {
            if choice["finish_reason"].as_str() != self.finish.as_deref()
                || !empty_post_finish_delta(&choice["delta"])
            {
                return Err(invalid("contradictory choice after finish_reason"));
            }
            return Ok(events);
        }
        let delta = &choice["delta"];
        if delta.get("reasoning_content").is_some_and(has_content)
            || delta.get("reasoning_details").is_some_and(has_content)
        {
            return Err(unsupported(
                "reasoning content cannot be replayed by this transport",
            ));
        }
        if let Some(part) = delta["content"].as_str().filter(|s| !s.is_empty()) {
            self.text.push_str(part);
            events.push(ModelStreamEvent::TextDelta(part.into()));
        }
        if let Some(fragments) = delta["tool_calls"].as_array() {
            for fragment in fragments {
                let index = fragment["index"]
                    .as_u64()
                    .ok_or_else(|| invalid("tool call fragment missing index"))?;
                let call = self.calls.entry(index).or_default();
                if let Some(id) = fragment["id"].as_str() {
                    if !call.id.is_empty() && call.id != id {
                        return Err(invalid("tool call ID changed within response"));
                    }
                    call.id = id.into();
                }
                if let Some(name) = fragment["function"]["name"].as_str() {
                    call.name.push_str(name);
                }
                if let Some(args) = fragment["function"]["arguments"].as_str() {
                    call.arguments.push_str(args);
                }
            }
        }
        if let Some(reason) = choice["finish_reason"].as_str() {
            self.finish = Some(reason.into());
        }
        Ok(events)
    }
    fn complete(self, request: &ModelRequest) -> Result<ModelResponse, ProviderError> {
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
        let mut content = Vec::new();
        if !self.text.is_empty() {
            content.push(Content::Text(self.text));
        }
        if matches!(termination, ResponseTermination::Completed) {
            let mut ids = BTreeSet::new();
            for (_, call) in self.calls {
                if call.id.is_empty() || !ids.insert(call.id.clone()) || !valid_name(&call.name) {
                    return Err(invalid("incomplete or duplicate streamed function call"));
                }
                let arguments: Value = serde_json::from_str(&call.arguments)
                    .map_err(|_| invalid("invalid function arguments JSON"))?;
                if !arguments.is_object() {
                    return Err(invalid("function arguments must be an object"));
                }
                content.push(Content::ToolCall(ToolCall {
                    id: call.id,
                    name: call.name,
                    arguments,
                }));
            }
        }
        let response = ModelResponse {
            message: Message {
                role: Role::Assistant,
                content,
                provider_replay: None,
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

fn has_content(value: &Value) -> bool {
    !value.is_null() && value != "" && value.as_array().is_none_or(|items| !items.is_empty())
}

fn empty_post_finish_delta(delta: &Value) -> bool {
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
        "reasoning_content" | "reasoning_details" => !has_content(value),
        _ => false,
    })
}

#[derive(Default)]
struct UsageState {
    input: Option<u64>,
    output: Option<u64>,
}
impl UsageState {
    fn value(&self) -> Usage {
        Usage {
            input_tokens: self.input,
            output_tokens: self.output,
        }
    }
    fn set_chat(&mut self, value: &Value) -> Result<(), ProviderError> {
        self.input = Some(
            value["prompt_tokens"]
                .as_u64()
                .ok_or_else(|| invalid("invalid prompt token count"))?,
        );
        self.output = Some(
            value["completion_tokens"]
                .as_u64()
                .ok_or_else(|| invalid("invalid completion token count"))?,
        );
        Ok(())
    }
}

enum AnthropicBlock {
    Text(String),
    Tool {
        id: String,
        name: String,
        initial: Value,
        fragments: Option<String>,
    },
    Closed(Content),
}
#[derive(Default)]
struct AnthropicState {
    model: Option<String>,
    blocks: Vec<AnthropicBlock>,
    ids: BTreeSet<String>,
    finish: Option<String>,
    in_message_delta: bool,
    input: u64,
    output: u64,
    cache_creation: u64,
    cache_read: u64,
}
impl AnthropicState {
    fn usage(&self) -> Result<Usage, ProviderError> {
        let input = self
            .input
            .checked_add(self.cache_creation)
            .and_then(|v| v.checked_add(self.cache_read))
            .ok_or_else(|| invalid("usage token count overflow"))?;
        Ok(Usage::known(input, self.output))
    }
    fn update_usage(&mut self, value: &Value, initial: bool) -> Result<(), ProviderError> {
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
            } else if key == "output_tokens" || (initial && key == "input_tokens") {
                return Err(invalid("missing required token usage"));
            }
        }
        self.usage()?;
        Ok(())
    }
    fn accept(
        &mut self,
        value: &Value,
        request: &ModelRequest,
    ) -> Result<Vec<ModelStreamEvent>, ProviderError> {
        let kind = value["type"]
            .as_str()
            .ok_or_else(|| invalid("Anthropic event missing type"))?;
        if kind == "error" {
            return Err(transport("provider sent an SSE error"));
        }
        if kind == "ping" {
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
                Ok(event.into_iter().collect())
            }
            "content_block_delta" => {
                if self.in_message_delta {
                    return Err(invalid("content delta after message_delta"));
                }
                let block = self
                    .blocks
                    .get_mut(index()?)
                    .ok_or_else(|| invalid("delta for unknown block"))?;
                match (block, value["delta"]["type"].as_str()) {
                    (AnthropicBlock::Text(text), Some("text_delta")) => {
                        let part = value["delta"]["text"]
                            .as_str()
                            .ok_or_else(|| invalid("invalid text delta"))?;
                        text.push_str(part);
                        Ok(if part.is_empty() {
                            Vec::new()
                        } else {
                            vec![ModelStreamEvent::TextDelta(part.into())]
                        })
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
                let block = self
                    .blocks
                    .get_mut(index()?)
                    .ok_or_else(|| invalid("stop for unknown block"))?;
                let content = match block {
                    AnthropicBlock::Text(text) => Content::Text(std::mem::take(text)),
                    AnthropicBlock::Tool {
                        id,
                        name,
                        initial,
                        fragments,
                    } => {
                        let arguments = if let Some(fragments) = fragments {
                            let parsed: Value = serde_json::from_str(fragments)
                                .map_err(|_| invalid("malformed tool input JSON"))?;
                            if !initial.as_object().is_some_and(serde_json::Map::is_empty)
                                || !parsed.is_object()
                            {
                                return Err(invalid("ambiguous or invalid tool input"));
                            }
                            parsed
                        } else {
                            std::mem::take(initial)
                        };
                        Content::ToolCall(ToolCall {
                            id: std::mem::take(id),
                            name: std::mem::take(name),
                            arguments,
                        })
                    }
                    AnthropicBlock::Closed(_) => {
                        return Err(invalid("duplicate content_block_stop"));
                    }
                };
                *block = AnthropicBlock::Closed(content);
                Ok(Vec::new())
            }
            "message_delta" => {
                if self.in_message_delta
                    || self
                        .blocks
                        .iter()
                        .any(|b| !matches!(b, AnthropicBlock::Closed(_)))
                {
                    return Err(invalid("duplicate message_delta or unfinished block"));
                }
                let reason = value["delta"]["stop_reason"]
                    .as_str()
                    .ok_or_else(|| invalid("missing Anthropic stop_reason"))?;
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
                if (reason == "tool_use" && self.ids.is_empty())
                    || (reason == "end_turn" && !self.ids.is_empty())
                {
                    return Err(invalid("stop_reason contradicts tool calls"));
                }
                self.in_message_delta = true;
                self.finish = Some(reason.into());
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
                for block in std::mem::take(&mut self.blocks) {
                    let AnthropicBlock::Closed(item) = block else {
                        return Err(invalid("unfinished Anthropic block"));
                    };
                    content.push(item);
                }
                let response = ModelResponse {
                    message: Message {
                        role: Role::Assistant,
                        content,
                        provider_replay: None,
                    },
                    usage: self.usage()?,
                    termination,
                    returned_model: self.model.take(),
                };
                if response.is_complete() {
                    validate_output(request, &response)?;
                }
                Ok(vec![ModelStreamEvent::Completed(response)])
            }
            _ => Err(unsupported("unsupported Anthropic SSE event")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ion_ai::{GenerationControls, ModelRef, ToolResult, ToolSpec};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    async fn serve(body: String, status: &str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1/messages", listener.local_addr().unwrap());
        let status = status.to_owned();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let _ = socket.read(&mut request).await.unwrap();
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        endpoint
    }

    fn request() -> ModelRequest {
        ModelRequest {
            model: ModelRef {
                provider: "test".into(),
                model: "test-model".into(),
            },
            instructions: Some("instructions".into()),
            messages: vec![Message {
                role: Role::User,
                content: vec![Content::Text("hello".into())],
                provider_replay: None,
            }],
            tools: vec![ToolSpec {
                name: "read".into(),
                description: "read".into(),
                input_schema: json!({"type":"object"}),
            }],
            controls: GenerationControls {
                max_output_tokens: 1024,
                temperature: None,
                top_p: None,
                reasoning: Reasoning::ProviderDefault,
                tool_choice: ToolChoice::Auto,
                parallel_tool_calls: false,
            },
        }
    }

    #[test]
    fn replay_remaps_call_ids_and_rejects_orphans() {
        let mut request = request();
        request.messages.push(Message {
            role: Role::Assistant,
            content: vec![Content::ToolCall(ToolCall {
                id: "provider-id".into(),
                name: "read".into(),
                arguments: json!({"path":"a"}),
            })],
            provider_replay: None,
        });
        request.messages.push(Message {
            role: Role::Tool,
            content: vec![Content::ToolResult(ToolResult {
                call_id: "provider-id".into(),
                name: "read".into(),
                result: json!({"ok":true}),
            })],
            provider_replay: None,
        });
        request.messages.push(Message {
            role: Role::User,
            content: vec![Content::Text("again".into())],
            provider_replay: None,
        });
        let chat = chat_body(&request, HttpWire::ChatCompletions).unwrap();
        assert_eq!(chat["messages"][2]["tool_calls"][0]["id"], "ion_call_0");
        assert_eq!(chat["messages"][3]["tool_call_id"], "ion_call_0");
        let anthropic = anthropic_body(&request).unwrap();
        assert_eq!(anthropic["messages"][1]["content"][0]["id"], "ion_call_0");
        assert_eq!(
            anthropic["messages"][2]["content"][0]["tool_use_id"],
            "ion_call_0"
        );
        request.messages[2].content = vec![Content::ToolResult(ToolResult {
            call_id: "wrong".into(),
            name: "read".into(),
            result: json!(null),
        })];
        assert!(chat_body(&request, HttpWire::ChatCompletions).is_err());
    }

    #[test]
    fn controls_are_encoded_or_rejected() {
        let mut request = request();
        request.controls.temperature = Some(0.4);
        request.controls.top_p = Some(0.9);
        request.controls.reasoning = Reasoning::High;
        let chat = chat_body(&request, HttpWire::ChatCompletions).unwrap();
        assert_eq!(chat["temperature"], 0.4);
        assert_eq!(chat["top_p"], 0.9);
        assert_eq!(chat["reasoning_effort"], "high");
        assert_eq!(
            anthropic_body(&request).unwrap_err().kind,
            ProviderErrorKind::Unsupported
        );
        request.controls.reasoning = Reasoning::BudgetTokens(100);
        assert_eq!(
            chat_body(&request, HttpWire::ChatCompletions)
                .unwrap_err()
                .kind,
            ProviderErrorKind::Unsupported
        );
    }

    #[test]
    fn flash_profiles_disable_unreplayable_thinking() {
        let request = request();
        let deepseek = chat_body(&request, HttpWire::DeepSeekChat).unwrap();
        assert_eq!(deepseek["reasoning_effort"], "none");
        assert_eq!(deepseek["max_tokens"], request.controls.max_output_tokens);
        assert!(deepseek.get("max_completion_tokens").is_none());
        let mimo = chat_body(&request, HttpWire::MiMoChat).unwrap();
        assert_eq!(mimo["thinking"]["type"], "disabled");
        let openrouter = chat_body(&request, HttpWire::OpenRouterNoReasoning).unwrap();
        assert_eq!(openrouter["reasoning"]["enabled"], false);

        let mut thinking = request;
        thinking.controls.reasoning = Reasoning::High;
        for wire in [
            HttpWire::DeepSeekChat,
            HttpWire::MiMoChat,
            HttpWire::OpenRouterNoReasoning,
        ] {
            assert_eq!(
                chat_body(&thinking, wire).unwrap_err().kind,
                ProviderErrorKind::Unsupported
            );
        }
        let mut state = ChatState::default();
        assert_eq!(state.accept(&json!({"choices":[{"delta":{"reasoning_content":"hidden"},"finish_reason":null}]})).unwrap_err().kind, ProviderErrorKind::Unsupported);
    }

    #[test]
    fn chat_completion_needs_finish_and_valid_tool() {
        let request = request();
        let mut state = ChatState::default();
        state.accept(&json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"read","arguments":"{}"}}]},"finish_reason":"tool_calls"}]})).unwrap();
        assert_eq!(state.complete(&request).unwrap().message.content.len(), 1);
        let mut state = ChatState::default();
        state.accept(&json!({"choices":[]})).unwrap();
        assert!(state.complete(&request).is_err());
    }

    #[test]
    fn chat_accepts_empty_openrouter_usage_chunk_after_finish() {
        let request = request();
        let mut state = ChatState::default();
        state.accept(&json!({"choices":[{"index":0,"delta":{"role":"assistant","content":"ok"},"finish_reason":null}]})).unwrap();
        state.accept(&json!({"choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":"stop"}]})).unwrap();
        state.accept(&json!({"usage":{"prompt_tokens":2,"completion_tokens":1},"choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":"stop"}]})).unwrap();
        assert_eq!(state.complete(&request).unwrap().usage, Usage::known(2, 1));

        let mut state = ChatState::default();
        state
            .accept(&json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}))
            .unwrap();
        assert!(state.accept(&json!({"choices":[{"index":0,"delta":{"content":"late"},"finish_reason":"stop"}]})).is_err());
    }

    #[test]
    fn sse_parsing_preserves_data_and_rejects_bad_utf8() {
        let frame = parse_frame(b": ping\r\ndata: {\"a\":\r\ndata: 1}\r\n\r\n")
            .unwrap()
            .unwrap();
        assert_eq!(frame.data, "{\"a\":\n1}");
        assert!(parse_frame(b"data: \xff\n\n").is_err());
    }

    #[tokio::test]
    async fn chat_stream_requires_done_and_reports_usage() {
        let body = concat!(
            "data: {\"model\":\"returned\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2}}\n\n",
            "data: [DONE]\n\n"
        );
        let endpoint = serve(body.into(), "200 OK").await;
        let model =
            HttpModelService::new(&endpoint, HttpWire::ChatCompletions, Arc::new(|| None)).unwrap();
        let mut stream = model.stream(request()).await.unwrap();
        let mut completed = None;
        while let Some(event) = stream.next().await {
            if let ModelStreamEvent::Completed(response) = event.unwrap() {
                completed = Some(response);
            }
        }
        let response = completed.unwrap();
        assert_eq!(response.returned_model.as_deref(), Some("returned"));
        assert_eq!(response.usage, Usage::known(3, 2));
        assert_eq!(response.message.content, vec![Content::Text("hi".into())]);

        let endpoint = serve(body.replace("data: [DONE]\n\n", ""), "200 OK").await;
        let model =
            HttpModelService::new(&endpoint, HttpWire::ChatCompletions, Arc::new(|| None)).unwrap();
        let mut stream = model.stream(request()).await.unwrap();
        while let Some(event) = stream.next().await {
            if let Err(error) = event {
                assert_eq!(error.kind, ProviderErrorKind::Transport);
                return;
            }
        }
        panic!("stream without [DONE] must fail");
    }

    #[tokio::test]
    async fn anthropic_stream_closes_tool_block_before_completion() {
        let body = concat!(
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"returned\",\"content\":[],\"usage\":{\"input_tokens\":3,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tool_1\",\"name\":\"read\",\"input\":{}}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"x\\\"}\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":5}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
        );
        let endpoint = serve(body.into(), "200 OK").await;
        let model =
            HttpModelService::new(&endpoint, HttpWire::AnthropicMessages, Arc::new(|| None))
                .unwrap();
        let mut stream = model.stream(request()).await.unwrap();
        let mut completed = None;
        while let Some(event) = stream.next().await {
            if let ModelStreamEvent::Completed(response) = event.unwrap() {
                completed = Some(response);
            }
        }
        let response = completed.unwrap();
        assert_eq!(response.usage, Usage::known(3, 5));
        assert_eq!(
            response.message.content,
            vec![Content::ToolCall(ToolCall {
                id: "tool_1".into(),
                name: "read".into(),
                arguments: json!({"path":"x"})
            })]
        );
    }
}
