//! Status line rendering (mode, model, token usage).

use crate::tui::App;
use crate::tui::util::{display_width, format_tokens, truncate_to_display_width};
use crossterm::cursor::MoveTo;
use crossterm::execute;
use crossterm::style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor};
use crossterm::terminal::{Clear, ClearType};
use std::io::Write;

fn write_plain_clipped<W: Write>(
    w: &mut W,
    text: &str,
    remaining: &mut usize,
) -> std::io::Result<()> {
    if *remaining == 0 {
        return Ok(());
    }
    let clipped = truncate_to_display_width(text, *remaining);
    if clipped.is_empty() {
        return Ok(());
    }
    *remaining = remaining.saturating_sub(display_width(&clipped));
    write!(w, "{clipped}")
}

fn write_colored_clipped<W: Write>(
    w: &mut W,
    text: &str,
    color: Color,
    remaining: &mut usize,
) -> std::io::Result<()> {
    if *remaining == 0 {
        return Ok(());
    }
    let clipped = truncate_to_display_width(text, *remaining);
    if clipped.is_empty() {
        return Ok(());
    }
    *remaining = remaining.saturating_sub(display_width(&clipped));
    execute!(w, SetForegroundColor(color), Print(clipped), ResetColor)
}

fn write_dim_clipped<W: Write>(
    w: &mut W,
    text: &str,
    remaining: &mut usize,
) -> std::io::Result<()> {
    if *remaining == 0 {
        return Ok(());
    }
    let clipped = truncate_to_display_width(text, *remaining);
    if clipped.is_empty() {
        return Ok(());
    }
    *remaining = remaining.saturating_sub(display_width(&clipped));
    execute!(
        w,
        SetAttribute(Attribute::Dim),
        Print(clipped),
        SetAttribute(Attribute::Reset)
    )
}

impl App {
    /// Render status line directly with crossterm.
    pub(crate) fn render_status_direct<W: std::io::Write>(
        &self,
        w: &mut W,
        row: u16,
        width: u16,
    ) -> std::io::Result<()> {
        execute!(w, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
        use crate::tool::ToolMode;

        let max_cells = width.saturating_sub(1) as usize;
        if max_cells == 0 {
            return Ok(());
        }

        let model_name = self
            .session
            .model
            .split('/')
            .next_back()
            .unwrap_or(&self.session.model);

        let (mode_label, mode_color) = match self.tool_mode {
            ToolMode::Read => ("READ", Color::Cyan),
            ToolMode::Write => ("WRITE", Color::Yellow),
        };

        let mut remaining = max_cells;
        write_plain_clipped(w, " [", &mut remaining)?;
        write_colored_clipped(w, mode_label, mode_color, &mut remaining)?;
        write_plain_clipped(w, "] • ", &mut remaining)?;
        write_plain_clipped(w, model_name, &mut remaining)?;

        // Thinking level (only shown when active)
        let think_label = self.thinking_level.label();
        if !think_label.is_empty() {
            write_plain_clipped(w, " ", &mut remaining)?;
            write_colored_clipped(w, think_label, Color::Magenta, &mut remaining)?;
        }

        // Token usage if available
        if let Some((used, max)) = self.token_usage {
            write_dim_clipped(
                w,
                &format!(" • {}/{}", format_tokens(used), format_tokens(max)),
                &mut remaining,
            )?;
            if max > 0 {
                let pct = (used * 100) / max;
                write_dim_clipped(w, &format!(" ({pct}%)"), &mut remaining)?;
            }
        }

        Ok(())
    }
}
