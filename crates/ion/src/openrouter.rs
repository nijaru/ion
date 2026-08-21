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

use ion_core::{EngineSignal, Provider, ProviderRequest, ToolCall, ToolSpec};

/// A step in the OpenAI-compatible SSE stream, decoded.
#[derive(Debug)]
enum StreamEvent {
    Text(String),
    ToolCallFragment {
        index: u64,
        id: Option<String>,
        name: Option<String>,
        arguments_fragment: String,
    },
    FinishReason(String),
}

pub struct OpenRouterProvider {
    model: String,
    api_key: String,
    base_url: String,
    client: reqwest::Client,
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
                "messages": [{ "role": "user", "content": request.prompt }],
                "stream": true,
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
            let mut finish_reason: Option<String> = None;

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
                                    StreamEvent::FinishReason(reason) => {
                                        finish_reason = Some(reason)
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
                if finish_reason.is_some() {
                    break;
                }
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
    let Some(choice) = value.get("choices").and_then(|c| c.get(0)) else {
        return Ok(Vec::new());
    };
    let mut events = Vec::new();
    if let Some(delta) = choice.get("delta") {
        if let Some(text) = delta
            .get("content")
            .and_then(|c| c.as_str())
            .filter(|text| !text.is_empty())
        {
            events.push(StreamEvent::Text(text.to_owned()));
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
    if let Some(reason) = choice.get("finish_reason").and_then(|f| f.as_str()) {
        events.push(StreamEvent::FinishReason(reason.to_owned()));
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
    /// `body` as an HTTP response, then stops.
    fn spawn_sse_server(body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 8192];
            let _ = socket.read(&mut buf);
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
            prompt: "user: hello".to_owned(),
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
    async fn streams_text_and_completes() {
        let base_url = spawn_sse_server(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hel\"}}]}\n\n\
             data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n\
             data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
             data: [DONE]\n\n",
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
}
