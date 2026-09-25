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
    AttemptId, ContentDigest, EgressRealm, ModelBoundary, ModelBoundaryIdentity, ModelStart,
    SemanticRequest, TranscriptContent, TranscriptRole,
};

const MAX_FRAME: usize = 256 * 1024;
const MAX_ERROR: usize = 16 * 1024;
const MAX_RESPONSE: usize = 8 * 1024 * 1024;

/// Host-owned, live credential lookup. The returned secret is used only to build the
/// Authorization header for the immediate request and is never included in fingerprints.
pub trait ApiKeySource: Send + Sync {
    fn api_key(&self) -> Option<String>;
}
impl<F> ApiKeySource for F
where
    F: Fn() -> Option<String> + Send + Sync,
{
    fn api_key(&self) -> Option<String> {
        self()
    }
}

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
    fn payload(request: &SemanticRequest) -> Value {
        let mut messages = vec![json!({"role":"system","content":request.instructions})];
        let mut call_ids = BTreeMap::new();
        for m in &request.messages {
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
                TranscriptContent::ToolCall { invocation, name, arguments, origin_provider_id } => { let id=origin_provider_id.clone().unwrap_or_else(|| format!("ion_{invocation}")); call_ids.insert(*invocation,id.clone()); calls.push(json!({"id":id,"type":"function","function":{"name":name,"arguments":arguments.to_string()}})); },
                TranscriptContent::ToolResult { invocation, name:_, result } => results.push(json!({"role":"tool","tool_call_id":call_ids.get(invocation).cloned().unwrap_or_else(||format!("ion_{invocation}")),"content":result.to_string()})),
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
            ion_ai::Reasoning::Off | ion_ai::Reasoning::BudgetTokens(_) => {
                body["reasoning_effort"] = json!("none")
            }
        }
        body
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
        let envelope = json!({"method":"POST","url":self.endpoint.as_str(),"headers":{"content-type":"application/json","idempotency-key":effect_key},"body":Self::payload(request)});
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
            let body = Self::payload(&request);
            let sent = self
                .client
                .post(self.endpoint.clone())
                .header("Idempotency-Key", effect_key)
                .bearer_auth(key)
                .json(&body)
                .send();
            let response = tokio::select! { _=stop.cancelled()=>return ModelStart::NotStarted{reason:"cancelled before HTTP send".into()}, result=sent=>match result{Ok(r)=>r,Err(_)=>return ModelStart::Indeterminate{reason:"HTTP send failed after dispatch boundary".into(),usage:Usage::unknown(),start_receipt:None}}};
            if !response.status().is_success() {
                let status = response.status();
                let mut body = response.bytes_stream();
                let mut bytes = Vec::new();
                while bytes.len() < MAX_ERROR {
                    match body.next().await {
                        Some(Ok(chunk)) => {
                            let n = (MAX_ERROR - bytes.len()).min(chunk.len());
                            bytes.extend_from_slice(&chunk[..n]);
                            if n < chunk.len() {
                                break;
                            }
                        }
                        _ => break,
                    }
                }
                let detail = String::from_utf8_lossy(&bytes);
                return ModelStart::Started {
                    stream: Box::pin(stream::iter(vec![Err(ProviderError {
                        kind: ProviderErrorKind::Server,
                        message: format!("HTTP {status}: {detail}"),
                    })])),
                    start_receipt: None,
                };
            }
            ModelStart::Started {
                stream: make_stream(
                    response.bytes_stream(),
                    request.model.provider.clone(),
                    stop,
                ),
                start_receipt: None,
            }
        })
    }
}

fn make_stream<S>(mut source: S, provider: String, stop: CancellationToken) -> ModelStream
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
                let payload = frame.split(|byte| *byte == b'\n')
                    .filter_map(|line| line.strip_prefix(b"data:"))
                    .map(|line| String::from_utf8_lossy(line).trim().to_owned())
                    .collect::<Vec<_>>().join("\n");
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
                if let Some(returned) = value.get("model").and_then(Value::as_str) { model = Some(returned.to_owned()); }
                if let Some(usage_value) = value.get("usage")
                    && let (Some(input), Some(output)) = (usage_value["prompt_tokens"].as_u64(), usage_value["completion_tokens"].as_u64()) {
                    usage = Usage::known(input, output);
                    yield Ok(ModelStreamEvent::Usage(usage));
                }
                if let Some(choice) = value["choices"].get(0) {
                    if let Some(delta) = choice.get("delta") {
                        if let Some(part) = delta["content"].as_str() {
                            text.push_str(part);
                            yield Ok(ModelStreamEvent::TextDelta(part.to_owned()));
                        }
                        if let Some(fragments) = delta["tool_calls"].as_array() {
                            for fragment in fragments {
                                let index = fragment["index"].as_u64().unwrap_or(0);
                                let call = calls.entry(index).or_default();
                                if let Some(id) = fragment["id"].as_str() { call.0 = id.to_owned(); }
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
        let _ = provider;
        yield Ok(ModelStreamEvent::Completed(response));
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

    #[test]
    fn canonical_history_replays_function_result_with_matching_provider_id() {
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
        let payload = OpenAiCompatible::payload(&request);
        assert_eq!(
            payload["messages"][1]["tool_calls"][0]["id"],
            "call_original"
        );
        assert_eq!(payload["messages"][2]["tool_call_id"], "call_original");
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
