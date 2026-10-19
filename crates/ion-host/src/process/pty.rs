//! PTY-backed native process supervision.
//!
//! The master descriptor is opened with atomic close-on-exec, registered with
//! Tokio before spawn, and retained only by the single driver future. The child
//! gets a fresh session, controlling terminal and foreground process group.
//! Output is one merged raw stream; terminal replies are bounded and written
//! ahead of model input without becoming counted model-write bytes.
use std::{collections::VecDeque, io, os::fd::OwnedFd, process::Stdio};

use tokio::{
    process::Command,
    sync::{mpsc, oneshot},
};
use tokio_util::sync::CancellationToken;

use super::{
    Capture,
    driver::{InputAck, InputRequest, ProcessState, ResizeRequest, StartReceipt},
    lease::{LeaseRequest, attachment_input},
    screen::{Dimensions, Terminal},
};
use crate::local_tools::command;

/// Bound on automatic terminal replies waiting for the master to become
/// writable. Crossing it fails supervision instead of dropping a required
/// device-status or cursor reply.
const MAX_PENDING_REPLY_BYTES: usize = 8 * 1024;

pub(super) async fn run(
    state: std::sync::Arc<ProcessState>,
    receipt: oneshot::Sender<StartReceipt>,
    root_stop: CancellationToken,
) -> Result<(), String> {
    let Capture::Pty { raw, screen } = &state.capture else {
        unreachable!("PTY driver owns a PTY capture");
    };
    let dimensions = screen.dimensions();
    let (master, slave) = match open_pair(dimensions) {
        Ok(pair) => pair,
        Err(error) => {
            state.set_start_error(format!("terminal did not start: {error}"));
            let _ = receipt.send(StartReceipt::Failed(format!(
                "terminal did not start: {error}"
            )));
            return Ok(());
        }
    };
    // Register the readiness source before any child exists, so a registration
    // failure cannot abandon an already-started program.
    let master = match tokio::io::unix::AsyncFd::new(master) {
        Ok(master) => master,
        Err(error) => {
            state.set_start_error(format!("terminal did not start: {error}"));
            let _ = receipt.send(StartReceipt::Failed(format!(
                "terminal did not start: {error}"
            )));
            return Ok(());
        }
    };
    let stdin = match slave.try_clone() {
        Ok(fd) => Stdio::from(fd),
        Err(error) => return fail_spawn(&state, receipt, error),
    };
    let stdout = match slave.try_clone() {
        Ok(fd) => Stdio::from(fd),
        Err(error) => return fail_spawn(&state, receipt, error),
    };
    let stderr = match slave.try_clone() {
        Ok(fd) => Stdio::from(fd),
        Err(error) => return fail_spawn(&state, receipt, error),
    };
    let control = match slave.try_clone() {
        Ok(fd) => fd,
        Err(error) => return fail_spawn(&state, receipt, error),
    };
    drop(slave);

    let mut command = Command::new(command::shell_program());
    command
        .arg("-c")
        .arg(&state.command)
        .current_dir(&state.cwd)
        .stdin(stdin)
        .stdout(stdout)
        .stderr(stderr)
        .kill_on_drop(true);
    terminal_environment(&mut command);
    // SAFETY: the closure runs between fork and exec. It performs only
    // async-signal-safe rustix syscalls, allocates nothing, takes no locks and
    // does not touch process state shared with other threads. The child becomes
    // a new session leader; its pid is therefore its process-group id, and the
    // inherited slave descriptor becomes the controlling terminal. We do not
    // call process_group(0) first because that would make setsid fail.
    #[allow(unsafe_code)]
    unsafe {
        command.pre_exec(move || {
            rustix::process::setsid()?;
            rustix::process::ioctl_tiocsctty(&control)?;
            rustix::termios::tcsetpgrp(&control, rustix::process::getpid())?;
            Ok(())
        });
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return fail_spawn(&state, receipt, error),
    };
    // The pre-exec closure held a parent copy of the slave for the child setup.
    // Dropping the command now releases it; the child's stdio copies remain.
    drop(command);
    state.started();

    let reader_exit = CancellationToken::new();
    let (input_tx, input_rx) = mpsc::channel(super::driver::INPUT_QUEUE);
    let (resize_tx, resize_rx) = mpsc::channel(super::driver::INPUT_QUEUE);
    let (lease_tx, lease_rx) = mpsc::channel(super::driver::INPUT_QUEUE);
    state.set_input(input_tx);
    state.set_resize(resize_tx);
    state.set_lease_control(lease_tx);
    let _ = receipt.send(StartReceipt::Started);

    let io_exit = reader_exit.clone();
    let supervision = super::driver::supervise(&state, &mut child, &root_stop, &reader_exit);
    let io = drive(
        master,
        screen.clone(),
        raw.clone(),
        state.clone(),
        IoControl {
            input: input_rx,
            resize: resize_rx,
            lease: lease_rx,
            input_stop: state.input_stop.clone(),
            exited: io_exit,
            process_stop: state.stop.clone(),
        },
    );
    let ((), io_result) = tokio::join!(supervision, io);
    state.clear_input();
    state.clear_resize();
    state.clear_lease_control();
    state.set_lease(None);
    let mut failures = Vec::new();
    if let Some(error) = state.status().exit.and_then(|exit| exit.wait_error) {
        failures.push(error);
    }
    if let Some(error) = raw.meta().capture_error {
        failures.push(format!("terminal capture failed: {error}"));
    }
    if let Err(error) = io_result {
        failures.push(error);
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

/// The child has Ion's virtual terminal, not the host emulator's device,
/// socket or multiplexer. Keep ordinary host configuration/authentication;
/// this is capability correctness, not an environment/privacy sandbox.
fn terminal_environment(command: &mut Command) {
    command.env("TERM", "xterm-256color");
    for (name, _) in std::env::vars_os() {
        if name.to_str().is_some_and(outer_terminal_variable) {
            command.env_remove(name);
        }
    }
}

fn outer_terminal_variable(name: &str) -> bool {
    matches!(
        name,
        "TERM_PROGRAM"
            | "TERM_PROGRAM_VERSION"
            | "TERM_SESSION_ID"
            | "TERMINAL_EMULATOR"
            | "LC_TERMINAL"
            | "LC_TERMINAL_VERSION"
            | "STY"
            | "WINDOW"
            | "WINDOWID"
            | "TERMCAP"
            | "COLUMNS"
            | "LINES"
            | "VTE_VERSION"
            | "KONSOLE_VERSION"
            | "INSIDE_EMACS"
            | "SSH_TTY"
            | "SSH_CONNECTION"
            | "SSH_CLIENT"
            | "ZELLIJ"
            | "TMUX"
    ) || [
        "TMUX_",
        "ITERM_",
        "KITTY_",
        "WEZTERM_",
        "GNOME_TERMINAL_",
        "KONSOLE_",
        "ZELLIJ_",
        "GHOSTTY_",
        "ALACRITTY_",
        "REX_",
    ]
    .iter()
    .any(|prefix| name.starts_with(prefix))
}

fn fail_spawn(
    state: &ProcessState,
    receipt: oneshot::Sender<StartReceipt>,
    error: io::Error,
) -> Result<(), String> {
    state.set_start_error(format!("command did not start: {error}"));
    let _ = receipt.send(StartReceipt::Failed(format!(
        "command did not start: {error}"
    )));
    Ok(())
}

fn open_pair(dimensions: Dimensions) -> Result<(OwnedFd, OwnedFd), String> {
    let master = rustix::fs::open(
        c"/dev/ptmx",
        rustix::fs::OFlags::RDWR | rustix::fs::OFlags::NOCTTY | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(|error| format!("open /dev/ptmx failed: {error}"))?;
    rustix::pty::grantpt(&master)
        .map_err(|error| format!("grant terminal access failed: {error}"))?;
    rustix::pty::unlockpt(&master).map_err(|error| format!("unlock terminal failed: {error}"))?;
    let slave_name = rustix::pty::ptsname(&master, Vec::new())
        .map_err(|error| format!("terminal name failed: {error}"))?;
    let slave = rustix::fs::open(
        &slave_name,
        rustix::fs::OFlags::RDWR | rustix::fs::OFlags::NOCTTY | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(|error| format!("open terminal slave failed: {error}"))?;
    rustix::termios::tcsetwinsize(
        &slave,
        rustix::termios::Winsize {
            ws_row: dimensions.rows,
            ws_col: dimensions.columns,
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    )
    .map_err(|error| format!("set initial terminal size failed: {error}"))?;
    let flags = rustix::fs::fcntl_getfl(&master)
        .map_err(|error| format!("inspect terminal master flags failed: {error}"))?;
    rustix::fs::fcntl_setfl(&master, flags | rustix::fs::OFlags::NONBLOCK)
        .map_err(|error| format!("make terminal master nonblocking failed: {error}"))?;
    Ok((master, slave))
}

struct PendingInput {
    request: InputRequest,
    data: Vec<u8>,
    written: usize,
}

impl PendingInput {
    fn new(mut request: InputRequest) -> Result<Self, (oneshot::Sender<InputAck>, InputAck)> {
        if request.stop.is_cancelled() {
            let ack = InputAck {
                bytes_written: 0,
                error: Some("input request cancelled before delivery".into()),
                cancelled: true,
            };
            return Err((request.reply, ack));
        }
        let Some(data) = request.data.take() else {
            let ack = InputAck {
                bytes_written: 0,
                error: Some(
                    "PTY EOF is terminal input interpreted by the line discipline, not a half-close"
                        .into(),
                ),
                cancelled: false,
            };
            return Err((request.reply, ack));
        };
        Ok(Self {
            request,
            data,
            written: 0,
        })
    }

    fn acknowledge(self) {
        let _ = self.request.reply.send(InputAck {
            bytes_written: self.written,
            error: None,
            cancelled: false,
        });
    }

    fn fail(self, reason: &str, cancelled: bool) {
        let _ = self.request.reply.send(InputAck {
            bytes_written: self.written,
            error: Some(reason.into()),
            cancelled,
        });
    }
}

struct WriteQueue {
    replies: VecDeque<u8>,
    pending: Option<PendingInput>,
}

impl WriteQueue {
    fn new() -> Self {
        Self {
            replies: VecDeque::new(),
            pending: None,
        }
    }

    fn has_pending(&self) -> bool {
        !self.replies.is_empty() || self.pending.is_some()
    }

    fn fail_input(&mut self, reason: &str, cancelled: bool) {
        if let Some(pending) = self.pending.take() {
            pending.fail(reason, cancelled);
        }
    }

    /// Write one reply or input step. Replies are always attempted first, so a
    /// partially delivered model write cannot block a required terminal reply.
    /// A returned `true` means durable progress was made; `WouldBlock` is only
    /// returned when nothing could be written.
    fn try_flush(&mut self, fd: &OwnedFd) -> io::Result<bool> {
        if !self.has_pending() {
            return Ok(false);
        }
        let mut progressed = false;
        if !self.replies.is_empty() {
            let (front, back) = self.replies.as_slices();
            let bytes = if front.is_empty() { back } else { front };
            match write_master(fd, bytes) {
                Ok(0) => return Err(io::Error::other("terminal write made no progress")),
                Ok(count) => {
                    self.replies.drain(..count);
                    progressed = true;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    return Err(error);
                }
                Err(error) => return Err(error),
            }
        }
        if !self.replies.is_empty() {
            return Ok(progressed);
        }
        let mut completed = false;
        let mut failure = None;
        if let Some(pending_input) = self.pending.as_mut() {
            if pending_input.written == pending_input.data.len() {
                completed = true;
            } else {
                match write_master(fd, &pending_input.data[pending_input.written..]) {
                    Ok(0) => {
                        failure = Some("terminal input write made no progress".to_owned());
                    }
                    Ok(count) => {
                        pending_input.written += count;
                        progressed = true;
                        completed = pending_input.written == pending_input.data.len();
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    Err(error) => {
                        failure = Some(format!(
                            "terminal input write failed after {} bytes: {error}",
                            pending_input.written
                        ));
                    }
                }
            }
        }
        if let Some(reason) = failure {
            if let Some(pending_input) = self.pending.take() {
                let written = pending_input.written;
                let _ = pending_input.request.reply.send(InputAck {
                    bytes_written: written,
                    error: Some(reason),
                    cancelled: false,
                });
            }
            return Ok(true);
        }
        if completed {
            let pending_input = self.pending.take().expect("pending input exists");
            pending_input.acknowledge();
            return Ok(true);
        }
        if progressed {
            Ok(true)
        } else {
            Err(io::ErrorKind::WouldBlock.into())
        }
    }
}

struct IoControl {
    input: mpsc::Receiver<InputRequest>,
    resize: mpsc::Receiver<ResizeRequest>,
    lease: mpsc::Receiver<LeaseRequest>,
    input_stop: CancellationToken,
    exited: CancellationToken,
    process_stop: CancellationToken,
}

async fn drive(
    master: tokio::io::unix::AsyncFd<OwnedFd>,
    terminal: std::sync::Arc<Terminal>,
    raw: std::sync::Arc<super::ring::Ring>,
    state: std::sync::Arc<ProcessState>,
    control: IoControl,
) -> Result<(), String> {
    let IoControl {
        mut input,
        mut resize,
        mut lease,
        input_stop,
        exited,
        process_stop,
    } = control;
    let mut chunk = [0u8; 8192];
    let mut queue = WriteQueue::new();
    let mut input_open = true;
    let mut resize_open = true;
    let mut active_stop: Option<CancellationToken> = None;
    let mut draining: Option<tokio::time::Instant> = None;
    let mut lease_token: Option<uuid::Uuid> = None;

    let outcome = loop {
        if input_open && input_stop.is_cancelled() {
            input_open = false;
            queue.fail_input("process input stopped", true);
            active_stop = None;
            close_input_queue(&mut input, "process input is closed");
        }
        if draining.is_none() && exited.is_cancelled() {
            draining = Some(tokio::time::Instant::now() + command::POST_EXIT_OUTPUT_IDLE);
            input_open = false;
            input_stop.cancel();
            queue.fail_input("process exited before input delivery completed", true);
            queue.replies.clear();
            active_stop = None;
            close_input_queue(&mut input, "process exited before input delivery");
        }
        if active_stop
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
        {
            if let Some(pending) = queue.pending.take() {
                pending.fail("input request cancelled", true);
            }
            active_stop = None;
        }

        // A total drain bound must apply even while the descriptor stays ready.
        // Observe one final ready chunk/EOF, without following a noisy descendant.
        if draining.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
            break capture_final(master.get_ref(), &terminal, &raw);
        }

        match queue.try_flush(master.get_ref()) {
            Ok(true) => {
                if queue.pending.is_none() {
                    active_stop = None;
                }
                continue;
            }
            Ok(false) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => {
                process_stop.cancel();
                break Err(format!("terminal write failed: {error}"));
            }
        }

        let deadline = draining;
        let idle = async move {
            match deadline {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending().await,
            }
        };
        tokio::pin!(idle);
        let mut stop_wait = active_stop.clone();
        tokio::select! {
            biased;
            () = input_stop.cancelled(), if input_open => {
                input_open = false;
                queue.fail_input("process input stopped", true);
                active_stop = None;
                close_input_queue(&mut input, "process input is closed");
            }
            () = exited.cancelled(), if draining.is_none() => {
                draining = Some(tokio::time::Instant::now() + command::POST_EXIT_OUTPUT_IDLE);
                input_open = false;
                input_stop.cancel();
                queue.fail_input("process exited before input delivery completed", true);
                queue.replies.clear();
                active_stop = None;
                close_input_queue(&mut input, "process exited before input delivery");
            }
            () = async move {
                match stop_wait.take() {
                    Some(stop) => stop.cancelled().await,
                    None => std::future::pending().await,
                }
            }, if active_stop.is_some() => {
                if let Some(pending) = queue.pending.take() {
                    pending.fail("input request cancelled", true);
                }
                active_stop = None;
            }
            () = idle, if draining.is_some() => continue,
            request = lease.recv() => {
                match request {
                    Some(LeaseRequest::Acquire { token, reply }) => {
                        let live = draining.is_none() && !exited.is_cancelled() && !process_stop.is_cancelled();
                        if !live {
                            let _ = reply.send(Err("process terminal is not live".into()));
                        } else if lease_token.is_some() {
                            let _ = reply.send(Err("terminal already has an attached human".into()));
                        } else {
                            // Serialize with the writer: keep the delivered prefix,
                            // reject the remainder and every queued request.
                            transfer_to_lease(&mut queue, &mut input);
                            active_stop = None;
                            lease_token = Some(token);
                            state.set_lease(Some(token));
                            let _ = reply.send(Ok(()));
                        }
                    }
                    Some(LeaseRequest::Write { token, data, reply }) => {
                        let live = draining.is_none() && !exited.is_cancelled() && !process_stop.is_cancelled();
                        if lease_token != Some(token) {
                            let _ = reply.send(InputAck {
                                bytes_written: 0,
                                error: Some("attachment lease is stale".into()),
                                cancelled: false,
                            });
                        } else if !live {
                            let _ = reply.send(InputAck {
                                bytes_written: 0,
                                error: Some("process terminal is not live".into()),
                                cancelled: false,
                            });
                        } else if queue.pending.is_some() {
                            let _ = reply.send(InputAck {
                                bytes_written: 0,
                                error: Some("terminal is still delivering an earlier write".into()),
                                cancelled: false,
                            });
                        } else {
                            match PendingInput::new(attachment_input(data, reply)) {
                                Ok(pending) => {
                                    active_stop = Some(pending.request.stop.clone());
                                    queue.pending = Some(pending);
                                }
                                Err((reply, ack)) => {
                                    let _ = reply.send(ack);
                                }
                            }
                        }
                    }
                    Some(LeaseRequest::Resize { token, dimensions, reply }) => {
                        let result = if lease_token != Some(token) {
                            Err("attachment lease is stale".into())
                        } else if draining.is_some() || !input_open || process_stop.is_cancelled() {
                            Err("process is not live".into())
                        } else {
                            terminal.resize(master.get_ref(), dimensions)
                        };
                        let _ = reply.send(result);
                    }
                    Some(LeaseRequest::Release { token, reply }) => {
                        if lease_token == Some(token) {
                            lease_token = None;
                            state.set_lease(None);
                            queue.fail_input("attachment detached before the write completed", false);
                            active_stop = None;
                        }
                        if let Some(reply) = reply {
                            let _ = reply.send(());
                        }
                    }
                    None => {}
                }
            }
            request = input.recv(), if input_open && queue.pending.is_none() => {
                match request {
                    Some(request) if lease_token.is_some() => {
                        let _ = request.reply.send(InputAck {
                            bytes_written: 0,
                            error: Some("terminal input is owned by an attached human".into()),
                            cancelled: request.stop.is_cancelled(),
                        });
                    }
                    Some(request) => {
                        match PendingInput::new(request) {
                            Ok(request) => {
                                active_stop = Some(request.request.stop.clone());
                                queue.pending = Some(request);
                            }
                            Err((reply, ack)) => {
                                let _ = reply.send(ack);
                            }
                        }
                    }
                    None => input_open = false,
                }
            }
            ready = master.writable(), if queue.has_pending() => {
                let mut guard = match ready {
                    Ok(guard) => guard,
                    Err(error) => {
                        process_stop.cancel();
                        break Err(format!("terminal readiness failed: {error}"));
                    }
                };
                match guard.try_io(|fd| queue.try_flush(fd.get_ref())) {
                    Ok(Ok(_)) => {
                        if queue.pending.is_none() {
                            active_stop = None;
                        }
                    }
                    Ok(Err(error)) => {
                        process_stop.cancel();
                        break Err(format!("terminal write failed: {error}"));
                    }
                    Err(_would_block) => {}
                }
            }
            request = resize.recv(), if resize_open => {
                match request {
                    Some(request) => {
                        let result = if lease_token.is_some() {
                            Err("terminal resize is owned by an attached human".into())
                        } else if draining.is_some()
                            || !input_open
                            || process_stop.is_cancelled()
                        {
                            Err("process is not live".into())
                        } else {
                            terminal.resize(master.get_ref(), request.dimensions)
                        };
                        let _ = request.reply.send(result);
                    }
                    None => resize_open = false,
                }
            }
            ready = master.readable() => {
                let mut guard = match ready {
                    Ok(guard) => guard,
                    Err(error) => {
                        process_stop.cancel();
                        break Err(format!("terminal readiness failed: {error}"));
                    }
                };
                match guard.try_io(|fd| read_master(fd.get_ref(), &mut chunk)) {
                    Ok(Ok(0)) => {
                        raw.mark_eof();
                        input_stop.cancel();
                        break Ok(());
                    }
                    Ok(Ok(count)) => {
                        let bytes = &chunk[..count];
                        let replies_enabled = draining.is_none() && !exited.is_cancelled();
                        match capture(&terminal, &raw, bytes, replies_enabled) {
                            Ok(replies_from_parse) => {
                                if queue
                                    .replies
                                    .len()
                                    .saturating_add(replies_from_parse.len())
                                    > MAX_PENDING_REPLY_BYTES
                                {
                                    let reason = format!(
                                        "terminal protocol replies exceeded {MAX_PENDING_REPLY_BYTES} pending bytes"
                                    );
                                    input_stop.cancel();
                                    process_stop.cancel();
                                    break Err(reason);
                                }
                                queue.replies.extend(replies_from_parse);
                            }
                            Err(error) => {
                                input_stop.cancel();
                                process_stop.cancel();
                                break Err(error);
                            }
                        }
                    }
                    Ok(Err(error))
                        if error.raw_os_error()
                            == Some(rustix::io::Errno::IO.raw_os_error()) =>
                    {
                        // Linux reports EIO when the last slave closes. Treat it
                        // as terminal EOF; every other read fault remains visible.
                        raw.mark_eof();
                        input_stop.cancel();
                        break Ok(());
                    }
                    Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => {}
                    Ok(Err(error)) => {
                        raw.mark_error(format!("terminal read failed: {error}"));
                        process_stop.cancel();
                        break Err(format!("terminal read failed: {error}"));
                    }
                    Err(_would_block) => {}
                }
            }
        }
    };
    state.set_lease(None);
    let reason = outcome
        .as_ref()
        .err()
        .map(String::as_str)
        .unwrap_or("process input is closed");
    // Every exit path retains actual delivered prefixes, including read,
    // readiness and protocol failures. Queued requests are known undelivered.
    queue.fail_input(reason, false);
    close_input_queue(&mut input, reason);
    drop(master);
    outcome
}

/// Acquire is serialized with the writer: the in-flight request reports its
/// actual delivered prefix, every queued request is rejected, and later model
/// requests are refused by the lease branch until release.
fn transfer_to_lease(queue: &mut WriteQueue, input: &mut mpsc::Receiver<InputRequest>) {
    queue.fail_input(
        "human terminal lease acquired; remaining input was not delivered",
        false,
    );
    while let Ok(queued) = input.try_recv() {
        let _ = queued.reply.send(InputAck {
            bytes_written: 0,
            error: Some(
                "human terminal lease acquired; queued model input was not delivered".into(),
            ),
            cancelled: false,
        });
    }
}

fn close_input_queue(input: &mut mpsc::Receiver<InputRequest>, reason: &str) {
    input.close();
    while let Ok(request) = input.try_recv() {
        let _ = request.reply.send(InputAck {
            bytes_written: 0,
            error: Some(reason.into()),
            cancelled: request.stop.is_cancelled(),
        });
    }
}

/// Capture precedes projection: even a fatal reply/parser fault cannot erase
/// bytes already observed. A projection limit is not a capture/process failure.
fn capture(
    terminal: &Terminal,
    raw: &super::ring::Ring,
    bytes: &[u8],
    replies_enabled: bool,
) -> Result<VecDeque<u8>, String> {
    raw.append(bytes);
    match terminal.process(bytes, replies_enabled) {
        Ok(replies) => Ok(replies),
        Err(error) if error.fatal => Err(format!("terminal protocol failed: {}", error.reason)),
        Err(_unavailable) => Ok(VecDeque::new()),
    }
}

/// One nonblocking probe at the total post-exit deadline. Input is already
/// quiesced: deriving the final screen must not enqueue protocol replies.
fn capture_final(fd: &OwnedFd, terminal: &Terminal, raw: &super::ring::Ring) -> Result<(), String> {
    let mut chunk = [0; 8192];
    match read_master(fd, &mut chunk) {
        Ok(0) => raw.mark_eof(),
        Ok(count) => {
            capture(terminal, raw, &chunk[..count], false)?;
        }
        Err(error) if error.raw_os_error() == Some(rustix::io::Errno::IO.raw_os_error()) => {
            raw.mark_eof();
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
        Err(error) => {
            raw.mark_error(format!("terminal read failed: {error}"));
            return Err(format!("terminal read failed: {error}"));
        }
    }
    Ok(())
}

fn read_master(fd: &OwnedFd, buffer: &mut [u8]) -> io::Result<usize> {
    loop {
        match rustix::io::read(fd, &mut *buffer) {
            Ok(count) => return Ok(count),
            Err(rustix::io::Errno::INTR) => continue,
            Err(error) => return Err(error.into()),
        }
    }
}

fn write_master(fd: &OwnedFd, buffer: &[u8]) -> io::Result<usize> {
    loop {
        match rustix::io::write(fd, buffer) {
            Ok(count) => return Ok(count),
            Err(rustix::io::Errno::INTR) => continue,
            Err(error) => return Err(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::ring::Ring;

    #[tokio::test]
    async fn failure_cleanup_acknowledges_partial_and_queued_input() {
        let (reply, ack) = oneshot::channel();
        let mut queue = WriteQueue::new();
        queue.pending = Some(PendingInput {
            request: InputRequest {
                data: None,
                stop: CancellationToken::new(),
                reply,
            },
            data: b"abcdef".to_vec(),
            written: 3,
        });
        let (sender, mut input) = mpsc::channel(1);
        let (reply, queued) = oneshot::channel();
        sender
            .send(InputRequest {
                data: Some(b"later".to_vec()),
                stop: CancellationToken::new(),
                reply,
            })
            .await
            .unwrap();
        queue.fail_input("terminal read failed", false);
        close_input_queue(&mut input, "terminal read failed");
        let partial = ack.await.unwrap();
        assert_eq!(partial.bytes_written, 3);
        assert!(!partial.cancelled);
        assert_eq!(partial.error.as_deref(), Some("terminal read failed"));
        assert_eq!(queued.await.unwrap().bytes_written, 0);
        assert!(sender.is_closed());
    }

    #[tokio::test]
    async fn lease_acquisition_keeps_delivered_prefix_and_rejects_queued_input() {
        let (sender, mut input) = mpsc::channel(4);
        let (reply, ack) = oneshot::channel();
        let mut queue = WriteQueue::new();
        queue.pending = Some(PendingInput {
            request: InputRequest {
                data: Some(b"abcdef".to_vec()),
                stop: CancellationToken::new(),
                reply,
            },
            data: b"abcdef".to_vec(),
            written: 3,
        });
        let (queued_reply, queued_ack) = oneshot::channel();
        sender
            .send(InputRequest {
                data: Some(b"later".to_vec()),
                stop: CancellationToken::new(),
                reply: queued_reply,
            })
            .await
            .unwrap();
        transfer_to_lease(&mut queue, &mut input);
        let partial = ack.await.unwrap();
        assert_eq!(partial.bytes_written, 3);
        assert!(!partial.cancelled);
        assert!(
            partial
                .error
                .as_deref()
                .is_some_and(|error| error.contains("human terminal lease")),
            "{partial:?}"
        );
        let queued = queued_ack.await.unwrap();
        assert_eq!(queued.bytes_written, 0);
        assert!(
            queued
                .error
                .as_deref()
                .is_some_and(|error| error.contains("queued model input")),
            "{queued:?}"
        );
        assert!(queue.pending.is_none());
        assert!(queue.replies.is_empty());
    }

    #[test]
    fn fatal_protocol_overflow_preserves_observed_raw_bytes() {
        let terminal = Terminal::new(Dimensions::new(24, 80).unwrap());
        let raw = Ring::default();
        let output = b"\x1b[6n".repeat(2_000);
        assert!(capture(&terminal, &raw, &output, true).is_err());
        assert_eq!(raw.read(0, 65_536, 65_536).unwrap().bytes, output);
        assert!(terminal.meta().is_err());
    }

    #[test]
    fn final_probe_captures_ready_bytes_then_terminal_eof_without_replies() {
        let dimensions = Dimensions::new(24, 80).unwrap();
        let terminal = Terminal::new(dimensions);
        let raw = Ring::default();
        let (master, slave) = open_pair(dimensions).unwrap();
        let output = b"TAIL\x1b[6n";
        assert_eq!(rustix::io::write(&slave, output).unwrap(), output.len());
        capture_final(&master, &terminal, &raw).unwrap();
        assert_eq!(raw.read(0, 65_536, 65_536).unwrap().bytes, output);
        drop(slave);
        capture_final(&master, &terminal, &raw).unwrap();
        assert!(raw.meta().complete);
        assert_eq!(terminal.page(0, 1).unwrap().text, "TAIL");
    }
}
