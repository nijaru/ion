//! Native command custody: one shell route, its process group, output pipes and
//! bounded/spooled captures. Filesystem and digest work stay in the parent module.
use std::{
    borrow::Cow,
    collections::VecDeque,
    fs::{self, OpenOptions},
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        process::ExitStatusExt,
    },
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use rustix::process::{Pid, Signal, kill_process_group};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};
use tokio_util::sync::CancellationToken;

use super::{ToolOutput, error};

/// Bounded final bytes retained per stream before the private raw capture.
const MAX_OUTPUT_BYTES: usize = 64 * 1024;
/// Quiet-pipe allowance after the direct child exits or a stop is requested.
const POST_EXIT_OUTPUT_IDLE: Duration = Duration::from_millis(100);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecInput {
    command: String,
    #[serde(default, deserialize_with = "super::present_input")]
    timeout_ms: Option<u64>,
}

/// Run a validated command in `cwd`, returning its observed streams and status.
pub(super) async fn run(cwd: &Path, args: &Value, stop: CancellationToken) -> ToolOutput {
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
    let mut command = Command::new(shell_program());
    command
        .arg("-c")
        .arg(input.command)
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
        .and_then(Pid::from_raw);
    let stdout = OutputCapture::start(child.stdout.take().expect("piped"));
    let stderr = OutputCapture::start(child.stderr.take().expect("piped"));
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
    let status = tokio::select! {
        status = child.wait() => status,
        () = stop.cancelled() => { cancelled = true; stop_child(&mut child, pid).await },
        () = &mut deadline => { timed_out = true; stop_child(&mut child, pid).await },
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
        result = &mut captures => result,
        () = stop.cancelled(), if !cancelled => {
            cancelled = true;
            if let Some(pid) = pid {
                let _ = kill_process_group(pid, Signal::TERM);
            }
            drain_stop.cancel();
            captures.await
        }
        // The deadline belongs to the entire command observation, not only
        // the direct child. Descendants can keep inherited pipes active
        // after that child exits. Stop acquisition, but join spool I/O.
        () = &mut deadline, if !already_stopped => {
            timed_out = true;
            if let Some(pid) = pid {
                let _ = kill_process_group(pid, Signal::TERM);
            }
            drain_stop.cancel();
            captures.await
        }
    };
    cancelled |= stop.is_cancelled();
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
    let result = json!({
        "exit_code": exit_code, "signal": signal, "wait_error": wait_error,
        "stdout_lossy": matches!(stdout_text, Cow::Owned(_)),
        "stderr_lossy": matches!(stderr_text, Cow::Owned(_)),
        "stdout": stdout_text, "stderr": stderr_text,
        "stdout_complete": stdout.complete, "stderr_complete": stderr.complete,
        "stdout_truncated": !stdout.complete || stdout.omitted_bytes != Some(0),
        "stderr_truncated": !stderr.complete || stderr.omitted_bytes != Some(0),
        "stdout_omitted_bytes": stdout.omitted_bytes, "stderr_omitted_bytes": stderr.omitted_bytes,
        "stdout_full_path": stdout.full_path, "stderr_full_path": stderr.full_path,
        "stdout_full_error": stdout.full_error, "stderr_full_error": stderr.full_error,
        "cancelled": cancelled, "timed_out": timed_out,
    });
    ToolOutput {
        value: result,
        images: Vec::new(),
        is_error: !succeeded || cancelled || timed_out,
    }
}

/// Prefer Bash when it is executable, then PATH, then the POSIX fallback.
fn shell_program() -> PathBuf {
    let primary = PathBuf::from("/bin/bash");
    if is_executable(&primary) {
        return primary;
    }
    if let Some(path) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&path) {
            let candidate = directory.join("bash");
            if is_executable(&candidate) {
                return candidate;
            }
        }
    }
    PathBuf::from("/bin/sh")
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

async fn stop_child(
    child: &mut tokio::process::Child,
    pid: Option<Pid>,
) -> std::io::Result<std::process::ExitStatus> {
    if let Some(pid) = pid {
        let _ = kill_process_group(pid, Signal::TERM);
    }
    match tokio::time::timeout(Duration::from_secs(1), child.wait()).await {
        Ok(status) => status,
        Err(_) => {
            if let Some(pid) = pid {
                let _ = kill_process_group(pid, Signal::KILL);
            }
            child.kill().await?;
            child.wait().await
        }
    }
}

struct Captured {
    bytes: Vec<u8>,
    complete: bool,
    omitted_bytes: Option<u64>,
    full_path: Option<PathBuf>,
    full_error: Option<String>,
}

struct CaptureState {
    bytes: VecDeque<u8>,
    total: u64,
    complete: bool,
    full_path: Option<PathBuf>,
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

impl Drop for CaptureState {
    fn drop(&mut self) {
        // A dropped client must not leave an unclaimed completed artifact.
        if let Some(path) = self.full_path.take() {
            let _ = fs::remove_file(path);
        }
    }
}

struct OutputSpool {
    path: Option<PathBuf>,
    file: tokio::fs::File,
}

impl OutputSpool {
    fn open() -> std::io::Result<Self> {
        let path = std::env::temp_dir().join(format!("ion-output-{}.log", uuid::Uuid::now_v7()));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        Ok(Self {
            path: Some(path),
            file: tokio::fs::File::from_std(file),
        })
    }

    async fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.file.write_all(bytes).await
    }

    async fn finish(mut self, complete: bool) -> std::io::Result<Option<PathBuf>> {
        // Tokio writes can return before the blocking filesystem operation
        // settles. Flush even an incomplete capture before deleting its file.
        self.file.flush().await?;
        Ok(if complete { self.path.take() } else { None })
    }
}

impl Drop for OutputSpool {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = fs::remove_file(path);
        }
    }
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
    fn start(mut pipe: impl tokio::io::AsyncRead + Unpin + Send + 'static) -> Self {
        let exited = CancellationToken::new();
        let capture_exited = exited.clone();
        let stop_acquisition = CancellationToken::new();
        let capture_stop = stop_acquisition.clone();
        let task = tokio::spawn(async move {
            let mut state = CaptureState {
                bytes: VecDeque::with_capacity(MAX_OUTPUT_BYTES),
                total: 0,
                complete: false,
                full_path: None,
                full_error: None,
            };
            let mut chunk = [0u8; 8192];
            let mut spool: Option<OutputSpool> = None;
            let mut spool_failed = false;
            let mut post_exit = false;
            loop {
                // Idle means a pending pipe read, never a spool write/flush.
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
                        let prefix = (!spool_failed
                            && spool.is_none()
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
                                let mut file = OutputSpool::open()?;
                                file.write(&prefix).await?;
                                file.write(&chunk[..n]).await?;
                                Ok::<_, std::io::Error>(file)
                            }
                            .await
                            {
                                Ok(file) => spool = Some(file),
                                Err(error) => {
                                    spool_failed = true;
                                    state.record_error(error.to_string());
                                }
                            }
                        } else if let Some(file) = &mut spool
                            && let Err(error) = file.write(&chunk[..n]).await
                        {
                            spool_failed = true;
                            spool = None;
                            state.record_error(error.to_string());
                        }
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
            // private spool. Do this only after acquisition, so a UTF-8 sequence
            // split across pipe reads is not mistaken for invalid output.
            if state.complete
                && spool.is_none()
                && !spool_failed
                && std::str::from_utf8(state.bytes.make_contiguous()).is_err()
            {
                match async {
                    let mut file = OutputSpool::open()?;
                    file.write(state.bytes.make_contiguous()).await?;
                    Ok::<_, std::io::Error>(file)
                }
                .await
                {
                    Ok(file) => spool = Some(file),
                    Err(error) => state.record_error(error.to_string()),
                }
            }
            // Acquisition has stopped, but every started filesystem operation
            // must settle. EOF remains true even if the artifact cannot finish.
            if let Some(spool) = spool {
                match spool.finish(state.complete).await {
                    Ok(path) => state.full_path = path,
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
            full_path: None,
            full_error: Some(format!("output capture task failed: {error}")),
        });
        let bytes: Vec<u8> = std::mem::take(&mut state.bytes).into();
        let complete = state.complete;
        let omitted_bytes = complete.then(|| state.total.saturating_sub(bytes.len() as u64));
        Captured {
            bytes,
            complete,
            omitted_bytes,
            full_path: state.full_path.take(),
            full_error: state.full_error.take(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ion_ai::ToolCall;
    use ion_core::{CodingToolSource, ToolCatalog, ToolSet};
    use std::{os::unix::fs::PermissionsExt, sync::Arc};

    fn tools(cwd: &Path) -> ToolCatalog {
        ToolSet::new([Arc::new(crate::LocalTools::new(cwd).unwrap()) as Arc<dyn CodingToolSource>])
            .snapshot()
    }

    async fn run_exec(cwd: &Path, args: Value, stop: CancellationToken) -> ToolOutput {
        tools(cwd)
            .execute_untracked(
                &ToolCall {
                    id: "exec".into(),
                    name: "exec".into(),
                    arguments: args,
                    raw_arguments: None,
                },
                stop,
            )
            .await
    }

    #[tokio::test]
    async fn spool_finalization_reports_delayed_write_failure_and_removes_artifact() {
        let path = std::env::temp_dir().join(format!("ion-spool-fault-{}", uuid::Uuid::now_v7()));
        fs::write(&path, b"original").unwrap();
        let mut spool = OutputSpool {
            path: Some(path.clone()),
            file: tokio::fs::File::from_std(fs::File::open(&path).unwrap()),
        };
        // Tokio accepts this into its buffer; the actual read-only-file fault
        // is only observed when the blocking write settles during flush.
        spool.write(b"cannot write").await.unwrap();
        assert!(spool.finish(true).await.is_err());
        assert!(!path.exists());
    }
    #[test]
    fn capture_settles_spool_io_without_treating_it_as_pipe_idle() {
        use std::{
            pin::Pin,
            sync::mpsc,
            task::{Context, Poll},
        };
        use tokio::io::{AsyncRead, ReadBuf};

        struct GatedRead {
            cursor: std::io::Cursor<Vec<u8>>,
            stall_at: u64,
            release: Option<mpsc::Receiver<()>>,
            entered: Option<tokio::sync::oneshot::Sender<()>>,
            eof: Option<tokio::sync::oneshot::Sender<()>>,
        }
        impl AsyncRead for GatedRead {
            fn poll_read(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                buf: &mut ReadBuf<'_>,
            ) -> Poll<std::io::Result<()>> {
                if self.cursor.position() == self.stall_at
                    && let Some(release) = self.release.take()
                {
                    let (ready_tx, ready_rx) = mpsc::channel();
                    tokio::task::spawn_blocking(move || {
                        let _ = ready_tx.send(());
                        let _ = release.recv();
                    });
                    // The sole blocking thread must be occupied before the
                    // next chunk can start its real Tokio file write.
                    ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                    let _ = self.entered.take().unwrap().send(());
                }
                let before = buf.filled().len();
                let result = Pin::new(&mut self.cursor).poll_read(cx, buf);
                if matches!(result, Poll::Ready(Ok(())))
                    && buf.filled().len() == before
                    && let Some(eof) = self.eof.take()
                {
                    let _ = eof.send(());
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
            for (stall_at, cancel) in [
                (65_536, false),
                (73_728, false),
                (65_536, true),
                (73_728, true),
            ] {
                let payload = [b"x\n".repeat(36_864), b"END_MARKER\n".to_vec()].concat();
                // Dropping the sender also releases the gate on any assertion
                // failure, before runtime Drop waits for blocking work.
                let (release, gate) = mpsc::channel::<()>();
                let (entered_tx, entered) = tokio::sync::oneshot::channel();
                let (eof_tx, eof) = tokio::sync::oneshot::channel();
                let capture = OutputCapture::start(GatedRead {
                    cursor: std::io::Cursor::new(payload.clone()),
                    stall_at,
                    release: Some(gate),
                    entered: Some(entered_tx),
                    eof: Some(eof_tx),
                });
                let stop = CancellationToken::new();
                let trigger = stop.clone();
                let finishing = tokio::spawn(async move { capture.finish(&stop, false).await });
                tokio::time::timeout(Duration::from_secs(2), entered)
                    .await
                    .unwrap()
                    .unwrap();
                let at_eof = stall_at == 73_728;
                if at_eof {
                    // The final write is queued behind the blocker; EOF then
                    // arrives while the real Tokio File flush is still pending.
                    tokio::time::timeout(Duration::from_secs(2), eof)
                        .await
                        .unwrap()
                        .unwrap();
                }
                if cancel {
                    trigger.cancel();
                }
                tokio::time::sleep(POST_EXIT_OUTPUT_IDLE * 3).await;
                let finished_before_io = finishing.is_finished();
                drop(release);
                let captured = tokio::time::timeout(Duration::from_secs(2), finishing)
                    .await
                    .unwrap()
                    .unwrap();
                assert!(
                    !finished_before_io,
                    "capture returned before spool I/O settled: {stall_at}, {cancel}"
                );
                let complete = !cancel || at_eof;
                assert_eq!(captured.complete, complete);
                assert_eq!(captured.full_error, None);
                if complete {
                    assert_eq!(captured.bytes, payload[payload.len() - MAX_OUTPUT_BYTES..]);
                    assert_eq!(
                        captured.omitted_bytes,
                        Some((payload.len() - MAX_OUTPUT_BYTES) as u64)
                    );
                    let path = captured.full_path.unwrap();
                    assert_eq!(fs::read(&path).unwrap(), payload);
                    assert_eq!(
                        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                        0o600
                    );
                    fs::remove_file(path).unwrap();
                } else {
                    assert_eq!(captured.omitted_bytes, None);
                    assert_eq!(captured.full_path, None);
                    assert_eq!(captured.bytes, payload[8_192..73_728]);
                }
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
            let captured = OutputCapture::start(FaultThenTail {
                prefix: std::io::Cursor::new(prefix.clone()),
                fault: Some(kind),
                tail: std::io::Cursor::new(tail.to_vec()),
            })
            .finish(&CancellationToken::new(), false)
            .await;
            if kind == ErrorKind::Interrupted {
                let saved = captured
                    .full_path
                    .as_ref()
                    .map(|path| fs::read(path).unwrap());
                if let Some(path) = &captured.full_path {
                    fs::remove_file(path).unwrap();
                }
                assert!(captured.complete, "an interrupted read is retryable");
                let payload = [prefix.as_slice(), tail].concat();
                assert_eq!(captured.bytes, payload[payload.len() - MAX_OUTPUT_BYTES..]);
                assert_eq!(
                    captured.omitted_bytes,
                    Some((payload.len() - MAX_OUTPUT_BYTES) as u64)
                );
                assert_eq!(saved, Some(payload));
                assert_eq!(captured.full_error, None);
            } else {
                assert!(!captured.complete);
                assert_eq!(captured.omitted_bytes, None);
                assert_eq!(captured.full_path, None);
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
            let output = run_exec(
                &std::env::temp_dir(),
                json!({"command": command}),
                CancellationToken::new(),
            )
            .await;
            let captures = ["stdout", "stderr"].map(|stream| {
                output.value[format!("{stream}_full_path")]
                    .as_str()
                    .map(|path| {
                        let bytes = fs::read(path);
                        let mode = fs::metadata(path).map(|meta| meta.permissions().mode() & 0o777);
                        fs::remove_file(path).unwrap();
                        (bytes.unwrap(), mode.unwrap())
                    })
            });
            assert!(!output.is_error, "{}", output.value);
            assert_eq!(output.value["exit_code"], 0);
            for ((stream, expected), capture) in
                ["stdout", "stderr"].into_iter().zip(expected).zip(captures)
            {
                assert_eq!(output.value[format!("{stream}_lossy")], lossy);
                assert_eq!(output.value[format!("{stream}_complete")], true);
                assert_eq!(output.value[format!("{stream}_truncated")], false);
                assert_eq!(output.value[format!("{stream}_omitted_bytes")], 0);
                assert_eq!(output.value[format!("{stream}_full_error")], Value::Null);
                assert_eq!(output.value[stream], "\u{fffd}");
                assert_eq!(capture, lossy.then_some((expected, 0o600)));
            }
        }
    }

    #[tokio::test]
    async fn utf8_tail_cut_reports_decoding_loss_separately_from_omitted_bytes() {
        let output = run_exec(
            &std::env::temp_dir(),
            json!({"command": "printf '\\303\\251'; head -c 65535 /dev/zero | tr '\\000' x"}),
            CancellationToken::new(),
        )
        .await;
        let capture = output.value["stdout_full_path"]
            .as_str()
            .map(|path| {
                let bytes = fs::read(path).unwrap();
                fs::remove_file(path).unwrap();
                bytes
            })
            .expect("raw capture");
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
        let output = run_exec(
            &std::env::temp_dir(),
            json!({"command":"yes x | head -c 70000; printf '\\377\\377\\000END_MARKER\\n'; yes e | head -c 70000 >&2; printf '\\377\\377\\000ERROR_MARKER\\n' >&2"}),
            CancellationToken::new(),
        )
        .await;
        // Capture and delete both owned artifacts before any assertions: a
        // failing verdict must not leak the other stream's private file.
        let captures = ["stdout", "stderr"].map(|stream| {
            output.value[format!("{stream}_full_path")]
                .as_str()
                .map(|path| {
                    let bytes = fs::read(path);
                    let permissions =
                        fs::metadata(path).map(|meta| meta.permissions().mode() & 0o777);
                    let removed = fs::remove_file(path);
                    (bytes, permissions, removed)
                })
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
            let (saved, permissions, removed) = capture.expect("complete raw capture");
            removed.unwrap();
            assert_eq!(permissions.unwrap(), 0o600);
            assert_eq!(saved.unwrap(), expected);
            assert_eq!(output.value[format!("{stream}_truncated")], true);
            assert_eq!(output.value[format!("{stream}_lossy")], true);
            assert_eq!(output.value[format!("{stream}_complete")], true);
            assert_eq!(output.value[format!("{stream}_full_error")], Value::Null);
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
        let output = run_exec(
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
    async fn incomplete_long_capture_does_not_claim_a_full_output_file() {
        let output = run_exec(
            &std::env::temp_dir(),
            json!({"command":"sleep 2 & yes x | head -c 70000"}),
            CancellationToken::new(),
        )
        .await;
        assert!(!output.is_error, "{}", output.value);
        assert_eq!(output.value["stdout_truncated"], true);
        assert_eq!(output.value["stdout_omitted_bytes"], Value::Null);
        assert_eq!(output.value["stdout_full_path"], Value::Null);
    }
    #[tokio::test]
    async fn post_exit_output_activity_keeps_the_pipe_open_until_idle() {
        // A native producer sleeping 50ms can actually remain quiet beyond the
        // 100ms idle contract under load. Model readable arrivals in poll_read:
        // even if scheduling is delayed, available bytes beat an expired timer.
        // Native held-pipe, cancellation and spool custody have separate coverage.
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
            OutputCapture::start(reader).finish(&CancellationToken::new(), false),
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
            OutputCapture::start(reader).finish(&stop, false),
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
            OutputCapture::start(reader).finish(&stop, true),
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
            OutputCapture::start(tokio::io::repeat(b'x')).finish(&CancellationToken::new(), true),
        )
        .await
        .expect("stopped capture followed continuously ready output");
        assert!(!captured.complete);
        assert_eq!(captured.bytes, vec![b'x'; MAX_OUTPUT_BYTES]);
        assert_eq!(captured.omitted_bytes, None);
        assert_eq!(captured.full_path, None);
    }
    #[tokio::test]
    async fn shell_deadline_bounds_output_before_and_after_direct_exit() {
        for direct_exits in [false, true] {
            let root =
                std::env::temp_dir().join(format!("ion-timeout-drain-{}", uuid::Uuid::now_v7()));
            fs::create_dir(&root).unwrap();
            let command = format!(
                "(trap '' TERM; printf ready > ready; while :; do printf x || exit; sleep 0.01; done) & {}",
                if direct_exits {
                    "while [ ! -e ready ]; do sleep 0.01; done; exit 0"
                } else {
                    "exec sleep 30"
                }
            );
            let output = tokio::time::timeout(
                Duration::from_secs(3),
                run_exec(
                    &root,
                    json!({"command": command, "timeout_ms": 500}),
                    CancellationToken::new(),
                ),
            )
            .await
            .expect("timed-out command followed descendant output");
            assert!(root.join("ready").exists());
            assert!(output.is_error);
            assert_eq!(output.value["timed_out"], true);
            assert_eq!(output.value["cancelled"], false);
            if direct_exits {
                assert_eq!(output.value["exit_code"], 0);
                assert_eq!(output.value["signal"], Value::Null);
            } else {
                assert_eq!(output.value["exit_code"], Value::Null);
                assert_eq!(output.value["signal"], 15);
            }
            assert_eq!(output.value["wait_error"], Value::Null);
            assert!(!output.value["stdout"].as_str().unwrap().is_empty());
            assert_eq!(output.value["stdout_truncated"], true);
            assert_eq!(output.value["stdout_omitted_bytes"], Value::Null);
            fs::remove_dir_all(root).unwrap();
        }
    }
    #[tokio::test]
    async fn shell_timeout_is_opt_in_and_accepts_long_explicit_limits() {
        for args in [
            json!({"command":"printf ok"}),
            json!({"command":"printf ok","timeout_ms":600000}),
        ] {
            let output = run_exec(&std::env::temp_dir(), args, CancellationToken::new()).await;
            assert!(!output.is_error, "{}", output.value);
            assert_eq!(output.value["stdout"], "ok");
        }
    }

    #[tokio::test]
    async fn shell_uses_bash_when_available() {
        if !Path::new("/bin/bash").is_file() {
            return;
        }
        let output = run_exec(
            &std::env::temp_dir(),
            json!({"command":"set -o pipefail; false | true"}),
            CancellationToken::new(),
        )
        .await;
        assert_eq!(output.value["exit_code"], 1);
        assert!(output.is_error);
    }
}
