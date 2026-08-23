//! MCP capability transport (DESIGN.md §19).
//!
//! [`McpService`] owns server definitions, process/transport
//! lifecycle, protocol negotiation, and published tool descriptors.
//! Sessions never supervise MCP processes: the service registers each
//! server's tools into the [`ToolCatalog`] under a dedicated scope,
//! and invocations flow through the normal policy/effect path like
//! any other tool.
//!
//! Wire protocol: MCP stdio transport, carried by the official `rmcp`
//! client through the shared [`StdioRpc`] adapter (§24.2).

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use std::future::Future;
use std::pin::Pin;

use crate::rpc::{CloseHandler, PeerDef, StdioRpc, client_info};
use crate::tool::{Tool, ToolCatalog, ToolOutcome};

/// Handshake and discovery timeouts: a slow server delays startup once,
/// visibly, then is skipped.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

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
pub struct McpService {
    _marker: (),
}

impl McpService {
    #[must_use]
    pub fn new() -> Self {
        Self { _marker: () }
    }

    /// Start `defs`, discover their tools, and register them into
    /// `catalog` under `mcp:<name>` scopes. A failing server logs a
    /// warning and is skipped: one broken server never blocks startup.
    pub async fn start_into(&self, defs: &[ServerDef], catalog: &ToolCatalog) {
        for def in defs {
            let scope = format!("mcp:{}", def.name);
            let callback_scope = scope.clone();
            let callback_catalog = catalog.clone();
            let on_closed: CloseHandler = Arc::new(move || {
                callback_catalog.remove_scope(&callback_scope);
            });
            let peer = PeerDef {
                name: def.name.clone(),
                command: def.command.clone(),
                args: def.args.clone(),
            };
            let rpc = match StdioRpc::spawn(&peer, client_info(), HANDSHAKE_TIMEOUT, on_closed)
                .await
            {
                Ok(rpc) => rpc,
                Err(err) => {
                    tracing::warn!(server = %def.name, error = %err, "MCP server failed to start");
                    continue;
                }
            };
            let arc = Arc::new(rpc);
            match arc.list_tools().await {
                Ok(tools) => {
                    let scoped: Vec<Arc<dyn Tool>> = tools
                        .into_iter()
                        .map(|spec| {
                            Arc::new(McpTool {
                                connection: Arc::clone(&arc),
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
                    catalog.register_scope(scope.clone(), scoped);
                    if arc.is_closed() {
                        catalog.remove_scope(&scope);
                    }
                }
                Err(err) => {
                    tracing::warn!(server = %def.name, error = %err, "MCP tools/list failed");
                }
            }
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
