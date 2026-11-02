//! Native command process groups, shutdown and bounded/retained output capture.
use std::{
    borrow::Cow, collections::VecDeque, os::unix::process::ExitStatusExt, path::Path,
    process::Stdio, time::Duration,
};

use futures_util::FutureExt;
use rustix::process::{Pid, Signal, WaitId, WaitIdOptions, kill_process_group, waitid};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

use super::{ToolOutput, error};

/// Bounded final bytes retained per stream before the private raw capture.
const MAX_OUTPUT_BYTES: usize = 64 * 1024;
/// Quiet-pipe allowance after the direct child exits or a stop is requested.
pub(crate) const POST_EXIT_OUTPUT_IDLE: Duration = Duration::from_millis(100);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecInput {
    command: String,
    #[serde(default, deserialize_with = "super::present_input")]
    timeout_ms: Option<u64>,
}

/// Run a validated command in `cwd`, returning its observed streams and status.
pub(super) async fn run(
    environment: &crate::CommandEnvironment,
    cwd: &Path,
    args: &Value,
    stop: CancellationToken,
    artifacts: Option<ion_core::ArtifactSink>,
) -> ToolOutput {
    let input: ExecInput = match serde_json::from_value(args.clone()) {
        Ok(value) => value,
        Err(e) => return error(format!("invalid exec arguments: {e}")),
    };
    if input.command.is_empty() || input.command.contains('\0') {
        return error("invalid command");
    }
    if input.timeout_ms == Some(0) {
        return error("timeout_ms must be positive");
    }
    if stop.is_cancelled() {
        return error("cancelled before command start");
    }
    // Subscribe before spawning: normal exit observation must leave the root
    // unreaped while output draining can still request group termination.
    let mut exits = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::child()) {
        Ok(exits) => exits,
        Err(e) => return error(format!("cannot observe command exit before start: {e}")),
    };
    let mut command = environment.command(&input.command);
    command
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => return error(format!("command did not start: {e}")),
    };
    let pid = child
        .id()
        .and_then(|id| i32::try_from(id).ok())
        .and_then(Pid::from_raw)
        .expect("spawned unreaped command has a positive Unix pid");
    let stdout = OutputCapture::start(
        child.stdout.take().expect("piped"),
        artifacts.clone(),
        "stdout",
    );
    let stderr = OutputCapture::start(child.stderr.take().expect("piped"), artifacts, "stderr");
    let mut cancelled = false;
    let mut timed_out = false;
    let deadline = async {
        if let Some(timeout) = input.timeout_ms {
            tokio::time::sleep(Duration::from_millis(timeout)).await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    tokio::pin!(deadline);
    let mut status = tokio::select! {
        observed = observe_exit(pid, &mut exits) => {
            match observed {
                Ok(_) => None,
                Err(error) => Some(kill_child(&mut child, Some(pid)).await.and(Err(error))),
            }
        },
        () = stop.cancelled() => {
            cancelled = true;
            Some(stop_child(&mut child, Some(pid)).await)
        },
        () = &mut deadline => {
            timed_out = true;
            Some(stop_child(&mut child, Some(pid)).await)
        },
    };
    // A descendant may hold the pipes after the direct command exits.
    // Continue reading while output arrives, then release idle pipes.
    // Timeout already requested process stop too: neither stream may
    // follow an endlessly writing descendant during the final drain.
    let already_stopped = cancelled || timed_out;
    let drain_stop = CancellationToken::new();
    let captures = async {
        tokio::join!(
            stdout.finish(&drain_stop, already_stopped),
            stderr.finish(&drain_stop, already_stopped),
        )
    };
    tokio::pin!(captures);
    let (stdout, stderr) = tokio::select! {
        biased;
        () = stop.cancelled(), if !cancelled => {
            cancelled = true;
            if status.is_none() {
                status = Some(stop_child(&mut child, Some(pid)).await);
            }
            drain_stop.cancel();
            captures.await
        }
        // The deadline belongs to the entire command observation, not only
        // the direct child. Descendants can keep inherited pipes active
        // after that child exits. Stop acquisition, but join retained storage I/O.
        () = &mut deadline, if !already_stopped => {
            timed_out = true;
            if status.is_none() {
                status = Some(stop_child(&mut child, Some(pid)).await);
            }
            drain_stop.cancel();
            captures.await
        }
        result = &mut captures => result,
    };
    // Linearize observation completion before releasing the reserved root.
    // A stop/deadline becoming ready alongside EOF must still stop the group.
    cancelled |= stop.is_cancelled();
    if !timed_out {
        timed_out = deadline.as_mut().now_or_never().is_some();
    }
    if status.is_none() && (cancelled || timed_out) {
        status = Some(stop_child(&mut child, Some(pid)).await);
    }
    // No group signal follows this reap. Normal exit deliberately leaves
    // background descendants alone; cancellation/timeout stops the owned group.
    let status = match status {
        Some(status) => status,
        None => child.wait().await,
    };
    let (exit_code, signal, wait_error, succeeded) = match status {
        Ok(status) => (status.code(), status.signal(), None, status.success()),
        Err(error) => (
            None,
            None,
            Some(format!(
                "command started, but direct-child exit is unknown: {error}"
            )),
            false,
        ),
    };
    let stdout_text = String::from_utf8_lossy(&stdout.bytes);
    let stderr_text = String::from_utf8_lossy(&stderr.bytes);
    // This describes decoding of the returned tail, not the entire stream:
    // a byte-boundary cut can require replacement even for valid UTF-8 output.
    let mut artifacts = Vec::new();
    let stdout_artifact = stdout.artifact.map(|artifact| {
        let index = artifacts.len();
        artifacts.push(artifact);
        index
    });
    let stderr_artifact = stderr.artifact.map(|artifact| {
        let index = artifacts.len();
        artifacts.push(artifact);
        index
    });
    let result = json!({
        "exit_code": exit_code, "signal": signal, "wait_error": wait_error,
        "stdout_lossy": matches!(stdout_text, Cow::Owned(_)),
        "stderr_lossy": matches!(stderr_text, Cow::Owned(_)),
        "stdout": stdout_text, "stderr": stderr_text,
        "stdout_complete": stdout.complete, "stderr_complete": stderr.complete,
        "stdout_truncated": !stdout.complete || stdout.omitted_bytes != Some(0),
        "stderr_truncated": !stderr.complete || stderr.omitted_bytes != Some(0),
        "stdout_omitted_bytes": stdout.omitted_bytes, "stderr_omitted_bytes": stderr.omitted_bytes,
        "stdout_artifact": stdout_artifact, "stderr_artifact": stderr_artifact,
        "stdout_artifact_error": stdout.full_error, "stderr_artifact_error": stderr.full_error,
        "cancelled": cancelled, "timed_out": timed_out,
    });
    ToolOutput {
        artifacts,
        value: result,
        images: Vec::new(),
        is_error: !succeeded || cancelled || timed_out,
    }
}

/// Observe without reaping so one-shot owners can terminate a reserved group.
pub(crate) async fn observe_exit(
    pid: Pid,
    exits: &mut tokio::signal::unix::Signal,
) -> std::io::Result<bool> {
    loop {
        match waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        ) {
            Ok(Some(status)) => return Ok(status.exit_status() == Some(0)),
            Ok(None) => {
                exits
                    .recv()
                    .await
                    .ok_or_else(|| std::io::Error::other("child exit observation closed"))?;
            }
            Err(rustix::io::Errno::INTR) => tokio::task::yield_now().await,
            Err(error) => return Err(error.into()),
        }
    }
}

/// Escalate the owned group before reaping its root. This joins the direct
/// child, not arbitrary grandchildren or work in escaped process groups.
pub(crate) async fn stop_child(
    child: &mut tokio::process::Child,
    pid: Option<Pid>,
) -> std::io::Result<std::process::ExitStatus> {
    if let Some(pid) = pid {
        // Reserve the root PID through escalation. Reaping on its TERM exit
        // would both abandon TERM-resistant members and make a later group
        // signal unsafe. Preserve the root's graceful status and final output.
        if let Ok(mut exits) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::child())
        {
            let _ = kill_process_group(pid, Signal::TERM);
            let _ =
                tokio::time::timeout(Duration::from_secs(1), observe_exit(pid, &mut exits)).await;
        }
    }
    kill_child(child, pid).await
}

/// Terminate the owned group before reaping its direct child. Protocol workers
/// use this without a grace period when a reply can no longer be accepted.
pub(crate) async fn kill_child(
    child: &mut tokio::process::Child,
    pid: Option<Pid>,
) -> std::io::Result<std::process::ExitStatus> {
    if let Some(pid) = pid {
        let _ = kill_process_group(pid, Signal::KILL);
    }
    child.kill().await?;
    child.wait().await
}

struct Captured {
    bytes: Vec<u8>,
    complete: bool,
    omitted_bytes: Option<u64>,
    artifact: Option<ion_core::ArtifactRef>,
    full_error: Option<String>,
}

struct CaptureState {
    bytes: VecDeque<u8>,
    total: u64,
    complete: bool,
    artifact: Option<ion_core::ArtifactRef>,
    full_error: Option<String>,
}

impl CaptureState {
    fn record_error(&mut self, message: String) {
        if let Some(previous) = &mut self.full_error {
            previous.push_str("; ");
            previous.push_str(&message);
        } else {
            self.full_error = Some(message);
        }
    }
}

fn start_artifact(
    sink: Option<&ion_core::ArtifactSink>,
    label: &str,
) -> std::io::Result<ion_core::ArtifactWriter> {
    let sink = sink.ok_or_else(|| {
        std::io::Error::other("raw capture requires an explicit Session artifact sink")
    })?;
    sink.start(label)
}

struct OutputCapture {
    task: tokio::task::JoinHandle<CaptureState>,
    exited: CancellationToken,
    stop_acquisition: CancellationToken,
}
impl Drop for OutputCapture {
    fn drop(&mut self) {
        // Stop acquisition without aborting filesystem work in flight.
        self.stop_acquisition.cancel();
    }
}
impl OutputCapture {
    fn start(
        mut pipe: impl tokio::io::AsyncRead + Unpin + Send + 'static,
        sink: Option<ion_core::ArtifactSink>,
        label: &'static str,
    ) -> Self {
        let exited = CancellationToken::new();
        let capture_exited = exited.clone();
        let stop_acquisition = CancellationToken::new();
        let capture_stop = stop_acquisition.clone();
        let task = tokio::spawn(async move {
            let mut state = CaptureState {
                bytes: VecDeque::with_capacity(MAX_OUTPUT_BYTES),
                total: 0,
                complete: false,
                artifact: None,
                full_error: None,
            };
            let mut chunk = [0u8; 8192];
            let mut writer: Option<ion_core::ArtifactWriter> = None;
            let mut writer_failed = false;
            let mut post_exit = false;
            loop {
                // Idle means a pending pipe read, never a writer write/flush.
                // Stop takes priority even for endless ready output; otherwise
                // ready bytes or EOF win over a simultaneously expired idle wait.
                let read = tokio::select! {
                    biased;
                    () = capture_stop.cancelled() => break,
                    result = pipe.read(&mut chunk) => result,
                    () = capture_exited.cancelled(), if !post_exit => {
                        post_exit = true;
                        continue;
                    }
                    () = tokio::time::sleep(POST_EXIT_OUTPUT_IDLE), if post_exit => break,
                };
                match read {
                    Ok(0) => {
                        state.complete = true;
                        break;
                    }
                    Ok(n) => {
                        let prefix = (!writer_failed
                            && writer.is_none()
                            && state.total.saturating_add(n as u64) > MAX_OUTPUT_BYTES as u64)
                            .then(|| state.bytes.iter().copied().collect::<Vec<_>>());
                        state.total = state.total.saturating_add(n as u64);
                        let overflow = state
                            .bytes
                            .len()
                            .saturating_add(n)
                            .saturating_sub(MAX_OUTPUT_BYTES);
                        let remove_existing = overflow.min(state.bytes.len());
                        state.bytes.drain(..remove_existing);
                        state.bytes.extend(&chunk[overflow - remove_existing..n]);
                        if let Some(prefix) = prefix {
                            match async {
                                let mut writer = start_artifact(sink.as_ref(), label)?;
                                writer.write_bytes(&prefix).await?;
                                writer.write_bytes(&chunk[..n]).await?;
                                Ok::<_, std::io::Error>(writer)
                            }
                            .await
                            {
                                Ok(capture) => writer = Some(capture),
                                Err(error) => {
                                    writer_failed = true;
                                    state.record_error(error.to_string());
                                }
                            }
                        } else if let Some(capture) = &mut writer
                            && let Err(error) = capture.write_bytes(&chunk[..n]).await
                        {
                            writer_failed = true;
                            writer = None;
                            state.record_error(error.to_string());
                        }
                        tokio::task::consume_budget().await;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                        // A signal is not EOF or a permanent capture fault.
                        // Yield so repeated interruptions cannot starve stop.
                        tokio::task::yield_now().await;
                    }
                    Err(error) => {
                        state.record_error(format!("output pipe read failed: {error}"));
                        break;
                    }
                }
            }
            // A small complete stream normally needs no artifact. If decoding
            // would destroy bytes, preserve its raw evidence through the same
            // Session capture. Do this only after acquisition, so a UTF-8 sequence
            // split across pipe reads is not mistaken for invalid output.
            if state.complete
                && writer.is_none()
                && !writer_failed
                && std::str::from_utf8(state.bytes.make_contiguous()).is_err()
            {
                match async {
                    let mut writer = start_artifact(sink.as_ref(), label)?;
                    writer.write_bytes(state.bytes.make_contiguous()).await?;
                    Ok::<_, std::io::Error>(writer)
                }
                .await
                {
                    Ok(capture) => writer = Some(capture),
                    Err(error) => state.record_error(error.to_string()),
                }
            }
            // Acquisition has stopped, but every started storage operation
            // must settle. EOF remains true even if the artifact cannot finish.
            if let Some(writer) = writer {
                match writer.finish_async(state.complete).await {
                    Ok(artifact) => state.artifact = Some(artifact),
                    Err(error) => state.record_error(error.to_string()),
                }
            }
            state
        });
        Self {
            task,
            exited,
            stop_acquisition,
        }
    }

    async fn finish(mut self, stop: &CancellationToken, already_stopped: bool) -> Captured {
        self.exited.cancel();
        // Only cancellation/timeout imposes a bounded drain regardless of
        // activity. Normal direct exit has no total post-exit cutoff.
        let stop_reading = async {
            if already_stopped {
                tokio::time::sleep(POST_EXIT_OUTPUT_IDLE).await;
            } else {
                stop.cancelled().await;
            }
        };
        let result = tokio::select! {
            biased;
            result = &mut self.task => result,
            () = stop_reading => {
                self.stop_acquisition.cancel();
                // Do not abort: this task may be writing or flushing, even
                // after EOF. It will stop at its next acquisition boundary.
                (&mut self.task).await
            }
        };
        let mut state = result.unwrap_or_else(|error| CaptureState {
            bytes: VecDeque::new(),
            total: 0,
            complete: false,
            artifact: None,
            full_error: Some(format!("output capture task failed: {error}")),
        });
        let bytes: Vec<u8> = std::mem::take(&mut state.bytes).into();
        let complete = state.complete;
        let omitted_bytes = complete.then(|| state.total.saturating_sub(bytes.len() as u64));
        Captured {
            bytes,
            complete,
            omitted_bytes,
            artifact: state.artifact.take(),
            full_error: state.full_error.take(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::ArtifactFixture;
    use ion_ai::ToolCall;
    use ion_core::{ToolCatalog, ToolSet, ToolSource};
    use std::{fs, os::unix::fs::PermissionsExt, sync::Arc};

    fn tools(cwd: &Path) -> ToolCatalog {
        ToolSet::new([Arc::new(crate::LocalTools::new(cwd).unwrap()) as Arc<dyn ToolSource>])
            .snapshot()
    }

    async fn run_exec(
        cwd: &Path,
        args: Value,
        stop: CancellationToken,
    ) -> (ToolOutput, ArtifactFixture) {
        let owner = ArtifactFixture::new();
        let output = tools(cwd)
            .execute(
                ion_core::ToolInvocation::untracked(
                    &ToolCall {
                        id: "exec".into(),
                        name: "exec".into(),
                        arguments: args,
                        raw_arguments: None,
                    },
                    stop,
                )
                .with_artifact_sink(owner.sink()),
            )
            .await;
        (output, owner)
    }

    #[tokio::test]
    async fn untracked_overflow_reports_missing_custody_without_changing_exit_status() {
        let output = tools(&std::env::temp_dir())
            .execute(ion_core::ToolInvocation::untracked(
                &ToolCall {
                    id: "exec".into(),
                    name: "exec".into(),
                    arguments: json!({"command":"head -c 70000 /dev/zero | tr '\\000' x"}),
                    raw_arguments: None,
                },
                CancellationToken::new(),
            ))
            .await;
        assert!(!output.is_error, "{}", output.value);
        assert_eq!(output.value["exit_code"], 0);
        assert_eq!(output.value["stdout"], "x".repeat(MAX_OUTPUT_BYTES));
        assert_eq!(output.value["stdout_complete"], true);
        assert_eq!(
            output.value["stdout_omitted_bytes"],
            70000 - MAX_OUTPUT_BYTES
        );
        assert_eq!(output.value["stdout_artifact"], Value::Null);
        assert!(output.artifacts.is_empty());
        assert!(
            output.value["stdout_artifact_error"]
                .as_str()
                .unwrap()
                .contains("explicit Session artifact sink")
        );
    }
    #[test]
    fn capture_joins_storage_before_and_after_eof_even_when_stopped() {
        use std::{
            pin::Pin,
            sync::{
                atomic::{AtomicUsize, Ordering},
                mpsc,
            },
            task::{Context, Poll},
        };
        use tokio::io::{AsyncRead, ReadBuf};
        struct ObservedRead {
            cursor: std::io::Cursor<Vec<u8>>,
            delivered: Arc<AtomicUsize>,
            eof: Option<tokio::sync::oneshot::Sender<()>>,
        }
        impl AsyncRead for ObservedRead {
            fn poll_read(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                buf: &mut ReadBuf<'_>,
            ) -> Poll<std::io::Result<()>> {
                let before = buf.filled().len();
                let result = Pin::new(&mut self.cursor).poll_read(cx, buf);
                if matches!(result, Poll::Ready(Ok(()))) {
                    let n = buf.filled().len() - before;
                    self.delivered.fetch_add(n, Ordering::SeqCst);
                    if n == 0
                        && let Some(eof) = self.eof.take()
                    {
                        let _ = eof.send(());
                    }
                }
                result
            }
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        runtime.block_on(async {
            for (at_eof, cancel) in [(false, false), (true, false), (false, true), (true, true)] {
                let owner = ArtifactFixture::new();
                let payload = [
                    b"x\n".repeat(if at_eof { 36864 } else { 524288 }),
                    b"END_MARKER\n".to_vec(),
                ]
                .concat();
                let delivered = Arc::new(AtomicUsize::new(0));
                // Sender Drop also releases the blocking thread during panic.
                let (release, gate) = mpsc::channel::<()>();
                let (entered_tx, entered) = tokio::sync::oneshot::channel();
                let blocker = tokio::task::spawn_blocking(move || {
                    let _ = entered_tx.send(());
                    let _ = gate.recv();
                });
                entered.await.unwrap();
                let (eof_tx, eof) = tokio::sync::oneshot::channel();
                let capture = OutputCapture::start(
                    ObservedRead {
                        cursor: std::io::Cursor::new(payload.clone()),
                        delivered: delivered.clone(),
                        eof: Some(eof_tx),
                    },
                    Some(owner.sink()),
                    "stdout",
                );
                let stop = CancellationToken::new();
                let trigger = stop.clone();
                let finishing = tokio::spawn(async move { capture.finish(&stop, false).await });
                if at_eof {
                    tokio::time::timeout(Duration::from_secs(2), eof)
                        .await
                        .unwrap()
                        .unwrap();
                } else {
                    tokio::time::timeout(Duration::from_secs(2), async {
                        while delivered.load(Ordering::SeqCst) < 262144 {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .unwrap();
                }
                if cancel {
                    trigger.cancel();
                }
                tokio::time::sleep(POST_EXIT_OUTPUT_IDLE * 3).await;
                let before_release = delivered.load(Ordering::SeqCst);
                let premature = finishing.is_finished();
                drop(release);
                blocker.await.unwrap();
                let captured = tokio::time::timeout(Duration::from_secs(3), finishing)
                    .await
                    .unwrap()
                    .unwrap();
                assert!(
                    !premature,
                    "capture released queued storage: eof={at_eof}, cancel={cancel}"
                );
                let complete = !cancel || at_eof;
                assert_eq!(captured.complete, complete);
                assert_eq!(captured.full_error, None);
                let observed = if complete {
                    payload.as_slice()
                } else {
                    &payload[..before_release]
                };
                assert_eq!(
                    captured.bytes,
                    observed[observed.len() - MAX_OUTPUT_BYTES..]
                );
                assert_eq!(
                    captured.omitted_bytes,
                    complete.then_some((observed.len() - MAX_OUTPUT_BYTES) as u64)
                );
                let reference = captured.artifact.unwrap();
                assert_eq!(reference.complete, complete);
                assert_eq!(reference.bytes, observed.len() as u64);
                assert_eq!(owner.bytes(&reference), observed);
            }
        });
    }
    #[tokio::test]
    async fn interrupted_capture_retries_and_fatal_pipe_errors_remain_visible() {
        use std::{
            io::ErrorKind,
            pin::Pin,
            task::{Context, Poll},
        };
        use tokio::io::{AsyncRead, ReadBuf};
        struct FaultThenTail {
            prefix: std::io::Cursor<Vec<u8>>,
            fault: Option<ErrorKind>,
            tail: std::io::Cursor<Vec<u8>>,
        }
        impl AsyncRead for FaultThenTail {
            fn poll_read(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                buf: &mut ReadBuf<'_>,
            ) -> Poll<std::io::Result<()>> {
                if self.prefix.position() < self.prefix.get_ref().len() as u64 {
                    return Pin::new(&mut self.prefix).poll_read(cx, buf);
                }
                if let Some(kind) = self.fault.take() {
                    return Poll::Ready(Err(std::io::Error::new(kind, "injected pipe fault")));
                }
                Pin::new(&mut self.tail).poll_read(cx, buf)
            }
        }
        let prefix = b"x\n".repeat(35000);
        let tail = b"END_MARKER\n";
        for kind in [ErrorKind::Interrupted, ErrorKind::Other] {
            let owner = ArtifactFixture::new();
            let captured = OutputCapture::start(
                FaultThenTail {
                    prefix: std::io::Cursor::new(prefix.clone()),
                    fault: Some(kind),
                    tail: std::io::Cursor::new(tail.to_vec()),
                },
                Some(owner.sink()),
                "stdout",
            )
            .finish(&CancellationToken::new(), false)
            .await;
            if kind == ErrorKind::Interrupted {
                let reference = captured.artifact.as_ref().unwrap();
                let saved = owner.bytes(reference);
                assert!(reference.complete);
                assert!(captured.complete, "an interrupted read is retryable");
                let payload = [prefix.as_slice(), tail].concat();
                assert_eq!(captured.bytes, payload[payload.len() - MAX_OUTPUT_BYTES..]);
                assert_eq!(
                    captured.omitted_bytes,
                    Some((payload.len() - MAX_OUTPUT_BYTES) as u64)
                );
                assert_eq!(saved, payload);
                assert_eq!(captured.full_error, None);
            } else {
                assert!(!captured.complete);
                assert_eq!(captured.omitted_bytes, None);
                let reference = captured.artifact.as_ref().unwrap();
                assert!(!reference.complete);
                assert_eq!(owner.bytes(reference), prefix);
                assert_eq!(captured.bytes, prefix[prefix.len() - MAX_OUTPUT_BYTES..]);
                assert!(
                    captured
                        .full_error
                        .as_deref()
                        .is_some_and(|message| message.contains("injected pipe fault")),
                    "missing read failure: {:?}",
                    captured.full_error
                );
            }
        }
    }
    #[tokio::test]
    async fn small_invalid_output_is_marked_and_preserved_without_failing_the_command() {
        for (command, expected, lossy) in [
            (
                "printf '\\377'; printf '\\376' >&2",
                vec![vec![0xff], vec![0xfe]],
                true,
            ),
            (
                "printf '\\357\\277\\275'; printf '\\357\\277\\275' >&2",
                vec!["\u{fffd}".as_bytes().to_vec(); 2],
                false,
            ),
        ] {
            let (output, owner) = run_exec(
                &std::env::temp_dir(),
                json!({"command": command}),
                CancellationToken::new(),
            )
            .await;
            let captures = ["stdout", "stderr"].map(|stream| {
                output.value[format!("{stream}_artifact")]
                    .as_u64()
                    .map(|index| {
                        let reference = &output.artifacts[index as usize];
                        assert!(reference.complete);
                        owner.bytes(reference)
                    })
            });
            assert!(!output.is_error, "{}", output.value);
            assert_eq!(output.value["exit_code"], 0);
            assert_eq!(
                fs::metadata(owner.session.path())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            for ((stream, expected), capture) in
                ["stdout", "stderr"].into_iter().zip(expected).zip(captures)
            {
                assert_eq!(output.value[format!("{stream}_lossy")], lossy);
                assert_eq!(output.value[format!("{stream}_complete")], true);
                assert_eq!(output.value[format!("{stream}_truncated")], false);
                assert_eq!(output.value[format!("{stream}_omitted_bytes")], 0);
                assert_eq!(
                    output.value[format!("{stream}_artifact_error")],
                    Value::Null
                );
                assert_eq!(output.value[stream], "\u{fffd}");
                assert_eq!(capture, lossy.then_some(expected));
            }
        }
    }

    #[tokio::test]
    async fn utf8_tail_cut_reports_decoding_loss_separately_from_omitted_bytes() {
        let (output, owner) = run_exec(
            &std::env::temp_dir(),
            json!({"command": "printf '\\303\\251'; head -c 65535 /dev/zero | tr '\\000' x"}),
            CancellationToken::new(),
        )
        .await;
        let reference =
            &output.artifacts[output.value["stdout_artifact"].as_u64().unwrap() as usize];
        let capture = owner.bytes(reference);
        assert!(reference.complete);
        assert!(!output.is_error, "{}", output.value);
        assert!(
            std::str::from_utf8(&capture).is_ok(),
            "the full stream is valid UTF-8"
        );
        assert_eq!(output.value["stdout_lossy"], true);
        assert_eq!(output.value["stdout_complete"], true);
        assert_eq!(output.value["stdout_truncated"], true);
        assert_eq!(output.value["stdout_omitted_bytes"], 1);
        assert_eq!(
            output.value["stdout"],
            format!("\u{fffd}{}", "x".repeat(MAX_OUTPUT_BYTES - 1))
        );
        assert_eq!(
            capture,
            [b"\xc3\xa9".as_slice(), &vec![b'x'; MAX_OUTPUT_BYTES - 1]].concat()
        );
    }

    #[tokio::test]
    async fn long_command_output_keeps_raw_captures_and_lossy_failure_tails() {
        let (output, owner) = run_exec(
            &std::env::temp_dir(),
            json!({"command":"yes x | head -c 70000; printf '\\377\\377\\000END_MARKER\\n'; yes e | head -c 70000 >&2; printf '\\377\\377\\000ERROR_MARKER\\n' >&2"}),
            CancellationToken::new(),
        )
        .await;
        let captures = ["stdout", "stderr"].map(|stream| {
            let reference = &output.artifacts
                [output.value[format!("{stream}_artifact")].as_u64().unwrap() as usize];
            assert!(reference.complete);
            owner.bytes(reference)
        });
        assert!(!output.is_error, "{}", output.value);
        assert_eq!(output.value["exit_code"], 0);
        assert_eq!(output.value["cancelled"], false);
        assert_eq!(output.value["timed_out"], false);
        let expected_stdout = [b"x\n".repeat(35_000), b"\xff\xff\0END_MARKER\n".to_vec()].concat();
        let expected_stderr =
            [b"e\n".repeat(35_000), b"\xff\xff\0ERROR_MARKER\n".to_vec()].concat();
        for ((stream, expected), capture) in
            [("stdout", expected_stdout), ("stderr", expected_stderr)]
                .into_iter()
                .zip(captures)
        {
            assert_eq!(capture, expected);
            assert_eq!(output.value[format!("{stream}_truncated")], true);
            assert_eq!(output.value[format!("{stream}_lossy")], true);
            assert_eq!(output.value[format!("{stream}_complete")], true);
            assert_eq!(
                output.value[format!("{stream}_artifact_error")],
                Value::Null
            );
            assert_eq!(
                output.value[format!("{stream}_omitted_bytes")],
                expected.len() - MAX_OUTPUT_BYTES
            );
            let tail = &expected[expected.len() - MAX_OUTPUT_BYTES..];
            let rendered = output.value[stream].as_str().unwrap();
            assert_eq!(rendered, String::from_utf8_lossy(tail));
            // Two invalid bytes expand to replacements; omission is measured
            // in raw bytes, never in this longer text representation.
            assert_eq!(rendered.len(), MAX_OUTPUT_BYTES + 4);
            assert!(rendered.contains('\0'));
            assert!(rendered.ends_with(if stream == "stdout" {
                "END_MARKER\n"
            } else {
                "ERROR_MARKER\n"
            }));
        }
    }
    #[tokio::test]
    async fn direct_exit_keeps_output_when_descendant_holds_pipe_open() {
        let (output, _owner) = run_exec(
            &std::env::temp_dir(),
            json!({"command":"sleep 3 & printf 'DIRECT_EXIT_MARKER\\n'"}),
            CancellationToken::new(),
        )
        .await;
        assert!(!output.is_error, "{}", output.value);
        assert_eq!(output.value["exit_code"], 0);
        assert_eq!(output.value["stdout"], "DIRECT_EXIT_MARKER\n");
        assert_eq!(output.value["stdout_complete"], false);
        assert_eq!(output.value["stdout_lossy"], false);
        assert_eq!(output.value["stdout_truncated"], true);
        assert_eq!(output.value["stdout_omitted_bytes"], Value::Null);
    }
    #[tokio::test]
    async fn incomplete_long_capture_retains_a_truthful_observed_prefix() {
        let (output, owner) = run_exec(
            &std::env::temp_dir(),
            json!({"command":"sleep 2 & yes x | head -c 70000"}),
            CancellationToken::new(),
        )
        .await;
        assert!(!output.is_error, "{}", output.value);
        assert_eq!(output.value["stdout_truncated"], true);
        assert_eq!(output.value["stdout_omitted_bytes"], Value::Null);
        let reference =
            &output.artifacts[output.value["stdout_artifact"].as_u64().unwrap() as usize];
        assert!(!reference.complete);
        assert_eq!(owner.bytes(reference), b"x\n".repeat(35000));
    }
    #[tokio::test]
    async fn post_exit_output_activity_keeps_the_pipe_open_until_idle() {
        // A native producer sleeping 50ms can actually remain quiet beyond the
        // 100ms idle contract under load. Model readable arrivals in poll_read:
        // even if scheduling is delayed, available bytes beat an expired timer.
        // Native held-pipe, cancellation and storage custody have separate coverage.
        struct Pulse {
            next: std::pin::Pin<Box<tokio::time::Sleep>>,
            count: u8,
        }
        impl tokio::io::AsyncRead for Pulse {
            fn poll_read(
                mut self: std::pin::Pin<&mut Self>,
                cx: &mut std::task::Context<'_>,
                buf: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                use std::future::Future;
                if self.count == 24 {
                    return std::task::Poll::Ready(Ok(()));
                }
                if self.next.as_mut().poll(cx).is_pending() {
                    return std::task::Poll::Pending;
                }
                buf.put_slice(&[b'a' + self.count]);
                self.count += 1;
                self.next
                    .as_mut()
                    .reset(tokio::time::Instant::now() + POST_EXIT_OUTPUT_IDLE / 4);
                std::task::Poll::Ready(Ok(()))
            }
        }
        let reader = Pulse {
            next: Box::pin(tokio::time::sleep(POST_EXIT_OUTPUT_IDLE / 4)),
            count: 0,
        };
        let output = tokio::time::timeout(
            Duration::from_secs(3),
            OutputCapture::start(reader, None, "stdout").finish(&CancellationToken::new(), false),
        )
        .await
        .expect("active output failed to settle");
        assert_eq!(output.bytes, (b'a'..=b'x').collect::<Vec<_>>());
        assert!(output.complete);
        assert_eq!(output.omitted_bytes, Some(0));
    }
    #[tokio::test]
    async fn cancellation_stops_post_exit_capture() {
        // A periodic native writer can exceed the idle deadline before a timer
        // cancels it. Trigger cancellation from an actual read instead, while
        // acquisition is provably active and normal direct-exit drain has begun.
        struct CancelAfterRead {
            stop: CancellationToken,
            read: bool,
        }
        impl tokio::io::AsyncRead for CancelAfterRead {
            fn poll_read(
                mut self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
                buf: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                if self.read {
                    return std::task::Poll::Pending;
                }
                buf.put_slice(b"x");
                self.read = true;
                self.stop.cancel();
                std::task::Poll::Ready(Ok(()))
            }
        }
        let stop = CancellationToken::new();
        let reader = CancelAfterRead {
            stop: stop.clone(),
            read: false,
        };
        let output = tokio::time::timeout(
            Duration::from_secs(2),
            OutputCapture::start(reader, None, "stdout").finish(&stop, false),
        )
        .await
        .expect("post-exit capture ignored cancellation");
        assert!(!output.complete);
        assert_eq!(output.omitted_bytes, None);
        assert_eq!(output.bytes, b"x");
    }
    #[tokio::test]
    async fn cancelled_capture_does_not_follow_endless_output() {
        use tokio::io::AsyncWriteExt;

        let (reader, mut writer) = tokio::io::duplex(64);
        let writer_task = tokio::spawn(async move {
            loop {
                if writer.write_all(b"x").await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
        let stop = CancellationToken::new();
        stop.cancel();
        let output = tokio::time::timeout(
            Duration::from_millis(500),
            OutputCapture::start(reader, None, "stdout").finish(&stop, true),
        )
        .await;
        writer_task.abort();
        let _ = writer_task.await;
        let captured = output.expect("cancelled capture followed endless output");
        assert!(!captured.complete);
        assert!(!captured.bytes.is_empty());
    }
    #[tokio::test]
    async fn stopped_capture_bounds_continuously_ready_output() {
        let captured = tokio::time::timeout(
            Duration::from_secs(2),
            OutputCapture::start(tokio::io::repeat(b'x'), None, "stdout")
                .finish(&CancellationToken::new(), true),
        )
        .await
        .expect("stopped capture followed continuously ready output");
        assert!(!captured.complete);
        assert_eq!(captured.bytes, vec![b'x'; MAX_OUTPUT_BYTES]);
        assert_eq!(captured.omitted_bytes, None);
        assert_eq!(captured.artifact, None);
        assert!(
            captured
                .full_error
                .unwrap()
                .contains("explicit Session artifact sink")
        );
    }
    async fn native_state(pid: &str) -> String {
        let output = tokio::process::Command::new("ps")
            .args(["-o", "stat=", "-p", pid.trim()])
            .output()
            .await
            .unwrap();
        assert!(matches!(output.status.code(), Some(0 | 1)), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        String::from_utf8(output.stdout).unwrap().trim().into()
    }

    async fn survivor_state(pid: &str) -> Option<String> {
        // SIGKILL requests exit; it does not join arbitrary grandchildren.
        // Allow signal delivery, and exclude Linux orphan zombies, which are
        // still visible to kill(pid, 0) but cannot execute further work.
        let until = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let state = native_state(pid).await;
            if state.is_empty() || state.starts_with('Z') {
                return None;
            }
            if tokio::time::Instant::now() >= until {
                return Some(state);
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    #[tokio::test]
    async fn cancellation_ready_with_output_completion_still_stops_the_reserved_group() {
        let root = std::env::temp_dir().join(format!("ion-stop-eof-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let stop = CancellationToken::new();
        let command = "printf %s $$ > root.pid; sh -c 'trap \"\" TERM; printf %s $$ > descendant.pid; touch ready; while [ ! -e close ]; do sleep 0.01; done; exec >/dev/null 2>&1; touch closed; exec sleep 30' & while [ ! -e ready ]; do sleep 0.01; done; exit 0";
        let run = run_exec(&root, json!({"command": command}), stop.clone());
        tokio::pin!(run);
        assert!(run.as_mut().now_or_never().is_none());
        tokio::time::timeout(Duration::from_secs(3), async {
            while !root.join("ready").exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let pid = fs::read_to_string(root.join("root.pid")).unwrap();
            loop {
                if native_state(&pid).await.starts_with('Z') {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        // Advance the actual driver from exit observation into capture waits,
        // then withhold its polling while its independently spawned readers
        // see EOF. Cancellation is ready when that same select is polled again.
        assert!(run.as_mut().now_or_never().is_none());
        fs::write(root.join("close"), b"").unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while !root.join("closed").exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            tokio::task::yield_now().await;
        })
        .await
        .unwrap();
        stop.cancel();
        let result = tokio::time::timeout(Duration::from_secs(3), run).await;
        let descendant = fs::read_to_string(root.join("descendant.pid")).unwrap();
        let pid = Pid::from_raw(descendant.parse().unwrap()).unwrap();
        let survivor = survivor_state(&descendant).await;
        if survivor.is_some() {
            let _ = rustix::process::kill_process(pid, Signal::KILL);
        }
        fs::remove_dir_all(&root).unwrap();
        let (output, _owner) = result.unwrap();
        assert!(output.is_error);
        assert_eq!(output.value["cancelled"], true);
        assert_eq!(output.value["exit_code"], 0);
        assert_eq!(output.value["wait_error"], Value::Null);
        assert!(
            survivor.is_none(),
            "cancellation lost to completed output: {survivor:?}"
        );
    }

    #[tokio::test]
    async fn shell_stop_terminates_term_resistant_group_members_before_and_after_direct_exit() {
        for (direct_exits, graceful_root) in [(false, false), (true, false), (false, true)] {
            for cancel in [false, true] {
                let root =
                    std::env::temp_dir().join(format!("ion-stop-group-{}", uuid::Uuid::now_v7()));
                fs::create_dir(&root).unwrap();
                let command = format!(
                    "printf %s $$ > root.pid; {} sh -c 'trap \"\" TERM PIPE; printf %s $$ > descendant.pid; printf x >&2; touch ready; while :; do printf x >&2; done' & while [ ! -e ready ]; do sleep 0.01; done; {}",
                    if graceful_root {
                        "trap 'printf ROOT_CLEANUP; exit 0' TERM;"
                    } else {
                        ""
                    },
                    if direct_exits {
                        "exit 0"
                    } else if graceful_root {
                        "wait"
                    } else {
                        "exec sleep 30"
                    }
                );
                let stop = CancellationToken::new();
                let trigger = stop.clone();
                let work = async {
                    let catalog = tools(&root);
                    let call = ToolCall {
                        id: "stop-group".into(),
                        name: "exec".into(),
                        arguments: json!({"command": command, "timeout_ms": if cancel { 30000 } else { 1000 }}),
                        raw_arguments: None,
                    };
                    // Continuous ready output cannot accidentally hit the idle
                    // cutoff. No artifact sink: this lifecycle fixture retains
                    // only the bounded tail, not an unbounded test database.
                    let run = catalog.execute(ion_core::ToolInvocation::untracked(&call, stop));
                    let cancel_when_ready = async {
                        if cancel {
                            while !root.join("ready").exists() {
                                tokio::time::sleep(Duration::from_millis(5)).await;
                            }
                            if direct_exits {
                                let root_pid = fs::read_to_string(root.join("root.pid")).unwrap();
                                loop {
                                    if native_state(&root_pid).await.starts_with('Z') {
                                        break;
                                    }
                                    tokio::time::sleep(Duration::from_millis(5)).await;
                                }
                            }
                            trigger.cancel();
                        }
                    };
                    tokio::join!(run, cancel_when_ready).0
                };
                let result = tokio::time::timeout(Duration::from_secs(5), work).await;
                let descendant = fs::read_to_string(root.join("descendant.pid"))
                    .unwrap()
                    .parse::<i32>()
                    .unwrap();
                let pid = Pid::from_raw(descendant).unwrap();
                let survivor = survivor_state(&descendant.to_string()).await;
                // Always clean the original-red survivor before asserting.
                if survivor.is_some() {
                    let _ = rustix::process::kill_process(pid, Signal::KILL);
                }
                fs::remove_dir_all(&root).unwrap();
                let output = result.expect("command stop did not settle");
                assert!(output.is_error, "{}", output.value);
                assert_eq!(output.value["cancelled"], cancel);
                assert_eq!(output.value["timed_out"], !cancel);
                assert_eq!(output.value["wait_error"], Value::Null);
                assert_eq!(
                    output.value["exit_code"],
                    if direct_exits || graceful_root {
                        json!(0)
                    } else {
                        Value::Null
                    }
                );
                assert_eq!(
                    output.value["signal"],
                    if direct_exits || graceful_root {
                        Value::Null
                    } else {
                        json!(15)
                    }
                );
                if graceful_root {
                    assert_eq!(output.value["stdout"], "ROOT_CLEANUP");
                }
                assert!(!output.value["stderr"].as_str().unwrap().is_empty());
                let complete = output.value["stderr_complete"].as_bool().unwrap();
                // Killing every pipe holder can now expose EOF. A cancelled
                // acquisition may instead stop first; neither may invent EOF.
                let omitted = output.value["stderr_omitted_bytes"].as_u64();
                assert_eq!(omitted.is_some(), complete);
                assert_eq!(
                    output.value["stderr_truncated"],
                    !complete || omitted != Some(0)
                );
                assert!(
                    survivor.is_none(),
                    "same-group descendant survived: direct_exits={direct_exits}, graceful_root={graceful_root}, cancel={cancel}, state={survivor:?}"
                );
            }
        }
    }

    #[tokio::test]
    async fn shell_timeout_is_opt_in_and_accepts_long_explicit_limits() {
        for args in [
            json!({"command":"printf ok"}),
            json!({"command":"printf ok","timeout_ms":600000}),
        ] {
            let (output, _owner) =
                run_exec(&std::env::temp_dir(), args, CancellationToken::new()).await;
            assert!(!output.is_error, "{}", output.value);
            assert_eq!(output.value["stdout"], "ok");
        }
    }

    #[tokio::test]
    async fn shell_uses_bash_when_available() {
        if !Path::new("/bin/bash").is_file() {
            return;
        }
        let (output, _owner) = run_exec(
            &std::env::temp_dir(),
            json!({"command":"set -o pipefail; false | true"}),
            CancellationToken::new(),
        )
        .await;
        assert_eq!(output.value["exit_code"], 1);
        assert!(output.is_error);
    }
}
