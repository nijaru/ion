//! Bounded Anthropic Messages streaming boundary (text and client tools only).
//!
//! Protocol references, checked 2026-09-25:
//! <https://platform.claude.com/docs/en/build-with-claude/streaming> and
//! <https://platform.claude.com/docs/en/api/messages>.
//! Thinking/opaque replay, server tools and explicit sampling/reasoning controls
//! are unsupported rather than silently discarded. No beta headers are sent.
//! The stream is capped at 8 MiB total and 256 KiB per SSE frame, including framing.
//! Tool names are restricted to 1–64 ASCII alphanumeric/underscore/hyphen bytes;
//! assistant prefill and empty messages are unsupported.
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use futures_util::stream;
use ion_ai::{ProviderError, ProviderErrorKind, Reasoning, ToolChoice, Usage};
use reqwest::{Url, header::HeaderValue};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::{
    ApiKeySource, AttemptId, ContentDigest, EgressRealm, ModelBoundary, ModelBoundaryIdentity,
    ModelStart, SemanticRequest, TranscriptContent, TranscriptRole,
};

mod streaming;

/// Exact-origin Messages adapter. Supply the full `/v1/messages` URL
/// and a matching frozen remote egress realm. Redirects, proxies and retries are
/// disabled. The host owns attempt timeouts and live egress admission.
///
/// This boundary has no durable start receipts or idempotency guarantee. The
/// effect key is neither sent nor included in the wire fingerprint.
pub struct AnthropicMessages {
    identity: ModelBoundaryIdentity,
    endpoint: Url,
    client: reqwest::Client,
    key: Arc<dyn ApiKeySource>,
}

impl AnthropicMessages {
    pub fn new(
        identity: ModelBoundaryIdentity,
        endpoint: &str,
        key: Arc<dyn ApiKeySource>,
    ) -> Result<Self, String> {
        let endpoint = Url::parse(endpoint).map_err(|_| "invalid endpoint URL")?;
        let local = endpoint
            .host_str()
            .is_some_and(|h| matches!(h, "127.0.0.1" | "[::1]"));
        if !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || endpoint.path() != "/v1/messages"
            || !(endpoint.scheme() == "https" || (local && endpoint.scheme() == "http"))
        {
            return Err(
                "endpoint must be HTTPS or literal-loopback HTTP /v1/messages without userinfo/query/fragment".into(),
            );
        }
        if identity.egress != EgressRealm::Remote(endpoint.origin().ascii_serialization()) {
            return Err("endpoint origin does not match frozen remote egress realm".into());
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .build()
            .map_err(|_| "could not construct HTTP client")?;
        Ok(Self {
            identity,
            endpoint,
            client,
            key,
        })
    }

    fn payload(request: &SemanticRequest) -> Result<Value, ProviderError> {
        request.controls.validate()?;
        if request.controls.reasoning != Reasoning::ProviderDefault
            || request.controls.temperature.is_some()
            || request.controls.top_p.is_some()
        {
            return Err(unsupported(
                "explicit reasoning and sampling controls are unsupported",
            ));
        }
        if request.model.model.is_empty() || request.messages.is_empty() {
            return Err(invalid("Messages requires a model and nonempty history"));
        }
        let mut messages: Vec<Value> = Vec::new();
        let mut seen = BTreeSet::new();
        let mut pending = BTreeMap::new();
        for message in &request.messages {
            if message.provider_replay.is_some() {
                return Err(unsupported(
                    "opaque provider replay is unsupported by Messages",
                ));
            }
            if message.role != TranscriptRole::Tool && !pending.is_empty() {
                return Err(invalid("unanswered logical tool calls"));
            }
            if message.content.is_empty() {
                return Err(invalid("empty transcript message"));
            }
            let mut blocks = Vec::new();
            for content in &message.content {
                match content {
                    TranscriptContent::Text(text) if message.role != TranscriptRole::Tool => {
                        // Empty streamed text carries no semantics, but Messages
                        // rejects empty text blocks on replay. Retain every nonempty
                        // block in order, including text between tool calls.
                        if text.is_empty() {
                            continue;
                        }
                        blocks.push(json!({"type":"text","text":text}));
                    }
                    TranscriptContent::ToolCall {
                        invocation,
                        name,
                        arguments,
                        ..
                    } if message.role == TranscriptRole::Assistant => {
                        if !seen.insert(*invocation) {
                            return Err(invalid("duplicate logical tool invocation"));
                        }
                        if !valid_name(name) || !arguments.is_object() {
                            return Err(invalid("invalid logical tool call"));
                        }
                        let id = format!("ion_{invocation}");
                        pending.insert(*invocation, (id.clone(), name));
                        blocks
                            .push(json!({"type":"tool_use","id":id,"name":name,"input":arguments}));
                    }
                    TranscriptContent::ToolResult {
                        invocation,
                        name,
                        result,
                    } if message.role == TranscriptRole::Tool => {
                        let Some((id, call_name)) = pending.remove(invocation) else {
                            return Err(invalid("orphan or duplicate logical tool result"));
                        };
                        if name != call_name {
                            return Err(invalid("tool result name does not match call"));
                        }
                        blocks.push(json!({"type":"tool_result","tool_use_id":id,"content":result.to_string()}));
                    }
                    _ => return Err(invalid("content does not match transcript role")),
                }
            }
            if blocks.is_empty() {
                return Err(invalid("empty transcript message"));
            }
            let role = if message.role == TranscriptRole::Assistant {
                "assistant"
            } else {
                "user"
            };
            // Consecutive native tool results must be in one user message. Coalescing
            // all adjacent equal roles also mirrors Messages' documented semantics.
            if let Some(previous) = messages.last_mut().filter(|m| m["role"] == role) {
                previous["content"]
                    .as_array_mut()
                    .expect("constructed content array")
                    .extend(blocks);
            } else {
                messages.push(json!({"role":role,"content":blocks}));
            }
        }
        if !pending.is_empty() {
            return Err(invalid("unanswered logical tool calls"));
        }
        if messages[0]["role"] != "user" || messages.last().is_some_and(|m| m["role"] != "user") {
            return Err(unsupported(
                "assistant prefill or history without an initial user is unsupported",
            ));
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
        let mut body = json!({"model":request.model.model,"messages":messages,
            "max_tokens":request.controls.max_output_tokens,"stream":true});
        if !request.instructions.is_empty() {
            body["system"] = json!(request.instructions);
        }
        if !request.tools.is_empty() {
            body["tools"] = Value::Array(request.tools.iter().map(|tool| json!({
                "name":tool.name,"description":tool.description,"input_schema":tool.input_schema
            })).collect());
            let mut choice = match &request.controls.tool_choice {
                ToolChoice::None => json!({"type":"none"}),
                ToolChoice::Auto => json!({"type":"auto"}),
                ToolChoice::Required => json!({"type":"any"}),
                ToolChoice::Named(name) => {
                    if !names.contains(name) {
                        return Err(invalid("named tool is not in the loadout"));
                    }
                    json!({"type":"tool","name":name})
                }
            };
            if request.controls.tool_choice != ToolChoice::None {
                choice["disable_parallel_tool_use"] = json!(!request.controls.parallel_tool_calls);
            }
            body["tool_choice"] = choice;
        } else if matches!(
            request.controls.tool_choice,
            ToolChoice::Required | ToolChoice::Named(_)
        ) {
            return Err(invalid("required tool choice has no tools"));
        }
        Ok(body)
    }
}

impl ModelBoundary for AnthropicMessages {
    fn identity(&self) -> ModelBoundaryIdentity {
        self.identity.clone()
    }

    fn fingerprint(
        &self,
        request: &SemanticRequest,
        _effect_key: &str,
    ) -> Result<ContentDigest, ProviderError> {
        let envelope = json!({"method":"POST","url":self.endpoint.as_str(),
            "headers":{"content-type":"application/json","accept":"text/event-stream","anthropic-version":"2023-06-01"},
            "body":Self::payload(request)?});
        serde_json::to_vec(&envelope)
            .map(|bytes| ContentDigest::of_bytes(&bytes))
            .map_err(|_| invalid("could not encode Messages request"))
    }

    fn start<'a>(
        &'a self,
        _attempt: AttemptId,
        _effect_key: String,
        request: SemanticRequest,
        stop: CancellationToken,
    ) -> ion_ai::BoxFuture<'a, ModelStart> {
        Box::pin(async move {
            let body = match Self::payload(&request) {
                Ok(body) => body,
                Err(error) => {
                    return ModelStart::NotStarted {
                        reason: error.to_string(),
                    };
                }
            };
            if stop.is_cancelled() {
                return ModelStart::NotStarted {
                    reason: "cancelled before HTTP dispatch".into(),
                };
            }
            let key = self.key.api_key();
            if key.is_none() && self.endpoint.scheme() != "http" {
                return ModelStart::NotStarted {
                    reason: "provider credential unavailable".into(),
                };
            }
            let mut request = self
                .client
                .post(self.endpoint.clone())
                .header("anthropic-version", "2023-06-01")
                .header("accept", "text/event-stream")
                .json(&body);
            if let Some(key) = key {
                let mut key = match HeaderValue::from_str(&key) {
                    Ok(key) if !key.is_empty() => key,
                    _ => {
                        return ModelStart::NotStarted {
                            reason: "invalid provider credential".into(),
                        };
                    }
                };
                key.set_sensitive(true);
                request = request.header("x-api-key", key);
            }
            let sent = request.send();
            if stop.is_cancelled() {
                return ModelStart::NotStarted {
                    reason: "cancelled before HTTP dispatch".into(),
                };
            }
            // No negative receipt is possible once send is polled. Never retry here.
            let response = tokio::select! {
                biased;
                _ = stop.cancelled() => return ModelStart::Indeterminate {
                    reason: "HTTP dispatch cancelled after start admission".into(), usage: Usage::unknown(), start_receipt: None },
                response = sent => match response {
                    Ok(response) => response,
                    Err(_) => return ModelStart::Indeterminate { reason: "HTTP send failed after dispatch boundary".into(), usage: Usage::unknown(), start_receipt: None },
                }
            };
            if !response.status().is_success() {
                let status = response.status().as_u16();
                let kind = match status {
                    401 => ProviderErrorKind::Authentication,
                    403 => ProviderErrorKind::Permission,
                    408 => ProviderErrorKind::Timeout,
                    429 => ProviderErrorKind::RateLimited,
                    400..=499 => ProviderErrorKind::InvalidRequest,
                    503 | 529 => ProviderErrorKind::Overloaded,
                    _ => ProviderErrorKind::Server,
                };
                // Never read or persist error bodies, URLs, or transport diagnostics.
                return failed_stream(ProviderError {
                    kind,
                    message: format!("HTTP {status}"),
                });
            }
            let is_sse = response
                .headers()
                .get("content-type")
                .and_then(|h| h.to_str().ok())
                .is_some_and(|h| {
                    h.split(';')
                        .next()
                        .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/event-stream"))
                });
            if !is_sse {
                return failed_stream(invalid("response is not text/event-stream"));
            }
            ModelStart::Started {
                stream: streaming::make_stream(response.bytes_stream(), stop),
                start_receipt: None,
            }
        })
    }
}

fn failed_stream(error: ProviderError) -> ModelStart {
    ModelStart::Started {
        stream: Box::pin(stream::once(async { Err(error) })),
        start_receipt: None,
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
fn invalid(message: &str) -> ProviderError {
    error(message, ProviderErrorKind::InvalidRequest)
}
fn unsupported(message: &str) -> ProviderError {
    error(message, ProviderErrorKind::Unsupported)
}
fn error(message: &str, kind: ProviderErrorKind) -> ProviderError {
    ProviderError {
        kind,
        message: message.into(),
    }
}

#[cfg(test)]
mod end_to_end;
#[cfg(test)]
mod tests;
