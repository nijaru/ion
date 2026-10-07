use std::io::{self, Write};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use crossterm::cursor::Show;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::style::{Attribute, SetAttribute};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, terminal};

use crate::capabilities::{CapabilitySupport, TerminalCapabilities};
use crate::requirements::TerminalRequirements;
use crate::session::TerminalOutput;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Surface {
    Primary,
    Alternate,
    Indeterminate,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Keyboard {
    Unowned,
    Owned,
    Indeterminate,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Lifecycle {
    Active,
    Suspended,
    Failed { restored: bool },
}

/// All physical output and mode custody share one lock, including the panic
/// hook. An ambiguous write consumes destructive authority: retrying a pop
/// could remove the caller's keyboard stack rather than ours.
pub(crate) struct TerminalState<W> {
    pub(crate) output: TerminalOutput<W>,
    pub(crate) requirements: TerminalRequirements,
    capabilities: TerminalCapabilities,
    lifecycle: Lifecycle,
    panic_generation: Arc<AtomicU64>,
    acquired_generation: u64,
    raw: bool,
    paste: bool,
    mouse: bool,
    surface: Surface,
    primary_keyboard: Keyboard,
    alternate_keyboard: Keyboard,
}

impl<W: Write> TerminalState<W> {
    pub(crate) fn new(
        output: TerminalOutput<W>,
        requirements: TerminalRequirements,
        panic_generation: Arc<AtomicU64>,
    ) -> Self {
        let acquired_generation = panic_generation.load(Ordering::SeqCst);
        Self {
            output,
            requirements,
            capabilities: TerminalCapabilities::default(),
            lifecycle: Lifecycle::Suspended,
            panic_generation,
            acquired_generation,
            raw: false,
            paste: false,
            mouse: false,
            surface: Surface::Primary,
            primary_keyboard: Keyboard::Unowned,
            alternate_keyboard: Keyboard::Unowned,
        }
    }

    pub(crate) fn usable(&self) -> io::Result<()> {
        if self.lifecycle == Lifecycle::Active && !self.panic_invalidated() {
            Ok(())
        } else {
            Err(io::Error::other("terminal is suspended or failed"))
        }
    }

    fn panic_invalidated(&self) -> bool {
        self.acquired_generation == u64::MAX
            || self.panic_generation.load(Ordering::SeqCst) != self.acquired_generation
    }

    pub(crate) fn fail(&mut self) {
        self.lifecycle = Lifecycle::Failed {
            restored: self.lifecycle == Lifecycle::Suspended
                || self.lifecycle == Lifecycle::Failed { restored: true },
        };
    }

    pub(crate) fn is_alt_screen(&self) -> io::Result<bool> {
        self.usable()?;
        Ok(self.surface == Surface::Alternate)
    }

    fn push_keyboard(&mut self, alternate: bool) -> io::Result<()> {
        let custody = if alternate {
            &mut self.alternate_keyboard
        } else {
            &mut self.primary_keyboard
        };
        if *custody != Keyboard::Unowned {
            return Err(io::Error::other("keyboard stack custody is not available"));
        }
        *custody = Keyboard::Indeterminate;
        execute!(
            self.output,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
        *custody = Keyboard::Owned;
        Ok(())
    }

    fn pop_keyboard(&mut self, alternate: bool) -> io::Result<()> {
        let custody = if alternate {
            &mut self.alternate_keyboard
        } else {
            &mut self.primary_keyboard
        };
        match *custody {
            Keyboard::Unowned => return Ok(()),
            Keyboard::Indeterminate => {
                return Err(io::Error::other("keyboard stack outcome is indeterminate"));
            }
            Keyboard::Owned => {}
        }
        *custody = Keyboard::Indeterminate;
        execute!(self.output, PopKeyboardEnhancementFlags)?;
        *custody = Keyboard::Unowned;
        Ok(())
    }

    pub(crate) fn enter_alt_screen(&mut self) -> io::Result<()> {
        self.usable()?;
        if self.surface == Surface::Alternate {
            return Ok(());
        }
        let result = (|| {
            self.surface = Surface::Indeterminate;
            execute!(self.output, EnterAlternateScreen)?;
            self.surface = Surface::Alternate;
            if self.capabilities.kitty_keyboard == CapabilitySupport::Supported {
                self.push_keyboard(true)?;
            }
            self.mouse = true;
            execute!(self.output, EnableMouseCapture)
        })();
        if result.is_err() {
            self.fail();
        }
        result
    }

    pub(crate) fn leave_alt_screen(&mut self) -> io::Result<()> {
        self.usable()?;
        if self.surface == Surface::Primary {
            return Ok(());
        }
        let result = (|| {
            self.pop_keyboard(true)?;
            execute!(self.output, DisableMouseCapture)?;
            self.mouse = false;
            self.surface = Surface::Indeterminate;
            execute!(self.output, LeaveAlternateScreen)?;
            self.surface = Surface::Primary;
            Ok(())
        })();
        if result.is_err() {
            self.fail();
        }
        result
    }

    pub(crate) fn activate(&mut self) -> io::Result<()> {
        if self.panic_invalidated() {
            self.fail();
        }
        match self.lifecycle {
            Lifecycle::Active => return Ok(()),
            Lifecycle::Failed { .. } => return Err(io::Error::other("terminal lifecycle failed")),
            Lifecycle::Suspended => {}
        }
        let result = (|| {
            terminal::enable_raw_mode()?;
            self.raw = true;
            self.lifecycle = Lifecycle::Active;
            if self.requirements.bracketed_paste {
                self.paste = true;
                execute!(self.output, EnableBracketedPaste)?;
                self.capabilities.bracketed_paste = CapabilitySupport::Supported;
            } else {
                self.capabilities.bracketed_paste = CapabilitySupport::Unsupported;
            }
            if self.requirements.mouse {
                self.mouse = true;
                execute!(self.output, EnableMouseCapture)?;
                self.capabilities.mouse = CapabilitySupport::Supported;
            } else {
                self.capabilities.mouse = CapabilitySupport::Unsupported;
            }
            self.capabilities.kitty_keyboard = if self.requirements.keyboard_enhancement {
                match terminal::supports_keyboard_enhancement() {
                    Ok(true) => CapabilitySupport::Supported,
                    Ok(false) => CapabilitySupport::Unsupported,
                    Err(_) => CapabilitySupport::Unknown,
                }
            } else {
                CapabilitySupport::Unsupported
            };
            if self.capabilities.kitty_keyboard == CapabilitySupport::Supported {
                self.push_keyboard(false)?;
            }
            Ok(())
        })();
        if result.is_err() {
            self.fail();
            let _ = self.restore();
        }
        result
    }

    pub(crate) fn restore(&mut self) -> io::Result<()> {
        if self.panic_invalidated() {
            self.fail();
        }
        if matches!(
            self.lifecycle,
            Lifecycle::Suspended | Lifecycle::Failed { restored: true }
        ) {
            return Ok(());
        }
        let mut first_error = None;
        let mut record = |result: io::Result<()>| {
            if let Err(error) = result
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        };
        if self.mouse {
            let result = execute!(self.output, DisableMouseCapture);
            if result.is_ok() {
                self.mouse = false;
            }
            record(result);
        }
        if self.surface == Surface::Alternate {
            record(self.pop_keyboard(true));
        }
        if self.surface != Surface::Primary {
            // Leaving is idempotent; popping on an unknown surface is not.
            self.surface = Surface::Indeterminate;
            let result = execute!(self.output, LeaveAlternateScreen);
            if result.is_ok() {
                self.surface = Surface::Primary;
            }
            record(result);
        }
        if self.surface == Surface::Primary {
            record(self.pop_keyboard(false));
        }
        if self.paste {
            let result = execute!(self.output, DisableBracketedPaste);
            if result.is_ok() {
                self.paste = false;
            }
            record(result);
        }
        record(execute!(self.output, Show));
        record(execute!(self.output, SetAttribute(Attribute::Reset)));
        if self.raw {
            let result = terminal::disable_raw_mode();
            if result.is_ok() {
                self.raw = false;
            }
            record(result);
        }
        record(self.output.flush());
        if self.primary_keyboard == Keyboard::Indeterminate
            || self.alternate_keyboard == Keyboard::Indeterminate
            || self.surface == Surface::Indeterminate
        {
            record(Err(io::Error::other(
                "terminal mode outcome is indeterminate",
            )));
        }
        if first_error.is_some() {
            self.fail();
        } else {
            self.lifecycle = match self.lifecycle {
                Lifecycle::Failed { .. } => Lifecycle::Failed { restored: true },
                _ => Lifecycle::Suspended,
            };
        }
        first_error.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active() -> TerminalState<Vec<u8>> {
        let mut state = TerminalState::new(
            TerminalOutput::new(Vec::new(), None).unwrap(),
            TerminalRequirements::default(),
            Default::default(),
        );
        state.lifecycle = Lifecycle::Active;
        state.capabilities.kitty_keyboard = CapabilitySupport::Supported;
        state.push_keyboard(false).unwrap();
        state
    }

    #[test]
    fn keyboard_custody_follows_surfaces_and_restoration_is_idempotent() {
        let mut state = active();
        state.enter_alt_screen().unwrap();
        state.enter_alt_screen().unwrap();
        state.leave_alt_screen().unwrap();
        state.enter_alt_screen().unwrap();
        state.restore().unwrap();
        state.restore().unwrap();
        let bytes = &state.output.output;
        let text = String::from_utf8_lossy(bytes);
        assert_eq!(text.matches("\x1b[>1u").count(), 3);
        assert_eq!(text.matches("\x1b[<1u").count(), 3);
        assert!(
            text.ends_with("\x1b[<1u\x1b[?1049l\x1b[<1u\x1b[?25h\x1b[0m"),
            "{text:?}"
        );
    }

    #[test]
    fn panic_restoration_does_not_pop_twice_or_touch_a_suspended_owner() {
        use std::sync::Mutex;
        let owner = Mutex::new(active());
        {
            let mut state = owner.lock().unwrap();
            state.enter_alt_screen().unwrap();
            let before_panic = state.output.output.clone();
            super::super::record_panic(&state.panic_generation);
            super::super::restore_on_panic(&owner);
            assert_eq!(state.output.output, before_panic, "busy hook cannot pop");
            assert!(
                state.usable().is_err(),
                "invalidation cannot require the lock"
            );
        }
        super::super::restore_on_panic(&owner);
        let mut state = owner.lock().unwrap();
        let restored = state.output.output.clone();
        state.restore().unwrap();
        assert_eq!(state.output.output, restored);
        assert!(state.usable().is_err());
        assert!(state.activate().is_err());
        assert_eq!(
            String::from_utf8_lossy(&restored)
                .matches("\x1b[<1u")
                .count(),
            2
        );
        drop(state);

        let suspended = Mutex::new(TerminalState::new(
            TerminalOutput::new(Vec::new(), None).unwrap(),
            TerminalRequirements::default(),
            Default::default(),
        ));
        super::super::restore_on_panic(&suspended);
        assert!(suspended.lock().unwrap().output.output.is_empty());
    }

    #[test]
    fn unknown_surface_never_authorizes_an_alternate_or_duplicate_primary_pop() {
        let mut state = TerminalState::new(
            TerminalOutput::new(
                FlushFault {
                    bytes: Vec::new(),
                    fail: false,
                },
                None,
            )
            .unwrap(),
            TerminalRequirements::default(),
            Default::default(),
        );
        state.lifecycle = Lifecycle::Active;
        state.capabilities.kitty_keyboard = CapabilitySupport::Supported;
        state.push_keyboard(false).unwrap();
        state.output.output.fail = true;
        assert!(state.enter_alt_screen().is_err());
        state.output.output.fail = false;
        state.restore().unwrap();
        state.restore().unwrap();
        let text = String::from_utf8_lossy(&state.output.output.bytes);
        assert_eq!(text.matches("\x1b[>1u").count(), 1);
        assert_eq!(text.matches("\x1b[<1u").count(), 1);
        assert!(text.contains("\x1b[?1049l\x1b[<1u"), "{text:?}");
    }

    #[test]
    fn capture_failure_after_physical_output_consumes_pop_authority() {
        let mut state = active();
        state.output.capture =
            Some(std::fs::File::open(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap());
        assert!(state.restore().is_err());
        let after_fault = state.output.output.len();
        state.output.capture = None;
        assert!(state.restore().is_err());
        assert!(state.restore().is_err());
        let later = String::from_utf8_lossy(&state.output.output[after_fault..]);
        assert!(!later.contains("\x1b[<"), "{later:?}");
        assert!(state.usable().is_err());
    }

    #[test]
    fn poisoned_cleanup_keeps_an_interrupted_pop_indeterminate() {
        use std::sync::Mutex;
        struct PanicFlush {
            bytes: Vec<u8>,
            panic: bool,
        }
        impl Write for PanicFlush {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                assert!(!self.panic, "flush panic");
                Ok(())
            }
        }
        let mut state = TerminalState::new(
            TerminalOutput::new(
                PanicFlush {
                    bytes: Vec::new(),
                    panic: false,
                },
                None,
            )
            .unwrap(),
            TerminalRequirements::default(),
            Default::default(),
        );
        state.lifecycle = Lifecycle::Active;
        state.push_keyboard(false).unwrap();
        let owner = Mutex::new(state);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut state = owner.lock().unwrap();
            state.output.output.panic = true;
            let _ = state.restore();
        }));
        assert!(result.is_err());
        owner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .output
            .output
            .panic = false;
        super::super::restore_on_panic(&owner);
        let mut state = owner.lock().unwrap_or_else(|error| error.into_inner());
        assert!(state.restore().is_err());
        assert!(state.usable().is_err());
        let text = String::from_utf8_lossy(&state.output.output.bytes);
        assert_eq!(text.matches("\x1b[<1u").count(), 1, "{text:?}");
    }

    #[test]
    fn short_write_then_failure_does_not_retry_an_incomplete_pop() {
        struct ShortFault {
            bytes: Vec<u8>,
            remaining: usize,
        }
        impl Write for ShortFault {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                let count = bytes.len().min(self.remaining);
                if count == 0 {
                    return Err(io::Error::other("short write fault"));
                }
                self.bytes.extend_from_slice(&bytes[..count]);
                self.remaining -= count;
                Ok(count)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut state = TerminalState::new(
            TerminalOutput::new(
                ShortFault {
                    bytes: Vec::new(),
                    remaining: usize::MAX,
                },
                None,
            )
            .unwrap(),
            TerminalRequirements::default(),
            Default::default(),
        );
        state.lifecycle = Lifecycle::Active;
        state.capabilities.kitty_keyboard = CapabilitySupport::Supported;
        state.push_keyboard(false).unwrap();
        state.enter_alt_screen().unwrap();
        state.output.output.remaining = 2;
        assert!(state.leave_alt_screen().is_err());
        let after_fault = state.output.output.bytes.len();
        state.output.output.remaining = usize::MAX;
        assert!(state.restore().is_err());
        assert!(state.restore().is_err());
        let later = String::from_utf8_lossy(&state.output.output.bytes[after_fault..]);
        assert_eq!(later.matches("\x1b[<1u").count(), 1, "{later:?}");
        assert!(later.contains("\x1b[?1049l\x1b[<1u"), "{later:?}");
    }

    struct FlushFault {
        bytes: Vec<u8>,
        fail: bool,
    }

    impl Write for FlushFault {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            if self.fail {
                Err(io::Error::other("flush fault"))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn ambiguous_pop_flush_never_retries_the_callers_stack() {
        let mut state = TerminalState::new(
            TerminalOutput::new(
                FlushFault {
                    bytes: Vec::new(),
                    fail: false,
                },
                None,
            )
            .unwrap(),
            TerminalRequirements::default(),
            Default::default(),
        );
        state.lifecycle = Lifecycle::Active;
        state.capabilities.kitty_keyboard = CapabilitySupport::Supported;
        state.push_keyboard(false).unwrap();
        state.enter_alt_screen().unwrap();
        state.output.output.fail = true;
        assert!(state.leave_alt_screen().is_err());
        state.output.output.fail = false;
        assert!(state.restore().is_err());
        assert!(state.restore().is_err());
        let text = String::from_utf8_lossy(&state.output.output.bytes);
        assert_eq!(text.matches("\x1b[<1u").count(), 2, "{text:?}");
        assert!(state.activate().is_err());
        assert!(state.usable().is_err());
    }
}
