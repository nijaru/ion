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
    pub fn refresh(&mut self) {
        self.providers = ProviderStatus::sorted(ProviderStatus::detect_all());
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

    /// Delete last word from filter (Ctrl+W).
    pub fn delete_word(&mut self) {
        // Trim trailing whitespace first
        let trimmed = self.filter.trim_end();
        if let Some(last_space) = trimmed.rfind(' ') {
            self.filter.truncate(last_space + 1);
        } else {
            self.filter.clear();
        }
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

    /// Select a specific provider by enum value.
    pub fn select_provider(&mut self, provider: crate::provider::ApiProvider) {
        if let Some(idx) = self.filtered.iter().position(|s| s.provider == provider) {
            self.list_state.select(Some(idx));
        }
    }

    /// Render the picker modal.
    pub fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();

        // Size to content: width for longest name + hint, height for items
        let content_width = 50u16; // Enough for name + auth hint columns
        let list_height = (self.filtered.len() as u16 + 2).max(5); // +2 for borders, min 5
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
        let search_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Filter (type to search) ");

        let search_text = if self.filter.is_empty() {
            " ...".to_string()
        } else {
            format!(" {}_", self.filter)
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

        // Column width for provider name
        let name_col_width = 20usize;

        // Provider list
        let items: Vec<ListItem> = self
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

                let name = status.provider.name();
                let name_padded = format!("{:width$}", name, width = name_col_width);

                let auth_hint = if !status.authenticated {
                    format!(
                        "set {}",
                        status.provider.env_vars().first().unwrap_or(&"API_KEY")
                    )
                } else {
                    String::new()
                };

                let line = Line::from(vec![
                    Span::styled(icon, icon_style),
                    Span::raw(" "),
                    Span::styled(name_padded, name_style),
                    Span::styled(auth_hint, Style::default().fg(Color::Red).dim()),
                ]);

                ListItem::new(line)
            })
            .collect();

        let count = self.filtered.len();
        let total = self.providers.len();
        let title = format!(" Providers ({}/{}) ", count, total);

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
