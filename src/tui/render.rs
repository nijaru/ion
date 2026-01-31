//! Rendering functions for the TUI.
//!
//! Terminal APIs use u16 for dimensions; numeric casts are intentional.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use crate::tui::App;
use crate::tui::chat_renderer::ChatRenderer;
use crate::tui::composer::build_visual_lines;
use crate::tui::message_list::Sender;
use crate::tui::terminal::StyledLine;
use crate::tui::types::{Mode, SelectorPage};
use crate::tui::util::{format_elapsed, format_relative_time, format_tokens};
use crossterm::execute;

/// Input prompt prefix " > "
const PROMPT: &str = " > ";
/// Continuation line prefix "   "
const CONTINUATION: &str = "   ";
/// Width of prompt/continuation prefix
const PROMPT_WIDTH: u16 = 3;
/// Total input margin (prompt + right padding)
const INPUT_MARGIN: u16 = 4;
/// Height of the progress bar area
const PROGRESS_HEIGHT: u16 = 1;
/// Selector layout overhead: tabs(1) + desc(1) + search box(3) + hint(1) + list header
const SELECTOR_OVERHEAD: u16 = 7;
/// Maximum visible items in selector list
const MAX_VISIBLE_ITEMS: u16 = 15;

/// A single item in the selector list.
struct SelectorItem {
    label: String,
    is_valid: bool,
    hint: String,
}

/// Data needed to render the selector UI.
struct SelectorData {
    title: &'static str,
    description: &'static str,
    items: Vec<SelectorItem>,
    selected_idx: usize,
    filter_text: String,
    show_tabs: bool,
    active_tab: usize, // 0 = providers, 1 = models
}

impl App {
    /// Calculate the height needed for the input box based on content.
    /// Returns height including borders.
    /// Min: 3 lines (1 content + 2 borders)
    /// Max: `viewport_height` - 3 (reserved for progress + status)
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
    /// For selector mode, returns height based on actual item count.
    pub fn calculate_ui_height(&self, width: u16, height: u16) -> u16 {
        if self.mode == Mode::Selector {
            let item_count = match self.selector_page {
                SelectorPage::Provider => self.provider_picker.filtered.len(),
                SelectorPage::Model => self.model_picker.filtered_models.len(),
                SelectorPage::Session => self.session_picker.filtered_sessions.len(),
            } as u16;

            // Show all items up to max, with minimum of 3 for usability
            let list_height = item_count.clamp(3, MAX_VISIBLE_ITEMS);
            let needed_height = SELECTOR_OVERHEAD + list_height;

            // Cap at screen height minus a few lines for context
            let max_height = height.saturating_sub(2);
            return needed_height.min(max_height);
        }

        let progress_height = PROGRESS_HEIGHT;
        let input_height = self.calculate_input_height(width, height);
        let status_height = 1u16;
        progress_height + input_height + status_height
    }

    /// Resolve the UI start row, using row tracking or startup anchor.
    pub fn ui_start_row(&self, height: u16, ui_height: u16) -> u16 {
        let bottom_start = height.saturating_sub(ui_height);

        // Row tracking mode: UI follows chat content
        if let Some(chat_row) = self.render_state.chat_row {
            return chat_row.min(bottom_start);
        }

        // Startup: use anchor when no messages exist
        if self.message_list.entries.is_empty()
            && let Some(anchor) = self.render_state.startup_ui_anchor
        {
            return anchor.min(bottom_start);
        }

        // Default: bottom of screen
        bottom_start
    }

    /// Take new chat entries and render them as lines for insertion.
    pub fn take_chat_inserts(&mut self, width: u16) -> Vec<StyledLine> {
        let wrap_width = width.saturating_sub(2);
        if wrap_width == 0 {
            return Vec::new();
        }

        // Insert header once at startup (into scrollback, not viewport)
        let header_lines = if self.render_state.header_inserted {
            Vec::new()
        } else {
            self.render_state.header_inserted = true;
            Self::startup_header_lines()
        };

        let entry_count = self.message_list.entries.len();
        if self.render_state.rendered_entries > entry_count {
            self.render_state.rendered_entries = 0;
            self.render_state.buffered_chat_lines.clear();
        }

        let mut new_lines = Vec::new();
        let mut index = self.render_state.rendered_entries;
        while index < entry_count {
            let entry = &self.message_list.entries[index];
            // Only skip the last entry if it's an Agent entry being actively streamed
            // This allows Tool entries and completed Agent responses to render mid-run
            let is_last = index == entry_count - 1;
            if entry.sender == Sender::Agent && self.is_running && is_last {
                break;
            }
            let mut entry_lines = ChatRenderer::build_lines(
                &self.message_list.entries[index..=index],
                None,
                wrap_width as usize,
            );
            new_lines.append(&mut entry_lines);
            index += 1;
        }
        self.render_state.rendered_entries = index;

        if self.mode == Mode::Selector {
            if !new_lines.is_empty() {
                self.render_state.buffered_chat_lines.extend(new_lines);
            }
            // Still return header if it needs to be inserted
            return header_lines;
        }

        if new_lines.is_empty()
            && self.render_state.buffered_chat_lines.is_empty()
            && header_lines.is_empty()
        {
            return Vec::new();
        }

        let mut out = header_lines;
        if !self.render_state.buffered_chat_lines.is_empty() {
            out.append(&mut self.render_state.buffered_chat_lines);
        }
        out.extend(new_lines);
        out
    }

    /// Build chat history lines for a given width.
    pub fn build_chat_lines(&self, width: u16) -> Vec<StyledLine> {
        let wrap_width = width.saturating_sub(2);
        if wrap_width == 0 {
            return Vec::new();
        }

        let mut lines = Vec::new();
        lines.extend(Self::startup_header_lines());

        let entry_count = self.message_list.entries.len();
        let mut end = entry_count;
        if self.is_running
            && let Some(last) = self.message_list.entries.last()
            && last.sender == Sender::Agent
        {
            end = end.saturating_sub(1);
        }
        if end > 0 {
            lines.extend(ChatRenderer::build_lines(
                &self.message_list.entries[..end],
                None,
                wrap_width as usize,
            ));
        }

        lines
    }

    /// Reprint full chat history into scrollback (used on resize reflow).
    pub fn reprint_chat_scrollback<W: std::io::Write>(
        &mut self,
        w: &mut W,
        width: u16,
    ) -> std::io::Result<()> {
        let entry_count = self.message_list.entries.len();
        let mut end = entry_count;
        if self.is_running
            && let Some(last) = self.message_list.entries.last()
            && last.sender == Sender::Agent
        {
            end = end.saturating_sub(1);
        }

        let lines = self.build_chat_lines(width);
        for line in &lines {
            line.write_to(w)?;
            write!(w, "\r\n")?;
        }

        self.render_state.mark_reflow_complete(end);

        Ok(())
    }

    /// Calculate the viewport height needed for the UI (progress + input + status).
    /// Header is inserted into scrollback, not rendered in viewport.
    /// Note: With full-height viewport, this is no longer used for viewport sizing,
    /// but may be useful for debugging or future use.
    #[allow(dead_code)]
    pub fn viewport_height(&self, terminal_width: u16, terminal_height: u16) -> u16 {
        let input_height = self.calculate_input_height(terminal_width, terminal_height);
        let progress_height = PROGRESS_HEIGHT;
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
        let ui_start = self.ui_start_row(height, ui_height);
        let progress_height = PROGRESS_HEIGHT;

        // Detect width decrease - terminal rewraps old content, pushing it up
        let width_decreased = self
            .render_state
            .last_render_width
            .is_some_and(|old| width < old);
        self.render_state.last_render_width = Some(width);

        // Determine clear_from based on positioning mode:
        // - Row-tracking: only clear UI area (chat is immediately above, must preserve)
        // - Scroll mode: clear from min(old, new) ui_start to handle UI shrinking
        //
        // Note: We read last_ui_start BEFORE updating it - the old value is needed
        // for the scroll mode comparison, then we store the new value for next frame.
        let in_row_tracking = self.render_state.chat_row.is_some();
        let old_ui_start = self.render_state.last_ui_start;
        self.render_state.last_ui_start = Some(ui_start);

        let clear_from = if in_row_tracking {
            ui_start
        } else {
            old_ui_start.map_or_else(
                || {
                    self.render_state
                        .startup_ui_anchor
                        .unwrap_or(ui_start)
                        .min(ui_start)
                },
                |old| old.min(ui_start),
            )
        };

        let preserve_header =
            self.message_list.entries.is_empty() && self.render_state.startup_ui_anchor.is_some();
        if width_decreased && !preserve_header {
            // Full clear needed: old wider borders got wrapped into multiple lines
            execute!(w, Clear(ClearType::All), MoveTo(0, ui_start))?;
        } else {
            // Clear from appropriate position
            execute!(w, MoveTo(0, clear_from), Clear(ClearType::FromCursorDown))?;
        }

        // Progress line (only when active)
        if progress_height > 0 {
            execute!(w, MoveTo(0, ui_start), Clear(ClearType::CurrentLine))?;
            self.render_progress_direct(w, width)?;
        }

        // Input area (with borders)
        let input_start = ui_start + progress_height;
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
        execute!(w, MoveTo(0, status_row), Clear(ClearType::CurrentLine))?;

        // In selector mode, render selector instead of normal input/status
        if self.mode == Mode::Selector {
            self.render_selector_direct(w, ui_start, width, height)?;
        } else {
            self.render_status_direct(w, width)?;

            // Render completer popup above input (mutually exclusive)
            if self.command_completer.is_active() {
                self.render_command_completer_direct(w, input_start, width)?;
            } else if self.file_completer.is_active() {
                self.render_file_completer_direct(w, input_start, width)?;
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

    /// Render file path completion popup above the input box.
    fn render_file_completer_direct<W: std::io::Write>(
        &self,
        w: &mut W,
        input_start: u16,
        width: u16,
    ) -> std::io::Result<()> {
        use crossterm::{
            cursor::MoveTo,
            style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
            terminal::{Clear, ClearType},
        };

        let candidates = self.file_completer.visible_candidates();
        if candidates.is_empty() {
            return Ok(());
        }

        let selected = self.file_completer.selected();
        let popup_height = candidates.len() as u16;

        // Position popup above input box
        // input_start is where the top border is, so popup goes above that
        let popup_start = input_start.saturating_sub(popup_height);

        // Calculate popup width (max path length + padding)
        let max_label_len = candidates
            .iter()
            .map(|p| p.to_string_lossy().len())
            .max()
            .unwrap_or(20);
        let popup_width = (max_label_len + 4).min(width as usize - 4) as u16;

        // Render each candidate
        for (i, path) in candidates.iter().enumerate() {
            let row = popup_start + i as u16;
            let is_selected = i == selected;
            let path_str = path.to_string_lossy();

            // Clear and position
            execute!(w, MoveTo(1, row), Clear(ClearType::CurrentLine))?;

            // Background highlight for selected item
            if is_selected {
                execute!(w, SetAttribute(Attribute::Reverse))?;
            }

            // Add icon for directories
            let working_dir = &self.session.working_dir;
            let is_dir = working_dir.join(path).is_dir();
            let icon = if is_dir { "󰉋 " } else { "  " };

            // Truncate path if needed
            let display_width = popup_width.saturating_sub(4) as usize;
            let display: String = if path_str.len() > display_width {
                format!(
                    "…{}",
                    &path_str[path_str.len().saturating_sub(display_width - 1)..]
                )
            } else {
                path_str.to_string()
            };

            execute!(
                w,
                Print(" "),
                SetForegroundColor(if is_dir { Color::Blue } else { Color::Reset }),
                Print(icon),
                ResetColor,
                Print(&display),
            )?;

            // Pad to popup width
            let padding = popup_width.saturating_sub(display.len() as u16 + 3);
            for _ in 0..padding {
                execute!(w, Print(" "))?;
            }

            if is_selected {
                execute!(w, SetAttribute(Attribute::NoReverse))?;
            }
        }

        Ok(())
    }

    /// Render command completion popup above the input box.
    fn render_command_completer_direct<W: std::io::Write>(
        &self,
        w: &mut W,
        input_start: u16,
        width: u16,
    ) -> std::io::Result<()> {
        use crossterm::{
            cursor::MoveTo,
            style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
            terminal::{Clear, ClearType},
        };

        let candidates = self.command_completer.visible_candidates();
        if candidates.is_empty() {
            return Ok(());
        }

        let selected = self.command_completer.selected();
        let popup_height = candidates.len() as u16;

        // Position popup above input box
        let popup_start = input_start.saturating_sub(popup_height);

        // Calculate popup width (command + description + padding)
        let max_cmd_len = candidates
            .iter()
            .map(|(cmd, _)| cmd.len())
            .max()
            .unwrap_or(10);
        let max_desc_len = candidates
            .iter()
            .map(|(_, desc)| desc.len())
            .max()
            .unwrap_or(20);
        let popup_width = (max_cmd_len + max_desc_len + 6).min(width as usize - 4) as u16;

        // Render each candidate
        for (i, (cmd, desc)) in candidates.iter().enumerate() {
            let row = popup_start + i as u16;
            let is_selected = i == selected;

            // Clear and position
            execute!(w, MoveTo(1, row), Clear(ClearType::CurrentLine))?;

            // Background highlight for selected item
            if is_selected {
                execute!(w, SetAttribute(Attribute::Reverse))?;
            }

            // Command in cyan, description dimmed
            execute!(
                w,
                Print(" "),
                SetForegroundColor(Color::Cyan),
                Print(*cmd),
                ResetColor,
            )?;

            // Pad between command and description
            let cmd_padding = max_cmd_len.saturating_sub(cmd.len()) + 2;
            for _ in 0..cmd_padding {
                execute!(w, Print(" "))?;
            }

            // Description (dimmed)
            execute!(
                w,
                SetAttribute(Attribute::Dim),
                Print(*desc),
                SetAttribute(Attribute::NormalIntensity),
            )?;

            // Pad to popup width
            let total_len = cmd.len() + cmd_padding + desc.len() + 1;
            let padding = popup_width.saturating_sub(total_len as u16);
            for _ in 0..padding {
                execute!(w, Print(" "))?;
            }

            if is_selected {
                execute!(w, SetAttribute(Attribute::NoReverse))?;
            }
        }

        Ok(())
    }

    /// Extract data needed to render the current selector page.
    fn selector_data(&self) -> SelectorData {
        match self.selector_page {
            SelectorPage::Provider => {
                let items = self
                    .provider_picker
                    .filtered
                    .iter()
                    .map(|s| {
                        let hint = if s.authenticated {
                            String::new()
                        } else {
                            s.provider.auth_hint().to_string()
                        };
                        SelectorItem {
                            label: s.provider.name().to_string(),
                            is_valid: s.authenticated,
                            hint,
                        }
                    })
                    .collect();
                SelectorData {
                    title: "Providers",
                    description: "Select a provider",
                    items,
                    selected_idx: self.provider_picker.list_state.selected().unwrap_or(0),
                    filter_text: self.provider_picker.filter_input.text().to_string(),
                    show_tabs: true,
                    active_tab: 0,
                }
            }
            SelectorPage::Model => {
                let items = self
                    .model_picker
                    .filtered_models
                    .iter()
                    .map(|m| SelectorItem {
                        label: m.id.clone(),
                        is_valid: true,
                        hint: String::new(),
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
                    .filtered_sessions
                    .iter()
                    .map(|s| {
                        let preview = s.first_user_message.as_ref().map_or_else(
                            || "No preview".to_string(),
                            |m| m.chars().take(40).collect::<String>(),
                        );
                        let label = format!("{} - {}", preview, format_relative_time(s.updated_at));
                        SelectorItem {
                            label,
                            is_valid: true,
                            hint: String::new(),
                        }
                    })
                    .collect();
                SelectorData {
                    title: "Sessions",
                    description: "Select a session to resume",
                    items,
                    selected_idx: self.session_picker.list_state.selected().unwrap_or(0),
                    filter_text: self.session_picker.filter_input.text().to_string(),
                    show_tabs: false,
                    active_tab: 0,
                }
            }
        }
    }

    /// Render selector (provider/model/session picker) directly with crossterm.
    fn render_selector_direct<W: std::io::Write>(
        &mut self,
        w: &mut W,
        start_row: u16,
        width: u16,
        _height: u16,
    ) -> std::io::Result<()> {
        use crossterm::{
            cursor::MoveTo,
            style::{
                Attribute, Color as CColor, Print, ResetColor, SetAttribute, SetForegroundColor,
            },
        };

        let data = self.selector_data();

        // Layout: tabs, description, search box, list, hint
        // Clear from start_row to bottom
        execute!(
            w,
            MoveTo(0, start_row),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::FromCursorDown)
        )?;

        let mut row = start_row;

        // Tab bar (only for provider/model, session has its own header)
        if data.show_tabs {
            let provider_bold = data.active_tab == 0;
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
            if !provider_bold {
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
        } else {
            execute!(w, MoveTo(0, row))?;
            execute!(
                w,
                SetForegroundColor(CColor::Yellow),
                SetAttribute(Attribute::Bold),
                Print(" "),
                Print(data.title),
                SetAttribute(Attribute::Reset),
                ResetColor
            )?;
        }
        row += 1;

        // Description
        execute!(w, MoveTo(0, row))?;
        write!(w, " {}", data.description)?;
        row += 1;

        // Search box
        execute!(
            w,
            MoveTo(0, row),
            SetForegroundColor(CColor::Cyan),
            Print("┌─ "),
            Print(data.title),
            Print(" "),
            Print("─".repeat((width as usize).saturating_sub(data.title.len() + 5))),
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
            Print(&data.filter_text),
        )?;
        // Save cursor position for filter input
        let filter_cursor_col = 2 + data.filter_text.len() as u16;
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

        // Render list items
        row = render_selector_list(w, &data, row)?;

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
        if self.is_running {
            self.render_progress_running(w)
        } else {
            self.render_progress_completed(w)
        }
    }

    /// Render progress line when a task is running (spinner + tool name + elapsed).
    fn render_progress_running<W: std::io::Write>(&self, w: &mut W) -> std::io::Result<()> {
        use crossterm::style::{
            Attribute, Color as CColor, Print, ResetColor, SetAttribute, SetForegroundColor,
        };

        const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let frame = (self.frame_count % SPINNER.len() as u64) as usize;

        execute!(
            w,
            Print(" "),
            SetForegroundColor(CColor::Cyan),
            Print(SPINNER[frame]),
            ResetColor
        )?;

        execute!(w, SetForegroundColor(CColor::Cyan))?;
        if let Some(ref tool) = self.current_tool {
            execute!(w, Print(format!(" {tool}")))?;
        } else {
            execute!(w, Print(" Ionizing..."))?;
        }
        execute!(w, ResetColor)?;

        if let Some(start) = self.task_start_time {
            let elapsed = start.elapsed().as_secs();
            execute!(
                w,
                SetAttribute(Attribute::Dim),
                Print(format!(" ({elapsed}s · Esc to cancel)")),
                SetAttribute(Attribute::Reset)
            )?;
        }

        Ok(())
    }

    /// Render progress line after task completion (status + stats summary).
    fn render_progress_completed<W: std::io::Write>(&self, w: &mut W) -> std::io::Result<()> {
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
            Print(format!(" ({})", stats.join(" · "))),
            SetAttribute(Attribute::Reset)
        )?;

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
        let total_lines = self
            .input_state
            .visual_line_count(&self.input_buffer, content_width);
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
        write!(w, "] · {model_name}")?;

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

/// Render the selector item list with scrolling. Returns the next row after the list.
fn render_selector_list<W: std::io::Write>(
    w: &mut W,
    data: &SelectorData,
    start_row: u16,
) -> std::io::Result<u16> {
    use crossterm::{
        cursor::MoveTo,
        style::{Attribute, Color as CColor, Print, ResetColor, SetAttribute, SetForegroundColor},
    };

    let list_height = (data.items.len() as u16).clamp(3, MAX_VISIBLE_ITEMS);

    // Calculate scroll offset to keep selection visible
    let scroll_offset = if data.selected_idx >= list_height as usize {
        data.selected_idx.saturating_sub(list_height as usize - 1)
    } else {
        0
    };

    // Calculate max label length for column alignment (visible items only)
    let visible_items: Vec<_> = data
        .items
        .iter()
        .skip(scroll_offset)
        .take(list_height as usize)
        .collect();
    let max_label_len = visible_items
        .iter()
        .map(|item| item.label.chars().count())
        .max()
        .unwrap_or(0);

    let mut row = start_row;
    for (i, item) in visible_items.into_iter().enumerate() {
        execute!(w, MoveTo(0, row))?;
        let actual_idx = scroll_offset + i;
        let is_selected = actual_idx == data.selected_idx;

        // Selection indicator
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

        // Validity indicator
        if item.is_valid {
            execute!(w, SetForegroundColor(CColor::Green), Print(" ● "))?;
        } else {
            execute!(w, SetAttribute(Attribute::Dim), Print(" ○ "))?;
        }

        // Label styling
        if is_selected {
            execute!(
                w,
                SetForegroundColor(CColor::Yellow),
                SetAttribute(Attribute::Bold)
            )?;
        } else if !item.is_valid {
            execute!(w, SetAttribute(Attribute::Dim))?;
        }

        // Label with padding for column alignment
        let label_len = item.label.chars().count();
        let padding = max_label_len.saturating_sub(label_len);
        write!(w, "{}", item.label)?;
        execute!(w, SetAttribute(Attribute::Reset), ResetColor)?;

        // Auth hint in second column (dimmed)
        if !item.hint.is_empty() {
            write!(w, "{:padding$}  ", "", padding = padding)?;
            execute!(w, SetAttribute(Attribute::Dim))?;
            write!(w, "{}", item.hint)?;
            execute!(w, SetAttribute(Attribute::Reset))?;
        }

        row += 1;
    }

    Ok(row)
}
