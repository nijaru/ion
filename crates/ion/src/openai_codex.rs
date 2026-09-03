//! Pi-compatible OpenAI Codex Responses adapter.
//!
//! Ion keeps the semantic conversation local and sends a fresh canonical
//! Responses request for each model step. Provider output is normalized into
//! Ion signals; partial tool arguments never leave this adapter.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use futures_util::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use ion_core::{
    ContextMessage, ContextPlan, EngineSignal, ModelCapabilities, Provider, ProviderRequest,
    TokenUsage, ToolCall, ToolSpec,
};

const DEFAULT_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api";
const CODEX_CONTEXT_WINDOW: u64 = 272_000;

#[derive(Clone)]
pub struct CodexCredential {
    access_token: String,
    account_id: String,
}

impl CodexCredential {
    /// Resolve an existing Pi OAuth credential without modifying the Pi
    /// authentication file. An explicit environment token takes precedence.
    pub fn from_environment_or_pi() -> Result<Self, String> {
        let access_token = std::env::var("OPENAI_CODEX_ACCESS_TOKEN").ok();
        let account_id = std::env::var("OPENAI_CODEX_ACCOUNT_ID").ok();
        if let Some(access_token) = access_token {
            let account_id = account_id
                .or_else(|| jwt_account_id(&access_token))
                .ok_or_else(|| {
                    "OPENAI_CODEX_ACCOUNT_ID is required when the access token has no account claim"
                        .to_owned()
                })?;
            return Ok(Self {
                access_token,
                account_id,
            });
        }

        let path = pi_auth_path()?;
        let document = std::fs::read_to_string(&path).map_err(|err| {
            format!(
                "cannot read Pi authentication file {}: {err}",
                path.display()
            )
        })?;
        parse_pi_credential(&document, now_millis()).map_err(|err| {
            format!(
                "cannot use OpenAI Codex credential from {}: {err}",
                path.display()
            )
        })
    }

    #[cfg(test)]
    fn access_token(&self) -> &str {
        &self.access_token
    }

    #[cfg(test)]
    fn account_id(&self) -> &str {
        &self.account_id
    }
}

pub struct OpenAICodexProvider {
    model: String,
    credential: CodexCredential,
    base_url: String,
    client: reqwest::Client,
    reasoning_effort: Option<String>,
}

impl OpenAICodexProvider {
    #[must_use]
    pub fn new(
        model: impl Into<String>,
        access_token: impl Into<String>,
        account_id: impl Into<String>,
    ) -> Self {
        Self {
            model: model.into(),
            credential: CodexCredential {
                access_token: access_token.into(),
                account_id: account_id.into(),
            },
            base_url: DEFAULT_CODEX_BASE_URL.to_owned(),
            client: reqwest::Client::new(),
            reasoning_effort: Some("xhigh".to_owned()),
        }
    }

    #[must_use]
    pub fn from_credential(model: impl Into<String>, credential: CodexCredential) -> Self {
        Self {
            model: model.into(),
            credential,
            base_url: DEFAULT_CODEX_BASE_URL.to_owned(),
            client: reqwest::Client::new(),
            reasoning_effort: Some("xhigh".to_owned()),
        }
    }

    #[must_use]
    pub fn with_reasoning_effort(mut self, reasoning_effort: Option<String>) -> Self {
        self.reasoning_effort = reasoning_effort;
        self
    }

    #[cfg(test)]
    #[must_use]
    fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    fn tool_specs(specs: &[ToolSpec]) -> Value {
        Value::Array(
            specs
                .iter()
                .map(|spec| {
                    serde_json::json!({
                        "type": "function",
                        "name": spec.name,
                        "description": spec.description,
                        "parameters": spec.input_schema,
                        "strict": false,
                    })
                })
                .collect(),
        )
    }
}

impl Provider for OpenAICodexProvider {
    fn initial_model_ref(&self) -> String {
        self.model.clone()
    }

    async fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            reasoning: true,
            tool_calls: true,
            prompt_cache: true,
            streaming: true,
            images: false,
        }
    }

    async fn context_window(&self) -> Option<u64> {
        Some(CODEX_CONTEXT_WINDOW)
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
        let credential = self.credential.clone();
        let endpoint = codex_endpoint(&self.base_url);
        // Per-step frozen thinking selection wins; the adapter default is
        // only the fallback for lanes that never chose one (§14.8).
        let reasoning_effort = request
            .model
            .thinking
            .clone()
            .or_else(|| self.reasoning_effort.clone());
        let client = self.client.clone();
        async move {
            if !request.model.capabilities.streaming {
                send_failed(
                    &out,
                    operation_id,
                    step,
                    "provider capability mismatch: streaming is required",
                )
                .await;
                return;
            }
            if !request.tools.is_empty() && !request.model.capabilities.tool_calls {
                send_failed(
                    &out,
                    operation_id,
                    step,
                    "provider capability mismatch: tool calls are unavailable",
                )
                .await;
                return;
            }

            let mut body = serde_json::json!({
                "model": model,
                "store": false,
                "stream": true,
                "instructions": request.plan.system,
                "input": response_input(&request.plan),
                "text": { "verbosity": "low" },
                "include": ["reasoning.encrypted_content"],
                "tool_choice": "auto",
                "parallel_tool_calls": true,
                "tools": OpenAICodexProvider::tool_specs(&request.tools),
            });
            if let Some(effort) = reasoning_effort {
                body["reasoning"] = serde_json::json!({ "effort": effort, "summary": "auto" });
            }

            let response = tokio::select! {
                () = cancel.cancelled() => {
                    let _ = out.send(EngineSignal::Cancelled { operation_id, step }).await;
                    return;
                }
                response = client
                    .post(endpoint)
                    .header("Authorization", format!("Bearer {}", credential.access_token))
                    .header("chatgpt-account-id", credential.account_id)
                    .header("originator", "pi")
                    .header("User-Agent", concat!("ion/", env!("CARGO_PKG_VERSION")))
                    .header("OpenAI-Beta", "responses=experimental")
                    .header("accept", "text/event-stream")
                    .header("content-type", "application/json")
                    .header("session-id", operation_id.as_uuid().to_string())
                    .header("x-client-request-id", operation_id.as_uuid().to_string())
                    .json(&body)
                    .send() =>
                {
                    match response {
                        Ok(response) => response,
                        Err(err) => {
                            send_failed(&out, operation_id, step, &format!("provider request failed: {err}")).await;
                            return;
                        }
                    }
                }
            };

            if !response.status().is_success() {
                let status = response.status();
                let detail = response.text().await.unwrap_or_default();
                send_failed(
                    &out,
                    operation_id,
                    step,
                    &format!("provider returned {status}: {detail}"),
                )
                .await;
                return;
            }

            let mut stream = response.bytes_stream();
            let mut buffer = Vec::new();
            let mut drafts = BTreeMap::<u64, ToolDraft>::new();
            let mut terminal = false;

            'stream: loop {
                let chunk = tokio::select! {
                    () = cancel.cancelled() => {
                        let _ = out.send(EngineSignal::Cancelled { operation_id, step }).await;
                        return;
                    }
                    chunk = stream.next() => match chunk {
                        Some(Ok(bytes)) => bytes,
                        Some(Err(err)) => {
                            send_failed(&out, operation_id, step, &format!("provider stream failed: {err}")).await;
                            return;
                        }
                        None => break,
                    }
                };
                buffer.extend_from_slice(&chunk);
                while let Some(position) = find_line_end(&buffer) {
                    let line: Vec<u8> = buffer.drain(..=position).collect();
                    let line = String::from_utf8_lossy(&line).trim().to_owned();
                    let Some(payload) = line.strip_prefix("data:").map(str::trim) else {
                        continue;
                    };
                    if payload.is_empty() || payload == "[DONE]" {
                        continue;
                    }
                    let value: Value = match serde_json::from_str(payload) {
                        Ok(value) => value,
                        Err(err) => {
                            send_failed(
                                &out,
                                operation_id,
                                step,
                                &format!("malformed provider stream: {err}"),
                            )
                            .await;
                            return;
                        }
                    };
                    match value.get("type").and_then(Value::as_str) {
                        Some("response.output_text.delta") => {
                            if let Some(text) = value.get("delta").and_then(Value::as_str)
                                && out
                                    .send(EngineSignal::TextDelta {
                                        operation_id,
                                        step,
                                        text: text.to_owned(),
                                    })
                                    .await
                                    .is_err()
                            {
                                return;
                            }
                        }
                        Some("response.reasoning_summary_text.delta")
                        | Some("response.reasoning_text.delta") => {
                            if request.model.capabilities.reasoning
                                && let Some(text) = value.get("delta").and_then(Value::as_str)
                                && out
                                    .send(EngineSignal::ThinkingDelta {
                                        operation_id,
                                        step,
                                        text: text.to_owned(),
                                    })
                                    .await
                                    .is_err()
                            {
                                return;
                            }
                        }
                        Some("response.output_item.added") | Some("response.output_item.done") => {
                            if let Some(index) = value.get("output_index").and_then(Value::as_u64)
                                && let Some(item) = value.get("item")
                                && item.get("type").and_then(Value::as_str) == Some("function_call")
                            {
                                let draft = drafts.entry(index).or_default();
                                update_tool_draft(draft, item);
                            }
                        }
                        Some("response.function_call_arguments.delta") => {
                            if let Some(index) = value.get("output_index").and_then(Value::as_u64)
                                && let Some(delta) = value.get("delta").and_then(Value::as_str)
                            {
                                drafts.entry(index).or_default().arguments.push_str(delta);
                            }
                        }
                        Some("response.function_call_arguments.done") => {
                            if let Some(index) = value.get("output_index").and_then(Value::as_u64)
                                && let Some(arguments) =
                                    value.get("arguments").and_then(Value::as_str)
                            {
                                drafts.entry(index).or_default().arguments = arguments.to_owned();
                            }
                        }
                        Some("response.completed") => {
                            terminal = true;
                            let response = value.get("response").unwrap_or(&Value::Null);
                            if response.get("status").and_then(Value::as_str) != Some("completed") {
                                send_failed(
                                    &out,
                                    operation_id,
                                    step,
                                    "provider completed event had a non-completed status",
                                )
                                .await;
                                break 'stream;
                            }
                            if let Some(usage) = response_usage(response)
                                && out
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
                            let completed_drafts = response_tool_drafts(response, drafts);
                            for (index, draft) in completed_drafts {
                                let arguments = if draft.arguments.trim().is_empty() {
                                    serde_json::json!({})
                                } else {
                                    match serde_json::from_str(&draft.arguments) {
                                        Ok(arguments) => arguments,
                                        Err(err) => {
                                            send_failed(
                                                &out,
                                                operation_id,
                                                step,
                                                &format!(
                                                    "malformed tool arguments for {}: {err}",
                                                    draft.name
                                                ),
                                            )
                                            .await;
                                            break 'stream;
                                        }
                                    }
                                };
                                if out
                                    .send(EngineSignal::ToolCallCompleted {
                                        operation_id,
                                        step,
                                        call: ToolCall {
                                            operation_id,
                                            call_id: index + 1,
                                            name: draft.name,
                                            arguments,
                                        },
                                    })
                                    .await
                                    .is_err()
                                {
                                    return;
                                }
                            }
                            if out
                                .send(EngineSignal::Completed { operation_id, step })
                                .await
                                .is_err()
                            {
                                return;
                            }
                            break 'stream;
                        }
                        Some("response.incomplete") => {
                            terminal = true;
                            send_failed(
                                &out,
                                operation_id,
                                step,
                                "provider returned an incomplete response",
                            )
                            .await;
                            break 'stream;
                        }
                        Some("response.failed") | Some("error") => {
                            terminal = true;
                            let message = provider_error_message(&value);
                            send_failed(&out, operation_id, step, &message).await;
                            break 'stream;
                        }
                        _ => {}
                    }
                }
                if terminal {
                    break;
                }
            }

            if !terminal {
                send_failed(
                    &out,
                    operation_id,
                    step,
                    "provider stream ended before a terminal response event",
                )
                .await;
            }
        }
    }
}

#[derive(Default)]
struct ToolDraft {
    name: String,
    arguments: String,
}

fn response_tool_drafts(
    response: &Value,
    mut drafts: BTreeMap<u64, ToolDraft>,
) -> BTreeMap<u64, ToolDraft> {
    if let Some(output) = response.get("output").and_then(Value::as_array) {
        for (index, item) in output.iter().enumerate() {
            if item.get("type").and_then(Value::as_str) != Some("function_call") {
                continue;
            }
            let draft = drafts.entry(index as u64).or_default();
            update_tool_draft(draft, item);
        }
    }
    drafts.retain(|_, draft| !draft.name.is_empty());
    drafts
}

fn update_tool_draft(draft: &mut ToolDraft, item: &Value) {
    if let Some(name) = item.get("name").and_then(Value::as_str) {
        draft.name = name.to_owned();
    }
    if let Some(arguments) = item.get("arguments").and_then(Value::as_str) {
        draft.arguments = arguments.to_owned();
    }
}

fn response_input(plan: &ContextPlan) -> Vec<Value> {
    let mut output = Vec::new();
    for message in &plan.messages {
        match message {
            ContextMessage::User { content } => output.push(serde_json::json!({
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": content }],
            })),
            ContextMessage::Assistant {
                content,
                tool_calls,
            } => {
                if !content.is_empty() {
                    output.push(serde_json::json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": content }],
                        "status": "completed",
                    }));
                }
                for call in tool_calls {
                    output.push(serde_json::json!({
                        "type": "function_call",
                        "call_id": format!("call_{}", call.call_id),
                        "name": call.name,
                        "arguments": call.arguments.to_string(),
                    }));
                }
            }
            ContextMessage::Tool {
                call_id,
                content,
                images,
            } => {
                // Pi parity: images ride the function_call_output as
                // input_image parts beside the text output.
                let mut output_parts = vec![serde_json::json!({
                    "type": "input_text",
                    "text": content,
                })];
                output_parts.extend(images.iter().map(|image| {
                    serde_json::json!({
                        "type": "input_image",
                        "detail": "auto",
                        "image_url": format!("data:{};base64,{}", image.mime_type, image.data),
                    })
                }));
                output.push(serde_json::json!({
                    "type": "function_call_output",
                    "call_id": format!("call_{call_id}"),
                    "output": output_parts,
                }));
            }
        }
    }
    output
}

fn response_usage(response: &Value) -> Option<TokenUsage> {
    let usage = response.get("usage")?;
    let input_total = usage.get("input_tokens")?.as_u64()?;
    let output = usage.get("output_tokens")?.as_u64()?;
    let details = usage.get("input_tokens_details");
    let cache_read = details
        .and_then(|details| details.get("cached_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cache_write = details
        .and_then(|details| details.get("cache_write_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Some(TokenUsage {
        input: input_total.saturating_sub(cache_read + cache_write),
        output,
        cache_read,
        cache_write,
    })
}

fn provider_error_message(value: &Value) -> String {
    let response = value.get("response").unwrap_or(value);
    let error = response.get("error").unwrap_or(response);
    let code = error.get("code").and_then(Value::as_str);
    let message = error.get("message").and_then(Value::as_str);
    match (code, message) {
        (Some(code), Some(message)) => format!("provider error {code}: {message}"),
        (None, Some(message)) => format!("provider error: {message}"),
        _ => "provider returned an error event".to_owned(),
    }
}

async fn send_failed(
    out: &mpsc::Sender<EngineSignal>,
    operation_id: ion_core::OperationId,
    step: u64,
    message: &str,
) {
    let _ = out
        .send(EngineSignal::Failed {
            operation_id,
            step,
            message: message.to_owned(),
        })
        .await;
}

fn codex_endpoint(base_url: &str) -> String {
    let normalized = base_url.trim_end_matches('/');
    if normalized.ends_with("/codex/responses") {
        normalized.to_owned()
    } else if normalized.ends_with("/codex") {
        format!("{normalized}/responses")
    } else {
        format!("{normalized}/codex/responses")
    }
}

fn find_line_end(buffer: &[u8]) -> Option<usize> {
    buffer.iter().position(|&byte| byte == b'\n')
}

fn pi_auth_path() -> Result<std::path::PathBuf, String> {
    if let Some(path) = std::env::var_os("ION_PI_AUTH") {
        return Ok(path.into());
    }
    let base = etcetera::base_strategy::choose_base_strategy()
        .map_err(|err| format!("cannot resolve the user home directory: {err}"))?;
    use etcetera::base_strategy::BaseStrategy;
    Ok(base.home_dir().join(".pi").join("agent").join("auth.json"))
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn parse_pi_credential(document: &str, now: u64) -> Result<CodexCredential, String> {
    let value: Value =
        serde_json::from_str(document).map_err(|err| format!("invalid JSON: {err}"))?;
    let credential = value
        .get("openai-codex")
        .ok_or_else(|| "no openai-codex credential is configured".to_owned())?;
    let access_token = credential
        .get("access")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "the openai-codex credential has no access token".to_owned())?;
    if let Some(expires) = credential.get("expires").and_then(Value::as_u64)
        && expires <= now
    {
        return Err(
            "the stored OAuth access token is expired; log in with Pi or set OPENAI_CODEX_ACCESS_TOKEN"
                .to_owned(),
        );
    }
    let account_id = credential
        .get("accountId")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| jwt_account_id(access_token))
        .ok_or_else(|| "the credential has no ChatGPT account id".to_owned())?;
    Ok(CodexCredential {
        access_token: access_token.to_owned(),
        account_id,
    })
}

fn jwt_account_id(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(payload))
        .ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    value
        .get("https://api.openai.com/auth")
        .and_then(|auth| auth.get("chatgpt_account_id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ion_core::{ContextMessage, ModelConfig, OperationId};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    fn spawn_sse_server(body: &'static str, captured: std::sync::mpsc::Sender<String>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept");
            let mut buffer = vec![0_u8; 32 * 1024];
            let length = socket.read(&mut buffer).expect("request");
            let body_start = String::from_utf8_lossy(&buffer[..length])
                .split("\r\n\r\n")
                .nth(1)
                .unwrap_or_default()
                .to_owned();
            let _ = captured.send(body_start);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).expect("response");
        });
        format!("http://127.0.0.1:{port}")
    }

    fn request() -> ProviderRequest {
        ProviderRequest {
            operation_id: OperationId::generate(),
            step: 4,
            model: ModelConfig {
                thinking: None,
                model_ref: "gpt-5.6-luna".to_owned(),
                context_window: Some(CODEX_CONTEXT_WINDOW),
                capabilities: ModelCapabilities {
                    reasoning: true,
                    ..ModelCapabilities::default()
                },
            },
            plan: ContextPlan {
                system: "system".to_owned(),
                messages: vec![
                    ContextMessage::User {
                        content: "hello".to_owned(),
                    },
                    ContextMessage::Assistant {
                        content: "working".to_owned(),
                        tool_calls: vec![ToolCall {
                            operation_id: OperationId::generate(),
                            call_id: 7,
                            name: "read".to_owned(),
                            arguments: serde_json::json!({"path":"Cargo.toml"}),
                        }],
                    },
                    ContextMessage::Tool {
                        call_id: 7,
                        content: "contents".to_owned(),
                        images: Vec::new(),
                    },
                ],
            },
            tools: vec![ToolSpec {
                name: "read".to_owned(),
                description: "Read a file".to_owned(),
                input_schema: serde_json::json!({"type":"object"}),
            }],
        }
    }

    async fn collect(provider: OpenAICodexProvider) -> Vec<EngineSignal> {
        let (tx, mut rx) = mpsc::channel(64);
        let cancel = CancellationToken::new();
        let handle = tokio::spawn(async move { provider.run(request(), cancel, tx).await });
        let mut signals = Vec::new();
        while let Ok(Some(signal)) = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            signals.push(signal);
        }
        handle.await.expect("provider task");
        signals
    }

    #[test]
    fn parses_pi_credential_without_printing_or_refreshing_it() {
        let credential = parse_pi_credential(
            r#"{"openai-codex":{"type":"oauth","access":"access-token","refresh":"refresh-token","expires":2000,"accountId":"account"}}"#,
            1000,
        )
        .expect("credential");
        assert_eq!(credential.access_token(), "access-token");
        assert_eq!(credential.account_id(), "account");
    }

    #[test]
    fn rejects_expired_pi_credential() {
        let result = parse_pi_credential(
            r#"{"openai-codex":{"access":"access-token","expires":1000,"accountId":"account"}}"#,
            1000,
        );
        assert!(result.is_err());
        assert!(result.err().is_some_and(|error| error.contains("expired")));
    }

    #[test]
    fn codex_endpoint_matches_pi_shape() {
        assert_eq!(
            codex_endpoint("https://chatgpt.com/backend-api"),
            "https://chatgpt.com/backend-api/codex/responses"
        );
        assert_eq!(
            codex_endpoint("http://localhost/codex"),
            "http://localhost/codex/responses"
        );
    }

    #[tokio::test]
    async fn sends_responses_payload_and_normalizes_stream() {
        let (captured_tx, captured_rx) = std::sync::mpsc::channel();
        let base_url = spawn_sse_server(
            "data: {\"type\":\"response.created\"}\n\n\
             data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"call_id\":\"fc_remote\",\"name\":\"read\"}}\n\n\
             data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"{\\\"path\\\":\\\"Cargo.toml\\\"}\"}\n\n\
             data: {\"type\":\"response.output_text.delta\",\"delta\":\"done\"}\n\n\
             data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"function_call\",\"call_id\":\"fc_remote\",\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"Cargo.toml\\\"}\"}],\"usage\":{\"input_tokens\":100,\"output_tokens\":20,\"input_tokens_details\":{\"cached_tokens\":60}}}}\n\n",
            captured_tx,
        );
        let provider =
            OpenAICodexProvider::new("gpt-5.6-luna", "token", "account").with_base_url(base_url);
        let signals = collect(provider).await;
        assert!(signals.iter().any(|signal| matches!(
            signal,
            EngineSignal::TextDelta { text, .. } if text == "done"
        )));
        assert!(signals.iter().any(|signal| matches!(
            signal,
            EngineSignal::ToolCallCompleted { call, .. }
                if call.name == "read" && call.arguments["path"] == "Cargo.toml"
        )));
        assert!(signals.iter().any(|signal| matches!(
            signal,
            EngineSignal::UsageUpdate { usage, .. }
                if usage.input == 40 && usage.cache_read == 60
        )));
        assert!(matches!(
            signals.last(),
            Some(EngineSignal::Completed { .. })
        ));

        let body: Value = serde_json::from_str(&captured_rx.recv().expect("body")).expect("json");
        assert_eq!(body["model"], "gpt-5.6-luna");
        assert_eq!(body["store"], false);
        assert_eq!(body["reasoning"]["effort"], "xhigh");
        assert_eq!(body["input"][0]["role"], "user");
        assert_eq!(body["input"][3]["type"], "function_call_output");
        assert_eq!(body["tools"][0]["type"], "function");
    }

    #[tokio::test]
    async fn stream_without_terminal_event_fails_instead_of_completing() {
        let (captured_tx, _captured_rx) = std::sync::mpsc::channel();
        let base_url = spawn_sse_server(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n",
            captured_tx,
        );
        let provider =
            OpenAICodexProvider::new("gpt-5.6-luna", "token", "account").with_base_url(base_url);
        let signals = collect(provider).await;
        assert!(matches!(
            signals.last(),
            Some(EngineSignal::Failed { message, .. })
                if message.contains("before a terminal response")
        ));
    }
}
