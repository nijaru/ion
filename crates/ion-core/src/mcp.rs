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

use serde_json::Value;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use std::future::Future;
use std::pin::Pin;

use crate::rpc::{HANDSHAKE_TIMEOUT, PeerDef, StdioRpc, supervise_tool_peer};
use crate::tool::{Tool, ToolCatalog, ToolOutcome};

/// One configured MCP server (settings.toml `[mcp_servers.<name>]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerDef {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
}

/// Starts configured MCP peers. The [`ToolCatalog`] owns the spawned
/// supervisors and drains them when its lifetime closes (§19.1).
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
            let name = def.name.clone();
            let peer_service = service.clone();
            let spawned = service.spawn(async move {
                supervise_tool_peer(
                    PeerDef {
                        name: name.clone(),
                        command: def.command,
                        args: def.args,
                    },
                    // Namespaced scope so two servers cannot collide.
                    format!("mcp:{name}"),
                    peer_service,
                    Some(ready_tx),
                    "MCP server",
                    move |connection, spec| {
                        Arc::new(McpTool {
                            connection,
                            exposed_name: format!("{name}__{}", spec.name),
                            remote_name: spec.name.clone(),
                            spec,
                        }) as Arc<dyn Tool>
                    },
                )
                .await;
            });
            if !spawned {
                continue;
            }
            // Wait only for the first discovery attempt. Restart/backoff
            // belongs to the service task and never delays the rest of the
            // host after the initial bounded startup decision.
            let _ = tokio::time::timeout(HANDSHAKE_TIMEOUT, ready_rx).await;
        }
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
