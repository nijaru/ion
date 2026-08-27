//! Ratatui TUI frontend. Shared runtime contract: DESIGN.md §21;
//! TUI architecture: TERMINAL.md.
//!
//! One runtime contract: this frontend consumes `SessionHandle`
//! semantics only — snapshot plus bounded live events — and never
//! touches the store. Ion owns application state: [`UiState`] is a
//! plain value, `update` is a pure reducer over [`UiMessage`]s, and
//! effects call back into the session. The terminal is restored by one
//! RAII owner, never scattered across widgets.

use std::io::Write;

use libc::SIGTSTP;

use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr as _;

use crate::settings::Theme;
use ion_core::{
    CommandError, OperationState, OperationStatus, RuntimeError, RuntimeEvent, SessionHandle,
    SessionSnapshot,
};
use ion_terminal::{
    Frame, InputEvent, KeyCode, KeyEvent, Modifiers, Screen, TerminalSession, install_panic_hook,
};

mod render;
pub use render::{Palette, palette};
use render::{Transcript, append_snapshot_entries, build_live};
#[cfg(test)]
use render::{entry_lines, wrap_line};

/// Host-provided configuration for one launch. Cloneable handles;
/// never runtime state.
#[derive(Clone)]
pub struct HostConfig {
    /// Model id for the /model display; also marks switching as
    /// possible (a real model is configured; scripted launches have
    /// nothing to switch to).
    pub model_name: Option<String>,
    /// Seed for ctrl+t (pi-parity hideThinkingBlock).
    pub hide_thinking_block: bool,
    /// One-time store/startup notice (e.g. archived old-schema
    /// database) rendered once into the transcript.
    pub startup_notice: Option<String>,
    /// Basename of the launch working directory (status line).
    pub cwd_label: Option<String>,
    /// Git branch of the working directory, captured at launch.
    pub branch: Option<String>,
}

/// What the reducer wants the event loop to do. Effects are the only
/// path back into the runtime (TERMINAL.md, runtime interaction).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiEffect {
    Submit {
        text: String,
    },
    Enqueue {
        text: String,
    },
    Steer {
        text: String,
    },
    Compact {
        instructions: Option<String>,
    },
    SwitchModel {
        model: String,
    },
    Cancel,
    /// Approve the parked tool approval (§17.4).
    Approve,
    /// Deny the parked tool approval (§17.4).
    Deny,
    Quit,
}

/// A parked tool invocation awaiting the user's approval decision
/// (DESIGN.md §17.4). While set, the composer is inert and only the
/// decision keys are live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalPrompt {
    pub tool: String,
    pub target: Option<String>,
}

/// Inputs to the reducer: runtime events, keys, resizes.
#[derive(Debug, Clone)]
pub enum UiMessage {
    Runtime(RuntimeEvent),
    Key(KeyEvent),
    /// Bracketed-paste payload; inserted at the cursor.
    Paste(String),
    SubmitAccepted,
    SubmitRejected(String),
    EnqueueAccepted,
    EnqueueRejected(String),
    CompactAccepted,
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

/// One editor/global action, matched against decoded keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    Cancel,
    ClearComposer,
    Submit,
    SteerCurrent,
    ToggleToolOutput,
    ToggleThinking,
    HistoryPrevious,
    HistoryNext,
    CursorLeft,
    CursorRight,
    CursorHome,
    CursorEnd,
    KillToEnd,
    KillToStart,
    KillWord,
    Yank,
}

/// Resolved action → key bindings. Plain data owned by the UI state;
/// defaults match pi's editor defaults where they overlap.
#[derive(Debug, Clone)]
pub struct KeyMap {
    bindings: Vec<(Action, KeyCode, Modifiers)>,
}

impl Default for KeyMap {
    fn default() -> Self {
        let bind = |map: &mut Vec<(Action, KeyCode, Modifiers)>,
                    action: Action,
                    code: KeyCode,
                    modifiers: Modifiers| {
            map.push((action, code, modifiers));
        };
        let mut bindings = Vec::new();
        bind(
            &mut bindings,
            Action::Quit,
            KeyCode::Char('d'),
            Modifiers::CONTROL,
        );
        bind(&mut bindings, Action::Cancel, KeyCode::Esc, Modifiers::NONE);
        bind(
            &mut bindings,
            Action::ClearComposer,
            KeyCode::Char('c'),
            Modifiers::CONTROL,
        );
        bind(
            &mut bindings,
            Action::Submit,
            KeyCode::Enter,
            Modifiers::NONE,
        );
        bind(
            &mut bindings,
            Action::SteerCurrent,
            KeyCode::Enter,
            Modifiers::SHIFT,
        );
        bind(
            &mut bindings,
            Action::HistoryPrevious,
            KeyCode::Up,
            Modifiers::NONE,
        );
        bind(
            &mut bindings,
            Action::HistoryNext,
            KeyCode::Down,
            Modifiers::NONE,
        );
        bind(
            &mut bindings,
            Action::CursorLeft,
            KeyCode::Left,
            Modifiers::NONE,
        );
        bind(
            &mut bindings,
            Action::CursorRight,
            KeyCode::Right,
            Modifiers::NONE,
        );
        bind(
            &mut bindings,
            Action::CursorHome,
            KeyCode::Home,
            Modifiers::NONE,
        );
        bind(
            &mut bindings,
            Action::CursorHome,
            KeyCode::Char('a'),
            Modifiers::CONTROL,
        );
        bind(
            &mut bindings,
            Action::CursorEnd,
            KeyCode::End,
            Modifiers::NONE,
        );
        bind(
            &mut bindings,
            Action::CursorEnd,
            KeyCode::Char('e'),
            Modifiers::CONTROL,
        );
        bind(
            &mut bindings,
            Action::KillToEnd,
            KeyCode::Char('k'),
            Modifiers::CONTROL,
        );
        bind(
            &mut bindings,
            Action::KillToStart,
            KeyCode::Char('u'),
            Modifiers::CONTROL,
        );
        bind(
            &mut bindings,
            Action::KillWord,
            KeyCode::Char('w'),
            Modifiers::CONTROL,
        );
        bind(
            &mut bindings,
            Action::Yank,
            KeyCode::Char('y'),
            Modifiers::CONTROL,
        );
        bind(
            &mut bindings,
            Action::ToggleToolOutput,
            KeyCode::Char('o'),
            Modifiers::CONTROL,
        );
        bind(
            &mut bindings,
            Action::ToggleThinking,
            KeyCode::Char('t'),
            Modifiers::CONTROL,
        );
        KeyMap { bindings }
    }
}

impl KeyMap {
    /// Apply settings overrides: each named action rebinds to the
    /// given key string. Unparsable strings are a config error.
    pub fn from_settings(overrides: &crate::settings::Keybindings) -> Result<Self, String> {
        let mut map = KeyMap::default();
        let rebind =
            |map: &mut KeyMap, action: Action, spec: &Option<String>| -> Result<(), String> {
                if let Some(spec) = spec {
                    let (code, modifiers) = parse_key(spec)?;
                    map.bindings.retain(|(a, _, _)| *a != action);
                    map.bindings.push((action, code, modifiers));
                }
                Ok(())
            };
        rebind(&mut map, Action::Quit, &overrides.quit)?;
        rebind(&mut map, Action::Cancel, &overrides.cancel)?;
        rebind(&mut map, Action::Submit, &overrides.submit)?;
        rebind(
            &mut map,
            Action::HistoryPrevious,
            &overrides.history_previous,
        )?;
        rebind(&mut map, Action::HistoryNext, &overrides.history_next)?;
        rebind(&mut map, Action::CursorLeft, &overrides.cursor_left)?;
        rebind(&mut map, Action::CursorRight, &overrides.cursor_right)?;
        rebind(&mut map, Action::CursorHome, &overrides.cursor_home)?;
        rebind(&mut map, Action::CursorEnd, &overrides.cursor_end)?;
        rebind(&mut map, Action::KillToEnd, &overrides.kill_to_end)?;
        rebind(&mut map, Action::KillToStart, &overrides.kill_to_start)?;
        rebind(&mut map, Action::KillWord, &overrides.kill_word)?;
        rebind(&mut map, Action::Yank, &overrides.yank)?;
        rebind(
            &mut map,
            Action::ToggleToolOutput,
            &overrides.toggle_tool_output,
        )?;
        rebind(&mut map, Action::ToggleThinking, &overrides.toggle_thinking)?;
        Ok(map)
    }

    fn action_for(&self, key: &KeyEvent) -> Option<Action> {
        self.bindings
            .iter()
            .find(|(_, code, modifiers)| *code == key.code && *modifiers == key.modifiers)
            .map(|(action, _, _)| *action)
    }
}

/// Parse a keybinding string like `ctrl+k`, `alt+left`, or `enter`.
fn parse_key(spec: &str) -> Result<(KeyCode, Modifiers), String> {
    let mut modifiers = Modifiers::NONE;
    let mut key = None;
    for part in spec.split('+') {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" => modifiers |= Modifiers::CONTROL,
            "alt" => modifiers |= Modifiers::ALT,
            "shift" => modifiers |= Modifiers::SHIFT,
            "enter" => key = Some(KeyCode::Enter),
            "esc" | "escape" => key = Some(KeyCode::Esc),
            "tab" => key = Some(KeyCode::Tab),
            "backspace" => key = Some(KeyCode::Backspace),
            "delete" => key = Some(KeyCode::Delete),
            "up" => key = Some(KeyCode::Up),
            "down" => key = Some(KeyCode::Down),
            "left" => key = Some(KeyCode::Left),
            "right" => key = Some(KeyCode::Right),
            "home" => key = Some(KeyCode::Home),
            "end" => key = Some(KeyCode::End),
            single if single.chars().count() == 1 => {
                key = Some(KeyCode::Char(single.chars().next().expect("checked")));
            }
            other => return Err(format!("unknown key {other:?} in binding {spec:?}")),
        }
    }
    key.map(|code| (code, modifiers))
        .ok_or_else(|| format!("empty key binding {spec:?}"))
}

/// Live tool-line state, driving the marker color: yellow while
/// running, green on success, red on failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolState {
    Running,
    Ok,
    Error,
}

/// One started tool effect: its display label plus the bounded output
/// preview from settlement (rendered only while expanded).
#[derive(Debug, Clone)]
struct ToolRow {
    tool: String,
    target: Option<String>,
    state: ToolState,
    preview: Option<String>,
}

/// One UI state owner (TERMINAL.md). Plain data; no handles, no hidden state.
#[derive(Debug, Clone, Default)]
pub struct UiState {
    /// Composer buffer.
    composer: String,
    /// Cursor position as a char offset into `composer`.
    cursor: usize,
    /// Submitted prompts, oldest first; up/down navigates.
    history: Vec<String>,
    /// Position in `history` while browsing; None edits the live
    /// draft.
    history_index: Option<usize>,
    /// The live draft set aside when history browsing starts.
    history_stash: Option<String>,
    /// Last kill (ctrl-k/u/w); ctrl-y yanks it back.
    kill_buffer: String,
    /// Resolved key bindings (settings overrides applied).
    keymap: KeyMap,
    /// Streaming assistant draft for the live step.
    draft: String,
    /// Live reasoning text for the current step (display-only).
    draft_thinking: String,
    /// Whether reasoning renders at all (ctrl+t; seeded by the
    /// hideThinkingBlock setting).
    thinking_visible: bool,
    /// Whether /model <id> can switch (host provided a switch handle).
    model_switching_available: bool,
    /// Whether settled tool rows render their output preview
    /// (ctrl+o, pi-parity app.tools.expand).
    tool_output_expanded: bool,
    /// True after an event lag dropped deltas: the draft is partial
    /// and must never present as a completed turn.
    draft_degraded: bool,
    /// Completed tool rows for the live operation, newest last.
    tool_rows: Vec<ToolRow>,
    /// Basename of the working directory for the status line.
    cwd_label: Option<String>,
    /// Git branch of the working directory, captured at launch.
    branch: Option<String>,
    status: UiStatus,
    /// Model id for /model display (host-provided, not runtime state).
    model_name: Option<String>,
    /// Lines queued for scrollback: flushed above the inline viewport
    /// when the composer redraws.
    pending_scrollback: Vec<Line<'static>>,
    quit_requested: bool,
    /// Previous ctrl+c press, for the double-press exit (pi parity).
    last_clear: Option<std::time::Instant>,
    /// Transient footer hint (e.g. "ctrl+c again to exit").
    hint: Option<String>,
    /// The parked tool approval awaiting the user's decision (§17.4);
    /// set by the runtime, cleared by the decision or the next tool.
    approval: Option<ApprovalPrompt>,
}

impl UiState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Host-provided model id for the /model display.
    pub fn set_model_name(&mut self, model_name: Option<String>) {
        self.model_name = model_name;
    }

    /// Replace the key bindings with settings-resolved ones.
    pub fn set_keymap(&mut self, keymap: KeyMap) {
        self.keymap = keymap;
    }
}

/// Pure reducer: `update(UiState, UiMessage) -> UiState` plus
/// at most one effect. Deterministic; no I/O.
#[must_use]
pub fn update(state: UiState, message: UiMessage) -> (UiState, Option<UiEffect>) {
    let mut state = state;
    match message {
        UiMessage::Key(key) => {
            state.hint = None;
            handle_key(state, key)
        }
        UiMessage::Paste(text) => {
            insert_at_cursor(&mut state, &text);
            (state, None)
        }
        UiMessage::Runtime(event) => (apply_runtime_event(state, event), None),
        UiMessage::SubmitAccepted => {
            state.composer.clear();
            (state, None)
        }
        UiMessage::CompactAccepted => {
            state
                .pending_scrollback
                .push(Line::from("compaction requested at next boundary").dim());
            (state, None)
        }
        UiMessage::SteerAccepted => {
            state.composer.clear();
            (state, None)
        }
        UiMessage::EnqueueAccepted => {
            state
                .pending_scrollback
                .push(Line::from("queued for the next operation").dim());
            state.composer.clear();
            (state, None)
        }
        UiMessage::SubmitRejected(message)
        | UiMessage::EnqueueRejected(message)
        | UiMessage::SteerRejected(message) => {
            state
                .pending_scrollback
                .push(Line::from(format!("! {message}")).red());
            (state, None)
        }
    }
}

fn handle_key(mut state: UiState, key: KeyEvent) -> (UiState, Option<UiEffect>) {
    // A parked approval owns the keyboard (§17.4): only the decision
    // keys act; every other key is swallowed so a stray keystroke can
    // never submit, quit, or edit the composer mid-decision.
    if state.approval.is_some() {
        let allow = match key.code {
            KeyCode::Enter if key.modifiers.is_empty() => Some(true),
            KeyCode::Char('y') if key.modifiers.is_empty() || key.modifiers == Modifiers::SHIFT => {
                Some(true)
            }
            KeyCode::Esc if key.modifiers.is_empty() => Some(false),
            KeyCode::Char('n') if key.modifiers.is_empty() || key.modifiers == Modifiers::SHIFT => {
                Some(false)
            }
            _ => None,
        };
        let Some(allow) = allow else {
            return (state, None);
        };
        state.approval = None;
        return (
            state,
            Some(if allow {
                UiEffect::Approve
            } else {
                UiEffect::Deny
            }),
        );
    }
    if let Some(action) = state.keymap.action_for(&key) {
        return handle_action(state, action);
    }
    match key.code {
        KeyCode::Backspace => handle_backspace(state),
        KeyCode::Delete => {
            let mut state = state;
            delete_at_cursor(&mut state);
            (state, None)
        }
        KeyCode::Char(' ') if key.modifiers.is_empty() => {
            let mut state = state;
            insert_at_cursor(&mut state, " ");
            (state, None)
        }
        KeyCode::Char(ch) if key.modifiers.is_empty() || key.modifiers == Modifiers::SHIFT => {
            let mut state = state;
            insert_at_cursor(&mut state, &ch.to_string());
            (state, None)
        }
        _ => (state, None),
    }
}

fn handle_backspace(mut state: UiState) -> (UiState, Option<UiEffect>) {
    if state.cursor > 0 {
        state.cursor -= 1;
        delete_at_cursor(&mut state);
    }
    (state, None)
}

/// One user-visible scrollback notice (system line).
fn notice(state: &mut UiState, text: &str) {
    state
        .pending_scrollback
        .push(Line::from(text.to_owned()).dim());
}

/// Slash-command surface: /help, /compact, /model. Anything else is a
/// visible unknown-command error, never a silent no-op.
fn handle_command(state: &mut UiState, command: &str) -> (UiState, Option<UiEffect>) {
    let (name, rest) = match command.split_once(' ') {
        Some((name, rest)) => (name, rest.trim()),
        None => (command, ""),
    };
    match name {
        "help" => {
            for line in [
                "/compact [instructions] - summarize the active operation's context",
                "/model [id]             - show or switch the model",
                "enter                   - submit or queue the next operation",
                "shift+enter             - steer the active operation",
                "ctrl+o                  - toggle tool output previews",
                "ctrl+t                  - toggle thinking blocks",
                "/help                   - this list",
            ] {
                notice(state, line);
            }
            (std::mem::take(state), None)
        }
        "compact" => {
            let instructions = (!rest.is_empty()).then(|| rest.to_owned());
            (
                std::mem::take(state),
                Some(UiEffect::Compact { instructions }),
            )
        }
        "model" => {
            if rest.is_empty() {
                let shown = state.model_name.as_deref().unwrap_or("(scripted provider)");
                notice(state, &format!("model: {shown}"));
                return (std::mem::take(state), None);
            }
            if !state.model_switching_available {
                notice(state, "model switching unavailable (scripted provider)");
                return (std::mem::take(state), None);
            }
            (
                std::mem::take(state),
                Some(UiEffect::SwitchModel {
                    model: rest.to_owned(),
                }),
            )
        }
        other => {
            notice(state, &format!("unknown command: /{other} (try /help)"));
            (std::mem::take(state), None)
        }
    }
}

fn handle_action(mut state: UiState, action: Action) -> (UiState, Option<UiEffect>) {
    match action {
        // Escape interrupts a running operation. At idle it does
        // nothing: quitting is ctrl+c twice or ctrl+d (empty), so a
        // reflexive escape can never exit the app or discard state.
        Action::Cancel => {
            if matches!(state.status, UiStatus::Idle) {
                state.hint = None;
                (state, None)
            } else {
                (state, Some(UiEffect::Cancel))
            }
        }
        // Pi parity: ctrl+d exits only from an empty, idle composer.
        Action::Quit => {
            if state.composer.is_empty() && matches!(state.status, UiStatus::Idle) {
                state.quit_requested = true;
                (state, Some(UiEffect::Quit))
            } else {
                (state, None)
            }
        }
        // Pi parity: ctrl+c clears a non-empty composer; a second
        // press within 2s exits.
        Action::ClearComposer => {
            if !state.composer.is_empty() {
                state.composer.clear();
                state.cursor = 0;
                state.hint = None;
                state.last_clear = None;
            } else if state
                .last_clear
                .is_some_and(|at| at.elapsed() < std::time::Duration::from_secs(2))
            {
                state.quit_requested = true;
                return (state, Some(UiEffect::Quit));
            } else {
                state.hint = Some("ctrl+c again to exit".to_owned());
                state.last_clear = Some(std::time::Instant::now());
            }
            (state, None)
        }
        Action::ToggleToolOutput => {
            state.tool_output_expanded = !state.tool_output_expanded;
            (state, None)
        }
        Action::ToggleThinking => {
            state.thinking_visible = !state.thinking_visible;
            (state, None)
        }
        Action::Submit => {
            let text = state.composer.trim().to_owned();
            if text.is_empty() {
                return (state, None);
            }
            state.composer.clear();
            state.cursor = 0;
            state.history_index = None;
            state.history_stash = None;
            // Slash commands are frontend presentation over SessionHandle
            // commands - never TUI-only session logic.
            if let Some(command) = text.strip_prefix('/') {
                return handle_command(&mut state, command);
            }
            state.history.push(text.clone());
            match &state.status {
                UiStatus::Idle => (state, Some(UiEffect::Submit { text })),
                UiStatus::Working { .. } => (state, Some(UiEffect::Enqueue { text })),
            }
        }
        Action::SteerCurrent => {
            let text = state.composer.trim().to_owned();
            if text.is_empty() || matches!(state.status, UiStatus::Idle) {
                return (state, None);
            }
            state.composer.clear();
            state.cursor = 0;
            state.history_index = None;
            state.history_stash = None;
            state.history.push(text.clone());
            (state, Some(UiEffect::Steer { text }))
        }
        Action::CursorLeft if state.cursor > 0 => {
            state.cursor -= 1;
            state.exit_history_browse();
            (state, None)
        }
        Action::CursorRight if state.cursor < state.composer.chars().count() => {
            state.cursor += 1;
            state.exit_history_browse();
            (state, None)
        }
        Action::CursorLeft | Action::CursorRight => (state, None),
        Action::CursorHome => {
            state.cursor = 0;
            (state, None)
        }
        Action::CursorEnd => {
            state.cursor = state.composer.chars().count();
            (state, None)
        }
        Action::HistoryPrevious => {
            browse_history(&mut state, -1);
            (state, None)
        }
        Action::HistoryNext => {
            browse_history(&mut state, 1);
            (state, None)
        }
        Action::KillToEnd => {
            let chars = state.composer.chars().count();
            state.kill_buffer = split_off_chars(&mut state.composer, state.cursor, chars);
            (state, None)
        }
        Action::KillToStart => {
            state.kill_buffer = split_off_chars(&mut state.composer, 0, state.cursor);
            state.cursor = 0;
            (state, None)
        }
        Action::KillWord => {
            let start = word_start(&state.composer, state.cursor);
            state.kill_buffer = split_off_chars(&mut state.composer, start, state.cursor);
            state.cursor = start;
            (state, None)
        }
        Action::Yank => {
            let yank = state.kill_buffer.clone();
            insert_at_cursor(&mut state, &yank);
            (state, None)
        }
    }
}

fn insert_at_cursor(state: &mut UiState, text: &str) {
    let byte = char_offset_to_byte(&state.composer, state.cursor);
    state.composer.insert_str(byte, text);
    state.cursor += text.chars().count();
    state.exit_history_browse();
}

fn delete_at_cursor(state: &mut UiState) {
    let end = char_offset_to_byte(&state.composer, state.cursor + 1);
    let byte = char_offset_to_byte(&state.composer, state.cursor);
    if byte < end {
        state.composer.replace_range(byte..end, "");
    }
}

/// Remove chars `[from, to)` and return them.
fn split_off_chars(buffer: &mut String, from: usize, to: usize) -> String {
    let start = char_offset_to_byte(buffer, from);
    let end = char_offset_to_byte(buffer, to);
    buffer.drain(start..end).collect()
}

/// Start of the word before `cursor` (whitespace-delimited).
fn word_start(buffer: &str, cursor: usize) -> usize {
    let chars: Vec<char> = buffer.chars().take(cursor).collect();
    let mut i = chars.len();
    while i > 0 && chars[i - 1].is_whitespace() {
        i -= 1;
    }
    while i > 0 && !chars[i - 1].is_whitespace() {
        i -= 1;
    }
    i
}

fn char_offset_to_byte(buffer: &str, offset: usize) -> usize {
    buffer
        .char_indices()
        .nth(offset)
        .map_or(buffer.len(), |(byte, _)| byte)
}

impl UiState {
    fn exit_history_browse(&mut self) {
        if self.history_index.is_some() {
            self.history_index = None;
            self.history_stash = None;
        }
    }
}

/// Step through submitted prompts; direction -1 is older. Leaving the
/// live draft stashes it; stepping past the newest entry restores it.
fn browse_history(state: &mut UiState, direction: i32) {
    if state.history.is_empty() {
        return;
    }
    let index = match state.history_index {
        None => {
            if direction >= 0 {
                return;
            }
            state.history_stash = Some(state.composer.clone());
            state.history.len() - 1
        }
        Some(index) => {
            let next = index as i64 + i64::from(direction);
            if next < 0 {
                return;
            }
            if next as usize >= state.history.len() {
                state.history_index = None;
                state.composer = state.history_stash.take().unwrap_or_default();
                state.cursor = state.composer.chars().count();
                return;
            }
            next as usize
        }
    };
    state.history_index = Some(index);
    state.composer = state.history[index].clone();
    state.cursor = state.composer.chars().count();
}

fn apply_runtime_event(mut state: UiState, event: RuntimeEvent) -> UiState {
    let palette = palette(Theme::Dark);
    match event {
        RuntimeEvent::OperationStarted { prompt, .. } => {
            state
                .pending_scrollback
                .push(Line::from(format!("> {prompt}")).style(palette.user_marker));
            state.draft.clear();
            state.tool_rows.clear();
            state.status = UiStatus::Working {
                operation: "thinking".to_owned(),
            };
        }
        RuntimeEvent::AssistantTextDelta { text, .. } => {
            flush_thinking(&mut state);
            state.draft.push_str(&text);
        }
        RuntimeEvent::ThinkingDelta { text, .. } => {
            state.draft_thinking.push_str(&text);
        }
        RuntimeEvent::ApprovalPending { tool, target, .. } => {
            // The operation is live but parked; the draft stays put and
            // the prompt owns the band until the decision arrives.
            state.approval = Some(ApprovalPrompt { tool, target });
            state.status = UiStatus::Working {
                operation: "awaiting approval".to_owned(),
            };
        }
        RuntimeEvent::ToolStarted { tool, target, .. } => {
            flush_thinking(&mut state);
            state.approval = None;
            state.status = UiStatus::Working {
                operation: format!("running {tool}"),
            };
            state.tool_rows.push(ToolRow {
                tool,
                target,
                state: ToolState::Running,
                preview: None,
            });
        }
        RuntimeEvent::ToolSettled {
            is_error, preview, ..
        } => {
            if let Some(row) = state.tool_rows.last_mut() {
                // The running row is the one this settlement answers.
                row.state = if is_error {
                    ToolState::Error
                } else {
                    ToolState::Ok
                };
                row.preview = preview;
            }
        }
        RuntimeEvent::OperationFinished { .. } => {
            state.flush_draft();
            state.approval = None;
            state.status = UiStatus::Idle;
        }
        RuntimeEvent::OperationFailed { message, .. } => {
            state.flush_draft();
            state
                .pending_scrollback
                .push(Line::from(format!("! failed: {message}")).red());
            state.approval = None;
            state.status = UiStatus::Idle;
        }
        RuntimeEvent::OperationIndeterminate { message, .. } => {
            state.flush_draft();
            state.pending_scrollback.push(
                Line::from(format!("! indeterminate: {message}"))
                    .yellow()
                    .bold(),
            );
            state.approval = None;
            state.status = UiStatus::Idle;
        }
        RuntimeEvent::OperationCancelled { .. } => {
            state.flush_draft();
            state
                .pending_scrollback
                .push(Line::from("! cancelled".to_owned()).yellow());
            state.approval = None;
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
            state.approval = None;
            state.status = UiStatus::Idle;
        }
        RuntimeEvent::SessionClosed { .. } => {
            state.quit_requested = true;
        }
    }
    state
}

/// Markdown-lite inline styling for assistant scrollback: `**bold**`
/// and `` `code` `` spans; unmatched markers stay literal. Headers
/// (leading #) render bold. No block structure — the viewport wraps
/// plain lines.
fn markdown_line(line: &str) -> Line<'static> {
    let mut spans = Vec::new();
    let mut rest = line;
    if let Some(after) = rest.strip_prefix('#') {
        let after = after.trim_start();
        spans.push(Span::from(format!("## {after}")).bold());
        return Line::from(spans);
    }
    while let Some(start) = rest.find(['`', '*']) {
        let marker = rest[start..].chars().next().expect("find matched");
        let token = if marker == '`' { "`" } else { "**" };
        let Some(end) = rest[start + token.len()..].find(token) else {
            break;
        };
        let (plain, styled, tail) = (
            &rest[..start],
            &rest[start + token.len()..start + token.len() + end],
            &rest[start + token.len() + end + token.len()..],
        );
        if !plain.is_empty() {
            spans.push(Span::from(plain.to_owned()));
        }
        spans.push(if marker == '`' {
            Span::from(styled.to_owned()).cyan()
        } else {
            Span::from(styled.to_owned()).bold()
        });
        rest = tail;
    }
    if !rest.is_empty() || spans.is_empty() {
        spans.push(Span::from(rest.to_owned()));
    }
    Line::from(spans)
}

/// Move accumulated reasoning into scrollback as a dim italic block.
/// Hidden thinking is dropped, matching pi's hideThinkingBlock.
fn flush_thinking(state: &mut UiState) {
    if state.draft_thinking.is_empty() {
        return;
    }
    if state.thinking_visible {
        for line in state.draft_thinking.lines() {
            state
                .pending_scrollback
                .push(Line::from(format!("✻ {line}")).dim().italic());
        }
    }
    state.draft_thinking.clear();
}

impl UiState {
    /// Move the live draft into scrollback as a completed assistant
    /// turn (inline scrollback pattern: completed content leaves the
    /// live viewport). Assistant lines get markdown-lite styling.
    fn flush_draft(&mut self) {
        flush_thinking(self);
        // Tool rows precede the text they enabled. Expanded rendering
        // includes each settled output preview (pi-parity ctrl+o).
        for row in self.tool_rows.drain(..) {
            self.pending_scrollback.push(render::tool_row_line(
                &row.tool,
                row.target.as_deref(),
                row.state,
                None,
            ));
            if self.tool_output_expanded {
                for line in row.preview.iter().flat_map(|p| p.lines()) {
                    self.pending_scrollback
                        .push(Line::from(format!("  {line}")).dark_gray());
                }
            }
        }
        if !self.draft.is_empty() {
            for line in self.draft.lines() {
                // Assistant content renders plain (pi parity); blank
                // lines dropped (single-newline spacing).
                if line.trim().is_empty() {
                    continue;
                }
                self.pending_scrollback.push(markdown_line(line.trim_end()));
            }
            if self.draft_degraded {
                self.pending_scrollback.push(
                    Line::from("… truncated by display lag; full text: ion --resume").yellow(),
                );
                self.draft_degraded = false;
            }
            self.draft.clear();
        }
        self.draft_degraded = false;
    }

    /// Rebuild live state from a fresh snapshot after an event lag
    /// (§21.4): the snapshot is authoritative for operation status;
    /// partial deltas and missed tool rows are display-only losses.
    fn resync_after_lag(&mut self, snapshot: &SessionSnapshot) {
        self.status = match &snapshot.operation {
            OperationStatus::Idle => UiStatus::Idle,
            OperationStatus::Active { prompt, .. } => UiStatus::Working {
                operation: format!("working: {prompt}"),
            },
        };
        // A parked approval is durable state, not a live event (§17.4):
        // it must be reconstructable from the snapshot alone.
        self.approval = match &snapshot.operation {
            OperationStatus::Active {
                state: OperationState::ApprovalPending { call, .. },
                ..
            } => Some(ApprovalPrompt {
                tool: call.name.clone(),
                target: None,
            }),
            _ => None,
        };
        self.tool_rows.clear();
        match &snapshot.live {
            // The snapshot's draft is the runtime's authoritative
            // accumulation, so reconstruction is exact (§21.4).
            Some(live) => {
                self.draft = live.draft_text.clone();
                self.draft_thinking = live.draft_thinking.clone();
                for pending in &live.pending_tools {
                    self.tool_rows.push(ToolRow {
                        tool: pending.tool.clone(),
                        target: pending.target.clone(),
                        state: ToolState::Running,
                        preview: None,
                    });
                }
                self.draft_degraded = false;
            }
            None => {
                self.draft.clear();
                self.draft_thinking.clear();
                self.draft_degraded = false;
            }
        }
    }
}

// Terminal lifecycle and output ownership live in ion_terminal.
fn merge_shutdown_error(slot: &mut Option<RuntimeError>, next: RuntimeError) {
    if let Some(previous) = slot.take() {
        *slot = Some(RuntimeError::OperationFailed(format!("{previous}; {next}")));
    } else {
        *slot = Some(next);
    }
}

/// Enter the terminal before any session state is touched: the
/// close-on-error path suspends open operations, so a failed launch
/// must never get as far as opening or resuming a session.
pub fn setup_terminal() -> Result<TerminalSession, RuntimeError> {
    let session = TerminalSession::enter()
        .map_err(|err| RuntimeError::OperationFailed(format!("terminal setup failed: {err}")))?;
    install_panic_hook();
    // Test-only hook: the PTY restoration test drives a real panic
    // through the guard's restore path.
    if std::env::var_os("ION_TEST_PANIC").is_some() {
        panic!("ION_TEST_PANIC");
    }
    Ok(session)
}

/// Suspend the terminal claim, stop via the default TSTP
/// disposition, then re-arm on wake. In orphaned process groups the
/// kernel discards the re-raised TSTP and execution continues
/// immediately; every step is idempotent so both worlds are correct.
fn suspend_and_rearm(
    terminal: &mut TerminalSession,
    screen: &mut Screen,
    sigtstp: &mut tokio::signal::unix::Signal,
) -> Result<(), RuntimeError> {
    use tokio::signal::unix::{SignalKind, signal};

    // Give the shell back a usable terminal before stopping.
    terminal
        .suspend()
        .map_err(|err| RuntimeError::OperationFailed(format!("terminal suspend failed: {err}")))?;
    screen.invalidate();
    // Default disposition for the real stop: unregister by dropping
    // the stream inside our own slot.
    {
        use tokio::signal::unix::{SignalKind, signal as register};
        // Take the old stream so it drops now, before the re-raise.
        let _old = std::mem::replace(
            sigtstp,
            // Placeholder replaced again right after the raise.
            register(SignalKind::from_raw(SIGTSTP)).map_err(|err| {
                RuntimeError::OperationFailed(format!("signal setup failed: {err}"))
            })?,
        );
    }
    // SAFETY: plain signal syscalls on this process.
    #[allow(unsafe_code)]
    unsafe {
        libc::kill(libc::getpid(), libc::SIGTSTP);
    }
    // Resumed (SIGCONT) or TSTP was swallowed by an orphaned group:
    // re-arm everything.
    *sigtstp = signal(SignalKind::from_raw(SIGTSTP))
        .map_err(|err| RuntimeError::OperationFailed(format!("signal setup failed: {err}")))?;
    terminal
        .resume()
        .map_err(|err| RuntimeError::OperationFailed(format!("terminal resume failed: {err}")))?;
    screen.invalidate();
    Ok(())
}

/// The TUI event loop: runtime events and terminal keys into the
/// reducer; effects dispatch straight back into the session. Never
/// blocks rendering on provider/tool I/O (TERMINAL.md, runtime interaction).
pub async fn run(
    session: SessionHandle,
    resume_session: Option<ion_core::SessionId>,
    theme: Theme,
    keymap: KeyMap,
    host: HostConfig,
    mut terminal: TerminalSession,
) -> Result<(), RuntimeError> {
    let switching_available = host.model_name.is_some();

    let palette = palette(theme);

    let (term_w, term_h) = terminal.size().unwrap_or((80, 24));

    // The banner is committed straight to native scrollback above the
    // region (inline-first semantics): completed content never lives in
    // the diffed window.
    let banner = if resume_session.is_some() {
        format!(
            "ion v{} — resumed; enter sends; escape interrupts",
            env!("CARGO_PKG_VERSION")
        )
    } else {
        format!("ion v{}", env!("CARGO_PKG_VERSION"))
    };
    {
        let out = terminal.output();
        // Raw mode has no ONLCR: every line needs an explicit \r.
        writeln!(out, "{banner}\r").map_err(|err| {
            RuntimeError::OperationFailed(format!("terminal output failed: {err}"))
        })?;
        // Key cheats (pi parity): dim lines under the header.
        for cheat in [
            "escape to interrupt",
            "ctrl+c clear · twice to exit",
            "ctrl+d exit (empty)",
            "shift+enter to steer",
            "ctrl+o tool output",
            "ctrl+t thinking",
            "up/down history",
            "ctrl+z to suspend",
            "/ for commands",
        ] {
            writeln!(out, "{cheat}\r").map_err(|err| {
                RuntimeError::OperationFailed(format!("terminal output failed: {err}"))
            })?;
        }
        out.flush().map_err(|err| {
            RuntimeError::OperationFailed(format!("terminal flush failed: {err}"))
        })?;
    }

    // Anchor the region at the launch cursor. Queried before the
    // EventStream exists, so no competing stdin reader.
    terminal
        .output()
        .record_external(b"\x1b[6n")
        .map_err(|err| RuntimeError::OperationFailed(format!("terminal capture failed: {err}")))?;
    let (_, cursor_row) = terminal
        .cursor_position()
        .map_err(|err| RuntimeError::OperationFailed(format!("cursor query failed: {err}")))?;
    let mut origin = cursor_row;
    // Keep a minimal usable region above the screen bottom.
    const MIN_REGION_ROWS: u16 = 4;
    if term_h.saturating_sub(origin) < MIN_REGION_ROWS {
        let push = MIN_REGION_ROWS - (term_h - origin);
        {
            let out = terminal.output();
            write!(out, "{}", "\n".repeat(push as usize)).map_err(|err| {
                RuntimeError::OperationFailed(format!("terminal output failed: {err}"))
            })?;
            out.flush().map_err(|err| {
                RuntimeError::OperationFailed(format!("terminal flush failed: {err}"))
            })?;
        }
        origin = origin.saturating_sub(push);
    }
    let mut screen = Screen::new(term_w, origin, term_h);

    // Committed transcript: restored entries, flushed turns. Committed
    // lines never change once appended (line-diff model, TERMINAL.md).
    let mut transcript = Transcript::new(term_w);

    // The EventStream is the sole terminal reader, so crossterm parses
    // cursor-position responses itself; blocking cursor queries (used
    // by Terminal::clear) cannot deadlock against key reads.
    let mut key_stream = terminal.input();

    // One live UiState for the whole loop; host-provided display
    // config seeds it here and nowhere else.
    let mut state = UiState::new();
    state.set_keymap(keymap);
    state.set_model_name(host.model_name.clone());
    state.thinking_visible = !host.hide_thinking_block;
    state.cwd_label = host.cwd_label.clone();
    state.branch = host.branch.clone();
    state.model_switching_available = switching_available;
    if let Some(notice) = host.startup_notice {
        state
            .pending_scrollback
            .push(Line::from(format!("! {notice}")).yellow().bold());
    }
    let (snapshot, mut events) = session.subscribe().await?;
    let resume_entry_count = snapshot.reopen_entry_count.unwrap_or(0);
    // The session's durable selection is authoritative once subscribed;
    // a resumed session may have switched models in an earlier run.
    // Scripted launches keep the host's display fallback.
    if host.model_name.is_some() {
        state.set_model_name(Some(snapshot.model_ref.clone()));
    }
    if let Some(warning) = &snapshot.indeterminate {
        state.pending_scrollback.push(
            Line::from(format!(
                "! indeterminate operation {}: {}",
                warning.operation_id, warning.message
            ))
            .yellow()
            .bold(),
        );
    }
    // §21.4/§31.14: the initial snapshot is authoritative for durable
    // history. Entries settled between the resume load and this subscribe
    // are placed after the resume marker.
    append_snapshot_entries(
        &mut transcript,
        &snapshot.entries,
        resume_entry_count,
        resume_session,
        &palette,
    );
    let mut active_operation: Option<ion_core::OperationId> = match snapshot.operation {
        OperationStatus::Active { operation_id, .. } => Some(operation_id),
        OperationStatus::Idle => None,
    };
    // A parked approval is durable state: re-surface it at launch so a
    // resume never strands a waiting decision (§17.4).
    state.approval = match &snapshot.operation {
        OperationStatus::Active {
            state: OperationState::ApprovalPending { call, .. },
            ..
        } => Some(ApprovalPrompt {
            tool: call.name.clone(),
            target: None,
        }),
        _ => None,
    };
    let mut result: Result<(), RuntimeError> = Ok(());
    // Crossterm's EventStream can terminate on transient reads (notably
    // SIGWINCH during resize). Recreate it rather than treating the
    // stream end as fatal; give up only after repeated immediate ends.
    let mut stream_recreations = 0u32;
    const MAX_STREAM_RECREATIONS: u32 = 64;

    // Job control (TERMINAL.md): SIGTSTP must leave the shell a cooked,
    // capability-clean terminal while ion is stopped; SIGCONT re-arms
    // the negotiated modes and repaints. In orphaned process groups a
    // re-raised SIGTSTP is discarded by the kernel, so the raise may
    // return immediately; the resume path is idempotent either way.
    let mut sigtstp =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::from_raw(SIGTSTP))
            .map_err(|err| RuntimeError::OperationFailed(format!("signal setup failed: {err}")))?;
    let mut sigcont =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::from_raw(libc::SIGCONT))
            .map_err(|err| RuntimeError::OperationFailed(format!("signal setup failed: {err}")))?;

    loop {
        // Size changes are polled directly: resize events ride the same
        // fragile stream as keys.
        if let Ok((w, h)) = terminal.size() {
            screen.resize(w, h);
        }
        transcript.rewrap_if_needed(screen.size().0);
        // Flush completed turns into the committed transcript, then
        // draw committed history + live band as one line-diff frame
        // (line-diff model, TERMINAL.md).
        if !state.pending_scrollback.is_empty() {
            let flushed = std::mem::take(&mut state.pending_scrollback);
            transcript.extend(flushed);
        }
        let (live, live_cursor) = build_live(&state, &palette, screen.size().0 as usize);
        let cursor = live_cursor.map(|(row, col)| (transcript.wrapped.len() + row, col));
        terminal
            .render(
                &mut screen,
                &Frame {
                    committed: &transcript.wrapped,
                    live: &live,
                    cursor,
                },
            )
            .map_err(|err| RuntimeError::OperationFailed(format!("draw failed: {err}")))?;

        if state.quit_requested {
            break;
        }

        tokio::select! {
            maybe_key = key_stream.next() => {
                match maybe_key {
                    Some(Ok(InputEvent::Key(key))) => {
                        stream_recreations = 0;
                        if key.code == KeyCode::Char('z') && key.modifiers | Modifiers::CONTROL == key.modifiers
                        {
                            if let Err(err) =
                                suspend_and_rearm(&mut terminal, &mut screen, &mut sigtstp)
                            {
                                result = Err(err);
                                break;
                            }
                            continue;
                        }
                        let (next, effect) = update(state, UiMessage::Key(key));
                        state = next;
                        if let Some(effect) = effect {
                            dispatch(&session, &mut state, active_operation, effect).await;
                        }
                    }
                    Some(Ok(InputEvent::Paste(text))) => {
                        stream_recreations = 0;
                        let (next, _) = update(state, UiMessage::Paste(text));
                        state = next;
                    }
                    // Resize/focus/mouse: handled by per-frame size
                    // polling; a successfully read event proves the
                    // stream is healthy.
                    Some(Ok(_)) => {
                        stream_recreations = 0;
                    }
                    None | Some(Err(_)) => {
                        stream_recreations += 1;
                        if stream_recreations > MAX_STREAM_RECREATIONS {
                            result = Err(RuntimeError::OperationFailed(
                                "terminal event stream ended".to_owned(),
                            ));
                            break;
                        }
                        key_stream = terminal.input();
                    }
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
                                | RuntimeEvent::OperationIndeterminate { .. }
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
                        // Bounded loss (§21.4): re-subscribe; the fresh
                        // snapshot is authoritative for live state.
                        match session.subscribe().await {
                            Ok((snapshot, fresh)) => {
                                events = fresh;
                                active_operation = match &snapshot.operation {
                                    OperationStatus::Active { operation_id, .. } => {
                                        Some(*operation_id)
                                    }
                                    OperationStatus::Idle => None,
                                };
                                state.resync_after_lag(&snapshot);
                                // §21.4/§31.14: the snapshot is also
                                // authoritative for committed history. The
                                // resume marker is a presentation boundary,
                                // not a rendered-line prefix.
                                transcript.clear();
                                append_snapshot_entries(
                                    &mut transcript,
                                    &snapshot.entries,
                                    resume_entry_count,
                                    resume_session,
                                    &palette,
                                );
                            }
                            Err(err) => {
                                result = Err(err.into());
                                break;
                            }
                        }
                        continue;
                    }
                    Err(err) => {
                        result = Err(err);
                        break;
                    }
                }
            }
            _ = sigtstp.recv() => {
                if let Err(err) = suspend_and_rearm(&mut terminal, &mut screen, &mut sigtstp) {
                    result = Err(err);
                    break;
                }
            }
            _ = sigcont.recv() => {
                // External continue after a stop we did not observe:
                // make sure modes and surface are live again.
                if let Err(err) = terminal.resume() {
                    result = Err(RuntimeError::OperationFailed(format!(
                        "terminal resume failed: {err}"
                    )));
                    break;
                }
                screen.invalidate();
            }
        }
    }

    let finish_result = screen.finish(terminal.output());
    let flush_result = terminal.output().flush();
    let restore_result = terminal.restore();
    let close_result = match session.close().await {
        Ok(()) | Err(CommandError::Closed) => Ok(()),
        Err(err) => Err(err.into()),
    };

    let mut shutdown_error = result.err();
    if let Err(err) = finish_result {
        merge_shutdown_error(
            &mut shutdown_error,
            RuntimeError::OperationFailed(format!("terminal finish failed: {err}")),
        );
    }
    if let Err(err) = flush_result {
        merge_shutdown_error(
            &mut shutdown_error,
            RuntimeError::OperationFailed(format!("terminal flush failed: {err}")),
        );
    }
    if let Err(err) = restore_result {
        merge_shutdown_error(
            &mut shutdown_error,
            RuntimeError::OperationFailed(format!("terminal restore failed: {err}")),
        );
    }
    if let Err(err) = close_result {
        merge_shutdown_error(&mut shutdown_error, err);
    }
    shutdown_error.map_or(Ok(()), Err)
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
        UiEffect::Submit { text } => match session.submit_if_idle(text).await {
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
        UiEffect::Enqueue { text } => match session.enqueue(text).await {
            Ok(_) => {
                let (next, _) = update(std::mem::take(state), UiMessage::EnqueueAccepted);
                *state = next;
            }
            Err(err) => {
                let (next, _) = update(
                    std::mem::take(state),
                    UiMessage::EnqueueRejected(err.to_string()),
                );
                *state = next;
            }
        },
        UiEffect::Compact { instructions } => match session.compact(instructions).await {
            Ok(true) => {
                let (next, _) = update(std::mem::take(state), UiMessage::CompactAccepted);
                *state = next;
            }
            Ok(false) => notice(
                state,
                "nothing to compact: compaction runs within an operation",
            ),
            Err(err) => notice(state, &format!("compact failed: {err}")),
        },
        UiEffect::SwitchModel { model } => match session.switch_model(&model).await {
            Ok(previous) => {
                state.model_name = Some(model.clone());
                notice(state, &format!("model switched: {previous} -> {model}"));
                let (next, _) = update(std::mem::take(state), UiMessage::SubmitAccepted);
                *state = next;
            }
            Err(err) => notice(state, &format!("model switch failed: {err}")),
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
        UiEffect::Approve | UiEffect::Deny => {
            let allow = matches!(effect, UiEffect::Approve);
            let Some(operation_id) = active_operation else {
                notice(state, "approval: no active operation");
                return;
            };
            if let Err(err) = session.decide_approval(operation_id, allow).await {
                notice(state, &format!("approval decision failed: {err}"));
            }
        }
        UiEffect::Cancel => {
            if let Some(operation_id) = active_operation
                && let Err(err) = session.cancel(operation_id).await
            {
                notice(state, &format!("cancel failed: {err}"));
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ion_core::{OperationId, RuntimeCursor, RuntimeInstanceId};
    use ratatui::style::{Color, Modifier};

    pub(crate) fn key(code: KeyCode) -> UiMessage {
        UiMessage::Key(KeyEvent::new(code, Modifiers::NONE))
    }

    pub(crate) fn ctrl(ch: char) -> UiMessage {
        UiMessage::Key(KeyEvent::new(KeyCode::Char(ch), Modifiers::CONTROL))
    }

    pub(crate) fn type_text(state: UiState, text: &str) -> UiState {
        text.chars()
            .fold(state, |state, ch| update(state, key(KeyCode::Char(ch))).0)
    }

    #[test]
    fn cursor_moves_and_edits_mid_string() {
        let state = type_text(UiState::new(), "hello");
        let state = update(state, key(KeyCode::Left)).0;
        let state = update(state, key(KeyCode::Left)).0;
        let state = update(state, key(KeyCode::Left)).0;
        let state = type_text(state, "X");
        assert_eq!(state.composer.as_str(), "heXllo");
        let state = update(state, key(KeyCode::Delete)).0;
        assert_eq!(state.composer.as_str(), "heXlo");
    }

    #[test]
    fn paste_inserts_at_cursor() {
        let state = type_text(UiState::new(), "ab");
        let (state, _) = update(state, UiMessage::Paste("cd".to_owned()));
        assert_eq!(state.composer.as_str(), "abcd");
        let state = update(state, key(KeyCode::Home)).0;
        let (state, _) = update(state, UiMessage::Paste("0".to_owned()));
        assert_eq!(state.composer.as_str(), "0abcd");
    }

    #[test]
    fn history_browses_and_restores_draft() {
        let state = type_text(UiState::new(), "first");
        let (state, effect) = update(state, key(KeyCode::Enter));
        assert!(matches!(effect, Some(UiEffect::Submit { .. })));
        let state = type_text(state, "draft");
        let state = update(state, key(KeyCode::Up)).0;
        assert_eq!(state.composer.as_str(), "first");
        let state = update(state, key(KeyCode::Down)).0;
        assert_eq!(state.composer.as_str(), "draft");
    }

    #[test]
    fn submit_pushes_history_and_clears_composer() {
        let state = type_text(UiState::new(), "one");
        let (state, _) = update(state, key(KeyCode::Enter));
        assert_eq!(state.composer.as_str(), "");
        let state = update(state, key(KeyCode::Up)).0;
        assert_eq!(state.composer.as_str(), "one");
    }

    #[test]
    fn kill_and_yank_round_trip() {
        let state = type_text(UiState::new(), "hello world");
        let state = update(state, ctrl('w')).0;
        assert_eq!(state.composer.as_str(), "hello ");
        let state = update(state, ctrl('y')).0;
        assert_eq!(state.composer.as_str(), "hello world");
    }

    #[test]
    fn markdown_lite_styles_bold_and_code() {
        let line = markdown_line("use **bold** and `code` here");
        let text: Vec<String> = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(text, vec!["use ", "bold", " and ", "code", " here"]);
        assert!(line.spans[1].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(line.spans[3].style.fg, Some(Color::Cyan));
    }

    #[test]
    fn markdown_lite_leaves_unmatched_markers_literal() {
        let line = markdown_line("a * b and c` d");
        let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(text, "a * b and c` d");
    }

    #[test]
    fn parse_key_handles_modifiers_and_names() {
        assert_eq!(
            parse_key("ctrl+k").unwrap(),
            (KeyCode::Char('k'), Modifiers::CONTROL)
        );
        assert_eq!(
            parse_key("alt+left").unwrap(),
            (KeyCode::Left, Modifiers::ALT)
        );
        assert_eq!(
            parse_key("enter").unwrap(),
            (KeyCode::Enter, Modifiers::NONE)
        );
        assert!(parse_key("ctrl+nope").is_err());
    }

    #[test]
    fn keymap_override_rebinds_action() {
        let overrides: crate::settings::Keybindings = toml::from_str("quit = \"ctrl+q\"").unwrap();
        let map = KeyMap::from_settings(&overrides).unwrap();
        let ctrl_q = KeyEvent::new(KeyCode::Char('q'), Modifiers::CONTROL);
        assert_eq!(map.action_for(&ctrl_q), Some(Action::Quit));
        // The old binding is gone.
        let ctrl_d = KeyEvent::new(KeyCode::Char('d'), Modifiers::CONTROL);
        assert_eq!(map.action_for(&ctrl_d), None);
    }

    #[test]
    fn default_keymap_matches_pi_overlap() {
        let map = KeyMap::default();
        let up = KeyEvent::new(KeyCode::Up, Modifiers::NONE);
        assert_eq!(map.action_for(&up), Some(Action::HistoryPrevious));
        let ctrl_y = KeyEvent::new(KeyCode::Char('y'), Modifiers::CONTROL);
        assert_eq!(map.action_for(&ctrl_y), Some(Action::Yank));
    }

    #[test]
    fn resync_after_lag_reconstructs_live_view_from_snapshot() {
        let mut state = UiState::new();
        state = apply_runtime_event(
            state,
            RuntimeEvent::AssistantTextDelta {
                cursor: RuntimeCursor::default(),
                operation_id: OperationId::generate(),
                text: "stale partial".to_owned(),
            },
        );
        let snapshot = SessionSnapshot {
            cursor: RuntimeCursor::default(),
            runtime_instance_id: RuntimeInstanceId::generate(),
            indeterminate: None,
            reopen_entry_count: None,
            operation: OperationStatus::Active {
                operation_id: OperationId::generate(),
                prompt: "do things".to_owned(),
                state: ion_core::OperationState::NeedAssistant,
            },
            entries: Vec::new(),
            model_ref: "test-model".to_owned(),
            live: Some(ion_core::LiveOperationState {
                draft_text: "authoritative draft".to_owned(),
                draft_thinking: "reasoning so far".to_owned(),
                pending_tools: vec![ion_core::PendingTool {
                    call_id: 7,
                    tool: "read".to_owned(),
                    target: Some("Cargo.toml".to_owned()),
                }],
            }),
        };
        state.resync_after_lag(&snapshot);
        assert_eq!(state.draft, "authoritative draft");
        assert_eq!(state.draft_thinking, "reasoning so far");
        assert!(!state.draft_degraded);
        assert_eq!(state.tool_rows.len(), 1);
        assert_eq!(state.tool_rows[0].tool, "read");
        assert_eq!(state.tool_rows[0].target.as_deref(), Some("Cargo.toml"));
    }

    #[test]
    fn resync_after_lag_on_idle_clears_partial_draft() {
        let mut state = UiState::new();
        state.draft = "partial".to_owned();
        let snapshot = SessionSnapshot {
            cursor: RuntimeCursor::default(),
            runtime_instance_id: RuntimeInstanceId::generate(),
            indeterminate: None,
            reopen_entry_count: None,
            operation: OperationStatus::Idle,
            entries: Vec::new(),
            model_ref: "test-model".to_owned(),
            live: None,
        };
        state.resync_after_lag(&snapshot);
        assert_eq!(state.status, UiStatus::Idle);
        assert!(state.tool_rows.is_empty());
        assert!(state.draft.is_empty());
        assert!(!state.draft_degraded);
    }

    #[test]
    fn degraded_draft_flushes_with_truncation_marker() {
        let mut state = UiState::new();
        state.draft = "partial ans".to_owned();
        state.draft_degraded = true;
        state.flush_draft();
        assert!(state.pending_scrollback.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.contains("truncated by display lag"))
        }));
        assert!(!state.draft_degraded);
    }

    #[test]
    fn resync_after_lag_tracks_active_operation() {
        let mut state = UiState::new();
        let operation_id = OperationId::generate();
        let snapshot = SessionSnapshot {
            cursor: RuntimeCursor::default(),
            runtime_instance_id: RuntimeInstanceId::generate(),
            indeterminate: None,
            reopen_entry_count: None,
            operation: OperationStatus::Active {
                operation_id,
                prompt: "do things".to_owned(),
                state: ion_core::OperationState::NeedAssistant,
            },
            entries: Vec::new(),
            model_ref: "test-model".to_owned(),
            live: None,
        };
        state.resync_after_lag(&snapshot);
        assert_eq!(
            state.status,
            UiStatus::Working {
                operation: "working: do things".to_owned()
            }
        );
    }

    #[test]
    fn resync_after_lag_resurfaces_parked_approval_from_snapshot() {
        let call = ion_core::ToolCall {
            operation_id: OperationId::generate(),
            call_id: 1,
            name: "bash".to_owned(),
            arguments: serde_json::json!({ "command": "echo hi" }),
        };
        let snapshot = SessionSnapshot {
            cursor: RuntimeCursor::default(),
            runtime_instance_id: RuntimeInstanceId::generate(),
            indeterminate: None,
            reopen_entry_count: None,
            operation: OperationStatus::Active {
                operation_id: call.operation_id,
                prompt: "do things".to_owned(),
                state: ion_core::OperationState::ApprovalPending {
                    call,
                    pending: Vec::new(),
                },
            },
            entries: Vec::new(),
            model_ref: "test-model".to_owned(),
            live: None,
        };
        let mut state = UiState::new();
        state.resync_after_lag(&snapshot);
        assert_eq!(
            state.approval,
            Some(ApprovalPrompt {
                tool: "bash".to_owned(),
                target: None,
            })
        );

        // A non-parked snapshot clears a stale prompt.
        let mut snapshot = snapshot;
        snapshot.operation = OperationStatus::Active {
            operation_id: OperationId::generate(),
            prompt: "do things".to_owned(),
            state: ion_core::OperationState::NeedAssistant,
        };
        state.resync_after_lag(&snapshot);
        assert!(state.approval.is_none());
    }

    #[test]
    fn parked_approval_owns_the_keyboard_until_decided() {
        let with_prompt = || {
            let mut state = UiState::new();
            state.composer = "draft".to_owned();
            state.cursor = 5;
            state.approval = Some(ApprovalPrompt {
                tool: "bash".to_owned(),
                target: Some("echo hi".to_owned()),
            });
            state
        };

        // Typing while parked never reaches the composer.
        let (state, effect) = update(with_prompt(), key(KeyCode::Char('h')));
        assert_eq!(state.composer.as_str(), "draft");
        assert!(effect.is_none());

        // Enter approves; y approves.
        let (state, effect) = update(with_prompt(), key(KeyCode::Enter));
        assert_eq!(effect, Some(UiEffect::Approve));
        assert!(state.approval.is_none());
        let (state, effect) = update(with_prompt(), key(KeyCode::Char('y')));
        assert_eq!(effect, Some(UiEffect::Approve));
        assert!(state.approval.is_none());

        // Esc denies; n denies.
        let (state, effect) = update(with_prompt(), key(KeyCode::Esc));
        assert_eq!(effect, Some(UiEffect::Deny));
        assert!(state.approval.is_none());
        let (state, effect) = update(with_prompt(), key(KeyCode::Char('n')));
        assert_eq!(effect, Some(UiEffect::Deny));
        assert!(state.approval.is_none());

        // Ctrl+c and ctrl+d are swallowed too: a stray key can never
        // exit the app mid-decision.
        let (state, effect) = update(with_prompt(), ctrl('c'));
        assert!(effect.is_none());
        assert!(state.approval.is_some());
        let (state, effect) = update(with_prompt(), ctrl('d'));
        assert!(effect.is_none());
        assert!(!state.quit_requested);
    }

    #[test]
    fn without_prompt_esc_stays_idle_safe() {
        // The interception is prompt-scoped: idle Esc does nothing and
        // idle keys behave as before.
        let (state, effect) = update(UiState::new(), key(KeyCode::Esc));
        assert!(effect.is_none());
        let (state, _) = update(state, key(KeyCode::Char('h')));
        assert_eq!(state.composer.as_str(), "h");
    }

    #[test]
    fn approval_event_sets_prompt_and_the_next_tool_clears_it() {
        let operation_id = OperationId::generate();
        let mut state = UiState::new();
        state = apply_runtime_event(
            state,
            RuntimeEvent::ApprovalPending {
                cursor: RuntimeCursor::default(),
                operation_id,
                tool: "bash".to_owned(),
                target: Some("echo hi".to_owned()),
            },
        );
        assert_eq!(
            state.approval,
            Some(ApprovalPrompt {
                tool: "bash".to_owned(),
                target: Some("echo hi".to_owned()),
            })
        );

        state = apply_runtime_event(
            state,
            RuntimeEvent::ToolStarted {
                cursor: RuntimeCursor::default(),
                operation_id,
                call_id: 1,
                tool: "bash".to_owned(),
                target: Some("echo hi".to_owned()),
            },
        );
        assert!(state.approval.is_none());

        // Terminal events clear a prompt that never got a decision.
        let mut state = UiState::new();
        state.approval = Some(ApprovalPrompt {
            tool: "bash".to_owned(),
            target: None,
        });
        state = apply_runtime_event(
            state,
            RuntimeEvent::OperationCancelled {
                cursor: RuntimeCursor::default(),
                operation_id,
            },
        );
        assert!(state.approval.is_none());
        assert_eq!(state.status, UiStatus::Idle);
    }

    #[test]
    fn tool_row_shows_canonical_target() {
        let mut state = UiState::new();
        state = apply_runtime_event(
            state,
            RuntimeEvent::ToolStarted {
                cursor: RuntimeCursor::default(),
                operation_id: OperationId::generate(),
                call_id: 1,
                tool: "read".to_owned(),
                target: Some("Cargo.toml".to_owned()),
            },
        );
        assert_eq!(
            state.tool_rows.last().map(|row| row.target.as_deref()),
            Some(Some("Cargo.toml"))
        );
    }

    #[test]
    fn ctrl_k_kills_to_end() {
        let state = type_text(UiState::new(), "abcdef");
        let state = update(state, key(KeyCode::Left)).0;
        let state = update(state, ctrl('k')).0;
        assert_eq!(state.composer.as_str(), "abcde");
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
    fn working_composer_queues_on_plain_enter() {
        let mut state = UiState::new();
        state.status = UiStatus::Working {
            operation: "thinking".to_owned(),
        };
        state.composer = "wait".to_owned();
        let (_, effect) = update(state, key(KeyCode::Enter));
        assert_eq!(
            effect,
            Some(UiEffect::Enqueue {
                text: "wait".to_owned()
            })
        );
    }

    #[test]
    fn working_composer_steers_on_shift_enter() {
        let mut state = UiState::new();
        state.status = UiStatus::Working {
            operation: "thinking".to_owned(),
        };
        state.composer = "wait".to_owned();
        let (_, effect) = update(
            state,
            UiMessage::Key(KeyEvent::new(KeyCode::Enter, Modifiers::SHIFT)),
        );
        assert_eq!(
            effect,
            Some(UiEffect::Steer {
                text: "wait".to_owned()
            })
        );
    }

    #[test]
    fn esc_cancels_when_working_and_is_inert_when_idle() {
        let mut state = UiState::new();
        state.status = UiStatus::Working {
            operation: "running bash".to_owned(),
        };
        let (_, effect) = update(state, key(KeyCode::Esc));
        assert_eq!(effect, Some(UiEffect::Cancel));

        // Idle: escape never exits — reflexive escapes must not quit
        // the app or touch durable state.
        let state = UiState::new();
        let (state, effect) = update(state, key(KeyCode::Esc));
        assert_eq!(effect, None);
        assert!(!state.quit_requested);
    }

    #[test]
    fn ctrl_c_clears_then_double_press_quits() {
        let mut state = UiState::new();
        state.composer = "draft".to_owned();
        let (state, effect) = update(
            state,
            UiMessage::Key(KeyEvent::new(KeyCode::Char('c'), Modifiers::CONTROL)),
        );
        assert_eq!(effect, None);
        assert!(state.composer.is_empty());

        // First empty press sets a hint; a second within 2s exits.
        let (state, effect) = update(
            state,
            UiMessage::Key(KeyEvent::new(KeyCode::Char('c'), Modifiers::CONTROL)),
        );
        assert_eq!(effect, None);
        assert_eq!(state.hint.as_deref(), Some("ctrl+c again to exit"));
        let (state, effect) = update(
            state,
            UiMessage::Key(KeyEvent::new(KeyCode::Char('c'), Modifiers::CONTROL)),
        );
        assert_eq!(effect, Some(UiEffect::Quit));
        assert!(state.quit_requested);
    }

    #[test]
    fn ctrl_d_quits_only_when_empty_and_idle() {
        let mut state = UiState::new();
        state.composer = "text".to_owned();
        let (state, effect) = update(state, ctrl('d'));
        assert_eq!(effect, None);
        assert!(!state.quit_requested);
        let mut state = state;
        state.composer.clear();
        state.status = UiStatus::Working {
            operation: "busy".to_owned(),
        };
        let (state, effect) = update(state, ctrl('d'));
        assert_eq!(effect, None);
        let mut state = state;
        state.status = UiStatus::Idle;
        let (_, effect) = update(state, ctrl('d'));
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
        assert_eq!(state.pending_scrollback[0].to_string(), "> read the design");

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
                .any(|line| line.to_string().contains("hello"))
        );
        assert!(state.draft.is_empty());
    }

    #[test]
    fn snapshot_projection_keeps_resume_marker_at_entry_boundary() {
        let session_id = ion_core::SessionId::generate();
        let entries = vec![
            ion_core::SessionEntry::UserMessage {
                text: "a".repeat(100),
            },
            ion_core::SessionEntry::AssistantMessage {
                text: "old answer".to_owned(),
            },
            ion_core::SessionEntry::UserMessage {
                text: "new prompt".to_owned(),
            },
        ];
        let mut transcript = Transcript::new(40);
        let palette = render::palette(Theme::Dark);
        append_snapshot_entries(&mut transcript, &entries, 2, Some(session_id), &palette);

        let marker = format!("— resumed session {session_id} —");
        let marker_index = transcript
            .raw
            .iter()
            .position(|line| line.to_string() == marker)
            .expect("resume marker");
        let history_lines = entry_lines(&entries[..2], &palette);
        assert_eq!(marker_index, history_lines.len());
        assert_eq!(
            transcript.raw[marker_index + 1..],
            entry_lines(&entries[2..], &palette)
        );
    }

    #[test]
    fn durable_entries_reflow_with_terminal_width() {
        let entries = [ion_core::SessionEntry::AssistantMessage {
            text: "a".repeat(100),
        }];
        let lines = entry_lines(&entries, &render::palette(Theme::Dark));
        assert_eq!(
            lines.len(),
            1,
            "entry projection must not impose 80 columns"
        );

        let mut transcript = Transcript::new(120);
        transcript.extend(lines);
        assert_eq!(transcript.wrapped.len(), 1);
        transcript.rewrap_if_needed(40);
        assert_eq!(transcript.wrapped.len(), 3);
    }

    #[test]
    fn rejection_lands_in_scrollback() {
        let state = UiState::new();
        let (state, _) = update(state, UiMessage::SubmitRejected("busy".to_owned()));
        assert!(state.pending_scrollback[0].to_string().contains("busy"));
    }

    #[test]
    fn indeterminate_outcome_is_visible_as_inspection_required() {
        let state = apply_runtime_event(
            UiState::new(),
            RuntimeEvent::OperationIndeterminate {
                cursor: RuntimeCursor::default(),
                operation_id: OperationId::generate(),
                message: "inspect before retrying".to_owned(),
            },
        );
        assert_eq!(state.status, UiStatus::Idle);
        assert!(
            state.pending_scrollback[0]
                .to_string()
                .contains("indeterminate: inspect before retrying")
        );
    }

    #[test]
    fn live_region_carries_status_and_composer_cursor() {
        let mut state = UiState::new();
        state.composer = "hello world".to_owned();
        state.cursor = state.composer.chars().count();
        state.status = UiStatus::Working {
            operation: "running bash".to_owned(),
        };
        let (lines, cursor) = build_live(&state, &palette(Theme::Dark), 40);
        let text: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        assert!(text.iter().any(|l| l.contains("hello world")), "{text:?}");
        assert!(
            text.iter().any(|l| l.contains("● running bash")),
            "{text:?}"
        );
        // cursor sits at the end of the typed text on the composer row
        let (row, col) = cursor.expect("cursor");
        assert!(text[row].starts_with("> hello world"), "{}", text[row]);
        assert_eq!(col as usize, 2 + "hello world".width());
    }

    #[test]
    fn wrapping_keeps_zwj_graphemes_atomic() {
        let family = "👩‍💻";
        let rows = wrap_line(&Line::from(format!("{family}x")), 2);

        assert_eq!(rows.len(), 2, "grapheme must occupy one display row");
        assert_eq!(rows[0].to_string(), family);
        assert_eq!(rows[1].to_string(), "x");
    }

    #[test]
    fn terminal_capture_records_emitted_bytes_when_opted_in() {
        let capture = tempfile::NamedTempFile::new().expect("capture file");
        let mut output = ion_terminal::TerminalOutput::new(Vec::new(), Some(capture.path()))
            .expect("capture setup");

        output.write_all(b"frame bytes").expect("write");
        output.flush().expect("flush");

        assert_eq!(
            std::fs::read(capture.path()).expect("read capture"),
            b"frame bytes"
        );
    }
}

#[cfg(test)]
mod display_surface_tests {
    use super::tests::ctrl;
    use super::*;
    use ion_core::{OperationId, RuntimeCursor};

    fn settled(preview: Option<String>) -> UiMessage {
        UiMessage::Runtime(RuntimeEvent::ToolSettled {
            cursor: RuntimeCursor::default(),
            operation_id: OperationId::generate(),
            call_id: 1,
            is_error: false,
            preview,
        })
    }

    fn started(state: UiState) -> UiState {
        apply_runtime_event(
            state,
            RuntimeEvent::ToolStarted {
                cursor: RuntimeCursor::default(),
                operation_id: OperationId::generate(),
                call_id: 1,
                tool: "bash".to_owned(),
                target: Some("echo hi".to_owned()),
            },
        )
    }

    #[test]
    fn settlement_stores_the_preview_on_the_running_row() {
        let state = started(UiState::new());
        let state = update(state, settled(Some("hello\nworld".to_owned()))).0;
        let row = state.tool_rows.last().expect("row");
        assert_eq!(row.preview.as_deref(), Some("hello\nworld"));
    }

    #[test]
    fn ctrl_o_toggles_whether_flushed_rows_carry_previews() {
        let state = started(UiState::new());
        let state = update(state, settled(Some("hello\nworld".to_owned()))).0;

        // Collapsed (default): label only.
        let mut collapsed = state.clone();
        collapsed.flush_draft();
        assert!(
            collapsed
                .pending_scrollback
                .iter()
                .all(|line| !line.to_string().contains("world"))
        );

        // Expanded: preview lines follow the label.
        let expanded = update(state, ctrl('o')).0;
        let mut expanded_state = expanded;
        expanded_state.flush_draft();
        assert!(
            expanded_state
                .pending_scrollback
                .iter()
                .any(|line| line.to_string().contains("world"))
        );
    }

    #[test]
    fn thinking_flushes_before_text_and_respects_visibility() {
        let mut state = UiState::new();
        state.thinking_visible = true;
        state = apply_runtime_event(
            state,
            RuntimeEvent::ThinkingDelta {
                cursor: RuntimeCursor::default(),
                operation_id: OperationId::generate(),
                text: "deep thought".to_owned(),
            },
        );
        state = apply_runtime_event(
            state,
            RuntimeEvent::AssistantTextDelta {
                cursor: RuntimeCursor::default(),
                operation_id: OperationId::generate(),
                text: "answer".to_owned(),
            },
        );
        // Thinking flushed into scrollback at the first text delta.
        assert!(state.draft_thinking.is_empty());
        assert_eq!(state.draft, "answer");
        assert!(
            state
                .pending_scrollback
                .iter()
                .any(|line| line.to_string().contains("deep thought"))
        );

        // Hidden mode drops reasoning entirely.
        let mut hidden = UiState::new();
        hidden.thinking_visible = false;
        hidden = apply_runtime_event(
            hidden,
            RuntimeEvent::ThinkingDelta {
                cursor: RuntimeCursor::default(),
                operation_id: OperationId::generate(),
                text: "secret".to_owned(),
            },
        );
        hidden = apply_runtime_event(
            hidden,
            RuntimeEvent::AssistantTextDelta {
                cursor: RuntimeCursor::default(),
                operation_id: OperationId::generate(),
                text: "x".to_owned(),
            },
        );
        assert!(
            !hidden
                .pending_scrollback
                .iter()
                .any(|line| line.to_string().contains("secret"))
        );
    }
}
