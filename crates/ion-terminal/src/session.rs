use std::fs::File;
use std::io::{self, Stdout, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, LazyLock, Mutex, MutexGuard, Once, TryLockError, Weak,
    atomic::{AtomicU64, Ordering},
};

use crossterm::{SynchronizedUpdate, terminal};
use tokio::sync::Notify;

use crate::CapabilitySupport;
use crate::Screen;
use crate::input::{InputEvent, InputStream};
use crate::requirements::TerminalRequirements;

#[path = "modes.rs"]
mod modes;
use modes::TerminalState;

#[path = "secret.rs"]
mod secret;

type State = TerminalState<Stdout>;
static PANIC_OWNER: Mutex<Weak<Mutex<State>>> = Mutex::new(Weak::new());
static PANIC_HOOK: Once = Once::new();
// A process panic invalidates the live lease even when either mutex is busy.
// It is an event generation, not a second owner of physical mode custody.
static PANIC_GENERATION: LazyLock<Arc<AtomicU64>> = LazyLock::new(|| Arc::new(AtomicU64::new(0)));
// Notification only wakes intake; the generation still determines lease validity.
static PANIC_WAKE: Notify = Notify::const_new();

/// Output that mirrors bytes to the optional PTY capture without changing the
/// writer contract used by the renderer.
pub struct TerminalOutput<W> {
    output: W,
    capture: Option<File>,
}

impl<W: Write> TerminalOutput<W> {
    pub fn new(output: W, capture_path: Option<&Path>) -> io::Result<Self> {
        let capture = capture_path
            .map(|path| {
                File::create(path).map_err(|err| {
                    io::Error::new(
                        err.kind(),
                        format!("terminal capture {}: {err}", path.display()),
                    )
                })
            })
            .transpose()?;
        Ok(Self { output, capture })
    }

    fn from_environment(output: W) -> io::Result<Self> {
        let capture_path = std::env::var_os("ION_TERMINAL_CAPTURE").map(PathBuf::from);
        Self::new(output, capture_path.as_deref())
    }
}

impl<W: Write> Write for TerminalOutput<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let written = self.output.write(bytes)?;
        if written > 0
            && let Some(capture) = &mut self.capture
        {
            capture.write_all(&bytes[..written])?;
        }
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()?;
        if let Some(capture) = &mut self.capture {
            capture.flush()?;
        }
        Ok(())
    }
}

/// A synchronous output lease. Physical writes cannot race panic restoration
/// or a keyboard-stack/screen transition. Never retain it across an await.
pub struct TerminalWriter<'a>(MutexGuard<'a, State>);

impl Write for TerminalWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.usable()?;
        let result = self.0.output.write(bytes);
        if result.is_err() {
            self.0.fail();
        }
        result
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.usable()?;
        let result = self.0.output.flush();
        if result.is_err() {
            self.0.fail();
        }
        result
    }
}

/// The process's exclusive physical terminal owner. Normal transitions,
/// output, and the panic hook use the same mode custody and output lock.
pub struct TerminalSession {
    state: Arc<Mutex<State>>,
    input: InputStream,
}

impl TerminalSession {
    pub async fn enter() -> io::Result<Self> {
        Self::with_requirements(TerminalRequirements::default()).await
    }

    pub async fn with_requirements(requirements: TerminalRequirements) -> io::Result<Self> {
        install_panic_hook();
        let state = {
            let mut owner = PANIC_OWNER
                .lock()
                .map_err(|_| io::Error::other("terminal owner registry is poisoned"))?;
            if owner.upgrade().is_some() {
                return Err(io::Error::other("terminal already has a physical owner"));
            }
            let state = Arc::new(Mutex::new(TerminalState::new(
                TerminalOutput::from_environment(io::stdout())?,
                requirements,
                Arc::clone(&PANIC_GENERATION),
            )));
            *owner = Arc::downgrade(&state);
            state
        };
        let mut session = Self {
            state,
            input: InputStream::new()?,
        };
        session.lock().activate()?;
        session.negotiate_keyboard().await?;
        Ok(session)
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        match self.state.lock() {
            Ok(state) => state,
            Err(error) => {
                let mut state = error.into_inner();
                state.fail();
                state
            }
        }
    }

    pub fn output(&mut self) -> io::Result<TerminalWriter<'_>> {
        let state = self.lock();
        state.usable()?;
        Ok(TerminalWriter(state))
    }

    pub fn check_active(&self) -> io::Result<()> {
        self.lock().usable()
    }

    pub async fn next_input(&mut self) -> Option<io::Result<InputEvent>> {
        loop {
            // notify_waiters reaches futures created before the notification,
            // even before their first poll. Create this before checking custody
            // so a panic between the check and await cannot strand idle intake.
            let panicked = PANIC_WAKE.notified();
            if let Err(error) = self.check_active() {
                return Some(Err(error));
            }
            let event = tokio::select! {
                biased;
                () = panicked => continue,
                event = self.input.next() => event,
            };
            if let Err(error) = self.check_active() {
                return Some(Err(error));
            }
            return event;
        }
    }

    async fn negotiate_keyboard(&mut self) -> io::Result<()> {
        if !self.lock().requirements.keyboard_enhancement {
            return Ok(());
        }
        // Queries use the same intake as keys; the output lease ends before waiting.
        self.output()?.write_all(b"\x1b[?u\x1b[c")?;
        self.output()?.flush()?;
        let support = match self.input.keyboard_support().await? {
            Some(true) => CapabilitySupport::Supported,
            Some(false) => CapabilitySupport::Unsupported,
            None => CapabilitySupport::Unknown,
        };
        self.lock().set_keyboard_support(support)
    }

    /// Own a keyboard push on the alternate surface while it is in use.
    pub fn enter_alt_screen(&mut self) -> io::Result<()> {
        self.lock().enter_alt_screen()
    }

    /// Pop only the alternate push before restoring the primary surface.
    pub fn leave_alt_screen(&mut self) -> io::Result<()> {
        self.lock().leave_alt_screen()
    }

    pub fn is_alt_screen(&self) -> io::Result<bool> {
        self.lock().is_alt_screen()
    }

    pub fn size(&self) -> io::Result<(u16, u16)> {
        self.lock().usable()?;
        terminal::size()
    }

    /// Observe the restored primary cursor alongside unchanged kernel geometry.
    /// Native reflow can lead the kernel; callers validate bounds/ownership only
    /// after resize settling. A change during the query defers the snapshot.
    pub async fn primary_geometry(&mut self) -> io::Result<Option<((u16, u16), (u16, u16))>> {
        if self.is_alt_screen()? {
            return Err(io::Error::other(
                "primary geometry queried on alternate screen",
            ));
        }
        self.input.take_resize_notifications();
        let size = self.size()?;
        self.output()?.write_all(b"\x1b[6n")?;
        self.output()?.flush()?;
        let cursor = self.input.cursor_position().await?;
        self.check_active()?;
        let current = self.size()?;
        if current != size || self.input.take_resize_notifications() {
            return Ok(None);
        }
        // Movement alone cannot authorize cancellation of active coding work.
        let Some(cursor) = cursor else {
            return Ok(None);
        };
        Ok(Some((size, cursor)))
    }

    /// Resolve native reflow before any inline publication, growth or paint.
    /// Neither the terminal output lease nor a frontend progress guard may be
    /// retained while waiting for CPR.
    pub async fn prepare_inline(&mut self, screen: &mut Screen) -> io::Result<Option<(u16, u16)>> {
        if self.is_alt_screen()? {
            return Err(io::Error::other(
                "inline preparation requires restored primary screen",
            ));
        }
        if self.input.take_resize_notifications() {
            screen.note_resize();
        }
        let size = self.size()?;
        if screen.needs_reconciliation(size) {
            if !screen.reconciliation_ready() {
                return Ok(None);
            }
            let Some((size, cursor)) = self.primary_geometry().await? else {
                screen.note_resize();
                return Ok(None);
            };
            if !screen.reconcile_inline(&mut self.output()?, size.0, size.1, cursor)? {
                return Ok(None);
            }
            return Ok(Some(size));
        }
        Ok(Some(size))
    }

    /// Stage one inline publication/paint without physical effects. Check the
    /// geometry after layout, then publish the bytes under one output lease.
    /// This narrows, but cannot eliminate, a native resize during physical I/O.
    pub fn paint_inline(
        &mut self,
        screen: &mut Screen,
        paint: impl FnOnce(&mut Screen, &mut Vec<u8>) -> io::Result<()>,
    ) -> io::Result<bool> {
        let expected = screen.primary_size();
        let mut staged = screen.staged();
        let mut bytes = Vec::new();
        let synchronized = self.lock().requirements.synchronized_output;
        if synchronized {
            bytes.sync_update(|out| paint(&mut staged, out))??;
        } else {
            paint(&mut staged, &mut bytes)?;
        }
        if self.size()? != expected {
            screen.note_resize();
            return Ok(false);
        }
        {
            let mut output = self.output()?;
            output.write_all(&bytes)?;
            output.flush()?;
        }
        *screen = staged;
        Ok(true)
    }

    pub fn suspend(&mut self) -> io::Result<()> {
        self.restore()
    }

    pub async fn resume(&mut self) -> io::Result<()> {
        self.input.resume()?;
        self.lock().activate()?;
        self.negotiate_keyboard().await
    }

    pub fn restore(&mut self) -> io::Result<()> {
        let input = self.input.suspend();
        let modes = self.lock().restore();
        input.and(modes)
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

fn record_panic(generation: &AtomicU64) {
    // At exhaustion every newly acquired lease is invalid too; never wrap and
    // accidentally resurrect a prior lease.
    let _ = generation.try_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
        value.checked_add(1)
    });
}

fn restore_on_panic<W: Write>(owner: &Mutex<TerminalState<W>>) {
    let mut state = match owner.try_lock() {
        Ok(state) => state,
        Err(TryLockError::Poisoned(error)) => error.into_inner(),
        // The panicking thread may already hold this lock. Another thread may
        // be between physical transitions. Neither permits a competing pop.
        Err(TryLockError::WouldBlock) => return,
    };
    let _ = state.restore();
    state.fail();
}

fn install_panic_hook() {
    PANIC_HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            record_panic(&PANIC_GENERATION);
            PANIC_WAKE.notify_waiters();
            let owner = PANIC_OWNER
                .try_lock()
                .ok()
                .and_then(|owner| owner.upgrade());
            if let Some(owner) = owner {
                restore_on_panic(&owner);
            }
            previous(info);
        }));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_requirements_enable_paste_and_keyboard() {
        let requirements = TerminalRequirements::default();
        assert!(requirements.bracketed_paste);
        assert!(requirements.keyboard_enhancement);
    }

    #[test]
    fn synchronized_output_wraps_one_operation() {
        let mut output = TerminalOutput::new(Vec::new(), None).expect("output");
        output
            .sync_update(|output| output.write_all(b"frame"))
            .expect("sync update")
            .expect("frame");
        let text = String::from_utf8(output.output).expect("utf8");
        assert!(text.starts_with("\x1b[?2026h"));
        assert!(text.ends_with("\x1b[?2026l"));
        assert!(text.contains("frame"));
    }
}
