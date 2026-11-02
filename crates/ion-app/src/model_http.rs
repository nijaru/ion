//! HTTP model transports. Provider wire formats stay outside Session state.
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};

use async_stream::try_stream;
use futures_util::StreamExt;
use ion_ai::{
    BoxFuture, Content, IncompleteReason, Message, ModelContextState, ModelRequest, ModelResponse,
    ModelService, ModelStream, ModelStreamEvent, PromptCacheIntent, ProviderError,
    ProviderErrorKind, ProviderReplay, Reasoning, ResponseTermination, Role, ToolCall, ToolChoice,
    ToolSpec, Usage,
};
#[cfg(test)]
use ion_ai::{ModelRoute, ModelRouteReason};
use reqwest::{
    Client, Url,
    header::{AUTHORIZATION, HeaderMap, HeaderValue},
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::{CredentialResolver, catalog::ModelCapabilities};

mod anthropic;
mod anthropic_request;
mod chat;
mod chat_request;
mod controls;
mod errors;
mod limits;
mod responses;
mod responses_request;
mod tool_ids;
mod transcript;
use anthropic::AnthropicState;
#[cfg(test)]
use anthropic::anthropic_replay_notices;
#[cfg(test)]
use anthropic_request::anthropic_body;
use anthropic_request::{
    anthropic_body_for_route, anthropic_body_prefix_digest, anthropic_body_uses_inline_tools,
    managed_anthropic_thinking,
};
use chat::ChatState;
use chat_request::chat_body;
use controls::control_fields;
use errors::{
    anthropic_sse_error, chat_stream_error, classify_status_error, provider_error_detail_value,
};
use limits::{ResponseBudget, StreamBudget};
use responses::ResponsesState;
use responses_request::responses_body;

const MAX_FRAME: usize = 256 * 1024;
const MAX_ERROR_BODY: usize = 16 * 1024;
const PROVIDER_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
const CHAT_REASONING_CONTENT_REPLAY: &str = "chat_reasoning_content";
const OPENROUTER_PLAIN_REASONING_REPLAY: &str = "openrouter_plain_reasoning";
// Scope structured/signed replay to the tool-status envelope and withholding
// projection. Reject prior records rather than rewriting a possibly signed
// prefix when Core reconstructs model-visible tool messages after reopen.
const OPENROUTER_DETAILS_REPLAY: &str = "openrouter_reasoning_details_observed_tool_status";
const ANTHROPIC_CONTENT_REPLAY: &str = "anthropic_content_blocks";
const ANTHROPIC_BINDING_BETA: &str = "thinking-binding-controls-2026-08-01";
const ANTHROPIC_INLINE_TOOLS_BETA: &str = "inline-tools-2026-09-15";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpWire {
    ChatCompletions,
    LlamaCppNoThinking,
    DeepSeekChat,
    MiMoChat,
    OpenRouterChat,
    AnthropicMessages,
    OpenAiResponses,
}

impl HttpWire {
    fn is_chat(self) -> bool {
        match self {
            Self::ChatCompletions
            | Self::LlamaCppNoThinking
            | Self::DeepSeekChat
            | Self::MiMoChat
            | Self::OpenRouterChat => true,
            Self::AnthropicMessages | Self::OpenAiResponses => false,
        }
    }

    fn frame_limit(self) -> usize {
        // Responses terminal events repeat the full output, not just a delta.
        if self == Self::OpenAiResponses {
            8 * 1024 * 1024
        } else {
            MAX_FRAME
        }
    }
}

pub struct HttpModelService {
    client: Client,
    endpoint: Url,
    wire: HttpWire,
    credentials: Arc<dyn CredentialResolver>,
    capabilities: ModelCapabilities,
    context_window_tokens: Option<u32>,
}

impl HttpModelService {
    /// Validate an explicit route with the same URL rule used at dispatch.
    pub fn validate_endpoint(endpoint: &str) -> Result<(), ProviderError> {
        parse_endpoint(endpoint).map(|_| ())
    }

    /// Resolve a compatible API base URL to the request path for its wire.
    /// An already complete standard request URL is kept as-is.
    pub fn resolve_endpoint(
        base_or_endpoint: &str,
        wire: HttpWire,
    ) -> Result<String, ProviderError> {
        let mut url = parse_endpoint(base_or_endpoint)?;
        let path = url.path().trim_end_matches('/');
        let complete = match wire {
            HttpWire::AnthropicMessages => path.ends_with("/v1/messages"),
            HttpWire::OpenAiResponses => path.ends_with("/responses"),
            _ => path.ends_with("/chat/completions"),
        };
        let has_anthropic_version = path.ends_with("/v1");
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| invalid("provider endpoint cannot contain path segments"))?;
        segments.pop_if_empty();
        if !complete {
            match wire {
                HttpWire::AnthropicMessages => {
                    if !has_anthropic_version {
                        segments.push("v1");
                    }
                    segments.push("messages");
                }
                HttpWire::OpenAiResponses => {
                    segments.push("responses");
                }
                _ => {
                    segments.push("chat").push("completions");
                }
            }
        }
        drop(segments);
        Ok(url.to_string())
    }

    pub fn new(
        endpoint: &str,
        wire: HttpWire,
        credentials: Arc<dyn CredentialResolver>,
    ) -> Result<Self, ProviderError> {
        Self::new_with_capabilities(
            endpoint,
            wire,
            credentials,
            ModelCapabilities::conservative(),
            None,
        )
    }

    pub fn new_with_capabilities(
        endpoint: &str,
        wire: HttpWire,
        credentials: Arc<dyn CredentialResolver>,
        capabilities: ModelCapabilities,
        context_window_tokens: Option<u32>,
    ) -> Result<Self, ProviderError> {
        let endpoint = parse_endpoint(endpoint)?;
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| transport("HTTP client setup failed"))?;
        Ok(Self {
            client,
            endpoint,
            wire,
            credentials,
            capabilities,
            context_window_tokens,
        })
    }
}

fn parse_endpoint(endpoint: &str) -> Result<Url, ProviderError> {
    let endpoint = Url::parse(endpoint).map_err(|_| invalid("invalid provider endpoint"))?;
    if !matches!(endpoint.scheme(), "http" | "https") {
        return Err(invalid("provider endpoint must use HTTP or HTTPS"));
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
    Ok(endpoint)
}

impl ModelService for HttpModelService {
    fn validate_controls(
        &self,
        model: &ion_ai::ModelRef,
        controls: &ion_ai::GenerationControls,
    ) -> Result<(), ProviderError> {
        let native = self.wire == HttpWire::AnthropicMessages
            && self.endpoint.host_str() == Some("api.anthropic.com");
        control_fields(model, controls, self.wire, native).map(|_| ())
    }

    fn stream<'a>(
        &'a self,
        request: ModelRequest,
    ) -> BoxFuture<'a, Result<ModelStream, ProviderError>> {
        Box::pin(async move {
            let native_anthropic = self.wire == HttpWire::AnthropicMessages
                && self.endpoint.host_str() == Some("api.anthropic.com");
            let mut body = match self.wire {
                HttpWire::OpenAiResponses => responses_body(&request)?,
                HttpWire::AnthropicMessages => anthropic_body_for_route(
                    &request,
                    native_anthropic,
                    self.capabilities.context_mutation.inline_tool_definitions,
                )?,
                _ => chat_body(&request, self.wire)?,
            };
            if native_anthropic
                && request.prompt_cache == PromptCacheIntent::Reusable
                && self.capabilities.prompt_cache.automatic_request
            {
                body["cache_control"] = json!({"type":"ephemeral"});
            }
            let inline_tools = anthropic_body_uses_inline_tools(&body);
            let mut decoder = match self.wire {
                HttpWire::OpenAiResponses => Decoder::Responses(ResponsesState::default()),
                HttpWire::AnthropicMessages => {
                    Decoder::Anthropic(AnthropicState::new(anthropic_body_prefix_digest(&body)?))
                }
                _ => Decoder::Chat(ChatState::new(self.wire)),
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
                let header_text = if self.wire == HttpWire::AnthropicMessages {
                    key
                } else {
                    format!("Bearer {key}")
                };
                let mut header = HeaderValue::from_str(&header_text).map_err(|_| {
                    error(
                        ProviderErrorKind::Authentication,
                        "invalid provider credential",
                    )
                })?;
                header.set_sensitive(true);
                post = if self.wire == HttpWire::AnthropicMessages {
                    post.header("x-api-key", header)
                } else {
                    post.header(AUTHORIZATION, header)
                };
            }
            if self.wire == HttpWire::AnthropicMessages {
                post = post.header("anthropic-version", "2023-06-01");
                let mut betas = Vec::new();
                if native_anthropic && managed_anthropic_thinking(&request.route.effective.model) {
                    betas.push(ANTHROPIC_BINDING_BETA);
                }
                if native_anthropic && inline_tools {
                    betas.push(ANTHROPIC_INLINE_TOOLS_BETA);
                }
                if !betas.is_empty() {
                    post = post.header("anthropic-beta", betas.join(","));
                }
            }
            let mut response = tokio::time::timeout(PROVIDER_IDLE_TIMEOUT, post.send())
                .await
                .map_err(|_| {
                    error(
                        ProviderErrorKind::Timeout,
                        "provider response headers timed out",
                    )
                })?
                .map_err(|_| transport("provider HTTP request failed"))?;
            if !response.status().is_success() {
                let status = response.status().as_u16();
                let retry_after_ms = parse_retry_after(response.headers());
                let mut body = Vec::new();
                while body.len() < MAX_ERROR_BODY {
                    let Some(chunk) = tokio::time::timeout(PROVIDER_IDLE_TIMEOUT, response.chunk())
                        .await
                        .map_err(|_| {
                            error(
                                ProviderErrorKind::Timeout,
                                "provider error response timed out",
                            )
                        })?
                        .map_err(|_| transport("provider HTTP error response failed"))?
                    else {
                        break;
                    };
                    append_error_prefix(&mut body, &chunk);
                }
                let value = serde_json::from_slice::<Value>(&body).ok();
                let kind = classify_status_error(status, value.as_ref());
                let detail = value.as_ref().and_then(provider_error_detail_value);
                let mut message = detail.map_or_else(
                    || format!("provider returned HTTP {status}"),
                    |detail| format!("provider returned HTTP {status}: {detail}"),
                );
                if let Some(delay_ms) = retry_after_ms {
                    message.push_str(&format!("; retry after {} ms", delay_ms));
                }
                return Err(ProviderError {
                    kind,
                    message,
                    retry_after_ms,
                });
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
            let context_window_tokens = self.context_window_tokens;
            let stream: ModelStream = Box::pin(try_stream! {
                let mut bytes = response.bytes_stream();
                let mut frame = Vec::new();
                let mut budget = StreamBudget::default();
                while let Some(next) = tokio::time::timeout(PROVIDER_IDLE_TIMEOUT, bytes.next())
                    .await
                    .map_err(|_| error(ProviderErrorKind::Timeout, "provider stream idle timeout"))? {
                    let chunk = next.map_err(|_| transport("provider stream transport failed"))?;
                    for byte in chunk {
                        budget.byte()?;
                        if frame.len() == wire.frame_limit() { Err(invalid("provider SSE frame exceeded byte limit"))?; }
                        frame.push(byte);
                        // SSE ignores exactly one initial UTF-8 BOM. The absolute
                        // byte count recognizes it even across transport chunks.
                        if budget.wire_bytes == 3 && frame.as_slice() == b"\xef\xbb\xbf" {
                            frame.clear();
                            continue;
                        }
                        if !has_sse_event_boundary(&frame) { continue; }
                        let events = decode_stream_frame(&frame, &mut decoder, &request, wire, &mut budget)?;
                        frame.clear();
                        let failure = decoder.take_error();
                        for event in events {
                            let event = normalize_capacity(event, wire, context_window_tokens);
                            let terminal = matches!(event, ModelStreamEvent::Completed(_));
                            yield event;
                            if terminal { return; }
                        }
                        if let Some(failure) = failure { Err(failure)?; }
                    }
                }
                // SSE permits a final event without a trailing blank line. Some
                // Chat Completions servers also close after finish_reason rather
                // than sending a separate [DONE] sentinel.
                if !frame.is_empty() {
                    let events = decode_stream_frame(&frame, &mut decoder, &request, wire, &mut budget)?;
                    let failure = decoder.take_error();
                    for event in events {
                        let event = normalize_capacity(event, wire, context_window_tokens);
                        let terminal = matches!(event, ModelStreamEvent::Completed(_));
                        yield event;
                        if terminal { return; }
                    }
                    if let Some(failure) = failure { Err(failure)?; }
                }
                if wire.is_chat() {
                    yield normalize_capacity(ModelStreamEvent::Completed(decoder.complete(&request)?), wire, context_window_tokens);
                    return;
                }
                Err(transport("provider stream ended before completion"))?;
            });
            Ok(stream)
        })
    }
}

fn normalize_capacity(
    mut event: ModelStreamEvent,
    wire: HttpWire,
    context_window_tokens: Option<u32>,
) -> ModelStreamEvent {
    if let ModelStreamEvent::Completed(response) = &mut event {
        // MiMo reports a full context as a zero-output length stop. Interpret
        // that wire behavior here, independent of the caller's provider alias.
        if wire == HttpWire::MiMoChat
            && response.termination
                == ResponseTermination::Incomplete(IncompleteReason::MaxOutputTokens)
            && response.usage.output_tokens == Some(0)
            && response.usage.input_tokens.is_some_and(|input| {
                context_window_tokens
                    .is_some_and(|window| u128::from(input) * 100 >= u128::from(window) * 99)
            })
        {
            response.termination = ResponseTermination::Incomplete(IncompleteReason::ContextLength);
        }
    }
    event
}

fn append_error_prefix(body: &mut Vec<u8>, chunk: &[u8]) {
    let retained = chunk.len().min(MAX_ERROR_BODY - body.len());
    body.extend_from_slice(&chunk[..retained]);
}

fn parse_retry_after(headers: &HeaderMap) -> Option<u64> {
    if let Some(value) = headers
        .get("retry-after-ms")
        .and_then(|value| value.to_str().ok())
        && let Ok(ms) = value.trim().parse::<u64>()
    {
        return Some(ms);
    }
    let value = headers.get("retry-after")?.to_str().ok()?.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return seconds.checked_mul(1000);
    }
    let instant = httpdate::parse_http_date(value).ok()?;
    Some(
        instant
            .duration_since(std::time::SystemTime::now())
            .unwrap_or_default()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX),
    )
}

fn error(kind: ProviderErrorKind, message: &str) -> ProviderError {
    ProviderError {
        kind,
        message: message.into(),
        retry_after_ms: None,
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
fn decode_stream_frame(
    frame: &[u8],
    decoder: &mut Decoder,
    request: &ModelRequest,
    wire: HttpWire,
    budget: &mut StreamBudget,
) -> Result<Vec<ModelStreamEvent>, ProviderError> {
    let before = decoder.retained_bytes();
    let events = decode_frame(frame, decoder, request, wire)?;
    let progress = decoder.retained_bytes() > before
        || events
            .iter()
            .any(|event| matches!(event, ModelStreamEvent::Completed(_)));
    budget.frame(frame.len(), progress)?;
    Ok(events)
}

fn decode_frame(
    frame: &[u8],
    decoder: &mut Decoder,
    request: &ModelRequest,
    wire: HttpWire,
) -> Result<Vec<ModelStreamEvent>, ProviderError> {
    let Some(parsed) = parse_frame(frame)? else {
        return Ok(Vec::new());
    };
    if wire.is_chat() && parsed.data == "[DONE]" {
        return Ok(vec![ModelStreamEvent::Completed(
            decoder.complete(request)?,
        )]);
    }
    let value: Value = serde_json::from_str(&parsed.data)
        .map_err(|_| invalid("provider sent invalid SSE JSON"))?;
    if wire == HttpWire::AnthropicMessages && parsed.event.as_deref() != value["type"].as_str() {
        return Err(invalid("Anthropic SSE event and data type disagree"));
    }
    if wire == HttpWire::OpenAiResponses
        && parsed
            .event
            .as_deref()
            .is_some_and(|event| Some(event) != value["type"].as_str())
    {
        return Err(invalid("Responses SSE event and data type disagree"));
    }
    decoder.accept(&value, request)
}
fn parse_frame(bytes: &[u8]) -> Result<Option<SseFrame>, ProviderError> {
    let text = std::str::from_utf8(bytes).map_err(|_| invalid("provider sent non-UTF-8 SSE"))?;
    let mut event = None;
    let mut data = String::new();
    let mut has_data = false;
    for line in text.split(['\r', '\n']) {
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
        return Ok(None);
    }
    Ok(Some(SseFrame { event, data }))
}

fn has_sse_event_boundary(frame: &[u8]) -> bool {
    fn last_line_break_start(bytes: &[u8]) -> Option<usize> {
        match bytes.last()? {
            b'\n' if bytes.get(bytes.len().saturating_sub(2)) == Some(&b'\r') => {
                Some(bytes.len() - 2)
            }
            b'\n' | b'\r' => Some(bytes.len() - 1),
            _ => None,
        }
    }
    last_line_break_start(frame)
        .is_some_and(|last_start| last_line_break_start(&frame[..last_start]).is_some())
}

fn validate_request(request: &ModelRequest) -> Result<(), ProviderError> {
    if request.route.effective.model.is_empty() || request.messages.is_empty() {
        return Err(invalid("model and conversation must be nonempty"));
    }
    if request.tools.is_empty()
        && matches!(
            request.controls.tool_choice,
            ToolChoice::Required | ToolChoice::Named(_)
        )
    {
        return Err(invalid("required tool choice has no tools"));
    }
    validate_tool_specs(&request.tools)?;
    if let ToolChoice::Named(name) = &request.controls.tool_choice
        && !request.tools.iter().any(|tool| &tool.name == name)
    {
        return Err(invalid("named tool is not in the loadout"));
    }
    if let Some(timeline) = &request.context_timeline {
        validate_tool_specs(&timeline.initial.tools)?;
        let mut previous_boundary = 0usize;
        for change in &timeline.changes {
            if change.after_message == 0
                || change.after_message > request.messages.len()
                || change.after_message < previous_boundary
            {
                return Err(invalid("invalid model context timeline boundary"));
            }
            previous_boundary = change.after_message;
            validate_tool_specs(&change.context.tools)?;
        }
        let effective = timeline
            .changes
            .last()
            .map_or(&timeline.initial, |change| &change.context);
        if effective.instructions != request.instructions || effective.tools != request.tools {
            return Err(invalid(
                "model context timeline does not match effective request context",
            ));
        }
    }
    Ok(())
}

fn validate_tool_specs(tools: &[ToolSpec]) -> Result<(), ProviderError> {
    let mut names = BTreeSet::new();
    for tool in tools {
        if !valid_identifier(&tool.name)
            || !names.insert(&tool.name)
            || tool.input_schema.get("type").and_then(Value::as_str) != Some("object")
        {
            return Err(invalid("invalid or duplicate tool specification"));
        }
    }
    Ok(())
}

fn valid_identifier(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
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
    // A model can invent a tool name. Keep the call in the transcript and let
    // the host return a tool-not-found result so the next model step can fix it.
    Ok(())
}

fn parse_tool_arguments(raw: String) -> (Value, Option<String>) {
    match serde_json::from_str::<Value>(&raw) {
        Ok(arguments) if arguments.is_object() => (arguments, None),
        _ => (json!({}), Some(raw)),
    }
}

enum Decoder {
    Chat(ChatState),
    Anthropic(AnthropicState),
    Responses(ResponsesState),
}
impl Decoder {
    fn take_error(&mut self) -> Option<ProviderError> {
        match self {
            Self::Responses(state) => state.pending_error.take(),
            _ => None,
        }
    }

    fn retained_bytes(&self) -> usize {
        match self {
            Self::Chat(state) => state.budget.used,
            Self::Anthropic(state) => state.budget.used,
            Self::Responses(state) => state.budget.used,
        }
    }

    fn accept(
        &mut self,
        value: &Value,
        request: &ModelRequest,
    ) -> Result<Vec<ModelStreamEvent>, ProviderError> {
        match self {
            Self::Chat(state) => state.accept(value),
            Self::Anthropic(state) => state.accept(value, request),
            Self::Responses(state) => state.accept(value, request),
        }
    }
    fn complete(&mut self, request: &ModelRequest) -> Result<ModelResponse, ProviderError> {
        match self {
            Self::Chat(state) => std::mem::take(state).complete(request),
            Self::Anthropic(_) => Err(invalid("[DONE] is not an Anthropic completion")),
            Self::Responses(_) => Err(invalid("Responses requires a terminal response event")),
        }
    }
}

fn valid_openrouter_detail(detail: &Value) -> bool {
    let Some(fields) = detail.as_object() else {
        return false;
    };
    if fields
        .get("id")
        .is_some_and(|value| !value.is_null() && !value.is_string())
        || fields.get("format").is_some_and(|value| !value.is_string())
        || fields.get("index").is_some_and(|value| !value.is_number())
        || fields
            .get("signature")
            .is_some_and(|value| !value.is_null() && !value.is_string())
    {
        return false;
    }
    match fields.get("type").and_then(Value::as_str) {
        Some("reasoning.text") => {
            match fields.get("text") {
                Some(text) => text.is_string(),
                // Gemini can stream a signature without a text fragment.
                None => fields
                    .get("signature")
                    .and_then(Value::as_str)
                    .is_some_and(|signature| !signature.is_empty()),
            }
        }
        Some("reasoning.summary") => fields.get("summary").is_some_and(Value::is_string),
        Some("reasoning.encrypted") => fields.get("data").is_some_and(Value::is_string),
        _ => false,
    }
}

fn valid_openrouter_details(value: &Value) -> bool {
    value
        .as_array()
        .is_some_and(|details| !details.is_empty() && details.iter().all(valid_openrouter_detail))
}

#[cfg(test)]
mod tests;
