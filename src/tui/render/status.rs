//! Status line rendering (mode, model, token usage).

use crate::tui::App;
use crate::tui::util::format_tokens;
use crossterm::cursor::MoveTo;
use crossterm::execute;
use crossterm::terminal::{Clear, ClearType};

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

        let model_name = self
            .session
            .model
            .split('/')
            .next_back()
            .unwrap_or(&self.session.model);

        let mode_label = match self.tool_mode {
            ToolMode::Read => "READ",
            ToolMode::Write => "WRITE",
        };

        let mut text = format!(" [{mode_label}] • {model_name}");

        // Thinking level (only shown when active)
        let think_label = self.thinking_level.label();
        if !think_label.is_empty() {
            text.push(' ');
            text.push_str(think_label);
        }

        // Token usage if available
        if let Some((used, max)) = self.token_usage {
            text.push_str(&format!(
                " • {}/{}",
                format_tokens(used),
                format_tokens(max)
            ));
            if max > 0 {
                let pct = (used * 100) / max;
                text.push_str(&format!(" ({pct}%)"));
            }
        }

        let clipped =
            crate::tui::util::truncate_to_display_width(&text, width.saturating_sub(1) as usize);
        write!(w, "{clipped}")?;
        Ok(())
    }
}
