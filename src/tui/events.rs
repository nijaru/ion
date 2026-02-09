//! Event handling for the TUI.

use crate::session::Session;
use crate::tool::ToolMode;
use crate::tui::App;
use crate::tui::PickerNavigation;
use crate::tui::composer::ComposerBuffer;
use crate::tui::fuzzy;
use crate::tui::message_list::{MessageEntry, Sender};
use crate::tui::model_picker::PickerStage;
use crate::tui::types::{CANCEL_WINDOW, Mode, SelectorPage};
use crate::tui::util::{format_cost, handle_filter_input_event};
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
                Mode::Selector => self.handle_selector_mode(key),
                Mode::HelpOverlay => {
                    self.mode = Mode::Input;
                }
                Mode::HistorySearch => self.handle_history_search_mode(key),
            },
            Event::Paste(text) => {
                if self.mode == Mode::Input {
                    self.handle_paste(text);
                }
            }
            Event::Resize(_, _) => {
                self.input_state.invalidate_width();
                // Invalidate all positioning state - row values are wrong after
                // terminal reflow. The needs_reflow flag triggers a targeted
                // clear + reprint of ion's content only (not pre-ion history).
                self.render_state.chat_row = None;
                self.render_state.startup_ui_anchor = None;
                self.render_state.last_ui_start = None;
                self.render_state.needs_reflow = true;
            }
            Event::FocusGained => {
                // No-op: terminal size poll handles any resize that
                // happened while away. Don't disturb chat_row tracking.
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
    #[allow(clippy::too_many_lines)]
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
                        let is_dir = path.is_dir();
                        let has_spaces = path_str.contains(' ');

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
                        // - Directory: trailing slash
                        // - Spaces: auto-quote
                        let full_path = if has_spaces {
                            format!("@\"{path_str}\" ")
                        } else if is_dir {
                            format!("@{path_str}/ ")
                        } else {
                            format!("@{path_str} ")
                        };
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
                        // Deactivate and perform the backspace (delete the @)
                        self.file_completer.deactivate();
                        self.handle_input_event_with_history(key);
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
                    self.interaction.cancel_pending = None;
                    self.interaction.esc_pending = None;
                } else if !self.input_is_empty() {
                    // Double-Esc to clear input
                    if let Some(when) = self.interaction.esc_pending
                        && when.elapsed() <= CANCEL_WINDOW
                    {
                        self.clear_input();
                        self.interaction.esc_pending = None;
                    } else {
                        self.interaction.esc_pending = Some(Instant::now());
                    }
                }
            }
            // Ctrl+C: Clear input (single), quit (double when idle)
            // Note: Esc cancels agent, Ctrl+C does not
            KeyCode::Char('c') if ctrl => {
                if !self.input_is_empty() {
                    self.clear_input();
                    self.interaction.cancel_pending = None;
                } else if !self.is_running {
                    // Only quit when idle (double-tap)
                    if let Some(when) = self.interaction.cancel_pending
                        && when.elapsed() <= CANCEL_WINDOW
                    {
                        self.quit();
                        self.interaction.cancel_pending = None;
                    } else {
                        self.interaction.cancel_pending = Some(Instant::now());
                    }
                }
            }

            // Ctrl+D: Quit if input empty (double-tap required, like Ctrl+C)
            KeyCode::Char('d') if ctrl => {
                if self.input_is_empty() {
                    if let Some(when) = self.interaction.cancel_pending
                        && when.elapsed() <= CANCEL_WINDOW
                    {
                        self.quit();
                        self.interaction.cancel_pending = None;
                    } else {
                        self.interaction.cancel_pending = Some(Instant::now());
                    }
                }
            }

            // Shift+Tab: Cycle tool mode (Read ↔ Write)
            KeyCode::BackTab => {
                self.tool_mode = match self.tool_mode {
                    ToolMode::Read => ToolMode::Write,
                    ToolMode::Write => ToolMode::Read,
                };
                crate::tool::builtin::spawn_subagent::set_shared_mode(
                    &self.shared_tool_mode,
                    self.tool_mode,
                );
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

            // Ctrl+P: Open provider picker (history still available via Up/Down)
            KeyCode::Char('p') if ctrl => {
                if !self.is_running {
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
                self.interaction.editor_requested = true;
            }

            // Ctrl+R: Open history search
            KeyCode::Char('r') if ctrl => {
                if !self.input_history.is_empty() {
                    self.history_search.clear();
                    self.history_search.update_matches(&self.input_history);
                    self.mode = Mode::HistorySearch;
                }
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
                            self.dispatch_slash_command(&input);
                            return;
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
                self.update_command_completer_query();
            }
        }
    }

    /// Dispatch a slash command (e.g., /compact, /model, /clear).
    fn dispatch_slash_command(&mut self, input: &str) {
        const COMMANDS: [&str; 8] =
            ["/compact", "/cost", "/model", "/provider", "/clear", "/quit", "/help", "/resume"];

        let cmd_line = input.trim().to_lowercase();
        let cmd_name = cmd_line.split_whitespace().next().unwrap_or("");

        self.clear_input();
        self.history_index = self.input_history.len();

        match cmd_name {
            "/compact" => {
                let modified = self.agent.compact_messages(&mut self.session.messages);
                if modified > 0 {
                    self.last_error = None;
                    self.message_list.push_entry(MessageEntry::new(
                        Sender::System,
                        format!("Compacted: pruned {modified} tool outputs"),
                    ));
                    let _ = self.store.save(&self.session);
                } else {
                    self.message_list.push_entry(MessageEntry::new(
                        Sender::System,
                        "Nothing to compact".to_string(),
                    ));
                }
            }
            "/cost" => {
                let msg = if self.session_cost > 0.0 {
                    let p = &self.model_pricing;
                    let mut parts = vec![format!("Session cost: {}", format_cost(self.session_cost))];
                    if p.input > 0.0 || p.output > 0.0 {
                        parts.push(format!(
                            "Pricing: {}/M input, {}/M output",
                            format_cost(p.input),
                            format_cost(p.output),
                        ));
                    }
                    parts.join(" · ")
                } else {
                    "No cost data yet (pricing available after model list loads)".to_string()
                };
                self.message_list
                    .push_entry(MessageEntry::new(Sender::System, msg));
            }
            "/model" | "/models" => self.open_model_selector(),
            "/provider" | "/providers" => self.open_provider_selector(),
            "/resume" | "/sessions" => self.open_session_selector(),
            "/quit" | "/exit" | "/q" => self.quit(),
            "/clear" => {
                if !self.session.messages.is_empty() {
                    let _ = self.store.save(&self.session);
                }
                let working_dir = self.session.working_dir.clone();
                let model = self.session.model.clone();
                let provider = self.session.provider.clone();
                let no_sandbox = self.session.no_sandbox;
                self.session = Session::new(working_dir, model);
                self.session.provider = provider;
                self.session.no_sandbox = no_sandbox;
                self.session_cost = 0.0;

                self.message_list.clear();
                self.render_state.reset_for_new_conversation();
                self.render_state.needs_screen_clear = true;

                let agent = self.agent.clone();
                tokio::spawn(async move {
                    agent.clear_plan().await;
                });
            }
            "/help" | "/?" => {
                self.mode = Mode::HelpOverlay;
            }
            _ => {
                if !cmd_name.is_empty() {
                    let suggestions =
                        fuzzy::top_matches(cmd_name, COMMANDS.iter().copied(), 3);
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
            }
        }
    }

    /// Handle selector mode key events.
    #[allow(clippy::too_many_lines)]
    pub(super) fn handle_selector_mode(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let mut handled = true;

        match key.code {
            // Ctrl+C/Ctrl+D: Close selector, double-tap to quit
            KeyCode::Char('c' | 'd') if ctrl => {
                if let Some(when) = self.interaction.cancel_pending
                    && when.elapsed() <= CANCEL_WINDOW
                {
                    self.should_quit = true;
                    self.interaction.cancel_pending = None;
                } else {
                    self.interaction.cancel_pending = Some(Instant::now());
                    if self.needs_setup && self.selector_page == SelectorPage::Model {
                        self.model_picker.reset();
                        self.open_provider_selector();
                    } else {
                        self.model_picker.reset();
                        self.exit_selector_mode();
                    }
                }
            }

            // Navigation - dispatch to active picker
            KeyCode::Up => self.dispatch_picker(|p| p.move_up(1)),
            KeyCode::Down => self.dispatch_picker(|p| p.move_down(1)),
            KeyCode::PageUp => self.dispatch_picker(|p| p.move_up(10)),
            KeyCode::PageDown => self.dispatch_picker(|p| p.move_down(10)),
            KeyCode::Home => self.dispatch_picker(|p| p.jump_to_top()),
            KeyCode::End => self.dispatch_picker(|p| p.jump_to_bottom()),

            // Selection
            KeyCode::Enter => match self.selector_page {
                SelectorPage::Provider => {
                    if let Some(status) = self.provider_picker.selected()
                        && status.authenticated
                    {
                        let provider = status.provider;
                        // Defer provider change until model is selected
                        // Only set now if it's the same provider (just opening model selector)
                        if provider == self.api_provider {
                            self.open_model_selector();
                        } else {
                            // Store pending provider and preview its models
                            self.pending_provider = Some(provider);
                            self.preview_provider_models(provider);
                        }
                    }
                }
                SelectorPage::Model => match self.model_picker.stage {
                    PickerStage::Provider => {
                        self.model_picker.select_provider();
                    }
                    PickerStage::Model => {
                        // Clone model data to avoid borrow conflict with set_provider
                        let model_data = self.model_picker.selected_model().map(|m| {
                            (m.id.clone(), m.context_window, m.supports_vision, m.pricing.clone())
                        });
                        if let Some((model_id, context_window, vision, pricing)) = model_data {
                            // Commit pending provider change now that model is selected
                            if let Some(provider) = self.pending_provider.take()
                                && let Err(err) = self.set_provider(provider)
                            {
                                self.last_error = Some(err.to_string());
                                self.exit_selector_mode();
                                return;
                            }

                            self.session.model = model_id.clone();
                            self.session.provider = self.api_provider.id().to_string();
                            self.model_pricing = pricing;
                            self.agent.set_supports_vision(vision);
                            if context_window > 0 {
                                let ctx_window = context_window as usize;
                                self.model_context_window = Some(ctx_window);
                                // Update agent's compaction config
                                self.agent.set_context_window(ctx_window);
                            } else {
                                self.model_context_window = None;
                            }
                            // Persist selection to config
                            self.config.model = Some(model_id);
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
                    handle_filter_input_event(self.provider_picker.filter_input_mut(), key)
                }
                SelectorPage::Model => {
                    handle_filter_input_event(&mut self.model_picker.filter_input, key)
                }
                SelectorPage::Session => {
                    handle_filter_input_event(self.session_picker.filter_input_mut(), key)
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

    /// Exit selector mode and return to input.
    pub(super) fn exit_selector_mode(&mut self) {
        self.mode = Mode::Input;
        // Clear any pending provider change (user cancelled)
        if self.pending_provider.take().is_some() {
            // Reset model picker to current provider's models
            self.model_picker.set_api_provider(self.api_provider.name());
        }
        // Mark that we need to clear the selector area (not full screen repaint)
        self.render_state.needs_selector_clear = true;
    }

    /// Dispatch a navigation action to the active picker.
    fn dispatch_picker(&mut self, action: impl FnOnce(&mut dyn PickerNavigation)) {
        match self.selector_page {
            SelectorPage::Provider => action(&mut self.provider_picker),
            SelectorPage::Model => action(&mut self.model_picker),
            SelectorPage::Session => action(&mut self.session_picker),
        }
    }

    /// Handle key events in history search mode (Ctrl+R).
    fn handle_history_search_mode(&mut self, key: KeyEvent) {
        match key.code {
            // Escape or Ctrl+C: Cancel search
            KeyCode::Esc => {
                self.history_search.clear();
                self.mode = Mode::Input;
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.history_search.clear();
                self.mode = Mode::Input;
            }

            // Enter: Select current match and insert into input
            KeyCode::Enter => {
                if let Some(idx) = self.history_search.selected_entry()
                    && let Some(entry) = self.input_history.get(idx).cloned()
                {
                    self.set_input_text(&entry);
                }
                self.history_search.clear();
                self.mode = Mode::Input;
            }

            // Ctrl+R or Up: Select previous (older) match
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.history_search.select_next();
            }
            KeyCode::Up => {
                self.history_search.select_next();
            }
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.history_search.select_next();
            }

            // Down or Ctrl+N: Select next (newer) match
            KeyCode::Down => {
                self.history_search.select_prev();
            }
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.history_search.select_prev();
            }

            // Backspace: Remove last char from query
            KeyCode::Backspace => {
                self.history_search.query.pop();
                self.history_search.update_matches(&self.input_history);
            }

            // Regular character: Add to query
            KeyCode::Char(c) => {
                self.history_search.query.push(c);
                self.history_search.update_matches(&self.input_history);
            }

            _ => {}
        }
    }
}
