//! Shared MCP client transport for subprocess capability peers.
//!
//! MCP servers and Ion extensions both publish tools over stdio. The
//! protocol lifecycle and JSON-RPC ownership belong to the official `rmcp`
//! client; Ion owns only the peer definition and the adapter to its local
//! [`crate::tool::Tool`] contract.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rmcp::ServiceExt;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, ClientCapabilities, ClientInfo, ContentBlock,
    Implementation, ProtocolVersion,
};
use rmcp::service::{Peer, RoleClient};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use serde_json::Value;
use tokio::process::Command;
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::tool::{CatalogService, Tool, ToolSpec};

pub(crate) type CloseHandler = Arc<dyn Fn() + Send + Sync + 'static>;

/// A live connection to one stdio MCP subprocess.
pub(crate) struct StdioRpc {
    client: Peer<RoleClient>,
    shutdown: CancellationToken,
    closed: Arc<AtomicBool>,
    monitor: std::sync::Mutex<Option<JoinHandle<()>>>,
}

/// Definition of one subprocess peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PeerDef {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
}

/// Ion's shared MCP client identity and negotiated legacy protocol version.
pub(crate) fn client_info() -> ClientInfo {
    ClientInfo::new(
        ClientCapabilities::default(),
        Implementation::new("ion", env!("CARGO_PKG_VERSION")),
    )
    .with_protocol_version(ProtocolVersion::V_2025_11_25)
}

impl StdioRpc {
    /// Spawn the peer and perform the official MCP initialize handshake.
    /// Dropping a failed or completed transport delegates process cleanup to
    /// `rmcp`'s child-process transport.
    pub(crate) async fn spawn(
        def: &PeerDef,
        client_info: ClientInfo,
        timeout: Duration,
        on_closed: CloseHandler,
    ) -> Result<Self, String> {
        let command = Command::new(&def.command).configure(|command| {
            command.args(&def.args);
        });
        let transport = TokioChildProcess::new(command)
            .map_err(|err| format!("spawn {}: {err}", def.command))?;
        let client = tokio::time::timeout(timeout, client_info.serve(transport))
            .await
            .map_err(|_| format!("initialize {} timed out", def.command))?
            .map_err(|err| format!("initialize {}: {err}", def.command))?;

        let version = client
            .peer_info()
            .map(|info| info.protocol_version.to_string())
            .unwrap_or_else(|| "?".to_owned());
        tracing::debug!(server = %def.name, %version, "subprocess peer initialized");

        let peer = client.peer().clone();
        let shutdown = CancellationToken::new();
        let monitor_shutdown = shutdown.clone();
        let closed = Arc::new(AtomicBool::new(false));
        let monitor_closed = Arc::clone(&closed);
        let client_shutdown = client.cancellation_token();
        let monitor = tokio::spawn(async move {
            let mut waiting = Box::pin(client.waiting());
            tokio::select! {
                result = &mut waiting => {
                    if let Err(err) = result {
                        tracing::debug!(error = %err, "subprocess peer monitor stopped");
                    }
                    monitor_closed.store(true, Ordering::Release);
                    on_closed();
                }
                () = monitor_shutdown.cancelled() => {
                    client_shutdown.cancel();
                    let _ = waiting.await;
                    monitor_closed.store(true, Ordering::Release);
                }
            }
        });

        Ok(Self {
            client: peer,
            shutdown,
            closed,
            monitor: std::sync::Mutex::new(Some(monitor)),
        })
    }

    /// Cancel the peer and drain its monitor. The monitor owns the rmcp
    /// client's transport wait, so joining it is the observable completion
    /// point for subprocess shutdown.
    pub(crate) async fn close(&self) {
        self.shutdown.cancel();
        let Some(mut monitor) = self.monitor.lock().expect("peer monitor poisoned").take() else {
            return;
        };
        if tokio::time::timeout(PEER_DRAIN_TIMEOUT, &mut monitor)
            .await
            .is_err()
        {
            tracing::warn!("subprocess peer monitor did not drain before shutdown deadline");
            monitor.abort();
            let _ = monitor.await;
        }
    }

    #[must_use]
    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// List all tools through the official MCP client API.
    pub(crate) async fn list_tools(&self) -> Result<Vec<crate::tool::ToolSpec>, String> {
        let tools = self
            .client
            .list_all_tools()
            .await
            .map_err(|err| format!("tools/list: {err}"))?;

        tools
            .into_iter()
            .map(|tool| {
                let input_schema = serde_json::to_value(tool.input_schema.as_ref())
                    .map_err(|err| format!("tools/list schema: {err}"))?;
                Ok(crate::tool::ToolSpec {
                    name: tool.name.into_owned(),
                    description: tool
                        .description
                        .map(|description| description.into_owned())
                        .unwrap_or_default(),
                    input_schema,
                })
            })
            .collect()
    }

    /// Invoke one tool without driving rmcp's automatic MRTR/task helpers.
    /// Ion does not yet expose a local elicitation or task lifecycle, so those
    /// protocol results remain visible errors instead of being silently
    /// retried or polled.
    pub(crate) async fn call_tool(&self, name: &str, arguments: Value) -> Result<String, String> {
        let arguments = arguments
            .as_object()
            .cloned()
            .ok_or_else(|| "MCP tool arguments must be a JSON object".to_owned())?;
        let response = self
            .client
            .call_tool_once(CallToolRequestParams::new(name.to_owned()).with_arguments(arguments))
            .await
            .map_err(|err| format!("tools/call: {err}"))?;

        match response {
            CallToolResponse::Complete(result) => {
                let text = extract_text(&result.content, &result);
                if result.is_error == Some(true) {
                    Err(text)
                } else {
                    Ok(text)
                }
            }
            CallToolResponse::InputRequired(_) => Err(
                "MCP tool requested additional input; Ion does not support elicitation yet"
                    .to_owned(),
            ),
            CallToolResponse::Task(_) => {
                Err("MCP tool created a task; Ion does not support task polling yet".to_owned())
            }
            _ => Err("MCP tool returned an unsupported response".to_owned()),
        }
    }
}

impl Drop for StdioRpc {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

/// Handshake and discovery timeouts: a slow peer delays startup once,
/// visibly, then is skipped.
pub(crate) const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Bounded restart supervision: three attempts, exponential backoff
/// 100ms/200ms/400ms capped at 2s, then the capability circuit opens.
pub(crate) const MAX_RESTARTS: u32 = 3;
pub(crate) const RESTART_BASE_BACKOFF: Duration = Duration::from_millis(100);
pub(crate) const RESTART_MAX_BACKOFF: Duration = Duration::from_secs(2);
const PEER_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// One stdio peer that publishes tools into a [`CatalogService`] scope:
/// spawn, discover, register, wait for closure or lifetime end, restart
/// with cancellation-aware backoff, and open the circuit after
/// [`MAX_RESTARTS`] failures. Shared by MCP servers (§19.1) and
/// extensions (§24.4); `label` only shapes the log message and
/// `make_tool` wraps each discovered tool in the peer's typed contract.
pub(crate) async fn supervise_tool_peer<F>(
    def: PeerDef,
    scope: String,
    service: CatalogService,
    mut ready: Option<oneshot::Sender<()>>,
    label: &str,
    make_tool: F,
) where
    F: Fn(Arc<StdioRpc>, ToolSpec) -> Arc<dyn Tool> + Send + Sync + 'static,
{
    let mut failures = 0;
    loop {
        let Some(lifetime) = service.lifetime() else {
            return;
        };

        let (closed_tx, mut closed_rx) = watch::channel(false);
        let callback_scope = scope.clone();
        let callback_service = service.clone();
        let on_closed: CloseHandler = Arc::new(move || {
            callback_service.remove_scope(&callback_scope);
            let _ = closed_tx.send(true);
        });
        let rpc = match tokio::select! {
            () = lifetime.cancelled() => return,
            result = StdioRpc::spawn(&def, client_info(), HANDSHAKE_TIMEOUT, on_closed) => result,
        } {
            Ok(rpc) => Arc::new(rpc),
            Err(err) => {
                tracing::warn!(peer = %def.name, error = %err, "{label} failed to start");
                if let Some(ready) = ready.take() {
                    let _ = ready.send(());
                }
                if !schedule_restart(&def.name, &lifetime, &mut failures, label).await {
                    return;
                }
                continue;
            }
        };

        let tools = match tokio::select! {
            () = lifetime.cancelled() => {
                rpc.close().await;
                return;
            }
            result = tokio::time::timeout(HANDSHAKE_TIMEOUT, rpc.list_tools()) => result,
        } {
            Ok(Ok(tools)) => tools,
            Ok(Err(err)) => {
                tracing::warn!(peer = %def.name, error = %err, "{label} tools/list failed");
                if let Some(ready) = ready.take() {
                    let _ = ready.send(());
                }
                rpc.close().await;
                if !schedule_restart(&def.name, &lifetime, &mut failures, label).await {
                    return;
                }
                continue;
            }
            Err(_) => {
                tracing::warn!(peer = %def.name, "{label} tools/list timed out");
                if let Some(ready) = ready.take() {
                    let _ = ready.send(());
                }
                rpc.close().await;
                if !schedule_restart(&def.name, &lifetime, &mut failures, label).await {
                    return;
                }
                continue;
            }
        };

        let scoped: Vec<Arc<dyn Tool>> = tools
            .into_iter()
            .map(|spec| make_tool(Arc::clone(&rpc), spec))
            .collect();
        tracing::info!(peer = %def.name, tools = scoped.len(), "{label} ready");
        service.register_scope(scope.clone(), scoped);
        if let Some(ready) = ready.take() {
            let _ = ready.send(());
        }
        if rpc.is_closed() {
            service.remove_scope(&scope);
        } else {
            tokio::select! {
                result = closed_rx.changed() => {
                    if result.is_err() {
                        service.remove_scope(&scope);
                    }
                }
                () = lifetime.cancelled() => {
                    service.remove_scope(&scope);
                }
            }
        }
        rpc.close().await;
        if lifetime.is_cancelled() {
            return;
        }
        if !schedule_restart(&def.name, &lifetime, &mut failures, label).await {
            return;
        }
    }
}

async fn schedule_restart(
    name: &str,
    lifetime: &CancellationToken,
    failures: &mut u32,
    label: &str,
) -> bool {
    if *failures >= MAX_RESTARTS {
        tracing::warn!(peer = %name, "{label} restart limit reached; capability circuit is open");
        return false;
    }
    *failures += 1;
    let multiplier = 1_u32 << failures.saturating_sub(1).min(4);
    let delay = (RESTART_BASE_BACKOFF * multiplier).min(RESTART_MAX_BACKOFF);
    tracing::warn!(peer = %name, attempt = *failures, ?delay, "{label} restarting");
    tokio::select! {
        () = lifetime.cancelled() => false,
        () = tokio::time::sleep(delay) => true,
    }
}

/// Preserve text blocks directly and serialize non-text-only results so the
/// model never receives an empty success for content Ion cannot render yet.
fn extract_text(content: &[ContentBlock], result: &impl serde::Serialize) -> String {
    let texts: Vec<&str> = content
        .iter()
        .filter_map(|block| block.as_text().map(|text| text.text.as_str()))
        .collect();
    if texts.is_empty() {
        serde_json::to_string(result)
            .unwrap_or_else(|_| "MCP tool returned an unreadable result".to_owned())
    } else {
        texts.join("\n")
    }
}
