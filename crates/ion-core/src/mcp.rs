//! MCP capability transport (DESIGN.md §19).
//!
//! [`McpService`] owns server definitions, process/transport
//! lifecycle, protocol negotiation, and published tool descriptors.
//! Sessions never supervise MCP processes: the service registers each
//! server's tools into the [`ToolCatalog`] under a dedicated scope,
//! and invocations flow through the normal policy/effect path like
//! any other tool.
//!
//! Wire protocol: MCP stdio transport - newline-delimited JSON-RPC 2.0
//! over the server's stdin/stdout (spec 2025-11-25). One subprocess
//! per configured server; stdout carries only protocol messages.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use std::future::Future;
use std::pin::Pin;

use crate::tool::{Tool, ToolCatalog, ToolOutcome, ToolSpec};

/// Handshake and discovery timeouts: a slow server delays startup once,
/// visibly, then is skipped.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Protocol version requested during `initialize`. Bumped deliberately;
/// per §19.4 today's version never shapes core storage or semantics.
const PROTOCOL_VERSION: &str = "2025-11-25";

/// One configured MCP server (settings.toml `[mcp_servers.<name>]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerDef {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
}

/// In-flight JSON-RPC requests keyed by id.
type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

/// A live connection to one MCP stdio server.
struct McpConnection {
    stdin_tx: mpsc::Sender<String>,
    next_id: AtomicU64,
    pending: Pending,
    _child: tokio::process::Child,
}

impl McpConnection {
    /// Spawn the server process, run the initialize handshake, and
    /// return the ready connection. Any failure tears the process down.
    async fn spawn(def: &ServerDef) -> Result<Self, String> {
        let mut child = tokio::process::Command::new(&def.command)
            .args(&def.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            // stderr belongs to the server's own logs, not the wire.
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|err| format!("spawn {}: {err}", def.command))?;

        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let (stdin_tx, mut stdin_rx) = mpsc::channel::<String>(64);
        tokio::spawn(async move {
            let mut stdin = stdin;
            while let Some(line) = stdin_rx.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                let _ = stdin.flush().await;
            }
        });

        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let reader_pending = Arc::clone(&pending);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let Some(id) = value.get("id").and_then(|v| v.as_u64()) else {
                    continue; // notification or malformed
                };
                let mut map = reader_pending.lock().await;
                if let Some(sender) = map.remove(&id) {
                    let result = match value.get("error") {
                        Some(error) => Err(format!("server error: {error}")),
                        None => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
                    };
                    let _ = sender.send(result);
                }
            }
        });

        let connection = Self {
            stdin_tx,
            next_id: AtomicU64::new(1),
            pending,
            _child: child,
        };

        let response = connection
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "ion", "version": env!("CARGO_PKG_VERSION") },
                }),
            )
            .await?;
        let negotiated = response
            .get("protocolVersion")
            .and_then(|v| v.as_str())
            .unwrap_or(PROTOCOL_VERSION);
        tracing::debug!(server = %def.name, version = %negotiated, "MCP initialized");
        connection.notify("notifications/initialized").await;

        Ok(connection)
    }

    /// Send one JSON-RPC request and await its matching response.
    async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        self.stdin_tx
            .send(format!("{message}\n"))
            .await
            .map_err(|_| "server closed".to_owned())?;
        match tokio::time::timeout(HANDSHAKE_TIMEOUT, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("response channel dropped".to_owned()),
            Err(_) => Err("timed out".to_owned()),
        }
    }

    /// Send one JSON-RPC notification (no id, no response).
    async fn notify(&self, method: &str) {
        let message = json!({ "jsonrpc": "2.0", "method": method });
        let _ = self.stdin_tx.send(format!("{message}\n")).await;
    }

    async fn list_tools(&self) -> Result<Vec<(String, ToolSpec)>, String> {
        let response = self.request("tools/list", json!({})).await?;
        let mut tools = Vec::new();
        for tool in response
            .get("tools")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            let Some(name) = tool.get("name").and_then(|v| v.as_str()) else {
                continue;
            };
            tools.push((
                name.to_owned(),
                ToolSpec {
                    name: name.to_owned(),
                    description: tool
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_owned(),
                    input_schema: tool
                        .get("inputSchema")
                        .cloned()
                        .unwrap_or_else(|| json!({"type": "object", "required": []})),
                },
            ));
        }
        Ok(tools)
    }

    async fn call_tool(&self, name: &str, arguments: Value) -> Result<String, String> {
        let response = self
            .request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            )
            .await?;
        if response.get("isError").and_then(|v| v.as_bool()) == Some(true) {
            return Err(extract_text(&response));
        }
        Ok(extract_text(&response))
    }
}

/// Best-effort text extraction from a CallToolResult: join all text
/// blocks; fall back to compact JSON so nothing is silently dropped.
fn extract_text(result: &Value) -> String {
    let empty = Vec::new();
    let content = result
        .get("content")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty);
    let texts: Vec<&str> = content
        .iter()
        .filter_map(|block| block.get("text").and_then(|v| v.as_str()))
        .collect();
    if texts.is_empty() {
        result.to_string()
    } else {
        texts.join("\n")
    }
}

/// Owns every configured MCP server's lifecycle and published
/// capabilities (§19.1).
#[derive(Default)]
pub struct McpService {
    servers: Mutex<HashMap<String, Arc<McpConnection>>>,
}

impl McpService {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Start `defs`, discover their tools, and register them into
    /// `catalog` under `mcp:<name>` scopes. A failing server logs a
    /// warning and is skipped: one broken server never blocks startup.
    pub async fn start_into(&self, defs: &[ServerDef], catalog: &ToolCatalog) {
        for def in defs {
            match McpConnection::spawn(def).await {
                Ok(connection) => {
                    let connection = Arc::new(connection);
                    match connection.list_tools().await {
                        Ok(tools) => {
                            let scoped: Vec<Arc<dyn Tool>> = tools
                                .into_iter()
                                .map(|(name, spec)| {
                                    Arc::new(McpTool {
                                        connection: Arc::clone(&connection),
                                        // Namespaced so two servers cannot collide.
                                        exposed_name: format!("{}__{}", def.name, name),
                                        remote_name: name,
                                        spec,
                                    }) as Arc<dyn Tool>
                                })
                                .collect();
                            tracing::info!(
                                server = %def.name,
                                tools = scoped.len(),
                                "MCP server ready"
                            );
                            catalog.register_scope(format!("mcp:{}", def.name), scoped);
                            self.servers
                                .lock()
                                .await
                                .insert(def.name.clone(), connection);
                        }
                        Err(err) => {
                            tracing::warn!(server = %def.name, error = %err, "MCP tools/list failed");
                        }
                    }
                }
                Err(err) => {
                    tracing::warn!(server = %def.name, error = %err, "MCP server failed to start");
                }
            }
        }
    }
}

/// An MCP tool surfaced through the normal [`Tool`] contract:
/// admission, policy, canonicalization, and recovery behave exactly as
/// for native tools. Remote effects never replay automatically.
struct McpTool {
    connection: Arc<McpConnection>,
    exposed_name: String,
    remote_name: String,
    spec: ToolSpec,
}

impl Tool for McpTool {
    fn spec(&self) -> ToolSpec {
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
