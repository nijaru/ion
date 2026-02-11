//! Progress line rendering (spinner, completion status, stats).

use crate::tui::App;
use crate::tui::util::{format_cost, format_elapsed, format_tokens};
use crossterm::cursor::MoveTo;
use crossterm::execute;
use crossterm::terminal::{Clear, ClearType};

impl App {
    /// Render progress line directly with crossterm.
    pub(crate) fn render_progress_direct<W: std::io::Write>(
        &self,
        w: &mut W,
        row: u16,
        width: u16,
    ) -> std::io::Result<()> {
        execute!(w, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
        let text = if self.is_running {
            self.progress_running_text()
        } else {
            self.progress_completed_text().unwrap_or_default()
        };
        let clipped =
            crate::tui::util::truncate_to_display_width(&text, width.saturating_sub(1) as usize);
        write!(w, "{clipped}")?;
        Ok(())
    }

    #[allow(clippy::cast_possible_truncation)]
    fn progress_running_text(&self) -> String {
        const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let frame = (self.frame_count % SPINNER.len() as u64) as usize;

        if let Some((ref reason, delay, started)) = self.task.retry_status {
            let elapsed = started.elapsed().as_secs();
            let remaining = delay.saturating_sub(elapsed);
            return format!(
                " {} {} • retrying in {}s",
                SPINNER[frame], reason, remaining
            );
        }

        let mut out = format!(" {} ", SPINNER[frame]);
        if let Some(ref tool) = self.task.current_tool {
            out.push_str(tool);
        } else if self.task.thinking_start.is_some() {
            out.push_str("Thinking...");
        } else {
            out.push_str("Ionizing...");
        }

        if let Some(start) = self.task.start_time {
            let elapsed = start.elapsed().as_secs();
            out.push_str(&format!(" ({elapsed}s • Esc to cancel)"));
        }
        out
    }

    /// Build progress line after task completion (status + stats summary).
    fn progress_completed_text(&self) -> Option<String> {
        let summary = self.last_task_summary.as_ref()?;

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

        let (symbol, label) = if self.last_error.is_some() {
            ("✗ ", "Error")
        } else if summary.was_cancelled {
            ("⚠ ", "Canceled")
        } else {
            ("✓ ", "Completed")
        };

        Some(format!(" {symbol}{label} ({})", stats.join(" • ")))
    }
}
