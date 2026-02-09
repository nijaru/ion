//! Status line rendering (mode, model, token usage).

use crate::tui::util::format_tokens;
use crate::tui::App;
use crossterm::cursor::MoveTo;
use crossterm::execute;
use crossterm::terminal::{Clear, ClearType};

impl App {
    /// Render status line directly with crossterm.
    pub(crate) fn render_status_direct<W: std::io::Write>(
        &self,
        w: &mut W,
        row: u16,
        _width: u16,
    ) -> std::io::Result<()> {
        execute!(w, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
        use crate::tool::ToolMode;
        use crossterm::style::{
            Attribute, Color as CColor, Print, ResetColor, SetAttribute, SetForegroundColor,
        };

        let model_name = self
            .session
            .model
            .split('/')
            .next_back()
            .unwrap_or(&self.session.model);

        let (mode_label, mode_color) = match self.tool_mode {
            ToolMode::Read => ("READ", CColor::Cyan),
            ToolMode::Write => ("WRITE", CColor::Yellow),
        };

        write!(w, " [")?;
        execute!(
            w,
            SetForegroundColor(mode_color),
            Print(mode_label),
            ResetColor
        )?;
        write!(w, "] · {model_name}")?;

        // Thinking level (only shown when active)
        let think_label = self.thinking_level.label();
        if !think_label.is_empty() {
            write!(w, " ")?;
            execute!(
                w,
                SetForegroundColor(CColor::Magenta),
                Print(think_label),
                ResetColor
            )?;
        }

        // Token usage if available
        if let Some((used, max)) = self.token_usage {
            execute!(w, SetAttribute(Attribute::Dim))?;
            write!(w, " · {}/{}", format_tokens(used), format_tokens(max))?;
            if max > 0 {
                let pct = (used * 100) / max;
                write!(w, " ({pct}%)")?;
            }
            execute!(w, SetAttribute(Attribute::Reset))?;
        }

        Ok(())
    }
}
