//! Ratatui TUI frontend (DESIGN.md §21, §22).
//!
//! One runtime contract: this frontend consumes `SessionHandle`
//! semantics only — snapshot plus bounded live events — and never
//! touches the store. Ion owns application state: [`UiState`] is a
//! plain value, `update` is a pure reducer over [`UiMessage`]s, and
//! effects call back into the session. The terminal is restored by one
//! RAII owner, never scattered across widgets.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures_util::StreamExt;

use ratatui::crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event as TermEvent, EventStream, KeyCode,
    KeyEvent, KeyModifiers,
};
use ratatui::crossterm::{execute, terminal};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr as _;

use crate::screen::{Frame, Screen};
use crate::settings::Theme;
use ion_core::{
    CommandError, OperationStatus, RuntimeError, RuntimeEvent, SessionHandle, SessionSnapshot,
    SessionStore,
};

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
}

impl HostConfig {
    #[must_use]
    pub fn display_only(model_name: Option<String>, hide_thinking_block: bool) -> Self {
        Self {
            model_name,
            hide_thinking_block,
        }
    }
}

/// What the reducer wants the event loop to do. Effects are the only
/// path back into the runtime (§22.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiEffect {
    Submit { text: String },
    Steer { text: String },
    Compact { instructions: Option<String> },
    SwitchModel { model: String },
    Cancel,
    Quit,
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
    Submit,
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
    bindings: Vec<(Action, KeyCode, KeyModifiers)>,
}

impl Default for KeyMap {
    fn default() -> Self {
        let bind = |map: &mut Vec<(Action, KeyCode, KeyModifiers)>,
                    action: Action,
                    code: KeyCode,
                    modifiers: KeyModifiers| {
            map.push((action, code, modifiers));
        };
        let mut bindings = Vec::new();
        bind(
            &mut bindings,
            Action::Quit,
            KeyCode::Char('d'),
            KeyModifiers::CONTROL,
        );
        bind(
            &mut bindings,
            Action::Cancel,
            KeyCode::Esc,
            KeyModifiers::NONE,
        );
        bind(
            &mut bindings,
            Action::Cancel,
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        );
        bind(
            &mut bindings,
            Action::Submit,
            KeyCode::Enter,
            KeyModifiers::NONE,
        );
        bind(
            &mut bindings,
            Action::HistoryPrevious,
            KeyCode::Up,
            KeyModifiers::NONE,
        );
        bind(
            &mut bindings,
            Action::HistoryNext,
            KeyCode::Down,
            KeyModifiers::NONE,
        );
        bind(
            &mut bindings,
            Action::CursorLeft,
            KeyCode::Left,
            KeyModifiers::NONE,
        );
        bind(
            &mut bindings,
            Action::CursorRight,
            KeyCode::Right,
            KeyModifiers::NONE,
        );
        bind(
            &mut bindings,
            Action::CursorHome,
            KeyCode::Home,
            KeyModifiers::NONE,
        );
        bind(
            &mut bindings,
            Action::CursorHome,
            KeyCode::Char('a'),
            KeyModifiers::CONTROL,
        );
        bind(
            &mut bindings,
            Action::CursorEnd,
            KeyCode::End,
            KeyModifiers::NONE,
        );
        bind(
            &mut bindings,
            Action::CursorEnd,
            KeyCode::Char('e'),
            KeyModifiers::CONTROL,
        );
        bind(
            &mut bindings,
            Action::KillToEnd,
            KeyCode::Char('k'),
            KeyModifiers::CONTROL,
        );
        bind(
            &mut bindings,
            Action::KillToStart,
            KeyCode::Char('u'),
            KeyModifiers::CONTROL,
        );
        bind(
            &mut bindings,
            Action::KillWord,
            KeyCode::Char('w'),
            KeyModifiers::CONTROL,
        );
        bind(
            &mut bindings,
            Action::Yank,
            KeyCode::Char('y'),
            KeyModifiers::CONTROL,
        );
        bind(
            &mut bindings,
            Action::ToggleToolOutput,
            KeyCode::Char('o'),
            KeyModifiers::CONTROL,
        );
        bind(
            &mut bindings,
            Action::ToggleThinking,
            KeyCode::Char('t'),
            KeyModifiers::CONTROL,
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
fn parse_key(spec: &str) -> Result<(KeyCode, KeyModifiers), String> {
    let mut modifiers = KeyModifiers::NONE;
    let mut key = None;
    for part in spec.split('+') {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" => modifiers |= KeyModifiers::CONTROL,
            "alt" => modifiers |= KeyModifiers::ALT,
            "shift" => modifiers |= KeyModifiers::SHIFT,
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

/// One started tool effect: its display label plus the bounded output
/// preview from settlement (rendered only while expanded).
#[derive(Debug, Clone)]
struct ToolRow {
    label: String,
    preview: Option<String>,
}

/// One UI state owner (§22.1). Plain data; no handles, no hidden state.
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
    status: UiStatus,
    /// Model id for /model display (host-provided, not runtime state).
    model_name: Option<String>,
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

    /// Host-provided model id for the /model display.
    pub fn set_model_name(&mut self, model_name: Option<String>) {
        self.model_name = model_name;
    }

    /// Replace the key bindings with settings-resolved ones.
    pub fn set_keymap(&mut self, keymap: KeyMap) {
        self.keymap = keymap;
    }
}

/// Pure reducer (§22.1): `update(UiState, UiMessage) -> UiState` plus
/// at most one effect. Deterministic; no I/O.
#[must_use]
pub fn update(state: UiState, message: UiMessage) -> (UiState, Option<UiEffect>) {
    let mut state = state;
    match message {
        UiMessage::Key(key) => handle_key(state, key),
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
        UiMessage::SubmitRejected(message) | UiMessage::SteerRejected(message) => {
            state
                .pending_scrollback
                .push(Line::from(format!("! {message}")).red());
            (state, None)
        }
    }
}

fn handle_key(state: UiState, key: KeyEvent) -> (UiState, Option<UiEffect>) {
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
        KeyCode::Char(ch) if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT => {
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
        Action::Cancel => {
            if matches!(state.status, UiStatus::Idle) {
                state.quit_requested = true;
                (state, Some(UiEffect::Quit))
            } else {
                (state, Some(UiEffect::Cancel))
            }
        }
        Action::Quit => {
            state.quit_requested = true;
            (state, Some(UiEffect::Quit))
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
                UiStatus::Working { .. } => (state, Some(UiEffect::Steer { text })),
            }
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
            flush_thinking(&mut state);
            state.draft.push_str(&text);
        }
        RuntimeEvent::ThinkingDelta { text, .. } => {
            state.draft_thinking.push_str(&text);
        }
        RuntimeEvent::ToolStarted { tool, target, .. } => {
            flush_thinking(&mut state);
            state.tool_rows.push(ToolRow {
                label: match target {
                    Some(target) => format!("· {tool} {target}…"),
                    None => format!("· {tool}…"),
                },
                preview: None,
            });
            state.status = UiStatus::Working {
                operation: format!("running {tool}"),
            };
        }
        RuntimeEvent::ToolSettled {
            is_error, preview, ..
        } => {
            if let Some(row) = state.tool_rows.last_mut() {
                // The running row is the one this settlement answers.
                if is_error && !row.label.ends_with("✗") {
                    row.label.push_str(" ✗");
                }
                row.preview = preview;
            }
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
            self.pending_scrollback.push(Line::from(row.label).dim());
            if self.tool_output_expanded {
                for line in row.preview.iter().flat_map(|p| p.lines()) {
                    self.pending_scrollback
                        .push(Line::from(format!("  {line}")).dark_gray());
                }
            }
        }
        if !self.draft.is_empty() {
            for line in self.draft.lines() {
                let mut styled = markdown_line(line);
                styled.spans.insert(0, Span::from("ion « ").dim());
                self.pending_scrollback.push(styled);
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
        self.tool_rows.clear();
        match &snapshot.live {
            // The snapshot's draft is the runtime's authoritative
            // accumulation, so reconstruction is exact (§21.4).
            Some(live) => {
                self.draft = live.draft_text.clone();
                self.draft_thinking = live.draft_thinking.clone();
                for tool in &live.pending_tools {
                    let label = match &tool.target {
                        Some(target) => format!("· {} {target}…", tool.tool),
                        None => format!("· {}…", tool.tool),
                    };
                    self.tool_rows.push(ToolRow {
                        label,
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

/// RAII owner of terminal restoration (§22.4). One guard owns raw
/// mode, bracketed paste, and the inline viewport teardown.
pub struct TerminalGuard {
    restored: bool,
}

struct TerminalOutput<W> {
    output: W,
    capture: Option<File>,
}

impl<W: Write> TerminalOutput<W> {
    fn new(output: W, capture_path: Option<&Path>) -> io::Result<Self> {
        let capture = capture_path
            .map(|path| {
                File::create(path).map_err(|err| {
                    io::Error::new(
                        err.kind(),
                        format!("terminal capture {}: {err}", path.display()),
                    )
                })
            })
            .transpose()?;
        Ok(Self { output, capture })
    }

    fn from_environment(output: W) -> io::Result<Self> {
        let capture_path = std::env::var_os("ION_TERMINAL_CAPTURE").map(PathBuf::from);
        Self::new(output, capture_path.as_deref())
    }

    fn record_external(&mut self, bytes: &[u8]) -> io::Result<()> {
        if let Some(capture) = &mut self.capture {
            capture.write_all(bytes)?;
        }
        Ok(())
    }
}

impl<W: Write> Write for TerminalOutput<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let written = self.output.write(bytes)?;
        if written > 0
            && let Some(capture) = &mut self.capture
        {
            capture.write_all(&bytes[..written])?;
        }
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()?;
        if let Some(capture) = &mut self.capture {
            capture.flush()?;
        }
        Ok(())
    }
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
        let _ = execute!(io::stdout(), DisableBracketedPaste, crossterm::cursor::Show,);
        // SGR reset: a draw interrupted mid-style must not tint the
        // panic message or the shell prompt.
        let _ = io::stdout().write_all(b"\x1b[0m");
        let _ = io::stdout().flush();
        previous(info);
    }));
}

/// Colors for the live viewport, chosen once from the theme setting.
/// Scrollback styling stays theme-independent (dim/red/yellow read on
/// both light and dark terminals).
#[derive(Clone, Copy)]
pub struct Palette {
    pub status_idle: Style,
    pub status_working: Style,
    pub tool_row: Style,
    pub composer: Style,
}

/// `Auto` follows the terminal preference, which has no portable query
/// in crossterm; it currently resolves to the dark palette.
pub fn palette(theme: Theme) -> Palette {
    match theme {
        Theme::Dark | Theme::Auto => Palette {
            status_idle: Style::new().dim(),
            status_working: Style::new().cyan(),
            tool_row: Style::new().dim(),
            composer: Style::new(),
        },
        Theme::Light => Palette {
            status_idle: Style::new().dark_gray(),
            status_working: Style::new().blue(),
            tool_row: Style::new().dark_gray(),
            composer: Style::new(),
        },
    }
}

/// Wrap one styled line to `width` columns (display width, char
/// boundaries). Styles carry over to the continuation rows.
/// Convert any borrowed Line to an owned `'static` one.
/// Convert any borrowed Line to an owned `'static` one, folding the
/// line-level style into each span so wrapping cannot discard it.
fn clone_static(line: &Line<'_>) -> Line<'static> {
    Line::from(
        line.spans
            .iter()
            .map(|s| Span::styled(s.content.to_string(), line.style.patch(s.style)))
            .collect::<Vec<_>>(),
    )
}

fn wrap_line(line: &Line<'_>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let total: usize = line.spans.iter().map(|s| s.content.width()).sum();
    if total <= width {
        return vec![clone_static(line)];
    }
    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut cur: Vec<Span> = Vec::new();
    let mut cur_width = 0usize;
    for span in &line.spans {
        let style = span.style;
        let mut chunk = String::new();
        for grapheme in span.content.graphemes(true) {
            let cw = grapheme.width();
            if cur_width + cw > width && cur_width > 0 {
                if !chunk.is_empty() {
                    cur.push(Span::styled(std::mem::take(&mut chunk), style));
                }
                rows.push(Line::from(std::mem::take(&mut cur)));
                cur_width = 0;
            }
            chunk.push_str(grapheme);
            cur_width += cw;
        }
        if !chunk.is_empty() {
            cur.push(Span::styled(chunk, style));
        }
    }
    if !cur.is_empty() {
        rows.push(Line::from(cur));
    }
    if rows.is_empty() {
        rows.push(Line::from(String::new()));
    }
    rows
}

fn str_width(line: &Line<'_>) -> usize {
    line.spans.iter().map(|s| s.content.width()).sum()
}

/// Maximum live-band height: tool row(s), draft tail, status,
/// composer. The band is variable-height within this cap; growth
/// beyond the window scrolls physically (monotonic offset), shrink
/// blanks freed rows in place — reversible edits never duplicate
/// committed content into scrollback (§22.3).
const LIVE_REGION_MAX_ROWS: usize = 6;

/// The live band below the committed transcript. Returns pre-wrapped
/// rows (at most LIVE_REGION_MAX_ROWS) plus the hardware cursor
/// position relative to the band; the composer occupies the last rows.
fn build_live(
    state: &UiState,
    palette: &Palette,
    width: usize,
) -> (Vec<Line<'static>>, Option<(usize, u16)>) {
    // Composer first: it is anchored to the band's bottom and owns the
    // hardware cursor.
    let cursor_byte = char_offset_to_byte(&state.composer, state.cursor);
    let before = &state.composer[..cursor_byte];
    let after = &state.composer[cursor_byte..];
    let prompt = "\u{203a} ";
    let target_col = prompt.width() + before.width();
    let composer = Line::from(format!("{prompt}{before}{after}"));
    let composer_rows = wrap_line(&composer, width);
    let composer_len = composer_rows.len();

    let mut head: Vec<Line<'static>> = Vec::new();
    if let Some(latest) = state.tool_rows.last() {
        head.extend(wrap_line(
            &Line::from(latest.label.clone()).style(palette.tool_row),
            width,
        ));
        if state.tool_output_expanded {
            for line in latest.preview.iter().flat_map(|p| p.lines()) {
                head.extend(wrap_line(
                    &Line::from(format!("  {line}"))
                        .style(palette.tool_row)
                        .italic(),
                    width,
                ));
            }
        }
    }
    if !state.draft.is_empty() {
        head.extend(wrap_line(
            &Line::from(format!("ion \u{ab} {}", state.draft)),
            width,
        ));
    } else if let Some(last) = state
        .draft_thinking
        .lines()
        .last()
        .filter(|_| state.thinking_visible && !state.draft_thinking.is_empty())
    {
        head.extend(wrap_line(
            &Line::from(format!("\u{273b} {last}")).dim().italic(),
            width,
        ));
    }

    let status = match &state.status {
        UiStatus::Idle => {
            let mut text = String::from("idle \u{2014} type a prompt, esc quits");
            if let Some(model) = &state.model_name {
                text.push_str("  \u{b7}  ");
                text.push_str(model);
            }
            Line::from(text).style(palette.status_idle)
        }
        UiStatus::Working { operation } => {
            Line::from(format!("\u{25cf} {operation}")).style(palette.status_working)
        }
    };
    head.push(status);

    // Fit the head above the composer inside the band cap, keeping
    // the newest content when truncating.
    let budget = LIVE_REGION_MAX_ROWS.saturating_sub(composer_len);
    if head.len() > budget {
        head = head.split_off(head.len() - budget);
    }

    let mut lines: Vec<Line<'static>> = head;

    // Cursor position within the wrapped composer rows.
    let mut cursor = None;
    let mut walked = 0usize;
    for (i, row) in composer_rows.iter().enumerate() {
        let row_width = str_width(row);
        if target_col <= walked + row_width {
            cursor = Some((
                lines.len() + i,
                (target_col - walked).min(width.saturating_sub(1)) as u16,
            ));
            break;
        }
        walked += row_width;
    }
    lines.extend(composer_rows);

    (lines, cursor)
}

/// Committed transcript with its wrapped projection cached per width:
/// appending wraps only new lines; a resize rewraps once.
struct Transcript {
    raw: Vec<Line<'static>>,
    wrapped: Vec<Line<'static>>,
    width: u16,
}

impl Transcript {
    fn new(width: u16) -> Self {
        Self {
            raw: Vec::new(),
            wrapped: Vec::new(),
            width,
        }
    }

    fn push(&mut self, line: Line<'static>) {
        self.wrapped.extend(wrap_line(&line, self.width as usize));
        self.raw.push(line);
    }

    fn extend(&mut self, lines: impl IntoIterator<Item = Line<'static>>) {
        for line in lines {
            self.push(line);
        }
    }

    fn clear(&mut self) {
        self.raw.clear();
        self.wrapped.clear();
    }

    fn rewrap_if_needed(&mut self, width: u16) {
        if width == self.width {
            return;
        }
        self.width = width;
        self.wrapped.clear();
        for line in &self.raw {
            self.wrapped.extend(wrap_line(line, width as usize));
        }
    }
}

/// Enter the terminal before any session state is touched: the
/// close-on-error path suspends open operations, so a failed launch
/// must never get as far as opening or resuming a session.
pub fn setup_terminal() -> Result<TerminalGuard, RuntimeError> {
    let guard = TerminalGuard::enter()
        .map_err(|err| RuntimeError::OperationFailed(format!("terminal setup failed: {err}")))?;
    install_panic_hook();
    // Test-only hook: the PTY restoration test drives a real panic
    // through the guard's restore path.
    if std::env::var_os("ION_TEST_PANIC").is_some() {
        panic!("ION_TEST_PANIC");
    }
    Ok(guard)
}

/// The TUI event loop: runtime events and terminal keys into the
/// reducer; effects dispatch straight back into the session. Never
/// blocks rendering on provider/tool I/O (§22.2).
pub async fn run(
    session: SessionHandle,
    store: Arc<SessionStore>,
    resume_session: Option<ion_core::SessionId>,
    theme: Theme,
    keymap: KeyMap,
    host: HostConfig,
    mut guard: TerminalGuard,
) -> Result<(), RuntimeError> {
    let switching_available = host.model_name.is_some();

    let palette = palette(theme);

    let (term_w, term_h) = crossterm::terminal::size().unwrap_or((80, 24));
    let mut out = TerminalOutput::from_environment(io::stdout()).map_err(|err| {
        RuntimeError::OperationFailed(format!("terminal output setup failed: {err}"))
    })?;

    // The banner is committed straight to native scrollback above the
    // region (§22.3 inline semantics): completed content never lives in
    // the diffed window.
    let banner = if resume_session.is_some() {
        "— ion — resumed; enter sends; esc cancels; ctrl-d quits —"
    } else {
        "— ion — type a prompt; enter sends; esc cancels; ctrl-d quits —"
    };
    writeln!(out, "{banner}")
        .map_err(|err| RuntimeError::OperationFailed(format!("terminal output failed: {err}")))?;
    out.flush()
        .map_err(|err| RuntimeError::OperationFailed(format!("terminal flush failed: {err}")))?;

    // Anchor the region at the launch cursor. Queried before the
    // EventStream exists, so no competing stdin reader.
    out.record_external(b"\x1b[6n")
        .map_err(|err| RuntimeError::OperationFailed(format!("terminal capture failed: {err}")))?;
    let (_, cursor_row) = crossterm::cursor::position()
        .map_err(|err| RuntimeError::OperationFailed(format!("cursor query failed: {err}")))?;
    let mut origin = cursor_row;
    // Keep a minimal usable region above the screen bottom.
    const MIN_REGION_ROWS: u16 = 4;
    if term_h.saturating_sub(origin) < MIN_REGION_ROWS {
        let push = MIN_REGION_ROWS - (term_h - origin);
        write!(out, "{}", "\n".repeat(push as usize)).map_err(|err| {
            RuntimeError::OperationFailed(format!("terminal output failed: {err}"))
        })?;
        out.flush().map_err(|err| {
            RuntimeError::OperationFailed(format!("terminal flush failed: {err}"))
        })?;
        origin = origin.saturating_sub(push);
    }
    let mut screen = Screen::new(term_w, origin, term_h);

    // Committed transcript: restored entries, flushed turns. Committed
    // lines never change once appended (§22 line-diff model).
    let mut transcript = Transcript::new(term_w);

    // Resume: project the persisted transcript into the committed array.
    let mut durable_prefix = 0usize; // presentation lines atop transcript
    if let Some(session_id) = resume_session {
        let loaded = store
            .load(session_id)
            .await
            .map_err(|err| RuntimeError::OperationFailed(err.to_string()))?;
        durable_prefix += loaded.entries.len(); // entry count, not line count
        let mut restored: Vec<Line<'static>> = Vec::new();
        for (_, entry) in loaded.entries {
            push_entry_lines(&entry, &mut restored);
        }
        restored.push(Line::from(format!("— resumed session {session_id} —")).dim());
        transcript.extend(restored);
    }

    // The EventStream is the sole terminal reader, so crossterm parses
    // cursor-position responses itself; blocking cursor queries (used
    // by Terminal::clear) cannot deadlock against key reads.
    let mut key_stream = EventStream::new();

    // One live UiState for the whole loop; host-provided display
    // config seeds it here and nowhere else.
    let mut state = UiState::new();
    state.set_keymap(keymap);
    state.set_model_name(host.model_name.clone());
    state.thinking_visible = !host.hide_thinking_block;
    state.model_switching_available = switching_available;
    let (snapshot, mut events) = session.subscribe().await?;
    // The session's durable selection is authoritative once subscribed;
    // a resumed session may have switched models in an earlier run.
    // Scripted launches keep the host's display fallback.
    if host.model_name.is_some() {
        state.set_model_name(Some(snapshot.model_ref.clone()));
    }
    // §21.4/§31.14: the initial snapshot is authoritative for durable
    // history. Entries settled between the resume load and this
    // subscribe are appended; a fresh session's snapshot is empty.
    if snapshot.entries.len() > durable_prefix {
        transcript.extend(entry_lines(&snapshot.entries[durable_prefix..]));
    }
    let mut active_operation: Option<ion_core::OperationId> = match snapshot.operation {
        OperationStatus::Active { operation_id, .. } => Some(operation_id),
        OperationStatus::Idle => None,
    };
    let mut result: Result<(), RuntimeError> = Ok(());
    // Crossterm's EventStream can terminate on transient reads (notably
    // SIGWINCH during resize). Recreate it rather than treating the
    // stream end as fatal; give up only after repeated immediate ends.
    let mut stream_recreations = 0u32;
    const MAX_STREAM_RECREATIONS: u32 = 64;

    loop {
        // Size changes are polled directly: resize events ride the same
        // fragile stream as keys.
        if let Ok((w, h)) = crossterm::terminal::size() {
            screen.resize(w, h);
        }
        transcript.rewrap_if_needed(screen.size().0);
        // Flush completed turns into the committed transcript, then
        // draw committed history + live band as one line-diff frame
        // (§22).
        if !state.pending_scrollback.is_empty() {
            let flushed = std::mem::take(&mut state.pending_scrollback);
            transcript.extend(flushed);
        }
        let (live, live_cursor) = build_live(&state, &palette, screen.size().0 as usize);
        let cursor = live_cursor.map(|(row, col)| (transcript.wrapped.len() + row, col));
        screen
            .draw(
                &mut out,
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
                    Some(Ok(TermEvent::Key(key))) => {
                        stream_recreations = 0;
                        let (next, effect) = update(state, UiMessage::Key(key));
                        state = next;
                        if let Some(effect) = effect {
                            dispatch(&session, &mut state, active_operation, effect).await;
                        }
                    }
                    Some(Ok(TermEvent::Paste(text))) => {
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
                        key_stream = EventStream::new();
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
                                // authoritative for committed history;
                                // presentation prefix is preserved.
                                let prefix: Vec<Line<'static>> =
                                    transcript.raw[..durable_prefix].to_vec();
                                transcript.clear();
                                transcript.extend(prefix);
                                transcript.extend(entry_lines(&snapshot.entries));
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
        }
    }

    screen.finish(&mut out).ok();
    guard.restore();
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
        UiEffect::Cancel => {
            if let Some(operation_id) = active_operation {
                let _ = session.cancel(operation_id).await;
            }
        }
    }
}

fn entry_lines(entries: &[ion_core::SessionEntry]) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for entry in entries {
        push_entry_lines(entry, &mut out);
    }
    out
}

fn push_entry_lines(entry: &ion_core::SessionEntry, out: &mut Vec<Line<'static>>) {
    let line = match entry {
        ion_core::SessionEntry::UserMessage { text } => Some(format!("you » {text}")),
        ion_core::SessionEntry::ModelChanged { model_ref } => {
            Some(format!("· model → {model_ref}"))
        }
        ion_core::SessionEntry::AssistantMessage { text } => Some(format!("ion « {text}")),
        ion_core::SessionEntry::ToolCall { call } => {
            let target = ion_core::target_from_arguments(&call.name, &call.arguments)
                .unwrap_or_else(|| format!("(call {})", call.call_id));
            Some(format!("· {} {target}…", call.name))
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
pub(crate) mod tests {
    use super::*;
    use ion_core::{OperationId, RuntimeCursor};
    use ratatui::style::{Color, Modifier};

    pub(crate) fn key(code: KeyCode) -> UiMessage {
        UiMessage::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    pub(crate) fn ctrl(ch: char) -> UiMessage {
        UiMessage::Key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL))
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
            (KeyCode::Char('k'), KeyModifiers::CONTROL)
        );
        assert_eq!(
            parse_key("alt+left").unwrap(),
            (KeyCode::Left, KeyModifiers::ALT)
        );
        assert_eq!(
            parse_key("enter").unwrap(),
            (KeyCode::Enter, KeyModifiers::NONE)
        );
        assert!(parse_key("ctrl+nope").is_err());
    }

    #[test]
    fn keymap_override_rebinds_action() {
        let overrides: crate::settings::Keybindings = toml::from_str("quit = \"ctrl+q\"").unwrap();
        let map = KeyMap::from_settings(&overrides).unwrap();
        let ctrl_q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL);
        assert_eq!(map.action_for(&ctrl_q), Some(Action::Quit));
        // The old binding is gone.
        let ctrl_d = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert_eq!(map.action_for(&ctrl_d), None);
    }

    #[test]
    fn default_keymap_matches_pi_overlap() {
        let map = KeyMap::default();
        let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(map.action_for(&up), Some(Action::HistoryPrevious));
        let ctrl_y = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL);
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
        assert_eq!(state.tool_rows[0].label, "· read Cargo.toml…");
    }

    #[test]
    fn resync_after_lag_on_idle_clears_partial_draft() {
        let mut state = UiState::new();
        state.draft = "partial".to_owned();
        let snapshot = SessionSnapshot {
            cursor: RuntimeCursor::default(),
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
            state.tool_rows.last().map(|row| row.label.as_str()),
            Some("· read Cargo.toml…")
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
        assert!(text[row].starts_with("› hello world"), "{}", text[row]);
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
        let mut output =
            TerminalOutput::new(Vec::new(), Some(capture.path())).expect("capture setup");

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
