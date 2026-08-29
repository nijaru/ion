//! Subprocess extensions (DESIGN.md §24, Step 9).
//!
//! An extension is a subprocess publishing tools over the shared stdio
//! JSON-RPC client - language-neutral by construction: any runtime
//! that speaks the initialize/tools-list/tools-call shape works. Each
//! extension owns an [`ToolCatalog`] scope (`ext:<name>`); unloading
//! tears down the scope structurally (§24.4).
//!
//! Contributions start closed: tools only (§24.3). Commands, skills,
//! and hooks are future contribution types with their own semantics.
//!
//! Trust (§24.5): executable configuration from the project directory
//! is only honored when the caller passes an explicit trust grant;
//! user-level configuration is trusted by being user-authored.

use std::sync::Arc;

use serde_json::Value;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use std::future::Future;
use std::pin::Pin;

use crate::rpc::{HANDSHAKE_TIMEOUT, PeerDef, StdioRpc, supervise_tool_peer};
use crate::tool::{Tool, ToolCatalog, ToolOutcome};

/// One configured extension (settings or trusted project manifest).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionDef {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
}

/// Starts extension subprocesses and publishes their tool contributions.
/// The [`ToolCatalog`] owns the spawned supervisors and drains them when its
/// lifetime closes (the supervisor role in the lifecycle hierarchy, §25.1).
#[derive(Default)]
pub struct ExtensionService;

impl ExtensionService {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Start `defs` and register their tools under `ext:<name>` scopes.
    /// A failing extension logs a warning and is skipped: one broken
    /// extension never blocks startup.
    pub async fn start_into(&self, defs: &[ExtensionDef], catalog: &ToolCatalog) {
        for def in defs {
            let (ready_tx, ready_rx) = oneshot::channel();
            let def = def.clone();
            let service = catalog.service_handle();
            let name = def.name.clone();
            // The configured extension owns this structural identity before
            // its first successful tools/list. Live generations may come and
            // go without changing the lane's admitted scope.
            let scope = format!("ext:{name}");
            service.declare_scope(scope.clone());
            let peer_service = service.clone();
            let spawned = service.spawn(async move {
                supervise_tool_peer(
                    PeerDef {
                        name: name.clone(),
                        command: def.command,
                        args: def.args,
                    },
                    scope,
                    peer_service,
                    Some(ready_tx),
                    "extension",
                    move |connection, spec| {
                        Arc::new(ExtensionTool {
                            connection,
                            exposed_name: format!("{name}__{}", spec.name),
                            remote_name: spec.name.clone(),
                            spec,
                            extension_name: name.clone(),
                        }) as Arc<dyn Tool>
                    },
                )
                .await;
            });
            if !spawned {
                continue;
            }
            // Wait only for the first discovery attempt. Later retries are
            // owned by the service task and do not block other extensions.
            let _ = tokio::time::timeout(HANDSHAKE_TIMEOUT, ready_rx).await;
        }
    }
}

/// An extension tool through the ordinary [`Tool`] contract (§24.2):
/// policy, cancellation, and events behave exactly as for native and
/// MCP tools. A dead process yields a typed crash error naming the
/// extension; the runtime survives it.
struct ExtensionTool {
    connection: Arc<StdioRpc>,
    exposed_name: String,
    remote_name: String,
    spec: crate::tool::ToolSpec,
    extension_name: String,
}

impl Tool for ExtensionTool {
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
                    // Typed failure: the extension's own error vs. a
                    // dead process are distinguishable to the model.
                    Err(err)
                        if err.contains("server closed") || err.contains("Transport closed") =>
                    ToolOutcome::error(format!(
                        "extension `{}` crashed",
                        self.extension_name
                    )),
                    Err(err) => ToolOutcome::error(err),
                },
                () = cancel.cancelled() => ToolOutcome::error("cancelled"),
            }
        })
    }
}
