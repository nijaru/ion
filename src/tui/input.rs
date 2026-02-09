//! Input handling for the TUI composer.

use crate::tui::terminal::{StyledLine, StyledSpan};
use crate::tui::App;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

impl App {
    /// Get the current input text (with placeholders for large pastes).
    pub(super) fn input_text(&self) -> String {
        self.input_buffer.get_content()
    }

    /// Get the full input text with paste blobs resolved.
    pub(super) fn resolved_input_text(&self) -> String {
        self.input_buffer.resolve_content()
    }

    /// Check if the input buffer is empty.
    pub(super) fn input_is_empty(&self) -> bool {
        self.input_buffer.is_empty()
    }

    /// Clear the input buffer.
    pub(super) fn clear_input(&mut self) {
        self.input_state.clear(&mut self.input_buffer);
    }

    /// Set the input text and move cursor to end.
    pub fn set_input_text(&mut self, text: &str) {
        self.input_buffer.set_content(text);
        self.input_state.move_to_end(&self.input_buffer);
    }

    /// Handle input key event and update history tracking.
    /// Returns true if the event was handled and text changed.
    pub(super) fn handle_input_event_with_history(&mut self, key: KeyEvent) -> bool {
        let changed = self.handle_input_event(key);
        if changed {
            self.history_index = self.input_history.len();
            self.history_draft = None;
        }
        changed
    }

    /// Generate the startup header lines for the TUI.
    pub(super) fn startup_header_lines(working_dir: &std::path::Path) -> Vec<StyledLine> {
        let version = format!("v{}", env!("CARGO_PKG_VERSION"));

        // Shorten working dir: replace home prefix with ~
        let dir_display = if let Some(home) = dirs::home_dir() {
            if let Ok(suffix) = working_dir.strip_prefix(&home) {
                format!("~/{}", suffix.display())
            } else {
                working_dir.display().to_string()
            }
        } else {
            working_dir.display().to_string()
        };

        // Try to get git branch (falls back to short SHA on detached HEAD)
        let branch = std::process::Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(working_dir)
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .and_then(|b| {
                if b == "HEAD" {
                    // Detached HEAD — show short SHA instead
                    std::process::Command::new("git")
                        .args(["rev-parse", "--short", "HEAD"])
                        .current_dir(working_dir)
                        .stderr(std::process::Stdio::null())
                        .output()
                        .ok()
                        .filter(|o| o.status.success())
                        .and_then(|o| String::from_utf8(o.stdout).ok())
                        .map(|s| s.trim().to_string())
                } else {
                    Some(b)
                }
            });

        let mut location_spans = vec![StyledSpan::dim(&dir_display)];
        if let Some(ref b) = branch {
            location_spans.push(StyledSpan::dim(format!(" [{b}]")));
        }

        vec![
            StyledLine::new(vec![
                StyledSpan::bold("ion"),
                StyledSpan::dim(format!(" {version}")),
            ]),
            StyledLine::new(location_spans),
            StyledLine::empty(),
        ]
    }

    /// Return startup header lines once and mark them as inserted.
    pub fn take_startup_header_lines(&mut self) -> Vec<StyledLine> {
        if self.render_state.header_inserted {
            return Vec::new();
        }
        self.render_state.header_inserted = true;
        Self::startup_header_lines(&self.session.working_dir)
    }

    /// Handle a key event for the input composer.
    /// Returns true if the event caused a text change.
    pub(super) fn handle_input_event(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let super_key = key.modifiers.contains(KeyModifiers::SUPER);

        match key.code {
            // Character input
            KeyCode::Char(c) if !ctrl && !alt && !super_key => {
                self.input_state.insert_char(&mut self.input_buffer, c);
                true
            }

            // Navigation: Cmd+Left/Right (macOS) for visual line start/end (wrapped lines)
            KeyCode::Left if super_key => {
                self.input_state
                    .move_to_visual_line_start(&self.input_buffer);
                false
            }
            KeyCode::Right if super_key => {
                self.input_state.move_to_visual_line_end(&self.input_buffer);
                false
            }

            // Navigation: Ctrl+Left/Right or Option+Left/Right (macOS) for word movement
            KeyCode::Left if ctrl || alt => {
                self.input_state.move_word_left(&self.input_buffer);
                false
            }
            KeyCode::Right if ctrl || alt => {
                self.input_state.move_word_right(&self.input_buffer);
                false
            }
            // Alt+b / Alt+f: Emacs-style word navigation (sent by many terminals for Option+Arrow)
            KeyCode::Char('b') if alt => {
                self.input_state.move_word_left(&self.input_buffer);
                false
            }
            KeyCode::Char('f') if alt => {
                self.input_state.move_word_right(&self.input_buffer);
                false
            }
            KeyCode::Left => {
                self.input_state.move_left(&self.input_buffer);
                false
            }
            KeyCode::Right => {
                self.input_state.move_right(&self.input_buffer);
                false
            }
            KeyCode::Home => {
                self.input_state.move_to_line_start(&self.input_buffer);
                false
            }
            KeyCode::End => {
                self.input_state.move_to_line_end(&self.input_buffer);
                false
            }

            // Emacs-style navigation
            KeyCode::Char('a') if ctrl => {
                self.input_state.move_to_line_start(&self.input_buffer);
                false
            }
            KeyCode::Char('e') if ctrl => {
                self.input_state.move_to_line_end(&self.input_buffer);
                false
            }

            // Deletion: Cmd+Backspace (macOS) or Ctrl+U for delete to line start
            KeyCode::Backspace if super_key => {
                self.input_state.delete_line_left(&mut self.input_buffer);
                true
            }
            // Deletion: Ctrl+Backspace or Option+Backspace (macOS) for delete word
            KeyCode::Backspace if ctrl || alt => {
                self.input_state.delete_word(&mut self.input_buffer);
                true
            }
            KeyCode::Backspace => {
                self.input_state.delete_char_before(&mut self.input_buffer);
                true
            }
            KeyCode::Delete => {
                self.input_state.delete_char_after(&mut self.input_buffer);
                true
            }

            // Line editing
            KeyCode::Char('w') if ctrl => {
                self.input_state.delete_word(&mut self.input_buffer);
                true
            }
            KeyCode::Char('u') if ctrl => {
                self.input_state.delete_line_left(&mut self.input_buffer);
                true
            }
            KeyCode::Char('k') if ctrl => {
                self.input_state.delete_line_right(&mut self.input_buffer);
                true
            }

            _ => false,
        }
    }

    /// Handle Up arrow key: cursor movement, queued message recall, or history.
    pub(super) fn handle_input_up(&mut self) -> bool {
        let input_empty = self.input_is_empty();
        // Try visual line movement first (handles both wrapped and newline-separated)
        if !input_empty && self.input_state.move_up(&self.input_buffer) {
            return true;
        }

        // When running with empty input, recall queued messages
        if self.is_running && input_empty {
            let queued = self.message_queue.as_ref().and_then(|queue| {
                if let Ok(mut q) = queue.lock() {
                    if q.is_empty() {
                        None
                    } else {
                        Some(q.drain(..).collect::<Vec<_>>())
                    }
                } else {
                    None
                }
            });

            if let Some(queued) = queued {
                self.set_input_text(&queued.join("\n\n"));
                return true;
            }
        }

        // Navigate to previous history entry
        if self.prev_history() {
            return true;
        }

        input_empty
    }

    /// Handle Down arrow key: cursor movement or history navigation.
    pub(super) fn handle_input_down(&mut self) -> bool {
        // Try visual line movement first (handles both wrapped and newline-separated)
        if self.input_state.move_down(&self.input_buffer) {
            return true;
        }

        self.next_history()
    }

    /// Navigate to previous history entry (Ctrl+P).
    /// Skips cursor movement, directly accesses history.
    pub(super) fn prev_history(&mut self) -> bool {
        if self.input_history.is_empty() {
            return false;
        }

        // Save current input as draft before navigating
        if self.history_index == self.input_history.len() && self.history_draft.is_none() {
            self.history_draft = Some(self.resolved_input_text());
        }

        if self.history_index > 0 {
            self.history_index -= 1;
            let entry = self.input_history[self.history_index].clone();
            self.set_input_text(&entry);
            true
        } else {
            false
        }
    }

    /// Navigate to next history entry (Ctrl+N).
    /// Skips cursor movement, directly accesses history.
    pub(super) fn next_history(&mut self) -> bool {
        if self.history_index < self.input_history.len() {
            self.history_index += 1;
            if self.history_index == self.input_history.len() {
                // Restore draft or clear
                if let Some(draft) = self.history_draft.take() {
                    self.set_input_text(&draft);
                } else {
                    self.clear_input();
                }
            } else {
                let entry = self.input_history[self.history_index].clone();
                self.set_input_text(&entry);
            }
            true
        } else {
            false
        }
    }

    /// Update the file completer query based on current input.
    /// Called after input changes when completer is active.
    pub(super) fn update_file_completer_query(&mut self) {
        if !self.file_completer.is_active() {
            return;
        }

        let at_pos = self.file_completer.at_position();
        let cursor = self.input_state.cursor_char_idx();
        let content = self.input_buffer.get_content();

        // Extract text between @ and cursor
        if cursor > at_pos {
            let query: String = content
                .chars()
                .skip(at_pos + 1)
                .take(cursor - at_pos - 1)
                .collect();
            self.file_completer.set_query(&query);
        } else {
            self.file_completer.set_query("");
        }
    }

    /// Check if we should activate file completion (@ at word boundary).
    pub(super) fn check_activate_file_completer(&mut self) {
        let cursor = self.input_state.cursor_char_idx();
        if cursor == 0 {
            return;
        }

        let content = self.input_buffer.get_content();
        let chars: Vec<char> = content.chars().collect();

        // Check if character before cursor is @
        if cursor > 0 && chars.get(cursor - 1) == Some(&'@') {
            // Check if @ is at start or preceded by whitespace (word boundary)
            let is_at_boundary =
                cursor == 1 || chars.get(cursor - 2).is_none_or(|c| c.is_whitespace());
            if is_at_boundary {
                self.file_completer.activate(cursor - 1);
            }
        }
    }

    /// Update the command completer query based on current input.
    pub(super) fn update_command_completer_query(&mut self) {
        if !self.command_completer.is_active() {
            return;
        }

        let cursor = self.input_state.cursor_char_idx();
        let content = self.input_buffer.get_content();

        if !content.starts_with('/') || cursor == 0 {
            self.command_completer.deactivate();
            return;
        }

        // Extract text after / (the query)
        if cursor > 1 {
            let query: String = content.chars().skip(1).take(cursor - 1).collect();
            self.command_completer.set_query(&query);
        } else {
            self.command_completer.set_query("");
        }
    }

    /// Check if we should activate command completion (/ at start of input).
    pub(super) fn check_activate_command_completer(&mut self) {
        let cursor = self.input_state.cursor_char_idx();
        let content = self.input_buffer.get_content();

        // Only activate if / is at position 0 (start of input)
        if cursor == 1 && content.starts_with('/') {
            self.command_completer.activate();
        }
    }
}
