//! Session picker for resuming previous sessions.

use crate::session::{SessionStore, SessionSummary};
use crate::tui::filter_input::FilterInputState;
use crate::tui::fuzzy;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

/// State for the session picker modal.
#[derive(Default)]
pub struct SessionPicker {
    /// All available sessions.
    pub sessions: Vec<SessionSummary>,
    /// Filtered sessions based on search.
    pub filtered_sessions: Vec<SessionSummary>,
    /// Filter input state.
    pub filter_input: FilterInputState,
    /// List state.
    pub list_state: ListState,
    /// Loading state.
    pub is_loading: bool,
    /// Error message if load failed.
    pub error: Option<String>,
}


impl SessionPicker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset picker state.
    pub fn reset(&mut self) {
        self.filter_input.clear();
        self.apply_filter();
    }

    /// Load sessions from store.
    pub fn load_sessions(&mut self, store: &SessionStore, limit: usize) {
        self.is_loading = true;
        match store.list_recent(limit) {
            Ok(sessions) => {
                self.sessions = sessions;
                self.is_loading = false;
                self.error = None;
                self.apply_filter();
            }
            Err(e) => {
                self.error = Some(e.to_string());
                self.is_loading = false;
            }
        }
    }

    /// Check if we have sessions loaded.
    pub fn has_sessions(&self) -> bool {
        !self.sessions.is_empty()
    }

    /// Apply filter to session list.
    pub fn apply_filter(&mut self) {
        let filter = self.filter_input.text();

        if filter.is_empty() {
            self.filtered_sessions = self.sessions.clone();
        } else {
            // Build candidate strings for fuzzy matching
            let candidates: Vec<String> = self
                .sessions
                .iter()
                .map(|s| {
                    format!(
                        "{} {} {}",
                        s.id,
                        s.first_user_message.as_deref().unwrap_or(""),
                        s.working_dir
                    )
                })
                .collect();

            let candidate_refs: Vec<&str> = candidates.iter().map(|s| s.as_str()).collect();
            let matches = fuzzy::top_matches(filter, candidate_refs.iter().copied(), 50);

            // Map matches back to sessions
            self.filtered_sessions = matches
                .iter()
                .filter_map(|matched| {
                    candidates
                        .iter()
                        .position(|c| c.as_str() == *matched)
                        .map(|idx| self.sessions[idx].clone())
                })
                .collect();
        }

        if !self.filtered_sessions.is_empty() {
            self.list_state.select(Some(0));
        } else {
            self.list_state.select(None);
        }
    }

    /// Move selection up.
    pub fn move_up(&mut self, count: usize) {
        if self.filtered_sessions.is_empty() {
            return;
        }
        let i = self.list_state.selected().unwrap_or(0);
        let new_i = i.saturating_sub(count);
        self.list_state.select(Some(new_i));
    }

    /// Move selection down.
    pub fn move_down(&mut self, count: usize) {
        if self.filtered_sessions.is_empty() {
            return;
        }
        let len = self.filtered_sessions.len();
        let i = self.list_state.selected().unwrap_or(0);
        let new_i = (i + count).min(len - 1);
        self.list_state.select(Some(new_i));
    }

    /// Jump to top.
    pub fn jump_to_top(&mut self) {
        if !self.filtered_sessions.is_empty() {
            self.list_state.select(Some(0));
        }
    }

    /// Jump to bottom.
    pub fn jump_to_bottom(&mut self) {
        if !self.filtered_sessions.is_empty() {
            self.list_state
                .select(Some(self.filtered_sessions.len() - 1));
        }
    }

    /// Get currently selected session.
    pub fn selected_session(&self) -> Option<&SessionSummary> {
        self.list_state
            .selected()
            .and_then(|i| self.filtered_sessions.get(i))
    }

    /// Render the picker modal.
    pub fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();

        // Modal dimensions
        let content_width = 80u16;
        let list_len = self.filtered_sessions.len();
        let list_height = (list_len as u16 + 2).clamp(5, 20);
        let total_height = 3 + list_height; // search bar + list

        let modal_width = content_width.min(area.width.saturating_sub(4));
        let modal_height = total_height.min(area.height.saturating_sub(4));
        let x = (area.width - modal_width) / 2;
        let y = (area.height - modal_height) / 2;
        let modal_area = Rect::new(x, y, modal_width, modal_height);

        // Clear the background
        frame.render_widget(Clear, modal_area);

        // Split into search bar + list
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(0)])
            .split(modal_area);

        // Search input
        use crate::tui::filter_input::FilterInput;
        let search_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Filter (type to search) ");

        let search_input = FilterInput::new().block(search_block);
        frame.render_stateful_widget(search_input, chunks[0], &mut self.filter_input);
        if let Some(cursor) = self.filter_input.screen_cursor() {
            frame.set_cursor_position(cursor);
        }

        // Loading/Error state
        if self.is_loading {
            let loading = Paragraph::new("Loading sessions...")
                .style(Style::default().fg(Color::Yellow))
                .block(Block::default().borders(Borders::ALL).title(" Loading "));
            frame.render_widget(loading, chunks[1]);
            return;
        }

        if let Some(ref err) = self.error {
            let error = Paragraph::new(format!("Error: {}", err))
                .style(Style::default().fg(Color::Red))
                .block(Block::default().borders(Borders::ALL).title(" Error "));
            frame.render_widget(error, chunks[1]);
            return;
        }

        if self.sessions.is_empty() {
            let empty = Paragraph::new("No sessions found")
                .style(Style::default().fg(Color::DarkGray))
                .block(Block::default().borders(Borders::ALL).title(" Sessions "));
            frame.render_widget(empty, chunks[1]);
            return;
        }

        self.render_session_list(frame, chunks[1]);
    }

    fn render_session_list(&mut self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .filtered_sessions
            .iter()
            .map(|s| {
                // Format relative time
                let time_str = format_relative_time(s.updated_at);

                // Truncate preview
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

                // Short ID (first 8 chars)
                let short_id: String = s.id.chars().take(8).collect();

                let line = Line::from(vec![
                    Span::styled(format!("{:>8}", time_str), Style::default().fg(Color::Blue)),
                    Span::raw("  "),
                    Span::styled(short_id, Style::default().fg(Color::DarkGray)),
                    Span::raw("  "),
                    Span::styled(preview, Style::default().fg(Color::White)),
                ]);

                ListItem::new(line)
            })
            .collect();

        let count = self.filtered_sessions.len();
        let total = self.sessions.len();
        let title = format!(" Sessions ({}/{}) ", count, total);

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan))
                    .title(title),
            )
            .highlight_style(
                Style::default()
                    .bg(Color::DarkGray)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("> ");

        frame.render_stateful_widget(list, area, &mut self.list_state);
    }
}

/// Format a Unix timestamp as a relative time string.
fn format_relative_time(timestamp: i64) -> String {
    let now = chrono::Utc::now().timestamp();
    let diff = now - timestamp;

    if diff < 60 {
        "just now".to_string()
    } else if diff < 3600 {
        let mins = diff / 60;
        format!("{}m ago", mins)
    } else if diff < 86400 {
        let hours = diff / 3600;
        format!("{}h ago", hours)
    } else if diff < 604800 {
        let days = diff / 86400;
        format!("{}d ago", days)
    } else {
        let weeks = diff / 604800;
        format!("{}w ago", weeks)
    }
}
