//! Progress line rendering (spinner, completion status, stats).

use crate::tui::util::{format_cost, format_elapsed, format_tokens};
use crate::tui::App;
use crossterm::execute;

impl App {
    /// Render progress line directly with crossterm.
    pub(crate) fn render_progress_direct<W: std::io::Write>(
        &self,
        w: &mut W,
        _width: u16,
    ) -> std::io::Result<()> {
        if self.is_running {
            self.render_progress_running(w)
        } else {
            self.render_progress_completed(w)
        }
    }

    /// Render progress line when a task is running (spinner + tool name + elapsed).
    pub(crate) fn render_progress_running<W: std::io::Write>(
        &self,
        w: &mut W,
    ) -> std::io::Result<()> {
        use crossterm::style::{
            Attribute, Color as CColor, Print, ResetColor, SetAttribute, SetForegroundColor,
        };

        const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let frame = (self.frame_count % SPINNER.len() as u64) as usize;

        // Check if we're in a retry state
        if let Some((ref reason, delay, started)) = self.task.retry_status {
            let elapsed = started.elapsed().as_secs();
            let remaining = delay.saturating_sub(elapsed);
            execute!(
                w,
                Print(" "),
                SetForegroundColor(CColor::Yellow),
                Print(SPINNER[frame]),
                Print(" "),
                Print(reason),
                Print(" · retrying in "),
                Print(remaining),
                Print("s"),
                ResetColor
            )?;
            return Ok(());
        }

        execute!(
            w,
            Print(" "),
            SetForegroundColor(CColor::Cyan),
            Print(SPINNER[frame]),
            ResetColor
        )?;

        execute!(w, SetForegroundColor(CColor::Cyan))?;
        if let Some(ref tool) = self.task.current_tool {
            execute!(w, Print(" "), Print(tool))?;
        } else if self.task.thinking_start.is_some() {
            execute!(w, Print(" Thinking..."))?;
        } else {
            execute!(w, Print(" Ionizing..."))?;
        }
        execute!(w, ResetColor)?;

        if let Some(start) = self.task.start_time {
            let elapsed = start.elapsed().as_secs();
            execute!(
                w,
                SetAttribute(Attribute::Dim),
                Print(" ("),
                Print(elapsed),
                Print("s · Esc to cancel)"),
                SetAttribute(Attribute::Reset)
            )?;
        }

        Ok(())
    }

    /// Render progress line after task completion (status + stats summary).
    pub(crate) fn render_progress_completed<W: std::io::Write>(
        &self,
        w: &mut W,
    ) -> std::io::Result<()> {
        use crossterm::style::{
            Attribute, Color as CColor, Print, ResetColor, SetAttribute, SetForegroundColor,
        };

        let Some(ref summary) = self.last_task_summary else {
            return Ok(());
        };

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

        let (symbol, label, color) = if self.last_error.is_some() {
            ("✗ ", "Error", CColor::Red)
        } else if summary.was_cancelled {
            ("⚠ ", "Canceled", CColor::Yellow)
        } else {
            ("✓ ", "Completed", CColor::Green)
        };

        write!(w, " ")?;
        execute!(
            w,
            SetForegroundColor(color),
            Print(symbol),
            Print(label),
            ResetColor
        )?;
        execute!(
            w,
            SetAttribute(Attribute::Dim),
            Print(" ("),
            Print(stats.join(" · ")),
            Print(")"),
            SetAttribute(Attribute::Reset)
        )?;

        Ok(())
    }
}
