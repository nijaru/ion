//! Command autocomplete for / and // prefix in input.

use crate::tui::completer_state::CompleterState;
use crate::tui::fuzzy;

/// Maximum number of candidates to show in the popup.
const MAX_VISIBLE: usize = 9;

/// Command with its description.
pub type Command = (&'static str, &'static str);

/// Available slash commands with their descriptions.
pub const COMMANDS: &[Command] = &[
    ("/compact", "Compact context (prune tool outputs)"),
    ("/cost", "Show session cost and pricing"),
    ("/export", "Export session to markdown file"),
    ("/model", "Open model selector"),
    ("/provider", "Open provider selector"),
    ("/resume", "Resume a previous session"),
    ("/clear", "Clear current conversation"),
    ("/help", "Show keybinding help"),
    ("/quit", "Exit the application"),
];

/// Which mode the completer is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompleterMode {
    Builtin,
    Skill,
}

/// State for command autocomplete (/ for builtins, // for skills).
#[derive(Debug, Clone)]
pub struct CommandCompleter {
    mode: CompleterMode,
    state: CompleterState<Command>,
    skill_candidates: Vec<(String, String)>,
    skill_state: CompleterState<(String, String)>,
}

impl Default for CommandCompleter {
    fn default() -> Self {
        Self {
            mode: CompleterMode::Builtin,
            state: CompleterState::new(MAX_VISIBLE),
            skill_candidates: Vec::new(),
            skill_state: CompleterState::new(MAX_VISIBLE),
        }
    }
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
        match self.mode {
            CompleterMode::Builtin => self.state.is_active(),
            CompleterMode::Skill => self.skill_state.is_active(),
        }
    }

    /// Check if the completer is in skill mode.
    #[must_use]
    pub fn is_skill_mode(&self) -> bool {
        self.mode == CompleterMode::Skill && self.skill_state.is_active()
    }

    /// Get the current query.
    #[must_use]
    pub fn query(&self) -> &str {
        match self.mode {
            CompleterMode::Builtin => self.state.query(),
            CompleterMode::Skill => self.skill_state.query(),
        }
    }

    /// Set skill candidates (called once at startup).
    pub fn set_skill_candidates(&mut self, candidates: Vec<(String, String)>) {
        self.skill_candidates = candidates;
    }

    /// Activate builtin completion (/ prefix).
    pub fn activate(&mut self) {
        self.mode = CompleterMode::Builtin;
        self.state.activate();
        self.skill_state.deactivate();
        self.apply_builtin_filter();
    }

    /// Switch to skill completion (// prefix).
    pub fn activate_skill_mode(&mut self) {
        self.mode = CompleterMode::Skill;
        self.skill_state.activate();
        self.state.deactivate();
        self.apply_skill_filter();
    }

    /// Switch back to builtin completion (/ prefix after backspacing //).
    pub fn activate_builtin_mode(&mut self) {
        self.mode = CompleterMode::Builtin;
        self.state.activate();
        self.skill_state.deactivate();
        self.apply_builtin_filter();
    }

    /// Count of currently visible candidates (for layout height calculation).
    #[must_use]
    pub fn visible_candidates_count(&self) -> usize {
        match self.mode {
            CompleterMode::Builtin => self.state.visible_candidates().len(),
            CompleterMode::Skill => self.skill_state.visible_candidates().len(),
        }
    }

    /// Deactivate completion entirely.
    pub fn deactivate(&mut self) {
        self.state.deactivate();
        self.skill_state.deactivate();
    }

    /// Update the query and refresh filtering.
    pub fn set_query(&mut self, query: &str) {
        match self.mode {
            CompleterMode::Builtin => {
                self.state.set_query(query);
                self.apply_builtin_filter();
            }
            CompleterMode::Skill => {
                self.skill_state.set_query(query);
                self.apply_skill_filter();
            }
        }
    }

    /// Move selection up.
    pub fn move_up(&mut self) {
        match self.mode {
            CompleterMode::Builtin => self.state.move_up(),
            CompleterMode::Skill => self.skill_state.move_up(),
        }
    }

    /// Move selection down.
    pub fn move_down(&mut self) {
        match self.mode {
            CompleterMode::Builtin => self.state.move_down(),
            CompleterMode::Skill => self.skill_state.move_down(),
        }
    }

    /// Get the selected command string.
    /// Returns `/cmd` in builtin mode, `//skill-name` in skill mode.
    #[must_use]
    pub fn selected_command(&self) -> Option<String> {
        match self.mode {
            CompleterMode::Builtin => self.state.selected().map(|(cmd, _)| cmd.to_string()),
            CompleterMode::Skill => self
                .skill_state
                .selected()
                .map(|(name, _)| format!("//{name}")),
        }
    }

    fn apply_builtin_filter(&mut self) {
        let filtered = if self.state.query().is_empty() {
            COMMANDS.to_vec()
        } else {
            let query_with_slash = format!("/{}", self.state.query());
            let candidates: Vec<&str> = COMMANDS.iter().map(|(cmd, _)| *cmd).collect();
            let matches = fuzzy::top_matches(&query_with_slash, candidates, MAX_VISIBLE);
            matches
                .into_iter()
                .filter_map(|m| COMMANDS.iter().find(|(cmd, _)| *cmd == m).copied())
                .collect()
        };
        self.state.set_filtered(filtered);
    }

    fn apply_skill_filter(&mut self) {
        let query = self.skill_state.query().to_string();
        let filtered: Vec<(String, String)> = if query.is_empty() {
            self.skill_candidates.clone()
        } else {
            let candidates: Vec<&str> = self
                .skill_candidates
                .iter()
                .map(|(n, _)| n.as_str())
                .collect();
            let matches = fuzzy::top_matches(&query, candidates, MAX_VISIBLE);
            matches
                .into_iter()
                .filter_map(|m| self.skill_candidates.iter().find(|(n, _)| n == m).cloned())
                .collect()
        };
        self.skill_state.set_filtered(filtered);
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
        assert!(!completer.state.visible_candidates().is_empty());

        completer.deactivate();
        assert!(!completer.is_active());
    }

    #[test]
    fn test_all_commands_shown_on_empty_query() {
        let mut completer = CommandCompleter::new();
        completer.activate();

        assert_eq!(completer.state.visible_candidates().len(), COMMANDS.len());
    }

    #[test]
    fn test_fuzzy_filter() {
        let mut completer = CommandCompleter::new();
        completer.activate();
        completer.set_query("mod");

        assert!(completer
            .state
            .visible_candidates()
            .iter()
            .any(|(cmd, _)| *cmd == "/model"));
    }

    #[test]
    fn test_navigation() {
        let mut completer = CommandCompleter::new();
        completer.activate();

        assert_eq!(completer.state.selected_index(), 0);

        completer.move_down();
        assert_eq!(completer.state.selected_index(), 1);

        completer.move_up();
        assert_eq!(completer.state.selected_index(), 0);

        // Should not go below 0
        completer.move_up();
        assert_eq!(completer.state.selected_index(), 0);
    }

    #[test]
    fn test_selected_command_builtin() {
        let mut completer = CommandCompleter::new();
        completer.activate();

        assert_eq!(completer.selected_command(), Some("/compact".to_string()));

        completer.move_down();
        assert_eq!(completer.selected_command(), Some("/cost".to_string()));
    }

    #[test]
    fn test_skill_mode() {
        let mut completer = CommandCompleter::new();
        completer.set_skill_candidates(vec![
            ("review".to_string(), "Review code".to_string()),
            ("test-skill".to_string(), "Run tests".to_string()),
        ]);

        assert!(!completer.is_active());
        completer.activate_skill_mode();
        assert!(completer.is_active());
        assert!(completer.is_skill_mode());
        assert!(!completer.state.is_active());

        // All skills shown on empty query
        assert_eq!(completer.skill_state.visible_candidates().len(), 2);

        // selected_command returns //name
        assert_eq!(completer.selected_command(), Some("//review".to_string()));
    }

    #[test]
    fn test_skill_fuzzy_filter() {
        let mut completer = CommandCompleter::new();
        completer.set_skill_candidates(vec![
            ("review".to_string(), "Review code".to_string()),
            ("test-skill".to_string(), "Run tests".to_string()),
        ]);

        completer.activate_skill_mode();
        completer.set_query("rev");

        let visible = completer.skill_state.visible_candidates();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].0, "review");
    }

    #[test]
    fn test_mode_switch_builtin_to_skill() {
        let mut completer = CommandCompleter::new();
        completer.activate();
        assert!(!completer.is_skill_mode());

        completer.activate_skill_mode();
        assert!(completer.is_skill_mode());
        assert!(!completer.state.is_active());
    }

    #[test]
    fn test_mode_switch_skill_to_builtin() {
        let mut completer = CommandCompleter::new();
        completer.activate_skill_mode();
        assert!(completer.is_skill_mode());

        completer.activate_builtin_mode();
        assert!(!completer.is_skill_mode());
        assert!(completer.state.is_active());
    }
}
