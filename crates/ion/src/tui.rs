//! Ratatui TUI frontend (DESIGN.md §21, §22).
//!
//! One runtime contract: this frontend consumes `SessionHandle`
//! semantics only — snapshot plus bounded live events — and never
//! touches the store. Ion owns application state: [`UiState`] is a
//! plain value, `update` is a pure reducer over [`UiMessage`]s, and
//! effects call back into the session. The terminal is restored by one
//! RAII owner, never scattered across widgets.

use std::io::{self, Write};
use std::sync::Arc;

use futures_util::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event as TermEvent, EventStream, KeyCode,
    KeyEvent, KeyModifiers,
};
use ratatui::crossterm::{execute, terminal};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::{Clear, Paragraph, Widget, Wrap};
use ratatui::{Frame, Terminal, TerminalOptions, Viewport};

use ion_core::{
    CommandError, OperationStatus, RuntimeError, RuntimeEvent, SessionHandle, SessionStore,
};

/// What the reducer wants the event loop to do. Effects are the only
/// path back into the runtime (§22.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiEffect {
    Submit { text: String },
    Steer { text: String },
    Cancel,
    Quit,
}

/// Inputs to the reducer: runtime events, keys, resizes.
#[derive(Debug, Clone)]
pub enum UiMessage {
    Runtime(RuntimeEvent),
    Key(KeyEvent),
    SubmitAccepted,
    SubmitRejected(String),
    SteerAccepted,
    SteerRejected(String),
}

/// The live operation presentation, derived from runtime events.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum UiStatus {
    #[default]
    Idle,
    Working {
        operation: String,
    },
}

/// One UI state owner (§22.1). Plain data; no handles, no hidden state.
#[derive(Debug, Clone, Default)]
pub struct UiState {
    /// Composer buffer.
    composer: String,
    /// Streaming assistant draft for the live step.
    draft: String,
    /// Completed tool rows for the live operation, newest last.
    tool_rows: Vec<String>,
    status: UiStatus,
    /// Lines queued for scrollback: flushed above the inline viewport
    /// when the composer redraws.
    pending_scrollback: Vec<Line<'static>>,
    quit_requested: bool,
}

impl UiState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// Pure reducer (§22.1): `update(UiState, UiMessage) -> UiState` plus
/// at most one effect. Deterministic; no I/O.
#[must_use]
pub fn update(state: UiState, message: UiMessage) -> (UiState, Option<UiEffect>) {
    let mut state = state;
    match message {
        UiMessage::Key(key) => handle_key(state, key),
        UiMessage::Runtime(event) => (apply_runtime_event(state, event), None),
        UiMessage::SubmitAccepted => {
            state.composer.clear();
            (state, None)
        }
        UiMessage::SteerAccepted => {
            state.composer.clear();
            (state, None)
        }
        UiMessage::SubmitRejected(message) | UiMessage::SteerRejected(message) => {
            state
                .pending_scrollback
                .push(Line::from(format!("! {message}")).red());
            (state, None)
        }
    }
}

fn handle_key(mut state: UiState, key: KeyEvent) -> (UiState, Option<UiEffect>) {
    match (key.code, key.modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
            // Ctrl-C cancels the active operation; a second press quits.
            if matches!(state.status, UiStatus::Idle) {
                state.quit_requested = true;
                (state, Some(UiEffect::Quit))
            } else {
                (state, Some(UiEffect::Cancel))
            }
        }
        (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
            state.quit_requested = true;
            (state, Some(UiEffect::Quit))
        }
        (KeyCode::Esc, _) => {
            if matches!(state.status, UiStatus::Idle) {
                state.quit_requested = true;
                (state, Some(UiEffect::Quit))
            } else {
                (state, Some(UiEffect::Cancel))
            }
        }
        (KeyCode::Enter, _) => {
            let text = state.composer.trim().to_owned();
            if text.is_empty() {
                return (state, None);
            }
            match &state.status {
                UiStatus::Idle => (state, Some(UiEffect::Submit { text })),
                UiStatus::Working { .. } => (state, Some(UiEffect::Steer { text })),
            }
        }
        (KeyCode::Backspace, _) => {
            state.composer.pop();
            (state, None)
        }
        (KeyCode::Char(' '), _) => {
            state.composer.push(' ');
            (state, None)
        }
        (KeyCode::Char(ch), modifiers)
            if modifiers.is_empty() || modifiers == KeyModifiers::SHIFT =>
        {
            state.composer.push(ch);
            (state, None)
        }
        _ => (state, None),
    }
}

fn apply_runtime_event(mut state: UiState, event: RuntimeEvent) -> UiState {
    match event {
        RuntimeEvent::OperationStarted { prompt, .. } => {
            state
                .pending_scrollback
                .push(Line::from(format!("you » {prompt}")).bold());
            state.draft.clear();
            state.tool_rows.clear();
            state.status = UiStatus::Working {
                operation: "thinking".to_owned(),
            };
        }
        RuntimeEvent::AssistantTextDelta { text, .. } => {
            state.draft.push_str(&text);
        }
        RuntimeEvent::ToolStarted { tool, call_id, .. } => {
            state.tool_rows.push(format!("· {tool} (call {call_id})…"));
            state.status = UiStatus::Working {
                operation: format!("running {tool}"),
            };
        }
        RuntimeEvent::OperationFinished { .. } => {
            state.flush_draft();
            state.status = UiStatus::Idle;
        }
        RuntimeEvent::OperationFailed { message, .. } => {
            state.flush_draft();
            state
                .pending_scrollback
                .push(Line::from(format!("! failed: {message}")).red());
            state.status = UiStatus::Idle;
        }
        RuntimeEvent::OperationCancelled { .. } => {
            state.flush_draft();
            state
                .pending_scrollback
                .push(Line::from("! cancelled".to_owned()).yellow());
            state.status = UiStatus::Idle;
        }
        RuntimeEvent::OperationApprovalRequired { tool, .. } => {
            state.flush_draft();
            state.pending_scrollback.push(
                Line::from(format!(
                    "! approval required: `{tool}` — rerun with --allow {tool}"
                ))
                .yellow(),
            );
            state.status = UiStatus::Idle;
        }
        RuntimeEvent::SessionClosed { .. } => {
            state.quit_requested = true;
        }
    }
    state
}

impl UiState {
    /// Move the live draft into scrollback as a completed assistant
    /// turn (inline scrollback pattern: completed content leaves the
    /// live viewport).
    fn flush_draft(&mut self) {
        if !self.draft.is_empty() {
            for line in self.draft.lines() {
                self.pending_scrollback
                    .push(Line::from(format!("ion « {line}")));
            }
            self.draft.clear();
        }
        for row in self.tool_rows.drain(..) {
            self.pending_scrollback.push(Line::from(row).dim());
        }
    }
}

/// RAII owner of terminal restoration (§22.4). One guard owns raw
/// mode, bracketed paste, and the inline viewport teardown.
pub struct TerminalGuard {
    restored: bool,
}

impl TerminalGuard {
    /// Enter raw mode and enable bracketed paste.
    pub fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        execute!(io::stdout(), EnableBracketedPaste)?;
        Ok(Self { restored: false })
    }

    fn restore(&mut self) {
        if self.restored {
            return;
        }
        self.restored = true;
        let _ = execute!(io::stdout(), DisableBracketedPaste);
        let _ = terminal::disable_raw_mode();
        let _ = io::stdout().flush();
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

/// Install a panic hook that restores the terminal before the default
/// hook runs, so a recoverable panic cannot leave raw mode behind.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(io::stdout(), DisableBracketedPaste);
        previous(info);
    }));
}

/// Render the live inline viewport: transcript tail, tool rows, draft,
/// status, composer.
pub fn render(state: &UiState, frame: &mut Frame) {
    let area = frame.area();
    // The inline viewport keeps cells between frames; clear before
    // drawing so stale status/draft text never survives a redraw.
    frame.render_widget(Clear, area);
    let rows = Layout::vertical([
        Constraint::Min(1),    // transcript tail / draft
        Constraint::Length(1), // tool row (latest)
        Constraint::Length(1), // status
        Constraint::Length(1), // composer
    ])
    .split(area);

    let mut tail: Vec<Line> = Vec::new();
    if let Some(latest) = state.tool_rows.last() {
        tail.push(Line::from(latest.clone()).dim());
    }
    if !state.draft.is_empty() {
        tail.push(Line::from(format!("ion « {}", state.draft)));
    }
    frame.render_widget(Paragraph::new(tail).wrap(Wrap { trim: false }), rows[0]);

    let status = match &state.status {
        UiStatus::Idle => Line::from(format!(
            "idle — type a prompt, esc quits  [area {}x{}@{}]",
            area.width, area.height, area.y
        ))
        .dim(),
        UiStatus::Working { operation } => Line::from(format!("● {operation}")).cyan(),
    };
    frame.render_widget(status, rows[2]);
    frame.render_widget(Paragraph::new(state.composer.clone() + "▏"), rows[3]);
}

/// The TUI event loop: runtime events and terminal keys into the
/// reducer; effects dispatch straight back into the session. Never
/// blocks rendering on provider/tool I/O (§22.2).
pub async fn run(
    session: SessionHandle,
    store: Arc<SessionStore>,
    resume_session: Option<ion_core::SessionId>,
) -> Result<(), RuntimeError> {
    let mut guard = TerminalGuard::enter()
        .map_err(|err| RuntimeError::OperationFailed(format!("terminal setup failed: {err}")))?;
    install_panic_hook();

    // The inline viewport anchors at the cursor; push it to the
    // bottom of the screen first (ratatui inline-example pattern) so
    // completed scrollback accumulates above a stable live area.
    print!("{}", "\n".repeat(8));
    io::stdout().flush().ok();

    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(6),
        },
    )
    .map_err(|err| RuntimeError::OperationFailed(format!("terminal setup failed: {err}")))?;

    print_banner(&mut terminal, resume_session.is_some())?;

    // Resume: project the persisted transcript into scrollback.
    if let Some(session_id) = resume_session {
        let loaded = store
            .load(session_id)
            .await
            .map_err(|err| RuntimeError::OperationFailed(err.to_string()))?;
        let mut restored: Vec<Line> = Vec::new();
        for (_, entry) in loaded.entries {
            push_entry_lines(&entry, &mut restored);
        }
        if !restored.is_empty() {
            let count = restored.len() as u16;
            let _ = terminal.insert_before(count, |buf| {
                Paragraph::new(restored.clone()).render(buf.area, buf);
            });
        }
        let _ = terminal.insert_before(1, |buf| {
            Paragraph::new(Line::from(format!("— resumed session {session_id} —")).dim())
                .render(buf.area, buf);
        });
    }

    // The EventStream is the sole terminal reader, so crossterm parses
    // cursor-position responses itself; blocking cursor queries (used
    // by Terminal::clear) cannot deadlock against key reads.
    let mut key_stream = EventStream::new();

    let mut state = UiState::new();
    let (snapshot, mut events) = session.subscribe().await?;
    let mut active_operation: Option<ion_core::OperationId> = match snapshot.operation {
        OperationStatus::Active { operation_id, .. } => Some(operation_id),
        OperationStatus::Idle => None,
    };
    let mut result: Result<(), RuntimeError> = Ok(());

    loop {
        // Flush completed content into scrollback, then draw the live
        // viewport (inline pattern, §22.3).
        if !state.pending_scrollback.is_empty() {
            let lines = std::mem::take(&mut state.pending_scrollback);
            let count = lines.len() as u16;
            let _ = terminal.insert_before(count, |buf| {
                Paragraph::new(lines.clone()).render(buf.area, buf);
            });
            // The viewport moved; the previous buffer no longer matches
            // the screen. Force a full repaint (safe: the EventStream
            // owns terminal reads, so the cursor query cannot deadlock).
            terminal.clear().ok();
        }
        terminal
            .draw(|frame| render(&state, frame))
            .map_err(|err| RuntimeError::OperationFailed(format!("draw failed: {err}")))?;
        #[cfg(debug_assertions)]
        if let Ok(mut log) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("/tmp/ion-tui-debug.log")
        {
            use std::io::Write;
            let _ = writeln!(
                log,
                "draw screen_h={:?} scrollback_pending={}",
                terminal.size().map(|s| s.height).unwrap_or(0),
                state.pending_scrollback.len()
            );
        }

        if state.quit_requested {
            break;
        }

        tokio::select! {
            maybe_key = key_stream.next() => {
                let Some(Ok(TermEvent::Key(key))) = maybe_key else {
                    result = Err(RuntimeError::OperationFailed(
                        "terminal event stream ended".to_owned(),
                    ));
                    break;
                };
                let (next, effect) = update(state, UiMessage::Key(key));
                state = next;
                if let Some(effect) = effect {
                    dispatch(&session, &mut state, active_operation, effect).await;
                }
            }
            event = events.recv() => {
                match event {
                    Ok(event) => {
                        if let RuntimeEvent::OperationStarted { operation_id, .. } = &event {
                            active_operation = Some(*operation_id);
                        }
                        if matches!(
                            event,
                            RuntimeEvent::OperationFinished { .. }
                                | RuntimeEvent::OperationFailed { .. }
                                | RuntimeEvent::OperationCancelled { .. }
                                | RuntimeEvent::OperationApprovalRequired { .. }
                        ) {
                            active_operation = None;
                        }
                        let (next, effect) = update(state, UiMessage::Runtime(event));
                        state = next;
                        if let Some(effect) = effect {
                            dispatch(&session, &mut state, active_operation, effect).await;
                        }
                    }
                    Err(RuntimeError::SubscriptionLagged) => {
                        // Bounded loss (§21.4): the snapshot on next
                        // subscribe is authoritative; v0 continues with
                        // the live stream.
                        continue;
                    }
                    Err(err) => {
                        result = Err(err);
                        break;
                    }
                }
            }
        }
    }

    guard.restore();
    terminal.clear().ok();
    result?;
    match session.close().await {
        Ok(()) | Err(CommandError::Closed) => Ok(()),
        Err(err) => Err(err.into()),
    }
}

/// Execute one reducer effect against the session; acceptance and
/// rejection return to the reducer as messages.
async fn dispatch(
    session: &SessionHandle,
    state: &mut UiState,
    active_operation: Option<ion_core::OperationId>,
    effect: UiEffect,
) {
    match effect {
        UiEffect::Quit => {
            state.quit_requested = true;
        }
        UiEffect::Submit { text } => match session.submit(text).await {
            Ok(_) => {
                let (next, _) = update(std::mem::take(state), UiMessage::SubmitAccepted);
                *state = next;
            }
            Err(err) => {
                let (next, _) = update(
                    std::mem::take(state),
                    UiMessage::SubmitRejected(err.to_string()),
                );
                *state = next;
            }
        },
        UiEffect::Steer { text } => match session.steer(text).await {
            Ok(()) => {
                let (next, _) = update(std::mem::take(state), UiMessage::SteerAccepted);
                *state = next;
            }
            Err(err) => {
                let (next, _) = update(
                    std::mem::take(state),
                    UiMessage::SteerRejected(err.to_string()),
                );
                *state = next;
            }
        },
        UiEffect::Cancel => {
            if let Some(operation_id) = active_operation {
                let _ = session.cancel(operation_id).await;
            }
        }
    }
}

fn print_banner(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    resumed: bool,
) -> Result<(), RuntimeError> {
    let banner = if resumed {
        "— ion — resumed; enter sends; esc cancels; ctrl-d quits —"
    } else {
        "— ion — type a prompt; enter sends; esc cancels; ctrl-d quits —"
    };
    terminal
        .insert_before(1, |buf| {
            Paragraph::new(Line::from(banner).dim()).render(buf.area, buf);
        })
        .map_err(|err| RuntimeError::OperationFailed(format!("terminal write failed: {err}")))
}

fn push_entry_lines(entry: &ion_core::SessionEntry, out: &mut Vec<Line<'static>>) {
    let line = match entry {
        ion_core::SessionEntry::UserMessage { text } => Some(format!("you » {text}")),
        ion_core::SessionEntry::AssistantMessage { text } => Some(format!("ion « {text}")),
        ion_core::SessionEntry::ToolCall { call } => {
            Some(format!("· {} (call {})…", call.name, call.call_id))
        }
        ion_core::SessionEntry::ToolResult {
            result: ion_core::ToolResult::Ok { output, .. },
        } => Some(format!("  = {output}")),
        ion_core::SessionEntry::ToolResult {
            result: ion_core::ToolResult::Err { error, .. },
        } => Some(format!("  ! {error}")),
        ion_core::SessionEntry::Compaction { summary, .. } => {
            Some(format!("≡ compacted: {summary}"))
        }
    };
    if let Some(line) = line {
        for chunk in line.chars().collect::<Vec<_>>().chunks(80) {
            out.push(Line::from(chunk.iter().collect::<String>()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ion_core::OperationId;

    fn key(code: KeyCode) -> UiMessage {
        UiMessage::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn composer_types_and_submits_when_idle() {
        let state = UiState::new();
        let (state, _) = update(state, key(KeyCode::Char('h')));
        let (state, _) = update(state, key(KeyCode::Char('i')));
        assert_eq!(state.composer.as_str(), "hi");
        let (state, effect) = update(state, key(KeyCode::Enter));
        assert_eq!(
            effect,
            Some(UiEffect::Submit {
                text: "hi".to_owned()
            })
        );
        // The loop clears the composer only after acceptance.
        let (state, _) = update(state, UiMessage::SubmitAccepted);
        assert_eq!(state.composer.as_str(), "");
        assert!(matches!(state.status, UiStatus::Idle));
    }

    #[test]
    fn working_composer_steers_instead_of_submitting() {
        let mut state = UiState::new();
        state.status = UiStatus::Working {
            operation: "thinking".to_owned(),
        };
        state.composer = "wait".to_owned();
        let (_, effect) = update(state, key(KeyCode::Enter));
        assert_eq!(
            effect,
            Some(UiEffect::Steer {
                text: "wait".to_owned()
            })
        );
    }

    #[test]
    fn esc_cancels_when_working_and_quits_when_idle() {
        let mut state = UiState::new();
        state.status = UiStatus::Working {
            operation: "running bash".to_owned(),
        };
        let (_, effect) = update(state, key(KeyCode::Esc));
        assert_eq!(effect, Some(UiEffect::Cancel));

        let state = UiState::new();
        let (state, effect) = update(state, key(KeyCode::Esc));
        assert_eq!(effect, Some(UiEffect::Quit));
        assert!(state.quit_requested);
    }

    #[test]
    fn ctrl_c_cancels_first_then_quits() {
        let mut state = UiState::new();
        state.status = UiStatus::Working {
            operation: "running".to_owned(),
        };
        let (_, effect) = update(
            state,
            UiMessage::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        );
        assert_eq!(effect, Some(UiEffect::Cancel));
        // After cancel the operation goes idle; the next ctrl-c quits.
        let state = UiState::new();
        let (_, effect) = update(
            state,
            UiMessage::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        );
        assert_eq!(effect, Some(UiEffect::Quit));
    }

    #[test]
    fn runtime_events_drive_status_and_scrollback() {
        let state = UiState::new();
        let operation_id = OperationId::generate();
        let (state, _) = update(
            state,
            UiMessage::Runtime(RuntimeEvent::OperationStarted {
                cursor: Default::default(),
                operation_id,
                prompt: "read the design".to_owned(),
            }),
        );
        assert!(matches!(state.status, UiStatus::Working { .. }));
        assert_eq!(
            state.pending_scrollback[0].to_string(),
            "you » read the design"
        );

        let (state, _) = update(
            state,
            UiMessage::Runtime(RuntimeEvent::AssistantTextDelta {
                cursor: Default::default(),
                operation_id,
                text: "hello".to_owned(),
            }),
        );
        assert_eq!(state.draft, "hello");

        let (state, _) = update(
            state,
            UiMessage::Runtime(RuntimeEvent::OperationFinished {
                cursor: Default::default(),
                operation_id,
            }),
        );
        assert_eq!(state.status, UiStatus::Idle);
        assert!(
            state
                .pending_scrollback
                .iter()
                .any(|line| line.to_string().contains("ion « hello"))
        );
        assert!(state.draft.is_empty());
    }

    #[test]
    fn rejection_lands_in_scrollback() {
        let state = UiState::new();
        let (state, _) = update(state, UiMessage::SubmitRejected("busy".to_owned()));
        assert!(state.pending_scrollback[0].to_string().contains("busy"));
    }

    #[test]
    fn renders_composer_and_status_to_test_backend() {
        use ratatui::{Terminal as RTerminal, backend::TestBackend};
        let mut state = UiState::new();
        state.composer = "hello world".to_owned();
        state.status = UiStatus::Working {
            operation: "running bash".to_owned(),
        };
        let backend = TestBackend::new(40, 6);
        let mut terminal = RTerminal::new(backend).expect("terminal");
        terminal.draw(|frame| render(&state, frame)).expect("draw");
        let buffer = terminal.backend().buffer();
        let content: String = (0..buffer.area().height)
            .map(|y| {
                (0..buffer.area().width)
                    .map(|x| {
                        buffer
                            .cell((x, y))
                            .map(|cell| cell.symbol().to_owned())
                            .unwrap_or_default()
                    })
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(content.contains("hello world▏"), "{content}");
        assert!(content.contains("● running bash"), "{content}");
    }
}
