//! MCP capability transport (DESIGN.md §19).
//!
//! [`McpService`] owns server definitions, process/transport
//! lifecycle, protocol negotiation, and published tool descriptors.
//! Sessions never supervise MCP processes: the service registers each
//! server's tools into the [`ToolCatalog`] under a dedicated scope. The
//! catalog exposes only host-selected active MCP scopes to model steps,
//! and invocations flow through the normal policy/effect path like any other
//! tool.
//!
//! Wire protocol: MCP stdio transport, carried by the official `rmcp`
//! client through the shared [`StdioRpc`] adapter (§24.2).

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::{oneshot, watch};
use tokio_util::sync::CancellationToken;

use std::future::Future;
use std::pin::Pin;

use crate::rpc::{CloseHandler, PeerDef, StdioRpc, client_info};
use crate::tool::{CatalogService, Tool, ToolCatalog, ToolOutcome};

/// Handshake and discovery timeouts: a slow server delays startup once,
/// visibly, then is skipped.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RESTARTS: u32 = 3;
const RESTART_BASE_BACKOFF: Duration = Duration::from_millis(100);
const RESTART_MAX_BACKOFF: Duration = Duration::from_secs(2);

/// One configured MCP server (settings.toml `[mcp_servers.<name>]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerDef {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
}

/// Owns every configured MCP server's lifecycle and published
/// capabilities (§19.1).
#[derive(Default)]
pub struct McpService;

impl McpService {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Start `defs`, discover their tools, and register them into
    /// `catalog` under `mcp:<name>` scopes. A failing server logs a warning
    /// and is skipped: one broken server never blocks startup. Hosts choose
    /// which registered scopes enter model-step snapshots with
    /// [`ToolCatalog::set_active_mcp_servers`].
    pub async fn start_into(&self, defs: &[ServerDef], catalog: &ToolCatalog) {
        for def in defs {
            let (ready_tx, ready_rx) = oneshot::channel();
            let def = def.clone();
            let service = catalog.service_handle();
            tokio::spawn(async move {
                supervise_server(def, service, Some(ready_tx)).await;
            });
            // Wait only for the first discovery attempt. Restart/backoff
            // belongs to the service task and never delays the rest of the
            // host after the initial bounded startup decision.
            let _ = tokio::time::timeout(HANDSHAKE_TIMEOUT, ready_rx).await;
        }
    }
}

async fn supervise_server(
    def: ServerDef,
    service: CatalogService,
    mut ready: Option<oneshot::Sender<()>>,
) {
    let scope = format!("mcp:{}", def.name);
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
        let peer = PeerDef {
            name: def.name.clone(),
            command: def.command.clone(),
            args: def.args.clone(),
        };
        let rpc = match StdioRpc::spawn(&peer, client_info(), HANDSHAKE_TIMEOUT, on_closed).await {
            Ok(rpc) => Arc::new(rpc),
            Err(err) => {
                tracing::warn!(server = %def.name, error = %err, "MCP server failed to start");
                if let Some(ready) = ready.take() {
                    let _ = ready.send(());
                }
                if !schedule_restart(&def.name, &lifetime, &mut failures).await {
                    return;
                }
                continue;
            }
        };

        let tools = match tokio::time::timeout(HANDSHAKE_TIMEOUT, rpc.list_tools()).await {
            Ok(Ok(tools)) => tools,
            Ok(Err(err)) => {
                tracing::warn!(server = %def.name, error = %err, "MCP tools/list failed");
                if let Some(ready) = ready.take() {
                    let _ = ready.send(());
                }
                drop(rpc);
                if !schedule_restart(&def.name, &lifetime, &mut failures).await {
                    return;
                }
                continue;
            }
            Err(_) => {
                tracing::warn!(server = %def.name, "MCP tools/list timed out");
                if let Some(ready) = ready.take() {
                    let _ = ready.send(());
                }
                drop(rpc);
                if !schedule_restart(&def.name, &lifetime, &mut failures).await {
                    return;
                }
                continue;
            }
        };

        let scoped: Vec<Arc<dyn Tool>> = tools
            .into_iter()
            .map(|spec| {
                Arc::new(McpTool {
                    connection: Arc::clone(&rpc),
                    // Namespaced so two servers cannot collide.
                    exposed_name: format!("{}__{}", def.name, spec.name),
                    remote_name: spec.name.clone(),
                    spec,
                }) as Arc<dyn Tool>
            })
            .collect();
        tracing::info!(
            server = %def.name,
            tools = scoped.len(),
            "MCP server ready"
        );
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
                () = lifetime.cancelled() => return,
            }
        }
        drop(rpc);
        if lifetime.is_cancelled() {
            return;
        }
        if !schedule_restart(&def.name, &lifetime, &mut failures).await {
            return;
        }
    }
}

async fn schedule_restart(name: &str, lifetime: &CancellationToken, failures: &mut u32) -> bool {
    if *failures >= MAX_RESTARTS {
        tracing::warn!(server = %name, "MCP restart limit reached; capability circuit is open");
        return false;
    }
    *failures += 1;
    let multiplier = 1_u32 << failures.saturating_sub(1).min(4);
    let delay = (RESTART_BASE_BACKOFF * multiplier).min(RESTART_MAX_BACKOFF);
    tracing::warn!(server = %name, attempt = *failures, ?delay, "MCP server restarting");
    tokio::select! {
        () = lifetime.cancelled() => false,
        () = tokio::time::sleep(delay) => true,
    }
}

/// An MCP tool surfaced through the normal [`Tool`] contract:
/// admission, policy, canonicalization, and recovery behave exactly as
/// for native tools. Remote effects never replay automatically.
struct McpTool {
    connection: Arc<StdioRpc>,
    exposed_name: String,
    remote_name: String,
    spec: crate::tool::ToolSpec,
}

impl Tool for McpTool {
    fn spec(&self) -> crate::tool::ToolSpec {
        let mut spec = self.spec.clone();
        spec.name = self.exposed_name.clone();
        spec
    }

    fn call<'a>(
        &'a self,
        arguments: Value,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            let call = self.connection.call_tool(&self.remote_name, arguments);
            tokio::select! {
                result = call => match result {
                    Ok(text) => ToolOutcome::text(text),
                    Err(err) => ToolOutcome::error(err),
                },
                () = cancel.cancelled() => ToolOutcome::error("cancelled"),
            }
        })
    }
}
