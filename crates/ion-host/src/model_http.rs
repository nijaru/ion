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
mod chat;
#[cfg(test)]
use anthropic::anthropic_body;
#[cfg(test)]
use anthropic::anthropic_replay_notices;
use anthropic::{
    AnthropicState, anthropic_body_for_route, anthropic_body_prefix_digest,
    anthropic_body_uses_inline_tools, managed_anthropic_thinking, validated_anthropic_replay,
};
use chat::{ChatState, chat_body};

const MAX_FRAME: usize = 256 * 1024;
const MAX_RESPONSE: usize = 8 * 1024 * 1024;
const PROVIDER_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
const CHAT_REASONING_CONTENT_REPLAY: &str = "chat_reasoning_content";
const OPENROUTER_PLAIN_REASONING_REPLAY: &str = "openrouter_plain_reasoning";
const OPENROUTER_DETAILS_REPLAY: &str = "openrouter_reasoning_details";
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
        let complete = if wire.is_chat() {
            path.ends_with("/chat/completions")
        } else {
            path.ends_with("/v1/messages")
        };
        let has_anthropic_version = path.ends_with("/v1");
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| invalid("provider endpoint cannot contain path segments"))?;
        segments.pop_if_empty();
        if !complete {
            if wire.is_chat() {
                segments.push("chat").push("completions");
            } else {
                if !has_anthropic_version {
                    segments.push("v1");
                }
                segments.push("messages");
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
    fn stream<'a>(
        &'a self,
        request: ModelRequest,
    ) -> BoxFuture<'a, Result<ModelStream, ProviderError>> {
        Box::pin(async move {
            request.controls.validate()?;
            let native_anthropic = self.wire == HttpWire::AnthropicMessages
                && self.endpoint.host_str() == Some("api.anthropic.com");
            let mut body = if self.wire.is_chat() {
                chat_body(&request, self.wire)?
            } else {
                anthropic_body_for_route(
                    &request,
                    native_anthropic,
                    self.capabilities.context_mutation.inline_tool_definitions,
                )?
            };
            if native_anthropic
                && request.prompt_cache == PromptCacheIntent::Reusable
                && self.capabilities.prompt_cache.automatic_request
            {
                body["cache_control"] = json!({"type":"ephemeral"});
            }
            let inline_tools = anthropic_body_uses_inline_tools(&body);
            let anthropic_prefix = (self.wire == HttpWire::AnthropicMessages)
                .then(|| anthropic_body_prefix_digest(&body))
                .transpose()?;
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
                while body.len() < 16 * 1024 {
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
                    if body.len() + chunk.len() > 16 * 1024 {
                        break;
                    }
                    body.extend_from_slice(&chunk);
                }
                let kind = classify_http_error(status, &body);
                let detail = provider_error_detail(&body);
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
                let mut total = 0usize;
                let mut decoder = if wire.is_chat() { Decoder::Chat(ChatState::new(wire)) } else { Decoder::Anthropic(AnthropicState::new(anthropic_prefix)) };
                while let Some(next) = tokio::time::timeout(PROVIDER_IDLE_TIMEOUT, bytes.next())
                    .await
                    .map_err(|_| error(ProviderErrorKind::Timeout, "provider stream idle timeout"))? {
                    let chunk = next.map_err(|_| transport("provider stream transport failed"))?;
                    for byte in chunk {
                        if total == MAX_RESPONSE { Err(invalid("provider stream exceeded byte limit"))?; }
                        total += 1;
                        if frame.len() == MAX_FRAME { Err(invalid("provider SSE frame exceeded byte limit"))?; }
                        frame.push(byte);
                        if !has_sse_event_boundary(&frame) { continue; }
                        let events = decode_frame(&frame, &mut decoder, &request, wire)?;
                        frame.clear();
                        for event in events {
                            let event = normalize_capacity(event, wire, context_window_tokens);
                            let terminal = matches!(event, ModelStreamEvent::Completed(_));
                            yield event;
                            if terminal { return; }
                        }
                    }
                }
                // SSE permits a final event without a trailing blank line. Some
                // Chat Completions servers also close after finish_reason rather
                // than sending a separate [DONE] sentinel.
                if !frame.is_empty() {
                    for event in decode_frame(&frame, &mut decoder, &request, wire)? {
                        let event = normalize_capacity(event, wire, context_window_tokens);
                        let terminal = matches!(event, ModelStreamEvent::Completed(_));
                        yield event;
                        if terminal { return; }
                    }
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

fn classify_http_error(status: u16, body: &[u8]) -> ProviderErrorKind {
    if status == 429 {
        let value = serde_json::from_slice::<Value>(body).ok();
        let code = value.as_ref().and_then(|value| {
            value
                .pointer("/error/code")
                .and_then(Value::as_str)
                .or_else(|| value.get("code").and_then(Value::as_str))
        });
        if matches!(
            code,
            Some("insufficient_quota" | "billing_hard_limit_reached")
        ) {
            return ProviderErrorKind::Quota;
        }
    }
    if matches!(status, 400 | 413) {
        let value = serde_json::from_slice::<Value>(body).ok();
        let code = value.as_ref().and_then(|value| {
            value
                .pointer("/error/code")
                .and_then(Value::as_str)
                .or_else(|| value.pointer("/error/type").and_then(Value::as_str))
                .or_else(|| value.get("code").and_then(Value::as_str))
        });
        if matches!(
            code,
            Some(
                "context_length_exceeded"
                    | "model_context_window_exceeded"
                    | "context_window_exceeded"
                    | "request_too_large"
            )
        ) {
            return ProviderErrorKind::ContextLength;
        }
        let message = value.as_ref().and_then(|value| {
            value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .or_else(|| value.get("message").and_then(Value::as_str))
        });
        if message.is_some_and(is_context_error_message) {
            return ProviderErrorKind::ContextLength;
        }
    }
    match status {
        401 => ProviderErrorKind::Authentication,
        403 => ProviderErrorKind::Permission,
        408 => ProviderErrorKind::Timeout,
        429 => ProviderErrorKind::RateLimited,
        503 | 529 => ProviderErrorKind::Overloaded,
        500..=599 => ProviderErrorKind::Server,
        _ => ProviderErrorKind::InvalidRequest,
    }
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

fn is_context_error_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    [
        "context length",
        "context window",
        "prompt is too long",
        "prompt too long",
        "available context size",
        "request_too_large",
    ]
    .iter()
    .any(|part| lower.contains(part))
}

fn provider_error_detail(body: &[u8]) -> Option<String> {
    let value = serde_json::from_slice::<Value>(body).ok()?;
    provider_error_detail_value(&value)
}

fn provider_error_detail_value(value: &Value) -> Option<String> {
    let detail = value
        .pointer("/error/message")
        .and_then(Value::as_str)
        .or_else(|| value.get("message").and_then(Value::as_str))
        .or_else(|| value.pointer("/error/code").and_then(Value::as_str))?;
    let detail: String = detail
        .chars()
        .filter(|ch| !ch.is_control())
        .take(500)
        .collect();
    (!detail.is_empty()).then_some(detail)
}

fn chat_stream_error(value: &Value) -> ProviderError {
    let code = &value["error"]["code"];
    let kind = if let Some(status) = code.as_u64().and_then(|code| u16::try_from(code).ok()) {
        classify_http_error(status, &[])
    } else {
        match code.as_str() {
            Some("server_error") => ProviderErrorKind::Server,
            Some("rate_limit_exceeded" | "rate_limited") => ProviderErrorKind::RateLimited,
            Some("insufficient_quota") => ProviderErrorKind::Quota,
            Some("context_length_exceeded") => ProviderErrorKind::ContextLength,
            _ => ProviderErrorKind::Transport,
        }
    };
    let message = provider_error_detail_value(value).map_or_else(
        || "provider sent a stream error".to_owned(),
        |detail| format!("provider stream error: {detail}"),
    );
    ProviderError {
        kind,
        message,
        retry_after_ms: None,
    }
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
        if !valid_name(&tool.name)
            || !names.insert(&tool.name)
            || tool.input_schema.get("type").and_then(Value::as_str) != Some("object")
        {
            return Err(invalid("invalid or duplicate tool specification"));
        }
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

#[derive(Debug, Clone)]
struct AnthropicTimelinePlan {
    initial: ModelContextState,
    changes: BTreeMap<usize, Vec<Value>>,
}

/// Keep provider call IDs alongside their results. Some reasoning signatures
/// bind to the original function-call identity during continuation.
fn wire_messages(request: &ModelRequest, wire: HttpWire) -> Result<Vec<Value>, ProviderError> {
    wire_messages_with_anthropic_context(request, wire, None)
}

fn wire_messages_with_anthropic_context(
    request: &ModelRequest,
    wire: HttpWire,
    context_plan: Option<&AnthropicTimelinePlan>,
) -> Result<Vec<Value>, ProviderError> {
    let anthropic = !wire.is_chat();
    let mut messages = Vec::new();
    let mut pending = BTreeMap::<String, String>::new();
    let mut tool_images = Vec::new();
    for (message_index, message) in request.messages.iter().enumerate() {
        let reasoning_content = match &message.provider_replay {
            Some(_) if message.role != Role::Assistant => {
                return Err(invalid("provider replay requires an assistant message"));
            }
            Some(replay) if !replay.is_compatible_with(&request.route.effective.provider) => {
                return Err(unsupported(
                    "provider replay belongs to a different provider",
                ));
            }
            Some(replay)
                if matches!(wire, HttpWire::DeepSeekChat | HttpWire::MiMoChat)
                    && replay.kind == CHAT_REASONING_CONTENT_REPLAY =>
            {
                Some(
                    replay
                        .data
                        .as_str()
                        .ok_or_else(|| invalid("invalid reasoning-content replay"))?,
                )
            }
            Some(replay)
                if wire == HttpWire::OpenRouterChat
                    && replay.kind == OPENROUTER_PLAIN_REASONING_REPLAY =>
            {
                Some(
                    replay
                        .data
                        .as_str()
                        .ok_or_else(|| invalid("invalid OpenRouter reasoning replay"))?,
                )
            }
            Some(replay)
                if wire == HttpWire::OpenRouterChat && replay.kind == OPENROUTER_DETAILS_REPLAY =>
            {
                if !valid_openrouter_details(&replay.data) {
                    return Err(invalid("invalid OpenRouter reasoning replay"));
                }
                None
            }
            Some(replay)
                if wire == HttpWire::AnthropicMessages
                    && replay.kind == ANTHROPIC_CONTENT_REPLAY =>
            {
                None
            }
            Some(_) => return Err(unsupported("provider replay is incompatible with route")),
            None => None,
        };
        if message.content.is_empty() {
            return Err(invalid("empty transcript message"));
        }
        if message.role != Role::Tool && !pending.is_empty() {
            return Err(invalid("unanswered tool calls"));
        }
        if !anthropic && message.role != Role::Tool && !tool_images.is_empty() {
            append_chat_tool_images(&mut messages, &mut tool_images);
        }
        let mut text = String::new();
        let mut blocks = Vec::new();
        let mut user_blocks = Vec::new();
        let mut has_image = false;
        let mut calls = Vec::new();
        let mut results = Vec::new();
        for item in &message.content {
            match (message.role, item) {
                (Role::User | Role::Assistant, Content::Text(part)) => {
                    text.push_str(part);
                    if message.role == Role::User && !part.is_empty() {
                        user_blocks.push(json!({"type":"text","text":part}));
                    }
                    if anthropic && !part.is_empty() {
                        blocks.push(json!({"type":"text","text":part}));
                    }
                }
                (Role::User, Content::Image(image)) => {
                    image
                        .validate()
                        .map_err(|_| invalid("invalid image data in transcript"))?;
                    has_image = true;
                    if anthropic {
                        blocks.push(json!({"type":"image","source":{
                            "type":"base64","media_type":image.mime_type().as_str(),"data":image.data()
                        }}));
                    } else {
                        user_blocks.push(json!({"type":"image_url","image_url":{
                            "url":format!("data:{};base64,{}", image.mime_type().as_str(), image.data())
                        }}));
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
                    pending.insert(call.id.clone(), call.name.clone());
                    if anthropic {
                        blocks.push(json!({"type":"tool_use","id":call.id,"name":call.name,"input":call.arguments}));
                    } else {
                        calls.push(json!({"id":call.id,"type":"function","function":{"name":call.name,"arguments":call.arguments.to_string()}}));
                    }
                }
                (Role::Tool, Content::ToolResult(result)) => {
                    let Some(name) = pending.remove(&result.call_id) else {
                        return Err(invalid("orphan or duplicate tool result"));
                    };
                    if result.name != name {
                        return Err(invalid("tool result name does not match call"));
                    }
                    if anthropic {
                        let mut content =
                            vec![json!({"type":"text","text":result.result.to_string()})];
                        for image in &result.images {
                            image
                                .validate()
                                .map_err(|_| invalid("invalid tool image data in transcript"))?;
                            content.push(json!({"type":"image","source":{
                                "type":"base64","media_type":image.mime_type().as_str(),"data":image.data()
                            }}));
                        }
                        blocks.push(json!({"type":"tool_result","tool_use_id":result.call_id,"content":content,"is_error":result.is_error}));
                    } else {
                        results.push(json!({"role":"tool","tool_call_id":result.call_id,"content":result.result.to_string()}));
                        for image in &result.images {
                            image
                                .validate()
                                .map_err(|_| invalid("invalid tool image data in transcript"))?;
                            tool_images.push(json!({"type":"image_url","image_url":{
                                "url":format!("data:{};base64,{}", image.mime_type().as_str(), image.data())
                            }}));
                        }
                    }
                }
                _ => return Err(invalid("content does not match transcript role")),
            }
        }
        if anthropic {
            if let Some(replay) = &message.provider_replay {
                blocks = validated_anthropic_replay(
                    &replay.data,
                    &message.content,
                    request,
                    &messages,
                    context_plan.map(|plan| &plan.initial),
                )?;
            }
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
            if let Some(change_blocks) = context_plan
                .and_then(|plan| plan.changes.get(&(message_index + 1)))
                .filter(|blocks| !blocks.is_empty())
            {
                messages.push(json!({"role":"system","content":change_blocks}));
            }
        } else if message.role == Role::Tool {
            messages.extend(results);
        } else {
            let role = if message.role == Role::Assistant {
                "assistant"
            } else {
                "user"
            };
            let content = if has_image {
                Value::Array(user_blocks)
            } else if text.is_empty() {
                Value::Null
            } else {
                json!(text)
            };
            let mut value = json!({"role":role,"content":content});
            if !calls.is_empty() {
                value["tool_calls"] = Value::Array(calls);
            }
            if let Some(reasoning) = reasoning_content {
                let field = if wire == HttpWire::OpenRouterChat {
                    "reasoning"
                } else {
                    "reasoning_content"
                };
                value[field] = json!(reasoning);
            } else if message.role == Role::Assistant
                && !request.tools.is_empty()
                && matches!(wire, HttpWire::DeepSeekChat | HttpWire::MiMoChat)
            {
                value["reasoning_content"] = json!("");
            }
            if let Some(replay) = &message.provider_replay
                && replay.kind == OPENROUTER_DETAILS_REPLAY
            {
                value["reasoning_details"] = replay.data.clone();
            }
            messages.push(value);
        }
    }
    if !anthropic && !tool_images.is_empty() {
        append_chat_tool_images(&mut messages, &mut tool_images);
    }
    if !pending.is_empty() {
        return Err(invalid("unanswered tool calls"));
    }
    if anthropic
        && (messages[0]["role"] != "user"
            || messages
                .last()
                .is_some_and(|message| message["role"] == "assistant"))
    {
        return Err(unsupported("Anthropic assistant prefill is unsupported"));
    }
    Ok(messages)
}

fn append_chat_tool_images(messages: &mut Vec<Value>, images: &mut Vec<Value>) {
    let mut content = vec![json!({"type":"text","text":"Attached image(s) from tool result:"})];
    content.append(images);
    messages.push(json!({"role":"user","content":content}));
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
    fn complete(&mut self, request: &ModelRequest) -> Result<ModelResponse, ProviderError> {
        match self {
            Self::Chat(state) => std::mem::take(state).complete(request),
            Self::Anthropic(_) => Err(invalid("[DONE] is not an Anthropic completion")),
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
mod tests {
    use super::*;
    use ion_ai::{
        GenerationControls, ModelContextChange, ModelContextState, ModelContextTimeline, ModelRef,
        ToolResult, ToolSpec,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    async fn serve(body: String, status: &str) -> String {
        serve_split(body, status, usize::MAX).await
    }

    async fn serve_split(body: String, status: &str, chunk_size: usize) -> String {
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
            for chunk in response.as_bytes().chunks(chunk_size) {
                socket.write_all(chunk).await.unwrap();
                tokio::task::yield_now().await;
            }
        });
        endpoint
    }

    fn request() -> ModelRequest {
        ModelRequest {
            route: ModelRoute::direct(
                ModelRef {
                    provider: "test".into(),
                    model: "test-model".into(),
                },
                ModelRouteReason::UserRequest,
            ),
            provider_session_id: None,
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
            context_timeline: None,
            prompt_cache: ion_ai::PromptCacheIntent::Reusable,
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
    fn openrouter_conversation_affinity_is_explicit_and_wire_scoped() {
        let mut request = request();
        request.provider_session_id = Some("018f7d1b-2391-7000-8000-000000000001".into());
        let body = chat_body(&request, HttpWire::OpenRouterChat).unwrap();
        assert_eq!(body["session_id"], "018f7d1b-2391-7000-8000-000000000001");
        for wire in [
            HttpWire::ChatCompletions,
            HttpWire::DeepSeekChat,
            HttpWire::MiMoChat,
            HttpWire::LlamaCppNoThinking,
        ] {
            assert!(
                chat_body(&request, wire)
                    .unwrap()
                    .get("session_id")
                    .is_none()
            );
        }
        assert!(
            anthropic_body(&request, false)
                .unwrap()
                .get("session_id")
                .is_none()
        );
        request.prompt_cache = PromptCacheIntent::Default;
        assert_eq!(
            chat_body(&request, HttpWire::OpenRouterChat).unwrap()["session_id"],
            body["session_id"]
        );
        request.provider_session_id = None;
        assert!(
            chat_body(&request, HttpWire::OpenRouterChat)
                .unwrap()
                .get("session_id")
                .is_none()
        );
        for id in [String::new(), "a".repeat(257)] {
            request.provider_session_id = Some(id);
            assert!(chat_body(&request, HttpWire::OpenRouterChat).is_err());
        }
        request.provider_session_id = Some("a".repeat(256));
        assert!(chat_body(&request, HttpWire::OpenRouterChat).is_ok());
    }

    #[test]
    fn anthropic_inline_tool_deltas_keep_the_initial_top_level_prefix() {
        let search = ToolSpec {
            name: "tool_search".into(),
            description: "search available tools".into(),
            input_schema: json!({"type":"object"}),
        };
        let special = ToolSpec {
            name: "special_lookup".into(),
            description: "specialized lookup".into(),
            input_schema: json!({"type":"object"}),
        };
        let mut request = request();
        request.route.effective.provider = "anthropic".into();
        request.route.effective.model = "claude-opus-5-5".into();
        request.tools = vec![special.clone()];
        request.messages = vec![
            Message::user_input("find a tool".into(), []),
            Message {
                role: Role::Assistant,
                content: vec![Content::ToolCall(ToolCall {
                    id: "search".into(),
                    name: "tool_search".into(),
                    arguments: json!({"query":"special"}),
                    raw_arguments: None,
                })],
                provider_replay: None,
            },
            Message {
                role: Role::Tool,
                content: vec![Content::ToolResult(ToolResult {
                    call_id: "search".into(),
                    name: "tool_search".into(),
                    result: json!({"loaded":["special_lookup"]}),
                    images: Vec::new(),
                    is_error: false,
                })],
                provider_replay: None,
            },
        ];
        request.context_timeline = Some(ModelContextTimeline {
            initial: ModelContextState {
                instructions: request.instructions.clone(),
                tools: vec![search.clone()],
            },
            changes: vec![ModelContextChange {
                after_message: 3,
                context: ModelContextState {
                    instructions: request.instructions.clone(),
                    tools: vec![special.clone()],
                },
            }],
        });

        let body = anthropic_body_for_route(&request, true, true).unwrap();
        assert_eq!(
            body["tools"],
            json!([{
                "name":"tool_search",
                "description":"search available tools",
                "input_schema":{"type":"object"}
            }])
        );
        let system = body["messages"].as_array().unwrap().last().unwrap();
        assert_eq!(system["role"], "system");
        assert_eq!(
            system["content"],
            json!([
                {
                    "type":"tool_removal",
                    "tool":{"type":"tool_reference","name":"tool_search"}
                },
                {
                    "type":"tool_addition",
                    "tool":{
                        "type":"tool_definition",
                        "definition":{
                            "name":"special_lookup",
                            "description":"specialized lookup",
                            "input_schema":{"type":"object"}
                        }
                    }
                }
            ])
        );
        assert!(anthropic_body_uses_inline_tools(&body));
    }

    #[test]
    fn anthropic_inline_tool_definition_replaces_same_name_schema() {
        let old = ToolSpec {
            name: "read".into(),
            description: "old".into(),
            input_schema: json!({"type":"object","properties":{"path":{"type":"string"}}}),
        };
        let new = ToolSpec {
            name: "read".into(),
            description: "new".into(),
            input_schema: json!({"type":"object","properties":{"path":{"type":"string"},"limit":{"type":"integer"}}}),
        };
        let mut request = request();
        request.route.effective.provider = "anthropic".into();
        request.route.effective.model = "claude-opus-5-5".into();
        request.tools = vec![new.clone()];
        request.context_timeline = Some(ModelContextTimeline {
            initial: ModelContextState {
                instructions: request.instructions.clone(),
                tools: vec![old],
            },
            changes: vec![ModelContextChange {
                after_message: 1,
                context: ModelContextState {
                    instructions: request.instructions.clone(),
                    tools: vec![new],
                },
            }],
        });
        let body = anthropic_body_for_route(&request, true, true).unwrap();
        let blocks = body["messages"][1]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "tool_addition");
        assert_eq!(blocks[0]["tool"]["definition"]["description"], "new");
        assert_eq!(blocks[0]["tool"]["definition"]["name"], "read");
    }

    #[test]
    fn anthropic_instruction_change_falls_back_to_latest_leading_context() {
        let mut request = request();
        request.route.effective.provider = "anthropic".into();
        request.route.effective.model = "claude-opus-5-5".into();
        let current = request.tools.clone();
        request.instructions = Some("new instructions".into());
        request.context_timeline = Some(ModelContextTimeline {
            initial: ModelContextState {
                instructions: Some("old instructions".into()),
                tools: current.clone(),
            },
            changes: vec![ModelContextChange {
                after_message: 1,
                context: ModelContextState {
                    instructions: request.instructions.clone(),
                    tools: current,
                },
            }],
        });
        let body = anthropic_body_for_route(&request, true, true).unwrap();
        assert_eq!(body["system"], "new instructions");
        assert!(!anthropic_body_uses_inline_tools(&body));
        assert!(
            body["messages"]
                .as_array()
                .unwrap()
                .iter()
                .all(|message| message["role"] != "system")
        );
    }

    #[test]
    fn anthropic_tool_delta_preserves_signed_replay_prefix() {
        let old_tool = ToolSpec {
            name: "read".into(),
            description: "read".into(),
            input_schema: json!({"type":"object"}),
        };
        let new_tool = ToolSpec {
            name: "write".into(),
            description: "write".into(),
            input_schema: json!({"type":"object"}),
        };
        let mut initial = request();
        initial.route.effective.provider = "anthropic".into();
        initial.route.effective.model = "claude-opus-5-5".into();
        initial.tools = vec![old_tool.clone()];
        let initial_body = anthropic_body(&initial, true).unwrap();
        let prefix = anthropic_body_prefix_digest(&initial_body).unwrap();

        let call = ToolCall {
            id: "tool_1".into(),
            name: "read".into(),
            arguments: json!({"path":"x"}),
            raw_arguments: None,
        };
        let replay = ProviderReplay::new(
            "anthropic",
            ANTHROPIC_CONTENT_REPLAY,
            json!({
                "blocks":[
                    {"type":"thinking","thinking":"","signature":"signature"},
                    {"type":"tool_use","id":"tool_1","name":"read","input":{"path":"x"}}
                ],
                "prefix_sha256":prefix
            }),
        )
        .with_prefix_binding(true);

        let mut continuation = initial.clone();
        continuation.messages.push(Message {
            role: Role::Assistant,
            content: vec![Content::ToolCall(call)],
            provider_replay: Some(replay),
        });
        continuation.messages.push(Message {
            role: Role::Tool,
            content: vec![Content::ToolResult(ToolResult {
                call_id: "tool_1".into(),
                name: "read".into(),
                result: json!({"text":"ok"}),
                images: Vec::new(),
                is_error: false,
            })],
            provider_replay: None,
        });
        continuation.tools = vec![new_tool.clone()];
        continuation.context_timeline = Some(ModelContextTimeline {
            initial: ModelContextState {
                instructions: initial.instructions.clone(),
                tools: vec![old_tool],
            },
            changes: vec![ModelContextChange {
                after_message: 3,
                context: ModelContextState {
                    instructions: continuation.instructions.clone(),
                    tools: vec![new_tool],
                },
            }],
        });

        let body = anthropic_body_for_route(&continuation, true, true).unwrap();
        assert_eq!(body["messages"][1]["content"][0]["type"], "thinking");
        assert_eq!(body["messages"][3]["role"], "system");
        assert!(anthropic_body_uses_inline_tools(&body));
    }

    #[test]
    fn user_images_keep_order_and_encode_for_each_supported_wire() {
        let image: ion_ai::ImageContent = serde_json::from_value(json!({
            "mime_type":"image/png",
            "data":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg=="
        }))
        .unwrap();
        let mut request = request();
        request.messages[0].content = vec![
            Content::Text("inspect".into()),
            Content::Image(image.clone()),
            Content::Text("and explain".into()),
        ];
        let chat = wire_messages(&request, HttpWire::ChatCompletions).unwrap();
        assert_eq!(chat[0]["content"][0]["text"], "inspect");
        assert!(
            chat[0]["content"][1]["image_url"]["url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
        assert_eq!(chat[0]["content"][2]["text"], "and explain");
        let anthropic = wire_messages(&request, HttpWire::AnthropicMessages).unwrap();
        assert_eq!(anthropic[0]["content"][0]["text"], "inspect");
        assert_eq!(
            anthropic[0]["content"][1]["source"]["media_type"],
            "image/png"
        );
        assert_eq!(anthropic[0]["content"][2]["text"], "and explain");

        request.messages[0].content = vec![Content::Text(String::new()), Content::Image(image)];
        let chat = wire_messages(&request, HttpWire::ChatCompletions).unwrap();
        let chat_parts = chat[0]["content"].as_array().unwrap();
        assert_eq!(chat_parts.len(), 1);
        assert_eq!(chat_parts[0]["type"], "image_url");
        let anthropic = wire_messages(&request, HttpWire::AnthropicMessages).unwrap();
        assert_eq!(anthropic[0]["content"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn explicit_http_endpoint_accepts_non_loopback_host() {
        assert!(
            HttpModelService::validate_endpoint("http://desktop:8080/v1/chat/completions").is_ok()
        );
        assert!(HttpModelService::validate_endpoint("file:///tmp/model").is_err());
    }

    #[test]
    fn compatible_base_url_resolves_one_standard_request_path() {
        for (input, wire, expected) in [
            (
                "http://desktop:8080/v1",
                HttpWire::ChatCompletions,
                "http://desktop:8080/v1/chat/completions",
            ),
            (
                "http://desktop:8080/v1/chat/completions",
                HttpWire::ChatCompletions,
                "http://desktop:8080/v1/chat/completions",
            ),
            (
                "https://proxy.example/api/v1/",
                HttpWire::OpenRouterChat,
                "https://proxy.example/api/v1/chat/completions",
            ),
            (
                "https://api.anthropic.com",
                HttpWire::AnthropicMessages,
                "https://api.anthropic.com/v1/messages",
            ),
            (
                "https://proxy.example/anthropic/v1/",
                HttpWire::AnthropicMessages,
                "https://proxy.example/anthropic/v1/messages",
            ),
            (
                "https://proxy.example/anthropic/v1/messages",
                HttpWire::AnthropicMessages,
                "https://proxy.example/anthropic/v1/messages",
            ),
        ] {
            assert_eq!(
                HttpModelService::resolve_endpoint(input, wire).unwrap(),
                expected
            );
        }
        assert!(
            HttpModelService::resolve_endpoint(
                "http://desktop:8080/v1?key=hidden",
                HttpWire::ChatCompletions,
            )
            .is_err()
        );
    }

    #[test]
    fn structured_context_error_is_distinct_from_other_bad_requests() {
        assert_eq!(
            classify_http_error(400, br#"{"error":{"code":"context_length_exceeded"}}"#),
            ProviderErrorKind::ContextLength
        );
        assert_eq!(
            classify_http_error(400, br#"{"error":{"code":"invalid_api_key"}}"#),
            ProviderErrorKind::InvalidRequest
        );
        assert_eq!(
            classify_http_error(413, b""),
            ProviderErrorKind::InvalidRequest
        );
        assert_eq!(
            classify_http_error(413, br#"{"error":{"type":"request_too_large"}}"#),
            ProviderErrorKind::ContextLength
        );
        assert_eq!(
            classify_http_error(400, br#"{"error":{"message":"Prompt is too long"}}"#),
            ProviderErrorKind::ContextLength
        );
        assert_eq!(
            classify_http_error(429, br#"{"error":{"code":"insufficient_quota"}}"#),
            ProviderErrorKind::Quota
        );
        assert_eq!(
            provider_error_detail(br#"{"error":{"message":"bad\nrequest"}}"#).as_deref(),
            Some("badrequest")
        );
    }

    #[test]
    fn anthropic_sse_error_keeps_provider_reason_and_kind() {
        let mut decoder = Decoder::Anthropic(AnthropicState::default());
        let error = decode_frame(
            b"event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Too busy\"}}\n\n",
            &mut decoder,
            &request(),
            HttpWire::AnthropicMessages,
        )
        .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Overloaded);
        assert!(error.message.contains("Too busy"));

        let mut decoder = Decoder::Anthropic(AnthropicState::default());
        let error = decode_frame(
            b"event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"rate_limit_error\",\"message\":\"Slow down\"}}\n\n",
            &mut decoder,
            &request(),
            HttpWire::AnthropicMessages,
        )
        .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::RateLimited);
        assert!(error.message.contains("Slow down"));
    }

    #[test]
    fn anthropic_unknown_sse_event_does_not_interrupt_a_message() {
        let mut decoder = Decoder::Anthropic(AnthropicState::default());
        assert!(
            decode_frame(
                b"event: future_event\ndata: {\"type\":\"future_event\"}\n\n",
                &mut decoder,
                &request(),
                HttpWire::AnthropicMessages,
            )
            .unwrap()
            .is_empty()
        );
        assert!(decode_frame(
            b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"type\":\"message\",\"role\":\"assistant\",\"model\":\"returned\",\"content\":[],\"stop_reason\":null,\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
            &mut decoder,
            &request(),
            HttpWire::AnthropicMessages,
        )
        .is_ok());
        assert!(
            decode_frame(
                b"event: future_event\ndata: {\"type\":\"future_event\"}\n\n",
                &mut decoder,
                &request(),
                HttpWire::AnthropicMessages,
            )
            .unwrap()
            .is_empty()
        );
    }

    #[test]
    fn server_retry_headers_are_parsed_without_unbounded_arithmetic() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", HeaderValue::from_static("2"));
        assert_eq!(parse_retry_after(&headers), Some(2000));
        headers.insert("retry-after-ms", HeaderValue::from_static("75"));
        assert_eq!(parse_retry_after(&headers), Some(75));
        headers.remove("retry-after-ms");
        headers.insert(
            "retry-after",
            HeaderValue::from_static("18446744073709551615"),
        );
        assert_eq!(parse_retry_after(&headers), None);
        let date = httpdate::fmt_http_date(std::time::SystemTime::now() + Duration::from_secs(5));
        headers.insert("retry-after", HeaderValue::from_str(&date).unwrap());
        assert!(parse_retry_after(&headers).is_some_and(|delay| delay <= 5000));
    }

    #[test]
    fn replay_preserves_call_ids_and_rejects_orphans() {
        let mut request = request();
        request.messages.push(Message {
            role: Role::Assistant,
            content: vec![Content::ToolCall(ToolCall {
                id: "provider-id".into(),
                name: "read".into(),
                arguments: json!({"path":"a"}),
                raw_arguments: None,
            })],
            provider_replay: None,
        });
        request.messages.push(Message {
            role: Role::Tool,
            content: vec![Content::ToolResult(ToolResult {
                call_id: "provider-id".into(),
                name: "read".into(),
                result: json!({"ok":true}),
                images: Vec::new(),
                is_error: false,
            })],
            provider_replay: None,
        });
        request.messages.push(Message {
            role: Role::User,
            content: vec![Content::Text("again".into())],
            provider_replay: None,
        });
        let chat = chat_body(&request, HttpWire::ChatCompletions).unwrap();
        assert_eq!(chat["messages"][2]["tool_calls"][0]["id"], "provider-id");
        assert_eq!(chat["messages"][3]["tool_call_id"], "provider-id");
        let anthropic = anthropic_body(&request, true).unwrap();
        assert_eq!(anthropic["messages"][1]["content"][0]["id"], "provider-id");
        assert_eq!(
            anthropic["messages"][2]["content"][0]["tool_use_id"],
            "provider-id"
        );
        assert_eq!(anthropic["messages"][2]["content"][0]["is_error"], false);
        request.messages[2].content = vec![Content::ToolResult(ToolResult {
            call_id: "provider-id".into(),
            name: "read".into(),
            result: json!({"error":"file missing"}),
            images: Vec::new(),
            is_error: true,
        })];
        let anthropic = anthropic_body(&request, true).unwrap();
        assert_eq!(anthropic["messages"][2]["content"][0]["is_error"], true);
        request.messages[1].content = vec![Content::ToolCall(ToolCall {
            id: "provider-id".into(),
            name: "read".into(),
            arguments: json!({}),
            raw_arguments: Some("{\"path\":".into()),
        })];
        assert_eq!(
            chat_body(&request, HttpWire::ChatCompletions).unwrap()["messages"][2]["tool_calls"][0]
                ["function"]["arguments"],
            "{}"
        );
        request.messages[2].content = vec![Content::ToolResult(ToolResult {
            call_id: "wrong".into(),
            name: "read".into(),
            result: json!(null),
            images: Vec::new(),
            is_error: true,
        })];
        assert!(chat_body(&request, HttpWire::ChatCompletions).is_err());
    }

    #[test]
    fn tool_images_keep_all_tool_replies_adjacent_on_chat_and_stay_inside_anthropic_results() {
        let image = ion_ai::ImageContent::from_bytes(&[
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1,
            8, 6, 0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 248, 207,
            192, 240, 31, 0, 5, 0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66,
            96, 130,
        ])
        .unwrap();
        let mut request = request();
        request.messages.push(Message {
            role: Role::Assistant,
            content: ["first", "second"]
                .into_iter()
                .map(|id| {
                    Content::ToolCall(ToolCall {
                        id: id.into(),
                        name: "read".into(),
                        arguments: json!({"path":"a.png"}),
                        raw_arguments: None,
                    })
                })
                .collect(),
            provider_replay: None,
        });
        for id in ["first", "second"] {
            request.messages.push(Message {
                role: Role::Tool,
                content: vec![Content::ToolResult(ToolResult {
                    call_id: id.into(),
                    name: "read".into(),
                    result: json!({"path":"a.png"}),
                    images: vec![image.clone()],
                    is_error: false,
                })],
                provider_replay: None,
            });
        }
        let chat = chat_body(&request, HttpWire::ChatCompletions).unwrap();
        let messages = chat["messages"].as_array().unwrap();
        assert_eq!(messages[3]["role"], "tool");
        assert_eq!(messages[4]["role"], "tool");
        assert_eq!(messages[5]["role"], "user");
        assert_eq!(messages[5]["content"].as_array().unwrap().len(), 3);
        assert!(
            messages[5]["content"][1]["image_url"]["url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );

        let anthropic = anthropic_body(&request, true).unwrap();
        let results = anthropic["messages"][2]["content"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["content"][1]["type"], "image");
        assert_eq!(
            results[1]["content"][1]["source"]["media_type"],
            "image/png"
        );
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
            anthropic_body(&request, true).unwrap_err().kind,
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
    fn direct_flash_profiles_keep_default_thinking_with_replay() {
        let request = request();
        let deepseek = chat_body(&request, HttpWire::DeepSeekChat).unwrap();
        assert!(deepseek.get("reasoning_effort").is_none());
        assert!(deepseek.get("thinking").is_none());
        assert_eq!(deepseek["max_tokens"], request.controls.max_output_tokens);
        assert!(deepseek.get("max_completion_tokens").is_none());
        let mimo = chat_body(&request, HttpWire::MiMoChat).unwrap();
        assert!(mimo.get("thinking").is_none());
        let openrouter = chat_body(&request, HttpWire::OpenRouterChat).unwrap();
        assert!(openrouter.get("reasoning").is_none());
        let llama_cpp = chat_body(&request, HttpWire::LlamaCppNoThinking).unwrap();
        assert_eq!(llama_cpp["chat_template_kwargs"]["enable_thinking"], false);

        let mut thinking = request;
        thinking.controls.reasoning = Reasoning::High;
        assert_eq!(
            chat_body(&thinking, HttpWire::DeepSeekChat).unwrap()["reasoning_effort"],
            "high"
        );
        assert_eq!(
            chat_body(&thinking, HttpWire::MiMoChat).unwrap()["thinking"]["type"],
            "enabled"
        );
        assert_eq!(
            chat_body(&thinking, HttpWire::OpenRouterChat).unwrap()["reasoning"]["effort"],
            "high"
        );
        assert_eq!(
            chat_body(&thinking, HttpWire::LlamaCppNoThinking)
                .unwrap_err()
                .kind,
            ProviderErrorKind::Unsupported
        );
        thinking.controls.reasoning = Reasoning::Off;
        for wire in [HttpWire::DeepSeekChat, HttpWire::MiMoChat] {
            assert_eq!(
                chat_body(&thinking, wire).unwrap()["thinking"]["type"],
                "disabled"
            );
        }
        assert_eq!(
            chat_body(&thinking, HttpWire::OpenRouterChat).unwrap()["reasoning"]["enabled"],
            false
        );
        let mut state = ChatState::default();
        assert_eq!(state.accept(&json!({"choices":[{"delta":{"reasoning_content":"hidden"},"finish_reason":null}]})).unwrap_err().kind, ProviderErrorKind::Unsupported);
    }

    #[test]
    fn direct_reasoning_survives_streaming_and_later_tool_requests() {
        for wire in [HttpWire::DeepSeekChat, HttpWire::MiMoChat] {
            let mut request = request();
            request.route.effective.provider = match wire {
                HttpWire::DeepSeekChat => "deepseek",
                HttpWire::MiMoChat => "xiaomi",
                _ => unreachable!(),
            }
            .into();
            let mut state = ChatState::new(wire);
            for part in ["first ", "second"] {
                state.accept(&json!({"choices":[{"delta":{"reasoning_content":part},"finish_reason":null}]})).unwrap();
            }
            state.accept(&json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"remote","function":{"name":"read","arguments":"{}"}}]},"finish_reason":"tool_calls"}]})).unwrap();
            let assistant = state.complete(&request).unwrap().message;
            assert_eq!(
                assistant.provider_replay.as_ref().unwrap().data,
                "first second"
            );
            request.messages.push(assistant);
            request.messages.push(Message {
                role: Role::Tool,
                content: vec![Content::ToolResult(ToolResult {
                    call_id: "remote".into(),
                    name: "read".into(),
                    result: json!({"value":42}),
                    images: Vec::new(),
                    is_error: false,
                })],
                provider_replay: None,
            });
            request.messages.push(Message {
                role: Role::User,
                content: vec![Content::Text("continue".into())],
                provider_replay: None,
            });
            let body = chat_body(&request, wire).unwrap();
            assert_eq!(body["messages"][2]["reasoning_content"], "first second");
            assert_eq!(body["messages"][2]["tool_calls"][0]["id"], "remote");
            assert_eq!(body["messages"][3]["tool_call_id"], "remote");
            let mut wrong_provider = request.clone();
            wrong_provider.route.effective.provider = "other".into();
            assert_eq!(
                chat_body(&wrong_provider, wire).unwrap_err().kind,
                ProviderErrorKind::Unsupported
            );
            request.messages[1].provider_replay.as_mut().unwrap().kind = "unknown".into();
            assert_eq!(
                chat_body(&request, wire).unwrap_err().kind,
                ProviderErrorKind::Unsupported
            );
        }
    }

    #[test]
    fn openrouter_replays_fragmented_plain_details() {
        let mut request = request();
        request.route.effective.provider = "openrouter".into();
        let mut state = ChatState::new(HttpWire::OpenRouterChat);
        for text in ["plan ", "read"] {
            state.accept(&json!({"choices":[{"delta":{"reasoning":text,"reasoning_details":[{"type":"reasoning.text","text":text,"format":"unknown","index":0}]},"finish_reason":null}]})).unwrap();
        }
        state.accept(&json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"remote","function":{"name":"read","arguments":"{}"}}]},"finish_reason":"tool_calls"}]})).unwrap();
        let assistant = state.complete(&request).unwrap().message;
        assert_eq!(
            assistant.provider_replay.as_ref().unwrap().data,
            json!([{"type":"reasoning.text","text":"plan read","format":"unknown","index":0}])
        );
        request.messages.push(assistant);
        request.messages.push(Message {
            role: Role::Tool,
            content: vec![Content::ToolResult(ToolResult {
                call_id: "remote".into(),
                name: "read".into(),
                result: json!({"ok":true}),
                images: Vec::new(),
                is_error: false,
            })],
            provider_replay: None,
        });
        let body = chat_body(&request, HttpWire::OpenRouterChat).unwrap();
        assert_eq!(
            body["messages"][2]["reasoning_details"],
            json!([{"type":"reasoning.text","text":"plan read","format":"unknown","index":0}])
        );
        assert!(body["messages"][2].get("reasoning").is_none());
        assert_eq!(body["messages"][2]["tool_calls"][0]["id"], "remote");
        assert_eq!(body["messages"][3]["tool_call_id"], "remote");

        let mut contradictory = ChatState::new(HttpWire::OpenRouterChat);
        contradictory.accept(&json!({"choices":[{"delta":{"reasoning":"one","reasoning_details":[{"type":"reasoning.text","text":"two"}]},"finish_reason":"stop"}]})).unwrap();
        assert_eq!(
            contradictory.complete(&request).unwrap_err().kind,
            ProviderErrorKind::InvalidRequest
        );
        let mut generic = ChatState::default();
        assert_eq!(
            generic
                .accept(&json!({"choices":[{"delta":{"reasoning":"hidden"},"finish_reason":null}]}))
                .unwrap_err()
                .kind,
            ProviderErrorKind::Unsupported
        );
    }

    #[test]
    fn openrouter_replays_plain_reasoning_without_details() {
        let mut request = request();
        request.route.effective.provider = "openrouter".into();
        let mut state = ChatState::new(HttpWire::OpenRouterChat);
        state
            .accept(
                &json!({"choices":[{"delta":{"reasoning":"inspect file"},"finish_reason":null}]}),
            )
            .unwrap();
        state.accept(&json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"remote","function":{"name":"read","arguments":"{}"}}]},"finish_reason":"tool_calls"}]})).unwrap();
        let assistant = state.complete(&request).unwrap().message;
        assert_eq!(
            assistant.provider_replay.as_ref().unwrap().kind,
            OPENROUTER_PLAIN_REASONING_REPLAY
        );
        request.messages.push(assistant);
        request.messages.push(Message {
            role: Role::Tool,
            content: vec![Content::ToolResult(ToolResult {
                call_id: "remote".into(),
                name: "read".into(),
                result: json!({"ok":true}),
                images: Vec::new(),
                is_error: false,
            })],
            provider_replay: None,
        });
        let body = chat_body(&request, HttpWire::OpenRouterChat).unwrap();
        assert_eq!(body["messages"][2]["reasoning"], "inspect file");
        assert!(body["messages"][2].get("reasoning_details").is_none());
    }

    #[test]
    fn openrouter_replays_signed_and_encrypted_details_in_order() {
        let mut request = request();
        request.route.effective.provider = "openrouter".into();
        let mut state = ChatState::new(HttpWire::OpenRouterChat);
        state.accept(&json!({"choices":[{"delta":{"reasoning_details":[{"type":"reasoning.text","text":"Need ","index":0}]},"finish_reason":null}]})).unwrap();
        state.accept(&json!({"choices":[{"delta":{"reasoning_details":[{"type":"reasoning.text","text":"read","index":0,"signature":"signed","format":"google-gemini-v1"},{"type":"reasoning.summary","summary":"First ","index":1}]},"finish_reason":null}]})).unwrap();
        state.accept(&json!({"choices":[{"delta":{"reasoning_details":[{"type":"reasoning.summary","summary":"inspect","index":1},{"type":"reasoning.encrypted","data":"opaque","format":"google-gemini-v1","id":"thought-1","index":2}]},"finish_reason":null}]})).unwrap();
        state.accept(&json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"provider-call","function":{"name":"read","arguments":"{}"}}]},"finish_reason":"tool_calls"}]})).unwrap();
        let assistant = state.complete(&request).unwrap().message;
        let details = json!([
            {"type":"reasoning.text","text":"Need read","index":0,"signature":"signed","format":"google-gemini-v1"},
            {"type":"reasoning.summary","summary":"First inspect","index":1},
            {"type":"reasoning.encrypted","data":"opaque","format":"google-gemini-v1","id":"thought-1","index":2}
        ]);
        assert_eq!(assistant.provider_replay.as_ref().unwrap().data, details);
        request.messages.push(assistant);
        request.messages.push(Message {
            role: Role::Tool,
            content: vec![Content::ToolResult(ToolResult {
                call_id: "provider-call".into(),
                name: "read".into(),
                result: json!({"ok":true}),
                images: Vec::new(),
                is_error: false,
            })],
            provider_replay: None,
        });
        let body = chat_body(&request, HttpWire::OpenRouterChat).unwrap();
        assert_eq!(body["messages"][2]["reasoning_details"], details);
        assert!(body["messages"][2].get("reasoning").is_none());
        assert_eq!(body["messages"][2]["tool_calls"][0]["id"], "provider-call");
        assert_eq!(body["messages"][3]["tool_call_id"], "provider-call");

        let mut wrong_provider = request.clone();
        wrong_provider.route.effective.provider = "other".into();
        assert_eq!(
            chat_body(&wrong_provider, HttpWire::OpenRouterChat)
                .unwrap_err()
                .kind,
            ProviderErrorKind::Unsupported
        );
        request.messages[1].provider_replay.as_mut().unwrap().data =
            json!([{"type":"reasoning.encrypted"}]);
        assert_eq!(
            chat_body(&request, HttpWire::OpenRouterChat)
                .unwrap_err()
                .kind,
            ProviderErrorKind::InvalidRequest
        );
    }

    #[test]
    fn openrouter_keeps_signature_only_text_detail() {
        let mut request = request();
        request.route.effective.provider = "openrouter".into();
        let mut state = ChatState::new(HttpWire::OpenRouterChat);
        let detail = json!({"type":"reasoning.text","format":"google-gemini-v1","index":0,"signature":"opaque"});
        state
            .accept(&json!({"choices":[{"delta":{"reasoning_details":[detail],"content":"Done"},"finish_reason":"stop"}]}))
            .unwrap();
        let assistant = state.complete(&request).unwrap().message;
        assert_eq!(
            assistant.provider_replay.as_ref().unwrap().data,
            json!([detail])
        );
        request.messages.push(assistant);
        request
            .messages
            .push(Message::user_input("Continue".into(), []));
        let body = chat_body(&request, HttpWire::OpenRouterChat).unwrap();
        assert_eq!(body["messages"][2]["reasoning_details"], json!([detail]));

        let mut invalid = ChatState::new(HttpWire::OpenRouterChat);
        assert_eq!(
            invalid
                .accept(&json!({"choices":[{"delta":{"reasoning_details":[{"type":"reasoning.text","index":0}]},"finish_reason":null}]}))
                .unwrap_err()
                .kind,
            ProviderErrorKind::InvalidRequest
        );
        assert!(!valid_openrouter_detail(&json!({
            "type":"reasoning.text","text":42,"signature":"opaque"
        })));
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

        let mut state = ChatState::default();
        state.accept(&json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"invented","function":{"name":"not_a_tool","arguments":"{}"}}]},"finish_reason":"tool_calls"}]})).unwrap();
        assert_eq!(
            state.complete(&request).unwrap().message.content,
            vec![Content::ToolCall(ToolCall {
                id: "invented".into(),
                name: "not_a_tool".into(),
                arguments: json!({}),
                raw_arguments: None,
            })]
        );

        let mut state = ChatState::default();
        state.accept(&json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"broken","function":{"name":"exec","arguments":"{\"command\":"}}]},"finish_reason":"tool_calls"}]})).unwrap();
        let response = state.complete(&request).unwrap();
        assert_eq!(
            response.message.content,
            vec![Content::ToolCall(ToolCall {
                id: "broken".into(),
                name: "exec".into(),
                arguments: json!({}),
                raw_arguments: Some("{\"command\":".into()),
            })]
        );
    }

    #[test]
    fn chat_tool_call_keeps_first_metadata_while_arguments_stream() {
        let request = request();
        let mut state = ChatState::default();
        state
            .accept(&json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call-first","function":{"name":"read","arguments":"{\"path\":\""}}]},"finish_reason":null}]}))
            .unwrap();
        state
            .accept(&json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call-later","function":{"name":"read","arguments":"README.md\"}"}}]},"finish_reason":"tool_calls"}]}))
            .unwrap();
        assert_eq!(
            state.complete(&request).unwrap().message.content,
            vec![Content::ToolCall(ToolCall {
                id: "call-first".into(),
                name: "read".into(),
                arguments: json!({"path":"README.md"}),
                raw_arguments: None,
            })]
        );

        let mut duplicate = ChatState::default();
        duplicate
            .accept(&json!({"choices":[{"index":0,"delta":{"tool_calls":[
            {"index":0,"id":"same","function":{"name":"read","arguments":"{}"}},
            {"index":1,"id":"same","function":{"name":"read","arguments":"{}"}}
        ]},"finish_reason":"tool_calls"}]}))
            .unwrap();
        assert!(duplicate.complete(&request).is_err());
    }

    #[test]
    fn chat_length_stop_retains_calls_for_safe_rejection() {
        let request = request();
        let mut state = ChatState::default();
        state
            .accept(&json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"partial","function":{"name":"write","arguments":"{\"path\":\"short\",\"content\":\"hel\"}"}}]},"finish_reason":"length"}]}))
            .unwrap();
        let response = state.complete(&request).unwrap();
        assert_eq!(
            response.termination,
            ResponseTermination::Incomplete(IncompleteReason::MaxOutputTokens)
        );
        assert!(matches!(
            &response.message.content[0],
            Content::ToolCall(call) if call.id == "partial" && call.name == "write"
        ));
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
            .accept(
                &json!({"choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop"}]}),
            )
            .unwrap();
        state
            .accept(&json!({"usage":{"prompt_tokens":2,"completion_tokens":1}}))
            .unwrap();
        assert_eq!(state.complete(&request).unwrap().usage, Usage::known(2, 1));

        let mut state = ChatState::default();
        state
            .accept(&json!({"choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop","usage":{"prompt_tokens":7,"completion_tokens":2}}]}))
            .unwrap();
        assert_eq!(state.complete(&request).unwrap().usage, Usage::known(7, 2));

        let mut state = ChatState::default();
        state
            .accept(&json!({"usage":{"prompt_tokens":8,"completion_tokens":3},"choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop","usage":{"prompt_tokens":7,"completion_tokens":2}}]}))
            .unwrap();
        assert_eq!(state.complete(&request).unwrap().usage, Usage::known(8, 3));

        let mut state = ChatState::default();
        state
            .accept(&json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}))
            .unwrap();
        assert!(state.accept(&json!({"choices":[{"index":0,"delta":{"content":"late"},"finish_reason":"stop"}]})).is_err());
    }

    #[test]
    fn chat_partial_usage_retains_previous_counts() {
        let request = request();
        let mut state = ChatState::default();
        state
            .accept(&json!({"usage":{"prompt_tokens":12,"completion_tokens":1}}))
            .unwrap();
        state
            .accept(&json!({"choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop","usage":{"completion_tokens":3}}]}))
            .unwrap();
        assert_eq!(state.complete(&request).unwrap().usage, Usage::known(12, 3));

        let mut state = ChatState::default();
        state
            .accept(&json!({"usage":{"prompt_tokens":12}}))
            .unwrap();
        state
            .accept(
                &json!({"choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop"}]}),
            )
            .unwrap();
        assert_eq!(
            state.complete(&request).unwrap().usage,
            Usage {
                input_tokens: Some(12),
                output_tokens: None,
                ..Usage::unknown()
            }
        );

        assert!(
            ChatState::default()
                .accept(&json!({"usage":{"prompt_tokens":"wrong"}}))
                .is_err()
        );
    }

    #[test]
    fn sse_parsing_preserves_data_and_rejects_bad_utf8() {
        assert!(!has_sse_event_boundary(b"data: x\r\n"));
        for terminator in [
            b"\n\n".as_slice(),
            b"\r\n\r\n",
            b"\r\r",
            b"\r\n\n",
            b"\n\r\n",
            b"\r\r\n",
        ] {
            let mut frame = b"data: x".to_vec();
            frame.extend_from_slice(terminator);
            assert!(has_sse_event_boundary(&frame), "{terminator:?}");
        }
        let frame = parse_frame(b": ping\r\ndata: {\"a\":\r\ndata: 1}\r\n\r\n")
            .unwrap()
            .unwrap();
        assert_eq!(frame.data, "{\"a\":\n1}");
        let frame = parse_frame(b": ping\rdata: {\"a\":\rdata: 1}\r\r")
            .unwrap()
            .unwrap();
        assert_eq!(frame.data, "{\"a\":\n1}");
        assert!(parse_frame(b"data: \xff\n\n").is_err());
    }

    #[tokio::test]
    async fn mimo_capacity_is_normalized_for_aliases_on_every_completion_path() {
        let frame = "data: {\"model\":\"served-model\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"length\"}],\"usage\":{\"prompt_tokens\":49900,\"completion_tokens\":0}}";
        for suffix in ["\n\ndata: [DONE]\n\n", "\n\ndata: [DONE]", ""] {
            let endpoint = serve(format!("{frame}{suffix}"), "200 OK").await;
            let model = HttpModelService::new_with_capabilities(
                &endpoint,
                HttpWire::MiMoChat,
                Arc::new(|| None),
                ModelCapabilities::conservative(),
                Some(50_000),
            )
            .unwrap();
            let mut request = request();
            request.route = ModelRoute::direct(
                ModelRef {
                    provider: "renamed-mimo".into(),
                    model: "requested-model".into(),
                },
                ModelRouteReason::UserRequest,
            );
            let mut stream = model.stream(request).await.unwrap();
            let mut completed = None;
            while let Some(event) = stream.next().await {
                if let ModelStreamEvent::Completed(response) = event.unwrap() {
                    assert!(completed.is_none());
                    completed = Some(response);
                }
            }
            let response = completed.unwrap();
            assert_eq!(
                response.termination,
                ResponseTermination::Incomplete(IncompleteReason::ContextLength),
                "{suffix:?}"
            );
            assert_eq!(response.usage, Usage::known(49_900, 0));
            assert_eq!(response.returned_model.as_deref(), Some("served-model"));
        }
    }

    #[test]
    fn capacity_normalization_requires_the_mimo_wire_and_known_window() {
        for (wire, window, input, output, expected) in [
            (
                HttpWire::MiMoChat,
                Some(50_000),
                49_499,
                0,
                IncompleteReason::MaxOutputTokens,
            ),
            (
                HttpWire::MiMoChat,
                Some(50_000),
                49_500,
                0,
                IncompleteReason::ContextLength,
            ),
            (
                HttpWire::MiMoChat,
                Some(50_000),
                u64::MAX,
                0,
                IncompleteReason::ContextLength,
            ),
            (
                HttpWire::MiMoChat,
                Some(50_000),
                49_900,
                1,
                IncompleteReason::MaxOutputTokens,
            ),
            (
                HttpWire::MiMoChat,
                None,
                49_900,
                0,
                IncompleteReason::MaxOutputTokens,
            ),
            (
                HttpWire::ChatCompletions,
                Some(50_000),
                49_900,
                0,
                IncompleteReason::MaxOutputTokens,
            ),
        ] {
            let event = ModelStreamEvent::Completed(ModelResponse {
                message: Message {
                    role: Role::Assistant,
                    content: Vec::new(),
                    provider_replay: None,
                },
                usage: Usage::known(input, output),
                termination: ResponseTermination::Incomplete(IncompleteReason::MaxOutputTokens),
                returned_model: None,
            });
            let ModelStreamEvent::Completed(response) = normalize_capacity(event, wire, window)
            else {
                unreachable!();
            };
            assert_eq!(
                response.termination,
                ResponseTermination::Incomplete(expected)
            );
        }
    }

    #[tokio::test]
    async fn chat_stream_accepts_cr_and_mixed_sse_line_endings() {
        let body = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\r\r",
            "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\r\n\n",
            "data: [DONE]\n\r\n"
        );
        let endpoint = serve_split(body.into(), "200 OK", 1).await;
        let model =
            HttpModelService::new(&endpoint, HttpWire::ChatCompletions, Arc::new(|| None)).unwrap();
        let mut stream = model.stream(request()).await.unwrap();
        assert!(matches!(
            stream.next().await.unwrap().unwrap(),
            ModelStreamEvent::TextDelta(text) if text == "hi"
        ));
        assert!(matches!(
            stream.next().await.unwrap().unwrap(),
            ModelStreamEvent::Completed(_)
        ));
    }

    #[tokio::test]
    async fn chat_stream_accepts_eof_after_finish_and_reports_usage() {
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
        let mut completed = false;
        while let Some(event) = stream.next().await {
            if matches!(event.unwrap(), ModelStreamEvent::Completed(_)) {
                completed = true;
            }
        }
        assert!(completed);

        let endpoint = serve("data: {\"choices\":[{\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n".into(), "200 OK").await;
        let model =
            HttpModelService::new(&endpoint, HttpWire::ChatCompletions, Arc::new(|| None)).unwrap();
        let mut stream = model.stream(request()).await.unwrap();
        assert!(matches!(
            stream.next().await.unwrap().unwrap(),
            ModelStreamEvent::TextDelta(_)
        ));
        assert_eq!(
            stream.next().await.unwrap().unwrap_err().kind,
            ProviderErrorKind::Transport
        );

        let endpoint = serve(
            "data: {\"choices\":[{\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}"
                .into(),
            "200 OK",
        )
        .await;
        let model =
            HttpModelService::new(&endpoint, HttpWire::ChatCompletions, Arc::new(|| None)).unwrap();
        let mut stream = model.stream(request()).await.unwrap();
        assert!(matches!(
            stream.next().await.unwrap().unwrap(),
            ModelStreamEvent::TextDelta(_)
        ));
        assert!(matches!(
            stream.next().await.unwrap().unwrap(),
            ModelStreamEvent::Completed(_)
        ));
    }

    #[tokio::test]
    async fn chat_stream_error_preserves_bounded_reason_and_never_completes() {
        let body = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"}}]}\n\n",
            "data: {\"error\":{\"code\":\"server_error\",\"message\":\"upstream disconnected\\nwhile generating\"},\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"error\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let endpoint = serve(body.into(), "200 OK").await;
        let model =
            HttpModelService::new(&endpoint, HttpWire::OpenRouterChat, Arc::new(|| None)).unwrap();
        let mut stream = model.stream(request()).await.unwrap();
        assert!(matches!(
            stream.next().await.unwrap().unwrap(),
            ModelStreamEvent::TextDelta(_)
        ));
        let error = stream.next().await.unwrap().unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Server);
        assert!(
            error
                .message
                .contains("upstream disconnectedwhile generating")
        );
        assert!(!error.message.contains('\n'));

        let rate_limit = json!({"error":{"code":429,"message":"slow down"},"choices":[{"finish_reason":"error"}]});
        let mut state = ChatState::default();
        let error = state.accept(&rate_limit).unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::RateLimited);
        assert!(error.message.contains("slow down"));

        let long = json!({"error":{"message":"x".repeat(1000)}});
        let error = state.accept(&long).unwrap_err();
        assert_eq!(error.message.len(), "provider stream error: ".len() + 500);
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
                arguments: json!({"path":"x"}),
                raw_arguments: None,
            })]
        );

        let endpoint = serve_split(body.replace('\n', "\r"), "200 OK", 1).await;
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
        assert_eq!(completed.unwrap().message.content, response.message.content);
    }

    #[test]
    fn anthropic_signed_blocks_survive_tool_continuation_and_reopen() {
        let mut request = request();
        let mut state = AnthropicState::default();
        let events = [
            json!({"type":"message_start","message":{"type":"message","role":"assistant",
                "model":"returned","content":[],"stop_reason":null,
                "usage":{"input_tokens":3,"output_tokens":0}}}),
            json!({"type":"content_block_start","index":0,
                "content_block":{"type":"thinking","thinking":"","signature":""}}),
            json!({"type":"content_block_delta","index":0,
                "delta":{"type":"thinking_delta","thinking":""}}),
            json!({"type":"content_block_delta","index":0,
                "delta":{"type":"signature_delta","signature":"sig"}}),
            json!({"type":"content_block_delta","index":0,
                "delta":{"type":"signature_delta","signature":"nature"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"content_block_start","index":1,
                "content_block":{"type":"redacted_thinking","data":"opaque"}}),
            json!({"type":"content_block_stop","index":1}),
            json!({"type":"content_block_start","index":2,
                "content_block":{"type":"text","text":""}}),
            json!({"type":"content_block_stop","index":2}),
            json!({"type":"content_block_start","index":3,
                "content_block":{"type":"tool_use","id":"tool_1","name":"read","input":{}}}),
            json!({"type":"content_block_delta","index":3,
                "delta":{"type":"input_json_delta","partial_json":"{\"path\":\"x\"}"}}),
            json!({"type":"content_block_stop","index":3}),
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},
                "usage":{"output_tokens":7}}),
            json!({"type":"message_stop"}),
        ];
        let mut completed = None;
        for event in events {
            for output in state.accept(&event, &request).unwrap() {
                if let ModelStreamEvent::Completed(response) = output {
                    completed = Some(response);
                }
            }
        }
        let assistant = completed.unwrap().message;
        assert_eq!(assistant.content.len(), 2);
        assert!(matches!(&assistant.content[0], Content::Text(text) if text.is_empty()));
        let persisted: Message =
            serde_json::from_slice(&serde_json::to_vec(&assistant).unwrap()).unwrap();
        assert!(persisted.provider_replay.as_ref().unwrap().prefix_bound);
        request.messages.push(persisted.clone());
        request.messages.push(Message {
            role: Role::Tool,
            content: vec![Content::ToolResult(ion_ai::ToolResult {
                call_id: "tool_1".into(),
                name: "read".into(),
                result: json!({"text":"ok"}),
                images: Vec::new(),
                is_error: false,
            })],
            provider_replay: None,
        });
        let continuation = anthropic_body(&request, true).unwrap();
        let blocks = continuation["messages"][1]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 4);
        assert_eq!(
            blocks[0],
            json!({"type":"thinking","thinking":"","signature":"signature"})
        );
        assert_eq!(
            blocks[1],
            json!({"type":"redacted_thinking","data":"opaque"})
        );
        assert_eq!(blocks[2], json!({"type":"text","text":""}));
        assert_eq!(blocks[3]["id"], "tool_1");
        assert_eq!(
            continuation["messages"][2]["content"][0]["tool_use_id"],
            "tool_1"
        );

        request
            .messages
            .push(Message::user_input("next".into(), []));
        assert_eq!(
            anthropic_body(&request, true).unwrap()["messages"][1]["content"],
            Value::Array(blocks.clone())
        );
        let mut changed_prefix = request.clone();
        changed_prefix.instructions = Some("new instructions".into());
        assert_eq!(
            anthropic_body(&changed_prefix, true).unwrap_err().kind,
            ProviderErrorKind::ReplayContextChanged
        );
        changed_prefix = request.clone();
        changed_prefix.messages[0].content[0] = Content::Text("rewritten".into());
        assert_eq!(
            anthropic_body(&changed_prefix, true).unwrap_err().kind,
            ProviderErrorKind::ReplayContextChanged
        );
        let mut changed = request.clone();
        changed.messages[1].content[0] = Content::Text("edited".into());
        assert_eq!(
            anthropic_body(&changed, true).unwrap_err().kind,
            ProviderErrorKind::InvalidRequest
        );
        let mut corrupt = request;
        corrupt.messages[1].provider_replay.as_mut().unwrap().data["blocks"][0]["signature"] =
            json!("");
        assert_eq!(
            anthropic_body(&corrupt, true).unwrap_err().kind,
            ProviderErrorKind::InvalidRequest
        );
    }

    #[test]
    fn anthropic_unsigned_thinking_is_rejected_before_completion() {
        let mut state = AnthropicState::default();
        let request = request();
        for event in [
            json!({"type":"message_start","message":{"type":"message","role":"assistant",
                "model":"returned","content":[],"stop_reason":null,
                "usage":{"input_tokens":1,"output_tokens":0}}}),
            json!({"type":"content_block_start","index":0,
                "content_block":{"type":"thinking","thinking":"","signature":""}}),
        ] {
            state.accept(&event, &request).unwrap();
        }
        assert_eq!(
            state
                .accept(&json!({"type":"content_block_stop","index":0}), &request)
                .unwrap_err()
                .kind,
            ProviderErrorKind::InvalidRequest
        );
    }

    #[test]
    fn signed_anthropic_tool_with_invalid_json_does_not_enter_history() {
        let mut state = AnthropicState::default();
        let request = request();
        for event in [
            json!({"type":"message_start","message":{"type":"message","role":"assistant",
                "model":"returned","content":[],"stop_reason":null,
                "usage":{"input_tokens":1,"output_tokens":0}}}),
            json!({"type":"content_block_start","index":0,
                "content_block":{"type":"thinking","thinking":"","signature":""}}),
            json!({"type":"content_block_delta","index":0,
                "delta":{"type":"signature_delta","signature":"signed"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"content_block_start","index":1,
                "content_block":{"type":"tool_use","id":"tool_1","name":"read","input":{}}}),
            json!({"type":"content_block_delta","index":1,
                "delta":{"type":"input_json_delta","partial_json":"{"}}),
            json!({"type":"content_block_stop","index":1}),
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},
                "usage":{"output_tokens":3}}),
        ] {
            state.accept(&event, &request).unwrap();
        }
        assert_eq!(
            state
                .accept(&json!({"type":"message_stop"}), &request)
                .unwrap_err()
                .kind,
            ProviderErrorKind::InvalidRequest
        );
    }

    #[test]
    fn current_claude_enforces_prefix_binding_only_on_native_api() {
        let mut request = request();
        request.route.effective.provider = "anthropic".into();
        request.route.effective.model = "claude-sonnet-5-5".into();
        assert_eq!(
            anthropic_body(&request, true).unwrap()["thinking"],
            json!({"type":"adaptive","block_binding":{"prefix_mismatch_behavior":"error"}})
        );
        assert!(
            anthropic_body(&request, false)
                .unwrap()
                .get("thinking")
                .is_none()
        );
        request.controls.tool_choice = ToolChoice::Required;
        assert_eq!(
            anthropic_body(&request, true).unwrap_err().kind,
            ProviderErrorKind::Unsupported
        );
        request.controls.tool_choice = ToolChoice::Auto;
        request.route.effective.model = "claude-sonnet-4-6".into();
        assert!(
            anthropic_body(&request, true)
                .unwrap()
                .get("thinking")
                .is_none()
        );
    }

    #[test]
    fn anthropic_reports_known_replay_transformations_without_unknown_entries() {
        let notices = anthropic_replay_notices(Some(&json!([
            {"type":"thinking_dropped","reason":"model_binding_mismatch","path":"messages.1.content.0"},
            {"type":"thinking_dropped","reason":"model_binding_mismatch","path":"messages.3.content.0"},
            {"type":"thinking_dropped","reason":"organization_binding_mismatch","path":"messages.5.content.0"},
            {"type":"future_type","reason":"future_reason"}
        ])));
        assert_eq!(notices.len(), 2);
        assert!(notices.iter().any(|notice| matches!(notice,
            ModelStreamEvent::ProviderReplayNotice { action, reason, count: 2 }
            if action == "thinking_dropped" && reason == "model_binding_mismatch"
        )));
        assert!(notices.iter().any(|notice| matches!(notice,
            ModelStreamEvent::ProviderReplayNotice { action, reason, count: 1 }
            if action == "thinking_dropped" && reason == "organization_binding_mismatch"
        )));
    }

    #[tokio::test]
    async fn anthropic_stream_accepts_multiple_message_deltas() {
        let body = concat!(
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"type\":\"message\",\"role\":\"assistant\",\"model\":\"returned\",\"content\":[],\"stop_reason\":null,\"usage\":{\"input_tokens\":3,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"ok\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":null},\"usage\":{\"output_tokens\":1}}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        );
        let endpoint = serve_split(body.into(), "200 OK", 1).await;
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
        assert_eq!(response.usage, Usage::known(3, 2));
        assert_eq!(response.message.content, vec![Content::Text("ok".into())]);
    }

    #[test]
    fn anthropic_partial_usage_delta_keeps_previous_counts() {
        let request = request();
        let mut state = AnthropicState::default();
        state
            .accept(&json!({"type":"message_start","message":{"type":"message","role":"assistant","model":"returned","content":[],"stop_reason":null,"usage":{"input_tokens":3,"output_tokens":0}}}), &request)
            .unwrap();
        state
            .accept(&json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":"ok"}}), &request)
            .unwrap();
        state
            .accept(&json!({"type":"content_block_stop","index":0}), &request)
            .unwrap();
        state
            .accept(&json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}), &request)
            .unwrap();
        assert_eq!(state.usage().unwrap(), Usage::known(3, 2));
        let completed = state
            .accept(&json!({"type":"message_stop"}), &request)
            .unwrap();
        assert!(
            matches!(&completed[0], ModelStreamEvent::Completed(response) if response.usage == Usage::known(3, 2))
        );

        let mut state = AnthropicState::default();
        state
            .accept(&json!({"type":"message_start","message":{"type":"message","role":"assistant","model":"returned","content":[],"stop_reason":null,"usage":{"input_tokens":3,"output_tokens":0}}}), &request)
            .unwrap();
        state
            .accept(
                &json!({"type":"message_delta","delta":{"stop_reason":"end_turn"}}),
                &request,
            )
            .unwrap();
        assert_eq!(state.usage().unwrap(), Usage::known(3, 0));
    }

    #[test]
    fn anthropic_multiple_message_deltas_need_consistent_stop_reason() {
        let request = request();
        let mut state = AnthropicState::default();
        state
            .accept(&json!({"type":"message_start","message":{"type":"message","role":"assistant","model":"returned","content":[],"stop_reason":null,"usage":{"input_tokens":3,"output_tokens":0}}}), &request)
            .unwrap();
        state
            .accept(
                &json!({"type":"message_delta","delta":{},"usage":{"output_tokens":1}}),
                &request,
            )
            .unwrap();
        assert!(
            state
                .accept(&json!({"type":"message_stop"}), &request)
                .is_err()
        );
        state
            .accept(
                &json!({"type":"message_delta","delta":{"stop_reason":"end_turn"}}),
                &request,
            )
            .unwrap();
        assert!(
            state
                .accept(
                    &json!({"type":"message_delta","delta":{"stop_reason":"tool_use"}}),
                    &request
                )
                .is_err()
        );
    }
}
