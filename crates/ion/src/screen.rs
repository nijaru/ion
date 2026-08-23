//! Line-diff screen renderer (DESIGN.md §22): committed scrollback and
//! the live composer/footer region form one growing line array; each
//! frame diffs against the previous visible window and writes only the
//! changed cells, scrolling physically as content grows. Mirrors the
//! pi-tui model; replaces a reserved inline viewport.

use std::io::{self, Write};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget};

/// A rendered frame: the full line array (transcript + live chrome)
/// plus where the hardware cursor belongs. Rows index the WRAPPED
/// array the caller builds.
pub struct Frame<'a> {
    pub lines: &'a [Line<'a>],
    /// (row, column) within `lines`; None hides the hardware cursor.
    pub cursor: Option<(usize, u16)>,
}

/// What the physical screen shows in our window right now.
struct Window {
    buf: Buffer,
    offset: usize,
}

pub struct Screen {
    width: u16,
    height: u16,
    current: Option<Window>,
}

impl Screen {
    pub fn new(width: u16, height: u16) -> Self {
        let (width, height) = clamp_size(width, height);
        Self {
            width,
            height,
            current: None,
        }
    }

    pub fn size(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    /// A resize invalidates the window; the next draw repaints fully.
    pub fn resize(&mut self, width: u16, height: u16) {
        let (width, height) = clamp_size(width, height);
        if width == self.width && height == self.height {
            return;
        }
        self.width = width;
        self.height = height;
        self.current = None;
    }

    /// Render one frame. Lines must already be wrapped to `width`;
    /// each occupies exactly one row (overlong spans truncate).
    pub fn draw(&mut self, out: &mut impl Write, frame: &Frame) -> io::Result<()> {
        let h = self.height as usize;
        let w = self.width;
        let total = frame.lines.len();
        let offset = total.saturating_sub(h);

        let mut next = Buffer::empty(Rect::new(0, 0, w, self.height));
        for r in 0..h {
            if let Some(line) = frame.lines.get(r + offset) {
                Paragraph::new(line.clone()).render(Rect::new(0, r as u16, w, 1), &mut next);
            }
        }

        // Purely-appended growth: park the cursor on the bottom row and
        // feed newlines so the terminal moves every displayed row up by
        // k. Afterwards the previous window's row r + k holds what the
        // new frame expects at row r for stable history, so the diff
        // below skips untouched content.
        let scrolled = match &self.current {
            Some(prev) => offset.saturating_sub(prev.offset),
            None => 0,
        };
        if scrolled > 0 {
            write!(out, "\x1b[{};1H", self.height)?;
            for _ in 0..scrolled {
                out.write_all(b"\r\n")?;
            }
        }

        write!(out, "\x1b[?25l")?;
        for r in 0..h as u16 {
            let src_row = r as usize + scrolled;
            let have_row = match (&self.current, src_row < h) {
                (Some(_), true) => Some(src_row as u16),
                _ => None,
            };
            // Emit changed cells as maximal same-style horizontal runs:
            // one MoveTo + one SGR + contiguous text per run.
            let mut run_start: Option<u16> = None;
            let mut run_style = Style::default();
            let mut run_text = String::new();
            for x in 0..w {
                let want = next[(x, r)].clone();
                let have = have_row
                    .and_then(|sr| self.current.as_ref().map(|p| p.buf[(x, sr)].clone()))
                    .unwrap_or_default();
                if cells_equal(&have, &want) {
                    if let Some(start) = run_start.take() {
                        emit_run(out, r, start, run_style, &run_text)?;
                        run_text.clear();
                    }
                    continue;
                }
                match run_start {
                    Some(_) if want.style() == run_style => run_text.push_str(want.symbol()),
                    Some(start) => {
                        emit_run(out, r, start, run_style, &run_text)?;
                        run_start = Some(x);
                        run_style = want.style();
                        run_text.clear();
                        run_text.push_str(want.symbol());
                    }
                    None => {
                        run_start = Some(x);
                        run_style = want.style();
                        run_text.push_str(want.symbol());
                    }
                }
            }
            if let Some(start) = run_start {
                emit_run(out, r, start, run_style, &run_text)?;
            }
        }
        write!(out, "\x1b[0m")?;

        if let Some((row, col)) = frame.cursor {
            let screen_row = row + offset;
            if screen_row < total && screen_row < h {
                write!(out, "\x1b[{};{}H", screen_row + 1, col + 1)?;
                write!(out, "\x1b[?25h")?;
            }
        }
        out.flush()?;

        self.current = Some(Window { buf: next, offset });
        Ok(())
    }

    /// Park below the rendered content on shutdown so the shell prompt
    /// lands on a fresh line.
    pub fn finish(&mut self, out: &mut impl Write) -> io::Result<()> {
        let row = self.height;
        write!(out, "\x1b[{row};1H\x1b[?25h\r\n")
    }
}

fn clamp_size(width: u16, height: u16) -> (u16, u16) {
    (width.max(1), height.max(1))
}

fn cells_equal(a: &ratatui::buffer::Cell, b: &ratatui::buffer::Cell) -> bool {
    a.symbol() == b.symbol() && a.fg == b.fg && a.bg == b.bg && a.modifier == b.modifier
}

fn emit_run(out: &mut impl Write, row: u16, col: u16, style: Style, text: &str) -> io::Result<()> {
    if text.is_empty() {
        return Ok(());
    }
    write!(out, "\x1b[{};{}H", row + 1, col + 1)?;
    emit_style(out, style)?;
    write!(out, "{text}")?;
    write!(out, "\x1b[0m")
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

    fn line(text: &str) -> Line<'static> {
        Line::from(text.to_owned())
    }

    type FrameSpec = (Vec<Line<'static>>, Option<(usize, u16)>);

    fn render(frames: Vec<FrameSpec>) -> Vec<u8> {
        let mut out = Vec::new();
        let mut screen = Screen::new(40, 6);
        for (lines, cursor) in frames {
            screen
                .draw(
                    &mut out,
                    &Frame {
                        lines: &lines,
                        cursor,
                    },
                )
                .expect("draw");
        }
        out
    }

    #[test]
    fn first_draw_paints_all_rows_and_positions_cursor() {
        let out = render(vec![(vec![line("hello"), line("world")], Some((1, 3)))]);
        let s = String::from_utf8(out).expect("utf8");
        assert!(s.contains("hello"));
        assert!(s.contains("world"));
        // cursor lands on row 2 (screen row 2 = line index 1 + offset 0), col 4
        assert!(s.contains("\x1b[2;4H"));
        assert!(s.contains("\x1b[?25h"));
    }

    #[test]
    fn unchanged_frame_writes_no_text_cells() {
        let frame = vec![(vec![line("alpha"), line("beta")], None::<(usize, u16)>)]
            .into_iter()
            .chain(std::iter::repeat_n(
                (vec![line("alpha"), line("beta")], None),
                2,
            ))
            .collect::<Vec<_>>();
        let out = render(frame);
        let s = String::from_utf8(out).expect("utf8");
        assert_eq!(s.matches("alpha").count(), 1, "repaint must skip text: {s}");
        assert_eq!(s.matches("beta").count(), 1);
    }

    #[test]
    fn appended_lines_scroll_instead_of_rewriting_history() {
        let first: Vec<Line<'static>> = (0..6).map(|i| line(&format!("row{i}"))).collect();
        let mut second = first.clone();
        second.push(line("row6"));
        second.push(line("row7"));
        let out = render(vec![(first.clone(), None), (second, None)]);
        let s = String::from_utf8(out).expect("utf8");
        // two physical scrolls for the two appended rows...
        assert_eq!(s.matches("\r\n").count(), 2);
        // ...history never rewritten...
        assert_eq!(s.matches("row0").count(), 1);
        assert_eq!(s.matches("row5").count(), 1);
        // ...only the new tail appears.
        assert_eq!(s.matches("row6").count(), 1);
        assert!(s.contains("row7"));
    }

    #[test]
    fn shrink_blanks_the_freed_rows() {
        let first: Vec<Line<'static>> = (0..6).map(|i| line(&format!("row{i}"))).collect();
        let second: Vec<Line<'static>> = first[..3].to_vec();
        let out = render(vec![(first, None), (second, None)]);
        let s = String::from_utf8(out).expect("utf8");
        // rows 3..6 must be cleared: their cells are rewritten as spaces
        let cleared = "\x1b[4;1H";
        assert!(s.contains(cleared), "expected row-4 erase: {s}");
    }

    #[test]
    fn live_edit_rewrites_only_that_row() {
        let out = render(vec![
            (vec![line("idle"), line("› ")], Some((1, 2))),
            (vec![line("idle"), line("› hi")], Some((1, 4))),
        ]);
        let s = String::from_utf8(out).expect("utf8");
        assert!(s.contains("hi"));
        // 'idle' was not repainted
        assert_eq!(s.matches("idle").count(), 1);
        // cursor moved to col 5 on the composer row
        assert!(s.contains("\x1b[2;5H"));
    }

    #[test]
    fn styled_line_emits_sgr_once_per_run() {
        let styled = Line::from(vec![Span::from("dim ").dim(), Span::from("bright")]);
        let out = render(vec![(vec![styled], None)]);
        let s = String::from_utf8(out).expect("utf8");
        assert!(s.contains("\x1b[2m"));
        assert!(s.ends_with("\x1b[0m") || s.contains("\x1b[0m"));
    }

    use ratatui::text::Span;
}
