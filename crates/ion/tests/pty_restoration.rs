//! PTY automation for terminal restoration (TERMINAL.md): the TUI must leave
//! the terminal usable after panic and clean exit. The tests drive the
//! real binary through a PTY pair. A background thread drains the
//! master continuously — the child blocks writing its panic backtrace
//! once the PTY buffer fills, so stopping the reads would deadlock it.

use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::pty::{OpenptyResult, openpty};
use nix::sys::termios::{LocalFlags, tcgetattr};

/// Remove CSI/escape sequences so needles match rendered text even
/// though the child emits cursor moves between words.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                // Consume until a final byte in @..~. Cursor moves
                // separate words on screen; keep one space so text
                // needles still match.
                let mut final_byte = '\0';
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        final_byte = next;
                        break;
                    }
                }
                if final_byte == 'H' || final_byte == 'J' || final_byte == 'K' {
                    out.push(' ');
                }
            }
            continue;
        }
        out.push(ch);
    }
    out
}

struct PtySession {
    _settings: tempfile::NamedTempFile,
    _data_root: tempfile::TempDir,
    output: Arc<Mutex<Vec<u8>>>,
    master_write: std::fs::File,
    slave: std::fs::File,
    child: std::process::Child,
    close_master: Arc<AtomicBool>,
}

/// A temp settings file with no default model, so the scripted
/// provider answers deterministically regardless of the ambient
/// OPENROUTER_API_KEY.
fn scripted_settings() -> tempfile::NamedTempFile {
    use std::io::Write as _;
    let mut file = tempfile::NamedTempFile::new().expect("temp settings");
    // Omitting defaultModel means unset: the scripted fallback runs.
    writeln!(file, "theme = \"dark\"").unwrap();
    file
}

fn spawn_ion(envs: &[(&str, &str)]) -> PtySession {
    spawn_ion_with_args(&[], envs)
}

fn spawn_ion_with_args(args: &[&str], envs: &[(&str, &str)]) -> PtySession {
    // A zero winsize would give the TUI an empty area to draw in.
    let size = nix::pty::Winsize {
        ws_row: 30,
        ws_col: 100,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let OpenptyResult { master, slave } = openpty(Some(&size), None).expect("openpty");
    fcntl(&master, FcntlArg::F_SETFL(OFlag::O_NONBLOCK)).expect("set O_NONBLOCK");
    let master_write = master.try_clone().expect("dup master for writes");
    let held_slave = slave.try_clone().expect("hold slave for termios");
    let dup = |what: &str| {
        Stdio::from(
            slave
                .as_fd()
                .try_clone_to_owned()
                .unwrap_or_else(|err| panic!("{what}: {err}")),
        )
    };
    let settings = scripted_settings();
    let data_root = tempfile::tempdir().expect("temp data root");
    let mut command = Command::new(env!("CARGO_BIN_EXE_ion"));
    command.args(args);
    command.env("ION_SETTINGS", settings.path());
    // Isolate the session store: the schema version gate must never
    // see (or refuse) the developer's real database.
    command.env("XDG_DATA_HOME", data_root.path());
    for (key, value) in envs {
        command.env(key, value);
    }
    // The TUI owns a terminal: give the child the PTY as its
    // controlling tty so crossterm's /dev/tty access lands on it.
    #[allow(unsafe_code)] // pre_exec runs between fork and exec
    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(|| {
            // SAFETY: the forked child is single-threaded; these calls
            // only touch process/tty state.
            libc::setsid();
            // Job-control signals must be default-disposition in the
            // child regardless of what the invoking shell ignored.
            libc::signal(libc::SIGTSTP, libc::SIG_DFL);
            libc::signal(libc::SIGINT, libc::SIG_DFL);
            libc::signal(libc::SIGHUP, libc::SIG_DFL);
            // Make the pty the controlling terminal so kernel hangup
            // (master close) reaches the child, and become its
            // foreground group like a real login shell would.
            // TIOCSCTTY's platform type differs (u32 on macos, c_ulong
            // on linux bsd mod); widen via `as` on the constant itself.
            libc::ioctl(0, libc::TIOCSCTTY as libc::c_ulong, 0 as libc::c_ulong);
            libc::tcsetpgrp(0, libc::getpgrp());
            Ok(())
        });
    }
    let child = command
        .stdin(dup("stdin"))
        .stdout(dup("stdout"))
        .stderr(dup("stderr"))
        .spawn()
        .expect("spawn ion");

    // Drain the master until EOF so the child never blocks on write,
    // answering cursor-position queries like a terminal emulator would
    // (a bare PTY has none, and the inline viewport blocks without a
    // reply).
    let collected: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let reader_buffer = Arc::clone(&collected);
    let close_master = Arc::new(AtomicBool::new(false));
    let mut responder = std::fs::File::from(master.try_clone().expect("dup master"));
    let drain_close = Arc::clone(&close_master);
    std::thread::spawn(move || {
        let mut file = std::fs::File::from(master);
        let mut chunk = [0u8; 4096];
        let mut answered = 0usize;
        let mut keyboard_answered = 0usize;
        // Non-blocking reads: EAGAIN just means no data yet; EOF (or
        // the master closing) ends the drain.
        loop {
            if drain_close.load(Ordering::SeqCst) {
                // Dropping both dups closes the master; the kernel then
                // hangs up the child's foreground process group.
                break;
            }
            match file.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    // Respond outside the lock: a master write can
                    // block, and holding the lock would deadlock the
                    // assertions.
                    let pending = {
                        let mut buffer = reader_buffer.lock().expect("drain lock");
                        buffer.extend_from_slice(&chunk[..n]);
                        let queries = buffer.windows(4).filter(|w| *w == b"\x1b[6n").count();
                        let respond = queries > answered;
                        answered = queries;
                        let keyboard_queries =
                            buffer.windows(4).filter(|w| *w == b"\x1b[?u").count();
                        let respond_keyboard = keyboard_queries > keyboard_answered;
                        keyboard_answered = keyboard_queries;
                        (respond, respond_keyboard)
                    };
                    if pending.0 {
                        // Report row 5, column 1 (inside the viewport).
                        use std::io::Write as _;
                        let _ = responder.write_all(b"\x1b[5;1R");
                        let _ = responder.flush();
                    }
                    if pending.1 {
                        // Advertise Kitty keyboard disambiguation and then
                        // answer the primary-device query Crossterm pairs
                        // with it during capability detection.
                        let _ = responder.write_all(b"\x1b[?1u\x1b[?1;2c");
                        let _ = responder.flush();
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(err) => {
                    reader_buffer
                        .lock()
                        .expect("drain lock")
                        .extend_from_slice(format!("\n<<DRAIN-ERR {err}>>").as_bytes());
                    break;
                }
            }
        }
    });

    PtySession {
        _settings: settings,
        _data_root: data_root,
        output: collected,
        master_write: master_write.into(),
        slave: held_slave.into(),
        child,
        close_master,
    }
}

impl PtySession {
    fn wait_for_output(&self, needle: &str, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if self.saw(needle) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        self.saw(needle)
    }

    fn saw(&self, needle: &str) -> bool {
        let buffer = self.output.lock().expect("output lock");
        let text = strip_ansi(&String::from_utf8_lossy(&buffer));
        text.contains(needle)
    }

    fn output_len(&self) -> usize {
        self.output.lock().expect("output lock").len()
    }

    fn saw_since(&self, offset: usize, needle: &str) -> bool {
        let buffer = self.output.lock().expect("output lock");
        let text = strip_ansi(&String::from_utf8_lossy(
            &buffer[offset.min(buffer.len())..],
        ));
        text.contains(needle)
    }

    /// Wait for a fresh active footer after the caller submitted a turn.
    /// Checking from an output offset avoids matching an earlier turn's
    /// `thinking` frame in the append-only PTY capture.
    fn wait_for_active_since(&self, offset: usize, timeout: std::time::Duration) -> bool {
        self.wait_for_since(offset, "● thinking", timeout)
    }

    fn wait_for_since(&self, offset: usize, needle: &str, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if self.saw_since(offset, needle) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        self.saw_since(offset, needle)
    }

    /// Wait until the newest footer has no active-operation marker.
    /// This is the PTY-visible completion boundary; streamed model text
    /// alone is not evidence that `OperationFinished` was handled.
    fn wait_for_idle(&self, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if self.latest_footer_is_idle() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        self.latest_footer_is_idle()
    }

    fn latest_footer_is_idle(&self) -> bool {
        let buffer = self.output.lock().expect("output lock");
        let text = strip_ansi(&String::from_utf8_lossy(&buffer));
        text.rfind("ion ")
            .is_some_and(|start| !text[start..].contains("● "))
    }

    /// Match against the raw byte stream (for escape sequences).
    fn saw_raw(&self, needle: &str) -> bool {
        let buffer = self.output.lock().expect("output lock");
        std::str::from_utf8(&buffer)
            .map(|text| text.contains(needle))
            .unwrap_or(false)
    }

    /// Count occurrences in the raw byte stream (for re-emitted
    /// capability sequences after suspend/resume).
    fn count_raw(&self, needle: &str) -> usize {
        let buffer = self.output.lock().expect("output lock");
        std::str::from_utf8(&buffer)
            .map(|text| text.matches(needle).count())
            .unwrap_or(0)
    }

    /// Close every master fd so the kernel hangs up the child.
    fn hang_up(&mut self) {
        self.close_master.store(true, Ordering::SeqCst);
        self.master_write = std::fs::File::open("/dev/null").expect("placeholder fd");
    }

    #[allow(unsafe_code)] // raw signal syscall
    fn continue_child(&self) {
        let pid = self.child.id() as i32;
        // SAFETY: plain signal syscall on a live, stopped child.
        unsafe { libc::kill(pid, libc::SIGCONT) };
    }

    /// Set the pty window size; the kernel delivers SIGWINCH to the
    /// foreground process group.
    #[allow(unsafe_code)] // raw ioctl for winsize changes
    fn set_winsize(&self, rows: u16, cols: u16) {
        let winsize = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let fd = self.slave.as_fd().as_raw_fd();
        // SAFETY: TIOCSWINSZ with a valid winsize pointer.
        unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &winsize) };
    }

    fn wait_exit(&mut self, timeout: std::time::Duration) -> Option<std::process::ExitStatus> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match self.child.try_wait().expect("try_wait") {
                Some(status) => return Some(status),
                None if std::time::Instant::now() > deadline => return None,
                None => std::thread::sleep(std::time::Duration::from_millis(100)),
            }
        }
    }

    fn wait_for_raw(&self, needle: &str, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if self.saw_raw(needle) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        self.saw_raw(needle)
    }

    /// The TUI puts the tty in raw mode; after any exit path ECHO and
    /// ICANON must be back on (the guard restored cooked mode). Read
    /// through the master: when the child owns the pty as its
    /// controlling terminal, a session-leader exit disassociates the
    /// slave and leaves ENOTTY on held slave fds.
    fn assert_cooked(&self) {
        let termios = tcgetattr(&self.master_write).expect("tcgetattr on pty master");
        assert!(
            termios
                .local_flags
                .contains(LocalFlags::ECHO | LocalFlags::ICANON),
            "terminal not restored to cooked mode: {:?}",
            termios.local_flags
        );
    }
}

#[test]
fn panic_restores_the_terminal() {
    let mut session = spawn_ion(&[("ION_TEST_PANIC", "1")]);
    assert!(
        session.wait_for_output("panicked at", std::time::Duration::from_secs(15)),
        "expected the test panic, got {} bytes",
        session.output.lock().unwrap().len()
    );
    let status = session.child.wait().expect("wait");
    assert!(!status.success(), "a panicking TUI must exit non-zero");
    // The guard's restore ran before the default hook printed the
    // backtrace: bracketed paste disabled is visible in the stream.
    assert!(
        session.saw("panicked at"),
        "expected the panic; output: {:?}",
        String::from_utf8_lossy(&session.output.lock().unwrap())
    );
    assert!(
        session.saw_raw("\x1b[?2004l"),
        "bracketed paste not disabled on panic"
    );
    assert!(
        session.wait_for_raw("\x1b[>1u", std::time::Duration::from_secs(2)),
        "keyboard enhancement was not enabled on panic"
    );
    assert!(
        session.wait_for_raw("\x1b[<1u", std::time::Duration::from_secs(2)),
        "keyboard enhancement was not restored on panic"
    );
    session.assert_cooked();
}

#[test]
fn startup_error_restores_terminal_before_diagnostic() {
    let mut session = spawn_ion_with_args(&["--resume"], &[]);
    let status = session
        .wait_exit(std::time::Duration::from_secs(15))
        .expect("resume-without-session must exit");
    assert_eq!(status.code(), Some(2));
    assert!(
        session.wait_for_output(
            "no persisted session to resume",
            std::time::Duration::from_secs(2)
        ),
        "expected resume diagnostic"
    );

    let output = session.output.lock().expect("output lock");
    let text = String::from_utf8_lossy(&output);
    let restored = text
        .find("\x1b[?2004l")
        .expect("bracketed paste disable before startup diagnostic");
    let diagnostic = text
        .find("no persisted session to resume")
        .expect("resume diagnostic");
    assert!(
        restored < diagnostic,
        "startup diagnostic was emitted before terminal restoration: {text:?}"
    );
    drop(output);
    session.assert_cooked();
}

#[test]
fn clean_exit_restores_the_terminal() {
    let mut session = spawn_ion(&[]);
    if !session.wait_for_output("ion v", std::time::Duration::from_secs(15)) {
        eprintln!("deadline hit; locking output");
        let buffer = session.output.lock().unwrap();
        eprintln!(
            "locked {} bytes: {:?}",
            buffer.len(),
            String::from_utf8_lossy(&buffer)
        );
        panic!("expected the startup header");
    }
    assert!(
        !String::from_utf8_lossy(&session.output.lock().unwrap()).contains("ctrl+c clear"),
        "startup must stay quiet; use /help for key discovery"
    );
    session.master_write.write_all(b"hello\r").expect("submit");
    // Esc only quits from idle; wait for the scripted response first
    // (an early esc would cancel the operation instead).
    assert!(
        session.wait_for_output("scripted provider", std::time::Duration::from_secs(15)),
        "expected the scripted response; output: {:?}",
        String::from_utf8_lossy(&session.output.lock().unwrap())
    );
    std::thread::sleep(std::time::Duration::from_millis(300));
    session
        .master_write
        .write_all(&[0x04])
        .expect("ctrl+d quits");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let status = loop {
        match session.child.try_wait().expect("try_wait") {
            Some(status) => break status,
            None if std::time::Instant::now() > deadline => {
                panic!(
                    "child did not exit; output: {:?}",
                    String::from_utf8_lossy(&session.output.lock().unwrap())
                );
            }
            None => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    };
    assert!(status.success(), "clean exit must be code 0");
    assert!(
        session.wait_for_raw("\x1b[>1u", std::time::Duration::from_secs(2)),
        "keyboard enhancement was not enabled on clean exit"
    );
    assert!(
        session.wait_for_raw("\x1b[<1u", std::time::Duration::from_secs(2)),
        "keyboard enhancement was not restored on clean exit"
    );
    session.assert_cooked();
}

#[test]
fn slash_commands_render_notices_without_runtime_calls() {
    let mut session = spawn_ion(&[]);
    assert!(
        session.wait_for_output("ion v", std::time::Duration::from_secs(15)),
        "TUI idle banner never appeared"
    );
    use std::io::Write as _;

    // /help
    session
        .master_write
        .write_all(b"/help\r")
        .expect("write /help");
    assert!(
        session.wait_for_output("/compact", std::time::Duration::from_secs(10)),
        "help listed the commands"
    );

    // Unknown command.
    session
        .master_write
        .write_all(b"/nope\r")
        .expect("write /nope");
    assert!(
        session.wait_for_output("unknown command: /nope", std::time::Duration::from_secs(10)),
        "unknown command surfaced; buffer: {:?}",
        String::from_utf8_lossy(&session.output.lock().unwrap())
    );

    // /model
    session
        .master_write
        .write_all(b"/model\r")
        .expect("write /model");
    assert!(
        session.wait_for_output(
            "model: (scripted provider)",
            std::time::Duration::from_secs(10)
        ),
        "/model shows the provider"
    );

    // /compact while idle explains itself, including the path forward.
    session
        .master_write
        .write_all(b"/compact\r")
        .expect("write /compact");
    assert!(
        session.wait_for_output("nothing to compact", std::time::Duration::from_secs(10)),
        "idle /compact explains itself"
    );
    assert!(
        session.wait_for_output(
            "automatic compaction near the model window",
            std::time::Duration::from_secs(10),
        ),
        "idle /compact hint must mention the automatic path"
    );
    // ctrl+d exits from an empty, idle composer.
    session.master_write.write_all(&[0x04]).ok();
    session.master_write.flush().ok();
    let _ = session.child.wait();
    session.assert_cooked();
}

/// H0b matrix: suspend must leave the shell a cooked, capability-clean
/// terminal while ion is stopped; resume/continue must re-arm the
/// negotiated modes and repaint. In raw mode ISIG is off, so a real
/// Ctrl+Z arrives as the 0x1a byte — that is the deterministic driver.
#[test]
fn sigstp_suspends_cleanly_and_resume_rearms() {
    let mut session = spawn_ion(&[]);
    assert!(
        session.wait_for_output("ion v", std::time::Duration::from_secs(15)),
        "TUI idle banner never appeared"
    );

    let disables_before = session.count_raw("\x1b[?2004l");
    let enables_before = session.count_raw("\x1b[>1u");
    session.master_write.write_all(&[0x1a]).expect("ctrl+z");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let disables_now = session.count_raw("\x1b[?2004l");
        let enables_now = session.count_raw("\x1b[>1u");
        if disables_now > disables_before && enables_now > enables_before {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "suspend/resume cycle not observed after ctrl+z"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // After the cycle the terminal must be back in raw mode with the
    // negotiated keyboard enhancement re-armed, and still interactive.

    session.continue_child();
    // Give the loop a moment to notice SIGCONT and settle.
    std::thread::sleep(std::time::Duration::from_millis(500));
    let termios = tcgetattr(&session.master_write).expect("tcgetter after resume");
    assert!(
        !termios
            .local_flags
            .contains(LocalFlags::ECHO | LocalFlags::ICANON),
        "raw mode not re-applied after resume: {:?}",
        termios.local_flags
    );

    session
        .master_write
        .write_all(&[0x04])
        .expect("ctrl+d quits");
    let status = session
        .wait_exit(std::time::Duration::from_secs(10))
        .expect("child did not exit after resume + esc");
    assert!(status.success(), "exit after resume must be clean");
    session.assert_cooked();
}

/// H0b matrix: a resize storm must not kill or wedge the renderer;
/// after the storm the TUI still accepts input and exits cleanly.
#[test]
fn resize_storm_survives_and_stays_interactive() {
    let mut session = spawn_ion(&[]);
    assert!(
        session.wait_for_output("ion v", std::time::Duration::from_secs(15)),
        "TUI idle banner never appeared"
    );
    let before_first = session.output_len();
    session.master_write.write_all(b"hello\r").expect("submit");
    assert!(
        session.wait_for_active_since(before_first, std::time::Duration::from_secs(15)),
        "first operation did not start"
    );
    assert!(
        session.wait_for_output("scripted provider", std::time::Duration::from_secs(15)),
        "expected the scripted response before the storm"
    );
    assert!(
        session.wait_for_idle(std::time::Duration::from_secs(15)),
        "first operation did not reach an idle footer before the storm"
    );

    for i in 0..12 {
        if i % 2 == 0 {
            session.set_winsize(15, 40);
        } else {
            session.set_winsize(30, 100);
        }
        std::thread::sleep(std::time::Duration::from_millis(40));
    }
    session.set_winsize(30, 100);

    // Still interactive after the storm. Wait for a fresh active footer
    // and then its idle transition; a historical response string is not
    // evidence that the second operation completed.
    let alive = session.child.try_wait().expect("try_wait").is_none();
    let before_second = session.output_len();
    session.master_write.write_all(b"second\r").expect("second");
    assert!(
        session.wait_for_active_since(before_second, std::time::Duration::from_secs(15)),
        "TUI did not start the second operation after resize storm (alive={alive})"
    );
    assert!(
        session.wait_for_idle(std::time::Duration::from_secs(15)),
        "TUI did not settle the second operation after resize storm (alive={alive})"
    );

    session
        .master_write
        .write_all(&[0x04])
        .expect("ctrl+d quits");
    let status = session
        .wait_exit(std::time::Duration::from_secs(10))
        .expect("child did not exit after storm");
    assert!(status.success(), "clean exit required after storm");
    session.assert_cooked();
}

/// H0b matrix: a lost terminal must end the process instead of
/// leaving it writing into the void forever. Linux kernels raise
/// SIGHUP themselves when the master closes; macOS stays silent, so
/// there the terminal manager's hangup signal is delivered explicitly.
#[test]
fn dead_tty_ends_the_process() {
    let mut session = spawn_ion(&[]);
    assert!(
        session.wait_for_output("ion v", std::time::Duration::from_secs(15)),
        "TUI idle banner never appeared"
    );
    session.hang_up();
    #[cfg(target_os = "linux")]
    if session
        .wait_exit(std::time::Duration::from_secs(3))
        .is_some()
    {
        return;
    }
    #[cfg(not(target_os = "linux"))]
    std::thread::sleep(std::time::Duration::from_millis(300));
    // SAFETY: plain signal syscall; the child pid is valid.
    #[allow(unsafe_code)]
    unsafe {
        libc::kill(session.child.id() as i32, libc::SIGHUP);
    }
    let status = session.wait_exit(std::time::Duration::from_secs(10));
    assert!(status.is_some(), "child outlived its terminal loss");
}
