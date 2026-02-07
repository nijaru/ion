//! Direct crossterm rendering functions (TUI v2 - no ratatui).

use crate::tui::composer::{build_visual_lines, ComposerState};
use crate::tui::render::{
    widgets::draw_horizontal_border, CONTINUATION, INPUT_MARGIN, PROGRESS_HEIGHT, PROMPT,
    PROMPT_WIDTH,
};
use crate::tui::render_selector::{self, SelectorData, SelectorItem};
use crate::tui::types::{Mode, SelectorPage};
use crate::tui::util::{format_elapsed, format_relative_time, format_tokens};
use crate::tui::App;
use crossterm::cursor::MoveTo;
use crossterm::execute;
use crossterm::terminal::{Clear, ClearType};

impl App {
    /// Direct crossterm rendering (TUI v2 - no ratatui Terminal/Frame).
    /// Renders the bottom UI area: progress, input, status.
    pub fn draw_direct<W: std::io::Write>(
        &mut self,
        w: &mut W,
        width: u16,
        height: u16,
    ) -> std::io::Result<()> {
        let ui_height = self.calculate_ui_height(width, height);
        let ui_start = self.ui_start_row(height, ui_height);
        let progress_height = PROGRESS_HEIGHT;

        // Determine clear_from based on positioning mode.
        // Read last_ui_start BEFORE updating; store new value for next frame.
        let old_ui_start = self.render_state.last_ui_start;
        self.render_state.last_ui_start = Some(ui_start);

        let clear_from = if self.render_state.chat_row.is_some() {
            // Row-tracking: UI sits directly below chat content. Never clear
            // above ui_start -- that would erase chat lines printed this frame.
            ui_start
        } else {
            // Scroll mode: UI is at bottom. Use min(old, new) to clear stale
            // UI remnants when ui_height changes (e.g. agent finish, selector).
            old_ui_start.map_or(ui_start, |old| old.min(ui_start))
        };

        // Clear from UI position downward (never clear full screen - preserves scrollback)
        execute!(w, MoveTo(0, clear_from), Clear(ClearType::FromCursorDown))?;

        // Progress line (only in Input mode when active - selector has its own UI)
        if progress_height > 0 && self.mode == Mode::Input {
            execute!(w, MoveTo(0, ui_start), Clear(ClearType::CurrentLine))?;
            self.render_progress_direct(w, width)?;
        }

        // Input area (with borders)
        let input_start = ui_start.saturating_add(progress_height);
        let input_height = self.calculate_input_height(width, height).saturating_sub(2); // Minus borders

        // Top border
        draw_horizontal_border(w, input_start, width)?;

        // Input content
        let content_start = input_start.saturating_add(1);
        self.render_input_direct(w, content_start, width, input_height)?;

        // Bottom border
        let border_row = content_start.saturating_add(input_height);
        draw_horizontal_border(w, border_row, width)?;

        // Status line
        let status_row = border_row.saturating_add(1);
        execute!(w, MoveTo(0, status_row), Clear(ClearType::CurrentLine))?;

        // In selector mode, render selector instead of normal input/status
        if self.mode == Mode::Selector {
            self.render_selector_direct(w, ui_start, width, height)?;
        } else if self.mode == Mode::HistorySearch {
            self.render_history_search(w, input_start, width)?;
        } else {
            self.render_status_direct(w, width)?;

            // Render completer popup above input (mutually exclusive)
            if self.command_completer.is_active() {
                self.command_completer.render(w, input_start, width)?;
            } else if self.file_completer.is_active() {
                self.file_completer.render(w, input_start, width)?;
            }

            // Position cursor in input area
            // cursor_pos is relative (x within content, y is visual line 0-indexed)
            let (cursor_x, cursor_y) = self.input_state.cursor_pos;
            let scroll_offset = self.input_state.scroll_offset() as u16;
            let cursor_y = cursor_y.saturating_sub(scroll_offset);
            execute!(w, MoveTo(cursor_x + PROMPT_WIDTH, content_start + cursor_y))?;
        }

        Ok(())
    }

    /// Extract data needed to render the current selector page.
    pub(crate) fn selector_data(&self) -> SelectorData {
        match self.selector_page {
            SelectorPage::Provider => {
                // Find max id length for column alignment
                let max_id_len = self
                    .provider_picker
                    .filtered()
                    .iter()
                    .map(|s| s.provider.id().len())
                    .max()
                    .unwrap_or(0);

                let items = self
                    .provider_picker
                    .filtered()
                    .iter()
                    .map(|s| {
                        // Always show id and auth method in aligned columns
                        let id = s.provider.id();
                        let auth_hint = s.provider.auth_hint();
                        let (hint, warning) = if auth_hint.is_empty() {
                            // Local provider - no auth needed
                            (id.to_string(), None)
                        } else if s.provider.is_oauth() {
                            // OAuth providers - show warning (unofficial)
                            (
                                format!("{:width$}", id, width = max_id_len),
                                Some("⚠ unofficial".to_string()),
                            )
                        } else {
                            // API key providers - show env var
                            (
                                format!("{:width$}  {}", id, auth_hint, width = max_id_len),
                                None,
                            )
                        };
                        SelectorItem {
                            label: s.provider.name().to_string(),
                            is_valid: s.authenticated,
                            hint,
                            warning,
                        }
                    })
                    .collect();
                SelectorData {
                    title: "Providers",
                    description: "Select a provider",
                    items,
                    selected_idx: self.provider_picker.list_state().selected().unwrap_or(0),
                    filter_text: self.provider_picker.filter_input().text().to_string(),
                    show_tabs: true,
                    active_tab: 0,
                }
            }
            SelectorPage::Model => {
                let items = self
                    .model_picker
                    .filtered_models
                    .iter()
                    .map(|m| {
                        let hint = crate::tui::util::format_context_window(m.context_window);
                        SelectorItem {
                            label: m.id.clone(),
                            is_valid: true,
                            hint,
                            warning: None,
                        }
                    })
                    .collect();
                SelectorData {
                    title: "Models",
                    description: "Select a model",
                    items,
                    selected_idx: self.model_picker.model_state.selected().unwrap_or(0),
                    filter_text: self.model_picker.filter_input.text().to_string(),
                    show_tabs: true,
                    active_tab: 1,
                }
            }
            SelectorPage::Session => {
                let items = self
                    .session_picker
                    .filtered_sessions()
                    .iter()
                    .map(|s| {
                        let preview = s.first_user_message.as_ref().map_or_else(
                            || "No preview".to_string(),
                            |m: &String| m.chars().take(40).collect::<String>(),
                        );
                        let label = format!("{} - {}", preview, format_relative_time(s.updated_at));
                        SelectorItem {
                            label,
                            is_valid: true,
                            hint: String::new(),
                            warning: None,
                        }
                    })
                    .collect();
                SelectorData {
                    title: "Sessions",
                    description: "Select a session to resume",
                    items,
                    selected_idx: self.session_picker.list_state().selected().unwrap_or(0),
                    filter_text: self.session_picker.filter_input().text().to_string(),
                    show_tabs: false,
                    active_tab: 0,
                }
            }
        }
    }

    /// Render selector (provider/model/session picker) directly with crossterm.
    pub(crate) fn render_selector_direct<W: std::io::Write>(
        &mut self,
        w: &mut W,
        start_row: u16,
        width: u16,
        _height: u16,
    ) -> std::io::Result<()> {
        use crossterm::cursor::MoveTo;

        let data = self.selector_data();
        let (cursor_col, cursor_row) =
            render_selector::render_selector(w, &data, start_row, width)?;

        // Position cursor in filter input
        execute!(w, MoveTo(cursor_col, cursor_row))?;

        Ok(())
    }

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

    /// Render input content directly with crossterm.
    pub(crate) fn render_input_direct<W: std::io::Write>(
        &mut self,
        w: &mut W,
        start_row: u16,
        width: u16,
        height: u16,
    ) -> std::io::Result<()> {
        use crossterm::cursor::MoveTo;

        let content = self.input_buffer.get_content();
        let content_width = width.saturating_sub(INPUT_MARGIN) as usize;
        let visual_lines = build_visual_lines(&content, content_width);

        // Calculate cursor and line count from precomputed content/lines (single pass)
        if content_width > 0 {
            self.input_state.calculate_cursor_pos_with(
                &content,
                &visual_lines,
                self.input_buffer.len_chars(),
                content_width,
            );
        }
        let total_lines =
            ComposerState::visual_line_count_with(&content, &visual_lines, content_width);
        let visible_height = height as usize;
        self.input_state
            .scroll_to_cursor(visible_height, total_lines);
        let scroll_offset = self.input_state.scroll_offset();
        let total_chars = content.chars().count();

        for row in 0..visible_height {
            let line_index = scroll_offset + row;
            if line_index >= total_lines {
                break;
            }
            let (start, end) = if line_index < visual_lines.len() {
                visual_lines[line_index]
            } else {
                (total_chars, total_chars)
            };

            // Extract chunk for this visual line (exclude trailing newline if present)
            let chunk: String = content
                .chars()
                .skip(start)
                .take(end.saturating_sub(start))
                .filter(|&c| c != '\n')
                .collect();

            execute!(w, MoveTo(0, start_row + row as u16))?;
            if line_index == 0 {
                write!(w, "{PROMPT}{chunk}")?;
            } else {
                write!(w, "{CONTINUATION}{chunk}")?;
            }
        }

        // If empty, just show the prompt
        if content.is_empty() {
            execute!(w, MoveTo(0, start_row))?;
            write!(w, "{PROMPT}")?;
        }

        Ok(())
    }

    /// Render status line directly with crossterm.
    pub(crate) fn render_status_direct<W: std::io::Write>(
        &self,
        w: &mut W,
        _width: u16,
    ) -> std::io::Result<()> {
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
            let format_k = |n: usize| -> String {
                if n >= 1000 {
                    format!("{}k", n / 1000)
                } else {
                    n.to_string()
                }
            };
            execute!(w, SetAttribute(Attribute::Dim))?;
            write!(w, " · {}/{}", format_k(used), format_k(max))?;
            if max > 0 {
                let pct = (used * 100) / max;
                write!(w, " ({pct}%)")?;
            }
            execute!(w, SetAttribute(Attribute::Reset))?;
        }

        Ok(())
    }

    /// Render history search overlay (Ctrl+R).
    #[allow(clippy::cast_possible_truncation)]
    fn render_history_search<W: std::io::Write>(
        &self,
        w: &mut W,
        input_start: u16,
        width: u16,
    ) -> std::io::Result<()> {
        use crossterm::style::{
            Attribute, Color as CColor, Print, ResetColor, SetAttribute, SetForegroundColor,
        };

        let max_visible = 8;
        let matches = &self.history_search.matches;
        let selected = self.history_search.selected;
        let query = &self.history_search.query;

        // Calculate how many matches to show
        let visible_count = matches.len().min(max_visible);
        let popup_height = (visible_count + 1) as u16; // +1 for search prompt
        let popup_start = input_start.saturating_sub(popup_height);

        // Render search prompt at bottom of popup
        let prompt_row = input_start.saturating_sub(1);
        execute!(w, MoveTo(0, prompt_row), Clear(ClearType::CurrentLine))?;
        execute!(
            w,
            SetForegroundColor(CColor::Cyan),
            Print("(reverse-i-search)`"),
            ResetColor,
            Print(query),
            SetForegroundColor(CColor::Cyan),
            Print("': "),
            ResetColor,
        )?;

        // Show selected entry preview
        if let Some(&idx) = matches.get(selected)
            && let Some(entry) = self.input_history.get(idx)
        {
            let preview: String = entry
                .chars()
                .take((width as usize).saturating_sub(25))
                .collect();
            execute!(w, Print(&preview))?;
        }

        // Render matches above the prompt
        if !matches.is_empty() {
            // Calculate visible window
            let start_idx = if selected >= max_visible {
                selected - max_visible + 1
            } else {
                0
            };

            for (i, &history_idx) in matches.iter().skip(start_idx).take(max_visible).enumerate() {
                let row = popup_start + i as u16;
                let is_selected = start_idx + i == selected;

                execute!(w, MoveTo(1, row), Clear(ClearType::CurrentLine))?;

                if is_selected {
                    execute!(w, SetAttribute(Attribute::Reverse))?;
                } else {
                    execute!(w, SetAttribute(Attribute::Dim))?;
                }

                if let Some(entry) = self.input_history.get(history_idx) {
                    // Truncate long entries
                    let max_len = (width as usize).saturating_sub(4);
                    let display: String = entry
                        .lines()
                        .next()
                        .unwrap_or("")
                        .chars()
                        .take(max_len)
                        .collect();
                    execute!(w, Print(" "), Print(&display))?;
                }

                execute!(w, SetAttribute(Attribute::Reset))?;
            }
        }

        Ok(())
    }
}
