//! Line-diff screen renderer (TERMINAL.md): committed scrollback and
//! the live composer/footer region form one growing line array; each
//! frame diffs against the previous visible window and rewrites only
//! changed rows. Physical scrolling happens only when committed
//! history advances: the live region is a fixed-height band rebuilt
//! every frame, so reversible edits can never leak into terminal
//! scrollback. The window occupies the bottom `height` rows of the
//! physical screen; the host reserves those rows before the first draw
//! (`reserve_rows`).

use std::io::{self, Write};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget};

/// A rendered frame. Rows index the WRAPPED arrays the caller builds;
/// `committed` grows monotonically, `live` is a fixed-height band.
pub struct Frame<'a> {
    pub committed: &'a [Line<'a>],
    pub live: &'a [Line<'a>],
    /// (absolute wrapped row, column); None hides the hardware cursor.
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
    offset: usize,
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
        }
    }

    pub fn size(&self) -> (u16, u16) {
        (self.width, self.avail())
    }

    /// Visible rows of the region: origin to screen bottom. Follows
    /// terminal growth and shrinkage.
    fn avail(&self) -> u16 {
        self.screen_height.saturating_sub(self.origin).max(1)
    }

    /// A size change invalidates the window; the next draw repaints
    /// every row from the fresh buffer. If the terminal shrank below
    /// the origin, the origin re-anchors near the new bottom.
    pub fn resize(&mut self, width: u16, height: u16) {
        let width = width.max(1);
        let height = height.max(1);
        if width == self.width && height == self.screen_height {
            return;
        }
        self.width = width;
        self.screen_height = height;
        if self.origin + 2 > height {
            self.origin = height.saturating_sub(2);
        }
        self.current = None;
        self.cursor_shown = false;
        self.cursor_at = None;
    }

    /// Force a full repaint on the next draw without changing size.
    /// Used after suspend/resume: the host terminal's visible surface
    /// is no longer what this Screen believes it is.
    pub fn invalidate(&mut self) {
        self.current = None;
        self.cursor_shown = false;
        self.cursor_at = None;
    }

    /// Render one frame. Lines must already be wrapped to `width`;
    /// each occupies exactly one row (overlong spans truncate).
    pub fn draw(&mut self, out: &mut impl Write, frame: &Frame) -> io::Result<()> {
        let previous = self.current.take();
        let previous_offset = previous.as_ref().map_or(0, |window| window.offset);
        let mut h = self.avail() as usize;
        let w = self.width;
        let committed_rows = frame.committed.len();
        let total = committed_rows + frame.live.len();
        let mut offset = total.saturating_sub(h);

        // A committed/live line can consume the blank rows above a
        // nonzero launch origin before the physical terminal needs to
        // scroll. Once the origin moves, the window height changes, so
        // rebuild the frame and compare it as a fresh surface. Without
        // this adjustment, a growing transcript keeps painting at the
        // old origin after the terminal has already scrolled that origin
        // upward, eventually addressing rows below the terminal.
        let origin_before = self.origin;
        if offset > previous_offset && self.origin > 0 {
            let shift = offset
                .saturating_sub(previous_offset)
                .min(u16::MAX as usize) as u16;
            self.origin = self.origin.saturating_sub(shift);
            if self.origin != origin_before {
                h = self.avail() as usize;
                offset = total.saturating_sub(h);
            }
        }

        let mut next = Surface::new(w, self.avail());
        for r in 0..h {
            let absolute = r + offset;
            let line = if absolute < committed_rows {
                frame.committed.get(absolute)
            } else {
                frame.live.get(absolute - committed_rows)
            };
            if let Some(line) = line {
                next.render_line(line.clone(), r as u16);
            }
        }

        // Physical scroll tracks committed advancement only. With the
        // live band at fixed height, offset can rise only when
        // committed history grows, so scrolled rows are permanently
        // finished content and the shift maps old window row r + k to
        // new window row r.
        let scrolled = offset.saturating_sub(previous_offset);
        if scrolled > 0 {
            write!(out, "\x1b[{};1H", self.screen_height)?;
            for _ in 0..scrolled {
                out.write_all(b"\r\n")?;
            }
        }

        let mut painted = false;
        for r in 0..h as u16 {
            let src_row = r as usize + scrolled;
            let comparable = previous.as_ref().is_some_and(|prev| {
                self.origin == origin_before && src_row < prev.surface.buffer.area.height as usize
            });
            if !comparable
                || row_differs(
                    &next.buffer,
                    r,
                    &previous.as_ref().expect("checked").surface.buffer,
                    src_row as u16,
                )
            {
                emit_buffer_row(out, &next.buffer, r, self.origin)?;
                painted = true;
            }
        }
        if painted {
            write!(out, "\x1b[0m")?;
        }

        // Touch the cursor only when something was painted or it moved:
        // per-frame hide/show is perceived flicker while typing.
        let mut cursor_shown = self.cursor_shown;
        let mut cursor_at_out = self.cursor_at;
        if let Some((row, col)) = frame.cursor {
            // Absolute wrapped row -> visible row inside the window.
            if row >= offset {
                let screen_row = row - offset;
                if screen_row < h {
                    let at = (self.origin + screen_row as u16, col);
                    if painted || !self.cursor_shown || self.cursor_at != Some(at) {
                        write!(out, "\x1b[{};{}H", at.0 + 1, at.1 + 1)?;
                        if !self.cursor_shown {
                            write!(out, "\x1b[?25h")?;
                        }
                        cursor_shown = true;
                        cursor_at_out = Some(at);
                    }
                }
            }
        }
        out.flush()?;

        self.current = Some(Window {
            surface: next,
            offset,
        });
        self.cursor_shown = cursor_shown;
        self.cursor_at = cursor_at_out;
        Ok(())
    }

    /// Park below the rendered window on shutdown so the shell prompt
    /// lands on a fresh line.
    pub fn finish(&mut self, out: &mut impl Write) -> io::Result<()> {
        let row = self.screen_height;
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
            x += 1;
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

    type Spec<'a> = (
        &'a [Line<'static>],
        &'a [Line<'static>],
        Option<(usize, u16)>,
    );

    fn render(frames: Vec<Spec>) -> Vec<u8> {
        let mut out = Vec::new();
        let mut screen = Screen::new(40, 0, 6);
        for (committed, live, cursor) in frames {
            screen
                .draw(
                    &mut out,
                    &Frame {
                        committed,
                        live,
                        cursor,
                    },
                )
                .expect("draw");
        }
        out
    }

    #[test]
    fn first_draw_paints_all_rows_and_positions_cursor() {
        let out = render(vec![(
            &[line("hello"), line("world")],
            &[line("status"), line("› ")],
            Some((1, 3)),
        )]);
        let s = String::from_utf8(out).expect("utf8");
        assert!(s.contains("hello"));
        assert!(s.contains("world"));
        // cursor: absolute row 1, offset 0 -> screen row 2
        assert!(s.contains("\x1b[2;4H"));
        assert!(s.contains("\x1b[?25h"));
    }

    #[test]
    fn unchanged_frame_writes_no_text_cells() {
        let committed = [line("alpha")];
        let live = [line("beta")];
        let out = render(vec![
            (&committed, &live, None),
            (&committed, &live, None),
            (&committed, &live, None),
        ]);
        let s = String::from_utf8(out).expect("utf8");
        assert_eq!(s.matches("alpha").count(), 1, "repaint must skip text: {s}");
        assert_eq!(s.matches("beta").count(), 1);
    }

    #[test]
    fn committed_growth_scrolls_without_rewriting_history() {
        // Start inside the window (5 total rows < 6), then append three
        // committed rows so offset grows 0 -> 3.
        let c2: Vec<Line> = (0..4).map(|i| line(&format!("row{i}"))).collect();
        let mut c3 = c2.clone();
        c3.extend((4..8).map(|i| line(&format!("row{i}"))));
        let live = [line("status")];
        let out = render(vec![(&c2, &live, None), (&c3, &live, None)]);
        let s = String::from_utf8(out).expect("utf8");
        // three physical scrolls for the four appended rows minus one
        // previously-free window row
        assert_eq!(s.matches("\r\n").count(), 3);
        // history never rewritten
        assert_eq!(s.matches("row0").count(), 1);
        assert_eq!(s.matches("row3").count(), 1);
        assert_eq!(s.matches("row6").count(), 1);
        assert!(s.contains("row7"));
    }

    #[test]
    fn stateful_terminal_tracks_committed_scroll_without_duplicates() {
        let c2: Vec<Line> = (0..4).map(|i| line(&format!("row{i}"))).collect();
        let mut c3 = c2.clone();
        c3.extend((4..8).map(|i| line(&format!("row{i}"))));
        let live = [line("status")];
        let mut screen = Screen::new(40, 0, 6);
        let mut terminal = vt100::Parser::new(6, 40, 16);

        for committed in [&c2, &c3] {
            let mut bytes = Vec::new();
            screen
                .draw(
                    &mut bytes,
                    &Frame {
                        committed,
                        live: &live,
                        cursor: None,
                    },
                )
                .expect("draw");
            terminal.process(&bytes);
        }

        let rows: Vec<String> = terminal
            .screen()
            .rows(0, 40)
            .map(|row| row.trim_end().to_owned())
            .collect();
        assert_eq!(
            rows,
            vec![
                "row3".to_owned(),
                "row4".to_owned(),
                "row5".to_owned(),
                "row6".to_owned(),
                "row7".to_owned(),
                "status".to_owned(),
            ]
        );
    }

    #[test]
    fn stateful_terminal_tracks_committed_scroll_from_a_nonzero_origin() {
        // Launching below existing shell output leaves only four rows in
        // the first inline region. Once committed history grows past that
        // region, the physical scroll moves the anchor upward; the screen
        // must still end with the newest six rows in the terminal window.
        let first: Vec<Line> = (0..3).map(|i| line(&format!("row{i}"))).collect();
        let second: Vec<Line> = (0..7).map(|i| line(&format!("row{i}"))).collect();
        let live = [line("status")];
        let mut screen = Screen::new(40, 2, 6);
        let mut terminal = vt100::Parser::new(6, 40, 16);

        for committed in [&first, &second] {
            let mut bytes = Vec::new();
            screen
                .draw(
                    &mut bytes,
                    &Frame {
                        committed,
                        live: &live,
                        cursor: None,
                    },
                )
                .expect("draw");
            terminal.process(&bytes);
        }

        let rows: Vec<String> = terminal
            .screen()
            .rows(0, 40)
            .map(|row| row.trim_end().to_owned())
            .collect();
        assert_eq!(
            rows,
            vec![
                "row2".to_owned(),
                "row3".to_owned(),
                "row4".to_owned(),
                "row5".to_owned(),
                "row6".to_owned(),
                "status".to_owned(),
            ]
        );
    }

    #[test]
    fn live_band_growth_never_scrolls_or_duplicates_history() {
        // The composer wrapping from one row to two must not push the
        // transcript into scrollback (Sol review P1.2).
        let committed = [line("transcript-line")];
        let short = [line("status"), line("› ")];
        let grown = [line("status"), line("› aaaaaa"), line("bbbbbb")];
        let shrunk_again = [line("status"), line("› aaa")];
        let out = render(vec![
            (&committed, &short, None),
            (&committed, &grown, None),
            (&committed, &shrunk_again, None),
        ]);
        let s = String::from_utf8(out).expect("utf8");
        assert!(
            !s.contains("\r\n"),
            "live-band changes must not physically scroll: {s}"
        );
        // "transcript-line" appears exactly once across all three frames
        assert_eq!(s.matches("transcript-line").count(), 1);
    }

    #[test]
    fn cursor_translates_through_the_offset() {
        // 8 absolute rows in a 6-row window: offset = 2. A cursor on
        // absolute row 6 must land on screen row 5 (Sol review P1.3).
        let committed: Vec<Line> = (0..7).map(|i| line(&format!("c{i}"))).collect();
        let live = [line("composer-row")];
        let out = render(vec![(&committed, &live, Some((6, 2)))]);
        let s = String::from_utf8(out).expect("utf8");
        assert!(s.contains("\x1b[5;3H"), "cursor at screen row 5 col 3: {s}");
        assert!(s.contains("\x1b[?25h"));
    }

    #[test]
    fn cursor_translation_includes_the_anchored_origin() {
        // The visible row is relative to the line-diff window, but the
        // terminal cursor position is absolute. A nonzero origin models
        // launching Ion below existing shell scrollback.
        let out = {
            let mut out = Vec::new();
            let mut screen = Screen::new(40, 3, 10);
            screen
                .draw(
                    &mut out,
                    &Frame {
                        committed: &[],
                        live: &[line("status"), line("› ")],
                        cursor: Some((1, 2)),
                    },
                )
                .expect("draw");
            out
        };
        let s = String::from_utf8(out).expect("utf8");
        assert!(
            s.contains("\x1b[5;3H"),
            "cursor must include the anchored origin: {s:?}"
        );
        assert!(
            !s.contains("\x1b[2;3H"),
            "cursor must not jump above the anchored window: {s:?}"
        );
    }

    #[test]
    fn cursor_hidden_when_outside_the_window() {
        let committed: Vec<Line> = (0..10).map(|i| line(&format!("c{i}"))).collect();
        let out = render(vec![(&committed, &[line("s")], Some((1, 0)))]);
        let s = String::from_utf8(out).expect("utf8");
        assert!(!s.contains("\x1b[?25h"), "cursor above window stays hidden");
    }

    #[test]
    fn resize_repaints_every_row() {
        let committed = [line("history")];
        let live_before = [line("status one")];
        let live_after = [line("status after resize")];
        let mut out = Vec::new();
        let mut screen = Screen::new(40, 0, 6);
        screen
            .draw(
                &mut out,
                &Frame {
                    committed: &committed,
                    live: &live_before,
                    cursor: None,
                },
            )
            .expect("draw");
        screen.resize(40, 4);
        screen
            .draw(
                &mut out,
                &Frame {
                    committed: &committed,
                    live: &live_after,
                    cursor: None,
                },
            )
            .expect("draw");
        let s = String::from_utf8(out).expect("utf8");
        // invalidation repaints even rows whose content did not change
        assert_eq!(s.matches("history").count(), 2, "{s}");
    }

    #[test]
    fn shrink_blanks_freed_rows() {
        let big = [
            line("t0"),
            line("t1"),
            line("t2"),
            line("t3"),
            line("t4"),
            line("t5"),
        ];
        let small = [line("t0"), line("t1")];
        let out = render(vec![
            (&big, &[line("s")], None),
            (&small, &[line("s")], None),
        ]);
        let s = String::from_utf8(out).expect("utf8");
        // freed rows are erased (spaces written), not left as ghosts
        assert!(!s.matches("t4").count() > 1 || s.contains("\x1b[5;1H"));
        let erase_row5 = s.matches("\x1b[5;1H").count();
        let erase_row6 = s.matches("\x1b[6;1H").count();
        assert!(erase_row5 >= 1 && erase_row6 >= 1, "{s}");
    }

    #[test]
    fn wide_char_edit_rewrites_the_full_row() {
        // Editing single-width text into a wide CJK char must rewrite
        // the whole row from a fresh render so continuation cells stay
        // consistent (Sol review P1.5).
        let out = render(vec![
            (&[], &[line("abx")], None),
            (&[], &[line("界x")], None),
        ]);
        let s = String::from_utf8(out).expect("utf8");
        assert!(s.contains('界'));
        // full-row repaint emits both characters in order in one run
        let pong = s.find('界').expect("wide char present");
        let x_pos = s[pong..].find('x').expect("trailing cell rewritten");
        assert!(x_pos < 6, "continuation cell must follow immediately");
    }

    #[test]
    fn styled_line_emits_sgr_once_per_run() {
        let styled = Line::from(vec![Span::from("dim ").dim(), Span::from("bright")]);
        let out = render(vec![(&[], &[styled], None)]);
        let s = String::from_utf8(out).expect("utf8");
        assert!(s.contains("\x1b[2m"));
        assert!(s.contains("\x1b[0m"));
    }
}
