//! IonApp — ion's App implementation for crates/tui.
//!
//! Wraps the existing `App` struct for all business logic (agent, session,
//! orchestrator) and bridges its data model to the new `crates/tui` widget
//! layer. Agent events arrive via periodic `Tick` messages that drain
//! `inner.agent_rx` through `inner.update()`, then `sync_conversation()`
//! incrementally propagates changes to `ConversationView`.

use std::time::Duration;

use tui::{
    app::{App as TuiApp, Effect},
    event::{Event, KeyCode, KeyModifiers},
    layout::Dimension,
    Col, Element, Input, InputAction, InputState, IntoElement,
};

use crate::cli::PermissionSettings;
use crate::tool::ToolMode;
use crate::tui::{
    App as IonState, ResumeOption,
    message::IonMsg,
    message_list::{MessageEntry, Sender},
};
use crate::ui::{ConversationEntry, ConversationView, StatusBar};

// ── IonApp ────────────────────────────────────────────────────────────────────

pub struct IonApp {
    pub(crate) inner: IonState,
    conversation: ConversationView,
    input: InputState,
    status: StatusBar,
    width: u16,
    height: u16,
    /// Number of `inner.message_list` entries already synced to `conversation`.
    synced_entry_count: usize,
    /// Content length of the last streaming entry, used to detect incremental
    /// token updates without a full equality check.
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
            conversation: ConversationView::new(),
            input: InputState::new(),
            status: StatusBar::new(),
            width,
            height,
            synced_entry_count: 0,
            last_streaming_len: 0,
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
                        // First time we're seeing this streaming entry.
                        self.conversation.push_assistant(content);
                        self.synced_entry_count += 1;
                        self.last_streaming_len = new_len;
                    } else if new_len != self.last_streaming_len {
                        // Token update: replace the last entry's content.
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
        Sender::System => {} // System messages are not shown in ConversationView.
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

    fn update(&mut self, msg: IonMsg) -> Effect<IonMsg> {
        match msg {
            IonMsg::Tick => {
                let was_running = self.inner.is_running;
                self.inner.update();
                self.sync_conversation();
                self.sync_status();
                if was_running && !self.inner.is_running {
                    // Re-enable auto-scroll when streaming completes.
                    self.conversation.resume_auto_scroll();
                }
                if self.inner.should_quit {
                    return Effect::Quit;
                }
                Effect::None
            }

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

            IonMsg::InputSubmit(s) => {
                // Route to ask_user response if the agent is waiting for one.
                if let Some(tx) = self.inner.pending_ask_user.take() {
                    self.inner.message_list.push_user_message(s.clone());
                    let _ = tx.send(s);
                    self.sync_conversation();
                    return Effect::None;
                }

                // Ignore submissions while the agent is already running.
                if !self.inner.is_running {
                    self.inner.message_list.push_user_message(s.clone());
                    self.sync_conversation();
                    self.inner.run_agent_task(s);
                }
                Effect::None
            }

            IonMsg::Resize(w, h) => {
                self.width = w;
                self.height = h;
                self.inner.term_width = w;
                Effect::None
            }

            IonMsg::Quit => {
                self.inner.quit();
                Effect::Quit
            }

            IonMsg::ToggleMode => {
                self.inner.tool_mode = match self.inner.tool_mode {
                    ToolMode::Read => ToolMode::Write,
                    ToolMode::Write => ToolMode::Read,
                };
                self.sync_status();
                Effect::None
            }

            IonMsg::ScrollUp => {
                self.conversation.scroll_up(3);
                Effect::None
            }

            IonMsg::ScrollDown => {
                self.conversation.scroll_down(3);
                Effect::None
            }

            // These variants are reserved for a future direct agent-event
            // bridge. For now, all agent events arrive via Tick → inner.update().
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

        Col::new(vec![
            // Conversation fills all space not taken by status + input.
            self.conversation.view(width).flex_grow(1.0),
            // Status bar: exactly 1 row, does not grow or shrink.
            self.status
                .view(width)
                .height(Dimension::Cells(1))
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

    fn handle_event(&self, event: &Event) -> Option<IonMsg> {
        match event {
            Event::Key(k) => {
                // Ctrl+C → quit (saves session).
                if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CTRL) {
                    return Some(IonMsg::Quit);
                }
                // Tab → toggle tool mode.
                if k.code == KeyCode::Tab {
                    return Some(IonMsg::ToggleMode);
                }
                // Scroll keybindings.
                match k.code {
                    KeyCode::PageUp => return Some(IonMsg::ScrollUp),
                    KeyCode::PageDown => return Some(IonMsg::ScrollDown),
                    _ => {}
                }
                Some(IonMsg::InputKey(k.clone()))
            }
            Event::Resize(w, h) => Some(IonMsg::Resize(*w, *h)),
            Event::Tick => Some(IonMsg::Tick),
            _ => None,
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
            let line =
                crate::tui::terminal::StyledLine::dim(format!("Session: {}", self.inner.session.id));
            let mut stdout = std::io::stdout();
            let _ = line.writeln(&mut stdout);
        }
    }
}
