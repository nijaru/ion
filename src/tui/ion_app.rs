//! IonApp — ion's App implementation for crates/tui.
//!
//! Wraps the existing `App` struct for all business logic (agent, session,
//! orchestrator) and bridges its data model to the new `crates/tui` widget
//! layer. Agent events arrive via periodic `Tick` messages that drain
//! `inner.agent_rx` through `inner.update()`, then `sync_scrollback()`
//! incrementally pushes new entries to native terminal scrollback.

use std::time::{Duration, Instant};

use tui::{
    Col, Element, Input, InputAction, InputState, IntoElement,
    app::{App as TuiApp, Effect},
    event::{Event, KeyCode, KeyModifiers},
    geometry::Rect,
    layout::Dimension,
    terminal::RenderMode,
};

use crate::cli::PermissionSettings;
use crate::session::Session;
use crate::tool::ToolMode;
use crate::tui::{
    App as IonState, PickerNavigation, ResumeOption, SelectorPage,
    chat_renderer::ChatRenderer,
    fuzzy,
    message::IonMsg,
    message_list::{MessageEntry, Sender},
    model_picker::PickerStage,
};
use crate::ui::StatusBar;

use crate::tui::types::CANCEL_WINDOW;

/// Threshold for storing paste as blob: >5 lines or >500 chars
const PASTE_BLOB_LINE_THRESHOLD: usize = 5;
const PASTE_BLOB_CHAR_THRESHOLD: usize = 500;

// ── AppMode ──────────────────────────────────────────────────────────────────

/// Modal state for the IonApp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum AppMode {
    /// Normal chat mode — input focused.
    #[default]
    Input,
    /// Ctrl+M — model picker overlay.
    ModelPicker,
    /// Ctrl+P — provider picker overlay.
    ProviderPicker,
    /// /resume — session picker overlay.
    SessionPicker,
    /// Ctrl+H — help overlay.
    Help,
    /// Ctrl+R — incremental history search.
    HistorySearch,
    /// OAuth ban-risk confirmation dialog.
    OAuthConfirm,
}

// ── IonApp ────────────────────────────────────────────────────────────────────

pub struct IonApp {
    pub(crate) inner: IonState,
    input: InputState,
    status: StatusBar,
    mode: AppMode,
    width: u16,
    height: u16,
    /// Cached input area rect from last render (for cursor positioning).
    input_area: Rect,
    /// Timestamp of last Ctrl+C / Ctrl+D press for double-tap quit detection.
    last_cancel_at: Option<Instant>,
    /// Timestamp of last Esc press for double-tap clear detection.
    last_esc_at: Option<Instant>,

    // ── Scrollback tracking ─────────────────────────────────────────────────
    /// Number of message_list entries already printed to scrollback.
    scrollback_entry_count: usize,
    /// Lines from the in-progress streaming entry already committed to scrollback.
    streaming_committed_lines: usize,
    /// Buffered lines to insert above the inline region on next render.
    pending_scrollback: Vec<String>,
    /// Whether the startup header has been printed to scrollback.
    header_printed: bool,
    /// Content length of the last streaming entry (detect incremental updates).
    last_streaming_len: usize,
}

impl IonApp {
    pub async fn new(permissions: PermissionSettings) -> anyhow::Result<Self> {
        let mut inner = IonState::with_permissions(permissions).await?;
        // Disable first-time setup flow — API config is via config file or
        // environment variables when using the new TUI.
        inner.needs_setup = false;

        let (width, height) = crossterm::terminal::size().unwrap_or((80, 24));

        Ok(Self {
            inner,
            input: InputState::new(),
            status: StatusBar::new(),
            mode: AppMode::default(),
            width,
            height,
            input_area: Rect::default(),
            last_cancel_at: None,
            last_esc_at: None,
            scrollback_entry_count: 0,
            streaming_committed_lines: 0,
            pending_scrollback: Vec::new(),
            header_printed: false,
            last_streaming_len: 0,
        })
    }

    /// Apply a resume option, loading session state into `inner` and queuing
    /// the loaded history to scrollback.
    pub fn apply_resume(&mut self, resume_option: ResumeOption) {
        match resume_option {
            ResumeOption::None | ResumeOption::Selector => {}
            ResumeOption::Latest => {
                let cwd = std::env::current_dir()
                    .unwrap_or_default()
                    .display()
                    .to_string();
                match self.inner.store.list_recent_for_dir(&cwd, 1) {
                    Ok(sessions) => {
                        if let Some(session) = sessions.first() {
                            if let Err(e) = self.inner.load_session(&session.id) {
                                self.inner.message_list.push_entry(MessageEntry::new(
                                    Sender::System,
                                    format!("Error: Failed to load session: {e}"),
                                ));
                            }
                        }
                    }
                    Err(e) => {
                        self.inner.message_list.push_entry(MessageEntry::new(
                            Sender::System,
                            format!("Error: Failed to list sessions: {e}"),
                        ));
                    }
                }
            }
            ResumeOption::ById(ref id) => {
                if let Err(e) = self.inner.load_session(id) {
                    self.inner.message_list.push_entry(MessageEntry::new(
                        Sender::System,
                        format!("Error: Session '{}' not found: {e}", id),
                    ));
                }
            }
        }
        self.reset_scrollback();
    }

    // ── Scrollback sync ──────────────────────────────────────────────────────

    /// Reset scrollback tracking and queue all existing entries for re-print.
    fn reset_scrollback(&mut self) {
        self.scrollback_entry_count = 0;
        self.streaming_committed_lines = 0;
        self.last_streaming_len = 0;
        self.header_printed = false;
        self.pending_scrollback.clear();
    }

    /// Queue the startup header lines for scrollback insertion.
    fn queue_startup_header(&mut self) {
        if self.header_printed {
            return;
        }
        self.header_printed = true;
        let header_lines = IonState::startup_header_lines(&self.inner.session.working_dir);
        for line in &header_lines {
            self.pending_scrollback
                .push(line.to_ansi_string_with_width(self.width));
        }
    }

    /// Incrementally sync new entries from `inner.message_list` to scrollback.
    /// Handles both stable (completed) entries and the actively streaming last entry.
    fn sync_scrollback(&mut self) {
        // Ensure startup header is queued first.
        self.queue_startup_header();

        let width = self.width;
        let entry_count = self.inner.message_list.entries.len();

        // All entries except the last are "stable" while streaming.
        let stable_count = if self.inner.is_running
            && self
                .inner
                .message_list
                .entries
                .last()
                .is_some_and(|e| e.sender == Sender::Agent)
        {
            entry_count.saturating_sub(1)
        } else {
            entry_count
        };

        // Push all new stable entries to scrollback.
        while self.scrollback_entry_count < stable_count {
            let idx = self.scrollback_entry_count;
            let entry = &self.inner.message_list.entries[idx];
            let lines = render_entry_lines_for_scrollback(entry, width);
            // Blank separator between entries
            if idx > 0 || self.header_printed {
                self.pending_scrollback.push(String::new());
            }
            for line in &lines {
                self.pending_scrollback
                    .push(line.to_ansi_string_with_width(width));
            }
            self.scrollback_entry_count += 1;
        }

        // Handle the actively streaming last entry — commit new lines incrementally.
        if self.inner.is_running && entry_count > 0 {
            let last = &self.inner.message_list.entries[entry_count - 1];
            if last.sender == Sender::Agent {
                let content = last.content_as_markdown().to_owned();
                let new_len = content.len();
                let is_new_entry = self.scrollback_entry_count < entry_count;

                if is_new_entry || new_len != self.last_streaming_len {
                    let streaming_entry = MessageEntry::new(Sender::Agent, content.clone());
                    let all_lines = render_entry_lines_for_scrollback(&streaming_entry, width);

                    // Add separator if this is the first time we see this entry
                    if is_new_entry && self.streaming_committed_lines == 0 {
                        self.pending_scrollback.push(String::new());
                        self.scrollback_entry_count += 1;
                    }

                    // Commit lines beyond what we've already committed.
                    // Keep the last line uncommitted (it may still be growing).
                    let committable = if all_lines.len() > 1 {
                        all_lines.len() - 1
                    } else {
                        0
                    };

                    if committable > self.streaming_committed_lines {
                        for line in &all_lines[self.streaming_committed_lines..committable] {
                            self.pending_scrollback
                                .push(line.to_ansi_string_with_width(width));
                        }
                        self.streaming_committed_lines = committable;
                    }

                    self.last_streaming_len = new_len;
                }
            }
        }

        // When streaming finishes, commit any remaining lines.
        if !self.inner.is_running && self.streaming_committed_lines > 0 {
            let final_idx = self.scrollback_entry_count.saturating_sub(1);
            if let Some(entry) = self.inner.message_list.entries.get(final_idx)
                && entry.sender == Sender::Agent
            {
                let content = entry.content_as_markdown().to_owned();
                let streaming_entry = MessageEntry::new(Sender::Agent, content);
                let all_lines = render_entry_lines_for_scrollback(&streaming_entry, width);

                if all_lines.len() > self.streaming_committed_lines {
                    for line in &all_lines[self.streaming_committed_lines..] {
                        self.pending_scrollback
                            .push(line.to_ansi_string_with_width(width));
                    }
                }
            }
            self.streaming_committed_lines = 0;
            self.last_streaming_len = 0;
        }
    }

    fn sync_status(&mut self) {
        let model = self.inner.session.model.clone();
        let branch = self.inner.git_branch.clone();
        let mode = match self.inner.tool_mode {
            ToolMode::Write => "write",
            ToolMode::Read => "read",
        };
        let tokens = self
            .inner
            .token_usage
            .map(|(used, max)| format!("{}/{}", fmt_compact(used), fmt_compact(max)));
        let cost =
            (self.inner.session_cost > 0.001).then(|| format!("${:.2}", self.inner.session_cost));

        self.status = StatusBar {
            model: (!model.is_empty()).then_some(model),
            tokens,
            cost,
            branch,
            mode: Some(mode.to_string()),
        };
    }

    // ── Slash commands ───────────────────────────────────────────────────────

    fn handle_slash_command(&mut self, input: &str) -> Effect<IonMsg> {
        const COMMANDS: [&str; 9] = [
            "/compact",
            "/cost",
            "/export",
            "/model",
            "/provider",
            "/clear",
            "/quit",
            "/help",
            "/resume",
        ];

        let trimmed = input.trim();
        let cmd_line = trimmed.to_lowercase();
        let cmd_name = cmd_line.split_whitespace().next().unwrap_or("");

        // Handle //skill-name [args] skill invocation (preserve original case)
        if trimmed.starts_with("//") {
            let skill_input = trimmed.strip_prefix("//").unwrap_or("").trim();
            if !skill_input.is_empty() {
                let (name, args) = skill_input.split_once(' ').unwrap_or((skill_input, ""));
                let agent = self.inner.agent.clone();
                let name = name.to_string();
                let args = args.to_string();
                let display_name = name.clone();
                tokio::spawn(async move {
                    if let Err(e) = agent.activate_skill_with_args(&name, &args).await {
                        tracing::warn!("Failed to activate skill '{name}': {e}");
                    }
                });
                self.inner.message_list.push_entry(MessageEntry::new(
                    Sender::System,
                    format!("Skill active: {display_name}"),
                ));
                self.sync_scrollback();
            }
            return Effect::None;
        }

        match cmd_name {
            "/compact" => {
                let modified = self
                    .inner
                    .agent
                    .compact_messages(&mut self.inner.session.messages);
                if modified > 0 {
                    self.inner.last_error = None;
                    self.inner.message_list.push_entry(MessageEntry::new(
                        Sender::System,
                        format!("Compacted: pruned {modified} tool outputs"),
                    ));
                    let _ = self.inner.store.save(&self.inner.session);
                    self.inner
                        .persist_display_entries(&self.inner.session.id.clone());
                } else {
                    self.inner.message_list.push_entry(MessageEntry::new(
                        Sender::System,
                        "Nothing to compact".to_string(),
                    ));
                }
                self.sync_scrollback();
            }
            "/cost" => {
                let msg = if self.inner.api_provider.is_oauth() {
                    "Subscription provider — no per-token cost".to_string()
                } else if self.inner.session_cost > 0.0 {
                    let p = &self.inner.model_pricing;
                    let mut parts = vec![format!("Session cost: ${:.4}", self.inner.session_cost)];
                    if p.input > 0.0 || p.output > 0.0 {
                        parts.push(format!(
                            "Pricing: ${:.2}/M input, ${:.2}/M output",
                            p.input, p.output,
                        ));
                    }
                    parts.join(" | ")
                } else {
                    "No cost data yet (pricing available after model list loads)".to_string()
                };
                self.inner
                    .message_list
                    .push_entry(MessageEntry::new(Sender::System, msg));
                self.sync_scrollback();
            }
            "/export" => {
                self.inner.export_session_markdown();
                self.sync_scrollback();
            }
            "/model" | "/models" => {
                self.inner.open_model_selector();
                self.mode = AppMode::ModelPicker;
            }
            "/provider" | "/providers" => {
                self.inner.open_provider_selector();
                self.mode = AppMode::ProviderPicker;
            }
            "/resume" | "/sessions" => {
                self.inner.open_session_selector();
                self.mode = AppMode::SessionPicker;
            }
            "/quit" | "/exit" | "/q" => {
                self.inner.quit();
                return Effect::Quit;
            }
            "/clear" => {
                if !self.inner.session.messages.is_empty() {
                    let id = self.inner.session.id.clone();
                    let _ = self.inner.store.save(&self.inner.session);
                    self.inner.persist_display_entries(&id);
                }
                let working_dir = self.inner.session.working_dir.clone();
                let model = self.inner.session.model.clone();
                let provider = self.inner.session.provider.clone();
                let no_sandbox = self.inner.session.no_sandbox;
                self.inner.session = Session::new(working_dir, model);
                self.inner.session.provider = provider;
                self.inner.session.no_sandbox = no_sandbox;
                self.inner.session_cost = 0.0;
                self.inner.refresh_startup_header_cache();
                self.inner.message_list.clear();
                self.reset_scrollback();
            }
            "/help" | "/?" => {
                self.mode = AppMode::Help;
            }
            _ => {
                if !cmd_name.is_empty() {
                    let suggestions = fuzzy::top_matches(cmd_name, COMMANDS.iter().copied(), 3);
                    let message = if suggestions.is_empty() {
                        format!("Unknown command {cmd_name}")
                    } else {
                        format!(
                            "Unknown command {}. Did you mean {}?",
                            cmd_name,
                            suggestions.join(", ")
                        )
                    };
                    self.inner
                        .message_list
                        .push_entry(MessageEntry::new(Sender::System, message));
                    self.sync_scrollback();
                }
            }
        }
        Effect::None
    }

    // ── Picker helpers ───────────────────────────────────────────────────────

    /// Derive the active selector page from the current mode.
    fn selector_page(&self) -> SelectorPage {
        match self.mode {
            AppMode::ProviderPicker => SelectorPage::Provider,
            AppMode::SessionPicker => SelectorPage::Session,
            // ModelPicker: check if model_picker is in provider or model stage
            AppMode::ModelPicker => {
                if self.inner.model_picker.stage == PickerStage::Provider {
                    SelectorPage::Provider
                } else {
                    SelectorPage::Model
                }
            }
            _ => SelectorPage::Model,
        }
    }

    /// Dispatch a navigation action to the active picker.
    fn dispatch_picker(&mut self, action: impl FnOnce(&mut dyn PickerNavigation)) {
        match self.selector_page() {
            SelectorPage::Provider => action(&mut self.inner.provider_picker),
            SelectorPage::Model => action(&mut self.inner.model_picker),
            SelectorPage::Session => action(&mut self.inner.session_picker),
        }
    }

    /// Handle Enter in a picker.
    fn handle_picker_select(&mut self) {
        // OAuth confirm mode: confirm and preview models
        if self.mode == AppMode::OAuthConfirm {
            if let Some(provider) = self.inner.oauth_confirm_provider.take() {
                self.inner.pending_provider = Some(provider);
                self.inner.preview_provider_models(provider);
                self.mode = AppMode::ModelPicker;
            }
            return;
        }

        match self.selector_page() {
            SelectorPage::Provider => {
                if let Some(status) = self.inner.provider_picker.selected()
                    && status.authenticated
                {
                    let provider = status.provider;
                    if provider == self.inner.api_provider {
                        self.inner.open_model_selector();
                        self.mode = AppMode::ModelPicker;
                    } else if provider == crate::provider::Provider::Gemini {
                        self.inner.oauth_confirm_provider = Some(provider);
                        self.mode = AppMode::OAuthConfirm;
                    } else {
                        self.inner.pending_provider = Some(provider);
                        self.inner.preview_provider_models(provider);
                        self.mode = AppMode::ModelPicker;
                    }
                }
            }
            SelectorPage::Model => match self.inner.model_picker.stage {
                PickerStage::Provider => {
                    self.inner.model_picker.select_provider();
                }
                PickerStage::Model => {
                    let model_data = self.inner.model_picker.selected_model().map(|m| {
                        (
                            m.id.clone(),
                            m.context_window,
                            m.supports_vision,
                            m.pricing.clone(),
                        )
                    });
                    if let Some((model_id, context_window, vision, pricing)) = model_data {
                        if let Some(provider) = self.inner.pending_provider.take()
                            && let Err(err) = self.inner.set_provider(provider)
                        {
                            self.inner.last_error = Some(err.to_string());
                            self.exit_selector_mode();
                            return;
                        }

                        self.inner.session.model = model_id.clone();
                        self.inner.session.provider = self.inner.api_provider.id().to_string();
                        self.inner.model_pricing = pricing;
                        self.inner.agent.set_supports_vision(vision);
                        if context_window > 0 {
                            let ctx_window = context_window as usize;
                            self.inner.model_context_window = Some(ctx_window);
                            self.inner.agent.set_context_window(ctx_window);
                        } else {
                            self.inner.model_context_window = None;
                        }
                        self.inner.config.model = Some(model_id);
                        if let Err(e) = self.inner.config.save() {
                            tracing::warn!("Failed to save config: {}", e);
                        }
                        self.inner.model_picker.reset();
                        if self.inner.needs_setup {
                            self.inner.needs_setup = false;
                        }
                        self.exit_selector_mode();
                        self.sync_status();
                    }
                }
            },
            SelectorPage::Session => {
                if let Some(summary) = self.inner.session_picker.selected_session() {
                    let session_id = summary.id.clone();
                    let loaded = if let Err(e) = self.inner.load_session(&session_id) {
                        self.inner.last_error = Some(format!("Failed to load session: {e}"));
                        false
                    } else {
                        true
                    };
                    self.inner.session_picker.reset();
                    self.exit_selector_mode();
                    if loaded {
                        self.reset_scrollback();
                    }
                }
            }
        }
    }

    /// Handle filter key input in a picker.
    fn handle_picker_filter_key(&mut self, key: &tui::event::KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CTRL);
        let filter = match self.selector_page() {
            SelectorPage::Provider => self.inner.provider_picker.filter_input_mut(),
            SelectorPage::Model => &mut self.inner.model_picker.filter_input,
            SelectorPage::Session => self.inner.session_picker.filter_input_mut(),
        };

        let text_changed = match key.code {
            KeyCode::Char('w') if ctrl => {
                filter.delete_word();
                true
            }
            KeyCode::Char('u') if ctrl => {
                filter.delete_line_left();
                true
            }
            KeyCode::Char(c) => {
                filter.insert_char(c);
                true
            }
            KeyCode::Backspace => {
                filter.delete_char_before();
                true
            }
            _ => false,
        };

        if text_changed {
            match self.selector_page() {
                SelectorPage::Provider => self.inner.provider_picker.apply_filter(),
                SelectorPage::Model => self.inner.model_picker.apply_filter(),
                SelectorPage::Session => self.inner.session_picker.apply_filter(),
            }
        }
    }

    /// Exit selector mode and return to input.
    fn exit_selector_mode(&mut self) {
        self.mode = AppMode::Input;
        if self.inner.pending_provider.take().is_some() {
            self.inner
                .model_picker
                .set_api_provider(self.inner.api_provider.name());
        }
    }

    // ── Input history helpers ────────────────────────────────────────────────

    /// Handle Up arrow: cursor movement, queue recall, or history.
    fn handle_input_up(&mut self) -> bool {
        let input_empty = self.input.is_empty();

        // When running with empty input, recall queued messages
        if self.inner.is_running && input_empty {
            let queued = self.inner.message_queue.as_ref().and_then(|queue| {
                let mut q = match queue.lock() {
                    Ok(q) => q,
                    Err(poisoned) => poisoned.into_inner(),
                };
                if q.is_empty() {
                    None
                } else {
                    Some(q.drain(..).collect::<Vec<_>>())
                }
            });

            if let Some(queued) = queued {
                self.input.set_value(&queued.join("\n\n"));
                return true;
            }
        }

        // Navigate to previous history entry
        self.handle_prev_history()
    }

    /// Handle Down arrow: cursor movement or history navigation.
    fn handle_input_down(&mut self) -> bool {
        self.handle_next_history()
    }

    /// Navigate to previous history entry.
    fn handle_prev_history(&mut self) -> bool {
        if self.inner.input_history.is_empty() {
            return false;
        }

        // Save current input as draft before navigating
        if self.inner.history_index == self.inner.input_history.len()
            && self.inner.history_draft.is_none()
        {
            self.inner.history_draft = Some(self.input.value());
        }

        if self.inner.history_index > 0 {
            self.inner.history_index -= 1;
            let entry = self.inner.input_history[self.inner.history_index].clone();
            self.input.set_value(&entry);
            true
        } else {
            false
        }
    }

    /// Navigate to next history entry.
    fn handle_next_history(&mut self) -> bool {
        if self.inner.history_index < self.inner.input_history.len() {
            self.inner.history_index += 1;
            if self.inner.history_index == self.inner.input_history.len() {
                if let Some(draft) = self.inner.history_draft.take() {
                    self.input.set_value(&draft);
                } else {
                    self.input.clear();
                }
            } else {
                let entry = self.inner.input_history[self.inner.history_index].clone();
                self.input.set_value(&entry);
            }
            true
        } else {
            false
        }
    }

    // ── Completer helpers ────────────────────────────────────────────────────

    /// Handle key events when command completer is active.
    fn handle_command_completer_key(&mut self, k: &tui::event::KeyEvent) -> Effect<IonMsg> {
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let ctrl = k.modifiers.contains(KeyModifiers::CTRL);
        match k.code {
            KeyCode::Up => {
                self.inner.command_completer.move_up();
            }
            KeyCode::Down => {
                self.inner.command_completer.move_down();
            }
            KeyCode::Tab | KeyCode::Enter if !shift => {
                if let Some(cmd) = self.inner.command_completer.selected_command() {
                    let cmd_with_space = format!("{cmd} ");
                    self.input.clear();
                    self.input.insert_text(&cmd_with_space);
                }
                self.inner.command_completer.deactivate();
            }
            KeyCode::Esc => {
                self.inner.command_completer.deactivate();
            }
            KeyCode::Backspace => {
                let value = self.input.value();
                let cursor = value.chars().count();
                if cursor <= 1 && !self.inner.command_completer.is_skill_mode() {
                    self.inner.command_completer.deactivate();
                    self.input.handle_key(k);
                } else {
                    self.input.handle_key(k);
                    self.update_command_completer_query();
                }
            }
            KeyCode::Char(_) if !ctrl => {
                self.input.handle_key(k);
                self.update_command_completer_query();
            }
            _ => {
                self.inner.command_completer.deactivate();
                self.input.handle_key(k);
            }
        }
        Effect::None
    }

    /// Handle key events when file completer is active.
    fn handle_file_completer_key(&mut self, k: &tui::event::KeyEvent) -> Effect<IonMsg> {
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let ctrl = k.modifiers.contains(KeyModifiers::CTRL);
        match k.code {
            KeyCode::Up => {
                self.inner.file_completer.move_up();
            }
            KeyCode::Down => {
                self.inner.file_completer.move_down();
            }
            KeyCode::Tab | KeyCode::Enter if !shift => {
                if let Some(path) = self.inner.file_completer.selected_path() {
                    let at_pos = self.inner.file_completer.at_position();
                    let path_str = path.to_string_lossy().to_string();
                    let is_dir = path.is_dir();
                    let has_spaces = path_str.contains(' ');

                    // Rebuild input: keep text before @, replace @query with path
                    let current = self.input.value();
                    let before: String = current.chars().take(at_pos).collect();
                    let full_path = if has_spaces {
                        format!("@\"{path_str}\" ")
                    } else if is_dir {
                        format!("@{path_str}/ ")
                    } else {
                        format!("@{path_str} ")
                    };
                    let new_value = format!("{before}{full_path}");
                    self.input.set_value(&new_value);
                }
                self.inner.file_completer.deactivate();
            }
            KeyCode::Esc => {
                self.inner.file_completer.deactivate();
            }
            KeyCode::Backspace => {
                let at_pos = self.inner.file_completer.at_position();
                let value = self.input.value();
                let cursor = value.chars().count();
                if cursor <= at_pos + 1 {
                    self.inner.file_completer.deactivate();
                    self.input.handle_key(k);
                } else {
                    self.input.handle_key(k);
                    self.update_file_completer_query();
                }
            }
            KeyCode::Char(_) if !ctrl => {
                self.input.handle_key(k);
                self.update_file_completer_query();
            }
            _ => {
                self.inner.file_completer.deactivate();
                self.input.handle_key(k);
            }
        }
        Effect::None
    }

    /// Update the file completer query from current input.
    fn update_file_completer_query(&mut self) {
        if !self.inner.file_completer.is_active() {
            return;
        }
        let at_pos = self.inner.file_completer.at_position();
        let value = self.input.value();
        let cursor = value.chars().count();
        if cursor > at_pos + 1 {
            let query: String = value
                .chars()
                .skip(at_pos + 1)
                .take(cursor - at_pos - 1)
                .collect();
            self.inner.file_completer.set_query(&query);
        } else {
            self.inner.file_completer.set_query("");
        }
    }

    /// Update the command completer query from current input.
    fn update_command_completer_query(&mut self) {
        if !self.inner.command_completer.is_active() {
            return;
        }
        let value = self.input.value();
        let cursor = value.chars().count();

        if value.starts_with("//") {
            if !self.inner.command_completer.is_skill_mode() {
                self.inner.command_completer.activate_skill_mode();
            }
            if cursor > 2 {
                let query: String = value.chars().skip(2).take(cursor - 2).collect();
                self.inner.command_completer.set_query(&query);
            } else {
                self.inner.command_completer.set_query("");
            }
        } else if value.starts_with('/') {
            if self.inner.command_completer.is_skill_mode() {
                self.inner.command_completer.activate_builtin_mode();
            }
            if cursor == 0 {
                self.inner.command_completer.deactivate();
                return;
            }
            if cursor > 1 {
                let query: String = value.chars().skip(1).take(cursor - 1).collect();
                self.inner.command_completer.set_query(&query);
            } else {
                self.inner.command_completer.set_query("");
            }
        } else {
            self.inner.command_completer.deactivate();
        }
    }

    /// Check if we should activate file completion (@ at word boundary).
    fn check_activate_file_completer(&mut self) {
        let value = self.input.value();
        let chars: Vec<char> = value.chars().collect();
        let cursor = chars.len(); // cursor at end after inserting @

        if cursor == 0 {
            return;
        }
        if chars.get(cursor - 1) == Some(&'@') {
            let is_at_boundary =
                cursor == 1 || chars.get(cursor - 2).is_none_or(|c| c.is_whitespace());
            if is_at_boundary {
                self.inner.file_completer.activate(cursor - 1);
            }
        }
    }

    /// Check if we should activate command completion (/ at start).
    fn check_activate_command_completer(&mut self) {
        let value = self.input.value();
        let cursor = value.chars().count();

        if cursor == 1 && value.starts_with('/') && !value.starts_with("//") {
            self.inner.command_completer.activate();
        }
    }

    // ── View methods ─────────────────────────────────────────────────────────

    /// Bottom-UI only view (status bar + input). Chat goes to native scrollback.
    fn view_bottom_ui(&mut self) -> Element {
        let width = self.width;
        let input_height = self.input.line_count().max(1) as u16;
        let status_height: u16 = 1;
        self.input_area = Rect::new(0, status_height, width, input_height);

        Col::new(vec![
            self.status
                .view(width)
                .height(Dimension::Cells(status_height))
                .flex_grow(0.0)
                .flex_shrink(0.0),
            Input::new(&self.input)
                .placeholder("Type a message... (Enter to send)")
                .into_element()
                .height(Dimension::Cells(input_height))
                .flex_grow(0.0)
                .flex_shrink(0.0),
        ])
        .into_element()
    }

    /// Picker overlay view (model/provider/session).
    fn view_picker(&mut self) -> Element {
        use tui::{Canvas, Style, buffer::Buffer};

        let title = match self.mode {
            AppMode::ModelPicker => {
                if self.inner.model_picker.stage == PickerStage::Provider {
                    "Select Provider (for models)"
                } else {
                    "Select Model"
                }
            }
            AppMode::ProviderPicker => "Select Provider",
            AppMode::SessionPicker => "Resume Session",
            _ => "",
        };

        let filter_text = match self.selector_page() {
            SelectorPage::Provider => self.inner.provider_picker.filter_input().text().to_owned(),
            SelectorPage::Model => self.inner.model_picker.filter_input.text().to_owned(),
            SelectorPage::Session => self.inner.session_picker.filter_input().text().to_owned(),
        };

        // Build item strings + selected index
        let (items, selected_idx): (Vec<String>, Option<usize>) = match self.selector_page() {
            SelectorPage::Provider => {
                let entries = self.inner.provider_picker.filtered();
                let items: Vec<String> = entries
                    .iter()
                    .map(|s| {
                        let auth = if s.authenticated { " [ok]" } else { "" };
                        format!("  {}{auth}", s.provider.name())
                    })
                    .collect();
                let sel = self.inner.provider_picker.list_state().selected();
                (items, sel)
            }
            SelectorPage::Model => match self.inner.model_picker.stage {
                PickerStage::Provider => {
                    let entries = &self.inner.model_picker.filtered_providers;
                    let items: Vec<String> = entries
                        .iter()
                        .map(|p| format!("  {} ({} models)", p.name, p.model_count))
                        .collect();
                    let sel = self.inner.model_picker.provider_state.selected();
                    (items, sel)
                }
                PickerStage::Model => {
                    let entries = &self.inner.model_picker.filtered_models;
                    let items: Vec<String> = entries
                        .iter()
                        .map(|m| {
                            if m.context_window > 0 {
                                format!("  {} ({}k ctx)", m.id, m.context_window / 1000)
                            } else {
                                format!("  {}", m.id)
                            }
                        })
                        .collect();
                    let sel = self.inner.model_picker.model_state.selected();
                    (items, sel)
                }
            },
            SelectorPage::Session => {
                let entries = self.inner.session_picker.filtered_sessions();
                let items: Vec<String> = entries
                    .iter()
                    .map(|s| {
                        let msg = s.first_user_message.as_deref().unwrap_or("(no messages)");
                        let truncated: String = msg.chars().take(60).collect();
                        format!("  {truncated}")
                    })
                    .collect();
                let sel = self.inner.session_picker.list_state().selected();
                (items, sel)
            }
        };

        let loading = match self.mode {
            AppMode::ModelPicker => self.inner.model_picker.is_loading,
            AppMode::SessionPicker => self.inner.session_picker.is_loading,
            _ => false,
        };

        let tab_hint = match self.mode {
            AppMode::ModelPicker | AppMode::ProviderPicker => {
                "Tab: switch provider/model  Esc: cancel"
            }
            _ => "Esc: cancel",
        };

        Canvas::new(move |area: Rect, buf: &mut Buffer| {
            let dim = Style::default().dim();
            let normal = Style::default();
            let highlight = Style::default().reversed();

            // Title
            buf.set_string(0, 0, title, dim);

            // Filter input
            let filter_line = format!("> {filter_text}");
            buf.set_string(0, 1, &filter_line, normal);

            // Items
            let list_start = 2u16;
            let max_items = (area.height.saturating_sub(list_start + 1)) as usize;

            if loading && items.is_empty() {
                buf.set_string(2, list_start, "Loading...", dim);
            } else if items.is_empty() {
                buf.set_string(2, list_start, "No matches", dim);
            } else {
                // Scroll window around selected item
                let sel = selected_idx.unwrap_or(0);
                let start = if sel >= max_items {
                    sel - max_items + 1
                } else {
                    0
                };
                let end = (start + max_items).min(items.len());

                for (i, item) in items[start..end].iter().enumerate() {
                    let y = list_start + i as u16;
                    if y >= area.height {
                        break;
                    }
                    let style = if start + i == sel { highlight } else { normal };
                    buf.set_string(0, y, item, style);
                }
            }

            // Bottom hint
            let hint_y = area.height.saturating_sub(1);
            buf.set_string(0, hint_y, tab_hint, dim);
        })
        .into_element()
    }

    /// Help overlay view.
    fn view_help(&self) -> Element {
        use tui::{Canvas, Style, buffer::Buffer};

        Canvas::new(move |area: Rect, buf: &mut Buffer| {
            let dim = Style::default().dim();
            let normal = Style::default();

            let lines = [
                ("Keybindings:", true),
                ("  Enter         Send message", false),
                ("  Shift+Enter   New line", false),
                ("  Ctrl+C        Clear input / quit (double-tap)", false),
                ("  Esc           Cancel task / clear (double-tap)", false),
                ("  Shift+Tab     Toggle read/write mode", false),
                ("  Ctrl+M        Select model", false),
                ("  Ctrl+P        Select provider", false),
                ("  Ctrl+T        Cycle thinking level", false),
                ("  Ctrl+O        Toggle tool output", false),
                ("  Ctrl+R        Search history", false),
                ("  Ctrl+G        Open in editor", false),
                ("  Ctrl+H        This help", false),
                ("  @file         File completion", false),
                ("  /command      Slash commands", false),
                ("  //skill       Skill invocation", false),
                ("  !cmd          Shell passthrough", false),
                ("", false),
                ("  Press any key to close", true),
            ];

            for (i, (text, is_dim)) in lines.iter().enumerate() {
                let y = i as u16;
                if y >= area.height {
                    break;
                }
                let style = if *is_dim { dim } else { normal };
                buf.set_string(0, y, text, style);
            }
        })
        .into_element()
    }

    /// History search view — renders in the inline region.
    fn view_history_search(&mut self) -> Element {
        use tui::{Canvas, Style, buffer::Buffer};

        let match_text = self
            .inner
            .history_search
            .selected_entry()
            .and_then(|idx| self.inner.input_history.get(idx))
            .cloned()
            .unwrap_or_default();
        let query = self.inner.history_search.query.clone();
        let match_idx = self.inner.history_search.selected;
        let total_matches = self.inner.history_search.matches.len();

        Canvas::new(move |area: Rect, buf: &mut Buffer| {
            let dim = Style::default().dim();
            let normal = Style::default();

            // Preview of matched entry (top area)
            if !match_text.is_empty() {
                let max_w = area.width as usize;
                let preview_lines = area.height.saturating_sub(1) as usize;
                for (i, line) in match_text.lines().take(preview_lines).enumerate() {
                    let truncated: String = line.chars().take(max_w).collect();
                    buf.set_string(0, i as u16, &truncated, normal);
                }
            } else {
                buf.set_string(0, 0, "  (no match)", dim);
            }

            // Search bar at bottom
            let search_y = area.height.saturating_sub(1);
            let prompt = format!("search: {query}");
            buf.set_string(0, search_y, &prompt, normal);
            if total_matches > 0 {
                let info = format!(" [{}/{}]", match_idx + 1, total_matches);
                let x = prompt.chars().count() as u16;
                buf.set_string(x, search_y, &info, dim);
            }
        })
        .into_element()
    }

    /// OAuth ban-risk confirmation dialog.
    fn view_oauth_confirm(&self) -> Element {
        use tui::{Canvas, Style, buffer::Buffer};

        Canvas::new(move |area: Rect, buf: &mut Buffer| {
            let normal = Style::default();
            let bold = Style::default().bold();

            let lines = [
                ("WARNING: Gemini OAuth / AI Studio", true),
                ("", false),
                ("Google's ToS prohibits automated API access via", false),
                ("OAuth tokens. Using this provider may result in", false),
                ("your Google account being banned.", false),
                ("", false),
                ("Press Enter/Y to continue, any other key to cancel.", true),
            ];

            for (i, (text, is_bold)) in lines.iter().enumerate() {
                let y = i as u16;
                if y >= area.height {
                    break;
                }
                let style = if *is_bold { bold } else { normal };
                buf.set_string(0, y, text, style);
            }
        })
        .into_element()
    }
}

fn render_entry_lines_for_scrollback(
    entry: &MessageEntry,
    width: u16,
) -> Vec<crate::tui::terminal::StyledLine> {
    let mut lines = ChatRenderer::build_lines(std::slice::from_ref(entry), None, width as usize);
    while lines
        .last()
        .is_some_and(crate::tui::terminal::StyledLine::is_empty)
    {
        lines.pop();
    }
    lines
}

fn fmt_compact(n: usize) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

// ── TuiApp impl ───────────────────────────────────────────────────────────────

impl TuiApp for IonApp {
    type Message = IonMsg;

    fn tick_rate(&self) -> Option<Duration> {
        // 20 Hz tick drives inner.update(), which polls agent_rx and session_rx.
        Some(Duration::from_millis(50))
    }

    fn handle_event(&self, event: &Event) -> Option<IonMsg> {
        match event {
            Event::Key(k) => {
                let ctrl = k.modifiers.contains(KeyModifiers::CTRL);

                // Global keybindings (work in any mode).
                match k.code {
                    KeyCode::Char('c') if ctrl => return Some(IonMsg::ClearInputOrQuit),
                    KeyCode::Char('d') if ctrl => return Some(IonMsg::ClearInputOrQuit),
                    _ => {}
                }

                // Mode-specific keybindings.
                match self.mode {
                    AppMode::Help => {
                        // Any key dismisses help.
                        return Some(IonMsg::OpenHelp);
                    }
                    AppMode::OAuthConfirm => {
                        return match k.code {
                            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                                Some(IonMsg::PickerSelect)
                            }
                            _ => Some(IonMsg::CancelTask),
                        };
                    }
                    AppMode::ModelPicker | AppMode::ProviderPicker | AppMode::SessionPicker => {
                        return match k.code {
                            KeyCode::Esc => Some(IonMsg::CancelTask),
                            KeyCode::Up => Some(IonMsg::PickerUp),
                            KeyCode::Down => Some(IonMsg::PickerDown),
                            KeyCode::PageUp => Some(IonMsg::PickerPageUp),
                            KeyCode::PageDown => Some(IonMsg::PickerPageDown),
                            KeyCode::Home => Some(IonMsg::PickerHome),
                            KeyCode::End => Some(IonMsg::PickerEnd),
                            KeyCode::Enter => Some(IonMsg::PickerSelect),
                            KeyCode::Tab => Some(IonMsg::PickerTab),
                            KeyCode::Backspace => Some(IonMsg::PickerBack),
                            KeyCode::Char('m') if ctrl => Some(IonMsg::OpenModelPicker),
                            KeyCode::Char('p') if ctrl => Some(IonMsg::OpenProviderPicker),
                            _ => Some(IonMsg::PickerFilterKey(k.clone())),
                        };
                    }
                    AppMode::HistorySearch => {
                        return match k.code {
                            KeyCode::Esc => Some(IonMsg::CancelTask),
                            KeyCode::Enter => Some(IonMsg::HistorySearchAccept),
                            KeyCode::Up => Some(IonMsg::HistorySearchPrev),
                            KeyCode::Down => Some(IonMsg::HistorySearchNext),
                            KeyCode::Char('r') if ctrl => Some(IonMsg::HistorySearchPrev),
                            KeyCode::Char('p') if ctrl => Some(IonMsg::HistorySearchPrev),
                            KeyCode::Char('n') if ctrl => Some(IonMsg::HistorySearchNext),
                            KeyCode::Backspace => Some(IonMsg::HistorySearchBackspace),
                            KeyCode::Char(c) => Some(IonMsg::HistorySearchChar(c)),
                            _ => None,
                        };
                    }
                    AppMode::Input => {}
                }

                // Input mode keybindings.
                match k.code {
                    KeyCode::Esc => Some(IonMsg::CancelTask),
                    KeyCode::BackTab => Some(IonMsg::ToggleMode),
                    KeyCode::Char('m') if ctrl => Some(IonMsg::OpenModelPicker),
                    KeyCode::Char('p') if ctrl => {
                        if self.inner.is_running {
                            Some(IonMsg::InputKey(k.clone()))
                        } else {
                            Some(IonMsg::OpenProviderPicker)
                        }
                    }
                    KeyCode::Char('n') if ctrl => Some(IonMsg::HistoryNext),
                    KeyCode::Char('h') if ctrl => Some(IonMsg::OpenHelp),
                    KeyCode::Char('t') if ctrl => Some(IonMsg::CycleThinking),
                    KeyCode::Char('g') if ctrl => Some(IonMsg::OpenEditor),
                    KeyCode::Char('o') if ctrl => Some(IonMsg::ToggleToolExpansion),
                    KeyCode::Char('r') if ctrl => Some(IonMsg::OpenHistorySearch),
                    KeyCode::Char('?') if self.input.is_empty() => Some(IonMsg::OpenHelp),
                    KeyCode::PageUp => Some(IonMsg::ScrollUp),
                    KeyCode::PageDown => Some(IonMsg::ScrollDown),
                    _ => Some(IonMsg::InputKey(k.clone())),
                }
            }
            Event::Mouse(m) => match m.kind {
                tui::event::MouseEventKind::ScrollUp => Some(IonMsg::ScrollUp),
                tui::event::MouseEventKind::ScrollDown => Some(IonMsg::ScrollDown),
                _ => None,
            },
            Event::Paste(text) => Some(IonMsg::Paste(text.clone())),
            Event::Resize(w, h) => Some(IonMsg::Resize(*w, *h)),
            Event::FocusGained => Some(IonMsg::FocusGained),
            Event::FocusLost => Some(IonMsg::FocusLost),
            Event::Tick => Some(IonMsg::Tick),
        }
    }

    fn update(&mut self, msg: IonMsg) -> Effect<IonMsg> {
        match msg {
            // ── Tick ─────────────────────────────────────────────────────────
            IonMsg::Tick => {
                self.inner.update();
                self.sync_scrollback();
                self.sync_status();
                if self.inner.should_quit {
                    return Effect::Quit;
                }
                Effect::None
            }

            // ── Ctrl+C / Ctrl+D (double-tap quit) ───────────────────────────
            IonMsg::ClearInputOrQuit => {
                // If input has text, clear it.
                if !self.input.is_empty() {
                    self.input.clear();
                    self.last_cancel_at = None;
                    return Effect::None;
                }
                // If in a modal, return to input mode.
                if self.mode != AppMode::Input {
                    self.mode = AppMode::Input;
                    self.last_cancel_at = None;
                    return Effect::None;
                }
                // If running, ignore (Esc is for cancel).
                if self.inner.is_running {
                    return Effect::None;
                }
                // Double-tap quit.
                if let Some(when) = self.last_cancel_at {
                    if when.elapsed() <= CANCEL_WINDOW {
                        self.inner.quit();
                        return Effect::Quit;
                    }
                }
                self.last_cancel_at = Some(Instant::now());
                // Show hint in status (will be overwritten on next sync_status).
                self.inner.message_list.push_entry(MessageEntry::new(
                    Sender::System,
                    "Press Ctrl+C again to quit".to_string(),
                ));
                self.sync_scrollback();
                Effect::None
            }

            // ── Esc (cancel task / clear input) ─────────────────────────────
            IonMsg::CancelTask => {
                // If in a modal, return to input.
                if self.mode != AppMode::Input {
                    self.exit_selector_mode();
                    self.inner.oauth_confirm_provider = None;
                    self.inner.history_search.clear();
                    self.inner.command_completer.deactivate();
                    self.inner.file_completer.deactivate();
                    self.last_esc_at = None;
                    return Effect::None;
                }
                // If running, cancel the agent task.
                if self.inner.is_running && !self.inner.session.abort_token.is_cancelled() {
                    self.inner.session.abort_token.cancel();
                    self.last_esc_at = None;
                    return Effect::None;
                }
                // If input non-empty, double-tap to clear.
                if !self.input.is_empty() {
                    if let Some(when) = self.last_esc_at {
                        if when.elapsed() <= CANCEL_WINDOW {
                            self.input.clear();
                            self.last_esc_at = None;
                            return Effect::None;
                        }
                    }
                    self.last_esc_at = Some(Instant::now());
                }
                Effect::None
            }

            // ── Shift+Tab: toggle tool mode ─────────────────────────────────
            IonMsg::ToggleMode => {
                self.inner.tool_mode = match self.inner.tool_mode {
                    ToolMode::Read => ToolMode::Write,
                    ToolMode::Write => ToolMode::Read,
                };
                crate::tool::builtin::spawn_subagent::set_shared_mode(
                    &self.inner.shared_tool_mode,
                    self.inner.tool_mode,
                );
                let orchestrator = self.inner.orchestrator.clone();
                let mode = self.inner.tool_mode;
                tokio::spawn(async move {
                    orchestrator.set_tool_mode(mode).await;
                });
                self.sync_status();
                Effect::None
            }

            // ── Input key handling ───────────────────────────────────────────
            IonMsg::InputKey(k) => {
                // Command completer intercept
                if self.inner.command_completer.is_active() {
                    return self.handle_command_completer_key(&k);
                }
                // File completer intercept
                if self.inner.file_completer.is_active() {
                    return self.handle_file_completer_key(&k);
                }
                // Up/Down: delegate to inner history/queue-recall logic
                let ctrl = k.modifiers.contains(KeyModifiers::CTRL);
                match k.code {
                    KeyCode::Up => {
                        if !self.handle_input_up() {
                            self.input.handle_key(&k);
                        }
                        return Effect::None;
                    }
                    KeyCode::Down => {
                        if !self.handle_input_down() {
                            self.input.handle_key(&k);
                        }
                        return Effect::None;
                    }
                    // Ctrl+P when running → prev history
                    KeyCode::Char('p') if ctrl => {
                        self.handle_prev_history();
                        return Effect::None;
                    }
                    // @ might trigger file completion
                    KeyCode::Char('@') => {
                        self.input.handle_key(&k);
                        self.check_activate_file_completer();
                        return Effect::None;
                    }
                    // / might trigger command completion
                    KeyCode::Char('/') => {
                        self.input.handle_key(&k);
                        self.check_activate_command_completer();
                        return Effect::None;
                    }
                    _ => {}
                }
                match self.input.handle_key(&k) {
                    InputAction::Submit => {
                        let text = self.input.value();
                        if !text.trim().is_empty() {
                            self.input.clear();
                            Effect::Emit(IonMsg::InputSubmit(text))
                        } else {
                            Effect::None
                        }
                    }
                    _ => Effect::None,
                }
            }

            // ── Input submit (Enter) ─────────────────────────────────────────
            IonMsg::InputSubmit(s) => {
                // Route to ask_user response if the agent is waiting for one.
                if let Some(tx) = self.inner.pending_ask_user.take() {
                    self.inner.message_list.push_user_message(s.clone());
                    let _ = tx.send(s);
                    self.sync_scrollback();
                    return Effect::None;
                }

                // Check for slash commands.
                if s.starts_with('/') {
                    return self.handle_slash_command(&s);
                }

                // Check for ! bash passthrough.
                if let Some(cmd) = s.strip_prefix('!') {
                    let cmd = cmd.trim().to_string();
                    if !cmd.is_empty() {
                        self.inner.run_bash_passthrough(cmd);
                        self.sync_scrollback();
                        return Effect::None;
                    }
                }

                // Queue message if agent is already running (mid-turn steering).
                if self.inner.is_running {
                    if let Some(queue) = self.inner.message_queue.as_ref() {
                        match queue.lock() {
                            Ok(mut q) => q.push(s),
                            Err(poisoned) => {
                                tracing::warn!("Message queue lock poisoned, recovering");
                                poisoned.into_inner().push(s);
                            }
                        }
                    }
                    return Effect::None;
                }

                // Normalize and persist to history
                let normalized = crate::tui::util::normalize_input(&s);
                self.inner.input_history.push(normalized.clone());
                self.inner.history_index = self.inner.input_history.len();
                self.inner.history_draft = None;
                let _ = self.inner.store.add_input_history(&normalized);
                self.input.push_history(normalized.clone());

                // Normal submit: send to agent.
                self.inner.message_list.push_user_message(normalized);
                self.sync_scrollback();
                self.inner.run_agent_task(s);
                Effect::None
            }

            // ── Paste ────────────────────────────────────────────────────────
            IonMsg::Paste(text) => {
                let text = if self.inner.config.auto_backtick_paste && text.lines().count() > 1 {
                    format!("```\n{text}\n```")
                } else {
                    text
                };

                let line_count = text.lines().count();
                let char_count = text.chars().count();

                if line_count > PASTE_BLOB_LINE_THRESHOLD || char_count > PASTE_BLOB_CHAR_THRESHOLD
                {
                    use crate::tui::composer::ComposerBuffer;
                    let blob_idx = self.inner.input_buffer.push_blob(text);
                    let placeholder = ComposerBuffer::internal_placeholder(blob_idx);
                    self.input.insert_text(&placeholder);
                } else {
                    self.input.insert_text(&text);
                }
                Effect::None
            }

            // ── Resize ───────────────────────────────────────────────────────
            IonMsg::Resize(w, h) => {
                self.width = w;
                self.height = h;
                self.inner.term_width = w;
                Effect::None
            }

            // ── Scroll (native scrollback handles this now) ─────────────────
            IonMsg::ScrollUp | IonMsg::ScrollDown => Effect::None,

            // ── Quit ─────────────────────────────────────────────────────────
            IonMsg::Quit => {
                self.inner.quit();
                Effect::Quit
            }

            // ── Keybinding actions ───────────────────────────────────────────
            IonMsg::OpenModelPicker => {
                if !self.inner.is_running {
                    self.inner.open_model_selector();
                    self.mode = AppMode::ModelPicker;
                }
                Effect::None
            }
            IonMsg::OpenProviderPicker => {
                if !self.inner.is_running {
                    self.inner.open_provider_selector();
                    self.mode = AppMode::ProviderPicker;
                }
                Effect::None
            }
            IonMsg::OpenHelp => {
                self.mode = if self.mode == AppMode::Help {
                    AppMode::Input
                } else {
                    AppMode::Help
                };
                Effect::None
            }
            IonMsg::CycleThinking => {
                self.inner.thinking_level = self.inner.thinking_level.next();
                Effect::None
            }
            IonMsg::OpenEditor => {
                self.inner.interaction.editor_requested = true;
                Effect::None
            }
            IonMsg::ToggleToolExpansion => {
                self.inner.message_list.toggle_tool_expansion();
                // Reset and reprint all entries with new expansion state.
                self.reset_scrollback();
                self.sync_scrollback();
                Effect::None
            }
            IonMsg::OpenHistorySearch => {
                if !self.inner.input_history.is_empty() {
                    self.inner.history_search.clear();
                    self.inner
                        .history_search
                        .update_matches(&self.inner.input_history);
                    self.mode = AppMode::HistorySearch;
                }
                Effect::None
            }
            IonMsg::FocusGained => {
                self.inner.refresh_startup_header_cache();
                Effect::None
            }
            IonMsg::FocusLost => Effect::None,

            // ── Picker navigation ────────────────────────────────────────────
            IonMsg::PickerUp => {
                self.dispatch_picker(|p| p.move_up(1));
                Effect::None
            }
            IonMsg::PickerDown => {
                self.dispatch_picker(|p| p.move_down(1));
                Effect::None
            }
            IonMsg::PickerPageUp => {
                self.dispatch_picker(|p| p.move_up(10));
                Effect::None
            }
            IonMsg::PickerPageDown => {
                self.dispatch_picker(|p| p.move_down(10));
                Effect::None
            }
            IonMsg::PickerHome => {
                self.dispatch_picker(|p| p.jump_to_top());
                Effect::None
            }
            IonMsg::PickerEnd => {
                self.dispatch_picker(|p| p.jump_to_bottom());
                Effect::None
            }
            IonMsg::PickerTab => {
                match self.selector_page() {
                    SelectorPage::Provider => {
                        self.inner.open_model_selector();
                        self.mode = AppMode::ModelPicker;
                    }
                    SelectorPage::Model => {
                        self.inner.open_provider_selector();
                        self.mode = AppMode::ProviderPicker;
                    }
                    SelectorPage::Session => {}
                }
                Effect::None
            }
            IonMsg::PickerBack => {
                // Backspace: go back from model→provider when filter empty
                if self.mode == AppMode::ModelPicker
                    && self.inner.model_picker.filter_input.text().is_empty()
                    && self.inner.model_picker.stage == PickerStage::Model
                    && !self.inner.needs_setup
                {
                    self.inner.model_picker.back_to_providers();
                } else if self.mode == AppMode::OAuthConfirm {
                    // Cancel OAuth confirm
                    self.inner.oauth_confirm_provider = None;
                    self.inner.open_provider_selector();
                    self.mode = AppMode::ProviderPicker;
                } else {
                    // Normal filter backspace
                    let key = tui::event::KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
                    self.handle_picker_filter_key(&key);
                }
                Effect::None
            }
            IonMsg::PickerSelect => {
                self.handle_picker_select();
                Effect::None
            }
            IonMsg::PickerFilterKey(k) => {
                self.handle_picker_filter_key(&k);
                Effect::None
            }

            // ── History search ───────────────────────────────────────────────
            IonMsg::HistorySearchAccept => {
                if let Some(idx) = self.inner.history_search.selected_entry()
                    && let Some(entry) = self.inner.input_history.get(idx).cloned()
                {
                    self.input.set_value(&entry);
                }
                self.inner.history_search.clear();
                self.mode = AppMode::Input;
                Effect::None
            }
            IonMsg::HistorySearchPrev => {
                self.inner.history_search.select_next();
                Effect::None
            }
            IonMsg::HistorySearchNext => {
                self.inner.history_search.select_prev();
                Effect::None
            }
            IonMsg::HistorySearchBackspace => {
                self.inner.history_search.query.pop();
                self.inner
                    .history_search
                    .update_matches(&self.inner.input_history);
                Effect::None
            }
            IonMsg::HistorySearchChar(c) => {
                self.inner.history_search.query.push(c);
                self.inner
                    .history_search
                    .update_matches(&self.inner.input_history);
                Effect::None
            }

            // ── Ctrl+N — next history ────────────────────────────────────────
            IonMsg::HistoryNext => {
                self.handle_next_history();
                Effect::None
            }

            // Agent events — arrive via Tick → inner.update() for now.
            IonMsg::TokenReceived(_)
            | IonMsg::ToolStarted { .. }
            | IonMsg::ToolCompleted { .. }
            | IonMsg::StreamingDone
            | IonMsg::AgentError(_) => Effect::None,
        }
    }

    fn view(&mut self) -> Element {
        match self.mode {
            AppMode::Help => return self.view_help(),
            AppMode::ModelPicker | AppMode::ProviderPicker | AppMode::SessionPicker => {
                return self.view_picker();
            }
            AppMode::OAuthConfirm => return self.view_oauth_confirm(),
            AppMode::HistorySearch => return self.view_history_search(),
            AppMode::Input => {}
        }

        self.view_bottom_ui()
    }

    fn pre_render_insert(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending_scrollback)
    }

    fn render_mode_override(&self) -> Option<RenderMode> {
        match self.mode {
            AppMode::Input => None, // use configured inline mode
            // Overlays need the full terminal.
            AppMode::ModelPicker
            | AppMode::ProviderPicker
            | AppMode::SessionPicker
            | AppMode::Help
            | AppMode::HistorySearch
            | AppMode::OAuthConfirm => Some(RenderMode::Fullscreen),
        }
    }

    fn cursor_position(&self) -> Option<(u16, u16)> {
        match self.mode {
            AppMode::Input => Input::new(&self.input).cursor_position(self.input_area),
            // Picker filter: cursor at end of filter text row.
            AppMode::ModelPicker | AppMode::ProviderPicker | AppMode::SessionPicker => {
                let filter_text = match self.selector_page() {
                    SelectorPage::Provider => self.inner.provider_picker.filter_input().text(),
                    SelectorPage::Model => self.inner.model_picker.filter_input.text(),
                    SelectorPage::Session => self.inner.session_picker.filter_input().text(),
                };
                // Filter prompt is "> " (2 chars) + text
                let col = 2 + filter_text.chars().count() as u16;
                // Filter row is at y=1 (title at y=0)
                Some((col, 1))
            }
            AppMode::HistorySearch => {
                // Search prompt at bottom of the fullscreen overlay.
                let col = 8 + self.inner.history_search.query.chars().count() as u16;
                Some((col, self.height.saturating_sub(1)))
            }
            AppMode::Help | AppMode::OAuthConfirm => None,
        }
    }

    fn on_exit(&mut self) {
        // Print session ID if there were user messages, matching old behavior.
        let has_user = self
            .inner
            .message_list
            .entries
            .iter()
            .any(|e| e.sender == Sender::User);
        if has_user {
            let line = crate::tui::terminal::StyledLine::dim(format!(
                "Session: {}",
                self.inner.session.id
            ));
            let mut stdout = std::io::stdout();
            let _ = line.writeln(&mut stdout);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::render_entry_lines_for_scrollback;
    use crate::tui::message_list::{MessageEntry, Sender, ToolMeta};

    #[test]
    fn tool_mapping_preserves_rendered_result_content() {
        let mut entry = MessageEntry::new(Sender::Tool, "read(src/main.rs)".to_string());
        entry.append_text("\n✓ 3 lines");
        entry.tool_meta = Some(ToolMeta {
            header: "read(src/main.rs)".to_string(),
            tool_name: "read".to_string(),
            raw_result: "line1\nline2\nline3".to_string(),
            is_error: false,
        });

        let lines = render_entry_lines_for_scrollback(&entry, 120);
        let joined = lines
            .iter()
            .map(crate::tui::terminal::StyledLine::plain_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("read"));
        assert!(joined.contains("✓ 3 lines"));
    }
}
