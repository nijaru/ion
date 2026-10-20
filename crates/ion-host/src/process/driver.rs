//! One native process supervisor. It owns the direct child, its process group,
//! bounded input queue and output readers until all are joined. Pipe and PTY
//! programs share the owner, start receipt, status and join contract.
use std::{
    path::PathBuf,
    process::{ExitStatus, Stdio},
    sync::{Arc, Mutex as StdMutex},
};

use futures_util::FutureExt;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::ChildStdin,
    sync::{mpsc, oneshot},
};
use tokio_util::sync::CancellationToken;

use super::{
    Backend, Capture,
    ring::Ring,
    screen::{Dimensions, Terminal},
};
use crate::local_tools::command;

pub(super) const INPUT_QUEUE: usize = 8;

pub(super) enum StartReceipt {
    Started,
    Failed(String),
    Cancelled,
}

#[derive(Clone)]
pub(super) struct Exit {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub wait_error: Option<String>,
}

#[derive(Default)]
struct Status {
    started: bool,
    start_error: Option<String>,
    exit: Option<Exit>,
    joined: bool,
    supervision_error: Option<String>,
}

pub(super) struct ProcessState {
    pub id: uuid::Uuid,
    pub cwd: PathBuf,
    pub command: String,
    pub environment: Arc<crate::CommandEnvironment>,
    pub backend: Backend,
    pub stop: CancellationToken,
    pub input_stop: CancellationToken,
    pub capture: Capture,
    input: StdMutex<Option<mpsc::Sender<InputRequest>>>,
    resize: StdMutex<Option<mpsc::Sender<ResizeRequest>>>,
    lease: StdMutex<Option<uuid::Uuid>>,
    lease_control: StdMutex<Option<mpsc::Sender<super::lease::LeaseRequest>>>,
    status: StdMutex<Status>,
}

pub(super) struct InputRequest {
    /// `None` closes stdin after every earlier queued request.
    pub data: Option<Vec<u8>>,
    pub stop: CancellationToken,
    pub reply: oneshot::Sender<InputAck>,
}

#[derive(Debug)]
pub(super) struct InputAck {
    pub bytes_written: usize,
    pub error: Option<String>,
    pub cancelled: bool,
}

pub(super) struct ResizeRequest {
    pub dimensions: Dimensions,
    pub reply: oneshot::Sender<Result<(), String>>,
}

pub(super) struct StatusView {
    pub started: bool,
    pub start_error: Option<String>,
    pub exit: Option<Exit>,
    pub joined: bool,
    pub supervision_error: Option<String>,
}

impl ProcessState {
    pub(super) fn new(
        id: uuid::Uuid,
        cwd: PathBuf,
        command: String,
        backend: Backend,
        dimensions: Option<Dimensions>,
        environment: Arc<crate::CommandEnvironment>,
    ) -> Self {
        let capture = match backend {
            Backend::Pipe => Capture::Pipe {
                stdout: Arc::new(Ring::default()),
                stderr: Arc::new(Ring::default()),
            },
            Backend::Pty => Capture::Pty {
                raw: Arc::new(Ring::default()),
                screen: Arc::new(Terminal::new(
                    dimensions.expect("PTY start carries validated dimensions"),
                )),
            },
        };
        Self {
            id,
            cwd,
            command,
            environment,
            backend,
            stop: CancellationToken::new(),
            input_stop: CancellationToken::new(),
            capture,
            input: StdMutex::new(None),
            resize: StdMutex::new(None),
            lease: StdMutex::new(None),
            lease_control: StdMutex::new(None),
            status: StdMutex::new(Status::default()),
        }
    }

    pub(super) fn input(&self) -> Option<mpsc::Sender<InputRequest>> {
        if !self.lock_status().started {
            return None;
        }
        self.input.lock().ok().and_then(|input| input.clone())
    }

    pub(super) fn resize(&self) -> Option<mpsc::Sender<ResizeRequest>> {
        let status = self.lock_status();
        if !status.started || status.exit.is_some() || status.joined {
            return None;
        }
        drop(status);
        self.resize.lock().ok().and_then(|resize| resize.clone())
    }

    pub(super) fn status(&self) -> StatusView {
        let status = self.lock_status();
        StatusView {
            started: status.started,
            start_error: status.start_error.clone(),
            exit: status.exit.clone(),
            joined: status.joined,
            supervision_error: status.supervision_error.clone(),
        }
    }

    pub(super) fn mark_joined(&self, failure: Option<String>) {
        let mut status = self.lock_status();
        status.joined = true;
        if let Some(failure) = failure {
            status.supervision_error = Some(match status.supervision_error.take() {
                Some(previous) => format!("{previous}; {failure}"),
                None => failure,
            });
        }
    }

    fn lock_status(&self) -> std::sync::MutexGuard<'_, Status> {
        match self.status.lock() {
            Ok(status) => status,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    pub(super) fn set_start_error(&self, error: impl Into<String>) {
        self.lock_status().start_error = Some(error.into());
    }

    pub(super) fn started(&self) {
        self.lock_status().started = true;
    }

    pub(super) fn set_input(&self, sender: mpsc::Sender<InputRequest>) {
        if let Ok(mut input) = self.input.lock() {
            *input = Some(sender);
        }
    }

    pub(super) fn clear_input(&self) {
        if let Ok(mut input) = self.input.lock() {
            *input = None;
        }
    }

    pub(super) fn set_resize(&self, sender: mpsc::Sender<ResizeRequest>) {
        if let Ok(mut resize) = self.resize.lock() {
            *resize = Some(sender);
        }
    }

    pub(super) fn clear_resize(&self) {
        if let Ok(mut resize) = self.resize.lock() {
            *resize = None;
        }
    }

    pub(super) fn set_lease_control(&self, sender: mpsc::Sender<super::lease::LeaseRequest>) {
        if let Ok(mut control) = self.lease_control.lock() {
            *control = Some(sender);
        }
    }

    pub(super) fn lease_control(&self) -> Option<mpsc::Sender<super::lease::LeaseRequest>> {
        self.lease_control
            .lock()
            .ok()
            .and_then(|control| control.clone())
    }

    pub(super) fn clear_lease_control(&self) {
        if let Ok(mut control) = self.lease_control.lock() {
            *control = None;
        }
    }

    pub(super) fn lease_owner(&self) -> Option<uuid::Uuid> {
        self.lease.lock().ok().and_then(|lease| *lease)
    }

    pub(super) fn set_lease(&self, token: Option<uuid::Uuid>) {
        if let Ok(mut lease) = self.lease.lock() {
            *lease = token;
        }
    }

    pub(super) fn set_exit(&self, exit: Exit) {
        self.lock_status().exit = Some(exit);
    }
}

/// Wait for the registry's start permit, then own the direct child until the
/// child, writer and both readers have really settled.
pub(super) async fn run(
    state: Arc<ProcessState>,
    go: oneshot::Receiver<()>,
    receipt: oneshot::Sender<StartReceipt>,
    start_stop: CancellationToken,
    root_stop: CancellationToken,
) -> Result<(), String> {
    if go.await.is_err() {
        state.set_start_error("process start was abandoned before it could begin");
        return Ok(());
    }
    if start_stop.is_cancelled() || state.stop.is_cancelled() || root_stop.is_cancelled() {
        state.set_start_error("cancelled before process start");
        let _ = receipt.send(StartReceipt::Cancelled);
        return Ok(());
    }

    match state.backend {
        Backend::Pipe => run_pipe(state, receipt, root_stop).await,
        Backend::Pty => super::pty::run(state, receipt, root_stop).await,
    }
}

async fn run_pipe(
    state: Arc<ProcessState>,
    receipt: oneshot::Sender<StartReceipt>,
    root_stop: CancellationToken,
) -> Result<(), String> {
    let Capture::Pipe { stdout, stderr } = &state.capture else {
        unreachable!("pipe driver owns a pipe capture");
    };
    let mut command = state.environment.command(&state.command);
    command
        .current_dir(&state.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            state.set_start_error(format!("command did not start: {error}"));
            let _ = receipt.send(StartReceipt::Failed(format!(
                "command did not start: {error}"
            )));
            return Ok(());
        }
    };
    state.started();

    let stdout_pipe = child.stdout.take().expect("piped stdout");
    let stderr_pipe = child.stderr.take().expect("piped stderr");
    let stdin = child.stdin.take().expect("piped stdin");
    let reader_exit = CancellationToken::new();
    let (input_tx, input_rx) = mpsc::channel(INPUT_QUEUE);
    state.set_input(input_tx);
    let _ = receipt.send(StartReceipt::Started);

    let supervision = supervise(&state, &mut child, &root_stop, &reader_exit);
    // These are concurrent futures inside the one retained driver, not detached
    // tasks. A panic drops every pipe and pending acknowledgement together. Read
    // through graceful stop and then a bounded drain, rather than dropping the
    // shutdown handler's final output as soon as stop is requested.
    let ((), _, _, ()) = tokio::join!(
        supervision,
        read_stream(stdout_pipe, stdout.clone(), reader_exit.clone()),
        read_stream(stderr_pipe, stderr.clone(), reader_exit.clone()),
        write_input(stdin, input_rx, state.input_stop.clone()),
    );
    state.clear_input();
    let mut failures = Vec::new();
    if let Some(error) = state.status().exit.and_then(|exit| exit.wait_error) {
        failures.push(error);
    }
    for (name, capture) in [("stdout", stdout), ("stderr", stderr)] {
        if let Some(error) = capture.meta().capture_error {
            failures.push(format!("{name} capture failed: {error}"));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

/// Both backends use one child-lifetime policy. Stop closes input before
/// signalling; output keeps draining concurrently until the actual child wait
/// settles. Never signal a group after its owned child is known reaped.
pub(super) async fn supervise(
    state: &ProcessState,
    child: &mut tokio::process::Child,
    root_stop: &CancellationToken,
    reader_exit: &CancellationToken,
) {
    let pid = child
        .id()
        .and_then(|id| i32::try_from(id).ok())
        .and_then(rustix::process::Pid::from_raw);
    let status = tokio::select! {
        biased;
        status = child.wait() => Some(status),
        () = state.stop.cancelled() => None,
        () = root_stop.cancelled() => None,
    };
    match status {
        Some(Ok(status)) => state.set_exit(status_exit(status)),
        Some(Err(error)) => state.set_exit(Exit {
            exit_code: None,
            signal: None,
            wait_error: Some(format!(
                "process started, but direct-child exit is unknown: {error}"
            )),
        }),
        None => {
            state.input_stop.cancel();
            match command::stop_child(child, pid).await {
                Ok(status) => state.set_exit(status_exit(status)),
                Err(error) => state.set_exit(Exit {
                    exit_code: None,
                    signal: None,
                    wait_error: Some(format!("process stop failed: {error}")),
                }),
            }
        }
    }
    state.input_stop.cancel();
    reader_exit.cancel();
}

pub(super) fn status_exit(status: ExitStatus) -> Exit {
    use std::os::unix::process::ExitStatusExt;
    Exit {
        exit_code: status.code(),
        signal: status.signal(),
        wait_error: None,
    }
}

async fn read_stream<R: AsyncRead + Unpin>(
    mut pipe: R,
    ring: Arc<Ring>,
    exited: CancellationToken,
) -> bool {
    let mut chunk = [0u8; 8192];
    let mut deadline: Option<tokio::time::Instant> = None;
    loop {
        let until = deadline;
        let idle = async move {
            match until {
                Some(until) => tokio::time::sleep_until(until).await,
                None => std::future::pending().await,
            }
        };
        tokio::pin!(idle);
        tokio::select! {
            biased;
            () = exited.cancelled(), if deadline.is_none() => {
                deadline = Some(tokio::time::Instant::now() + command::POST_EXIT_OUTPUT_IDLE);
            }
            () = &mut idle, if deadline.is_some() => {
                // The drain is a total bound, not an idle timeout: an escaped
                // descendant may write forever. Still observe a ready EOF or
                // final chunk before closing, without waiting or following an
                // unbounded ready stream beyond the deadline.
                match pipe.read(&mut chunk).now_or_never() {
                    Some(Ok(0)) => { ring.mark_eof(); return true; }
                    Some(Ok(n)) => ring.append(&chunk[..n]),
                    Some(Err(error)) if error.kind() == std::io::ErrorKind::Interrupted => {},
                    Some(Err(error)) => ring.mark_error(format!("output pipe read failed: {error}")),
                    None => {},
                }
                return false;
            }
            result = pipe.read(&mut chunk) => match result {
                Ok(0) => {
                    ring.mark_eof();
                    return true;
                }
                Ok(n) => ring.append(&chunk[..n]),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                    tokio::task::yield_now().await;
                }
                Err(error) => {
                    ring.mark_error(format!("output pipe read failed: {error}"));
                    return false;
                }
            },
        }
    }
}

async fn write_input(
    stdin: ChildStdin,
    mut input: mpsc::Receiver<InputRequest>,
    stop: CancellationToken,
) {
    let mut stdin = Some(stdin);
    loop {
        let request = tokio::select! {
            biased;
            () = stop.cancelled() => break,
            request = input.recv() => request,
        };
        let Some(request) = request else {
            break;
        };
        deliver(&mut stdin, request, &stop).await;
        if stdin.is_none() {
            break;
        }
    }
    drop(stdin);
    input.close();
    while let Ok(request) = input.try_recv() {
        let _ = request.reply.send(InputAck {
            bytes_written: 0,
            error: Some("process input is closed".into()),
            cancelled: stop.is_cancelled() || request.stop.is_cancelled(),
        });
    }
}

async fn deliver(stdin: &mut Option<ChildStdin>, request: InputRequest, stop: &CancellationToken) {
    let InputRequest {
        data,
        stop: request_stop,
        reply,
    } = request;
    let Some(data) = data else {
        let result = if request_stop.is_cancelled() {
            Err((true, "input request cancelled before EOF".into()))
        } else if stop.is_cancelled() {
            Err((true, "process input stopped before EOF".into()))
        } else {
            // AsyncWrite::shutdown is a no-op for Unix child pipes. EOF requires
            // closing our actual write handle before acknowledging it.
            drop(stdin.take());
            Ok(())
        };
        let _ = reply.send(match result {
            Ok(()) => InputAck {
                bytes_written: 0,
                error: None,
                cancelled: false,
            },
            Err((cancelled, error)) => InputAck {
                bytes_written: 0,
                error: Some(error),
                cancelled,
            },
        });
        return;
    };

    let stdin = stdin.as_mut().expect("writer closes its queue after EOF");
    let mut written = 0usize;
    let mut cancelled = false;
    let mut error = None;
    while written < data.len() {
        let result = tokio::select! {
            biased;
            () = request_stop.cancelled() => {
                cancelled = true;
                break;
            }
            () = stop.cancelled() => {
                cancelled = true;
                error = Some("process input stopped".to_owned());
                break;
            }
            result = stdin.write(&data[written..]) => result,
        };
        match result {
            Ok(0) => {
                error = Some("stdin write made no progress".to_owned());
                break;
            }
            Ok(count) => written += count,
            Err(write_error) => {
                error = Some(format!(
                    "stdin write failed after {written} bytes: {write_error}"
                ));
                break;
            }
        }
    }
    let _ = reply.send(InputAck {
        bytes_written: written,
        error,
        cancelled,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io,
        pin::Pin,
        task::{Context, Poll},
    };
    use tokio::io::ReadBuf;

    struct EofAt(tokio::time::Instant);
    impl AsyncRead for EofAt {
        fn poll_read(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            if tokio::time::Instant::now() >= self.0 {
                Poll::Ready(Ok(()))
            } else {
                // The production drain deadline wakes this select. At that
                // instant EOF and the deadline are simultaneously ready.
                Poll::Pending
            }
        }
    }

    #[tokio::test]
    async fn ready_eof_is_observed_when_the_total_drain_deadline_expires() {
        let ring = Arc::new(Ring::default());
        let exited = CancellationToken::new();
        exited.cancel();
        let pipe = EofAt(tokio::time::Instant::now() + command::POST_EXIT_OUTPUT_IDLE);
        assert!(read_stream(pipe, ring.clone(), exited).await);
        assert!(ring.meta().complete);
    }
}
