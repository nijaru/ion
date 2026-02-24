//! IonApp — ion's App implementation for crates/tui.
//!
//! Wraps the existing `App` struct for all business logic (agent, session,
//! orchestrator) and bridges its data model to the new `crates/tui` widget
//! layer. Agent events arrive via periodic `Tick` messages that drain
//! `inner.agent_rx` through `inner.update()`, then `sync_conversation()`
//! incrementally propagates changes to `ConversationView`.

use std::time::{Duration, Instant};

use tui::{
    app::{App as TuiApp, Effect},
    event::{Event, KeyCode, KeyModifiers},
    geometry::Rect,
    layout::Dimension,
    Col, Element, Input, InputAction, InputState, IntoElement,
};

use crate::cli::PermissionSettings;
use crate::session::Session;
use crate::tool::ToolMode;
use crate::tui::{
    App as IonState, ResumeOption,
    fuzzy,
    message::IonMsg,
    message_list::{MessageEntry, Sender},
};
use crate::ui::{ConversationEntry, ConversationView, StatusBar};

/// Double-tap window for Ctrl+C quit and Esc clear.
const CANCEL_WINDOW: Duration = Duration::from_millis(1500);

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
}

// ── IonApp ────────────────────────────────────────────────────────────────────

pub struct IonApp {
    pub(crate) inner: IonState,
    conversation: ConversationView,
    input: InputState,
    status: StatusBar,
    mode: AppMode,
    width: u16,
    height: u16,
    /// Number of `inner.message_list` entries already synced to `conversation`.
    synced_entry_count: usize,
    /// Content length of the last streaming entry, used to detect incremental
    /// token updates without a full equality check.
    last_streaming_len: usize,
    /// Cached input area rect from last render (for cursor positioning).
    input_area: Rect,
    /// Timestamp of last Ctrl+C / Ctrl+D press for double-tap quit detection.
    last_cancel_at: Option<Instant>,
    /// Timestamp of last Esc press for double-tap clear detection.
    last_esc_at: Option<Instant>,
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
            conversation: ConversationView::new(),
            input: InputState::new(),
            status: StatusBar::new(),
            mode: AppMode::default(),
            width,
            height,
            synced_entry_count: 0,
            last_streaming_len: 0,
            input_area: Rect::default(),
            last_cancel_at: None,
            last_esc_at: None,
        })
    }

    /// Apply a resume option, loading session state into `inner` and syncing
    /// the loaded history to `conversation`.
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
        self.sync_all_to_conversation();
    }

    // ── Conversation sync ────────────────────────────────────────────────────

    /// Rebuild `conversation` from scratch, syncing all current
    /// `inner.message_list` entries. Used after loading a session.
    pub(crate) fn sync_all_to_conversation(&mut self) {
        self.synced_entry_count = 0;
        self.last_streaming_len = 0;
        self.conversation = ConversationView::new();
        self.sync_conversation();
    }

    /// Incrementally sync new or updated entries from `inner.message_list`
    /// to `conversation`. Handles both stable (completed) entries and the
    /// actively streaming last entry.
    pub(crate) fn sync_conversation(&mut self) {
        let entries = &self.inner.message_list.entries;

        // All entries except the last are "stable" while streaming.
        let stable_count = if self.inner.is_running
            && entries.last().map_or(false, |e| e.sender == Sender::Agent)
        {
            entries.len().saturating_sub(1)
        } else {
            entries.len()
        };

        // Push all new stable entries.
        while self.synced_entry_count < stable_count {
            let entry = &entries[self.synced_entry_count];
            push_entry_to_conversation(&mut self.conversation, entry);
            self.synced_entry_count += 1;
        }

        // Handle the actively streaming last entry.
        if self.inner.is_running && !entries.is_empty() {
            if let Some(last) = entries.last() {
                if last.sender == Sender::Agent {
                    let content = last.content_as_markdown().to_owned();
                    let new_len = content.len();
                    if self.synced_entry_count < entries.len() {
                        self.conversation.push_assistant(content);
                        self.synced_entry_count += 1;
                        self.last_streaming_len = new_len;
                    } else if new_len != self.last_streaming_len {
                        self.conversation.set_last_content(&content);
                        self.last_streaming_len = new_len;
                    }
                }
            }
        }

        if !self.inner.is_running {
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
        let tokens = self.inner.token_usage.map(|(used, max)| {
            format!("{}/{}", fmt_compact(used), fmt_compact(max))
        });
        let cost = (self.inner.session_cost > 0.001)
            .then(|| format!("${:.2}", self.inner.session_cost));

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

        let cmd_line = input.trim().to_lowercase();
        let cmd_name = cmd_line.split_whitespace().next().unwrap_or("");

        // Handle //skill-name [args] skill invocation
        if cmd_line.starts_with("//") {
            let skill_input = cmd_line.strip_prefix("//").unwrap_or("").trim();
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
                self.sync_conversation();
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
                self.sync_conversation();
            }
            "/cost" => {
                let msg = if self.inner.api_provider.is_oauth() {
                    "Subscription provider — no per-token cost".to_string()
                } else if self.inner.session_cost > 0.0 {
                    let p = &self.inner.model_pricing;
                    let mut parts = vec![format!(
                        "Session cost: ${:.4}",
                        self.inner.session_cost
                    )];
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
                self.sync_conversation();
            }
            "/export" => {
                self.inner.export_session_markdown();
                self.sync_conversation();
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
                self.sync_all_to_conversation();
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
                    self.sync_conversation();
                }
            }
        }
        Effect::None
    }
}

fn push_entry_to_conversation(conversation: &mut ConversationView, entry: &MessageEntry) {
    let content = entry.content_as_markdown().to_owned();
    match entry.sender {
        Sender::User => conversation.push_user(content),
        Sender::Agent => conversation.push_assistant(content),
        Sender::Tool => {
            if let Some(meta) = &entry.tool_meta {
                let label = if meta.header.is_empty() {
                    meta.tool_name.clone()
                } else {
                    format!("{}: {}", meta.tool_name, meta.header)
                };
                conversation.push(ConversationEntry::tool_call(&meta.tool_name, label));
            }
        }
        Sender::System => {
            conversation.push(ConversationEntry::system(content));
        }
    }
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
                    AppMode::ModelPicker
                    | AppMode::ProviderPicker
                    | AppMode::SessionPicker
                    | AppMode::HistorySearch => {
                        // Esc returns to input mode.
                        if k.code == KeyCode::Esc {
                            return Some(IonMsg::CancelTask);
                        }
                        // Pass keys through to input for now (pickers handled via inner).
                        return Some(IonMsg::InputKey(k.clone()));
                    }
                    AppMode::Input => {}
                }

                // Input mode keybindings.
                match k.code {
                    KeyCode::Esc => Some(IonMsg::CancelTask),
                    KeyCode::BackTab => Some(IonMsg::ToggleMode),
                    KeyCode::Char('m') if ctrl => Some(IonMsg::OpenModelPicker),
                    KeyCode::Char('p') if ctrl => Some(IonMsg::OpenProviderPicker),
                    KeyCode::Char('h') if ctrl => Some(IonMsg::OpenHelp),
                    KeyCode::Char('t') if ctrl => Some(IonMsg::CycleThinking),
                    KeyCode::Char('g') if ctrl => Some(IonMsg::OpenEditor),
                    KeyCode::Char('o') if ctrl => Some(IonMsg::ToggleToolExpansion),
                    KeyCode::Char('r') if ctrl => Some(IonMsg::OpenHistorySearch),
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
                let was_running = self.inner.is_running;
                self.inner.update();
                self.sync_conversation();
                self.sync_status();
                if was_running && !self.inner.is_running {
                    self.conversation.resume_auto_scroll();
                }
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
                self.sync_conversation();
                Effect::None
            }

            // ── Esc (cancel task / clear input) ─────────────────────────────
            IonMsg::CancelTask => {
                // If in a modal, return to input.
                if self.mode != AppMode::Input {
                    self.mode = AppMode::Input;
                    self.last_esc_at = None;
                    return Effect::None;
                }
                // If running, cancel the agent task.
                if self.inner.is_running
                    && !self.inner.session.abort_token.is_cancelled()
                {
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
            IonMsg::InputKey(k) => match self.input.handle_key(&k) {
                InputAction::Submit => {
                    let text = self.input.value();
                    if !text.trim().is_empty() {
                        self.input.push_history(text.clone());
                        self.input.clear();
                        Effect::Emit(IonMsg::InputSubmit(text))
                    } else {
                        Effect::None
                    }
                }
                _ => Effect::None,
            },

            // ── Input submit (Enter) ─────────────────────────────────────────
            IonMsg::InputSubmit(s) => {
                // Route to ask_user response if the agent is waiting for one.
                if let Some(tx) = self.inner.pending_ask_user.take() {
                    self.inner.message_list.push_user_message(s.clone());
                    let _ = tx.send(s);
                    self.sync_conversation();
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
                        self.sync_conversation();
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

                // Normal submit: send to agent.
                self.inner.message_list.push_user_message(s.clone());
                self.sync_conversation();
                self.inner.run_agent_task(s);
                Effect::None
            }

            // ── Paste ────────────────────────────────────────────────────────
            IonMsg::Paste(text) => {
                let text = if self.inner.config.auto_backtick_paste
                    && text.lines().count() > 1
                {
                    format!("```\n{text}\n```")
                } else {
                    text
                };
                self.input.insert_text(&text);
                Effect::None
            }

            // ── Resize ───────────────────────────────────────────────────────
            IonMsg::Resize(w, h) => {
                self.width = w;
                self.height = h;
                self.inner.term_width = w;
                Effect::None
            }

            // ── Scroll ───────────────────────────────────────────────────────
            IonMsg::ScrollUp => {
                self.conversation.scroll_up(3);
                Effect::None
            }
            IonMsg::ScrollDown => {
                self.conversation.scroll_down(3);
                Effect::None
            }

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

            // Agent events — arrive via Tick → inner.update() for now.
            IonMsg::TokenReceived(_)
            | IonMsg::ToolStarted { .. }
            | IonMsg::ToolCompleted { .. }
            | IonMsg::StreamingDone
            | IonMsg::AgentError(_) => Effect::None,
        }
    }

    fn view(&mut self) -> Element {
        let width = self.width;
        let input_height = self.input.line_count().max(1) as u16;
        // Status bar is 1 row.
        let status_height: u16 = 1;
        // Compute input area for cursor positioning.
        // Input is at the bottom: y = height - input_height.
        let input_y = self.height.saturating_sub(input_height);
        self.input_area = Rect::new(0, input_y, width, input_height);

        Col::new(vec![
            // Conversation fills all space not taken by status + input.
            self.conversation.view(width).flex_grow(1.0),
            // Status bar: exactly 1 row, does not grow or shrink.
            self.status
                .view(width)
                .height(Dimension::Cells(status_height))
                .flex_grow(0.0)
                .flex_shrink(0.0),
            // Input: height matches current line count (1 for empty input).
            Input::new(&self.input)
                .placeholder("Type a message... (Enter to send)")
                .into_element()
                .height(Dimension::Cells(input_height))
                .flex_grow(0.0)
                .flex_shrink(0.0),
        ])
        .into_element()
    }

    fn cursor_position(&self) -> Option<(u16, u16)> {
        if self.mode != AppMode::Input {
            return None;
        }
        Input::new(&self.input).cursor_position(self.input_area)
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
            let line =
                crate::tui::terminal::StyledLine::dim(format!("Session: {}", self.inner.session.id));
            let mut stdout = std::io::stdout();
            let _ = line.writeln(&mut stdout);
        }
    }
}
