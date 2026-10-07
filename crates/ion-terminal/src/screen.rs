//! Line-diff renderer for Ion's mutable terminal surface.
//!
//! Normal inline chat owns only a live band. Settled transcript rows can be
//! appended once with `commit_text_lines`, after which the physical terminal
//! owns their scrollback/reflow. Drawing never publishes or scrolls live rows.
//! Fullscreen rendering is a separate transient surface.

use std::io::{self, Write};

use ratatui::buffer::{Buffer, CellWidth};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget};

/// Wrapped live rows, bottom-aligned and clipped to the reserved inline band.
/// Settled rows must be published separately through `commit_text_lines`.
pub struct Frame<'a> {
    pub live: &'a [Line<'a>],
    /// (live row, column); None hides the hardware cursor.
    pub cursor: Option<(usize, u16)>,
}

/// A virtual terminal surface used by the renderer before bytes are emitted.
/// It is deliberately small: the surface owns cells and dimensions, while
/// `Screen` owns physical cursor/scrollback policy.
pub struct Surface {
    buffer: Buffer,
}

impl Surface {
    #[must_use]
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            buffer: Buffer::empty(Rect::new(0, 0, width.max(1), height.max(1))),
        }
    }

    #[must_use]
    pub fn size(&self) -> (u16, u16) {
        (self.buffer.area.width, self.buffer.area.height)
    }

    pub fn resize(&mut self, width: u16, height: u16) {
        self.buffer
            .resize(Rect::new(0, 0, width.max(1), height.max(1)));
    }

    pub fn render_line(&mut self, line: Line<'_>, row: u16) {
        if row < self.buffer.area.height {
            Paragraph::new(line).render(
                Rect::new(0, row, self.buffer.area.width, 1),
                &mut self.buffer,
            );
        }
    }

    #[must_use]
    pub fn row_text(&self, row: u16) -> String {
        if row >= self.buffer.area.height {
            return String::new();
        }
        (0..self.buffer.area.width)
            .map(|column| self.buffer[(column, row)].symbol())
            .collect()
    }
}

/// What the physical screen shows in our window right now.
struct Window {
    surface: Surface,
    /// One-based cursor row to use for the shell prompt after this
    /// frame. It is the first row after the rendered content, capped at
    /// the terminal bottom when the window is full-height.
    finish_row: u16,
}

pub struct Screen {
    width: u16,
    /// Physical row where our region starts (the launch cursor). The
    /// region extends to the screen bottom; completed content above it
    /// is native terminal scrollback.
    origin: u16,
    screen_height: u16,
    current: Option<Window>,
    /// Last emitted hardware-cursor state; avoids redundant hide/show
    /// sequences between frames (perceived flicker while typing).
    cursor_shown: bool,
    cursor_at: Option<(u16, u16)>,
    /// Reserved mutable band height. Shorter frames are bottom-aligned;
    /// growing the reservation clears live cells before making physical room.
    live_height: Option<usize>,
    /// Previous fullscreen (alt-screen) frame, if the frontend is in
    /// fullscreen mode. Inline frames and fullscreen frames never
    /// compare: entering fullscreen forces a full repaint.
    fullscreen: Option<Surface>,
}

impl Screen {
    /// `origin_row` is the physical row the region starts on (the
    /// launch cursor after the banner); the region extends to the
    /// bottom of the `screen_height`-row terminal.
    pub fn new(width: u16, origin_row: u16, screen_height: u16) -> Self {
        let screen_height = screen_height.max(1);
        let origin = origin_row.min(screen_height.saturating_sub(1));
        Self {
            width: width.max(1),
            origin,
            screen_height,
            current: None,
            cursor_shown: false,
            cursor_at: None,
            live_height: None,
            fullscreen: None,
        }
    }

    /// Configure a stable virtual live band for an inline frontend. A
    /// shorter frame is bottom-aligned without scrolling; the frontend may
    /// still render fewer rows when the band has little content.
    pub fn with_live_height(
        width: u16,
        origin_row: u16,
        screen_height: u16,
        live_height: usize,
    ) -> Self {
        let mut screen = Self::new(width, origin_row, screen_height);
        screen.live_height = Some(live_height.max(1).min(screen.avail() as usize));
        screen
    }

    pub fn size(&self) -> (u16, u16) {
        (self.width, self.avail())
    }

    #[must_use]
    pub fn live_height(&self) -> usize {
        self.live_height.unwrap_or(1)
    }

    /// Grow the mutable inline band without ever scrolling mutable content
    /// into native history. The band only grows during one inline session.
    pub fn ensure_live_height(&mut self, out: &mut impl Write, rows: usize) -> io::Result<()> {
        let rows = rows.max(1).min(self.screen_height as usize);
        if rows <= self.live_height() {
            return Ok(());
        }

        let available = self.avail() as usize;
        if available < rows {
            // Erase the mutable surface before scrolling. Only terminal-owned
            // content above the band may move into scrollback.
            self.clear_inline_surface(out)?;
            let grow = rows - available;
            for _ in 0..grow {
                write!(out, "\x1b[{};1H\r\n", self.screen_height)?;
            }
            out.flush()?;
            self.origin = self
                .origin
                .saturating_sub(grow.min(u16::MAX as usize) as u16);
        }

        self.live_height = Some(rows);
        self.current = None;
        self.fullscreen = None;
        self.cursor_shown = false;
        self.cursor_at = None;
        Ok(())
    }

    /// Visible rows of the region: origin to screen bottom. Follows
    /// terminal growth and shrinkage.
    fn avail(&self) -> u16 {
        self.screen_height.saturating_sub(self.origin).max(1)
    }

    /// Repaint at the new dimensions without advancing terminal history.
    /// Native reflow remains owned by the terminal, not this cell cache.
    pub fn resize(&mut self, width: u16, height: u16) {
        let width = width.max(1);
        let height = height.max(1);
        if width == self.width && height == self.screen_height {
            return;
        }
        self.width = width;
        self.screen_height = height;
        if self.origin >= height.saturating_sub(1) {
            self.origin = height.saturating_sub(2);
        }
        self.live_height = self.live_height.map(|rows| rows.min(self.avail() as usize));
        self.invalidate();
    }

    /// Change the reservation without publishing or scrolling live rows.
    /// Frontends should shrink it after publication or a surface reset.
    pub fn set_live_height(&mut self, live_height: usize) {
        self.live_height = Some(live_height.max(1).min(self.avail() as usize));
        // Retain the old physical surface so freed rows are erased on redraw.
    }

    /// Force a full repaint on the next draw without changing size.
    /// Used after suspend/resume: the host terminal's visible surface
    /// is no longer what this Screen believes it is.
    pub fn invalidate(&mut self) {
        self.current = None;
        self.fullscreen = None;
        self.cursor_shown = false;
        self.cursor_at = None;
    }

    /// Append settled plain-text rows exactly once above the mutable live band.
    ///
    /// The current live surface is discarded, the rows are printed from the
    /// band's anchor with explicit CRLFs, and ordinary terminal scrolling moves
    /// older content into native scrollback. The anchor advances to the cursor's
    /// resulting physical row. Already committed rows are never re-rendered on
    /// resize; a subsequent `draw` repaints only the live band.
    pub fn commit_text_lines(&mut self, out: &mut impl Write, lines: &[String]) -> io::Result<()> {
        if lines.is_empty() {
            return Ok(());
        }
        self.clear_inline_surface(out)?;
        let mut origin = self.origin;
        for line in lines {
            write!(out, "\x1b[{};1H\x1b[2K{line}\r\n", origin + 1)?;
            origin = origin.saturating_add(1).min(self.screen_height - 1);
        }
        out.flush()?;
        self.origin = origin;
        self.live_height = Some(self.live_height().min(self.avail() as usize));
        self.invalidate();
        Ok(())
    }

    fn clear_inline_surface(&self, out: &mut impl Write) -> io::Result<()> {
        // At row one, erase-to-end is a full-screen erase. tmux saves that
        // screen in history, including provisional work. Erase owned rows
        // individually so only explicit publication can advance history.
        write!(out, "\x1b[0m")?;
        for row in self.origin..self.screen_height {
            write!(out, "\x1b[{};1H\x1b[2K", row + 1)?;
        }
        Ok(())
    }

    /// Repaint only the mutable band. Reservation growth and publication are
    /// explicit operations; resizing or replacing a frame cannot scroll it.
    pub fn draw(&mut self, out: &mut impl Write, frame: &Frame) -> io::Result<()> {
        let previous = self.current.take();
        let height = self.avail();
        let band = self
            .live_height
            .unwrap_or_else(|| frame.live.len().max(1))
            .min(height as usize);
        self.live_height = Some(band);
        let dropped = frame.live.len().saturating_sub(band);
        let padding = band.saturating_sub(frame.live.len());
        let mut next = Surface::new(self.width, height);
        for (index, line) in frame.live.iter().skip(dropped).enumerate() {
            next.render_line(line.clone(), (padding + index) as u16);
        }
        let mut painted = false;
        for row in 0..height {
            if previous
                .as_ref()
                .is_none_or(|prev| row_differs(&next.buffer, row, &prev.surface.buffer, row))
            {
                emit_buffer_row(out, &next.buffer, row, self.origin)?;
                painted = true;
            }
        }
        if painted {
            write!(out, "\x1b[0m")?;
        }
        let cursor = frame
            .cursor
            .filter(|(row, col)| *row < frame.live.len() && *col < self.width)
            .and_then(|(row, col)| {
                let row = row.checked_sub(dropped)? + padding;
                (row < band).then_some((self.origin + row as u16, col))
            });
        if let Some(at) = cursor {
            if painted || !self.cursor_shown || self.cursor_at != Some(at) {
                write!(out, "\x1b[{};{}H", at.0 + 1, at.1 + 1)?;
                if !self.cursor_shown {
                    write!(out, "\x1b[?25h")?;
                }
            }
        } else if self.cursor_shown {
            write!(out, "\x1b[?25l")?;
        }
        out.flush()?;
        self.cursor_shown = cursor.is_some();
        self.cursor_at = cursor;
        self.current = Some(Window {
            surface: next,
            finish_row: self
                .origin
                .saturating_add(band as u16)
                .saturating_add(1)
                .min(self.screen_height),
        });
        Ok(())
    }

    /// Render one fullscreen (alt-screen) frame. `rows` are the
    /// viewport lines: the transcript slice [scroll, scroll + height)
    /// followed by the pinned bottom chrome (search/status lines). The
    /// fullscreen surface uses the whole terminal: origin is 0 and the
    /// frame is compared against the previous fullscreen frame only —
    /// inline and fullscreen surfaces never mix.
    pub fn draw_fullscreen(
        &mut self,
        out: &mut impl Write,
        rows: &[Line<'_>],
        cursor: Option<(usize, u16)>,
    ) -> io::Result<()> {
        let previous = self.fullscreen.take();
        let h = self.screen_height as usize;
        let w = self.width;
        let mut next = Surface::new(w, self.screen_height);
        for (row, line) in rows.iter().take(h).enumerate() {
            next.render_line(line.clone(), row as u16);
        }
        let mut painted = false;
        for r in 0..h as u16 {
            let comparable = previous
                .as_ref()
                .is_some_and(|prev| r < prev.buffer.area.height);
            if !comparable
                || row_differs(
                    &next.buffer,
                    r,
                    &previous.as_ref().expect("checked").buffer,
                    r,
                )
            {
                emit_buffer_row(out, &next.buffer, r, 0)?;
                painted = true;
            }
        }
        // Hardware cursor: shown only while the docked composer owns
        // it. The cursor position is a viewport row (0-based, physical),
        // not a scrollback-absolute row.
        if let Some((row, col)) = cursor
            && (row as u16) < self.screen_height
        {
            write!(out, "\x1b[{};{}H\x1b[?25h", row + 1, col + 1)?;
        } else if self.cursor_shown {
            write!(out, "\x1b[?25l")?;
        }
        self.cursor_shown = cursor.is_some();
        if painted {
            write!(out, "\x1b[0m")?;
        }
        out.flush()?;
        self.fullscreen = Some(next);
        Ok(())
    }

    /// Park below the active rendered content on shutdown so the shell
    /// prompt follows the footer instead of the unused physical region.
    pub fn finish(&mut self, out: &mut impl Write) -> io::Result<()> {
        let row = self
            .current
            .as_ref()
            .map_or(self.origin.saturating_add(1), |window| window.finish_row);
        write!(out, "\x1b[{row};1H\x1b[?25h\x1b[0m\r\n")
    }
}

fn row_differs(next: &Buffer, r: u16, prev: &Buffer, prev_r: u16) -> bool {
    let w = next.area.width;
    for x in 0..w {
        if !cells_equal(&next[(x, r)], &prev[(x, prev_r)]) {
            return true;
        }
    }
    false
}

/// Rewrite one full row from the freshly rendered buffer. Row-level
/// granularity keeps wide characters consistent: both compared sides
/// come from complete renders, so continuation cells can never join
/// across a partial edit.
fn emit_buffer_row(out: &mut impl Write, buf: &Buffer, r: u16, origin: u16) -> io::Result<()> {
    let w = buf.area.width;
    let mut x = 0u16;
    while x < w {
        let cell = buf[(x, r)].clone();
        let start = x;
        let mut text = String::new();
        while x < w {
            let c = buf[(x, r)].clone();
            if !text.is_empty() && c.style() != cell.style() {
                break;
            }
            text.push_str(c.symbol());
            // A wide glyph already paints its continuation cells. Emitting
            // those reset cells again shifts text and can wrap the bottom row.
            x += c.cell_width().max(1);
        }
        // Blank runs are written too: they erase whatever the previous
        // frame left in that row (shrink, lag rebuild).
        write!(out, "\x1b[{};{}H", origin + r + 1, start + 1)?;
        emit_style(out, cell.style())?;
        write!(out, "{text}")?;
        write!(out, "\x1b[0m")?;
    }
    Ok(())
}

fn cells_equal(a: &ratatui::buffer::Cell, b: &ratatui::buffer::Cell) -> bool {
    a.symbol() == b.symbol() && a.fg == b.fg && a.bg == b.bg && a.modifier == b.modifier
}

fn emit_style(out: &mut impl Write, style: Style) -> io::Result<()> {
    if let Some(fg) = style.fg {
        write!(out, "{}", fg_sgr(fg))?;
    }
    if let Some(bg) = style.bg {
        write!(out, "{}", bg_sgr(bg))?;
    }
    let m = style.add_modifier;
    let mut attrs = String::new();
    if m.contains(ratatui::style::Modifier::BOLD) {
        attrs.push_str("\x1b[1m");
    }
    if m.contains(ratatui::style::Modifier::DIM) {
        attrs.push_str("\x1b[2m");
    }
    if m.contains(ratatui::style::Modifier::ITALIC) {
        attrs.push_str("\x1b[3m");
    }
    if m.contains(ratatui::style::Modifier::REVERSED) {
        attrs.push_str("\x1b[7m");
    }
    write!(out, "{attrs}")
}

fn fg_sgr(color: Color) -> String {
    match color {
        Color::Reset => "\x1b[39m".into(),
        Color::Black => "\x1b[30m".into(),
        Color::Red => "\x1b[31m".into(),
        Color::Green => "\x1b[32m".into(),
        Color::Yellow => "\x1b[33m".into(),
        Color::Blue => "\x1b[34m".into(),
        Color::Magenta => "\x1b[35m".into(),
        Color::Cyan => "\x1b[36m".into(),
        Color::Gray => "\x1b[37m".into(),
        Color::DarkGray => "\x1b[90m".into(),
        Color::LightRed => "\x1b[91m".into(),
        Color::LightGreen => "\x1b[92m".into(),
        Color::LightYellow => "\x1b[93m".into(),
        Color::LightBlue => "\x1b[94m".into(),
        Color::LightMagenta => "\x1b[95m".into(),
        Color::LightCyan => "\x1b[96m".into(),
        Color::White => "\x1b[97m".into(),
        Color::Indexed(i) => format!("\x1b[38;5;{i}m"),
        Color::Rgb(r, g, b) => format!("\x1b[38;2;{r};{g};{b}m"),
    }
}

fn bg_sgr(color: Color) -> String {
    match color {
        Color::Reset => "\x1b[49m".into(),
        Color::Black => "\x1b[40m".into(),
        Color::Red => "\x1b[41m".into(),
        Color::Green => "\x1b[42m".into(),
        Color::Yellow => "\x1b[43m".into(),
        Color::Blue => "\x1b[44m".into(),
        Color::Magenta => "\x1b[45m".into(),
        Color::Cyan => "\x1b[46m".into(),
        Color::Gray => "\x1b[47m".into(),
        Color::DarkGray => "\x1b[100m".into(),
        Color::LightRed => "\x1b[101m".into(),
        Color::LightGreen => "\x1b[102m".into(),
        Color::LightYellow => "\x1b[103m".into(),
        Color::LightBlue => "\x1b[104m".into(),
        Color::LightMagenta => "\x1b[105m".into(),
        Color::LightCyan => "\x1b[106m".into(),
        Color::White => "\x1b[107m".into(),
        Color::Indexed(i) => format!("\x1b[48;5;{i}m"),
        Color::Rgb(r, g, b) => format!("\x1b[48;2;{r};{g};{b}m"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Stylize;
    use ratatui::text::Span;

    fn line(text: &str) -> Line<'static> {
        Line::from(text.to_owned())
    }

    #[test]
    fn native_publication_uses_free_rows_before_scrolling() {
        let mut screen = Screen::with_live_height(30, 2, 12, 1);
        let mut terminal = vt100::Parser::new(12, 30, 32);
        terminal.process(b"banner\r\n\r\n");
        let mut out = Vec::new();
        screen
            .commit_text_lines(&mut out, &["first".into(), "second".into()])
            .unwrap();
        terminal.process(&out);
        out.clear();
        screen
            .draw(
                &mut out,
                &Frame {
                    live: &[line("prompt")],
                    cursor: Some((0, 0)),
                },
            )
            .unwrap();
        terminal.process(&out);
        let rows = terminal
            .screen()
            .rows(0, 30)
            .take(5)
            .map(|row| row.trim_end().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(rows, ["banner", "", "first", "second", "prompt"]);
    }

    #[test]
    fn passive_resize_does_not_publish_blank_history() {
        let mut screen = Screen::with_live_height(30, 0, 20, 12);
        let mut terminal = vt100::Parser::new(20, 30, 32);
        let live = [line("PROVISIONAL"), line("prompt")];
        let mut out = Vec::new();
        screen
            .draw(
                &mut out,
                &Frame {
                    live: &live,
                    cursor: Some((1, 0)),
                },
            )
            .unwrap();
        terminal.process(&out);
        screen.resize(30, 7);
        terminal.screen_mut().set_size(7, 30);
        out.clear();
        screen
            .draw(
                &mut out,
                &Frame {
                    live: &live,
                    cursor: Some((1, 0)),
                },
            )
            .unwrap();
        assert!(
            !out.windows(2).any(|bytes| bytes == b"\r\n"),
            "resize must not emit history advancement"
        );
        terminal.process(&out);
        assert!(terminal.screen().contents().contains("PROVISIONAL"));
    }

    #[test]
    fn removing_the_inline_cursor_hides_it() {
        let mut screen = Screen::new(20, 0, 4);
        let mut terminal = vt100::Parser::new(4, 20, 0);
        let mut out = Vec::new();
        for cursor in [Some((0, 0)), None] {
            out.clear();
            screen
                .draw(
                    &mut out,
                    &Frame {
                        live: &[line("prompt")],
                        cursor,
                    },
                )
                .unwrap();
            terminal.process(&out);
        }
        assert!(terminal.screen().hide_cursor());
    }

    #[test]
    fn growing_live_band_scrolls_only_after_clearing_mutable_rows() {
        let mut screen = Screen::with_live_height(80, 22, 24, 1);
        let mut terminal = vt100::Parser::new(24, 80, 32);
        terminal.process(b"\x1b[22;1HPRIOR_HISTORY");
        let mut output = Vec::new();
        screen
            .draw(
                &mut output,
                &Frame {
                    live: &[line("MUTABLE")],
                    cursor: None,
                },
            )
            .unwrap();
        terminal.process(&output);
        output.clear();
        screen.ensure_live_height(&mut output, 4).unwrap();
        terminal.process(&output);
        assert_eq!(screen.origin, 20);
        assert_eq!(screen.live_height(), 4);
        assert!(!terminal.screen().contents().contains("MUTABLE"));
        assert_eq!(
            terminal.screen().rows(0, 80).nth(19).unwrap(),
            "PRIOR_HISTORY"
        );

        output.clear();
        screen.ensure_live_height(&mut output, 2).unwrap();
        assert!(output.is_empty(), "the live band never shrinks during chat");
        assert_eq!(screen.origin, 20);
        assert_eq!(screen.live_height(), 4);
    }

    #[test]
    fn virtual_surface_owns_rows_without_physical_terminal_state() {
        let mut surface = Surface::new(8, 2);
        surface.render_line(line("hello"), 0);
        surface.render_line(line("world"), 1);

        assert_eq!(surface.size(), (8, 2));
        assert_eq!(surface.row_text(0), "hello   ");
        assert_eq!(surface.row_text(1), "world   ");
        assert_eq!(surface.row_text(2), "");

        surface.resize(4, 1);
        assert_eq!(surface.size(), (4, 1));
        assert_eq!(surface.row_text(0), "hell");
        assert_eq!(surface.row_text(1), "");
    }

    type Spec<'a> = (&'a [Line<'static>], Option<(usize, u16)>);

    fn render(frames: Vec<Spec>) -> Vec<u8> {
        let mut out = Vec::new();
        let mut screen = Screen::new(40, 0, 6);
        for (live, cursor) in frames {
            screen.draw(&mut out, &Frame { live, cursor }).unwrap();
        }
        out
    }

    fn all_rows(terminal: &mut vt100::Parser, width: u16) -> Vec<String> {
        terminal.screen_mut().set_scrollback(usize::MAX);
        let depth = terminal.screen().scrollback();
        let mut rows = Vec::new();
        for offset in (1..=depth).rev() {
            terminal.screen_mut().set_scrollback(offset);
            rows.push(terminal.screen().rows(0, width).next().unwrap());
        }
        terminal.screen_mut().set_scrollback(0);
        rows.extend(terminal.screen().rows(0, width));
        rows
    }

    #[test]
    fn first_draw_paints_all_rows_and_positions_cursor() {
        let live = [line("hello"), line("world"), line("status"), line("prompt")];
        let out = render(vec![(&live, Some((1, 3)))]);
        let mut terminal = vt100::Parser::new(6, 40, 0);
        terminal.process(&out);
        assert_eq!(
            terminal
                .screen()
                .rows(0, 40)
                .take(4)
                .map(|row| row.trim_end().to_owned())
                .collect::<Vec<_>>(),
            ["hello", "world", "status", "prompt"]
        );
        assert_eq!(terminal.screen().cursor_position(), (1, 3));
    }

    #[test]
    fn unchanged_frame_writes_no_text_cells() {
        let live = [line("alpha"), line("beta")];
        let out = render(vec![(&live, None), (&live, None), (&live, None)]);
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.matches("alpha").count(), 1);
        assert_eq!(text.matches("beta").count(), 1);
    }

    #[test]
    fn application_sequence_publishes_once_and_repaints_without_scrolling() {
        for origin in [0, 2] {
            let mut screen = Screen::with_live_height(30, origin, 12, 1);
            let mut terminal = vt100::Parser::new(12, 30, 64);
            let settled = (0..24)
                .map(|index| format!("settled-{index:02}"))
                .collect::<Vec<_>>();
            let mut out = Vec::new();
            let mut emitted = Vec::new();
            for batch in [&settled[..3], &settled[3..]] {
                out.clear();
                screen.commit_text_lines(&mut out, batch).unwrap();
                terminal.process(&out);
                emitted.extend_from_slice(&out);
            }
            for height in [4, 6] {
                out.clear();
                screen.ensure_live_height(&mut out, height).unwrap();
                terminal.process(&out);
                emitted.extend_from_slice(&out);
                for _ in 0..3 {
                    out.clear();
                    screen
                        .draw(
                            &mut out,
                            &Frame {
                                live: &[line("PROVISIONAL"), line("prompt")],
                                cursor: Some((1, 0)),
                            },
                        )
                        .unwrap();
                    assert!(!out.windows(2).any(|bytes| bytes == b"\r\n"));
                    terminal.process(&out);
                    emitted.extend_from_slice(&out);
                }
            }
            let rows = all_rows(&mut terminal, 30);
            let recorded = rows
                .iter()
                .filter(|row| row.starts_with("settled-"))
                .map(|row| row.trim_end())
                .collect::<Vec<_>>();
            assert_eq!(recorded, settled, "origin {origin}");
            terminal.screen_mut().set_scrollback(usize::MAX);
            let depth = terminal.screen().scrollback();
            for offset in 1..=depth {
                terminal.screen_mut().set_scrollback(offset);
                assert!(
                    !terminal
                        .screen()
                        .rows(0, 30)
                        .next()
                        .unwrap()
                        .contains("PROVISIONAL")
                );
            }
            terminal.screen_mut().set_scrollback(0);
            screen.set_live_height(1);
            out.clear();
            screen
                .draw(
                    &mut out,
                    &Frame {
                        live: &[line("ready")],
                        cursor: Some((0, 0)),
                    },
                )
                .unwrap();
            terminal.process(&out);
            assert!(!terminal.screen().contents().contains("PROVISIONAL"));
            for (height, width) in [(7, 20), (12, 30)] {
                screen.resize(width, height);
                terminal.screen_mut().set_size(height, width);
                out.clear();
                screen
                    .draw(
                        &mut out,
                        &Frame {
                            live: &[line("ready")],
                            cursor: Some((0, 0)),
                        },
                    )
                    .unwrap();
                assert!(!out.windows(2).any(|bytes| bytes == b"\r\n"));
                assert!(!String::from_utf8_lossy(&out).contains("settled-"));
                terminal.process(&out);
                assert!(terminal.screen().contents().contains("ready"));
            }
            let text = String::from_utf8(emitted).unwrap();
            for row in &settled {
                assert_eq!(text.matches(row).count(), 1);
            }
        }
    }

    #[test]
    fn stable_live_band_bottom_aligns_rows_and_cursor() {
        let mut screen = Screen::with_live_height(40, 3, 10, 4);
        let mut terminal = vt100::Parser::new(10, 40, 0);
        let mut out = Vec::new();
        screen
            .draw(
                &mut out,
                &Frame {
                    live: &[line("status"), line("prompt")],
                    cursor: Some((1, 2)),
                },
            )
            .unwrap();
        terminal.process(&out);
        assert_eq!(terminal.screen().cursor_position(), (6, 2));
        assert_eq!(
            terminal.screen().rows(0, 40).nth(5).unwrap().trim_end(),
            "status"
        );
    }

    #[test]
    fn reservation_shrink_blanks_freed_rows_without_scrolling() {
        let mut screen = Screen::with_live_height(20, 0, 6, 4);
        let mut terminal = vt100::Parser::new(6, 20, 0);
        let mut out = Vec::new();
        screen
            .draw(
                &mut out,
                &Frame {
                    live: &[line("one"), line("two"), line("three"), line("four")],
                    cursor: None,
                },
            )
            .unwrap();
        terminal.process(&out);
        screen.set_live_height(1);
        out.clear();
        screen
            .draw(
                &mut out,
                &Frame {
                    live: &[line("short")],
                    cursor: None,
                },
            )
            .unwrap();
        terminal.process(&out);
        assert!(!out.windows(2).any(|bytes| bytes == b"\r\n"));
        assert_eq!(
            terminal
                .screen()
                .rows(0, 20)
                .map(|row| row.trim_end().to_owned())
                .collect::<Vec<_>>(),
            ["short", "", "", "", "", ""]
        );
    }

    #[test]
    fn cursor_hidden_when_clipped_from_the_live_band() {
        let mut screen = Screen::with_live_height(20, 0, 6, 2);
        let mut out = Vec::new();
        screen
            .draw(
                &mut out,
                &Frame {
                    live: &[line("old"), line("new"), line("prompt")],
                    cursor: Some((0, 0)),
                },
            )
            .unwrap();
        assert!(!String::from_utf8(out).unwrap().contains("\x1b[?25h"));
    }

    #[test]
    fn finish_follows_short_rendered_region() {
        let mut out = Vec::new();
        let mut screen = Screen::new(40, 2, 10);
        screen
            .draw(
                &mut out,
                &Frame {
                    live: &[line("footer"), line("model")],
                    cursor: None,
                },
            )
            .unwrap();
        screen.finish(&mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.ends_with("\x1b[5;1H\x1b[?25h\x1b[0m\r\n"));
    }

    #[test]
    fn resize_repaints_every_row() {
        let mut out = Vec::new();
        let mut screen = Screen::new(40, 0, 6);
        for height in [6, 4] {
            screen.resize(40, height);
            screen
                .draw(
                    &mut out,
                    &Frame {
                        live: &[line("progress"), line("prompt")],
                        cursor: None,
                    },
                )
                .unwrap();
        }
        assert_eq!(
            String::from_utf8(out).unwrap().matches("progress").count(),
            2
        );
    }

    #[test]
    fn shorter_fullscreen_frame_clears_old_rows() {
        let mut screen = Screen::new(12, 0, 3);
        let mut terminal = vt100::Parser::new(3, 12, 0);
        let mut out = Vec::new();
        screen
            .draw_fullscreen(
                &mut out,
                &[line("first"), line("second"), line("third")],
                None,
            )
            .unwrap();
        terminal.process(&out);
        out.clear();
        screen
            .draw_fullscreen(&mut out, &[line("new")], None)
            .unwrap();
        terminal.process(&out);
        let rows = terminal
            .screen()
            .rows(0, 12)
            .map(|row| row.trim_end().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(rows, vec!["new", "", ""]);
    }

    #[test]
    fn fullscreen_resize_repaints_at_new_dimensions() {
        let mut screen = Screen::new(4, 0, 3);
        let mut out = Vec::new();
        screen
            .draw_fullscreen(&mut out, &[line("old")], None)
            .unwrap();
        screen.resize(8, 3);
        out.clear();
        screen
            .draw_fullscreen(&mut out, &[line("new")], None)
            .unwrap();
        let mut terminal = vt100::Parser::new(3, 8, 0);
        terminal.process(&out);
        assert_eq!(
            terminal.screen().rows(0, 8).next().unwrap().trim_end(),
            "new"
        );
    }

    #[test]
    fn invalidate_repaints_fullscreen() {
        let mut screen = Screen::new(8, 0, 3);
        let mut out = Vec::new();
        screen
            .draw_fullscreen(&mut out, &[line("hello")], None)
            .unwrap();
        out.clear();
        screen.invalidate();
        screen
            .draw_fullscreen(&mut out, &[line("hello")], None)
            .unwrap();
        assert!(String::from_utf8(out).unwrap().contains("hello"));
    }

    #[test]
    fn wide_char_edit_rewrites_the_full_row() {
        let out = render(vec![(&[line("abx")], None), (&[line("界x")], None)]);
        let mut terminal = vt100::Parser::new(6, 40, 0);
        terminal.process(&out);
        assert_eq!(
            terminal.screen().rows(0, 40).next().unwrap().trim_end(),
            "界x"
        );
    }

    #[test]
    fn styled_line_emits_sgr_once_per_run() {
        let styled = Line::from(vec![Span::from("dim ").dim(), Span::from("bright")]);
        let out = render(vec![(&[styled], None)]);
        let s = String::from_utf8(out).expect("utf8");
        assert!(s.contains("\x1b[2m"));
        assert!(s.contains("\x1b[0m"));
    }
}
