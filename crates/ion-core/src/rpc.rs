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
use tokio::sync::{Mutex, mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

use crate::process::ProcessGuard;

/// In-flight JSON-RPC requests keyed by id.
type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

/// A live connection to one stdio JSON-RPC subprocess.
pub(crate) struct StdioRpc {
    stdin_tx: mpsc::Sender<String>,
    next_id: AtomicU64,
    pending: Pending,
    closed: watch::Sender<bool>,
    /// Cancels the peer guardian on owner drop. The guardian remains alive
    /// long enough to kill and reap the process group.
    shutdown: CancellationToken,
    _waiter: tokio::task::JoinHandle<()>,
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
        let mut command = tokio::process::Command::new(&def.command);
        command
            .args(&def.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            // stderr belongs to the peer's own logs, not the wire.
            .stderr(std::process::Stdio::inherit());
        let mut process = ProcessGuard::spawn(&mut command)
            .map_err(|err| format!("spawn {}: {err}", def.command))?;

        let stdin = process.take_stdin().ok_or("no stdin")?;
        let stdout = process.take_stdout().ok_or("no stdout")?;
        let (stdin_tx, mut stdin_rx) = mpsc::channel::<String>(64);
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let (closed, _) = watch::channel(false);

        let writer_pending = Arc::clone(&pending);
        let writer_closed = closed.clone();
        tokio::spawn(async move {
            let mut stdin = stdin;
            while let Some(line) = stdin_rx.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err() {
                    close_connection(&writer_pending, &writer_closed).await;
                    break;
                }
                let _ = stdin.flush().await;
            }
        });

        let reader_pending = Arc::clone(&pending);
        let reader_closed = closed.clone();
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
            close_connection(&reader_pending, &reader_closed).await;
        });

        let waiter_pending = Arc::clone(&pending);
        let waiter_closed = closed.clone();
        let shutdown = CancellationToken::new();
        let waiter_shutdown = shutdown.child_token();
        let waiter = tokio::spawn(async move {
            let reaped = tokio::select! {
                result = process.wait() => result.is_ok(),
                () = waiter_shutdown.cancelled() => process.kill_and_wait().await.is_ok(),
            };
            close_connection(&waiter_pending, &waiter_closed).await;
            if reaped {
                process.disarm();
            }
        });

        let rpc = Self {
            stdin_tx,
            next_id: AtomicU64::new(1),
            pending,
            closed,
            shutdown,
            _waiter: waiter,
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
        let mut closed = self.closed.subscribe();
        if *closed.borrow() {
            return Err("server closed".to_owned());
        }

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let message = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let (tx, mut rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        if *closed.borrow() {
            self.pending.lock().await.remove(&id);
            return Err("server closed".to_owned());
        }
        if self
            .stdin_tx
            .send(format!("{message}{}", char::from(10)))
            .await
            .is_err()
        {
            self.pending.lock().await.remove(&id);
            close_connection(&self.pending, &self.closed).await;
            return Err("server closed".to_owned());
        }

        tokio::select! {
            result = &mut rx => result.unwrap_or_else(|_| Err("response channel dropped".to_owned())),
            changed = closed.changed() => {
                if changed.is_err() {
                    return Err("server closed".to_owned());
                }
                match rx.await {
                    Ok(result) => result,
                    Err(_) => Err("server closed".to_owned()),
                }
            }
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

async fn close_connection(pending: &Pending, closed: &watch::Sender<bool>) {
    closed.send_replace(true);
    let mut map = pending.lock().await;
    for (_, sender) in map.drain() {
        let _ = sender.send(Err("server closed".to_owned()));
    }
}

impl Drop for StdioRpc {
    fn drop(&mut self) {
        self.shutdown.cancel();
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn shell_quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', "'\\''"))
    }

    #[allow(unsafe_code)]
    fn process_probe(pid: i32) -> Result<bool, i32> {
        if unsafe { libc::kill(pid, 0) } == 0 {
            Ok(true)
        } else {
            Err(std::io::Error::last_os_error()
                .raw_os_error()
                .expect("kill errno"))
        }
    }

    #[tokio::test]
    async fn dropping_a_peer_kills_and_reaps_its_process_group() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let pid_path = directory.path().join("peer.pid");
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": { "protocolVersion": "test" },
        })
        .to_string();
        let script = format!(
            "printf '%s\\n' \"$$\" > {}; printf '%s\\n' {}; trap '' TERM; sleep 30",
            shell_quote(&pid_path.to_string_lossy()),
            shell_quote(&response),
        );
        let peer = PeerDef {
            name: "drop-test".to_owned(),
            command: "sh".to_owned(),
            args: vec!["-c".to_owned(), script],
        };

        let rpc = StdioRpc::spawn(&peer, serde_json::json!({}), Duration::from_secs(2))
            .await
            .expect("peer handshake");
        let pid = tokio::fs::read_to_string(&pid_path)
            .await
            .expect("peer pid")
            .trim()
            .parse::<i32>()
            .expect("numeric peer pid");
        drop(rpc);

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                // The guardian must reap the shell after killing its group;
                // ESRCH is stronger evidence than merely observing SIGKILL.
                let exited = match process_probe(pid) {
                    Ok(true) => false,
                    Err(errno) => {
                        assert_eq!(
                            errno,
                            libc::ESRCH,
                            "peer must not be hidden by a permission error"
                        );
                        true
                    }
                    Ok(false) => unreachable!("kill probe cannot return false"),
                };
                if exited {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("dropped peer must be killed and reaped");
    }
}
