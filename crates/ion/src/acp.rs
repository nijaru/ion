//! ACP frontend (DESIGN.md Step 8): exposes the session runtime over
//! the Agent Client Protocol v1 - newline-delimited JSON-RPC 2.0 on
//! stdio. This is an adapter, not a second runtime: every prompt turn
//! is a normal `submit` on the same `SessionHandle` the TUI and print
//! mode use, with the same streaming, cancellation, and durability
//! semantics.
//!
//! Supported v1 surface: `initialize`, `session/new`, `session/prompt`
//! (`session/update` streaming), and `session/cancel`. Session load/
//! resume over ACP is deferred: ion persists sessions in its own
//! store; replaying them as ACP updates is additional frontend
//! surface, not new runtime capability.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::Mutex;

use ion_core::{
    EventSubscription, OperationId, Provider, Runtime, RuntimeError, RuntimeEvent, SessionStore,
    ToolCatalog,
};

/// The ACP major version this adapter speaks (v1 is the stable spec;
/// v2 is a draft).
const PROTOCOL_VERSION: u64 = 1;

/// One connected client's agent state: shared writer plus one runtime
/// per ACP session. The provider is created per session via the
/// factory (providers own step cursors, so they are not shared).
pub struct AcpConfig<P> {
    pub make_provider: Arc<dyn Fn() -> P + Send + Sync>,
    pub store: Arc<SessionStore>,
    pub policy: Arc<dyn PolicyEngine>,
    pub trust_project: bool,
}

use ion_core::PolicyEngine;

struct AcpSession {
    handle: ion_core::SessionHandle,
    #[allow(dead_code)] // owns the runtime task until process exit
    runtime: Runtime,
    /// The in-flight prompt turn, if any: (JSON-RPC id, operation id).
    active_prompt: Option<(Value, OperationId)>,
}

/// Serve ACP over `input`/`output` until the peer disconnects.
pub async fn serve<P, R, W>(input: R, output: W, config: AcpConfig<P>) -> std::io::Result<()>
where
    P: Provider,
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let output = Arc::new(Mutex::new(output));
    let mut input = input;
    let mut sessions: HashMap<String, AcpSession> = HashMap::new();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];

    'read: loop {
        let Some(line_end) = find_line(&buf) else {
            let read = input.read(&mut chunk).await?;
            if read == 0 {
                break 'read;
            }
            buf.extend_from_slice(&chunk[..read]);
            continue;
        };
        let line: Vec<u8> = buf.drain(..=line_end).collect();
        let Ok(text) = std::str::from_utf8(&line) else {
            continue;
        };
        let Ok(message) = serde_json::from_str::<Value>(text.trim()) else {
            continue;
        };
        let method = message.get("method").and_then(|v| v.as_str());
        let id = message.get("id").cloned();
        let params = message.get("params").cloned().unwrap_or(json!({}));

        match method {
            Some("initialize") => {
                write(
                    &output,
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "protocolVersion": PROTOCOL_VERSION,
                            "agentCapabilities": { "loadSession": false },
                            "agentInfo": {
                                "name": "ion",
                                "version": env!("CARGO_PKG_VERSION"),
                            },
                            "authMethods": [],
                        },
                    }),
                )
                .await;
            }
            Some("session/new") => match session_new(&config, &params).await {
                Ok((session_id_string, session)) => {
                    sessions.insert(session_id_string.clone(), session);
                    write(
                        &output,
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": { "sessionId": session_id_string },
                        }),
                    )
                    .await;
                }
                Err(err) => {
                    error_response(&output, id, -32000, &err).await;
                }
            },
            Some("session/prompt") => {
                let Some(session_id) = params.get("sessionId").and_then(|v| v.as_str()) else {
                    error_response(&output, id, -32602, "missing sessionId").await;
                    continue;
                };
                let Some(session) = sessions.get_mut(session_id) else {
                    error_response(&output, id, -32001, "unknown session").await;
                    continue;
                };
                let text = prompt_text(&params);
                let Some(text) = text else {
                    error_response(
                        &output,
                        id,
                        -32602,
                        "prompt must contain at least one text block",
                    )
                    .await;
                    continue;
                };
                // Subscribe before submit: recovery events are live-only.
                let (_snapshot, events) = session.handle.subscribe().await.expect("subscribe");
                match session.handle.submit(text).await {
                    Ok(operation_id) => {
                        session.active_prompt = Some((id.clone().expect("request"), operation_id));
                        let output = Arc::clone(&output);
                        let session_id = session_id.to_owned();
                        tokio::spawn(async move {
                            let stop = pump_turn(events, operation_id, &session_id, &output).await;
                            finish_prompt(&output, id, session_id, stop).await;
                        });
                    }
                    Err(err) => {
                        error_response(&output, id, -32000, &format!("submit failed: {err}")).await;
                    }
                }
            }
            Some("session/cancel") => {
                let Some(session_id) = params.get("sessionId").and_then(|v| v.as_str()) else {
                    continue;
                };
                // The pump observes OperationCancelled and answers
                // session/prompt with stopReason "cancelled".
                let active = sessions
                    .get_mut(session_id)
                    .and_then(|s| s.active_prompt.take());
                let handle = sessions.get(session_id).map(|s| &s.handle);
                if let (Some((_, operation_id)), Some(handle)) = (active, handle) {
                    let _ = handle.cancel(operation_id).await;
                }
            }
            _ => {
                if id.is_some() {
                    error_response(
                        &output,
                        id,
                        -32601,
                        &format!("method not supported: {}", method.unwrap_or("?")),
                    )
                    .await;
                }
            }
        }
    }

    for (_, session) in sessions.drain() {
        let _ = session.handle.close().await;
    }
    Ok(())
}

/// Outcome of one pumped prompt turn.
enum TurnStop {
    EndTurn,
    Cancelled,
    Failed(String),
    ApprovalRequired(String),
}

/// Stream one operation's events as ACP updates; returns at the first
/// terminal event.
async fn pump_turn<W>(
    mut events: EventSubscription,
    operation_id: OperationId,
    session_id: &str,
    output: &Arc<Mutex<W>>,
) -> TurnStop
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    loop {
        let event = match events.recv().await {
            Ok(event) => event,
            Err(RuntimeError::SubscriptionLagged) => {
                return TurnStop::Failed("event stream lagged; updates incomplete".to_owned());
            }
            Err(_) => return TurnStop::Failed("event stream closed".to_owned()),
        };
        if event.operation_id() != Some(operation_id) {
            continue;
        }
        match event {
            RuntimeEvent::AssistantTextDelta { text, .. } => {
                update(
                    output,
                    session_id,
                    json!({
                        "sessionUpdate": "agent_message_chunk",
                        "content": { "type": "text", "text": text },
                    }),
                )
                .await;
            }
            RuntimeEvent::ThinkingDelta { text, .. } => {
                update(
                    output,
                    session_id,
                    json!({
                        "sessionUpdate": "agent_thought_chunk",
                        "content": { "type": "text", "text": text },
                    }),
                )
                .await;
            }
            RuntimeEvent::ToolStarted { call_id, tool, .. } => {
                update(
                    output,
                    session_id,
                    json!({
                        "sessionUpdate": "tool_call",
                        "toolCallId": call_id.to_string(),
                        "title": tool,
                        "kind": tool_kind(&tool),
                        "status": "pending",
                    }),
                )
                .await;
            }
            RuntimeEvent::ToolSettled {
                call_id, is_error, ..
            } => {
                update(
                    output,
                    session_id,
                    json!({
                        "sessionUpdate": "tool_call_update",
                        "toolCallId": call_id.to_string(),
                        "status": if is_error { "failed" } else { "completed" },
                    }),
                )
                .await;
            }
            RuntimeEvent::OperationFinished { .. } => return TurnStop::EndTurn,
            RuntimeEvent::OperationCancelled { .. } => return TurnStop::Cancelled,
            RuntimeEvent::OperationFailed { message, .. } => {
                return TurnStop::Failed(message);
            }
            RuntimeEvent::OperationApprovalRequired { tool, .. } => {
                return TurnStop::ApprovalRequired(tool);
            }
            RuntimeEvent::OperationStarted { .. } | RuntimeEvent::SessionClosed { .. } => {}
        }
    }
}

async fn finish_prompt<W>(
    output: &Arc<Mutex<W>>,
    id: Option<Value>,
    session_id: String,
    stop: TurnStop,
) where
    W: AsyncWrite + Unpin + Send + 'static,
{
    match stop {
        TurnStop::EndTurn => {
            write(
                output,
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "stopReason": "end_turn" },
                }),
            )
            .await;
        }
        TurnStop::Cancelled => {
            // Cancellations are not errors (spec): answer with the
            // meaningful stop reason so clients confirm cleanly.
            write(
                output,
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "stopReason": "cancelled" },
                }),
            )
            .await;
        }
        TurnStop::Failed(message) => {
            error_response(output, id, -32000, &message).await;
        }
        TurnStop::ApprovalRequired(tool) => {
            error_response(
                output,
                id,
                -32002,
                &format!(
                    "operation requires approval for `{tool}`; grant it via --allow or settings"
                ),
            )
            .await;
        }
    }
    let _ = session_id;
}

/// Create a runtime-backed ACP session from `session/new` params:
/// cwd-scoped tools plus any MCP servers the client supplies.
async fn session_new<P>(
    config: &AcpConfig<P>,
    params: &Value,
) -> Result<(String, AcpSession), String>
where
    P: Provider,
{
    let cwd = params
        .get("cwd")
        .and_then(|v| v.as_str())
        .ok_or("missing cwd")?;
    let catalog = ToolCatalog::with_cwd(std::path::PathBuf::from(cwd));
    let servers: Vec<ion_core::ServerDef> = params
        .get("mcpServers")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|server| {
            let name = server.get("name").and_then(|v| v.as_str())?;
            let command = server.get("command").and_then(|v| v.as_str())?;
            Some(ion_core::ServerDef {
                name: name.to_owned(),
                command: command.to_owned(),
                args: server
                    .get("args")
                    .and_then(|v| v.as_array())
                    .map(|args| {
                        args.iter()
                            .filter_map(|a| a.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default(),
            })
        })
        .collect();
    if !servers.is_empty() {
        ion_core::McpService::new()
            .start_into(&servers, &catalog)
            .await;
    }
    let trusted_resources =
        ion_core::load_trusted_resources(std::path::Path::new(cwd), config.trust_project)?;
    let runtime = Runtime::start_with_policy_and_resources(
        (config.make_provider)(),
        catalog.clone(),
        (*config.store).clone(),
        Arc::clone(&config.policy),
        trusted_resources.clone(),
    );
    let session_id = runtime.session_id();
    // ACP sessions can delegate to bounded read-only children (§20).
    let factory = Arc::clone(&config.make_provider);
    crate::enable_children(
        &catalog,
        &config.store,
        Arc::new(move || factory()),
        session_id,
        trusted_resources,
    );
    let session_id_string = session_id.to_string();
    let handle = runtime.session();
    Ok((
        session_id_string,
        AcpSession {
            handle,
            runtime,
            active_prompt: None,
        },
    ))
}

/// Join the prompt's text blocks; non-text content needs capabilities
/// ion does not advertise, so it is rejected upstream of the model.
fn prompt_text(params: &Value) -> Option<String> {
    let blocks = params.get("prompt")?.as_array()?;
    let mut texts = Vec::new();
    for block in blocks {
        if block.get("type").and_then(|v| v.as_str()) == Some("text") {
            texts.push(block.get("text")?.as_str()?.to_owned());
        }
    }
    if texts.is_empty() {
        None
    } else {
        Some(texts.join("\n"))
    }
}

fn tool_kind(tool: &str) -> &'static str {
    match tool {
        "bash" => "execute",
        "read" | "search" | "find" => "read",
        "write" | "edit" => "edit",
        _ => "other",
    }
}

async fn write<W: AsyncWrite + Unpin>(output: &Arc<Mutex<W>>, message: Value) {
    let mut out = output.lock().await;
    let line = message.to_string();
    let _ = out.write_all(line.as_bytes()).await;
    let _ = out.write_all(b"\n").await;
    let _ = out.flush().await;
}

async fn update<W: AsyncWrite + Unpin>(output: &Arc<Mutex<W>>, session_id: &str, update: Value) {
    write(
        output,
        json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": { "sessionId": session_id, "update": update },
        }),
    )
    .await;
}

async fn error_response<W: AsyncWrite + Unpin>(
    output: &Arc<Mutex<W>>,
    id: Option<Value>,
    code: i64,
    message: &str,
) {
    write(
        output,
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message },
        }),
    )
    .await;
}

/// Find the end of the first complete line in `buf`, if any.
fn find_line(buf: &[u8]) -> Option<usize> {
    buf.iter().position(|&b| b == b'\n')
}
