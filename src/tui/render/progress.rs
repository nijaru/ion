//! Progress line rendering (spinner, completion status, stats).

use crate::tui::util::{
    display_width, format_cost, format_elapsed, format_tokens, truncate_to_display_width,
};
use crate::tui::App;
use crossterm::cursor::MoveTo;
use crossterm::execute;
use crossterm::style::{Color, Print, ResetColor, SetForegroundColor};
use crossterm::terminal::{Clear, ClearType};
use std::io::Write;

fn write_colored<W: Write>(
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

fn write_plain<W: Write>(w: &mut W, text: &str, remaining: &mut usize) -> std::io::Result<()> {
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

impl App {
    /// Render progress line directly with crossterm.
    pub(crate) fn render_progress_direct<W: std::io::Write>(
        &self,
        w: &mut W,
        row: u16,
        width: u16,
    ) -> std::io::Result<()> {
        execute!(w, MoveTo(0, row), Clear(ClearType::CurrentLine))?;

        let max_cells = width.saturating_sub(1) as usize;
        if max_cells == 0 {
            return Ok(());
        }

        if self.is_running {
            self.render_progress_running(w, max_cells)
        } else {
            self.render_progress_completed(w, max_cells)
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    fn render_progress_running<W: std::io::Write>(
        &self,
        w: &mut W,
        max_cells: usize,
    ) -> std::io::Result<()> {
        const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let frame = (self.frame_count % SPINNER.len() as u64) as usize;
        let mut remaining = max_cells;

        if let Some((ref reason, delay, started)) = self.task.retry_status {
            let elapsed = started.elapsed().as_secs();
            let secs_left = delay.saturating_sub(elapsed);
            write_plain(w, " ", &mut remaining)?;
            write_colored(w, SPINNER[frame], Color::Yellow, &mut remaining)?;
            write_plain(
                w,
                &format!(" {reason} • retrying in {secs_left}s"),
                &mut remaining,
            )?;
            return Ok(());
        }

        write_plain(w, " ", &mut remaining)?;
        write_colored(w, SPINNER[frame], Color::Cyan, &mut remaining)?;
        write_plain(w, " ", &mut remaining)?;

        if let Some(ref tool) = self.task.current_tool {
            write_plain(w, tool, &mut remaining)?;
        } else if self.task.thinking_start.is_some() {
            write_plain(w, "Thinking...", &mut remaining)?;
        } else {
            write_plain(w, "Ionizing...", &mut remaining)?;
        }

        if let Some(start) = self.task.start_time {
            let elapsed = start.elapsed().as_secs();
            write_plain(w, &format!(" ({elapsed}s • Esc to cancel)"), &mut remaining)?;
        }
        Ok(())
    }

    fn render_progress_completed<W: std::io::Write>(
        &self,
        w: &mut W,
        max_cells: usize,
    ) -> std::io::Result<()> {
        let Some(summary) = self.last_task_summary.as_ref() else {
            return Ok(());
        };

        let mut remaining = max_cells;

        let (symbol, symbol_color, label) = if self.last_error.is_some() {
            ("✗ ", Color::Red, "Error")
        } else if summary.was_cancelled {
            ("⚠ ", Color::Yellow, "Canceled")
        } else {
            ("✓ ", Color::Green, "Completed")
        };

        write_plain(w, " ", &mut remaining)?;
        write_colored(w, symbol, symbol_color, &mut remaining)?;
        write_plain(w, label, &mut remaining)?;

        let elapsed = format_elapsed(summary.elapsed.as_secs());
        let mut stats = vec![elapsed];
        if summary.input_tokens > 0 {
            stats.push(format!("↑ {}", format_tokens(summary.input_tokens)));
        }
        if summary.output_tokens > 0 {
            stats.push(format!("↓ {}", format_tokens(summary.output_tokens)));
        }
        if summary.cost > 0.0 {
            stats.push(format_cost(summary.cost));
        }

        write_plain(w, &format!(" ({})", stats.join(" • ")), &mut remaining)?;
        Ok(())
    }
}
