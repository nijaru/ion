//! Bounded streaming adapter for OpenAI-compatible Chat Completions.
use std::{collections::BTreeMap, sync::Arc};

use futures_util::{StreamExt, stream};
use ion_ai::{
    Content, IncompleteReason, Message, ModelResponse, ModelStream, ModelStreamEvent,
    ProviderError, ProviderErrorKind, ResponseTermination, Role, ToolCall, Usage,
};
use reqwest::Url;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::{
    ApiKeySource, AttemptId, ContentDigest, EgressRealm, ModelBoundary, ModelBoundaryIdentity,
    ModelStart, SemanticRequest, TranscriptContent, TranscriptRole,
};

const MAX_FRAME: usize = 256 * 1024;
const MAX_RESPONSE: usize = 8 * 1024 * 1024;

pub struct OpenAiCompatible {
    identity: ModelBoundaryIdentity,
    endpoint: Url,
    client: reqwest::Client,
    key: Arc<dyn ApiKeySource>,
}

impl OpenAiCompatible {
    pub fn new(
        identity: ModelBoundaryIdentity,
        endpoint: &str,
        key: Arc<dyn ApiKeySource>,
    ) -> Result<Self, String> {
        Self::build(identity, endpoint, key, false)
    }
    #[cfg(test)]
    fn test_local(
        identity: ModelBoundaryIdentity,
        endpoint: &str,
        key: Arc<dyn ApiKeySource>,
    ) -> Result<Self, String> {
        Self::build(identity, endpoint, key, true)
    }
    fn build(
        identity: ModelBoundaryIdentity,
        endpoint: &str,
        key: Arc<dyn ApiKeySource>,
        allow_local: bool,
    ) -> Result<Self, String> {
        let endpoint = Url::parse(endpoint).map_err(|_| "invalid endpoint URL".to_owned())?;
        let local = endpoint
            .host_str()
            .is_some_and(|h| h == "localhost" || h == "127.0.0.1" || h == "::1");
        if endpoint.username() != ""
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || !(endpoint.scheme() == "https"
                || (allow_local && local && endpoint.scheme() == "http"))
        {
            return Err(
                "endpoint must be HTTPS without userinfo/query/fragment (HTTP is test-local only)"
                    .to_owned(),
            );
        }
        let realm = match &identity.egress {
            EgressRealm::Remote(realm) => realm,
            EgressRealm::Local => {
                return Err("OpenAI-compatible endpoint requires a remote egress realm".to_owned());
            }
        };
        let host = endpoint
            .host_str()
            .ok_or_else(|| "endpoint has no host".to_owned())?;
        let origin = format!(
            "{}://{}{}",
            endpoint.scheme(),
            host,
            endpoint.port().map(|p| format!(":{p}")).unwrap_or_default()
        );
        if realm != &origin {
            return Err("endpoint origin does not match frozen remote egress realm".to_owned());
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .map_err(|_| "could not construct HTTP client".to_owned())?;
        Ok(Self {
            identity,
            endpoint,
            client,
            key,
        })
    }
    fn payload(request: &SemanticRequest) -> Result<Value, ProviderError> {
        if matches!(
            request.controls.reasoning,
            ion_ai::Reasoning::BudgetTokens(_)
        ) {
            return Err(err(
                "exact reasoning-token budgets are unsupported by Chat Completions",
                ProviderErrorKind::Unsupported,
            ));
        }
        let mut messages = vec![json!({"role":"system","content":request.instructions})];
        let mut call_ids = BTreeMap::new();
        for m in &request.messages {
            if m.provider_replay.is_some() {
                return Err(err(
                    "opaque provider replay is unsupported by Chat Completions",
                    ProviderErrorKind::Unsupported,
                ));
            }
            let role = match m.role {
                TranscriptRole::User => "user",
                TranscriptRole::Assistant => "assistant",
                TranscriptRole::Tool => "tool",
            };
            let mut text = String::new();
            let mut calls = Vec::new();
            let mut results = Vec::new();
            for c in &m.content {
                match c {
                    TranscriptContent::Text(s) => text.push_str(s),
                    TranscriptContent::ToolCall {
                        invocation,
                        name,
                        arguments,
                        ..
                    } => {
                        // Provider IDs are not globally unique and origin compatibility is
                        // not encoded on this field. Remap all calls by durable invocation
                        // identity, including same-provider history, so fallback cannot
                        // smuggle colliding or incompatible wire IDs into a new request.
                        let id = format!("ion_{invocation}");
                        if call_ids.insert(*invocation, id.clone()).is_some() {
                            return Err(err(
                                "duplicate logical tool invocation",
                                ProviderErrorKind::InvalidRequest,
                            ));
                        }
                        calls.push(json!({"id":id,"type":"function","function":{"name":name,"arguments":arguments.to_string()}}));
                    }
                    TranscriptContent::ToolResult {
                        invocation,
                        name: _,
                        result,
                    } => {
                        let Some(id) = call_ids.get(invocation) else {
                            return Err(err(
                                "orphan logical tool result",
                                ProviderErrorKind::InvalidRequest,
                            ));
                        };
                        results.push(
                            json!({"role":"tool","tool_call_id":id,"content":result.to_string()}),
                        );
                    }
                }
            }
            if !results.is_empty() {
                messages.extend(results);
            } else {
                let mut value =
                    json!({"role":role,"content":if text.is_empty(){Value::Null}else{json!(text)}});
                if !calls.is_empty() {
                    value["tool_calls"] = json!(calls);
                }
                messages.push(value);
            }
        }
        let mut body = json!({"model":request.model.model,"messages":messages,"stream":true,"stream_options":{"include_usage":true},"max_completion_tokens":request.controls.max_output_tokens});
        if let Some(temperature) = request.controls.temperature {
            body["temperature"] = json!(temperature);
        }
        if let Some(top_p) = request.controls.top_p {
            body["top_p"] = json!(top_p);
        }
        if !request.tools.is_empty() {
            body["tools"]=Value::Array(request.tools.iter().map(|t|json!({"type":"function","function":{"name":t.name,"description":t.description,"parameters":t.input_schema}})).collect());
            body["tool_choice"] = match &request.controls.tool_choice {
                ion_ai::ToolChoice::None => json!("none"),
                ion_ai::ToolChoice::Auto => json!("auto"),
                ion_ai::ToolChoice::Required => json!("required"),
                ion_ai::ToolChoice::Named(name) => {
                    json!({"type":"function","function":{"name":name}})
                }
            };
            body["parallel_tool_calls"] = json!(request.controls.parallel_tool_calls);
        }
        match request.controls.reasoning {
            ion_ai::Reasoning::ProviderDefault => {}
            ion_ai::Reasoning::Low => body["reasoning_effort"] = json!("low"),
            ion_ai::Reasoning::Medium => body["reasoning_effort"] = json!("medium"),
            ion_ai::Reasoning::High => body["reasoning_effort"] = json!("high"),
            ion_ai::Reasoning::Off => body["reasoning_effort"] = json!("none"),
            ion_ai::Reasoning::BudgetTokens(_) => unreachable!("rejected before encoding"),
        }
        Ok(body)
    }
}

impl ModelBoundary for OpenAiCompatible {
    fn identity(&self) -> ModelBoundaryIdentity {
        self.identity.clone()
    }
    fn fingerprint(
        &self,
        request: &SemanticRequest,
        effect_key: &str,
    ) -> Result<ContentDigest, ProviderError> {
        let body = Self::payload(request)?;
        let envelope = json!({"method":"POST","url":self.endpoint.as_str(),"headers":{"content-type":"application/json","idempotency-key":effect_key},"body":body});
        serde_json::to_vec(&envelope)
            .map(|bytes| ContentDigest::of_bytes(&bytes))
            .map_err(|e| ProviderError {
                kind: ProviderErrorKind::InvalidRequest,
                message: e.to_string(),
            })
    }
    fn start<'a>(
        &'a self,
        _attempt: AttemptId,
        effect_key: String,
        request: SemanticRequest,
        stop: CancellationToken,
    ) -> ion_ai::BoxFuture<'a, ModelStart> {
        Box::pin(async move {
            let Some(key) = self.key.api_key() else {
                return ModelStart::NotStarted {
                    reason: "provider credential unavailable".into(),
                };
            };
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
            let sent = self
                .client
                .post(self.endpoint.clone())
                .header("Idempotency-Key", effect_key)
                .bearer_auth(key)
                .json(&body)
                .send();
            // Once the send future is polled, cancellation cannot prove no bytes
            // crossed the boundary. Preserve uncertain physical-start evidence.
            let response = tokio::select! {
                _=stop.cancelled()=>return ModelStart::Indeterminate{
                    reason:"HTTP dispatch was cancelled after start admission".into(),
                    usage:Usage::unknown(),start_receipt:None},
                result=sent=>match result{Ok(r)=>r,Err(_)=>return ModelStart::Indeterminate{
                    reason:"HTTP send failed after dispatch boundary".into(),
                    usage:Usage::unknown(),start_receipt:None}}
            };
            if !response.status().is_success() {
                let status = response.status();
                // Untrusted bodies can echo credentials or prompts. Only inspect
                // a bounded 429 body for recognized quota codes; never persist it.
                let kind = match status.as_u16() {
                    401 => ProviderErrorKind::Authentication,
                    403 => ProviderErrorKind::Permission,
                    408 => ProviderErrorKind::Timeout,
                    429 => classify_limit_response(response, &stop).await,
                    400..=499 => ProviderErrorKind::InvalidRequest,
                    503 => ProviderErrorKind::Overloaded,
                    _ => ProviderErrorKind::Server,
                };
                return ModelStart::Started {
                    stream: Box::pin(stream::iter(vec![Err(ProviderError {
                        kind,
                        message: format!("HTTP {status}"),
                    })])),
                    start_receipt: None,
                };
            }
            ModelStart::Started {
                stream: make_stream(response.bytes_stream(), stop),
                start_receipt: None,
            }
        })
    }
}

async fn classify_limit_response(
    mut response: reqwest::Response,
    stop: &CancellationToken,
) -> ProviderErrorKind {
    const MAX_ERROR_BODY: usize = 8 * 1024;
    if response
        .content_length()
        .is_some_and(|size| size > MAX_ERROR_BODY as u64)
    {
        return ProviderErrorKind::RateLimited;
    }
    let mut body = Vec::new();
    loop {
        let chunk = tokio::select! {
            _ = stop.cancelled() => return ProviderErrorKind::RateLimited,
            chunk = response.chunk() => chunk,
        };
        let Some(chunk) = (match chunk {
            Ok(chunk) => chunk,
            Err(_) => return ProviderErrorKind::RateLimited,
        }) else {
            break;
        };
        if body
            .len()
            .checked_add(chunk.len())
            .is_none_or(|len| len > MAX_ERROR_BODY)
        {
            return ProviderErrorKind::RateLimited;
        }
        body.extend_from_slice(&chunk);
    }
    let Ok(value) = serde_json::from_slice::<Value>(&body) else {
        return ProviderErrorKind::RateLimited;
    };
    let code = value.pointer("/error/code").and_then(Value::as_str);
    let category = value.pointer("/error/type").and_then(Value::as_str);
    if matches!(category, Some("insufficient_quota"))
        || matches!(
            code,
            Some(
                "insufficient_quota"
                    | "credit_balance_exhausted"
                    | "organization_usage_limit_exceeded"
                    | "organization_spend_limit_exceeded"
                    | "project_spend_limit_exceeded"
                    | "billing_hard_limit_reached"
            )
        )
    {
        ProviderErrorKind::Quota
    } else {
        ProviderErrorKind::RateLimited
    }
}

fn make_stream<S>(mut source: S, stop: CancellationToken) -> ModelStream
where
    S: futures_util::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Unpin + Send + 'static,
{
    Box::pin(async_stream::stream! {
        let mut buffer = Vec::new();
        let mut total = 0usize;
        let mut text = String::new();
        let mut calls: BTreeMap<u64, (String, String, String)> = BTreeMap::new();
        let mut usage = Usage::unknown();
        let mut finish: Option<String> = None;
        let mut saw_done = false;
        let mut model = None;
        loop {
            let next = tokio::select! {
                _ = stop.cancelled() => { yield Err(err("stream interrupted", ProviderErrorKind::Cancelled)); return; }
                value = source.next() => value,
            };
            let Some(chunk) = next else { break; };
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(_) => { yield Err(err("stream transport interrupted", ProviderErrorKind::Transport)); return; }
            };
            total = total.saturating_add(chunk.len());
            if total > MAX_RESPONSE { yield Err(err("response exceeded byte limit", ProviderErrorKind::InvalidRequest)); return; }
            buffer.extend_from_slice(&chunk);
            if buffer.len() > MAX_FRAME && find_frame_end(&buffer).is_none() {
                yield Err(err("SSE frame exceeded limit", ProviderErrorKind::InvalidRequest)); return;
            }
            while let Some((end, delimiter)) = find_frame_end(&buffer) {
                if end + delimiter > MAX_FRAME { yield Err(err("SSE frame exceeded limit", ProviderErrorKind::InvalidRequest)); return; }
                let frame: Vec<u8> = buffer.drain(..end + delimiter).collect();
                let lines = frame.split(|byte| *byte == b'\n')
                    .filter_map(|line| line.strip_prefix(b"data:"))
                    .map(std::str::from_utf8)
                    .collect::<Result<Vec<_>, _>>();
                let lines = match lines {
                    Ok(lines) => lines,
                    Err(_) => { yield Err(err("SSE data is not UTF-8", ProviderErrorKind::InvalidRequest)); return; }
                };
                let payload = lines.iter().map(|line| line.trim()).collect::<Vec<_>>().join("\n");
                if payload.is_empty() { continue; }
                if payload == "[DONE]" {
                    if finish.is_none() { yield Err(err("[DONE] before finish_reason", ProviderErrorKind::Transport)); return; }
                    saw_done = true;
                    break;
                }
                let value: Value = match serde_json::from_str(&payload) {
                    Ok(value) => value,
                    Err(_) => { yield Err(err("malformed SSE JSON", ProviderErrorKind::InvalidRequest)); return; }
                };
                if value.get("error").is_some_and(|error| !error.is_null()) {
                    yield Err(err("provider sent an SSE error", ProviderErrorKind::Transport));
                    return;
                }
                if let Some(returned) = value.get("model").and_then(Value::as_str) {
                    if model.as_deref().is_some_and(|old| old != returned) {
                        yield Err(err("returned model changed within one response", ProviderErrorKind::InvalidRequest));
                        return;
                    }
                    model = Some(returned.to_owned());
                }
                if let Some(usage_value) = value.get("usage")
                    && let (Some(input), Some(output)) = (usage_value["prompt_tokens"].as_u64(), usage_value["completion_tokens"].as_u64()) {
                    usage = Usage::known(input, output);
                    yield Ok(ModelStreamEvent::Usage(usage));
                }
                if value["choices"].as_array().is_some_and(|choices| choices.len() > 1) {
                    yield Err(err("multiple response choices are unsupported", ProviderErrorKind::InvalidRequest));
                    return;
                }
                if let Some(choice) = value["choices"].get(0) {
                    if choice["index"].as_u64().is_some_and(|index| index != 0) {
                        yield Err(err("unexpected response choice index", ProviderErrorKind::InvalidRequest));
                        return;
                    }
                    if finish.is_some() {
                        if !terminal_usage_trailer(choice, value.get("usage"), finish.as_deref()) {
                            yield Err(err("provider sent a contradictory choice after finish_reason", ProviderErrorKind::InvalidRequest));
                            return;
                        }
                        // Some compatible services repeat the exact terminal reason
                        // in a usage-bearing frame. No content or calls may follow.
                        continue;
                    }
                    if let Some(delta) = choice.get("delta") {
                        if let Some(part) = delta["content"].as_str() {
                            text.push_str(part);
                            yield Ok(ModelStreamEvent::TextDelta(part.to_owned()));
                        }
                        if let Some(fragments) = delta["tool_calls"].as_array() {
                            for fragment in fragments {
                                let Some(index) = fragment["index"].as_u64() else {
                                    yield Err(err("function call fragment has no index", ProviderErrorKind::InvalidRequest));
                                    return;
                                };
                                let call = calls.entry(index).or_default();
                                if let Some(id) = fragment["id"].as_str() {
                                    if !call.0.is_empty() && call.0 != id {
                                        yield Err(err("function call ID changed within one response", ProviderErrorKind::InvalidRequest));
                                        return;
                                    }
                                    call.0 = id.to_owned();
                                }
                                if let Some(name) = fragment["function"]["name"].as_str() { call.1.push_str(name); }
                                if let Some(arguments) = fragment["function"]["arguments"].as_str() { call.2.push_str(arguments); }
                            }
                        }
                    }
                    if let Some(reason) = choice["finish_reason"].as_str() { finish = Some(reason.to_owned()); }
                }
            }
            if saw_done { break; }
        }
        if !saw_done || finish.is_none() {
            yield Err(err("stream ended before finish_reason and [DONE]", ProviderErrorKind::Transport));
            return;
        }
        let termination = match finish.as_deref() {
            Some("stop" | "tool_calls") => ResponseTermination::Completed,
            Some("length") => ResponseTermination::Incomplete(IncompleteReason::MaxOutputTokens),
            Some("content_filter") => ResponseTermination::Incomplete(IncompleteReason::ContentFilter),
            _ => { yield Err(err("unsupported finish_reason", ProviderErrorKind::InvalidRequest)); return; }
        };
        if (finish.as_deref() == Some("tool_calls") && calls.is_empty())
            || (finish.as_deref() == Some("stop") && !calls.is_empty()) {
            yield Err(err("finish_reason contradicts tool call content", ProviderErrorKind::InvalidRequest));
            return;
        }
        let mut content = Vec::new();
        if !text.is_empty() { content.push(Content::Text(text)); }
        if matches!(termination, ResponseTermination::Completed) {
            for (_, (id, name, arguments)) in calls {
                if id.is_empty() || name.is_empty() { yield Err(err("incomplete streamed function call", ProviderErrorKind::InvalidRequest)); return; }
                let arguments = match serde_json::from_str(&arguments) {
                    Ok(value) => value,
                    Err(_) => { yield Err(err("invalid function arguments JSON", ProviderErrorKind::InvalidRequest)); return; }
                };
                content.push(Content::ToolCall(ToolCall { id, name, arguments }));
            }
        }
        let response = ModelResponse {
            message: Message { role: Role::Assistant, content, provider_replay: None },
            usage,
            termination,
            returned_model: model,
        };
        yield Ok(ModelStreamEvent::Completed(response));
    })
}

fn terminal_usage_trailer(choice: &Value, usage: Option<&Value>, finish: Option<&str>) -> bool {
    if usage.is_none_or(Value::is_null) || choice["finish_reason"].as_str() != finish {
        return false;
    }
    choice.get("delta").is_none_or(|delta| {
        delta.as_object().is_some_and(|fields| {
            fields.iter().all(|(key, value)| match key.as_str() {
                "role" => value.is_null() || value.as_str() == Some("assistant"),
                "content" => value.is_null() || value.as_str() == Some(""),
                "tool_calls" => value.is_null() || value.as_array().is_some_and(Vec::is_empty),
                "function_call" | "refusal" => value.is_null(),
                _ => false,
            })
        })
    })
}

fn find_frame_end(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer.windows(2).position(|w| w == b"\n\n").map(|p| (p, 2));
    let crlf = buffer
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| (p, 4));
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(if a.0 < b.0 { a } else { b }),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn err(message: &str, kind: ProviderErrorKind) -> ProviderError {
    ProviderError {
        kind,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ProviderBindingId, SemanticCompatibilityId};
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };

    fn identity(realm: String) -> ModelBoundaryIdentity {
        ModelBoundaryIdentity {
            binding: ProviderBindingId::new("openai-test").unwrap(),
            adapter: SemanticCompatibilityId::new("openai-compatible-v1").unwrap(),
            request_encoding: SemanticCompatibilityId::new("chat-completions-v1").unwrap(),
            egress: EgressRealm::Remote(realm),
        }
    }
    fn request() -> SemanticRequest {
        SemanticRequest {
            provider: ProviderBindingId::new("openai-test").unwrap(),
            model: ion_ai::ModelRef {
                provider: "openai".into(),
                model: "gpt-test".into(),
            },
            instructions: "rules".into(),
            messages: vec![],
            tools: vec![],
            controls: ion_ai::GenerationControls {
                max_output_tokens: 1024,
                temperature: None,
                top_p: None,
                reasoning: ion_ai::Reasoning::ProviderDefault,
                tool_choice: ion_ai::ToolChoice::Auto,
                parallel_tool_calls: false,
            },
        }
    }
    fn server(response: String) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut buf = [0u8; 8192];
            let _ = socket.read(&mut buf);
            let _ = socket.write_all(response.as_bytes());
        });
        format!("http://{addr}")
    }
    #[test]
    fn endpoint_requires_exact_remote_origin_and_public_https() {
        let local=server("HTTP/1.1 302 Found\\r\\nLocation: https://example.com\\r\\nContent-Length: 0\\r\\n\\r\\n".into());
        assert!(
            OpenAiCompatible::new(
                identity("https://example.com".into()),
                &local,
                Arc::new(|| Some("secret".into()))
            )
            .is_err()
        );
        assert!(
            OpenAiCompatible::new(
                identity("https://example.com".into()),
                "http://example.com/v1",
                Arc::new(|| Some("secret".into()))
            )
            .is_err()
        );
        assert!(
            OpenAiCompatible::test_local(
                identity(local.clone()),
                &local,
                Arc::new(|| Some("secret".into()))
            )
            .is_ok()
        );
    }
    #[tokio::test]
    async fn streamed_text_function_calls_usage_and_returned_model() {
        let frames = [
            json!({"model":"route-model","choices":[{"delta":{"content":"hi"},"finish_reason":null}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":1,"id":"call_b","function":{"name":"bar","arguments":"{\"b\":"}},{"index":0,"id":"call_a","function":{"name":"foo","arguments":"{\"a\":"}}]},"finish_reason":null}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"1}"}},{"index":1,"function":{"arguments":"2}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":5,"completion_tokens":3}}),
        ];
        let body = frames
            .iter()
            .map(|v| format!("data: {v}\n\n"))
            .collect::<String>()
            + "data: [DONE]\n\n";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let url = server(response);
        let origin = url.clone();
        let boundary = OpenAiCompatible::test_local(
            identity(origin),
            &url,
            Arc::new(|| Some("top-secret".into())),
        )
        .unwrap();
        let start = boundary
            .start(
                crate::AttemptId::new(1).unwrap(),
                "effect".into(),
                request(),
                CancellationToken::new(),
            )
            .await;
        let ModelStart::Started { mut stream, .. } = start else {
            panic!("expected dispatched")
        };
        let mut saw_delta = false;
        let result = loop {
            match stream.next().await.unwrap().unwrap() {
                ModelStreamEvent::TextDelta(s) => saw_delta |= s == "hi",
                ModelStreamEvent::Usage(u) => assert_eq!(u, Usage::known(5, 3)),
                ModelStreamEvent::Completed(response) => break response,
                other => panic!("unexpected event: {other:?}"),
            }
        };
        assert!(saw_delta);
        assert_eq!(result.returned_model.as_deref(), Some("route-model"));
        assert_eq!(result.usage, Usage::known(5, 3));
        assert_eq!(result.message.content[0], Content::Text("hi".into()));
        assert!(
            matches!(&result.message.content[1],Content::ToolCall(c) if c.id=="call_a" && c.name=="foo" && c.arguments["a"]==1)
        );
        assert!(
            matches!(&result.message.content[2],Content::ToolCall(c) if c.id=="call_b" && c.arguments["b"]==2)
        );
    }
    #[tokio::test]
    async fn truncation_status_errors_and_redirects_never_complete() {
        let cases = [
            r#"HTTP/1.1 200 OK
Content-Length: 42
Content-Type: text/event-stream
Connection: close

data: {"choices":[{"delta":{"content":"partial"},"finish_reason":null}]}

"#,
            "HTTP/1.1 429 Too Many Requests\r\nContent-Length: 9\r\nConnection: close\r\n\r\nrate limit",
            "HTTP/1.1 302 Found\r\nLocation: https://elsewhere.invalid/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ];
        for raw in cases {
            let url = server(raw.into());
            let boundary = OpenAiCompatible::test_local(
                identity(url.clone()),
                &url,
                Arc::new(|| Some("secret".into())),
            )
            .unwrap();
            let ModelStart::Started { mut stream, .. } = boundary
                .start(
                    crate::AttemptId::new(1).unwrap(),
                    "key".into(),
                    request(),
                    CancellationToken::new(),
                )
                .await
            else {
                panic!("expected response")
            };
            let mut completed = false;
            let mut failed = false;
            while let Some(item) = stream.next().await {
                match item {
                    Ok(ModelStreamEvent::Completed(_)) => completed = true,
                    Err(_) => failed = true,
                    _ => {}
                }
            }
            assert!(!completed);
            assert!(failed);
        }
    }

    #[tokio::test]
    async fn authentication_error_never_persists_echoed_credentials() {
        let body = "top-secret returned by an untrusted gateway";
        let url = server(format!(
            "HTTP/1.1 401 Unauthorized\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ));
        let boundary = OpenAiCompatible::test_local(
            identity(url.clone()),
            &url,
            Arc::new(|| Some("top-secret".into())),
        )
        .unwrap();
        let ModelStart::Started { mut stream, .. } = boundary
            .start(
                crate::AttemptId::new(1).unwrap(),
                "effect".into(),
                request(),
                CancellationToken::new(),
            )
            .await
        else {
            panic!("request was not dispatched")
        };
        let failure = stream.next().await.unwrap().unwrap_err();
        assert_eq!(failure.kind, ProviderErrorKind::Authentication);
        assert!(!failure.message.contains("top-secret"));
    }

    #[tokio::test]
    async fn bounded_429_classification_distinguishes_quota_from_retryable_rate_limit() {
        for (body, expected) in [
            (r#"{"error":{"type":"insufficient_quota","code":"credit_balance_exhausted","message":"top-secret"}}"#.to_owned(), ProviderErrorKind::Quota),
            (r#"{"error":{"type":"rate_limit_error","code":"organization_spend_limit_exceeded"}}"#.to_owned(), ProviderErrorKind::Quota),
            (r#"{"error":{"type":"rate_limit_error","code":"rate_limit_exceeded"}}"#.to_owned(), ProviderErrorKind::RateLimited),
            (format!("{{\"error\":{{\"code\":\"insufficient_quota\",\"message\":\"{}\"}}}}", "top-secret".repeat(2000)), ProviderErrorKind::RateLimited),
        ] {
            let url = server(format!(
                "HTTP/1.1 429 Too Many Requests\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ));
            let boundary = OpenAiCompatible::test_local(
                identity(url.clone()), &url, Arc::new(|| Some("top-secret".into())),
            ).unwrap();
            let ModelStart::Started { mut stream, .. } = boundary.start(
                crate::AttemptId::new(1).unwrap(), "effect".into(), request(), CancellationToken::new(),
            ).await else { panic!("request was not dispatched") };
            let failure = stream.next().await.unwrap().unwrap_err();
            assert_eq!(failure.kind, expected);
            assert_eq!(failure.message, "HTTP 429 Too Many Requests");
        }
    }

    #[tokio::test]
    async fn cancellation_after_http_request_is_uncertain_not_not_started() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (reached, arrival) = tokio::sync::oneshot::channel();
        let (release, released) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut bytes = [0u8; 4096];
            assert!(socket.read(&mut bytes).unwrap() > 0);
            reached.send(()).unwrap();
            released.recv().unwrap();
        });
        let boundary = Arc::new(
            OpenAiCompatible::test_local(
                identity(url.clone()),
                &url,
                Arc::new(|| Some("secret".into())),
            )
            .unwrap(),
        );
        let stop = CancellationToken::new();
        let running = tokio::spawn({
            let boundary = Arc::clone(&boundary);
            let stop = stop.clone();
            async move {
                boundary
                    .start(
                        crate::AttemptId::new(1).unwrap(),
                        "effect".into(),
                        request(),
                        stop,
                    )
                    .await
            }
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), arrival)
            .await
            .unwrap()
            .unwrap();
        stop.cancel();
        assert!(matches!(
            running.await.unwrap(),
            ModelStart::Indeterminate { .. }
        ));
        release.send(()).unwrap();
        server.join().unwrap();
    }

    #[tokio::test]
    async fn changing_returned_model_midstream_never_completes() {
        let sse = concat!(
            "data: {\"model\":\"first\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"x\"}}]}\n\n",
            "data: {\"model\":\"second\",\"choices\":[{\"index\":0,\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let url = server(format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{sse}",
            sse.len()
        ));
        let boundary = OpenAiCompatible::test_local(
            identity(url.clone()),
            &url,
            Arc::new(|| Some("secret".into())),
        )
        .unwrap();
        let ModelStart::Started { mut stream, .. } = boundary
            .start(
                crate::AttemptId::new(1).unwrap(),
                "effect".into(),
                request(),
                CancellationToken::new(),
            )
            .await
        else {
            panic!("request was not dispatched")
        };
        assert!(matches!(
            stream.next().await,
            Some(Ok(ModelStreamEvent::TextDelta(_)))
        ));
        assert!(matches!(
            stream.next().await,
            Some(Err(ProviderError {
                kind: ProviderErrorKind::InvalidRequest,
                ..
            }))
        ));
    }

    #[tokio::test]
    async fn usage_trailer_may_repeat_the_same_finish_without_new_content() {
        let body = [
            json!({"model":"gpt-test","choices":[{"index":0,"delta":{"tool_calls":[
                {"index":0,"id":"call_1","function":{"name":"read","arguments":"{}"}}
            ]},"finish_reason":"tool_calls"}]}),
            json!({"model":"gpt-test","choices":[{"index":0,
                "delta":{"content":"","role":"assistant"},"finish_reason":"tool_calls"}],
                "usage":{"prompt_tokens":11,"completion_tokens":7}}),
        ]
        .into_iter()
        .map(|value| format!("data: {value}\n\n"))
        .collect::<String>()
            + "data: [DONE]\n\n";
        let url = server(format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ));
        let boundary = OpenAiCompatible::test_local(
            identity(url.clone()),
            &url,
            Arc::new(|| Some("synthetic".into())),
        )
        .unwrap();
        let ModelStart::Started { mut stream, .. } = boundary
            .start(
                crate::AttemptId::new(1).unwrap(),
                "effect".into(),
                request(),
                CancellationToken::new(),
            )
            .await
        else {
            panic!("dispatch did not begin")
        };
        let mut completed = None;
        while let Some(event) = stream.next().await {
            if let ModelStreamEvent::Completed(response) = event.unwrap() {
                completed = Some(response);
            }
        }
        let response = completed.expect("repeated terminal usage trailer is valid");
        assert_eq!(response.usage, Usage::known(11, 7));
        assert_eq!(response.message.content.len(), 1);
    }

    #[tokio::test]
    async fn contradictory_terminal_frames_never_produce_a_completed_response() {
        let cases = [
            vec![json!({"model":"gpt-test","choices":[{"index":0,"finish_reason":"tool_calls"}]})],
            vec![
                json!({"model":"gpt-test","choices":[{"index":0,"finish_reason":"length"}]}),
                json!({"choices":[{"index":0,"finish_reason":"stop"}]}),
            ],
            vec![
                json!({"model":"gpt-test","choices":[{"index":0,"finish_reason":"stop"}]}),
                json!({"error":{"message":"late failure"}}),
            ],
            vec![
                json!({"model":"gpt-test","choices":[{"index":0,"delta":{"tool_calls":[
                {"index":0,"id":"call_1","function":{"name":"read","arguments":"{}"}}]},
                "finish_reason":"stop"}]}),
            ],
        ];
        for frames in cases {
            let mut body = frames
                .into_iter()
                .map(|value| format!("data: {value}\n\n"))
                .collect::<String>();
            body.push_str("data: [DONE]\n\n");
            let url = server(format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ));
            let boundary = OpenAiCompatible::test_local(
                identity(url.clone()),
                &url,
                Arc::new(|| Some("secret".into())),
            )
            .unwrap();
            let ModelStart::Started { mut stream, .. } = boundary
                .start(
                    crate::AttemptId::new(1).unwrap(),
                    "effect".into(),
                    request(),
                    CancellationToken::new(),
                )
                .await
            else {
                panic!("dispatch did not begin")
            };
            let mut failed = false;
            while let Some(event) = stream.next().await {
                match event {
                    Ok(ModelStreamEvent::Completed(_)) => panic!("malformed terminal was selected"),
                    Err(_) => failed = true,
                    _ => {}
                }
            }
            assert!(failed);
        }
    }

    #[test]
    fn explicit_reasoning_budget_and_opaque_replay_reject_before_dispatch() {
        let boundary = OpenAiCompatible::new(
            identity("https://api.example.test".into()),
            "https://api.example.test/v1/chat/completions",
            Arc::new(|| Some("secret".into())),
        )
        .unwrap();
        let mut configured = crate::config::tests::config();
        configured
            .control_ceiling
            .allowed_reasoning
            .push(ion_ai::Reasoning::BudgetTokens(1024));
        configured.controls.reasoning = ion_ai::Reasoning::BudgetTokens(1024);
        configured.validate().unwrap();
        let mut budget = request();
        budget.controls.reasoning = ion_ai::Reasoning::BudgetTokens(1024);
        assert_eq!(
            boundary.fingerprint(&budget, "effect").unwrap_err().kind,
            ProviderErrorKind::Unsupported
        );
        budget.controls.reasoning = ion_ai::Reasoning::Off;
        assert!(boundary.fingerprint(&budget, "effect").is_ok());
        budget.messages.push(crate::TranscriptMessage {
            role: TranscriptRole::User,
            content: vec![TranscriptContent::Text("hello".into())],
            provider_replay: Some(ion_ai::ProviderReplay::new(
                "another-provider",
                "opaque",
                json!({"token":"not portable"}),
            )),
        });
        assert_eq!(
            boundary.fingerprint(&budget, "effect").unwrap_err().kind,
            ProviderErrorKind::Unsupported
        );
    }

    #[test]
    fn canonical_history_remaps_colliding_provider_ids_to_unique_logical_aliases() {
        let invocation = crate::InvocationId::new(7).unwrap();
        let mut request = request();
        request.messages = vec![
            crate::TranscriptMessage {
                role: TranscriptRole::Assistant,
                content: vec![TranscriptContent::ToolCall {
                    invocation,
                    name: "read".into(),
                    arguments: json!({"path":"a"}),
                    origin_provider_id: Some("call_original".into()),
                }],
                provider_replay: None,
            },
            crate::TranscriptMessage {
                role: TranscriptRole::Tool,
                content: vec![TranscriptContent::ToolResult {
                    invocation,
                    name: "read".into(),
                    result: json!({"ok":true}),
                }],
                provider_replay: None,
            },
        ];
        for (id, origin) in [(8, "call_original"), (9, "ion_7")] {
            let invocation = crate::InvocationId::new(id).unwrap();
            request.messages.push(crate::TranscriptMessage {
                role: TranscriptRole::Assistant,
                content: vec![TranscriptContent::ToolCall {
                    invocation,
                    name: "read".into(),
                    arguments: json!({"path":"b"}),
                    origin_provider_id: Some(origin.into()),
                }],
                provider_replay: None,
            });
            request.messages.push(crate::TranscriptMessage {
                role: TranscriptRole::Tool,
                content: vec![TranscriptContent::ToolResult {
                    invocation,
                    name: "read".into(),
                    result: json!({"ok":true}),
                }],
                provider_replay: None,
            });
        }
        let payload = OpenAiCompatible::payload(&request).unwrap();
        for (index, id) in [(1, "ion_7"), (3, "ion_8"), (5, "ion_9")] {
            assert_eq!(payload["messages"][index]["tool_calls"][0]["id"], id);
            assert_eq!(payload["messages"][index + 1]["tool_call_id"], id);
        }
    }

    #[test]
    fn auth_is_not_part_of_canonical_fingerprint() {
        let url = "https://api.example.test/v1/chat/completions";
        let boundary = OpenAiCompatible::new(
            identity("https://api.example.test".into()),
            url,
            Arc::new(|| Some("secret".into())),
        )
        .unwrap();
        let digest = boundary.fingerprint(&request(), "effect-key").unwrap();
        assert_eq!(
            digest,
            boundary.fingerprint(&request(), "effect-key").unwrap()
        );
        assert_ne!(
            digest,
            boundary.fingerprint(&request(), "other-effect").unwrap()
        );
        let serialized = serde_json::to_string(&request()).unwrap();
        assert!(!serialized.contains("secret"));
    }
}

#[cfg(test)]
mod end_to_end;
