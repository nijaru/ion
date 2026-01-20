//! API Provider picker modal.
//!
//! Allows selecting the API provider (Anthropic, OpenRouter, etc.)
//! with visual indication of authentication status.

use crate::provider::ProviderStatus;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

/// State for the API provider picker modal.
pub struct ProviderPicker {
    /// All provider statuses (detected on open).
    pub providers: Vec<ProviderStatus>,
    /// List selection state.
    pub list_state: ListState,
    /// Filter text for type-to-filter.
    pub filter: String,
    /// Filtered providers based on search.
    pub filtered: Vec<ProviderStatus>,
}

impl Default for ProviderPicker {
    fn default() -> Self {
        Self {
            providers: Vec::new(),
            list_state: ListState::default(),
            filter: String::new(),
            filtered: Vec::new(),
        }
    }
}

impl ProviderPicker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Refresh provider detection and reset selection.
    /// Only shows implemented providers.
    pub fn refresh(&mut self) {
        self.providers = ProviderStatus::sorted(
            ProviderStatus::detect_all()
                .into_iter()
                .filter(|s| s.implemented)
                .collect(),
        );
        self.filter.clear();
        self.apply_filter();
    }

    /// Apply filter to provider list.
    fn apply_filter(&mut self) {
        let filter_lower = self.filter.to_lowercase();

        self.filtered = self
            .providers
            .iter()
            .filter(|p| {
                if filter_lower.is_empty() {
                    return true;
                }
                p.provider.name().to_lowercase().contains(&filter_lower)
                    || p.provider
                        .description()
                        .to_lowercase()
                        .contains(&filter_lower)
            })
            .cloned()
            .collect();

        if !self.filtered.is_empty() {
            self.list_state.select(Some(0));
        } else {
            self.list_state.select(None);
        }
    }

    /// Add character to filter.
    pub fn push_char(&mut self, c: char) {
        self.filter.push(c);
        self.apply_filter();
    }

    /// Remove last character from filter.
    pub fn pop_char(&mut self) {
        self.filter.pop();
        self.apply_filter();
    }

    /// Move selection up.
    pub fn move_up(&mut self, count: usize) {
        if self.filtered.is_empty() {
            return;
        }
        let i = self.list_state.selected().unwrap_or(0);
        let new_i = i.saturating_sub(count);
        self.list_state.select(Some(new_i));
    }

    /// Move selection down.
    pub fn move_down(&mut self, count: usize) {
        if self.filtered.is_empty() {
            return;
        }
        let len = self.filtered.len();
        let i = self.list_state.selected().unwrap_or(0);
        let new_i = (i + count).min(len - 1);
        self.list_state.select(Some(new_i));
    }

    /// Jump to top.
    pub fn jump_to_top(&mut self) {
        if !self.filtered.is_empty() {
            self.list_state.select(Some(0));
        }
    }

    /// Jump to bottom.
    pub fn jump_to_bottom(&mut self) {
        if !self.filtered.is_empty() {
            self.list_state.select(Some(self.filtered.len() - 1));
        }
    }

    /// Get currently selected provider.
    pub fn selected(&self) -> Option<&ProviderStatus> {
        self.list_state
            .selected()
            .and_then(|i| self.filtered.get(i))
    }

    /// Render the picker modal.
    pub fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();

        // Center the modal (60% width, 60% height)
        let modal_width = (area.width as f32 * 0.6) as u16;
        let modal_height = (area.height as f32 * 0.6) as u16;
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
        let search_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Filter providers (type to search) ");

        let search_text = if self.filter.is_empty() {
            "Start typing to filter...".to_string()
        } else {
            format!("Filter: {}_", self.filter)
        };

        let search_style = if self.filter.is_empty() {
            Style::default().dim()
        } else {
            Style::default().fg(Color::Yellow)
        };

        let search_para = Paragraph::new(search_text)
            .style(search_style)
            .block(search_block);
        frame.render_widget(search_para, chunks[0]);

        // Provider list
        let items: Vec<ListItem> = self
            .filtered
            .iter()
            .map(|status| {
                // Simple: green = authenticated, gray = not authenticated
                let (icon, icon_style, name_style, desc_style) = if status.authenticated {
                    (
                        "●",
                        Style::default().fg(Color::Green),
                        Style::default().fg(Color::White).bold(),
                        Style::default().fg(Color::Blue).dim(),
                    )
                } else {
                    (
                        "○",
                        Style::default().dim(),
                        Style::default().dim(),
                        Style::default().dim(),
                    )
                };

                let auth_hint = if !status.authenticated {
                    format!(
                        " [set {}]",
                        status.provider.env_vars().first().unwrap_or(&"API key")
                    )
                } else {
                    String::new()
                };

                let line = Line::from(vec![
                    Span::styled(icon, icon_style),
                    Span::raw(" "),
                    Span::styled(status.provider.name(), name_style),
                    Span::raw(" "),
                    Span::styled(status.provider.description(), desc_style),
                    Span::styled(auth_hint, Style::default().dim()),
                ]);

                ListItem::new(line)
            })
            .collect();

        let count = self.filtered.len();
        let total = self.providers.len();
        let title = format!(
            " API Providers ({}/{}) │ ↑↓ navigate │ Enter select │ Esc cancel ",
            count, total
        );

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
            .highlight_symbol("▸ ");

        frame.render_stateful_widget(list, chunks[1], &mut self.list_state);
    }
}
