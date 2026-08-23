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
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub(crate) type CloseHandler = Arc<dyn Fn() + Send + Sync + 'static>;

/// A live connection to one stdio MCP subprocess.
pub(crate) struct StdioRpc {
    client: Peer<RoleClient>,
    shutdown: CancellationToken,
    closed: Arc<AtomicBool>,
    _monitor: JoinHandle<()>,
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
            _monitor: monitor,
        })
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
