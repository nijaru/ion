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
use std::time::Duration;

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use std::future::Future;
use std::pin::Pin;

use crate::rpc::{PeerDef, StdioRpc};
use crate::tool::{Tool, ToolCatalog, ToolOutcome};

/// Handshake timeout: a slow extension delays startup once, visibly,
/// then is skipped.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Protocol version requested during initialize; matches the MCP
/// handshake shape so the two transports share one wire contract.
const PROTOCOL_VERSION: &str = "2025-11-25";

/// One configured extension (settings or trusted project manifest).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionDef {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
}

/// Owns extension subprocesses and publishes their tool contributions
/// (the supervisor role in the lifecycle hierarchy, §25.1).
#[derive(Default)]
pub struct ExtensionService {
    _marker: (),
}

impl ExtensionService {
    #[must_use]
    pub fn new() -> Self {
        Self { _marker: () }
    }

    /// Start `defs` and register their tools under `ext:<name>` scopes.
    /// A failing extension logs a warning and is skipped: one broken
    /// extension never blocks startup.
    pub async fn start_into(&self, defs: &[ExtensionDef], catalog: &ToolCatalog) {
        for def in defs {
            let peer = PeerDef {
                name: def.name.clone(),
                command: def.command.clone(),
                args: def.args.clone(),
            };
            let client_info = json!({
                "protocolVersion": PROTOCOL_VERSION,
                "clientInfo": { "name": "ion", "version": env!("CARGO_PKG_VERSION") },
            });
            let rpc = match StdioRpc::spawn(&peer, client_info, HANDSHAKE_TIMEOUT).await {
                Ok(rpc) => rpc,
                Err(err) => {
                    tracing::warn!(extension = %def.name, error = %err, "extension failed to start");
                    continue;
                }
            };
            let arc = Arc::new(rpc);
            match arc.list_tools().await {
                Ok(tools) => {
                    let scoped: Vec<Arc<dyn Tool>> = tools
                        .into_iter()
                        .map(|spec| {
                            Arc::new(ExtensionTool {
                                connection: Arc::clone(&arc),
                                exposed_name: format!("{}__{}", def.name, spec.name),
                                remote_name: spec.name.clone(),
                                spec,
                                extension_name: def.name.clone(),
                            }) as Arc<dyn Tool>
                        })
                        .collect();
                    tracing::info!(
                        extension = %def.name,
                        tools = scoped.len(),
                        "extension ready"
                    );
                    catalog.register_scope(format!("ext:{}", def.name), scoped);
                }
                Err(err) => {
                    tracing::warn!(extension = %def.name, error = %err, "extension tools/list failed");
                }
            }
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
                    Err(err) if err.contains("server closed") => ToolOutcome::error(format!(
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
