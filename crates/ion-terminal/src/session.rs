use std::fs::File;
use std::io::{self, Stdout, Write};
use std::path::{Path, PathBuf};

use crossterm::cursor::Show;
use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::{SynchronizedUpdate, execute, terminal};

use crate::capabilities::{CapabilitySupport, TerminalCapabilities};
use crate::input::InputStream;
use crate::requirements::TerminalRequirements;
use crate::{Frame, Screen};

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

    pub fn record_external(&mut self, bytes: &[u8]) -> io::Result<()> {
        if let Some(capture) = &mut self.capture {
            capture.write_all(bytes)?;
        }
        Ok(())
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

/// One owner for raw mode, bracketed paste, output capture, and input
/// creation. `suspend` and `resume` are idempotent lifecycle transitions.
pub struct TerminalSession {
    output: TerminalOutput<Stdout>,
    requirements: TerminalRequirements,
    capabilities: TerminalCapabilities,
    restored: bool,
    keyboard_enhancement_enabled: bool,
    focus_reporting_enabled: bool,
    mouse_enabled: bool,
}

impl TerminalSession {
    pub fn enter() -> io::Result<Self> {
        Self::with_requirements(TerminalRequirements::default())
    }

    pub fn with_requirements(requirements: TerminalRequirements) -> io::Result<Self> {
        let output = TerminalOutput::from_environment(io::stdout())?;
        let mut session = Self {
            output,
            requirements,
            capabilities: TerminalCapabilities::default(),
            restored: true,
            keyboard_enhancement_enabled: false,
            focus_reporting_enabled: false,
            mouse_enabled: false,
        };
        session.activate()?;
        Ok(session)
    }

    pub fn output(&mut self) -> &mut TerminalOutput<Stdout> {
        &mut self.output
    }

    pub fn input(&self) -> InputStream {
        InputStream::new()
    }

    pub fn size(&self) -> io::Result<(u16, u16)> {
        terminal::size()
    }

    pub fn cursor_position(&self) -> io::Result<(u16, u16)> {
        crossterm::cursor::position()
    }

    pub fn capabilities(&self) -> TerminalCapabilities {
        self.capabilities
    }

    pub fn requirements(&self) -> TerminalRequirements {
        self.requirements
    }

    /// Render one frame under the negotiated output policy. Synchronized
    /// output is opt-in because terminals may ignore the private mode.
    pub fn render(&mut self, screen: &mut Screen, frame: &Frame<'_>) -> io::Result<()> {
        if self.requirements.synchronized_output {
            self.output
                .sync_update(|output| screen.draw(output, frame))?
        } else {
            screen.draw(&mut self.output, frame)
        }
    }

    pub fn suspend(&mut self) -> io::Result<()> {
        self.restore()
    }

    pub fn resume(&mut self) -> io::Result<()> {
        if !self.restored {
            return Ok(());
        }
        self.activate()
    }

    pub fn restore(&mut self) -> io::Result<()> {
        if self.restored {
            return Ok(());
        }
        let mut first_error = None;
        if self.keyboard_enhancement_enabled {
            match execute!(self.output, PopKeyboardEnhancementFlags) {
                Ok(()) => self.keyboard_enhancement_enabled = false,
                Err(err) => first_error = Some(err),
            }
        }
        if self.mouse_enabled {
            match execute!(self.output, DisableMouseCapture) {
                Ok(()) => self.mouse_enabled = false,
                Err(err) if first_error.is_none() => first_error = Some(err),
                Err(_) => {}
            }
        }
        if self.focus_reporting_enabled {
            match execute!(self.output, DisableFocusChange) {
                Ok(()) => self.focus_reporting_enabled = false,
                Err(err) if first_error.is_none() => first_error = Some(err),
                Err(_) => {}
            }
        }
        if self.requirements.bracketed_paste
            && let Err(err) = execute!(self.output, DisableBracketedPaste)
            && first_error.is_none()
        {
            first_error = Some(err);
        }
        if let Err(err) = terminal::disable_raw_mode()
            && first_error.is_none()
        {
            first_error = Some(err);
        }
        if let Err(err) = self.output.flush()
            && first_error.is_none()
        {
            first_error = Some(err);
        }
        if first_error.is_none() {
            self.restored = true;
        }
        first_error.map_or(Ok(()), Err)
    }

    fn activate(&mut self) -> io::Result<()> {
        terminal::enable_raw_mode()?;
        self.restored = false;
        if self.requirements.bracketed_paste {
            if let Err(err) = execute!(self.output, EnableBracketedPaste) {
                let _ = self.restore();
                return Err(err);
            }
            self.capabilities.bracketed_paste = CapabilitySupport::Supported;
        } else {
            self.capabilities.bracketed_paste = CapabilitySupport::Unsupported;
        }

        if self.requirements.focus_reporting {
            if let Err(err) = execute!(self.output, EnableFocusChange) {
                let _ = self.restore();
                return Err(err);
            }
            self.focus_reporting_enabled = true;
            self.capabilities.focus_reporting = CapabilitySupport::Supported;
        } else {
            self.capabilities.focus_reporting = CapabilitySupport::Unsupported;
        }

        if self.requirements.mouse {
            if let Err(err) = execute!(self.output, EnableMouseCapture) {
                let _ = self.restore();
                return Err(err);
            }
            self.mouse_enabled = true;
            self.capabilities.mouse = CapabilitySupport::Supported;
        } else {
            self.capabilities.mouse = CapabilitySupport::Unsupported;
        }

        if self.requirements.keyboard_enhancement {
            match terminal::supports_keyboard_enhancement() {
                Ok(true) => {
                    let flags = KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES;
                    if let Err(err) = execute!(self.output, PushKeyboardEnhancementFlags(flags)) {
                        let _ = self.restore();
                        return Err(err);
                    }
                    self.keyboard_enhancement_enabled = true;
                    self.capabilities.kitty_keyboard = CapabilitySupport::Supported;
                }
                Ok(false) => {
                    self.capabilities.kitty_keyboard = CapabilitySupport::Unsupported;
                }
                Err(_) => {
                    self.capabilities.kitty_keyboard = CapabilitySupport::Unknown;
                }
            }
        } else {
            self.capabilities.kitty_keyboard = CapabilitySupport::Unsupported;
        }
        Ok(())
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

/// Install a panic hook that restores the process terminal before the
/// previous hook prints its diagnostic.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(io::stdout(), DisableBracketedPaste, Show);
        let _ = io::stdout().write_all(b"\x1b[0m");
        let _ = io::stdout().flush();
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_requirements_are_inline_and_paste_enabled() {
        let requirements = TerminalRequirements::default();
        assert_eq!(requirements.surface, crate::TerminalSurface::Inline);
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
        let bytes = output.output;
        let text = String::from_utf8(bytes).expect("utf8");
        assert!(text.starts_with("\x1b[?2026h"));
        assert!(text.ends_with("\x1b[?2026l"));
        assert!(text.contains("frame"));
    }
}
