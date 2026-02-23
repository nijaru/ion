use std::io::{self, Write as IoWrite};

use crossterm::{
    cursor, execute, queue,
    style::{
        Attribute, Color as CtColor, Print, SetAttribute, SetBackgroundColor, SetForegroundColor,
    },
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};

use crate::{
    buffer::DrawCommand,
    error::Result,
    geometry::{Position, Size},
    style::{Color, Style, StyleModifiers},
};

/// How the app occupies the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    /// Own the full terminal (alternate screen buffer).
    Fullscreen,
    /// Render inline at the current cursor position, bounded to `height` rows.
    Inline { height: u16 },
}

struct CrosstermBackend {
    out: io::Stdout,
}

/// Owns raw mode, the alternate screen, cursor visibility, and I/O flushing.
///
/// Crossterm is used here and nowhere else in the public API.
pub struct Terminal {
    backend: CrosstermBackend,
    size: Size,
    mode: RenderMode,
    cursor_visible: bool,
}

impl Terminal {
    /// Initialize the terminal.
    ///
    /// Enables raw mode and hides the cursor. Enters the alternate screen for
    /// [`RenderMode::Fullscreen`]; inline mode does not touch the screen buffer.
    pub fn new(mode: RenderMode) -> Result<Self> {
        let (width, height) = terminal::size()?;
        terminal::enable_raw_mode()?;
        let mut out = io::stdout();
        if matches!(mode, RenderMode::Fullscreen) {
            execute!(out, EnterAlternateScreen)?;
        }
        execute!(out, cursor::Hide)?;
        Ok(Self {
            backend: CrosstermBackend { out },
            size: Size::new(width, height),
            mode,
            cursor_visible: false,
        })
    }

    /// Current terminal size.
    pub fn size(&self) -> Size {
        self.size
    }

    /// Flush a sequence of [`DrawCommand`]s produced by [`crate::buffer::Buffer::diff`].
    pub(crate) fn flush_commands(&mut self, commands: Vec<DrawCommand>) -> Result<()> {
        let out = &mut self.backend.out;
        for cmd in commands {
            match cmd {
                DrawCommand::MoveTo(x, y) => queue!(out, cursor::MoveTo(x, y))?,
                DrawCommand::SetStyle(style) => queue_style(out, style)?,
                DrawCommand::Print(s) => queue!(out, Print(s))?,
                DrawCommand::ResetStyle => queue!(out, SetAttribute(Attribute::Reset))?,
            }
        }
        out.flush()?;
        Ok(())
    }

    /// Show or hide the hardware cursor.
    pub fn set_cursor_visible(&mut self, visible: bool) -> Result<()> {
        if visible {
            execute!(self.backend.out, cursor::Show)?;
        } else {
            execute!(self.backend.out, cursor::Hide)?;
        }
        self.cursor_visible = visible;
        Ok(())
    }

    /// Move the hardware cursor (used by the input widget).
    pub fn set_cursor_position(&mut self, pos: Position) -> Result<()> {
        execute!(self.backend.out, cursor::MoveTo(pos.x, pos.y))?;
        Ok(())
    }

    /// Restore the terminal to its pre-run state:
    /// - Leave alternate screen if fullscreen.
    /// - Show cursor.
    /// - Disable raw mode.
    pub fn restore(mut self) -> Result<()> {
        let out = &mut self.backend.out;
        execute!(out, SetAttribute(Attribute::Reset))?;
        execute!(out, cursor::Show)?;
        if matches!(self.mode, RenderMode::Fullscreen) {
            execute!(out, LeaveAlternateScreen)?;
        }
        terminal::disable_raw_mode()?;
        Ok(())
    }

    /// Handle a resize event — updates the cached size.
    pub(crate) fn handle_resize(&mut self, width: u16, height: u16) {
        self.size = Size::new(width, height);
    }

    /// Switch between render modes at runtime.
    pub fn switch_mode(&mut self, mode: RenderMode) -> Result<()> {
        match (&self.mode, &mode) {
            (RenderMode::Inline { .. }, RenderMode::Fullscreen) => {
                execute!(self.backend.out, EnterAlternateScreen)?;
            }
            (RenderMode::Fullscreen, RenderMode::Inline { .. }) => {
                execute!(self.backend.out, LeaveAlternateScreen)?;
            }
            _ => {}
        }
        self.mode = mode;
        Ok(())
    }
}

/// Apply a [`Style`] to the terminal via queued crossterm commands.
///
/// Always resets first then applies, so styles compose correctly across
/// adjacent cells with different attributes.
fn queue_style<W: IoWrite>(out: &mut W, style: Style) -> io::Result<()> {
    queue!(out, SetAttribute(Attribute::Reset))?;

    if let Some(fg) = style.fg {
        queue!(out, SetForegroundColor(color_to_ct(fg)))?;
    }
    if let Some(bg) = style.bg {
        queue!(out, SetBackgroundColor(color_to_ct(bg)))?;
    }

    if style.modifiers.contains(StyleModifiers::BOLD) {
        queue!(out, SetAttribute(Attribute::Bold))?;
    }
    if style.modifiers.contains(StyleModifiers::DIM) {
        queue!(out, SetAttribute(Attribute::Dim))?;
    }
    if style.modifiers.contains(StyleModifiers::ITALIC) {
        queue!(out, SetAttribute(Attribute::Italic))?;
    }
    if style.modifiers.contains(StyleModifiers::UNDERLINE) {
        queue!(out, SetAttribute(Attribute::Underlined))?;
    }
    if style.modifiers.contains(StyleModifiers::BLINK) {
        queue!(out, SetAttribute(Attribute::SlowBlink))?;
    }
    if style.modifiers.contains(StyleModifiers::REVERSED) {
        queue!(out, SetAttribute(Attribute::Reverse))?;
    }
    if style.modifiers.contains(StyleModifiers::HIDDEN) {
        queue!(out, SetAttribute(Attribute::Hidden))?;
    }
    if style.modifiers.contains(StyleModifiers::STRIKETHROUGH) {
        queue!(out, SetAttribute(Attribute::CrossedOut))?;
    }

    Ok(())
}

fn color_to_ct(c: Color) -> CtColor {
    match c {
        Color::Reset => CtColor::Reset,
        Color::Black => CtColor::Black,
        Color::Red => CtColor::DarkRed,
        Color::Green => CtColor::DarkGreen,
        Color::Yellow => CtColor::DarkYellow,
        Color::Blue => CtColor::DarkBlue,
        Color::Magenta => CtColor::DarkMagenta,
        Color::Cyan => CtColor::DarkCyan,
        Color::White => CtColor::White,
        Color::DarkGray => CtColor::DarkGrey,
        Color::LightRed => CtColor::Red,
        Color::LightGreen => CtColor::Green,
        Color::LightYellow => CtColor::Yellow,
        Color::LightBlue => CtColor::Blue,
        Color::LightMagenta => CtColor::Magenta,
        Color::LightCyan => CtColor::Cyan,
        Color::Gray => CtColor::Grey,
        Color::Rgb(r, g, b) => CtColor::Rgb { r, g, b },
        Color::Indexed(i) => CtColor::AnsiValue(i),
    }
}
