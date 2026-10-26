//! Line-diff renderer for Ion's mutable terminal surface.
//!
//! Normal inline chat owns only a live band. Settled transcript rows can be
//! appended once with `commit_lines`, after which the physical terminal
//! owns their scrollback/reflow. Drawing never publishes or scrolls live rows.
//! Fullscreen rendering is a separate transient surface.

use std::io::{self, Write};

use std::time::{Duration, Instant};

use ratatui::buffer::{Buffer, CellWidth};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget};

/// Wrapped live rows, bottom-aligned and clipped to the reserved inline band.
/// Settled rows must be published separately through `commit_lines`.
pub struct Frame<'a> {
    pub live: &'a [Line<'a>],
    /// (live row, column); None hides the hardware cursor.
    pub cursor: Option<(usize, u16)>,
}

/// A virtual terminal surface used by the renderer before bytes are emitted.
/// It is deliberately small: the surface owns cells and dimensions, while
/// `Screen` owns physical cursor/scrollback policy.
#[derive(Clone)]
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

pub struct Screen {
    width: u16,
    /// Physical row where our region starts (the launch cursor). The
    /// region extends to the screen bottom; completed content above it
    /// is native terminal scrollback.
    origin: u16,
    screen_height: u16,
    current: Option<Surface>,
    /// Invalidating cell comparisons must not discard primary ownership evidence.
    repaint: bool,
    resize_pending: Option<Instant>,
    /// Last primary cursor, including when hidden. Alternate output never changes it.
    physical_cursor: (u16, u16),
    fullscreen_size: (u16, u16),
    /// Last emitted hardware-cursor state; avoids redundant hide/show
    /// sequences between frames (perceived flicker while typing).
    cursor_shown: Option<bool>,
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
    pub(crate) fn staged(&self) -> Self {
        Self {
            width: self.width,
            origin: self.origin,
            screen_height: self.screen_height,
            current: self.current.clone(),
            repaint: self.repaint,
            resize_pending: self.resize_pending,
            physical_cursor: self.physical_cursor,
            fullscreen_size: self.fullscreen_size,
            cursor_shown: self.cursor_shown,
            cursor_at: self.cursor_at,
            live_height: self.live_height,
            fullscreen: self.fullscreen.clone(),
        }
    }

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
            repaint: true,
            resize_pending: None,
            physical_cursor: (origin, 0),
            fullscreen_size: (width.max(1), screen_height),
            cursor_shown: None,
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
        if available < rows {
            self.current = None;
            self.physical_cursor = (self.screen_height - 1, 0);
        }
        self.repaint = true;
        self.fullscreen = None;
        self.cursor_shown = None;
        self.cursor_at = None;
        Ok(())
    }

    /// Visible rows of the region: origin to screen bottom. Follows
    /// terminal growth and shrinkage.
    fn avail(&self) -> u16 {
        self.screen_height.saturating_sub(self.origin).max(1)
    }

    #[must_use]
    pub fn primary_size(&self) -> (u16, u16) {
        (self.width, self.screen_height)
    }

    /// A notification also matters when several native resizes return to the
    /// old geometry before intake. Keep custody until a primary CPR reconciles.
    pub fn note_resize(&mut self) {
        // Kernel geometry and native reflow are not atomic: tmux throttles
        // queued PTY sizes by 250 ms. Wait for that settling phase before
        // destructive reconciliation, even when cursor columns happen to agree.
        // Input/Core remain pollable; this is not a wait inside a cursor query.
        self.resize_pending = Some(Instant::now() + Duration::from_millis(300));
    }

    pub fn reconciliation_pending(&self) -> bool {
        self.resize_pending.is_some()
    }

    pub(crate) fn reconciliation_ready(&mut self) -> bool {
        let after = self
            .resize_pending
            .get_or_insert_with(|| Instant::now() + Duration::from_millis(300));
        Instant::now() >= *after
    }

    pub(crate) fn needs_reconciliation(&self, size: (u16, u16)) -> bool {
        self.reconciliation_pending() || size != self.primary_size()
    }

    /// Physical geometry used for fullscreen layout, independent of the primary
    /// inline footprint. Intake can compare it with the current kernel size.
    pub fn fullscreen_size(&self) -> (u16, u16) {
        self.fullscreen_size
    }

    /// Alternate geometry is independent of the retained primary footprint.
    pub fn resize_fullscreen(&mut self, width: u16, height: u16) {
        let size = (width.max(1), height.max(1));
        if size != self.fullscreen_size {
            self.fullscreen_size = size;
            self.fullscreen = None;
        }
    }

    /// Reconcile only the previous mutable footprint against the terminal's
    /// reflowed active cursor. Each emitted primary row is a hard line (EL
    /// before painting), and its extent excludes unpainted default padding.
    /// No committed cells are cached or replayed here.
    ///
    /// Native reflow may already have placed some mutable cells in history.
    /// Only the still-visible owned footprint can be erased; this transition
    /// cannot undo the emulator's earlier scrolling.
    pub fn reconcile_inline(
        &mut self,
        out: &mut impl Write,
        width: u16,
        height: u16,
        cursor: (u16, u16),
    ) -> io::Result<bool> {
        if self
            .resize_pending
            .is_some_and(|after| Instant::now() < after)
        {
            return Ok(false);
        }
        let width = width.max(1);
        let height = height.max(1);
        if cursor.0 >= width || cursor.1 >= height {
            return Err(io::Error::other(
                "resize cursor is outside terminal geometry",
            ));
        }
        let offset = self
            .physical_cursor
            .0
            .checked_sub(self.origin)
            .ok_or_else(|| {
                io::Error::other("primary cursor is outside the owned inline footprint")
            })?;
        let cursor_row = match &self.current {
            Some(window) => {
                let buffer = &window.buffer;
                let display = reflow_cursor(buffer, offset, self.physical_cursor.1, width)?;
                // Reflow can preserve the cursor's old column as either a
                // display-cell offset or an index into the compact glyph cells
                // of a wrapped row (observed in tmux). These are coordinate
                // interpretations, not different presentation width policies.
                let indexed = if width == buffer.area.width {
                    display
                } else {
                    reflow_indexed_cursor(buffer, offset, self.physical_cursor.1, width)?
                };
                match (display.1 == cursor.0, indexed.1 == cursor.0) {
                    (true, true) if display.0 == indexed.0 => display.0,
                    (true, false) => display.0,
                    (false, true) => indexed.0,
                    _ => {
                        return Err(io::Error::other(format!(
                            "terminal reflow does not establish a unique inline cursor row: cursor={cursor:?}, display={display:?}, indexed={indexed:?}, prior={:?} at {:?}, size={width}x{height}",
                            self.physical_cursor,
                            self.primary_size(),
                        )));
                    }
                }
            }
            // A cleared/discarded surface has no mutable cells left to reclaim.
            // Re-anchor at its measured blank cursor, not a former reservation.
            None if cursor.0 == 0 => 0,
            None => {
                return Err(io::Error::other(
                    "resize has no retained inline cursor evidence",
                ));
            }
        };
        // If the established boundary is above the viewport, every visible
        // row up to the cursor belongs to the old mutable region. Its invisible
        // part stays terminal-owned; do not clear or reconstruct native history.
        let origin = usize::from(cursor.1).saturating_sub(cursor_row) as u16;
        // There can be no committed content below the primary live anchor.
        // Erase rows, never ED: even ED at row one can archive provisional cells.
        write!(out, "\x1b[0m")?;
        for row in origin..height {
            write!(out, "\x1b[{};1H\x1b[2K", row + 1)?;
        }
        out.flush()?;
        self.width = width;
        self.screen_height = height;
        self.origin = origin;
        self.resize_pending = None;
        self.live_height = self.live_height.map(|rows| rows.min(self.avail() as usize));
        self.current = None;
        // Subsequent publication/growth/paint is synchronous; no query uses
        // this cleared intermediate surface as reflow evidence.
        self.physical_cursor = (height - 1, 0);
        self.invalidate();
        Ok(true)
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
        self.repaint = true;
        self.fullscreen = None;
        self.cursor_shown = None;
        self.cursor_at = None;
    }

    /// Append settled styled rows exactly once above the mutable live band.
    ///
    /// The current live surface is discarded, the rows are printed from the
    /// band's anchor with explicit CRLFs, and ordinary terminal scrolling moves
    /// older content into native scrollback. The anchor advances to the cursor's
    /// resulting physical row. Already committed rows are never re-rendered on
    /// resize; a subsequent `draw` repaints only the live band.
    pub fn commit_lines(&mut self, out: &mut impl Write, lines: &[Line<'_>]) -> io::Result<()> {
        if lines.is_empty() {
            return Ok(());
        }
        self.clear_inline_surface(out)?;
        let mut origin = self.origin;
        for line in lines {
            write!(out, "\x1b[{};1H\x1b[2K", origin + 1)?;
            for span in &line.spans {
                emit_style(out, line.style.patch(span.style))?;
                write!(out, "{}\x1b[0m", span.content)?;
            }
            write!(out, "\x1b[0m\r\n")?;
            origin = origin.saturating_add(1).min(self.screen_height - 1);
        }
        out.flush()?;
        self.origin = origin;
        self.current = None;
        self.physical_cursor = (origin, 0);
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
        let previous = self.current.as_ref();
        let height = self.avail();
        let band = self
            .live_height
            .unwrap_or_else(|| frame.live.len().max(1))
            .min(height as usize);
        self.live_height = Some(band);
        let dropped = frame.live.len().saturating_sub(band);
        let padding = band.saturating_sub(frame.live.len());
        let cursor = frame
            .cursor
            .filter(|(row, col)| *row < frame.live.len() && *col < self.width)
            .and_then(|(row, col)| {
                let row = row.checked_sub(dropped)? + padding;
                (row < band).then_some((self.origin + row as u16, col))
            });
        let mut next = Surface::new(self.width, band as u16);
        for (index, line) in frame.live.iter().skip(dropped).enumerate() {
            next.render_line(line.clone(), (padding + index) as u16);
        }
        let mut painted = false;
        for row in 0..band as u16 {
            let cursor_column = cursor.filter(|at| at.0 == self.origin + row).map(|at| at.1);
            // Include the cell under the cursor in the emitted extent. CPR
            // then denotes a real cell, not emulator-specific trailing-blank
            // or pending-wrap cursor policy.
            if self.repaint
                || previous.is_none_or(|prev| {
                    row >= prev.buffer.area.height
                        || row_differs(&next.buffer, row, &prev.buffer, row)
                })
                || cursor_column.is_some_and(|col| self.cursor_at != Some((self.origin + row, col)))
                || self
                    .cursor_at
                    .is_some_and(|at| at.0 == self.origin + row && cursor != Some(at))
            {
                emit_buffer_row(out, &next.buffer, row, self.origin, cursor_column)?;
                painted = true;
            }
        }
        if let Some(previous) = previous {
            for row in band as u16..previous.buffer.area.height {
                write!(out, "\x1b[0m\x1b[{};1H\x1b[2K", self.origin + row + 1)?;
                painted = true;
            }
        }
        if painted {
            write!(out, "\x1b[0m")?;
        }
        if let Some(at) = cursor {
            if painted || self.cursor_shown != Some(true) || self.cursor_at != Some(at) {
                write!(out, "\x1b[{};{}H", at.0 + 1, at.1 + 1)?;
                if self.cursor_shown != Some(true) {
                    write!(out, "\x1b[?25h")?;
                }
            }
        } else {
            write!(out, "\x1b[{};1H", self.origin + band as u16)?;
            if self.cursor_shown != Some(false) {
                write!(out, "\x1b[?25l")?;
            }
        }
        out.flush()?;
        self.repaint = false;
        self.cursor_shown = Some(cursor.is_some());
        self.cursor_at = cursor;
        self.physical_cursor = cursor.unwrap_or((self.origin + band as u16 - 1, 0));
        self.current = Some(next);
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
        let (w, height) = self.fullscreen_size;
        let h = height as usize;
        let mut next = Surface::new(w, height);
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
                emit_buffer_row(out, &next.buffer, r, 0, None)?;
                painted = true;
            }
        }
        // Hardware cursor: shown only while the docked composer owns
        // it. The cursor position is a viewport row (0-based, physical),
        // not a scrollback-absolute row.
        if let Some((row, col)) = cursor
            && row < h
        {
            write!(out, "\x1b[{};{}H\x1b[?25h", row + 1, col + 1)?;
        } else {
            write!(out, "\x1b[?25l")?;
        }
        if painted {
            write!(out, "\x1b[0m")?;
        }
        out.flush()?;
        self.fullscreen = Some(next);
        Ok(())
    }

    /// Yield the mutable primary band without publishing it into history.
    /// The caller must reconcile primary geometry and leave alternate screens
    /// first. The shell can then start at the released band’s anchor.
    pub fn release_inline(&mut self, out: &mut impl Write) -> io::Result<()> {
        self.clear_inline_surface(out)?;
        write!(out, "\x1b[{};1H\x1b[?25h", self.origin + 1)?;
        out.flush()?;
        self.current = None;
        self.physical_cursor = (self.origin, 0);
        self.invalidate();
        Ok(())
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

/// The extent actually painted, including styled blanks and wide continuations.
fn row_extent(buf: &Buffer, row: u16) -> u16 {
    let default = ratatui::buffer::Cell::default();
    let mut extent = 0;
    let mut column = 0;
    while column < buf.area.width {
        let cell = &buf[(column, row)];
        let end = (column + cell.cell_width().max(1)).min(buf.area.width);
        if !cells_equal(cell, &default) {
            extent = end;
        }
        column = end;
    }
    extent
}

/// Map the emitted hard rows, not App's new layout or full-width padding.
/// Only the prefix through the old active cursor is necessary to locate the
/// anchor: all cells below it also belong to the live surface, never history.
fn reflow_cursor(buf: &Buffer, row: u16, column: u16, width: u16) -> io::Result<(usize, u16)> {
    if row >= buf.area.height || column >= buf.area.width {
        return Err(io::Error::other(
            "cursor is outside the retained inline cells",
        ));
    }
    // Leading reservation blanks contain no emitted cells. They can move
    // into native history without losing mutable content, and need not be
    // reclaimed. The first painted row (or an empty cursor row) is the
    // physical ownership boundary used for erasure and the next reservation.
    let first = (0..=row)
        .find(|old_row| *old_row == row || row_extent(buf, *old_row) > 0)
        .expect("cursor row is in range");
    let mut mapped_row = 0usize;
    for old_row in first..=row {
        let extent = row_extent(buf, old_row).max(if old_row == row { column + 1 } else { 0 });
        let mut old_col = 0;
        let mut new_col = 0;
        while old_col < extent {
            let span = buf[(old_col, old_row)].cell_width().max(1);
            if span > width {
                return Err(io::Error::other(
                    "terminal width cannot retain an emitted wide glyph",
                ));
            }
            if span > width - new_col {
                mapped_row += 1;
                new_col = 0;
            }
            if old_row == row && column < old_col + span {
                return Ok((mapped_row, new_col + column - old_col));
            }
            old_col += span;
            new_col += span;
        }
        mapped_row += 1;
    }
    Err(io::Error::other(
        "retained inline cursor has no emitted cell",
    ))
}

// tmux reflow's cursor coordinate follows compact glyph-cell indexes rather
// than display columns. Keep that alternative separate from the display-width
// projection and accept it only when CPR establishes the same unique row.
fn reflow_indexed_cursor(
    buf: &Buffer,
    row: u16,
    column: u16,
    width: u16,
) -> io::Result<(usize, u16)> {
    let first = (0..=row)
        .find(|old_row| *old_row == row || row_extent(buf, *old_row) > 0)
        .expect("validated cursor row");
    let mut mapped_row = 0usize;
    for old_row in first..=row {
        let extent = row_extent(buf, old_row).max(if old_row == row { column + 1 } else { 0 });
        let mut old_col = 0;
        let mut new_col = 0;
        let mut cells = 0;
        let mut point = column;
        while old_col < extent {
            let span = buf[(old_col, old_row)].cell_width().max(1);
            if span > width {
                return Err(io::Error::other(
                    "terminal width cannot retain an emitted wide glyph",
                ));
            }
            if span > width - new_col {
                if old_row == row {
                    if point < cells {
                        return Ok((mapped_row, point));
                    }
                    point -= cells;
                }
                mapped_row += 1;
                new_col = 0;
                cells = 0;
            }
            old_col += span;
            new_col += span;
            cells += 1;
            if old_row == row && point < cells {
                return Ok((mapped_row, point));
            }
        }
        if old_row == row {
            return Ok((mapped_row, point));
        }
        mapped_row += 1;
    }
    Err(io::Error::other("validated cursor row was not reflowed"))
}

/// Rewrite one full row from the freshly rendered buffer. Row-level
/// granularity keeps wide characters consistent: both compared sides
/// come from complete renders, so continuation cells can never join
/// across a partial edit.
fn emit_buffer_row(
    out: &mut impl Write,
    buf: &Buffer,
    r: u16,
    origin: u16,
    cursor_column: Option<u16>,
) -> io::Result<()> {
    write!(out, "\x1b[0m\x1b[{};1H\x1b[2K", origin + r + 1)?;
    let w = row_extent(buf, r).max(cursor_column.map_or(0, |col| col + 1));
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
        // Internal and styled blanks are content; unpainted default padding
        // is erased by EL, not emitted as physical full-width text.
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
            .commit_lines(&mut out, &["first".into(), "second".into()])
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
    fn reconcile_resize_clears_the_old_reflowed_band_before_growth() {
        let mut screen = Screen::with_live_height(100, 26, 30, 2);
        let mut out = Vec::new();
        screen
            .draw(
                &mut out,
                &Frame {
                    live: &[
                        line("Shell finished with an error"),
                        line("› Preserve this draft: café 界 👩‍💻"),
                    ],
                    cursor: Some((1, 33)),
                },
            )
            .unwrap();
        // Independently supplied native-reflow outcome, not vt100::set_size
        // (which does not model tmux/Ghostty reflow).
        let mut terminal = vt100::Parser::new(7, 24, 32);
        terminal.process("HISTORY\x1b[3;1HShell finished with an e\x1b[4;1Hrror\x1b[5;1H› Preserve this draft: c\x1b[6;1Hafé 界 👩‍💻".as_bytes());
        out.clear();
        screen.reconcile_inline(&mut out, 24, 7, (9, 5)).unwrap();
        assert_eq!(screen.origin, 2);
        assert!(!out.windows(2).any(|bytes| bytes == b"\r\n"));
        terminal.process(&out);
        out.clear();
        screen.ensure_live_height(&mut out, 4).unwrap();
        screen
            .draw(
                &mut out,
                &Frame {
                    live: &[
                        line("Shell finished with an e"),
                        line("rror"),
                        line("› Preserve this draft: c"),
                        line("  afé 界 👩‍💻"),
                    ],
                    cursor: Some((3, 11)),
                },
            )
            .unwrap();
        terminal.process(&out);
        assert_eq!(terminal.screen().rows(0, 24).next().unwrap(), "HISTORY");
        assert_eq!(terminal.screen().contents().matches("Preserve").count(), 1);
        assert_eq!(terminal.screen().cursor_position(), (5, 11));
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
                .map(|index| Line::raw(format!("settled-{index:02}")))
                .collect::<Vec<_>>();
            let mut out = Vec::new();
            let mut emitted = Vec::new();
            for batch in [&settled[..3], &settled[3..]] {
                out.clear();
                screen.commit_lines(&mut out, batch).unwrap();
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
            assert_eq!(
                recorded,
                settled.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "origin {origin}"
            );
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
            let text = String::from_utf8(emitted).unwrap();
            for row in &settled {
                assert_eq!(text.matches(row.to_string().as_str()).count(), 1);
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
        assert!(!String::from_utf8_lossy(&out).contains("\x1b[?25h"));
        let mut terminal = vt100::Parser::new(6, 20, 0);
        terminal.process(&out);
        assert!(terminal.screen().hide_cursor());
    }

    #[test]
    fn releasing_inline_erases_only_unpublished_rows_without_scrolling() {
        let mut terminal = vt100::Parser::new(6, 20, 10);
        terminal.process(b"settled\r\n");
        let mut screen = Screen::with_live_height(20, 1, 6, 2);
        let mut out = Vec::new();
        screen
            .draw(
                &mut out,
                &Frame {
                    live: &[line("unsent draft"), line("live progress")],
                    cursor: Some((0, 3)),
                },
            )
            .unwrap();
        terminal.process(&out);
        out.clear();
        screen.release_inline(&mut out).unwrap();
        terminal.process(&out);
        assert_eq!(terminal.screen().contents(), "settled");
        assert_eq!(terminal.screen().cursor_position(), (1, 0));
        assert!(!terminal.screen().hide_cursor());
        assert!(!out.windows(2).any(|bytes| bytes == b"\r\n"));
        terminal.process(b"SHELL");
        assert_eq!(terminal.screen().contents(), "settled\nSHELL");
    }

    #[test]
    fn reflow_maps_actual_cells_and_hard_row_boundaries() {
        for (text, column, width, expected) in [
            ("123界ab", 6, 4, (1, 3)),
            ("123界ab", 7, 4, (2, 0)),
            ("a\u{301}b", 2, 2, (1, 0)),
            ("👩‍💻x", 2, 2, (1, 0)),
            ("long", 1, 8, (0, 1)),
        ] {
            let mut surface = Surface::new(20, 2);
            surface.render_line(line("12345"), 0);
            surface.render_line(line(text), 1);
            let prefix = 5usize.div_ceil(width as usize);
            assert_eq!(
                reflow_cursor(&surface.buffer, 1, column, width).unwrap(),
                (prefix + expected.0, expected.1),
                "{text}"
            );
        }
    }

    #[test]
    fn resize_does_not_reclaim_empty_reservation_rows_from_history() {
        let mut screen = Screen::with_live_height(100, 3, 30, 12);
        let mut out = Vec::new();
        screen
            .draw(
                &mut out,
                &Frame {
                    live: &[line("draft")],
                    cursor: Some((0, 5)),
                },
            )
            .unwrap();
        out.clear();
        screen.reconcile_inline(&mut out, 24, 7, (5, 3)).unwrap();
        assert_eq!(screen.origin, 3);
        assert!(!String::from_utf8_lossy(&out).contains("\x1b[1;1H"));
        assert!(!out.windows(2).any(|bytes| bytes == b"\r\n"));
    }

    #[test]
    fn uncertain_reflow_never_erases_guessed_rows_or_discards_evidence() {
        let mut screen = Screen::with_live_height(100, 26, 30, 2);
        let mut out = Vec::new();
        screen
            .draw(
                &mut out,
                &Frame {
                    live: &[line("1234567890123456789012345678"), line("draft")],
                    cursor: Some((1, 5)),
                },
            )
            .unwrap();
        screen.note_resize();
        screen.resize_fullscreen(24, 7);
        screen
            .draw_fullscreen(&mut out, &[line("modal")], Some((0, 3)))
            .unwrap();
        screen.invalidate(); // Modal return retains primary cells and cursor.
        out.clear();
        assert!(!screen.reconcile_inline(&mut out, 24, 7, (4, 5)).unwrap());
        // Matching columns cannot prove row ownership during kernel/grid lag.
        assert!(!screen.reconcile_inline(&mut out, 24, 7, (5, 5)).unwrap());
        assert!(out.is_empty());
        assert!(!screen.reconcile_inline(&mut out, 24, 7, (24, 5)).unwrap());
        assert!(out.is_empty());
        screen.resize_pending = Some(Instant::now() - Duration::from_millis(300));
        for cursor in [(24, 5), (5, 7)] {
            out.clear();
            assert!(screen.reconcile_inline(&mut out, 24, 7, cursor).is_err());
            assert!(out.is_empty());
            assert_eq!(screen.primary_size(), (100, 30));
            assert_eq!(screen.origin, 26);
            assert!(screen.needs_reconciliation((100, 30)));
            assert!(screen.current.is_some());
        }
        // Incoherence cannot defer forever after the native settling phase.
        screen.resize_pending = Some(Instant::now() - Duration::from_millis(300));
        out.clear();
        assert!(screen.reconcile_inline(&mut out, 24, 7, (4, 5)).is_err());
        assert!(out.is_empty());
        // A reply from the restored primary, not the modal cursor, locates
        // the footprint using the retained 100-column rows.
        screen.reconcile_inline(&mut out, 24, 7, (5, 5)).unwrap();
        assert_eq!(screen.origin, 3);
        assert!(!screen.needs_reconciliation((24, 7)));
    }

    #[test]
    fn cleared_footprint_survives_rejected_staged_paint() {
        let mut screen = Screen::with_live_height(100, 26, 30, 2);
        let mut out = Vec::new();
        screen
            .draw(
                &mut out,
                &Frame {
                    live: &[line("1234567890123456789012345678"), line("draft")],
                    cursor: Some((1, 5)),
                },
            )
            .unwrap();
        assert!(screen.reconcile_inline(&mut out, 24, 7, (5, 5)).unwrap());
        // No staged paint was published before geometry changed again.
        out.clear();
        assert!(screen.reconcile_inline(&mut out, 100, 30, (0, 6)).unwrap());
        assert_eq!(screen.origin, 6);
        assert!(!String::from_utf8(out).unwrap().contains("\x1b[6;1H"));
    }

    #[test]
    fn cursor_mapping_does_not_read_later_unrepresentable_glyphs() {
        let mut surface = Surface::new(10, 1);
        surface.render_line(line("x界"), 0);
        assert_eq!(reflow_cursor(&surface.buffer, 0, 0, 1).unwrap(), (0, 0));
        assert_eq!(
            reflow_indexed_cursor(&surface.buffer, 0, 0, 1).unwrap(),
            (0, 0)
        );
    }

    #[test]
    fn ambiguous_cursor_rows_never_clear_committed_content() {
        let mut screen = Screen::with_live_height(10, 3, 10, 1);
        let mut out = Vec::new();
        screen
            .draw(
                &mut out,
                &Frame {
                    live: &[line("界界x")],
                    cursor: Some((0, 2)),
                },
            )
            .unwrap();
        out.clear();
        assert!(screen.reconcile_inline(&mut out, 2, 5, (0, 3)).is_err());
        assert!(out.is_empty());
        assert_eq!(screen.origin, 3);
        assert_eq!(screen.primary_size(), (10, 10));
    }

    #[test]
    fn unicode_cursor_coordinates_preserve_the_committed_prefix() {
        for cursor in [(1, 6), (2, 6)] {
            let mut screen = Screen::with_live_height(100, 26, 30, 2);
            let mut out = Vec::new();
            screen
                .draw(
                    &mut out,
                    &Frame {
                        live: &[
                            line("Shell finished with an error"),
                            line("› UNSENT_RESIZE café 界 X"),
                        ],
                        cursor: Some((1, 25)),
                    },
                )
                .unwrap();
            out.clear();
            // Display columns and the observed tmux compact-cell coordinate
            // both establish row three as the first owned row in this layout.
            screen.reconcile_inline(&mut out, 24, 7, cursor).unwrap();
            assert_eq!(screen.origin, 3);
            let bytes = String::from_utf8(out).unwrap();
            assert!(!bytes.contains("\x1b[3;1H"));
            assert!(bytes.contains("\x1b[4;1H\x1b[2K"));
            assert!(!bytes.contains("\r\n"));
        }
    }

    #[test]
    fn native_clipping_reclaims_only_the_visible_mutable_suffix() {
        let mut screen = Screen::with_live_height(100, 26, 30, 2);
        let mut out = Vec::new();
        screen
            .draw(
                &mut out,
                &Frame {
                    live: &[line("1234567890123456789012345678"), line("draft")],
                    cursor: Some((1, 5)),
                },
            )
            .unwrap();
        out.clear();
        screen.reconcile_inline(&mut out, 24, 2, (5, 1)).unwrap();
        assert_eq!(screen.origin, 0);
        let bytes = String::from_utf8(out).unwrap();
        assert!(bytes.contains("\x1b[1;1H\x1b[2K"));
        assert!(bytes.contains("\x1b[2;1H\x1b[2K"));
        assert!(!bytes.contains("\x1b[3J"));
        assert!(!bytes.contains("\r\n"));
    }

    #[test]
    fn inline_output_trims_padding_but_preserves_styled_blanks_and_cursor_cell() {
        let mut screen = Screen::with_live_height(100, 3, 30, 2);
        let mut out = Vec::new();
        let styled = Line::from(vec![
            Span::raw("x"),
            Span::styled("  ", Style::default().bg(Color::Blue)),
        ]);
        screen
            .draw(
                &mut out,
                &Frame {
                    live: &[styled, line("draft")],
                    cursor: Some((1, 8)),
                },
            )
            .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("\x1b[44m  "));
        assert!(!text.contains("          "));
        assert!(
            !text.contains("\x1b[6;1H"),
            "unreserved rows are not emitted"
        );
        assert!(!text.contains("\x1b[2J"));
        assert_eq!(
            reflow_cursor(&screen.current.as_ref().unwrap().buffer, 1, 8, 4).unwrap(),
            (3, 0)
        );
        // Moving a cursor backwards must remove its old emitted blank extent.
        out = Vec::new();
        screen
            .draw(
                &mut out,
                &Frame {
                    live: &[line("x"), line("draft")],
                    cursor: Some((1, 2)),
                },
            )
            .unwrap();
        assert!(String::from_utf8(out).unwrap().contains("draft"));
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
        screen.resize_fullscreen(8, 3);
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
    fn native_publication_preserves_the_live_styles_without_leaking_them() {
        let styled = Line::from(vec![
            Span::styled("action", Style::default().fg(Color::Yellow)),
            Span::raw(" body"),
        ])
        .style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(ratatui::style::Modifier::BOLD),
        );
        let mut live_screen = Screen::new(30, 0, 4);
        let mut native_screen = Screen::new(30, 0, 4);
        let mut live = vt100::Parser::new(4, 30, 0);
        let mut native = vt100::Parser::new(4, 30, 0);
        let mut out = Vec::new();
        live_screen
            .draw(
                &mut out,
                &Frame {
                    live: std::slice::from_ref(&styled),
                    cursor: None,
                },
            )
            .unwrap();
        live.process(&out);
        out.clear();
        native_screen
            .commit_lines(&mut out, &[styled, Line::raw("plain")])
            .unwrap();
        native.process(&out);
        for column in 0..11 {
            let a = live.screen().cell(0, column).unwrap();
            let b = native.screen().cell(0, column).unwrap();
            assert_eq!(a.contents(), b.contents());
            assert_eq!(a.fgcolor(), b.fgcolor());
            assert_eq!(a.bold(), b.bold());
        }
        let plain = native.screen().cell(1, 0).unwrap();
        assert!(!plain.bold());
        assert_eq!(plain.fgcolor(), vt100::Color::Default);
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
