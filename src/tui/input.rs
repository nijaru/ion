//! Input handling for the TUI composer.

use crate::tui::App;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::prelude::*;

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
    pub(super) fn startup_header_lines(&self) -> Vec<Line<'static>> {
        let version = format!("v{}", env!("CARGO_PKG_VERSION"));
        vec![
            Line::from(Span::styled("ION", Style::default().bold())),
            Line::from(Span::styled(version, Style::default().dim())),
            Line::from(""),
        ]
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

        if self.history_index == self.input_history.len() && self.history_draft.is_none() {
            // Save resolved content (with blobs expanded) so it survives history navigation
            self.history_draft = Some(self.resolved_input_text());
        }

        if !self.input_history.is_empty() && self.history_index > 0 {
            self.history_index -= 1;
            let entry = self.input_history[self.history_index].clone();
            self.set_input_text(&entry);
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

        if self.history_index < self.input_history.len() {
            self.history_index += 1;
            if self.history_index == self.input_history.len() {
                if let Some(draft) = self.history_draft.take() {
                    self.set_input_text(&draft);
                } else {
                    self.clear_input();
                }
            } else {
                let entry = self.input_history[self.history_index].clone();
                self.set_input_text(&entry);
            }
            return true;
        }

        !self.input_is_empty()
    }
}
