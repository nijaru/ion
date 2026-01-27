//! Rendering functions for the TUI.

use crate::tool::ToolMode;
use crate::tui::chat_renderer::ChatRenderer;
use crate::tui::composer::ComposerWidget;
use crate::tui::filter_input::FilterInput;
use crate::tui::message_list::Sender;
use crate::tui::types::{LayoutAreas, Mode, SelectorPage};
use crate::tui::util::{format_elapsed, format_relative_time, format_tokens};
use crate::tui::App;
use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph};
use ratatui::Terminal;
use std::path::PathBuf;

impl App {
    /// Take a snapshot of the current TUI state for debugging.
    pub fn take_snapshot(&mut self) {
        let area = Rect::new(0, 0, 120, 40); // Standard "debug" terminal size
        let backend = ratatui::backend::TestBackend::new(area.width, area.height);
        let mut terminal = match Terminal::new(backend) {
            Ok(term) => term,
            Err(err) => match err {},
        };
        if terminal.draw(|f| self.draw(f)).is_err() {
            return;
        }

        let buffer = terminal.backend().buffer();
        let mut snapshot = String::new();

        for y in 0..area.height {
            for x in 0..area.width {
                let cell = &buffer[(x, y)];
                snapshot.push_str(cell.symbol());
            }
            snapshot.push('\n');
        }

        let path = PathBuf::from("ai/tmp/tui_snapshot.txt");
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, snapshot);
    }

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

    /// Take new chat entries and render them as lines for insertion.
    pub fn take_chat_inserts(&mut self, width: u16) -> Vec<Line<'static>> {
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

    /// Calculate layout areas for progress, input, and status.
    pub(super) fn layout_areas(
        &self,
        area: Rect,
        input_height: u16,
        progress_height: u16,
    ) -> LayoutAreas {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(progress_height),
                Constraint::Length(input_height),
                Constraint::Length(1),
            ])
            .split(area);

        LayoutAreas {
            progress: chunks[0],
            input: chunks[1],
            status: chunks[2],
        }
    }

    /// Render the progress line (spinner during task, summary after completion).
    pub(super) fn render_progress(&self, frame: &mut Frame, progress_area: Rect) {
        if progress_area.height == 0 {
            return;
        }

        if self.is_running {
            let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

            let (symbol, label, color, dim) = if self.session.abort_token.is_cancelled() {
                ("⚠", "Canceling...".to_string(), Color::Yellow, false)
            } else if let Some((reason, delay)) = &self.retry_status {
                // Retry in progress - show with dim yellow
                let s = spinner[(self.frame_count % spinner.len() as u64) as usize];
                (
                    s,
                    format!("{}, retrying in {}s...", reason, delay),
                    Color::Yellow,
                    true,
                )
            } else {
                let s = spinner[(self.frame_count % spinner.len() as u64) as usize];
                let label = if let Some(tool) = &self.current_tool {
                    format!("Running {}...", tool)
                } else {
                    "Ionizing...".to_string()
                };
                (s, label, Color::Cyan, false)
            };

            let style = if dim {
                Style::default().fg(color).dim()
            } else {
                Style::default().fg(color)
            };
            let mut progress_spans = vec![
                Span::styled(format!(" {} ", symbol), style),
                Span::styled(label, style),
            ];

            let mut stats = Vec::new();
            if let Some(start) = self.task_start_time {
                stats.push(format_elapsed(start.elapsed().as_secs()));
            }

            if self.input_tokens > 0 {
                stats.push(format!("↑ {}", format_tokens(self.input_tokens)));
            }
            if self.output_tokens > 0 {
                stats.push(format!("↓ {}", format_tokens(self.output_tokens)));
            }

            // Thinking indicator
            if self.thinking_start.is_some() {
                stats.push("thinking".to_string());
            } else if let Some(duration) = self.last_thinking_duration {
                let secs = duration.as_secs();
                stats.push(format!("thought for {}s", secs));
            }

            if !stats.is_empty() {
                progress_spans.push(Span::styled(
                    format!(" ({} · ", stats.join(" · ")),
                    Style::default().dim(),
                ));
                progress_spans.push(Span::styled("Esc", Style::default().dim().italic()));
                progress_spans.push(Span::styled(" to cancel)", Style::default().dim()));
            } else {
                progress_spans.push(Span::styled(" (", Style::default().dim()));
                progress_spans.push(Span::styled("Esc", Style::default().dim().italic()));
                progress_spans.push(Span::styled(" to cancel)", Style::default().dim()));
            }

            let progress_line = Line::from(progress_spans);

            // If we have extra height, show queued messages on first line
            if progress_area.height > 1 {
                let queued_count = self
                    .message_queue
                    .as_ref()
                    .and_then(|q| q.lock().ok())
                    .map(|g| g.len())
                    .unwrap_or(0);

                let queued_line = if queued_count > 0 {
                    Line::from(vec![
                        Span::styled(" ↳ ", Style::default().fg(Color::Yellow)),
                        Span::styled(
                            format!(
                                "{} message{} queued",
                                queued_count,
                                if queued_count == 1 { "" } else { "s" }
                            ),
                            Style::default().fg(Color::Yellow).dim(),
                        ),
                    ])
                } else {
                    Line::from("") // Empty line for visual separation
                };
                frame.render_widget(
                    Paragraph::new(vec![queued_line, progress_line]),
                    progress_area,
                );
            } else {
                frame.render_widget(Paragraph::new(vec![progress_line]), progress_area);
            }
        } else if let Some(summary) = &self.last_task_summary {
            let mut stats = vec![format_elapsed(summary.elapsed.as_secs())];
            if summary.input_tokens > 0 {
                stats.push(format!("↑ {}", format_tokens(summary.input_tokens)));
            }
            if summary.output_tokens > 0 {
                stats.push(format!("↓ {}", format_tokens(summary.output_tokens)));
            }

            let (symbol, label, color) = if self.last_error.is_some() {
                (" ✗ ", "Error", Color::Red)
            } else if summary.was_cancelled {
                (" ⚠ ", "Canceled", Color::Yellow)
            } else {
                (" ✓ ", "Completed", Color::Green)
            };

            let summary_line = Line::from(vec![
                Span::styled(symbol, Style::default().fg(color)),
                Span::styled(label, Style::default().fg(color)),
                Span::styled(format!(" ({})", stats.join(" · ")), Style::default().dim()),
            ]);
            frame.render_widget(Paragraph::new(vec![summary_line]), progress_area);
        }
    }

    /// Render input box or approval prompt.
    pub(super) fn render_input_or_approval(&mut self, frame: &mut Frame, input_area: Rect) {
        if self.mode == Mode::Approval {
            if let Some(req) = &self.pending_approval {
                let prompt = format!(
                    " [Approval] Allow {}? (y)es / (n)o / (a)lways / (A)lways permanent ",
                    req.tool_name
                );
                let approval_block = Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Red).bold())
                    .title(" Action Required ");
                let approval_para = Paragraph::new(prompt).block(approval_block);
                frame.render_widget(approval_para, input_area);
            }
            return;
        }

        if input_area.width > 0 && input_area.height > 1 {
            let block = Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::Cyan));
            frame.render_widget(&block, input_area);
            let text_area = block.inner(input_area);
            self.render_input_text(frame, text_area);
        }

        // Set cursor position from ComposerState
        frame.set_cursor_position(self.input_state.cursor_pos);
    }

    /// Render the input text with gutter.
    fn render_input_text(&mut self, frame: &mut Frame, text_area: Rect) {
        if text_area.width == 0 || text_area.height == 0 {
            return;
        }

        // ComposerWidget handles gutter rendering internally
        let composer =
            ComposerWidget::new(&self.input_buffer, &mut self.input_state).show_gutter(true);
        frame.render_widget(composer, text_area);
    }

    /// Render the status line.
    pub(super) fn render_status_line(&self, frame: &mut Frame, status_area: Rect) {
        let model_name = self
            .session
            .model
            .split('/')
            .next_back()
            .unwrap_or(&self.session.model);

        let mode_label = match self.tool_mode {
            ToolMode::Read => "READ",
            ToolMode::Write => "WRITE",
            ToolMode::Agi => "AGI",
        };
        let mode_color = match self.tool_mode {
            ToolMode::Read => Color::Cyan,
            ToolMode::Write => Color::Yellow,
            ToolMode::Agi => Color::Red,
        };

        let mut left_spans: Vec<Span> = vec![
            Span::raw(" "),
            Span::raw("["),
            Span::styled(mode_label, Style::default().fg(mode_color)),
            Span::raw("]"),
            Span::raw(" · "),
            Span::raw(model_name),
        ];

        if let Some((used, max)) = self.token_usage {
            let format_k = |n: usize| -> String {
                if n >= 1000 {
                    format!("{}k", n / 1000)
                } else {
                    n.to_string()
                }
            };
            // Prefer model metadata (most accurate), fall back to compaction config
            let context_max = match self.model_context_window {
                Some(m) if m > 0 => m,
                _ => max,
            };
            left_spans.push(Span::raw(" · "));
            if context_max > 0 {
                let pct = used.saturating_mul(100) / context_max;
                left_spans.push(Span::raw(format!(
                    "{}% ({}/{})",
                    pct,
                    format_k(used),
                    format_k(context_max)
                )));
            } else {
                // Unknown max - show just the used count
                left_spans.push(Span::raw(format!("({}/?)", format_k(used))));
            }
        }

        let left_len: usize = left_spans.iter().map(|s| s.content.chars().count()).sum();
        let (right, right_style) = ("? help ".to_string(), Style::default().dim());
        let width = status_area.width as usize;
        let right_len = right.chars().count();
        let padding = width.saturating_sub(left_len + right_len);

        let mut status_spans = left_spans;
        status_spans.push(Span::raw(" ".repeat(padding)));
        status_spans.push(Span::styled(right, right_style));
        let status_line = Line::from(status_spans);

        frame.render_widget(Paragraph::new(status_line), status_area);
    }

    /// Main draw function for the TUI.
    pub fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();

        // Clear the entire viewport first
        frame.render_widget(Clear, area);

        let input_height = self.calculate_input_height(area.width, area.height);

        let progress_height = if self.is_running {
            2 // Line 1: gap or queued indicator, Line 2: spinner
        } else if self.last_task_summary.is_some() {
            1
        } else {
            0
        };

        // UI fills the entire viewport (chat content is above via insert_before)
        // Clamp UI height to available viewport space
        let ui_height = (progress_height + input_height + 1).min(area.height); // +1 for status line
        let ui_area = Rect::new(area.x, area.y, area.width, ui_height);

        let areas = self.layout_areas(ui_area, input_height, progress_height);

        self.render_progress(frame, areas.progress);

        if self.mode == Mode::Selector {
            self.render_selector_shell(frame);
        } else {
            self.render_input_or_approval(frame, areas.input);
            self.render_status_line(frame, areas.status);
        }

        // Render overlays on top if active
        if self.mode == Mode::HelpOverlay {
            self.render_help_overlay(frame);
        }
    }

    /// Render the selector shell (provider/model/session picker).
    pub(super) fn render_selector_shell(&mut self, frame: &mut Frame) {
        let area = frame.area();

        let (title, description, list_len) = match self.selector_page {
            SelectorPage::Provider => (
                "Providers",
                "Select a provider",
                self.provider_picker.filtered.len(),
            ),
            SelectorPage::Model => (
                "Models",
                "Select a model",
                self.model_picker.filtered_models.len(),
            ),
            SelectorPage::Session => (
                "Sessions",
                "Select a session to resume",
                self.session_picker.filtered_sessions.len(),
            ),
        };

        let reserved_height = 1 + 1 + 3 + 1;
        let max_list_height = area.height.saturating_sub(reserved_height);
        let list_height = (list_len as u16).clamp(3, max_list_height.max(3));
        let total_height = reserved_height + list_height;

        let y = area.height.saturating_sub(total_height);
        let shell_area = Rect::new(area.x, area.y + y, area.width, total_height);

        frame.render_widget(Clear, shell_area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // Tabs
                Constraint::Length(1), // Description
                Constraint::Length(3), // Search
                Constraint::Length(list_height),
                Constraint::Length(1), // Hint
            ])
            .split(shell_area);

        // Session picker has its own header, Provider/Model share a tab bar
        if self.selector_page == SelectorPage::Session {
            let tabs = Line::from(vec![
                Span::raw(" "),
                Span::styled("Sessions", Style::default().fg(Color::Yellow).bold()),
            ]);
            frame.render_widget(Paragraph::new(tabs), chunks[0]);
        } else {
            let (provider_style, model_style) = match self.selector_page {
                SelectorPage::Provider => (
                    Style::default().fg(Color::Yellow).bold(),
                    Style::default().dim(),
                ),
                SelectorPage::Model | SelectorPage::Session => (
                    Style::default().dim(),
                    Style::default().fg(Color::Yellow).bold(),
                ),
            };

            let tabs = Line::from(vec![
                Span::raw(" "),
                Span::styled("Providers", provider_style),
                Span::raw("  "),
                Span::styled("Models", model_style),
            ]);
            frame.render_widget(Paragraph::new(tabs), chunks[0]);
        }

        frame.render_widget(
            Paragraph::new(Line::from(vec![Span::raw(" "), Span::raw(description)])),
            chunks[1],
        );

        let search_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(format!(" {} ", title));

        let search_input = FilterInput::new().block(search_block);
        match self.selector_page {
            SelectorPage::Provider => {
                frame.render_stateful_widget(
                    search_input,
                    chunks[2],
                    &mut self.provider_picker.filter_input,
                );
                if let Some(cursor) = self.provider_picker.filter_input.screen_cursor() {
                    frame.set_cursor_position(cursor);
                }
            }
            SelectorPage::Model => {
                frame.render_stateful_widget(
                    search_input,
                    chunks[2],
                    &mut self.model_picker.filter_input,
                );
                if let Some(cursor) = self.model_picker.filter_input.screen_cursor() {
                    frame.set_cursor_position(cursor);
                }
            }
            SelectorPage::Session => {
                frame.render_stateful_widget(
                    search_input,
                    chunks[2],
                    &mut self.session_picker.filter_input,
                );
                if let Some(cursor) = self.session_picker.filter_input.screen_cursor() {
                    frame.set_cursor_position(cursor);
                }
            }
        }

        match self.selector_page {
            SelectorPage::Provider => {
                let items: Vec<ListItem> = self
                    .provider_picker
                    .filtered
                    .iter()
                    .map(|status| {
                        let (icon, icon_style, name_style) = if status.authenticated {
                            (
                                "●",
                                Style::default().fg(Color::Green),
                                Style::default().fg(Color::White).bold(),
                            )
                        } else {
                            ("○", Style::default().dim(), Style::default().dim())
                        };

                        let auth_hint = if !status.authenticated {
                            format!(
                                " set {}",
                                status.provider.env_vars().first().unwrap_or(&"API_KEY")
                            )
                        } else {
                            String::new()
                        };

                        ListItem::new(Line::from(vec![
                            Span::styled(icon, icon_style),
                            Span::raw(" "),
                            Span::styled(status.provider.name(), name_style),
                            Span::styled(auth_hint, Style::default().fg(Color::Red).dim()),
                        ]))
                    })
                    .collect();

                let count = self.provider_picker.filtered.len();
                let total = self.provider_picker.providers.len();
                let list = List::new(items)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(format!(" Providers ({}/{}) ", count, total)),
                    )
                    .highlight_style(
                        Style::default()
                            .bg(Color::DarkGray)
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    )
                    .highlight_symbol("▸ ");

                frame.render_stateful_widget(list, chunks[3], &mut self.provider_picker.list_state);
            }
            SelectorPage::Model => {
                if self.model_picker.is_loading {
                    let provider_name = self
                        .model_picker
                        .api_provider_name
                        .as_deref()
                        .unwrap_or("provider");
                    let loading =
                        Paragraph::new(format!("Loading models from {}...", provider_name))
                            .style(Style::default().fg(Color::Yellow))
                            .block(Block::default().borders(Borders::ALL).title(" Loading "));
                    frame.render_widget(loading, chunks[3]);
                } else if let Some(ref err) = self.model_picker.error {
                    let error = Paragraph::new(format!("Error: {}", err))
                        .style(Style::default().fg(Color::Red))
                        .block(Block::default().borders(Borders::ALL).title(" Error "));
                    frame.render_widget(error, chunks[3]);
                } else {
                    let items: Vec<ListItem> = self
                        .model_picker
                        .filtered_models
                        .iter()
                        .map(|model| {
                            let context_k = model.context_window / 1000;
                            ListItem::new(Line::from(vec![
                                Span::styled(model.id.clone(), Style::default().fg(Color::White)),
                                Span::styled(
                                    format!("  {}k ctx", context_k),
                                    Style::default().dim(),
                                ),
                            ]))
                        })
                        .collect();

                    let count = self.model_picker.filtered_models.len();
                    let total = self.model_picker.all_models.len();
                    let list = List::new(items)
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .title(format!(" Models ({}/{}) ", count, total)),
                        )
                        .highlight_style(
                            Style::default()
                                .bg(Color::DarkGray)
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD),
                        )
                        .highlight_symbol("▸ ");

                    frame.render_stateful_widget(
                        list,
                        chunks[3],
                        &mut self.model_picker.model_state,
                    );
                }
            }
            SelectorPage::Session => {
                if self.session_picker.is_loading {
                    let loading = Paragraph::new("Loading sessions...")
                        .style(Style::default().fg(Color::Yellow))
                        .block(Block::default().borders(Borders::ALL).title(" Loading "));
                    frame.render_widget(loading, chunks[3]);
                } else if let Some(ref err) = self.session_picker.error {
                    let error = Paragraph::new(format!("Error: {}", err))
                        .style(Style::default().fg(Color::Red))
                        .block(Block::default().borders(Borders::ALL).title(" Error "));
                    frame.render_widget(error, chunks[3]);
                } else if self.session_picker.sessions.is_empty() {
                    let empty = Paragraph::new("No sessions found")
                        .style(Style::default().fg(Color::DarkGray))
                        .block(Block::default().borders(Borders::ALL).title(" Sessions "));
                    frame.render_widget(empty, chunks[3]);
                } else {
                    let items: Vec<ListItem> = self
                        .session_picker
                        .filtered_sessions
                        .iter()
                        .map(|s| {
                            // Format relative time
                            let time_str = format_relative_time(s.updated_at);
                            // Short preview
                            let preview = s
                                .first_user_message
                                .as_ref()
                                .map(|m| {
                                    let truncated: String = m.chars().take(40).collect();
                                    if m.chars().count() > 40 {
                                        format!("{}...", truncated)
                                    } else {
                                        truncated
                                    }
                                })
                                .unwrap_or_else(|| "(no messages)".to_string());
                            // Short ID
                            let short_id: String = s.id.chars().take(8).collect();

                            ListItem::new(Line::from(vec![
                                Span::styled(
                                    format!("{:>8}", time_str),
                                    Style::default().fg(Color::Blue),
                                ),
                                Span::raw("  "),
                                Span::styled(short_id, Style::default().fg(Color::DarkGray)),
                                Span::raw("  "),
                                Span::styled(preview, Style::default().fg(Color::White)),
                            ]))
                        })
                        .collect();

                    let count = self.session_picker.filtered_sessions.len();
                    let total = self.session_picker.sessions.len();
                    let list = List::new(items)
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .title(format!(" Sessions ({}/{}) ", count, total)),
                        )
                        .highlight_style(
                            Style::default()
                                .bg(Color::DarkGray)
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD),
                        )
                        .highlight_symbol("> ");

                    frame.render_stateful_widget(
                        list,
                        chunks[3],
                        &mut self.session_picker.list_state,
                    );
                }
            }
        }

        let hint = Paragraph::new(" Type to filter · Enter to select · Esc to close ")
            .style(Style::default().dim());
        frame.render_widget(hint, chunks[4]);
    }

    /// Render the help overlay.
    pub(super) fn render_help_overlay(&self, frame: &mut Frame) {
        let area = frame.area();
        // Fixed size modal, centered (40 inner width for clean columns)
        let width = 44.min(area.width.saturating_sub(4));
        let height = 24.min(area.height.saturating_sub(4));
        let x = (area.width.saturating_sub(width)) / 2;
        let y = (area.height.saturating_sub(height)) / 2;
        let help_area = Rect::new(area.x + x, area.y + y, width, height);

        frame.render_widget(ratatui::widgets::Clear, help_area);

        // Helper to create a row: key (col 1-18), description (col 19+)
        let row = |key: &str, desc: &str| {
            Line::from(vec![
                Span::styled(format!(" {:<17}", key), Style::default().fg(Color::Cyan)),
                Span::raw(desc.to_string()),
            ])
        };

        let help_text = vec![
            Line::from(Span::styled(
                "Keybindings",
                Style::default().fg(Color::Yellow).bold(),
            ))
            .alignment(ratatui::layout::Alignment::Center),
            row("Enter", "Send message"),
            row("Shift+Enter", "Insert newline"),
            row("Shift+Tab", "Cycle mode"),
            row("Ctrl+G", "External editor"),
            row("Ctrl+M", "Model selector"),
            row("Ctrl+P", "Provider selector"),
            row("Ctrl+T", "Thinking toggle"),
            row("Esc", "Cancel agent"),
            row("Ctrl+C", "Clear (double-tap quit)"),
            row("PgUp/PgDn", "Scroll chat"),
            Line::from(""),
            Line::from(Span::styled(
                "Commands",
                Style::default().fg(Color::Yellow).bold(),
            ))
            .alignment(ratatui::layout::Alignment::Center),
            row("/model", "Select model"),
            row("/provider", "Select provider"),
            row("/resume", "Resume session"),
            row("/clear", "Clear chat"),
            row("/quit", "Exit"),
            Line::from(""),
            Line::from(Span::styled(
                "Press any key to close",
                Style::default().dim(),
            ))
            .alignment(ratatui::layout::Alignment::Center),
        ];

        let help_para = Paragraph::new(help_text)
            .block(Block::default().borders(Borders::ALL).title(" ? Help "));

        frame.render_widget(help_para, help_area);
    }
}
