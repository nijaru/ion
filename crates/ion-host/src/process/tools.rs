//! Strict model-facing process actions over the captured live supervisor.
use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use base64::Engine;
use ion_ai::BoxFuture;
use ion_core::{
    CodingToolOutput, CodingToolSource, ToolActivityKind, ToolDefinition, ToolExecutor,
    ToolExposure, ToolInvocation, ToolOrigin, ToolPresentation, ToolRegistration,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{
    Entry, JoinGate, ProcessOwner,
    driver::{self, InputRequest, ProcessState, StartReceipt, StatusView},
    unavailable,
};

const MAX_COMMAND_BYTES: usize = 65_536;
const MAX_INPUT_BYTES: usize = 65_536;
const DEFAULT_READ_BYTES: usize = 16_384;
const MAX_READ_BYTES: usize = 65_536;
const DEFAULT_WAIT_MS: u64 = 1_000;
const MAX_WAIT_MS: u64 = 30_000;

/// Process controls bound to one canonical working directory and shared owner.
pub struct ProcessTools {
    cwd: PathBuf,
    owner: Arc<ProcessOwner>,
}

impl ProcessTools {
    pub fn new(cwd: impl AsRef<Path>, owner: Arc<ProcessOwner>) -> io::Result<Self> {
        let cwd = cwd.as_ref().canonicalize()?;
        if !cwd.is_dir() {
            return Err(io::Error::other("working directory is not a directory"));
        }
        Ok(Self { cwd, owner })
    }
}

impl CodingToolSource for ProcessTools {
    fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
        vec![ToolRegistration::new(
            ToolDefinition {
                spec: ion_ai::ToolSpec {
                    name: "process".into(),
                    description: "Application-owned pipe-backed native processes. start returns promptly after an actual shell spawn and survives this Turn; it is not a terminal, PTY or human-attached shell, and daemonized/background descendants are not contained. Coding child jobs close and release their own programs before settling; their process handles do not transfer to the parent. Commands run in the captured working directory with host permissions, stdin/stdout/stderr as pipes. Handles are owner UUIDs, never PIDs, and controls only work from the invoking Session and captured cwd in this application. list/inspect observe scoped slots. wait joins the real driver and readers without killing or consuming the slot; timeout is an observation, not a process deadline. read returns bounded raw rolling windows (64 KiB per stream) with monotonic cursors, gaps and observed-EOF completeness; it is not a whole-output artifact. write and eof serialize through a bounded input queue and report actual delivered bytes and partial progress. close requests a process-group stop and joins; acknowledge frees one actually joined retained slot. Unknown, foreign and previous-owner IDs are unavailable and are never restarted or replayed.".into(),
                    input_schema: json!({"type":"object","oneOf":[
                        {"type":"object","additionalProperties":false,"required":["action","command"],"properties":{"action":{"const":"start"},"command":{"type":"string","minLength":1,"maxLength":65536}}},
                        {"type":"object","additionalProperties":false,"required":["action"],"properties":{"action":{"const":"list"}}},
                        {"type":"object","additionalProperties":false,"required":["action","id"],"properties":{"action":{"const":"inspect"},"id":{"type":"string","format":"uuid"}}},
                        {"type":"object","additionalProperties":false,"required":["action","id"],"properties":{"action":{"const":"wait"},"id":{"type":"string","format":"uuid"},"timeout_ms":{"type":"integer","minimum":1,"maximum":30000}}},
                        {"type":"object","additionalProperties":false,"required":["action","id"],"properties":{"action":{"const":"read"},"id":{"type":"string","format":"uuid"},"stream":{"enum":["stdout","stderr"]},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":65536},"encoding":{"enum":["utf8","base64"]}}},
                        {"type":"object","additionalProperties":false,"required":["action","id","data"],"properties":{"action":{"const":"write"},"id":{"type":"string","format":"uuid"},"data":{"type":"string","maxLength":65536}}},
                        {"type":"object","additionalProperties":false,"required":["action","id"],"properties":{"action":{"const":"eof"},"id":{"type":"string","format":"uuid"}}},
                        {"type":"object","additionalProperties":false,"required":["action","id"],"properties":{"action":{"const":"close"},"id":{"type":"string","format":"uuid"}}},
                        {"type":"object","additionalProperties":false,"required":["action","id"],"properties":{"action":{"const":"acknowledge"},"id":{"type":"string","format":"uuid"}}}
                    ]}),
                },
                presentation: ToolPresentation::argument(ToolActivityKind::Command, "command"),
                exposure: ToolExposure::Direct,
            },
            self,
        )]
    }
}

impl ToolExecutor for ProcessTools {
    fn execute<'a>(&'a self, invocation: ToolInvocation<'a>) -> BoxFuture<'a, CodingToolOutput> {
        Box::pin(async move {
            let Some(origin) = invocation.origin() else {
                return error("process requires an admitted Core invocation");
            };
            let action: Action = match serde_json::from_value(invocation.call().arguments.clone()) {
                Ok(action) => action,
                Err(failure) => return error(format!("invalid process arguments: {failure}")),
            };
            let stop = invocation.stop().clone();
            match action {
                Action::Start { command } => self.start(origin, command, stop).await,
                Action::List => self.list(origin).await,
                Action::Inspect { id } => match self.owner.scoped(origin, &self.cwd, id).await {
                    Ok(entry) => success(entry.state, entry.gate, json!({})),
                    Err(failure) => error(failure),
                },
                Action::Wait { id, timeout_ms } => {
                    let timeout = timeout_ms.unwrap_or(DEFAULT_WAIT_MS);
                    if timeout == 0 || timeout > MAX_WAIT_MS {
                        return error("wait timeout_ms must be 1..=30000");
                    }
                    self.wait(origin, id, timeout, stop).await
                }
                Action::Read {
                    id,
                    stream,
                    offset,
                    limit,
                    encoding,
                } => {
                    let limit = limit.unwrap_or(DEFAULT_READ_BYTES as u64);
                    if limit == 0 || limit > MAX_READ_BYTES as u64 {
                        return error("read limit must be 1..=65536");
                    }
                    self.read(
                        origin,
                        id,
                        stream.unwrap_or(Stream::Stdout),
                        offset.unwrap_or(0),
                        limit as usize,
                        encoding.unwrap_or(Encoding::Utf8),
                    )
                    .await
                }
                Action::Write { id, data } => {
                    if data.len() > MAX_INPUT_BYTES {
                        return error("write data exceeds 65536 UTF-8 bytes");
                    }
                    self.input(origin, id, Some(data.into_bytes()), stop).await
                }
                Action::Eof { id } => self.input(origin, id, None, stop).await,
                Action::Close { id } => self.close(origin, id, stop).await,
                Action::Acknowledge { id } => self.acknowledge(origin, id).await,
            }
        })
    }
}

impl ProcessTools {
    async fn start(
        &self,
        origin: ToolOrigin,
        command: String,
        stop: CancellationToken,
    ) -> CodingToolOutput {
        if command.is_empty() || command.len() > MAX_COMMAND_BYTES || command.contains('\0') {
            return error("invalid process command");
        }
        if stop.is_cancelled() {
            return error("cancelled before process start");
        }
        let id = Uuid::now_v7();
        let state = Arc::new(ProcessState::new(id, self.cwd.clone(), command));
        let (go_tx, go_rx) = oneshot::channel();
        let (receipt_tx, receipt_rx) = oneshot::channel();
        let gate = {
            let mut entries = self.owner.entries.lock().await;
            if self.owner.closed() {
                return error("process owner is closed");
            }
            if entries.len() >= self.owner.capacity {
                return error("process capacity is full; acknowledge joined results");
            }
            let handle = tokio::spawn(driver::run(
                state.clone(),
                go_rx,
                receipt_tx,
                stop.clone(),
                self.owner.stop.clone(),
            ));
            let gate = Arc::new(JoinGate::new(state.clone(), handle));
            let entry = Entry {
                session: origin.session(),
                cwd: self.cwd.clone(),
                state: state.clone(),
                gate: gate.clone(),
            };
            entries.insert(id, entry);
            gate
        };
        // Registration and the retained task precede every spawn effect.
        let _ = go_tx.send(());
        let receipt = tokio::select! {
            biased;
            () = stop.cancelled() => {
                state.stop.cancel();
                let joined = gate.join().await;
                return cancellation_output(
                    &state,
                    &gate,
                    joined.err(),
                    "process start cancelled while waiting for spawn receipt; stop and join completed",
                );
            }
            receipt = receipt_rx => receipt,
        };
        match receipt {
            Ok(StartReceipt::Started) => success(state, gate, json!({})),
            Ok(StartReceipt::Failed(failure)) => {
                let failure = match gate.join().await {
                    Ok(()) => failure,
                    Err(error) => format!("{failure}; supervision failed: {error}"),
                };
                let mut output = snapshot(&state, &gate);
                output["error"] = json!(failure);
                CodingToolOutput {
                    value: output,
                    images: Vec::new(),
                    is_error: true,
                }
            }
            Ok(StartReceipt::Cancelled) => {
                let joined = gate.join().await;
                cancellation_output(
                    &state,
                    &gate,
                    joined.err(),
                    "process start cancelled before spawn; no process effect was started",
                )
            }
            Err(_) => {
                let joined = gate.join().await;
                cancellation_output(
                    &state,
                    &gate,
                    joined
                        .err()
                        .or_else(|| Some("start receipt was lost".into())),
                    "process start supervision ended without a receipt",
                )
            }
        }
    }

    async fn list(&self, origin: ToolOrigin) -> CodingToolOutput {
        let entries = self.owner.entries.lock().await;
        let mut processes = entries
            .values()
            .filter(|entry| entry.session == origin.session() && entry.cwd == self.cwd)
            .map(|entry| snapshot(&entry.state, &entry.gate))
            .collect::<Vec<_>>();
        processes.sort_by_key(|process| {
            process["id"]
                .as_str()
                .and_then(|id| Uuid::parse_str(id).ok())
        });
        success_value(json!({"backend":"pipe","processes":processes}))
    }

    async fn wait(
        &self,
        origin: ToolOrigin,
        id: Uuid,
        timeout_ms: u64,
        stop: CancellationToken,
    ) -> CodingToolOutput {
        let entry = match self.owner.scoped(origin, &self.cwd, id).await {
            Ok(entry) => entry,
            Err(failure) => return error(failure),
        };
        let mut timed_out = false;
        let mut cancelled = false;
        let result = tokio::select! {
            biased;
            result = entry.gate.join() => Some(result),
            () = tokio::time::sleep(std::time::Duration::from_millis(timeout_ms)) => {
                timed_out = true;
                None
            }
            () = stop.cancelled() => {
                cancelled = true;
                None
            }
        };
        let supervision_error = result
            .as_ref()
            .and_then(|result| result.as_ref().err().cloned());
        let mut output = snapshot(&entry.state, &entry.gate);
        output["timed_out"] = json!(timed_out);
        output["cancelled"] = json!(cancelled);
        if let Some(error) = supervision_error {
            output["error"] = json!(error);
        }
        CodingToolOutput {
            value: output,
            images: Vec::new(),
            is_error: cancelled || result.is_some_and(|result| result.is_err()),
        }
    }

    async fn read(
        &self,
        origin: ToolOrigin,
        id: Uuid,
        stream: Stream,
        offset: u64,
        limit: usize,
        encoding: Encoding,
    ) -> CodingToolOutput {
        let entry = match self.owner.scoped(origin, &self.cwd, id).await {
            Ok(entry) => entry,
            Err(failure) => return error(failure),
        };
        let ring = match stream {
            Stream::Stdout => &entry.state.stdout,
            Stream::Stderr => &entry.state.stderr,
        };
        let window = match ring.read(offset, limit, MAX_READ_BYTES) {
            Ok(window) => window,
            Err(failure) => return error(failure),
        };
        let meta = ring.meta();
        let mut output = json!({
            "id": id,
            "stream": match stream { Stream::Stdout => "stdout", Stream::Stderr => "stderr" },
            "first_available": window.first_available,
            "offset": window.offset,
            "next_offset": window.next_offset,
            "total_bytes": window.total_bytes,
            "gap_bytes": window.gap_bytes,
            "complete": window.complete,
            "capture_error": meta.capture_error,
            "captured_bytes": window.bytes.len(),
        });
        match encoding {
            Encoding::Utf8 => {
                let lossy = std::str::from_utf8(&window.bytes).is_err();
                output["encoding"] = json!("utf8");
                output["text"] = json!(String::from_utf8_lossy(&window.bytes));
                output["lossy"] = json!(lossy);
            }
            Encoding::Base64 => {
                output["encoding"] = json!("base64");
                output["base64"] =
                    json!(base64::engine::general_purpose::STANDARD.encode(&window.bytes));
            }
        }
        success_value(output)
    }

    async fn input(
        &self,
        origin: ToolOrigin,
        id: Uuid,
        data: Option<Vec<u8>>,
        stop: CancellationToken,
    ) -> CodingToolOutput {
        let entry = match self.owner.scoped(origin, &self.cwd, id).await {
            Ok(entry) => entry,
            Err(failure) => return error(failure),
        };
        let Some(sender) = entry.state.input() else {
            return error("process input is not available (not started or already joined)");
        };
        let (reply, reply_rx) = oneshot::channel();
        let is_eof = data.is_none();
        let request = InputRequest {
            data,
            stop: stop.clone(),
            reply,
        };
        tokio::select! {
            biased;
            () = stop.cancelled() => {
                return error_value(json!({
                    "id": id, "bytes_written": 0, "cancelled": true,
                    "enqueued": false, "error": "input request cancelled before enqueue",
                }));
            }
            result = sender.send(request) => {
                if result.is_err() {
                    return error("process input is not available (writer already joined)");
                }
            }
        }
        match reply_rx.await {
            Ok(ack) => {
                let mut output = json!({
                    "id": id,
                    "bytes_written": ack.bytes_written,
                    "cancelled": ack.cancelled,
                    "enqueued": true,
                });
                if let Some(failure) = &ack.error {
                    output["error"] = json!(failure);
                }
                CodingToolOutput {
                    value: output,
                    images: Vec::new(),
                    is_error: ack.cancelled || ack.error.is_some(),
                }
            }
            Err(_) => {
                let input_error = if !is_eof {
                    "process input writer ended without acknowledgement; delivery is unknown"
                } else {
                    "process input writer ended without acknowledging EOF"
                };
                error_value(json!({
                    "id": id, "bytes_written": Value::Null, "enqueued": true,
                    "delivery_unknown": true, "error": input_error,
                }))
            }
        }
    }

    async fn close(
        &self,
        origin: ToolOrigin,
        id: Uuid,
        stop: CancellationToken,
    ) -> CodingToolOutput {
        let entry = match self.owner.scoped(origin, &self.cwd, id).await {
            Ok(entry) => entry,
            Err(failure) => return error(failure),
        };
        entry.state.stop.cancel();
        let cancelled = tokio::select! {
            biased;
            result = entry.gate.join() => {
                if let Err(error) = result {
                    let mut output = snapshot(&entry.state, &entry.gate);
                    output["cancelled"] = json!(false);
                    output["error"] = json!(error);
                    return CodingToolOutput { value: output, images: Vec::new(), is_error: true };
                }
                false
            }
            () = stop.cancelled() => true,
        };
        let mut output = snapshot(&entry.state, &entry.gate);
        output["cancelled"] = json!(cancelled);
        CodingToolOutput {
            value: output,
            images: Vec::new(),
            is_error: false,
        }
    }

    async fn acknowledge(&self, origin: ToolOrigin, id: Uuid) -> CodingToolOutput {
        let mut entries = self.owner.entries.lock().await;
        let Some(entry) = entries.get(&id) else {
            return error(unavailable());
        };
        if entry.session != origin.session() || entry.cwd != self.cwd {
            return error(unavailable());
        }
        if !entry.gate.is_joined() {
            return error("process is not joined; wait or close before acknowledging");
        }
        entries.remove(&id);
        success_value(json!({"id": id, "acknowledged": true}))
    }
}

fn snapshot(state: &ProcessState, gate: &JoinGate) -> Value {
    let status = state.status();
    let joined = status.joined || gate.is_joined();
    let stdout = state.stdout.meta();
    let stderr = state.stderr.meta();
    json!({
        "id": state.id,
        "backend": "pipe",
        "joined": joined,
        "started": status.started,
        "running": status.started && status.exit.is_none() && !joined,
        "start_error": status.start_error,
        "exit": exit_json(&status),
        "supervision_error": status.supervision_error,
        "stdout": {
            "total_bytes": stdout.total_bytes,
            "complete": stdout.complete,
            "capture_error": stdout.capture_error,
        },
        "stderr": {
            "total_bytes": stderr.total_bytes,
            "complete": stderr.complete,
            "capture_error": stderr.capture_error,
        },
    })
}

fn exit_json(status: &StatusView) -> Value {
    match &status.exit {
        Some(exit) => json!({
            "exit_code": exit.exit_code,
            "signal": exit.signal,
            "wait_error": exit.wait_error,
        }),
        None => Value::Null,
    }
}

fn success(state: Arc<ProcessState>, gate: Arc<JoinGate>, extra: Value) -> CodingToolOutput {
    let mut value = snapshot(&state, &gate);
    if let (Some(value), Some(extra)) = (value.as_object_mut(), extra.as_object()) {
        value.extend(extra.clone());
    }
    CodingToolOutput {
        value,
        images: Vec::new(),
        is_error: false,
    }
}
fn success_value(value: Value) -> CodingToolOutput {
    CodingToolOutput {
        value,
        images: Vec::new(),
        is_error: false,
    }
}
fn error_value(value: Value) -> CodingToolOutput {
    CodingToolOutput {
        value,
        images: Vec::new(),
        is_error: true,
    }
}
fn error(message: impl Into<String>) -> CodingToolOutput {
    error_value(json!({"error": message.into()}))
}
fn cancellation_output(
    state: &ProcessState,
    gate: &JoinGate,
    supervision_error: Option<String>,
    message: &str,
) -> CodingToolOutput {
    let mut value = snapshot(state, gate);
    value["cancelled"] = json!(true);
    value["error"] = json!(message);
    if let Some(error) = supervision_error {
        value["supervision_error"] = json!(error);
    }
    error_value(value)
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Start {
        command: String,
    },
    List,
    Inspect {
        id: Uuid,
    },
    Wait {
        id: Uuid,
        #[serde(default, deserialize_with = "present_input")]
        timeout_ms: Option<u64>,
    },
    Read {
        id: Uuid,
        #[serde(default, deserialize_with = "present_input")]
        stream: Option<Stream>,
        #[serde(default, deserialize_with = "present_input")]
        offset: Option<u64>,
        #[serde(default, deserialize_with = "present_input")]
        limit: Option<u64>,
        #[serde(default, deserialize_with = "present_input")]
        encoding: Option<Encoding>,
    },
    Write {
        id: Uuid,
        data: String,
    },
    Eof {
        id: Uuid,
    },
    Close {
        id: Uuid,
    },
    Acknowledge {
        id: Uuid,
    },
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Stream {
    Stdout,
    Stderr,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Encoding {
    Utf8,
    Base64,
}

// Omission selects defaults; a present field must have its declared type.
fn present_input<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}
