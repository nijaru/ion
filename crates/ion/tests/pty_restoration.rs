//! PTY automation for terminal restoration (§22.4): the TUI must leave
//! the terminal usable after panic and clean exit. The tests drive the
//! real binary through a PTY pair. A background thread drains the
//! master continuously — the child blocks writing its panic backtrace
//! once the PTY buffer fills, so stopping the reads would deadlock it.

use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::process::{Command, Stdio};
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
    let mut responder = std::fs::File::from(master.try_clone().expect("dup master"));
    std::thread::spawn(move || {
        let mut file = std::fs::File::from(master);
        let mut chunk = [0u8; 4096];
        let mut answered = 0usize;
        let mut keyboard_answered = 0usize;
        // Non-blocking reads: EAGAIN just means no data yet; EOF (or
        // the master closing) ends the drain.
        loop {
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
                Err(_) => break,
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

    /// Match against the raw byte stream (for escape sequences).
    fn saw_raw(&self, needle: &str) -> bool {
        let buffer = self.output.lock().expect("output lock");
        std::str::from_utf8(&buffer)
            .map(|text| text.contains(needle))
            .unwrap_or(false)
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
    /// ICANON must be back on (the guard restored cooked mode).
    fn assert_cooked(&self) {
        let termios = tcgetattr(&self.slave).expect("tcgetattr on pty slave");
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
fn clean_exit_restores_the_terminal() {
    let mut session = spawn_ion(&[]);
    if !session.wait_for_output("idle", std::time::Duration::from_secs(15)) {
        eprintln!("deadline hit; locking output");
        let buffer = session.output.lock().unwrap();
        eprintln!(
            "locked {} bytes: {:?}",
            buffer.len(),
            String::from_utf8_lossy(&buffer)
        );
        panic!("expected the idle banner");
    }
    session.master_write.write_all(b"hello\r").expect("submit");
    // Esc only quits from idle; wait for the scripted response first
    // (an early esc would cancel the operation instead).
    assert!(
        session.wait_for_output("scripted provider", std::time::Duration::from_secs(15)),
        "expected the scripted response; output: {:?}",
        String::from_utf8_lossy(&session.output.lock().unwrap())
    );
    std::thread::sleep(std::time::Duration::from_millis(300));
    session.master_write.write_all(&[0x1b]).expect("esc quits");
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
        session.wait_for_output("idle", std::time::Duration::from_secs(15)),
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

    // /compact while idle explains itself.
    session
        .master_write
        .write_all(b"/compact\r")
        .expect("write /compact");
    assert!(
        session.wait_for_output("nothing to compact", std::time::Duration::from_secs(10)),
        "idle /compact explains itself"
    );
    session.master_write.write_all(&[0x03]).ok();
    session.master_write.flush().ok();
    let _ = session.child.wait();
    session.assert_cooked();
}
