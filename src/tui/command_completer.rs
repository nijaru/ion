//! Command autocomplete for / prefix in input.

use crate::tui::fuzzy;
use crossterm::{
    cursor::MoveTo,
    execute,
    style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
    terminal::{Clear, ClearType},
};
use std::io::Write;

/// Maximum number of candidates to show in the popup.
const MAX_VISIBLE: usize = 7;

/// Available slash commands with their descriptions.
pub const COMMANDS: &[(&str, &str)] = &[
    ("/model", "Open model selector"),
    ("/provider", "Open provider selector"),
    ("/resume", "Resume a previous session"),
    ("/clear", "Clear current conversation"),
    ("/help", "Show keybinding help"),
    ("/quit", "Exit the application"),
];

/// State for command autocomplete.
#[derive(Debug, Default)]
pub struct CommandCompleter {
    /// Whether completion is active (/ detected at start).
    active: bool,
    /// The query text after /.
    query: String,
    /// Filtered commands (after fuzzy match).
    filtered: Vec<(&'static str, &'static str)>,
    /// Currently selected index in filtered list.
    selected: usize,
}

impl CommandCompleter {
    /// Create a new command completer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if completion is active.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Get the current query (text after /).
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Get filtered candidates for display.
    #[must_use]
    pub fn visible_candidates(&self) -> &[(&'static str, &'static str)] {
        let end = self.filtered.len().min(MAX_VISIBLE);
        &self.filtered[..end]
    }

    /// Get the currently selected index.
    #[must_use]
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Get the selected command if any.
    #[must_use]
    pub fn selected_command(&self) -> Option<&'static str> {
        self.filtered.get(self.selected).map(|(cmd, _)| *cmd)
    }

    /// Activate completion.
    pub fn activate(&mut self) {
        self.active = true;
        self.query.clear();
        self.selected = 0;
        self.apply_filter();
    }

    /// Deactivate completion.
    pub fn deactivate(&mut self) {
        self.active = false;
        self.query.clear();
        self.filtered.clear();
        self.selected = 0;
    }

    /// Update the query and refresh filtering.
    pub fn set_query(&mut self, query: &str) {
        self.query = query.to_string();
        self.apply_filter();
    }

    /// Move selection up.
    pub fn move_up(&mut self) {
        if !self.filtered.is_empty() {
            self.selected = self.selected.saturating_sub(1);
        }
    }

    /// Move selection down.
    pub fn move_down(&mut self) {
        if !self.filtered.is_empty() {
            let max = self.filtered.len().min(MAX_VISIBLE).saturating_sub(1);
            self.selected = (self.selected + 1).min(max);
        }
    }

    /// Render the command completion popup above the input box.
    #[allow(clippy::cast_possible_truncation)]
    pub fn render<W: Write>(&self, w: &mut W, input_start: u16, width: u16) -> std::io::Result<()> {
        let candidates = self.visible_candidates();
        if candidates.is_empty() {
            return Ok(());
        }

        let popup_height = candidates.len() as u16;
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

        for (i, (cmd, desc)) in candidates.iter().enumerate() {
            let row = popup_start + i as u16;
            let is_selected = i == self.selected;

            execute!(w, MoveTo(1, row), Clear(ClearType::CurrentLine))?;

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

    /// Apply fuzzy filter to commands.
    fn apply_filter(&mut self) {
        if self.query.is_empty() {
            // Show all commands
            self.filtered = COMMANDS.to_vec();
        } else {
            // Fuzzy match on command names
            let query_with_slash = format!("/{}", self.query);
            let candidates: Vec<&str> = COMMANDS.iter().map(|(cmd, _)| *cmd).collect();
            let matches = fuzzy::top_matches(&query_with_slash, candidates, MAX_VISIBLE);
            self.filtered = matches
                .into_iter()
                .filter_map(|m| COMMANDS.iter().find(|(cmd, _)| *cmd == m).copied())
                .collect();
        }

        // Clamp selection
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_activate_deactivate() {
        let mut completer = CommandCompleter::new();

        assert!(!completer.is_active());
        completer.activate();
        assert!(completer.is_active());
        assert!(!completer.visible_candidates().is_empty());

        completer.deactivate();
        assert!(!completer.is_active());
    }

    #[test]
    fn test_all_commands_shown_on_empty_query() {
        let mut completer = CommandCompleter::new();
        completer.activate();

        assert_eq!(completer.visible_candidates().len(), COMMANDS.len());
    }

    #[test]
    fn test_fuzzy_filter() {
        let mut completer = CommandCompleter::new();
        completer.activate();
        completer.set_query("mod");

        let candidates = completer.visible_candidates();
        assert!(!candidates.is_empty());
        assert!(candidates.iter().any(|(cmd, _)| *cmd == "/model"));
    }

    #[test]
    fn test_navigation() {
        let mut completer = CommandCompleter::new();
        completer.activate();

        assert_eq!(completer.selected(), 0);

        completer.move_down();
        assert_eq!(completer.selected(), 1);

        completer.move_up();
        assert_eq!(completer.selected(), 0);

        // Should not go below 0
        completer.move_up();
        assert_eq!(completer.selected(), 0);
    }

    #[test]
    fn test_selected_command() {
        let mut completer = CommandCompleter::new();
        completer.activate();

        assert_eq!(completer.selected_command(), Some("/model"));

        completer.move_down();
        assert_eq!(completer.selected_command(), Some("/provider"));
    }
}
