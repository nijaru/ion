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

use crate::peer::PeerRegistry;
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
/// `ensure` diffs the desired defs against the live set: unchanged
/// peers keep running, changed defs replace their supervisor inside
/// the same declared scope, removed defs stop (their generation
/// unpublishes; the declared scope remains, §19).
#[derive(Default)]
pub struct McpService {
    registry: PeerRegistry,
}

impl McpService {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Reconcile the configured MCP server set: start new/changed
    /// defs, stop removed ones. Returns `(started, stopped)`.
    pub async fn ensure(&self, defs: &[ServerDef], catalog: &ToolCatalog) -> (usize, usize) {
        let mut started = 0;
        let mut stopped = 0;

        // Stop first: a changed def must fully release its old
        // supervisor before the replacement starts, so two
        // supervisors never race one scope.
        let desired: Vec<String> = defs
            .iter()
            .map(|def| crate::peer::peer_key(&def.name, &def.command, &def.args))
            .collect();
        let obsolete: Vec<String> = self
            .registry
            .keys()
            .into_iter()
            .filter(|key| !desired.contains(key))
            .collect();
        for key in obsolete {
            if self.registry.stop(&key).await {
                stopped += 1;
            }
        }

        for def in defs {
            let key = crate::peer::peer_key(&def.name, &def.command, &def.args);
            if self.registry.contains(&key) {
                continue;
            }
            let Some(lifetime) = catalog.service_handle().lifetime() else {
                continue;
            };
            let cancel = lifetime.child_token();
            let (ready_tx, ready_rx) = oneshot::channel();
            let (stopped_tx, stopped_rx) = oneshot::channel();
            let def = def.clone();
            let service = catalog.service_handle();
            let name = def.name.clone();
            // Structural identity belongs to configuration, not
            // successful discovery. A later supervisor restart
            // republishes a generation inside the same declared
            // scope.
            let scope = format!("mcp:{name}");
            service.declare_scope(scope.clone());
            let peer_service = service.clone();
            let supervise_cancel = cancel.clone();
            let spawned = service.spawn(async move {
                supervise_tool_peer(
                    PeerDef {
                        name: name.clone(),
                        command: def.command,
                        args: def.args,
                    },
                    // Namespaced scope so two servers cannot collide.
                    scope,
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
                    supervise_cancel,
                )
                .await;
                let _ = stopped_tx.send(());
            });
            if !spawned {
                continue;
            }
            self.registry.record(key, cancel, stopped_rx);
            // Wait only for the first discovery attempt. Restart/backoff
            // belongs to the service task and never delays the rest of the
            // host after the initial bounded startup decision.
            let _ = tokio::time::timeout(HANDSHAKE_TIMEOUT, ready_rx).await;
            started += 1;
        }
        (started, stopped)
    }

    /// Start `defs` without diffing (the legacy startup path —
    /// equivalent to `ensure` against an empty registry). Retained
    /// for existing callers; new callers should prefer `ensure`.
    pub async fn start_into(&self, defs: &[ServerDef], catalog: &ToolCatalog) {
        self.ensure(defs, catalog).await;
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
