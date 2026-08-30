//! ACP frontend (DESIGN.md Step 8): exposes the session runtime over
//! the Agent Client Protocol v1 - newline-delimited JSON-RPC 2.0 on
//! stdio. This is an adapter, not a second runtime: every prompt turn
//! is a normal `submit` on the same `SessionHandle` the TUI and print
//! mode use, with the same streaming, cancellation, and durability
//! semantics.
//!
//! Supported v1 surface: `initialize`, `session/new`, `session/load`,
//! `session/prompt` (`session/update` streaming), and `session/cancel`.
//! Loading restores Ion's durable runtime and replays its semantic history;
//! it does not create a second transcript or mutation path.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::Mutex;
use tokio::task::JoinSet;

use ion_core::{
    EventSubscription, IndeterminateWarning, OperationId, OperationOutcome, OperationSettlement,
    OperationStatus, Provider, Runtime, RuntimeError, RuntimeEvent, SessionHandle, SessionStore,
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

struct AcpSession<P> {
    handle: ion_core::SessionHandle,
    runtime: Runtime,
    catalog: ToolCatalog,
    agent_host: crate::AgentHost<P>,
    /// The in-flight prompt turn, if any: (JSON-RPC id, operation id).
    active_prompt: Arc<Mutex<Option<(Value, OperationId)>>>,
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
    let mut sessions: HashMap<String, AcpSession<P>> = HashMap::new();
    let mut prompt_tasks = JoinSet::new();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];

    'read: loop {
        while let Some(result) = prompt_tasks.try_join_next() {
            if let Err(err) = result {
                tracing::warn!(error = %err, "ACP prompt task failed");
            }
        }
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
                            "agentCapabilities": { "loadSession": true },
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
            Some("session/load") => {
                let already_loaded = params
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .is_some_and(|session_id| sessions.contains_key(session_id));
                if already_loaded {
                    error_response(&output, id, -32002, "session is already loaded").await;
                } else {
                    match session_load(&config, &params).await {
                        Ok((session_id_string, session, history)) => {
                            for update_payload in history {
                                update(&output, &session_id_string, update_payload).await;
                            }
                            sessions.insert(session_id_string, session);
                            write(
                                &output,
                                json!({
                                    "jsonrpc": "2.0",
                                    "id": id,
                                    "result": {},
                                }),
                            )
                            .await;
                        }
                        Err(err) => {
                            error_response(&output, id, -32000, &err).await;
                        }
                    }
                }
            }
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
                let Some(request_id) = id.clone() else {
                    continue;
                };
                // Subscribe before submit: recovery events are live-only.
                let (_snapshot, events) = session.handle.subscribe().await.expect("subscribe");
                match session.handle.submit_if_idle(text).await {
                    Ok(operation_id) => {
                        let active_prompt = Arc::clone(&session.active_prompt);
                        active_prompt
                            .lock()
                            .await
                            .replace((request_id.clone(), operation_id));
                        let output = Arc::clone(&output);
                        let handle = session.handle.clone();
                        let session_id = session_id.to_owned();
                        prompt_tasks.spawn(async move {
                            let stop =
                                pump_turn(events, handle, operation_id, &session_id, &output).await;
                            finish_prompt(&output, Some(request_id), stop, active_prompt).await;
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
                    .get(session_id)
                    .map(|s| Arc::clone(&s.active_prompt));
                let handle = sessions.get(session_id).map(|s| &s.handle);
                if let (Some(active), Some(handle)) = (active, handle) {
                    let operation_id = active.lock().await.as_ref().map(|(_, id)| *id);
                    if let Some(operation_id) = operation_id {
                        let _ = handle.cancel(operation_id).await;
                    }
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

    let mut shutdown_error = None;
    for (_, session) in sessions.drain() {
        if let Err(err) = session.agent_host.close().await {
            tracing::error!(error = %err, "failed to close ACP agent host");
            shutdown_error.get_or_insert_with(|| format!("ACP agent close failed: {err}"));
        }
        if let Err(err) = session.handle.close().await {
            tracing::error!(error = %err, "failed to close an ACP session at shutdown");
            shutdown_error.get_or_insert_with(|| format!("ACP session close failed: {err}"));
        }
        if let Err(err) = session.runtime.join().await {
            tracing::error!(error = %err, "failed to join an ACP session at shutdown");
            shutdown_error.get_or_insert_with(|| format!("ACP runtime join failed: {err}"));
        }
        if let Err(err) = session.catalog.close().await {
            tracing::error!(error = %err, "failed to close an ACP tool catalog at shutdown");
            shutdown_error.get_or_insert_with(|| format!("ACP tool catalog close failed: {err}"));
        }
    }
    prompt_tasks.shutdown().await;
    match shutdown_error {
        Some(err) => Err(std::io::Error::other(err)),
        None => Ok(()),
    }
}

/// Outcome of one pumped prompt turn.
enum TurnStop {
    EndTurn,
    Cancelled,
    Failed(String),
    ApprovalRequired(String),
}

const LAGGED_UPDATES_MESSAGE: &str = "event stream lagged; updates incomplete";

fn stop_from_settlement(
    settlement: &OperationSettlement,
    indeterminate: Option<&IndeterminateWarning>,
    updates_incomplete: bool,
) -> TurnStop {
    match &settlement.outcome {
        OperationOutcome::Completed if updates_incomplete => {
            TurnStop::Failed(LAGGED_UPDATES_MESSAGE.to_owned())
        }
        OperationOutcome::Completed => TurnStop::EndTurn,
        OperationOutcome::Failed(message) => TurnStop::Failed(message.clone()),
        OperationOutcome::Cancelled => TurnStop::Cancelled,
        OperationOutcome::Indeterminate => {
            let message = indeterminate
                .filter(|warning| warning.operation_id == settlement.operation_id)
                .map_or("external effect outcome is indeterminate", |warning| {
                    warning.message.as_str()
                });
            TurnStop::Failed(format!("indeterminate operation: {message}"))
        }
        OperationOutcome::ApprovalRequired { tool } => TurnStop::ApprovalRequired(tool.clone()),
    }
}

fn stop_after_resubscribe(
    snapshot: &ion_core::SessionSnapshot,
    operation_id: OperationId,
    updates_incomplete: bool,
) -> Result<Option<TurnStop>, String> {
    if let Some(settlement) = snapshot
        .latest_settlement
        .as_ref()
        .filter(|settlement| settlement.operation_id == operation_id)
    {
        return Ok(Some(stop_from_settlement(
            settlement,
            snapshot.indeterminate.as_ref(),
            updates_incomplete,
        )));
    }
    match &snapshot.operation {
        OperationStatus::Active {
            operation_id: active,
            ..
        } if *active == operation_id => Ok(None),
        OperationStatus::Active { .. } => {
            Err("event stream resynchronized to a different active operation".to_owned())
        }
        OperationStatus::Idle => {
            Err("event stream lagged; operation settlement unavailable".to_owned())
        }
    }
}

/// Stream one operation's events as ACP updates; returns only when the durable
/// operation settles. Lag marks streamed presentation as incomplete, but never
/// detaches the prompt task from still-running work.
async fn pump_turn<W>(
    mut events: EventSubscription,
    session: SessionHandle,
    operation_id: OperationId,
    session_id: &str,
    output: &Arc<Mutex<W>>,
) -> TurnStop
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    let mut updates_incomplete = false;
    loop {
        let event = match events.recv().await {
            Ok(event) => event,
            Err(RuntimeError::SubscriptionLagged) => {
                updates_incomplete = true;
                let (snapshot, fresh) = match session.subscribe().await {
                    Ok(subscription) => subscription,
                    Err(err) => {
                        return TurnStop::Failed(format!(
                            "event stream lagged and resubscribe failed: {err}"
                        ));
                    }
                };
                events = fresh;
                match stop_after_resubscribe(&snapshot, operation_id, updates_incomplete) {
                    Ok(Some(stop)) => return stop,
                    Ok(None) => continue,
                    Err(message) => return TurnStop::Failed(message),
                }
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
            RuntimeEvent::ToolProgress { .. } | RuntimeEvent::UsageUpdate { .. } => {}
            RuntimeEvent::OperationFinished { .. } => {
                return if updates_incomplete {
                    TurnStop::Failed(LAGGED_UPDATES_MESSAGE.to_owned())
                } else {
                    TurnStop::EndTurn
                };
            }
            RuntimeEvent::OperationCancelled { .. } => return TurnStop::Cancelled,
            RuntimeEvent::OperationFailed { message, .. } => {
                return TurnStop::Failed(message);
            }
            RuntimeEvent::OperationIndeterminate { message, .. } => {
                return TurnStop::Failed(format!("indeterminate operation: {message}"));
            }
            RuntimeEvent::OperationApprovalRequired { tool, .. } => {
                return TurnStop::ApprovalRequired(tool);
            }
            // This host is non-interactive, so a parked approval cannot
            // occur; surface it the same way if it ever does.
            RuntimeEvent::ApprovalPending { tool, .. } => {
                return TurnStop::ApprovalRequired(tool);
            }
            RuntimeEvent::OperationStarted { .. } | RuntimeEvent::SessionClosed { .. } => {}
        }
    }
}

async fn finish_prompt<W>(
    output: &Arc<Mutex<W>>,
    id: Option<Value>,
    stop: TurnStop,
    active_prompt: Arc<Mutex<Option<(Value, OperationId)>>>,
) where
    W: AsyncWrite + Unpin + Send + 'static,
{
    active_prompt.lock().await.take();
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
}

/// Create a runtime-backed ACP session from `session/new` params:
/// cwd-scoped tools plus any MCP servers the client supplies.
async fn session_new<P>(
    config: &AcpConfig<P>,
    params: &Value,
) -> Result<(String, AcpSession<P>), String>
where
    P: Provider + 'static,
{
    let (cwd, catalog, trusted_resources) = session_tools(config, params).await?;
    let runtime = Runtime::start_with_policy_and_resources_in_cwd(
        (config.make_provider)(),
        catalog.clone(),
        (*config.store).clone(),
        Arc::clone(&config.policy),
        trusted_resources.clone(),
        cwd,
    );
    let session_id = runtime.session_id();
    let session = attach_session(config, &catalog, runtime, trusted_resources).await?;
    Ok((session_id.to_string(), session))
}

/// Shared ACP session setup from params: validate the absolute cwd, build
/// the cwd-scoped tool catalog, start client-supplied MCP servers, and
/// resolve the host's trusted project resources.
async fn session_tools<P>(
    config: &AcpConfig<P>,
    params: &Value,
) -> Result<(String, ToolCatalog, Vec<ion_core::TrustedResource>), String>
where
    P: Provider,
{
    let cwd = params
        .get("cwd")
        .and_then(|v| v.as_str())
        .ok_or("missing cwd")?
        .to_owned();
    let cwd_path = std::path::Path::new(&cwd);
    if !cwd_path.is_absolute() {
        return Err("cwd must be absolute".to_owned());
    }
    let catalog = ToolCatalog::with_cwd(cwd_path);
    let servers = parse_mcp_servers(params);
    if !servers.is_empty() {
        ion_core::McpService::new()
            .start_into(&servers, &catalog)
            .await;
    }
    let trusted_resources = ion_core::load_trusted_resources(cwd_path, config.trust_project)?;
    Ok((cwd, catalog, trusted_resources))
}

/// Attach the root runtime's single agent host, then wrap it in ACP state.
async fn attach_session<P>(
    config: &AcpConfig<P>,
    catalog: &ToolCatalog,
    runtime: Runtime,
    trusted_resources: Vec<ion_core::TrustedResource>,
) -> Result<AcpSession<P>, String>
where
    P: Provider + 'static,
{
    let agent_host = match crate::enable_agent_host(
        catalog,
        &runtime,
        &config.store,
        Arc::clone(&config.make_provider),
        trusted_resources,
        4,
    )
    .await
    {
        Ok(host) => host,
        Err(err) => {
            let handle = runtime.session();
            let _ = handle.close().await;
            let _ = runtime.join().await;
            return Err(format!("attach agent host: {err}"));
        }
    };
    Ok(AcpSession {
        handle: runtime.session(),
        runtime,
        catalog: catalog.clone(),
        agent_host,
        active_prompt: Arc::new(Mutex::new(None)),
    })
}

/// Load one durable Ion session and return the semantic history that ACP
/// clients need to reconstruct their presentation.
async fn session_load<P>(
    config: &AcpConfig<P>,
    params: &Value,
) -> Result<(String, AcpSession<P>, Vec<Value>), String>
where
    P: Provider + 'static,
{
    let session_id_text = params
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or("missing sessionId")?;
    let session_id = parse_session_id(session_id_text)?;
    let (cwd, catalog, trusted_resources) = session_tools(config, params).await?;
    let loaded = config
        .store
        .load(session_id)
        .await
        .map_err(|err| format!("load session: {err}"))?;
    if loaded.session.cwd != cwd {
        return Err(format!(
            "cwd does not match the persisted session ({})",
            loaded.session.cwd
        ));
    }

    let runtime = Runtime::open_session_with_resources(
        (config.make_provider)(),
        catalog.clone(),
        (*config.store).clone(),
        session_id,
        Arc::clone(&config.policy),
        trusted_resources.clone(),
    )
    .await
    .map_err(|err| format!("open session: {err}"))?;
    let history = replay_history(&loaded.entries);
    let session = attach_session(config, &catalog, runtime, trusted_resources).await?;
    Ok((session_id_text.to_owned(), session, history))
}

fn parse_mcp_servers(params: &Value) -> Vec<ion_core::ServerDef> {
    params
        .get("mcpServers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|server| {
            let name = server.get("name").and_then(Value::as_str)?;
            let command = server.get("command").and_then(Value::as_str)?;
            Some(ion_core::ServerDef {
                name: name.to_owned(),
                command: command.to_owned(),
                args: server
                    .get("args")
                    .and_then(Value::as_array)
                    .map(|args| {
                        args.iter()
                            .filter_map(|arg| arg.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default(),
            })
        })
        .collect()
}

fn parse_session_id(value: &str) -> Result<ion_core::SessionId, String> {
    value
        .strip_prefix("session-")
        .and_then(ion_core::SessionId::parse)
        .ok_or_else(|| format!("invalid sessionId {value:?}"))
}

fn replay_history(entries: &[ion_core::EntryRecord]) -> Vec<Value> {
    let results: HashMap<u64, &ion_core::ToolResult> = entries
        .iter()
        .filter_map(|record| match &record.entry {
            ion_core::SessionEntry::ToolResult { result } => Some((result.call_id(), result)),
            _ => None,
        })
        .collect();
    let mut updates = Vec::new();
    for record in entries {
        match &record.entry {
            ion_core::SessionEntry::UserMessage { text } => updates.push(json!({
                "sessionUpdate": "user_message_chunk",
                "content": { "type": "text", "text": text },
            })),
            ion_core::SessionEntry::AgentMessage { from, text } => updates.push(json!({
                "sessionUpdate": "user_message_chunk",
                "content": {
                    "type": "text",
                    "text": format!("[Message from {from}]\n{text}"),
                },
            })),
            ion_core::SessionEntry::AssistantMessage { text } => updates.push(json!({
                "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": text },
            })),
            ion_core::SessionEntry::Compaction { summary, .. } => updates.push(json!({
                "sessionUpdate": "agent_message_chunk",
                "content": {
                    "type": "text",
                    "text": format!("[Context summary]\n{summary}"),
                },
            })),
            ion_core::SessionEntry::ToolCall { call } => {
                let result = results.get(&call.call_id);
                updates.push(json!({
                    "sessionUpdate": "tool_call",
                    "toolCallId": call.call_id.to_string(),
                    "title": call.name,
                    "kind": tool_kind(&call.name),
                    "status": result.map_or("pending", |result| {
                        if result.is_ok() { "completed" } else { "failed" }
                    }),
                    "rawInput": call.arguments,
                }));
                if let Some(result) = result {
                    updates.push(json!({
                        "sessionUpdate": "tool_call_update",
                        "toolCallId": call.call_id.to_string(),
                        "status": if result.is_ok() { "completed" } else { "failed" },
                        "rawOutput": result.model_text(),
                    }));
                }
            }
            ion_core::SessionEntry::ToolResult { .. } => {}
        }
    }
    updates
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

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::time::Duration;

    use ion_core::{EngineSignal, ProviderRequest, ToolRegistry};
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    use super::*;

    #[derive(Clone, Copy)]
    struct BurstProvider {
        wait_for_cancel: bool,
    }

    impl Provider for BurstProvider {
        fn run(
            &self,
            request: ProviderRequest,
            cancel: CancellationToken,
            out: mpsc::Sender<EngineSignal>,
        ) -> impl Future<Output = ()> + Send {
            let wait_for_cancel = self.wait_for_cancel;
            async move {
                for _ in 0..96 {
                    if out
                        .send(EngineSignal::TextDelta {
                            operation_id: request.operation_id,
                            step: request.step,
                            text: "x".to_owned(),
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                if wait_for_cancel {
                    cancel.cancelled().await;
                    let _ = out
                        .send(EngineSignal::Cancelled {
                            operation_id: request.operation_id,
                            step: request.step,
                        })
                        .await;
                } else {
                    let _ = out
                        .send(EngineSignal::Completed {
                            operation_id: request.operation_id,
                            step: request.step,
                        })
                        .await;
                }
            }
        }
    }

    async fn wait_for_snapshot(
        session: &SessionHandle,
        predicate: impl Fn(&ion_core::SessionSnapshot) -> bool,
    ) -> ion_core::SessionSnapshot {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let snapshot = session.snapshot().await.expect("snapshot");
                if predicate(&snapshot) {
                    break snapshot;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("snapshot condition timed out")
    }

    #[tokio::test]
    async fn lagged_pump_classifies_missed_completion_from_snapshot() {
        let runtime = Runtime::start_with_store(
            BurstProvider {
                wait_for_cancel: false,
            },
            ToolRegistry::default(),
            SessionStore::open_in_memory().expect("store"),
        );
        let session = runtime.session();
        let (_snapshot, events) = session.subscribe().await.expect("subscribe");
        let operation_id = session.submit_if_idle("go").await.expect("submit");
        let snapshot = wait_for_snapshot(&session, |snapshot| {
            snapshot
                .latest_settlement
                .as_ref()
                .is_some_and(|settlement| {
                    settlement.operation_id == operation_id
                        && settlement.outcome == OperationOutcome::Completed
                })
        })
        .await;
        assert_eq!(snapshot.operation, OperationStatus::Idle);

        let output = Arc::new(Mutex::new(tokio::io::sink()));
        let stop = pump_turn(
            events,
            session.clone(),
            operation_id,
            "session-test",
            &output,
        )
        .await;
        assert!(matches!(
            stop,
            TurnStop::Failed(message) if message == LAGGED_UPDATES_MESSAGE
        ));

        session.close().await.expect("close");
        runtime.join().await.expect("join");
    }

    #[test]
    fn resubscribe_prefers_target_settlement_over_newer_active_operation() {
        let target = OperationId::generate();
        let newer = OperationId::generate();
        let snapshot = ion_core::SessionSnapshot {
            cursor: ion_core::RuntimeCursor::default(),
            runtime_instance_id: ion_core::RuntimeInstanceId::generate(),
            indeterminate: None,
            latest_settlement: Some(OperationSettlement {
                operation_id: target,
                outcome: OperationOutcome::Completed,
            }),
            operation: OperationStatus::Active {
                operation_id: newer,
                prompt: "next turn".to_owned(),
                state: ion_core::OperationState::NeedAssistant,
            },
            entries: Vec::new(),
            reopen_entry_count: None,
            model_ref: "test-model".to_owned(),
            latest_usage: None,
            live: None,
        };
        let stop = stop_after_resubscribe(&snapshot, target, true)
            .expect("target settlement is authoritative")
            .expect("settled target stops the pump");
        assert!(matches!(
            stop,
            TurnStop::Failed(message) if message == LAGGED_UPDATES_MESSAGE
        ));
    }

    #[tokio::test]
    async fn lagged_pump_stays_attached_and_cancellable() {
        let runtime = Runtime::start_with_store(
            BurstProvider {
                wait_for_cancel: true,
            },
            ToolRegistry::default(),
            SessionStore::open_in_memory().expect("store"),
        );
        let session = runtime.session();
        let (_snapshot, events) = session.subscribe().await.expect("subscribe");
        let operation_id = session.submit_if_idle("go").await.expect("submit");
        wait_for_snapshot(&session, |snapshot| {
            matches!(
                (&snapshot.operation, &snapshot.live),
                (
                    OperationStatus::Active { operation_id: active, .. },
                    Some(live)
                ) if *active == operation_id && live.draft_text.len() == 96
            )
        })
        .await;

        let output = Arc::new(Mutex::new(tokio::io::sink()));
        let pump_output = Arc::clone(&output);
        let pump_session = session.clone();
        let pump = tokio::spawn(async move {
            pump_turn(
                events,
                pump_session,
                operation_id,
                "session-test",
                &pump_output,
            )
            .await
        });
        tokio::task::yield_now().await;
        assert!(!pump.is_finished(), "lag must not detach the prompt task");

        session.cancel(operation_id).await.expect("cancel");
        let stop = tokio::time::timeout(Duration::from_secs(2), pump)
            .await
            .expect("pump timed out")
            .expect("pump task");
        assert!(matches!(stop, TurnStop::Cancelled));

        session.close().await.expect("close");
        runtime.join().await.expect("join");
    }
}
