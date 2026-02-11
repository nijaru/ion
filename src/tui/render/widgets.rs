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
    // Leave the final terminal column empty to avoid autowrap-induced
    // redraw corruption in narrow terminals.
    let safe_width = width.saturating_sub(1) as usize;
    execute!(
        w,
        MoveTo(0, row),
        SetForegroundColor(Color::Cyan),
        Print("─".repeat(safe_width)),
        ResetColor
    )
}
