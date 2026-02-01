//! Reusable widget rendering functions.

use crossterm::execute;

/// Draw a horizontal border line at the given row.
pub fn draw_horizontal_border<W: std::io::Write>(
    w: &mut W,
    row: u16,
    width: u16,
) -> std::io::Result<()> {
    use crossterm::{
        cursor::MoveTo,
        style::{Color, Print, ResetColor, SetForegroundColor},
    };
    execute!(
        w,
        MoveTo(0, row),
        SetForegroundColor(Color::Cyan),
        Print("─".repeat(width as usize)),
        ResetColor
    )
}
