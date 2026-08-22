//! Shared stdio JSON-RPC 2.0 client for subprocess capability
//! transports (MCP servers §19, extensions §24).
//!
//! One subprocess per peer; newline-delimited JSON-RPC over the peer's
//! stdin/stdout, responses demuxed by id to awaiting callers. Both
//! consumers run an `initialize` handshake before discovery, so the
//! wire shape is identical; what differs is who owns the process and
//! what the contributions mean.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc, oneshot};

/// In-flight JSON-RPC requests keyed by id.
type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

/// A live connection to one stdio JSON-RPC subprocess.
pub(crate) struct StdioRpc {
    stdin_tx: mpsc::Sender<String>,
    next_id: AtomicU64,
    pending: Pending,
    /// Kept for teardown: dropping kills the process (kill_on_drop).
    _child: tokio::process::Child,
}

/// Definition of one subprocess peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PeerDef {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
}

impl StdioRpc {
    /// Spawn the peer and perform a `initialize` handshake. Any failure
    /// tears the process down.
    pub(crate) async fn spawn(
        def: &PeerDef,
        client_info: Value,
        timeout: Duration,
    ) -> Result<Self, String> {
        let mut child = tokio::process::Command::new(&def.command)
            .args(&def.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            // stderr belongs to the peer's own logs, not the wire.
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
            // Peer exited: fail every in-flight request so callers
            // observe the crash instead of waiting forever.
            let mut map = reader_pending.lock().await;
            for (_, sender) in map.drain() {
                let _ = sender.send(Err("server closed".to_owned()));
            }
        });

        let rpc = Self {
            stdin_tx,
            next_id: AtomicU64::new(1),
            pending,
            _child: child,
        };

        let response = tokio::time::timeout(timeout, async {
            rpc.request("initialize", client_info).await
        })
        .await
        .map_err(|_| "initialize timed out".to_owned())??;
        tracing::debug!(
            server = %def.name,
            version = %response
                .get("protocolVersion")
                .and_then(|v| v.as_str())
                .unwrap_or("?"),
            "subprocess peer initialized"
        );

        Ok(rpc)
    }

    /// Send one JSON-RPC request and await its matching response.
    pub(crate) async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let message = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        if self.stdin_tx.send(format!("{message}\n")).await.is_err() {
            self.pending.lock().await.remove(&id);
            return Err("server closed".to_owned());
        }
        match rx.await {
            Ok(result) => result,
            Err(_) => Err("response channel dropped".to_owned()),
        }
    }

    /// Send one JSON-RPC notification (no id, no response).
    pub(crate) async fn notify(&self, method: &str) {
        let message = serde_json::json!({ "jsonrpc": "2.0", "method": method });
        let _ = self.stdin_tx.send(format!("{message}\n")).await;
    }

    /// List tools via MCP's tools/list shape - the shared discovery
    /// contract for both transports.
    pub(crate) async fn list_tools(&self) -> Result<Vec<crate::tool::ToolSpec>, String> {
        let response = self.request("tools/list", serde_json::json!({})).await?;
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
            tools.push(crate::tool::ToolSpec {
                name: name.to_owned(),
                description: tool
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_owned(),
                input_schema: tool
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({"type": "object", "required": []})),
            });
        }
        Ok(tools)
    }

    /// Invoke a tool via MCP's tools/call shape. Server-side isError
    /// results surface as errors; text blocks join as output with
    /// compact JSON as fallback.
    pub(crate) async fn call_tool(&self, name: &str, arguments: Value) -> Result<String, String> {
        let response = self
            .request(
                "tools/call",
                serde_json::json!({ "name": name, "arguments": arguments }),
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
