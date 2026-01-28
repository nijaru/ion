//! Rendering functions for the TUI.

use crate::tui::chat_renderer::ChatRenderer;
use crate::tui::composer::build_visual_lines;
use crate::tui::message_list::Sender;
use crate::tui::terminal::StyledLine;
use crate::tui::types::{Mode, SelectorPage};
use crate::tui::util::{format_elapsed, format_relative_time, format_tokens};
use crate::tui::App;
use crossterm::execute;

/// Input prompt prefix " > "
const PROMPT: &str = " > ";
/// Continuation line prefix "   "
const CONTINUATION: &str = "   ";
/// Width of prompt/continuation prefix
const PROMPT_WIDTH: u16 = 3;
/// Total input margin (prompt + right padding)
const INPUT_MARGIN: u16 = 4;

impl App {
    /// Calculate the height needed for the input box based on content.
    /// Returns height including borders.
    /// Min: 3 lines (1 content + 2 borders)
    /// Max: viewport_height - 3 (reserved for progress + status)
    pub(super) fn calculate_input_height(&self, viewport_width: u16, viewport_height: u16) -> u16 {
        const MIN_HEIGHT: u16 = 3;
        const MIN_RESERVED: u16 = 3; // status (1) + optional progress (up to 2)
        const BORDER_OVERHEAD: u16 = 2; // Top and bottom borders
        const LEFT_MARGIN: u16 = 3; // " > " prompt gutter
        const RIGHT_MARGIN: u16 = 1; // Right margin for symmetry

        // Dynamic max based on viewport height
        let max_height = viewport_height.saturating_sub(MIN_RESERVED).max(MIN_HEIGHT);

        if self.input_is_empty() {
            return MIN_HEIGHT;
        }

        // Available width for text (subtract borders, gutter, and right margin)
        let text_width = viewport_width
            .saturating_sub(BORDER_OVERHEAD)
            .saturating_sub(LEFT_MARGIN + RIGHT_MARGIN) as usize;
        if text_width == 0 {
            return MIN_HEIGHT;
        }

        // Use ComposerState's visual line count
        let line_count = self
            .input_state
            .visual_line_count(&self.input_buffer, text_width) as u16;

        // Add border overhead and clamp to bounds
        (line_count + BORDER_OVERHEAD).clamp(MIN_HEIGHT, max_height)
    }

    /// Calculate the total height of the bottom UI area.
    /// Returns: progress (1) + input (with borders) + status (1)
    pub fn calculate_ui_height(&self, width: u16, height: u16) -> u16 {
        let progress_height = 1u16;
        let input_height = self.calculate_input_height(width, height);
        let status_height = 1u16;
        progress_height + input_height + status_height
    }

    /// Take new chat entries and render them as lines for insertion.
    pub fn take_chat_inserts(&mut self, width: u16) -> Vec<StyledLine> {
        let wrap_width = width.saturating_sub(2);
        if wrap_width == 0 {
            return Vec::new();
        }

        // Insert header once at startup (into scrollback, not viewport)
        let header_lines = if !self.header_inserted {
            self.header_inserted = true;
            self.startup_header_lines()
        } else {
            Vec::new()
        };

        let entry_count = self.message_list.entries.len();
        if self.rendered_entries > entry_count {
            self.rendered_entries = 0;
            self.buffered_chat_lines.clear();
        }

        let mut new_lines = Vec::new();
        let mut index = self.rendered_entries;
        while index < entry_count {
            let entry = &self.message_list.entries[index];
            // Only skip the last entry if it's an Agent entry being actively streamed
            // This allows Tool entries and completed Agent responses to render mid-run
            let is_last = index == entry_count - 1;
            if entry.sender == Sender::Agent && self.is_running && is_last {
                break;
            }
            let mut entry_lines = ChatRenderer::build_lines(
                &self.message_list.entries[index..index + 1],
                None,
                wrap_width as usize,
            );
            new_lines.append(&mut entry_lines);
            index += 1;
        }
        self.rendered_entries = index;

        if self.mode == Mode::Selector {
            if !new_lines.is_empty() {
                self.buffered_chat_lines.extend(new_lines);
            }
            // Still return header if it needs to be inserted
            return header_lines;
        }

        if new_lines.is_empty() && self.buffered_chat_lines.is_empty() && header_lines.is_empty() {
            return Vec::new();
        }

        let mut out = header_lines;
        if !self.buffered_chat_lines.is_empty() {
            out.append(&mut self.buffered_chat_lines);
        }
        out.extend(new_lines);
        out
    }

    /// Calculate the viewport height needed for the UI (progress + input + status).
    /// Header is inserted into scrollback, not rendered in viewport.
    /// Note: With full-height viewport, this is no longer used for viewport sizing,
    /// but may be useful for debugging or future use.
    #[allow(dead_code)]
    pub fn viewport_height(&self, terminal_width: u16, terminal_height: u16) -> u16 {
        let input_height = self.calculate_input_height(terminal_width, terminal_height);
        let progress_height = if self.is_running {
            2 // Line 1: gap or queued indicator, Line 2: spinner
        } else if self.last_task_summary.is_some() {
            1
        } else {
            0
        };
        progress_height + input_height + 1 // +1 for status line
    }

    /// Direct crossterm rendering (TUI v2 - no ratatui Terminal/Frame).
    /// Renders the bottom UI area: progress, input, status.
    pub fn draw_direct<W: std::io::Write>(
        &mut self,
        w: &mut W,
        width: u16,
        height: u16,
    ) -> std::io::Result<()> {
        use crossterm::{
            cursor::MoveTo,
            execute,
            terminal::{Clear, ClearType},
        };

        let ui_height = self.calculate_ui_height(width, height);
        let ui_start = height.saturating_sub(ui_height);

        // Detect width decrease - terminal rewraps old content, pushing it up
        let width_decreased = self.last_render_width.is_some_and(|old| width < old);
        self.last_render_width = Some(width);

        // Clear from min of old/new ui_start to handle UI height changes
        let clear_from = self.last_ui_start.map_or(ui_start, |old| old.min(ui_start));
        self.last_ui_start = Some(ui_start);

        if width_decreased {
            // Full clear needed: old wider borders got wrapped into multiple lines
            execute!(w, Clear(ClearType::All), MoveTo(0, ui_start))?;
        } else {
            // Clear from earliest UI position to handle shrinking input
            execute!(w, MoveTo(0, clear_from), Clear(ClearType::FromCursorDown))?;
        }

        // Progress line
        execute!(w, MoveTo(0, ui_start))?;
        self.render_progress_direct(w, width)?;

        // Input area (with borders)
        let input_start = ui_start + 1; // After progress line
        let input_height = self.calculate_input_height(width, height).saturating_sub(2); // Minus borders

        // Top border
        draw_horizontal_border(w, input_start, width)?;

        // Input content
        let content_start = input_start + 1;
        self.render_input_direct(w, content_start, width, input_height)?;

        // Bottom border
        let border_row = content_start + input_height;
        draw_horizontal_border(w, border_row, width)?;

        // Status line
        let status_row = border_row + 1;
        execute!(w, MoveTo(0, status_row))?;

        // In selector mode, render selector instead of normal input/status
        if self.mode == Mode::Selector {
            self.render_selector_direct(w, ui_start, width, height)?;
        } else {
            self.render_status_direct(w, width)?;
            // Position cursor in input area
            // cursor_pos is relative (x within content, y is visual line 0-indexed)
            let (cursor_x, cursor_y) = self.input_state.cursor_pos;
            execute!(w, MoveTo(cursor_x + PROMPT_WIDTH, content_start + cursor_y))?;
        }

        Ok(())
    }

    /// Render selector (provider/model/session picker) directly with crossterm.
    fn render_selector_direct<W: std::io::Write>(
        &mut self,
        w: &mut W,
        start_row: u16,
        width: u16,
        height: u16,
    ) -> std::io::Result<()> {
        use crossterm::{
            cursor::MoveTo,
            style::{
                Attribute, Color as CColor, Print, ResetColor, SetAttribute, SetForegroundColor,
            },
        };

        let (title, description, items, selected_idx, filter_text): (
            &str,
            &str,
            Vec<(String, bool)>,
            usize,
            String,
        ) = match self.selector_page {
            SelectorPage::Provider => {
                let items: Vec<(String, bool)> = self
                    .provider_picker
                    .filtered
                    .iter()
                    .map(|s| (format!("{:?}", s.provider), s.authenticated))
                    .collect();
                (
                    "Providers",
                    "Select a provider",
                    items,
                    self.provider_picker.list_state.selected().unwrap_or(0),
                    self.provider_picker.filter_input.text().to_string(),
                )
            }
            SelectorPage::Model => {
                let items: Vec<(String, bool)> = self
                    .model_picker
                    .filtered_models
                    .iter()
                    .map(|m| (m.id.clone(), true))
                    .collect();
                (
                    "Models",
                    "Select a model",
                    items,
                    self.model_picker.model_state.selected().unwrap_or(0),
                    self.model_picker.filter_input.text().to_string(),
                )
            }
            SelectorPage::Session => {
                let items: Vec<(String, bool)> = self
                    .session_picker
                    .filtered_sessions
                    .iter()
                    .map(|s| {
                        let preview = s
                            .first_user_message
                            .as_ref()
                            .map(|m| m.chars().take(40).collect::<String>())
                            .unwrap_or_else(|| "No preview".to_string());
                        let label = format!("{} - {}", preview, format_relative_time(s.updated_at));
                        (label, true)
                    })
                    .collect();
                (
                    "Sessions",
                    "Select a session to resume",
                    items,
                    self.session_picker.list_state.selected().unwrap_or(0),
                    self.session_picker.filter_input.text().to_string(),
                )
            }
        };

        // Layout: tabs, description, search box, list, hint
        // Clear from start_row to bottom
        execute!(
            w,
            MoveTo(0, start_row),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::FromCursorDown)
        )?;

        let mut row = start_row;

        // Tab bar (only for provider/model, session has its own header)
        if self.selector_page == SelectorPage::Session {
            execute!(w, MoveTo(0, row))?;
            execute!(
                w,
                SetForegroundColor(CColor::Yellow),
                SetAttribute(Attribute::Bold),
                Print(" Sessions"),
                SetAttribute(Attribute::Reset),
                ResetColor
            )?;
        } else {
            let (provider_bold, model_bold) = match self.selector_page {
                SelectorPage::Provider => (true, false),
                _ => (false, true),
            };
            execute!(w, MoveTo(0, row))?;
            write!(w, " ")?;
            if provider_bold {
                execute!(
                    w,
                    SetForegroundColor(CColor::Yellow),
                    SetAttribute(Attribute::Bold)
                )?;
            } else {
                execute!(w, SetAttribute(Attribute::Dim))?;
            }
            write!(w, "Providers")?;
            execute!(w, SetAttribute(Attribute::Reset), ResetColor)?;
            write!(w, "  ")?;
            if model_bold {
                execute!(
                    w,
                    SetForegroundColor(CColor::Yellow),
                    SetAttribute(Attribute::Bold)
                )?;
            } else {
                execute!(w, SetAttribute(Attribute::Dim))?;
            }
            write!(w, "Models")?;
            execute!(w, SetAttribute(Attribute::Reset), ResetColor)?;
        }
        row += 1;

        // Description
        execute!(w, MoveTo(0, row))?;
        write!(w, " {}", description)?;
        row += 1;

        // Search box
        execute!(
            w,
            MoveTo(0, row),
            SetForegroundColor(CColor::Cyan),
            Print("┌─ "),
            Print(title),
            Print(" "),
            Print("─".repeat((width as usize).saturating_sub(title.len() + 5))),
            Print("┐"),
            ResetColor
        )?;
        row += 1;

        execute!(
            w,
            MoveTo(0, row),
            SetForegroundColor(CColor::Cyan),
            Print("│"),
            ResetColor,
            Print(" "),
            Print(&filter_text),
        )?;
        // Save cursor position for filter input
        let filter_cursor_col = 2 + filter_text.len() as u16;
        let filter_cursor_row = row;
        execute!(
            w,
            MoveTo(width - 1, row),
            SetForegroundColor(CColor::Cyan),
            Print("│"),
            ResetColor
        )?;
        row += 1;

        execute!(
            w,
            MoveTo(0, row),
            SetForegroundColor(CColor::Cyan),
            Print("└"),
            Print("─".repeat((width as usize).saturating_sub(2))),
            Print("┘"),
            ResetColor
        )?;
        row += 1;

        // List items
        let max_list_height = height.saturating_sub(row.saturating_sub(start_row) + 1);
        let list_height = (items.len() as u16).min(max_list_height).max(1);

        // Calculate scroll offset to keep selection visible
        let scroll_offset = if selected_idx >= list_height as usize {
            selected_idx.saturating_sub(list_height as usize - 1)
        } else {
            0
        };

        for (i, (item, is_valid)) in items
            .iter()
            .skip(scroll_offset)
            .take(list_height as usize)
            .enumerate()
        {
            execute!(w, MoveTo(0, row))?;
            let actual_idx = scroll_offset + i;
            let is_selected = actual_idx == selected_idx;

            if is_selected {
                execute!(
                    w,
                    SetForegroundColor(CColor::Yellow),
                    SetAttribute(Attribute::Bold)
                )?;
                write!(w, " >")?;
            } else {
                write!(w, "  ")?;
            }

            if *is_valid {
                execute!(w, SetForegroundColor(CColor::Green), Print(" ● "))?;
            } else {
                execute!(w, SetAttribute(Attribute::Dim), Print(" ○ "))?;
            }

            if is_selected {
                execute!(
                    w,
                    SetForegroundColor(CColor::Yellow),
                    SetAttribute(Attribute::Bold)
                )?;
            } else if !is_valid {
                execute!(w, SetAttribute(Attribute::Dim))?;
            }
            // Truncate item name if too long
            let max_item_len = (width as usize).saturating_sub(6);
            let display_name: String = item.chars().take(max_item_len).collect();
            write!(w, "{}", display_name)?;
            execute!(w, SetAttribute(Attribute::Reset), ResetColor)?;

            row += 1;
        }

        // Hint line
        execute!(w, MoveTo(0, row), SetAttribute(Attribute::Dim))?;
        write!(w, " Type to filter · Enter to select · Esc to close")?;
        execute!(w, SetAttribute(Attribute::Reset))?;

        // Position cursor in filter input
        execute!(w, MoveTo(filter_cursor_col, filter_cursor_row))?;

        Ok(())
    }

    /// Render progress line directly with crossterm.
    fn render_progress_direct<W: std::io::Write>(
        &self,
        w: &mut W,
        _width: u16,
    ) -> std::io::Result<()> {
        use crossterm::style::{
            Attribute, Color as CColor, Print, ResetColor, SetAttribute, SetForegroundColor,
        };

        if !self.is_running {
            // Show last task summary if available
            if let Some(ref summary) = self.last_task_summary {
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
                    Print(format!(" ({})", stats.join(" · "))),
                    SetAttribute(Attribute::Reset)
                )?;
            }
            return Ok(());
        }

        // Running state - show spinner and stats
        let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let frame = (self.frame_count % spinner.len() as u64) as usize;

        // Cyan spinner
        execute!(
            w,
            Print(" "),
            SetForegroundColor(CColor::Cyan),
            Print(spinner[frame]),
            ResetColor
        )?;

        // "Ionizing..." or tool name in cyan
        execute!(w, SetForegroundColor(CColor::Cyan))?;
        if let Some(ref tool) = self.current_tool {
            execute!(w, Print(format!(" {}", tool)))?;
        } else {
            execute!(w, Print(" Ionizing..."))?;
        }
        execute!(w, ResetColor)?;

        // Elapsed time in dim
        if let Some(start) = self.task_start_time {
            let elapsed = start.elapsed().as_secs();
            execute!(
                w,
                SetAttribute(Attribute::Dim),
                Print(format!(" ({}s · Esc to cancel)", elapsed)),
                SetAttribute(Attribute::Reset)
            )?;
        }

        Ok(())
    }

    /// Render input content directly with crossterm.
    fn render_input_direct<W: std::io::Write>(
        &mut self,
        w: &mut W,
        start_row: u16,
        width: u16,
        height: u16,
    ) -> std::io::Result<()> {
        use crossterm::cursor::MoveTo;

        let content = self.input_buffer.get_content();
        let content_width = width.saturating_sub(INPUT_MARGIN) as usize;

        // Recalculate cursor position for current width
        if content_width > 0 {
            self.input_state
                .calculate_cursor_pos(&self.input_buffer, content_width);
        }

        // Use same word-wrap algorithm as cursor calculation
        let visual_lines = build_visual_lines(&content, content_width);

        for (row, (start, end)) in visual_lines.iter().enumerate() {
            if row as u16 >= height {
                break;
            }

            // Extract chunk for this visual line (exclude trailing newline if present)
            let chunk: String = content
                .chars()
                .skip(*start)
                .take(end.saturating_sub(*start))
                .filter(|&c| c != '\n')
                .collect();

            execute!(w, MoveTo(0, start_row + row as u16))?;
            if row == 0 {
                write!(w, "{}{}", PROMPT, chunk)?;
            } else {
                write!(w, "{}{}", CONTINUATION, chunk)?;
            }
        }

        // If empty, just show the prompt
        if content.is_empty() {
            execute!(w, MoveTo(0, start_row))?;
            write!(w, "{}", PROMPT)?;
        }

        Ok(())
    }

    /// Render status line directly with crossterm.
    fn render_status_direct<W: std::io::Write>(
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
            ToolMode::Agi => ("AGI", CColor::Red),
        };

        write!(w, " [")?;
        execute!(
            w,
            SetForegroundColor(mode_color),
            Print(mode_label),
            ResetColor
        )?;
        write!(w, "] · {}", model_name)?;

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
                write!(w, " ({}%)", pct)?;
            }
            execute!(w, SetAttribute(Attribute::Reset))?;
        }

        Ok(())
    }
}

/// Draw a horizontal border line at the given row.
fn draw_horizontal_border<W: std::io::Write>(
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
