use std::io::{self, Write as IoWrite};

use crossterm::{
    cursor, execute, queue,
    style::{
        Attribute, Color as CtColor, Print, SetAttribute, SetBackgroundColor, SetForegroundColor,
    },
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};

use crate::{
    buffer::DrawCommand,
    error::Result,
    geometry::{Position, Rect, Size},
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
///
/// ## Inline mode
///
/// In inline mode the app renders at the cursor's current row (`start_row`)
/// rather than taking over the full screen. All `MoveTo` y-coordinates
/// produced by [`Buffer::diff`] are offset by `start_row` so the UI stays
/// anchored to the right position. On [`Terminal::restore`] the cursor is
/// moved below the rendered region so the shell prompt appears naturally.
pub struct Terminal {
    backend: CrosstermBackend,
    size: Size,
    mode: RenderMode,
    /// The mode set at construction time — used by `restore()` to determine
    /// cleanup behavior even if `switch_mode` changed the active mode.
    initial_mode: RenderMode,
    cursor_visible: bool,
    /// Row where inline rendering begins (always 0 for fullscreen).
    start_row: u16,
    /// Next row to write content above the inline region. Starts at the
    /// shell cursor position. Content is written directly while
    /// `content_cursor < start_row`. Once it reaches `start_row`,
    /// `ScrollUp` is used to push content into scrollback.
    content_cursor: u16,
    /// Height of the last render (used by restore to position the cursor).
    rendered_height: u16,
    /// Terminal-global inline rows that must be cleared before the next frame.
    stale_inline_region: Option<Rect>,
    /// Whether `restore()` has already been called (makes Drop idempotent).
    restored: bool,
}

impl Terminal {
    /// Initialize the terminal.
    ///
    /// Enables raw mode and hides the cursor. Enters the alternate screen for
    /// [`RenderMode::Fullscreen`]; inline mode does not touch the screen buffer.
    ///
    /// For inline mode, the current cursor row is captured as `start_row`
    /// before raw mode is enabled.
    pub fn new(mode: RenderMode) -> Result<Self> {
        let (width, height) = terminal::size()?;

        // For inline mode, anchor the region at the bottom of the terminal.
        // MoveTo is absolute positioning, so we don't need to physically
        // move the cursor — just set start_row and let flush_commands
        // offset all draw commands.
        let (start_row, content_cursor) = match mode {
            RenderMode::Inline { height: h } => {
                let inline_h = h.min(height);
                let start = height.saturating_sub(inline_h);
                // Capture current cursor row so insert_before can fill
                // empty space before resorting to ScrollUp.
                let cursor_row = cursor::position().map(|(_, row)| row).unwrap_or(0);
                (start, cursor_row.min(start))
            }
            RenderMode::Fullscreen => (0, 0),
        };

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
            initial_mode: mode,
            cursor_visible: false,
            start_row,
            content_cursor,
            rendered_height: 0,
            stale_inline_region: None,
            restored: false,
        })
    }

    /// Current terminal size.
    pub fn size(&self) -> Size {
        self.size
    }

    /// Current render mode.
    pub(crate) fn mode(&self) -> RenderMode {
        self.mode
    }

    /// The buffer area that `AppRunner` should allocate for each frame.
    ///
    /// Always starts at `(0, 0)` so widget coordinates are 0-based. The
    /// `start_row` offset is applied in [`flush_commands`] when producing
    /// terminal `MoveTo` commands.
    pub fn render_area(&self) -> Rect {
        match self.mode {
            RenderMode::Fullscreen => Rect::new(0, 0, self.size.width, self.size.height),
            RenderMode::Inline { height } => {
                let h = height.min(self.size.height);
                Rect::new(0, 0, self.size.width, h)
            }
        }
    }

    /// Flush a sequence of [`DrawCommand`]s produced by [`crate::buffer::Buffer::diff`].
    ///
    /// In inline mode, all `MoveTo` y-coordinates are shifted by `start_row`.
    /// When `rendered_height` is less than the previously recorded height (i.e.
    /// the content shrank), the stale rows are cleared with
    /// [`ClearType::CurrentLine`] so they don't linger as ghost lines.
    pub(crate) fn flush_commands(
        &mut self,
        commands: Vec<DrawCommand>,
        rendered_height: u16,
    ) -> Result<()> {
        let prev_height = self.rendered_height;
        let out = &mut self.backend.out;
        if let Some(region) = self.stale_inline_region.take() {
            clear_terminal_region(out, region)?;
        }
        for cmd in commands {
            match cmd {
                DrawCommand::MoveTo(x, y) => {
                    queue!(out, cursor::MoveTo(x, y + self.start_row))?;
                }
                DrawCommand::SetStyle(style) => queue_style(out, style)?,
                DrawCommand::Print(s) => queue!(out, Print(s))?,
                DrawCommand::ResetStyle => queue!(out, SetAttribute(Attribute::Reset))?,
            }
        }
        // In inline mode: clear rows that are no longer part of the rendered
        // region so they don't show as ghost lines when content shrinks.
        if matches!(self.mode, RenderMode::Inline { .. }) && rendered_height < prev_height {
            for row in rendered_height..prev_height {
                queue!(out, cursor::MoveTo(0, self.start_row + row))?;
                queue!(out, Clear(ClearType::CurrentLine))?;
            }
        }
        self.rendered_height = rendered_height;
        // No flush here — caller wraps in begin_sync/end_sync.
        Ok(())
    }

    /// Show or hide the hardware cursor.
    ///
    /// Uses `queue!` so the command is batched with the current frame.
    /// The caller must flush (or use `end_sync`).
    pub fn set_cursor_visible(&mut self, visible: bool) -> Result<()> {
        if visible {
            queue!(self.backend.out, cursor::Show)?;
        } else {
            queue!(self.backend.out, cursor::Hide)?;
        }
        self.cursor_visible = visible;
        Ok(())
    }

    /// Move the hardware cursor (used by the input widget).
    ///
    /// In inline mode, `pos.y` is relative to the render area (0-based) and
    /// is offset by `start_row` automatically.
    ///
    /// Uses `queue!` so the command is batched with the current frame.
    pub fn set_cursor_position(&mut self, pos: Position) -> Result<()> {
        queue!(
            self.backend.out,
            cursor::MoveTo(pos.x, pos.y + self.start_row)
        )?;
        Ok(())
    }

    /// Restore the terminal to its pre-run state.
    ///
    /// - Leaves the alternate screen (fullscreen only).
    /// - Shows the cursor.
    /// - Disables raw mode.
    /// - In inline mode: moves the cursor to the row below the rendered
    ///   region so the shell prompt appears naturally.
    ///
    /// Idempotent — safe to call multiple times (second call is a no-op).
    pub fn restore(&mut self) -> Result<()> {
        if self.restored {
            return Ok(());
        }
        self.restored = true;

        let out = &mut self.backend.out;
        execute!(out, SetAttribute(Attribute::Reset))?;
        execute!(out, cursor::Show)?;

        // If currently in fullscreen (e.g. an overlay) but initially inline,
        // leave the alternate screen first to restore the main buffer.
        if matches!(self.mode, RenderMode::Fullscreen) {
            execute!(out, LeaveAlternateScreen)?;
        }

        // Position cursor for clean shell prompt restoration.
        if matches!(self.initial_mode, RenderMode::Inline { .. }) {
            let inline_h = match self.initial_mode {
                RenderMode::Inline { height } => height.min(self.size.height),
                _ => 0,
            };
            let start = self.size.height.saturating_sub(inline_h);
            let below = start + inline_h;
            execute!(out, cursor::MoveTo(0, below))?;
            writeln!(out)?;
        }

        terminal::disable_raw_mode()?;
        Ok(())
    }

    /// Handle a resize event — updates the cached size.
    ///
    /// For inline mode, `start_row` is clamped so the render region stays
    /// within the terminal.
    pub(crate) fn handle_resize(&mut self, width: u16, height: u16) {
        let old_size = self.size;
        let old_region = inline_region(old_size, self.mode);
        self.size = Size::new(width, height);
        if matches!(self.mode, RenderMode::Inline { .. }) {
            let inline_h = self.render_area().height;
            self.start_row = height.saturating_sub(inline_h);
            self.content_cursor = self.content_cursor.min(self.start_row);
            let new_region = inline_region(self.size, self.mode);
            self.stale_inline_region = union_optional_regions(
                self.stale_inline_region,
                union_optional_regions(old_region, new_region),
            );
        }
    }

    /// Insert lines above the inline region into native terminal scrollback.
    ///
    /// Works in two phases:
    ///
    /// 1. **Direct write** — while `content_cursor < start_row`, lines are
    ///    written directly at the cursor position with no scrolling. This
    ///    fills empty space between the shell prompt and the inline region.
    ///
    /// 2. **Scroll** — once content reaches the inline region, `ScrollUp`
    ///    pushes content into terminal scrollback. Large inserts are batched
    ///    (at most `start_row` lines per scroll).
    ///
    /// Does **not** flush — the caller is responsible for flushing after all
    /// terminal output for the frame is queued.
    ///
    /// No-op in fullscreen mode or when `lines` is empty.
    pub fn insert_before(&mut self, lines: &[String]) -> Result<()> {
        if !matches!(self.mode, RenderMode::Inline { .. }) || lines.is_empty() {
            return Ok(());
        }

        let out = &mut self.backend.out;
        let mut offset = 0;

        // Phase 1: Direct write while there's empty space above the inline region.
        if self.content_cursor < self.start_row {
            let available = (self.start_row - self.content_cursor) as usize;
            let direct_count = (lines.len() - offset).min(available);
            for i in 0..direct_count {
                let row = self.content_cursor + i as u16;
                queue!(out, cursor::MoveTo(0, row))?;
                queue!(out, Clear(ClearType::CurrentLine))?;
                queue!(out, Print(&lines[offset + i]))?;
            }
            self.content_cursor += direct_count as u16;
            offset += direct_count;
        }

        // Phase 2: ScrollUp for remaining lines (content_cursor >= start_row).
        if offset < lines.len() {
            let remaining = &lines[offset..];
            let max_batch = self.start_row as usize;
            if max_batch == 0 {
                return Ok(());
            }

            let mut batch_offset = 0;
            while batch_offset < remaining.len() {
                let batch_end = (batch_offset + max_batch).min(remaining.len());
                let batch = &remaining[batch_offset..batch_end];
                let n = batch.len() as u16;

                queue!(out, terminal::ScrollUp(n))?;

                let write_row = self.start_row.saturating_sub(n);
                for (i, line) in batch.iter().enumerate() {
                    let row = write_row + i as u16;
                    queue!(out, cursor::MoveTo(0, row))?;
                    queue!(out, Clear(ClearType::CurrentLine))?;
                    queue!(out, Print(line))?;
                }

                batch_offset = batch_end;
            }
        }

        Ok(())
    }

    /// Begin a synchronized update — all output until [`end_sync`] is buffered
    /// and applied atomically, preventing flicker.
    pub fn begin_sync(&mut self) -> Result<()> {
        queue!(self.backend.out, terminal::BeginSynchronizedUpdate)?;
        Ok(())
    }

    /// End a synchronized update and flush all queued output.
    pub fn end_sync(&mut self) -> Result<()> {
        queue!(self.backend.out, terminal::EndSynchronizedUpdate)?;
        self.backend.out.flush()?;
        Ok(())
    }

    /// Switch between render modes at runtime.
    pub fn switch_mode(&mut self, mode: RenderMode) -> Result<()> {
        if mode == self.mode {
            return Ok(());
        }
        let old_region = inline_region(self.size, self.mode);
        let new_region = inline_region(self.size, mode);
        match (&self.mode, &mode) {
            (RenderMode::Inline { .. }, RenderMode::Fullscreen) => {
                execute!(self.backend.out, EnterAlternateScreen)?;
                self.start_row = 0;
            }
            (RenderMode::Inline { .. }, RenderMode::Inline { height: h }) => {
                let inline_h = (*h).min(self.size.height);
                self.start_row = self.size.height.saturating_sub(inline_h);
                // If the inline region grows upward, rows that used to be free
                // space become part of the UI. Clamp the content cursor so new
                // inserts never write into that expanded UI area.
                self.content_cursor = self.content_cursor.min(self.start_row);
            }
            (RenderMode::Fullscreen, RenderMode::Inline { height: h }) => {
                execute!(self.backend.out, LeaveAlternateScreen)?;
                let inline_h = (*h).min(self.size.height);
                self.start_row = self.size.height.saturating_sub(inline_h);
                // After leaving fullscreen, the main buffer is restored.
                // Content cursor is at start_row since we can't know what's
                // visible — subsequent inserts will use ScrollUp.
                self.content_cursor = self.start_row;
            }
            _ => {}
        }
        self.mode = mode;
        self.rendered_height = 0;
        self.stale_inline_region = union_optional_regions(
            self.stale_inline_region,
            union_optional_regions(old_region, new_region),
        );
        Ok(())
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.restore();
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

fn inline_region(size: Size, mode: RenderMode) -> Option<Rect> {
    match mode {
        RenderMode::Fullscreen => None,
        RenderMode::Inline { height } => {
            let h = height.min(size.height);
            Some(Rect::new(0, size.height.saturating_sub(h), size.width, h))
        }
    }
}

fn union_optional_regions(a: Option<Rect>, b: Option<Rect>) -> Option<Rect> {
    match (a, b) {
        (Some(a), Some(b)) => Some(union_rect(a, b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn union_rect(a: Rect, b: Rect) -> Rect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    let right = a.x.saturating_add(a.width).max(b.x.saturating_add(b.width));
    let bottom =
        a.y.saturating_add(a.height)
            .max(b.y.saturating_add(b.height));
    Rect::new(x, y, right.saturating_sub(x), bottom.saturating_sub(y))
}

fn clear_terminal_region<W: IoWrite>(out: &mut W, region: Rect) -> io::Result<()> {
    for row in region.y..region.y.saturating_add(region.height) {
        queue!(out, cursor::MoveTo(region.x, row))?;
        queue!(out, Clear(ClearType::CurrentLine))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{RenderMode, inline_region, union_optional_regions, union_rect};
    use crate::geometry::{Rect, Size};

    #[test]
    fn inline_region_is_bottom_anchored() {
        let region = inline_region(Size::new(80, 24), RenderMode::Inline { height: 5 }).unwrap();
        assert_eq!(region, Rect::new(0, 19, 80, 5));
    }

    #[test]
    fn union_rect_covers_growth_transition() {
        let old = Rect::new(0, 19, 80, 5);
        let new = Rect::new(0, 18, 80, 6);
        assert_eq!(union_rect(old, new), Rect::new(0, 18, 80, 6));
    }

    #[test]
    fn union_optional_regions_preserves_existing_pending_clear() {
        let pending = Some(Rect::new(0, 18, 80, 6));
        let resize = Some(Rect::new(0, 17, 80, 7));
        assert_eq!(
            union_optional_regions(pending, resize),
            Some(Rect::new(0, 17, 80, 7))
        );
    }
}
