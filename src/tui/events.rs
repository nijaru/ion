//! Event handling for the TUI.

use crate::session::Session;
use crate::tool::{ApprovalResponse, ToolMode};
use crate::tui::App;
use crate::tui::composer::ComposerBuffer;
use crate::tui::fuzzy;
use crate::tui::message_list::{MessageEntry, Sender};
use crate::tui::model_picker::PickerStage;
use crate::tui::types::{CANCEL_WINDOW, Mode, SelectorPage};
use crate::tui::util::handle_filter_input_event;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use std::time::Instant;

/// Threshold for storing paste as blob: >5 lines or >500 chars
const PASTE_BLOB_LINE_THRESHOLD: usize = 5;
const PASTE_BLOB_CHAR_THRESHOLD: usize = 500;

impl App {
    /// Main event dispatcher.
    pub fn handle_event(&mut self, event: Event) {
        match event {
            Event::Key(key) => match self.mode {
                Mode::Input => self.handle_input_mode(key),
                Mode::Approval => self.handle_approval_mode(key),
                Mode::Selector => self.handle_selector_mode(key),
                Mode::HelpOverlay => {
                    self.mode = Mode::Input;
                }
            },
            Event::Paste(text) => {
                if self.mode == Mode::Input {
                    self.handle_paste(text);
                }
            }
            Event::Resize(_, _) => {
                // Invalidate cached width so cursor position is recalculated
                self.input_state.invalidate_width();
                // Reset render state to force reprint of all chat
                self.handle_resize();
            }
            Event::FocusGained => {
                // Force full UI redraw when terminal regains focus
                // This fixes rendering artifacts from terminal tab switching
                self.render_state.needs_full_repaint = true;
            }
            _ => {}
        }
    }

    /// Handle pasted text - large pastes get stored as blobs with placeholders.
    fn handle_paste(&mut self, text: String) {
        let line_count = text.lines().count();
        let char_count = text.chars().count();

        if line_count > PASTE_BLOB_LINE_THRESHOLD || char_count > PASTE_BLOB_CHAR_THRESHOLD {
            // Store as blob and insert placeholder with invisible delimiters
            // to prevent collision with user-typed text that looks like a placeholder
            let blob_idx = self.input_buffer.push_blob(text);
            let placeholder = ComposerBuffer::internal_placeholder(blob_idx);
            self.input_state
                .insert_str(&mut self.input_buffer, &placeholder);
        } else {
            // Small paste - insert directly
            self.input_state.insert_str(&mut self.input_buffer, &text);
        }

        // Reset history tracking since input changed
        self.history_index = self.input_history.len();
        self.history_draft = None;
    }

    /// Main input handler - always active unless a modal is open.
    pub(super) fn handle_input_mode(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        // Handle command completer input first if active
        if self.command_completer.is_active() {
            match key.code {
                // Navigation within completer
                KeyCode::Up => {
                    self.command_completer.move_up();
                    return;
                }
                KeyCode::Down => {
                    self.command_completer.move_down();
                    return;
                }
                // Accept selection
                KeyCode::Tab | KeyCode::Enter if !shift => {
                    if let Some(cmd) = self.command_completer.selected_command() {
                        // Replace current input with selected command + space
                        self.input_buffer.clear();
                        self.input_state.move_to_start();
                        let cmd_with_space = format!("{cmd} ");
                        self.input_state
                            .insert_str(&mut self.input_buffer, &cmd_with_space);
                    }
                    self.command_completer.deactivate();
                    return;
                }
                // Cancel completer
                KeyCode::Esc => {
                    self.command_completer.deactivate();
                    return;
                }
                // Backspace might cancel if we delete the /
                KeyCode::Backspace => {
                    let cursor = self.input_state.cursor_char_idx();
                    if cursor <= 1 {
                        self.command_completer.deactivate();
                        self.handle_input_event_with_history(key);
                    } else {
                        // Continue with normal backspace, then update query
                        self.handle_input_event_with_history(key);
                        self.update_command_completer_query();
                    }
                    return;
                }
                // Character input updates the query
                KeyCode::Char(_) if !ctrl => {
                    self.handle_input_event_with_history(key);
                    self.update_command_completer_query();
                    return;
                }
                _ => {
                    // Other keys deactivate completer and process normally
                    self.command_completer.deactivate();
                }
            }
        }

        // Handle file completer input first if active
        if self.file_completer.is_active() {
            match key.code {
                // Navigation within completer
                KeyCode::Up => {
                    self.file_completer.move_up();
                    return;
                }
                KeyCode::Down => {
                    self.file_completer.move_down();
                    return;
                }
                // Accept selection
                KeyCode::Tab | KeyCode::Enter if !shift => {
                    if let Some(path) = self.file_completer.selected_path() {
                        let at_pos = self.file_completer.at_position();
                        let path_str = path.to_string_lossy().to_string();

                        // Replace @query with the selected path
                        let cursor = self.input_state.cursor_char_idx();
                        let delete_end = cursor;

                        // Delete from @ to cursor
                        if delete_end > at_pos {
                            self.input_buffer.remove_range(at_pos..delete_end);
                            self.input_state
                                .set_cursor(at_pos, self.input_buffer.len_chars());
                        }

                        // Insert selected path with @
                        let full_path = format!("@{path_str} ");
                        self.input_state
                            .insert_str(&mut self.input_buffer, &full_path);
                    }
                    self.file_completer.deactivate();
                    return;
                }
                // Cancel completer
                KeyCode::Esc => {
                    self.file_completer.deactivate();
                    return;
                }
                // Backspace might cancel if we delete past @
                KeyCode::Backspace => {
                    let at_pos = self.file_completer.at_position();
                    let cursor = self.input_state.cursor_char_idx();
                    if cursor <= at_pos + 1 {
                        self.file_completer.deactivate();
                    } else {
                        // Continue with normal backspace, then update query
                        self.handle_input_event_with_history(key);
                        self.update_file_completer_query();
                    }
                    return;
                }
                // Character input updates the query
                KeyCode::Char(_) if !ctrl => {
                    self.handle_input_event_with_history(key);
                    self.update_file_completer_query();
                    return;
                }
                _ => {
                    // Other keys deactivate completer and process normally
                    self.file_completer.deactivate();
                }
            }
        }

        match key.code {
            // Esc: Cancel running task, or double-Esc to clear input
            KeyCode::Esc => {
                if self.is_running && !self.session.abort_token.is_cancelled() {
                    self.session.abort_token.cancel();
                    self.cancel_pending = None;
                    self.esc_pending = None;
                } else if !self.input_is_empty() {
                    // Double-Esc to clear input
                    if let Some(when) = self.esc_pending
                        && when.elapsed() <= CANCEL_WINDOW
                    {
                        self.clear_input();
                        self.esc_pending = None;
                    } else {
                        self.esc_pending = Some(Instant::now());
                    }
                }
            }
            // Ctrl+C: Clear input (single), quit (double when idle)
            // Note: Esc cancels agent, Ctrl+C does not
            KeyCode::Char('c') if ctrl => {
                if !self.input_is_empty() {
                    self.clear_input();
                    self.cancel_pending = None;
                } else if !self.is_running {
                    // Only quit when idle (double-tap)
                    if let Some(when) = self.cancel_pending
                        && when.elapsed() <= CANCEL_WINDOW
                    {
                        self.quit();
                        self.cancel_pending = None;
                    } else {
                        self.cancel_pending = Some(Instant::now());
                    }
                }
            }

            // Ctrl+D: Quit if input empty (double-tap required, like Ctrl+C)
            KeyCode::Char('d') if ctrl => {
                if self.input_is_empty() {
                    if let Some(when) = self.cancel_pending
                        && when.elapsed() <= CANCEL_WINDOW
                    {
                        self.quit();
                        self.cancel_pending = None;
                    } else {
                        self.cancel_pending = Some(Instant::now());
                    }
                }
            }

            // Shift+Tab: Cycle tool mode (Read ↔ Write, or Read → Write → Agi if --agi)
            KeyCode::BackTab => {
                self.tool_mode = if self.permissions.agi_enabled {
                    // AGI mode available: Read → Write → Agi → Read
                    match self.tool_mode {
                        ToolMode::Read => ToolMode::Write,
                        ToolMode::Write => ToolMode::Agi,
                        ToolMode::Agi => ToolMode::Read,
                    }
                } else {
                    // Normal mode: Read ↔ Write only
                    match self.tool_mode {
                        ToolMode::Read => ToolMode::Write,
                        ToolMode::Write | ToolMode::Agi => ToolMode::Read,
                    }
                };
                // Update the orchestrator
                let orchestrator = self.orchestrator.clone();
                let mode = self.tool_mode;
                tokio::spawn(async move {
                    orchestrator.set_tool_mode(mode).await;
                });
            }

            // Ctrl+M: Open model picker (current provider only)
            KeyCode::Char('m') if ctrl => {
                if !self.is_running {
                    self.open_model_selector();
                }
            }

            // Ctrl+P: Previous history (readline), or provider picker when input empty
            KeyCode::Char('p') if ctrl => {
                if self.input_is_empty() && !self.is_running {
                    self.open_provider_selector();
                } else {
                    self.prev_history();
                }
            }

            // Ctrl+N: Next history (readline)
            KeyCode::Char('n') if ctrl => {
                self.next_history();
            }

            // Ctrl+H: Open help overlay
            KeyCode::Char('h') if ctrl => {
                self.mode = Mode::HelpOverlay;
            }

            // Ctrl+T: Cycle thinking level (off → standard → extended → off)
            KeyCode::Char('t') if ctrl => {
                self.thinking_level = self.thinking_level.next();
            }

            // Ctrl+G: Open input in external editor
            KeyCode::Char('g') if ctrl => {
                self.editor_requested = true;
            }

            // Shift+Enter or Alt+Enter: Insert newline
            // Shift+Enter requires Kitty keyboard protocol (Ghostty, Kitty, WezTerm, iTerm2)
            // Alt+Enter works universally as a fallback
            KeyCode::Enter if shift || key.modifiers.contains(KeyModifiers::ALT) => {
                self.input_state.insert_newline(&mut self.input_buffer);
            }

            // Enter: Send message or queue for mid-task steering
            KeyCode::Enter => {
                // Reject empty or whitespace-only input
                let input = self.input_text();
                if !input.trim().is_empty() {
                    if self.is_running {
                        // Queue message for injection at next turn (resolve blobs)
                        let resolved = self.resolved_input_text();
                        if let Some(queue) = self.message_queue.as_ref() {
                            match queue.lock() {
                                Ok(mut q) => q.push(resolved),
                                Err(poisoned) => {
                                    // Lock poisoned - recover and push anyway
                                    tracing::warn!("Message queue lock poisoned, recovering");
                                    poisoned.into_inner().push(resolved);
                                }
                            }
                        }
                        self.clear_input();
                    } else {
                        // Check for slash commands
                        if input.starts_with('/') {
                            const COMMANDS: [&str; 6] =
                                ["/model", "/provider", "/clear", "/quit", "/help", "/resume"];
                            let cmd_line = input.trim().to_lowercase();
                            let cmd_name = cmd_line.split_whitespace().next().unwrap_or("");
                            match cmd_name {
                                "/model" | "/models" => {
                                    self.clear_input();
                                    self.history_index = self.input_history.len();
                                    self.open_model_selector();
                                    return;
                                }
                                "/provider" | "/providers" => {
                                    self.clear_input();
                                    self.history_index = self.input_history.len();
                                    self.open_provider_selector();
                                    return;
                                }
                                "/resume" | "/sessions" => {
                                    self.clear_input();
                                    self.history_index = self.input_history.len();
                                    self.open_session_selector();
                                    return;
                                }
                                "/quit" | "/exit" | "/q" => {
                                    self.clear_input();
                                    self.quit();
                                    return;
                                }
                                "/clear" => {
                                    self.clear_input();
                                    self.history_index = self.input_history.len();

                                    // Save current session before starting fresh
                                    if !self.session.messages.is_empty() {
                                        let _ = self.store.save(&self.session);
                                    }

                                    // Start a new session (keeps old session in history)
                                    let working_dir = self.session.working_dir.clone();
                                    let model = self.session.model.clone();
                                    let no_sandbox = self.session.no_sandbox;
                                    self.session = Session::new(working_dir, model);
                                    self.session.no_sandbox = no_sandbox;

                                    // Clear display state
                                    self.message_list.clear();
                                    self.render_state.reset_for_new_conversation();

                                    // Clear active plan so it doesn't pollute new conversations
                                    let agent = self.agent.clone();
                                    tokio::spawn(async move {
                                        agent.clear_plan().await;
                                    });
                                    return;
                                }
                                "/help" | "/?" => {
                                    self.clear_input();
                                    self.history_index = self.input_history.len();
                                    self.mode = Mode::HelpOverlay;
                                    return;
                                }
                                _ => {
                                    if !cmd_name.is_empty() {
                                        let suggestions = fuzzy::top_matches(
                                            cmd_name,
                                            COMMANDS.iter().copied(),
                                            3,
                                        );
                                        let message = if suggestions.is_empty() {
                                            format!("Unknown command {cmd_name}")
                                        } else {
                                            format!(
                                                "Unknown command {}. Did you mean {}?",
                                                cmd_name,
                                                suggestions.join(", ")
                                            )
                                        };
                                        self.message_list
                                            .push_entry(MessageEntry::new(Sender::System, message));
                                    }
                                    self.clear_input();
                                    return;
                                }
                            }
                        }

                        // Send message - resolve blobs for agent, keep display version for UI
                        let resolved_input = self.resolved_input_text();
                        let normalized_input = crate::tui::util::normalize_input(&input);
                        let normalized_resolved =
                            crate::tui::util::normalize_input(&resolved_input);
                        self.input_history.push(normalized_input.clone());
                        self.history_index = self.input_history.len();
                        self.history_draft = None;
                        self.clear_input();
                        // Note: startup_ui_anchor is consumed by main.rs when rendering
                        // the first chat content - don't clear it here
                        // Persist to database (with placeholders, for shorter storage)
                        let _ = self.store.add_input_history(&normalized_input);
                        // Display shows placeholder (user can see what they typed)
                        self.message_list
                            .push_user_message(normalized_input.clone());
                        // Agent gets full resolved content
                        self.run_agent_task(normalized_resolved);
                    }
                }
            }

            // Page Up/Down: Scroll chat history
            KeyCode::PageUp => self.message_list.scroll_up(10),
            KeyCode::PageDown => self.message_list.scroll_down(10),

            // Arrow Up: Move cursor up, recall queued messages, or recall history
            KeyCode::Up => {
                if !self.handle_input_up() {
                    self.handle_input_event_with_history(key);
                }
            }

            // Arrow Down: Move cursor down, or restore newer history
            KeyCode::Down => {
                if !self.handle_input_down() {
                    self.handle_input_event_with_history(key);
                }
            }

            // ? shows help when input is empty
            KeyCode::Char('?') if self.input_is_empty() => {
                self.mode = Mode::HelpOverlay;
            }

            // @ might trigger file completion
            KeyCode::Char('@') => {
                self.handle_input_event_with_history(key);
                self.check_activate_file_completer();
            }

            // / might trigger command completion (at start of input)
            KeyCode::Char('/') => {
                self.handle_input_event_with_history(key);
                self.check_activate_command_completer();
            }

            _ => {
                self.handle_input_event_with_history(key);
            }
        }
    }

    /// Handle approval mode key events.
    pub(super) fn handle_approval_mode(&mut self, key: KeyEvent) {
        if let Some(request) = self.pending_approval.take() {
            let response = match key.code {
                KeyCode::Char('y') | KeyCode::Enter => Some(ApprovalResponse::Yes),
                KeyCode::Char('n') | KeyCode::Esc => Some(ApprovalResponse::No),
                KeyCode::Char('a') => Some(ApprovalResponse::AlwaysSession),
                KeyCode::Char('A') => Some(ApprovalResponse::AlwaysPermanent),
                _ => None,
            };

            if let Some(resp) = response {
                let _ = request.response_tx.send(resp);
                self.mode = Mode::Input;
            } else {
                self.pending_approval = Some(request);
            }
        }
    }

    /// Handle selector mode key events.
    pub(super) fn handle_selector_mode(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let mut handled = true;

        match key.code {
            // Ctrl+C: Close selector, double-tap to quit
            KeyCode::Char('c') if ctrl => {
                if let Some(when) = self.cancel_pending
                    && when.elapsed() <= CANCEL_WINDOW
                {
                    self.should_quit = true;
                    self.cancel_pending = None;
                } else {
                    self.cancel_pending = Some(Instant::now());
                    if self.needs_setup && self.selector_page == SelectorPage::Model {
                        self.model_picker.reset();
                        self.open_provider_selector();
                    } else {
                        self.model_picker.reset();
                        self.exit_selector_mode();
                    }
                }
            }

            // Ctrl+D: Close selector, double-tap to quit (same as Ctrl+C)
            KeyCode::Char('d') if ctrl => {
                if let Some(when) = self.cancel_pending
                    && when.elapsed() <= CANCEL_WINDOW
                {
                    self.should_quit = true;
                    self.cancel_pending = None;
                } else {
                    self.cancel_pending = Some(Instant::now());
                    if self.needs_setup && self.selector_page == SelectorPage::Model {
                        self.model_picker.reset();
                        self.open_provider_selector();
                    } else {
                        self.model_picker.reset();
                        self.exit_selector_mode();
                    }
                }
            }

            // Navigation
            KeyCode::Up => match self.selector_page {
                SelectorPage::Provider => self.provider_picker.move_up(1),
                SelectorPage::Model => self.model_picker.move_up(1),
                SelectorPage::Session => self.session_picker.move_up(1),
            },
            KeyCode::Down => match self.selector_page {
                SelectorPage::Provider => self.provider_picker.move_down(1),
                SelectorPage::Model => self.model_picker.move_down(1),
                SelectorPage::Session => self.session_picker.move_down(1),
            },
            KeyCode::PageUp => match self.selector_page {
                SelectorPage::Provider => self.provider_picker.move_up(10),
                SelectorPage::Model => self.model_picker.move_up(10),
                SelectorPage::Session => self.session_picker.move_up(10),
            },
            KeyCode::PageDown => match self.selector_page {
                SelectorPage::Provider => self.provider_picker.move_down(10),
                SelectorPage::Model => self.model_picker.move_down(10),
                SelectorPage::Session => self.session_picker.move_down(10),
            },
            KeyCode::Home => match self.selector_page {
                SelectorPage::Provider => self.provider_picker.jump_to_top(),
                SelectorPage::Model => self.model_picker.jump_to_top(),
                SelectorPage::Session => self.session_picker.jump_to_top(),
            },
            KeyCode::End => match self.selector_page {
                SelectorPage::Provider => self.provider_picker.jump_to_bottom(),
                SelectorPage::Model => self.model_picker.jump_to_bottom(),
                SelectorPage::Session => self.session_picker.jump_to_bottom(),
            },

            // Selection
            KeyCode::Enter => match self.selector_page {
                SelectorPage::Provider => {
                    if let Some(status) = self.provider_picker.selected()
                        && status.authenticated
                    {
                        let provider = status.provider;
                        if let Err(err) = self.set_provider(provider) {
                            self.last_error = Some(err.to_string());
                        } else {
                            self.open_model_selector();
                        }
                    }
                }
                SelectorPage::Model => match self.model_picker.stage {
                    PickerStage::Provider => {
                        self.model_picker.select_provider();
                    }
                    PickerStage::Model => {
                        if let Some(model) = self.model_picker.selected_model() {
                            self.session.model = model.id.clone();
                            if model.context_window > 0 {
                                let ctx_window = model.context_window as usize;
                                self.model_context_window = Some(ctx_window);
                                // Update agent's compaction config
                                self.agent.set_context_window(ctx_window);
                            } else {
                                self.model_context_window = None;
                            }
                            // Persist selection to config
                            self.config.model = Some(model.id.clone());
                            if let Err(e) = self.config.save() {
                                tracing::warn!("Failed to save config: {}", e);
                            }
                            self.model_picker.reset();
                            // Complete setup if this was the setup flow
                            if self.needs_setup {
                                self.needs_setup = false;
                            }
                            self.exit_selector_mode();
                        }
                    }
                },
                SelectorPage::Session => {
                    if let Some(summary) = self.session_picker.selected_session() {
                        let session_id = summary.id.clone();
                        if let Err(e) = self.load_session(&session_id) {
                            self.last_error = Some(format!("Failed to load session: {e}"));
                        }
                        self.session_picker.reset();
                        self.exit_selector_mode();
                    }
                }
            },

            // Backspace: when empty on model stage, go back to providers (if allowed)
            KeyCode::Backspace if self.selector_page == SelectorPage::Model => {
                if self.model_picker.filter_input.text().is_empty()
                    && self.model_picker.stage == PickerStage::Model
                    && !self.needs_setup
                {
                    self.model_picker.back_to_providers();
                } else {
                    handled = false;
                }
            }

            // Cancel / Back
            KeyCode::Esc => {
                if self.needs_setup {
                    if self.selector_page == SelectorPage::Model {
                        self.model_picker.reset();
                        self.open_provider_selector();
                    }
                } else {
                    self.model_picker.reset();
                    self.session_picker.reset();
                    self.exit_selector_mode();
                }
            }

            // Tab: switch pages (only for provider/model)
            KeyCode::Tab => match self.selector_page {
                SelectorPage::Provider => self.open_model_selector(),
                SelectorPage::Model => self.open_provider_selector(),
                SelectorPage::Session => {} // No tab switching for session picker
            },
            KeyCode::Char('p') if ctrl => {
                self.open_provider_selector();
            }
            KeyCode::Char('m') if ctrl => {
                self.open_model_selector();
            }

            _ => {
                handled = false;
            }
        }

        if !handled {
            // Handle filter input key events
            let text_changed = match self.selector_page {
                SelectorPage::Provider => {
                    handle_filter_input_event(&mut self.provider_picker.filter_input, key)
                }
                SelectorPage::Model => {
                    handle_filter_input_event(&mut self.model_picker.filter_input, key)
                }
                SelectorPage::Session => {
                    handle_filter_input_event(&mut self.session_picker.filter_input, key)
                }
            };

            if text_changed {
                match self.selector_page {
                    SelectorPage::Provider => self.provider_picker.apply_filter(),
                    SelectorPage::Model => self.model_picker.apply_filter(),
                    SelectorPage::Session => self.session_picker.apply_filter(),
                }
            }
        }
    }

    /// Handle terminal resize: reset state to force reprint of all chat.
    fn handle_resize(&mut self) {
        self.force_full_repaint();
    }

    /// Force a full repaint of chat history and UI.
    /// Used after resize or when exiting fullscreen selector.
    fn force_full_repaint(&mut self) {
        let has_entries = !self.message_list.entries.is_empty();
        self.render_state.reset_for_reflow(has_entries);
    }

    /// Exit selector mode and return to input, triggering repaint.
    pub(super) fn exit_selector_mode(&mut self) {
        self.mode = Mode::Input;
        // Selector used large area - flag for full clear + repaint
        self.render_state.needs_full_repaint = true;
        self.force_full_repaint();
    }
}
