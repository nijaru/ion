use std::fs::File;
use std::io::{self, Stdout, Write};
use std::path::{Path, PathBuf};

use crossterm::cursor::Show;
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::{execute, terminal};

use crate::capabilities::{CapabilitySupport, TerminalCapabilities};
use crate::input::InputStream;
use crate::requirements::TerminalRequirements;

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
        self.restored = true;
        let mut first_error = None;
        if self.requirements.bracketed_paste
            && let Err(err) = execute!(self.output, DisableBracketedPaste)
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
        first_error.map_or(Ok(()), Err)
    }

    fn activate(&mut self) -> io::Result<()> {
        terminal::enable_raw_mode()?;
        if self.requirements.bracketed_paste {
            if let Err(err) = execute!(self.output, EnableBracketedPaste) {
                let _ = terminal::disable_raw_mode();
                return Err(err);
            }
            self.capabilities.bracketed_paste = CapabilitySupport::Supported;
        } else {
            self.capabilities.bracketed_paste = CapabilitySupport::Unsupported;
        }
        self.restored = false;
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
    }
}
