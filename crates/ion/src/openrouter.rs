//! OpenRouter provider adapter (DESIGN.md §13.3, §15).
//!
//! One model step per [`Provider::run`] call: the projected input plus
//! the frozen tool snapshot in, one validated provider generation out.
//! OpenRouter's OpenAI-compatible chat-completions API over SSE.
//!
//! Conservative v0 semantics, per design:
//! - tool calls are accumulated until the provider finishes the step and
//!   are emitted only as complete calls (§15.2);
//! - malformed accumulated arguments fail the step visibly instead of
//!   being repaired (§15.3, §33.10);
//! - no automatic retries — the operation layer owns retry policy
//!   (§10.5);
//! - no provider artifacts are stored; local semantic state stays
//!   canonical (P8, §13.2).

use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use ion_core::{
    ContextMessage, ContextPlan, EngineSignal, Provider, ProviderRequest, TokenUsage, ToolCall,
    ToolSpec,
};

/// A step in the OpenAI-compatible SSE stream, decoded.
#[derive(Debug)]
enum StreamEvent {
    Text(String),
    Thinking(String),
    ToolCallFragment {
        index: u64,
        id: Option<String>,
        name: Option<String>,
        arguments_fragment: String,
    },
    Usage(TokenUsage),
}

pub struct OpenRouterProvider {
    model: String,
    api_key: String,
    base_url: String,
    client: reqwest::Client,
    context_window: tokio::sync::OnceCell<Option<u64>>,
}

impl OpenRouterProvider {
    /// Build an adapter for `model` (e.g. `stealth/ox-alpha`) against the
    /// standard OpenRouter endpoint.
    #[must_use]
    pub fn new(model: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            api_key: api_key.into(),
            base_url: "https://openrouter.ai/api/v1".to_owned(),
            client: reqwest::Client::new(),
            context_window: tokio::sync::OnceCell::new(),
        }
    }

    /// Override the API root (tests point this at a local server).
    #[cfg(test)]
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    fn tool_specs(specs: &[ToolSpec]) -> serde_json::Value {
        serde_json::Value::Array(
            specs
                .iter()
                .map(|spec| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": spec.name,
                            "description": spec.description,
                            "parameters": spec.input_schema,
                        },
                    })
                })
                .collect(),
        )
    }
}

impl Provider for OpenRouterProvider {
    /// Fetch the model's `context_length` from the models endpoint
    /// once, lazily; any failure degrades to unknown (§14.8).
    async fn context_window(&self) -> Option<u64> {
        *self
            .context_window
            .get_or_init(|| async {
                let url = format!("{}/models/{}", self.base_url, self.model);
                let value = self
                    .client
                    .get(url)
                    .bearer_auth(&self.api_key)
                    .send()
                    .await
                    .ok()?
                    .error_for_status()
                    .ok()?
                    .json::<serde_json::Value>()
                    .await
                    .ok()?;
                value
                    .get("data")
                    .and_then(|data| data.get("context_length"))
                    .and_then(|v| v.as_u64())
            })
            .await
    }

    fn run(
        &self,
        request: ProviderRequest,
        cancel: CancellationToken,
        out: mpsc::Sender<EngineSignal>,
    ) -> impl Future<Output = ()> + Send {
        let operation_id = request.operation_id;
        let step = request.step;
        let model = self.model.clone();
        let api_key = self.api_key.clone();
        let base_url = self.base_url.clone();
        let client = self.client.clone();
        async move {
            let mut body = serde_json::json!({
                "model": model,
                "messages": message_payloads(&request.plan),
                "stream": true,
                "stream_options": { "include_usage": true },
            });
            if !request.tools.is_empty() {
                body["tools"] = OpenRouterProvider::tool_specs(&request.tools);
            }

            let response = tokio::select! {
                () = cancel.cancelled() => {
                    let _ = out.send(EngineSignal::Cancelled { operation_id, step }).await;
                    return;
                }
                response = client
                    .post(format!("{base_url}/chat/completions"))
                    .bearer_auth(&api_key)
                    .json(&body)
                    .send() =>
                {
                    match response {
                        Ok(response) => response,
                        Err(err) => {
                            let _ = out.send(EngineSignal::Failed {
                                operation_id,
                                step,
                                message: format!("provider request failed: {err}"),
                            }).await;
                            return;
                        }
                    }
                }
            };

            if !response.status().is_success() {
                let status = response.status();
                let detail = response.text().await.unwrap_or_default();
                let _ = out
                    .send(EngineSignal::Failed {
                        operation_id,
                        step,
                        message: format!("provider returned {status}: {detail}"),
                    })
                    .await;
                return;
            }

            let mut stream = response.bytes_stream();
            let mut buffer = Vec::new();
            // Accumulated tool calls by stream index: (id, name, args).
            let mut tool_calls: Vec<(Option<String>, String, String)> = Vec::new();

            loop {
                let chunk = tokio::select! {
                    () = cancel.cancelled() => {
                        let _ = out.send(EngineSignal::Cancelled { operation_id, step }).await;
                        return;
                    }
                    chunk = stream.next() => match chunk {
                        Some(Ok(bytes)) => bytes,
                        Some(Err(err)) => {
                            let _ = out.send(EngineSignal::Failed {
                                operation_id,
                                step,
                                message: format!("provider stream failed: {err}"),
                            }).await;
                            return;
                        }
                        None => break,
                    }
                };
                buffer.extend_from_slice(&chunk);
                while let Some(position) = find_line_end(&buffer) {
                    let line: Vec<u8> = buffer.drain(..=position).collect();
                    let line = String::from_utf8_lossy(&line).trim().to_owned();
                    let Some(payload) = line.strip_prefix("data:") else {
                        continue;
                    };
                    let payload = payload.trim();
                    if payload == "[DONE]" {
                        buffer.clear();
                        break;
                    }
                    match decode_events(payload) {
                        Ok(events) => {
                            for event in events {
                                match event {
                                    StreamEvent::Text(text) => {
                                        if out
                                            .send(EngineSignal::TextDelta {
                                                operation_id,
                                                step,
                                                text,
                                            })
                                            .await
                                            .is_err()
                                        {
                                            return;
                                        }
                                    }
                                    StreamEvent::Thinking(text) => {
                                        if out
                                            .send(EngineSignal::ThinkingDelta {
                                                operation_id,
                                                step,
                                                text,
                                            })
                                            .await
                                            .is_err()
                                        {
                                            return;
                                        }
                                    }
                                    StreamEvent::ToolCallFragment {
                                        index,
                                        id,
                                        name,
                                        arguments_fragment,
                                    } => {
                                        let slot = ensure_slot(&mut tool_calls, index);
                                        if let Some(id) = id {
                                            slot.0 = Some(id);
                                        }
                                        if let Some(name) = name {
                                            slot.1 = name;
                                        }
                                        slot.2.push_str(&arguments_fragment);
                                    }
                                    StreamEvent::Usage(usage) => {
                                        if out
                                            .send(EngineSignal::UsageUpdate {
                                                operation_id,
                                                step,
                                                usage,
                                            })
                                            .await
                                            .is_err()
                                        {
                                            return;
                                        }
                                    }
                                }
                            }
                        }
                        Err(err) => {
                            // Malformed provider data is visible, never
                            // repaired (§15.3, §33.10).
                            let _ = out
                                .send(EngineSignal::Failed {
                                    operation_id,
                                    step,
                                    message: format!("malformed provider stream: {err}"),
                                })
                                .await;
                            return;
                        }
                    }
                }
                // No early break on finish_reason: the usage chunk (and
                // any trailing provider metadata) arrives after it; the
                // stream ends at [DONE] or EOF.
            }

            // The step is complete: emit whole tool calls only (§15.2).
            for (index, (_id, name, arguments)) in tool_calls.iter().enumerate() {
                let parsed: serde_json::Value = if arguments.trim().is_empty() {
                    serde_json::json!({})
                } else {
                    match serde_json::from_str(arguments) {
                        Ok(value) => value,
                        Err(err) => {
                            let _ = out
                                .send(EngineSignal::Failed {
                                    operation_id,
                                    step,
                                    message: format!("malformed tool arguments for {name}: {err}"),
                                })
                                .await;
                            return;
                        }
                    }
                };
                if out
                    .send(EngineSignal::ToolCallCompleted {
                        operation_id,
                        step,
                        call: ToolCall {
                            operation_id,
                            call_id: index as u64 + 1,
                            name: name.clone(),
                            arguments: parsed,
                        },
                    })
                    .await
                    .is_err()
                {
                    return;
                }
            }
            let _ = out
                .send(EngineSignal::Completed { operation_id, step })
                .await;
        }
    }
}

/// Translate a deterministic context plan into OpenAI-compatible chat
/// messages: one system message, then role-structured conversation
/// messages with paired tool results.
fn message_payloads(plan: &ContextPlan) -> Vec<serde_json::Value> {
    let mut out = vec![serde_json::json!({
        "role": "system",
        "content": plan.system,
    })];
    for message in &plan.messages {
        match message {
            ContextMessage::User { content } => {
                out.push(serde_json::json!({ "role": "user", "content": content }));
            }
            ContextMessage::Assistant {
                content,
                tool_calls,
            } => {
                let mut message = serde_json::json!({ "role": "assistant" });
                if content.is_empty() {
                    message["content"] = serde_json::Value::Null;
                } else {
                    message["content"] = serde_json::json!(content);
                }
                if !tool_calls.is_empty() {
                    message["tool_calls"] = serde_json::Value::Array(
                        tool_calls
                            .iter()
                            .map(|call| {
                                serde_json::json!({
                                    "id": format!("call_{}", call.call_id),
                                    "type": "function",
                                    "function": {
                                        "name": call.name,
                                        "arguments": call.arguments.to_string(),
                                    },
                                })
                            })
                            .collect(),
                    );
                }
                out.push(message);
            }
            ContextMessage::Tool { call_id, content } => {
                out.push(serde_json::json!({
                    "role": "tool",
                    "tool_call_id": format!("call_{}", call_id),
                    "content": content,
                }));
            }
        }
    }
    out
}

fn find_line_end(buffer: &[u8]) -> Option<usize> {
    buffer.iter().position(|&b| b == b'\n')
}

fn ensure_slot(
    tool_calls: &mut Vec<(Option<String>, String, String)>,
    index: u64,
) -> &mut (Option<String>, String, String) {
    while tool_calls.len() <= index as usize {
        tool_calls.push((None, String::new(), String::new()));
    }
    &mut tool_calls[index as usize]
}

/// Decode one SSE `data:` payload into the stream events it carries.
fn decode_events(payload: &str) -> Result<Vec<StreamEvent>, String> {
    let value: serde_json::Value = serde_json::from_str(payload).map_err(|err| err.to_string())?;
    if value.get("error").is_some() {
        return Err(format!("provider error: {}", value["error"]));
    }
    let mut events = Vec::new();
    let usage_value = value.get("usage");
    if let (Some(input), Some(output)) = (
        usage_value
            .and_then(|usage| usage.get("prompt_tokens"))
            .and_then(|v| v.as_u64()),
        usage_value
            .and_then(|usage| usage.get("completion_tokens"))
            .and_then(|v| v.as_u64()),
    ) {
        let cache_read = usage_value
            .and_then(|usage| usage.get("prompt_tokens_details"))
            .and_then(|details| details.get("cached_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        // Anthropic-style cache writes, when the upstream passes them
        // through OpenRouter.
        let cache_write = usage_value
            .and_then(|usage| usage.get("cache_creation_input_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        events.push(StreamEvent::Usage(TokenUsage {
            input,
            output,
            cache_read,
            cache_write,
        }));
    }
    let Some(choice) = value.get("choices").and_then(|c| c.get(0)) else {
        // Usage-only terminal chunks carry an empty choices array.
        return Ok(events);
    };
    if let Some(delta) = choice.get("delta") {
        if let Some(text) = delta
            .get("content")
            .and_then(|c| c.as_str())
            .filter(|text| !text.is_empty())
        {
            events.push(StreamEvent::Text(text.to_owned()));
        }
        if let Some(text) = delta
            .get("reasoning")
            .and_then(|c| c.as_str())
            .filter(|text| !text.is_empty())
        {
            events.push(StreamEvent::Thinking(text.to_owned()));
        }
        if let Some(fragments) = delta.get("tool_calls").and_then(|t| t.as_array()) {
            for fragment in fragments {
                let index = fragment.get("index").and_then(|i| i.as_u64()).unwrap_or(0);
                let id = fragment
                    .get("id")
                    .and_then(|i| i.as_str())
                    .map(str::to_owned);
                let name = fragment
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    .map(str::to_owned);
                let arguments_fragment = fragment
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(|a| a.as_str())
                    .unwrap_or("")
                    .to_owned();
                events.push(StreamEvent::ToolCallFragment {
                    index,
                    id,
                    name,
                    arguments_fragment,
                });
            }
        }
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ion_core::ToolRegistry;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    /// One-shot local SSE server: replies to the first request with
    /// `body` as an HTTP response, then stops. The request body lands
    /// in `captured` for assertions.
    fn spawn_sse_server(
        body: &'static str,
        captured: Option<std::sync::mpsc::Sender<String>>,
    ) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept");
            let mut buf = vec![0u8; 16384];
            let n = socket.read(&mut buf).unwrap_or(0);
            if let (Some(captured), Some(payload)) = (
                captured,
                String::from_utf8(buf[..n].to_vec())
                    .ok()
                    .and_then(|text| text.split("\r\n\r\n").nth(1).map(str::to_owned)),
            ) {
                let _ = captured.send(payload);
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes());
        });
        format!("http://127.0.0.1:{port}/v1")
    }

    async fn collect(provider: OpenRouterProvider) -> Vec<EngineSignal> {
        let cancel = CancellationToken::new();
        let (tx, mut rx) = mpsc::channel(64);
        let request = ProviderRequest {
            operation_id: ion_core::OperationId::generate(),
            step: 1,
            model: ion_core::ModelConfig {
                model_ref: "test/model".to_owned(),
                context_window: None,
            },
            plan: ContextPlan {
                system: "sys".to_owned(),
                messages: vec![ContextMessage::User {
                    content: "hello".to_owned(),
                }],
            },
            tools: Vec::new(),
        };
        let handle = tokio::spawn(async move {
            provider.run(request, cancel, tx).await;
        });
        let mut signals = Vec::new();
        while let Ok(Some(signal)) = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            signals.push(signal);
        }
        let _ = handle.await;
        signals
    }

    #[tokio::test]
    async fn streams_reasoning_as_thinking_deltas() {
        let base_url = spawn_sse_server(
            "data: {\"choices\":[{\"delta\":{\"reasoning\":\"ponder\"}}]}\n\n\
             data: {\"choices\":[{\"delta\":{\"content\":\"ans\"}}]}\n\n\
             data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
             data: [DONE]\n\n",
            None,
        );
        let provider = OpenRouterProvider::new("test/model", "key").with_base_url(base_url);
        let signals = collect(provider).await;
        let thinking: Vec<&str> = signals
            .iter()
            .filter_map(|s| match s {
                EngineSignal::ThinkingDelta { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(thinking, ["ponder"]);
        // Text and completion are unaffected by interleaved reasoning.
        assert!(signals.iter().any(|s| matches!(
            s,
            EngineSignal::TextDelta { text, .. } if text == "ans"
        )));
        assert!(matches!(
            signals.last(),
            Some(EngineSignal::Completed { .. })
        ));
    }

    #[tokio::test]
    async fn streams_text_and_completes() {
        let base_url = spawn_sse_server(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hel\"}}]}\n\n\
             data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n\
             data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
             data: [DONE]\n\n",
            None,
        );
        let provider = OpenRouterProvider::new("test/model", "key").with_base_url(base_url);
        let signals = collect(provider).await;
        let texts: Vec<&str> = signals
            .iter()
            .filter_map(|s| match s {
                EngineSignal::TextDelta { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, ["hel", "lo"]);
        assert!(matches!(
            signals.last(),
            Some(EngineSignal::Completed { .. })
        ));
    }

    #[tokio::test]
    async fn tool_call_fragments_become_one_completed_call() {
        let base_url = spawn_sse_server(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"read\",\"arguments\":\"\"}}]}}]}\n\n\
             data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"path\\\":\\\"Cargo.toml\\\"}\"}}]}}]}\n\n\
             data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n\
             data: [DONE]\n\n",
            None,
        );
        let provider = OpenRouterProvider::new("test/model", "key").with_base_url(base_url);
        let signals = collect(provider).await;
        let calls: Vec<&ToolCall> = signals
            .iter()
            .filter_map(|s| match s {
                EngineSignal::ToolCallCompleted { call, .. } => Some(call),
                _ => None,
            })
            .collect();
        assert_eq!(
            calls.len(),
            1,
            "fragments must become one call: {signals:?}"
        );
        assert_eq!(calls[0].name, "read");
        assert_eq!(calls[0].arguments["path"], "Cargo.toml");
        assert!(matches!(
            signals.last(),
            Some(EngineSignal::Completed { .. })
        ));
    }

    #[tokio::test]
    async fn malformed_arguments_fail_visibly() {
        let base_url = spawn_sse_server(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"read\",\"arguments\":\"{not json\"}}]}}]}\n\n\
             data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n\
             data: [DONE]\n\n",
            None,
        );
        let provider = OpenRouterProvider::new("test/model", "key").with_base_url(base_url);
        let signals = collect(provider).await;
        assert!(matches!(
            signals.last(),
            Some(EngineSignal::Failed { message, .. }) if message.contains("malformed tool arguments")
        ));
    }

    /// Live proof against a real free OpenRouter model. Run explicitly:
    /// `cargo test -p ion --lib -- --ignored live_openrouter --nocapture`
    #[tokio::test]
    #[ignore = "live network; requires OPENROUTER_API_KEY"]
    async fn live_openrouter_completes_one_step() {
        let Some(key) = std::env::var("OPENROUTER_API_KEY").ok() else {
            panic!("OPENROUTER_API_KEY not set");
        };
        let provider = OpenRouterProvider::new("stealth/ox-alpha", key);
        let signals = collect(provider).await;
        for signal in &signals {
            println!("{signal:?}");
        }
        assert!(
            matches!(signals.last(), Some(EngineSignal::Completed { .. })),
            "live step must complete"
        );
        let _ = ToolRegistry::default();
    }
    #[tokio::test]
    async fn plan_becomes_role_structured_messages() {
        let (tx, rx) = std::sync::mpsc::channel();
        let base_url = spawn_sse_server(
            "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\n\
             data: [DONE]\n\n",
            Some(tx),
        );
        let provider = OpenRouterProvider::new("test/model", "key").with_base_url(base_url);
        let signals = collect(provider).await;
        assert!(matches!(
            signals.last(),
            Some(EngineSignal::Completed { .. })
        ));
        let body: serde_json::Value = serde_json::from_str(
            &rx.recv_timeout(std::time::Duration::from_secs(5))
                .expect("body"),
        )
        .expect("json body");
        assert_eq!(body["stream_options"]["include_usage"], true);
        let messages = body["messages"].as_array().expect("messages");
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "sys");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "hello");
    }

    #[tokio::test]
    async fn final_usage_chunk_becomes_a_usage_signal() {
        let base_url = spawn_sse_server(
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
             data: {\"choices\":[],\"usage\":{\"prompt_tokens\":120,\"completion_tokens\":34}}\n\n\
             data: [DONE]\n\n",
            None,
        );
        let provider = OpenRouterProvider::new("test/model", "key").with_base_url(base_url);
        let signals = collect(provider).await;
        let usages: Vec<_> = signals
            .iter()
            .filter_map(|signal| match signal {
                EngineSignal::UsageUpdate { usage, .. } => Some(*usage),
                _ => None,
            })
            .collect();
        assert_eq!(usages.len(), 1);
        assert_eq!((usages[0].input, usages[0].output), (120, 34));
        assert_eq!(usages[0].cache_read, 0);
        assert_eq!(usages[0].cache_write, 0);
    }

    #[tokio::test]
    async fn cache_metrics_decode_from_usage_details() {
        let base_url = spawn_sse_server(
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":500,\"completion_tokens\":10,\
             \"prompt_tokens_details\":{\"cached_tokens\":420},\"cache_creation_input_tokens\":80}}\n\n\
             data: [DONE]\n\n",
            None,
        );
        let provider = OpenRouterProvider::new("test/model", "key").with_base_url(base_url);
        let signals = collect(provider).await;
        let usages: Vec<_> = signals
            .iter()
            .filter_map(|signal| match signal {
                EngineSignal::UsageUpdate { usage, .. } => Some(*usage),
                _ => None,
            })
            .collect();
        assert_eq!(usages.len(), 1);
        assert_eq!(usages[0].cache_read, 420);
        assert_eq!(usages[0].cache_write, 80);
    }

    #[tokio::test]
    async fn context_window_fetched_from_models_endpoint() {
        let base_url = spawn_sse_server(
            "{\"data\":{\"id\":\"test/model\",\"context_length\":1000000}}",
            None,
        );
        let provider = OpenRouterProvider::new("test/model", "key").with_base_url(base_url);
        assert_eq!(provider.context_window().await, Some(1_000_000));
    }

    #[tokio::test]
    async fn context_window_degrades_to_unknown_on_bad_payload() {
        let base_url = spawn_sse_server("{\"data\":{}}", None);
        let provider = OpenRouterProvider::new("test/model", "key").with_base_url(base_url);
        assert_eq!(provider.context_window().await, None);
    }
}
