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
    CommandError, OperationOutcome, OperationSettlement, OperationState, OperationStatus,
    RuntimeError, RuntimeEvent, SessionHandle, SessionSnapshot, TokenUsage,
};
use ion_terminal::{
    Frame, InputEvent, KeyCode, KeyEvent, Modifiers, Screen, TerminalSession, install_panic_hook,
};

mod help;
mod render;
pub use render::{Palette, palette};
use render::{Transcript, append_snapshot_entries};
#[cfg(test)]
use render::{build_live, entry_lines, wrap_line};

/// Host-provided configuration for one launch. Cloneable handles;
/// never runtime state.
#[derive(Clone)]
pub struct HostConfig {
    /// Model id for the /model display; also marks switching as
    /// possible (a real model is configured; scripted launches have
    /// nothing to switch to).
    pub model_name: Option<String>,
    /// Provider label shown beside the model in the footer.
    pub model_provider: Option<String>,
    /// Host-provided finite model list shown by `/model` and selectable by
    /// number. Provider APIs do not need to enumerate models.
    pub model_catalog: Vec<String>,
    /// Seed for ctrl+t (pi-parity hideThinkingBlock).
    pub hide_thinking_block: bool,
    /// One-time store/startup notice (e.g. archived old-schema
    /// database) rendered once into the transcript.
    pub startup_notice: Option<String>,
    /// Home-relative launch working directory (status line).
    pub cwd_label: Option<String>,
    /// Bounded recursive workspace file list for the `@` picker and path
    /// completion (pi parity: fd-backed file search). Walked once at
    /// launch; the reducer never touches the filesystem.
    pub workspace_files: Vec<String>,
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
    /// Set the thinking level for future steps (pi parity: /thinking,
    /// shift+tab). `None` restores the adapter default.
    SwitchThinking {
        thinking: Option<String>,
    },
    /// Run one user shell passthrough (pi parity: `!command` visible to
    /// the model, `!!command` durable but excluded).
    RunShell {
        command: String,
        exclude_from_context: bool,
    },
    Cancel,
    /// Approve the parked tool approval (§17.4).
    Approve,
    /// Deny the parked tool approval (§17.4).
    Deny,
    /// Suspend the terminal, edit the composer draft in
    /// $VISUAL/$EDITOR, and continue with the edited text (pi parity:
    /// app.editor.external).
    ExternalEditor,
    /// Restore the queued next-run prompt to the composer (pi parity:
    /// app.message.dequeue, alt+up).
    DequeueNextRun,
    /// Close the attached session and start a new durable one.
    NewSession,
    /// Close the attached session and reopen the requested durable one.
    ResumeSession {
        session: ion_core::SessionId,
    },
    /// Clone the attached session's history into a new durable session
    /// and attach to it.
    CloneSession,
    /// Rename the attached session (durable, presentation-only).
    RenameSession {
        title: String,
    },
    /// Load picker rows from the store (the reducer cannot read the
    /// store; the host resolves this into `SessionListed`).
    RequestSessionList,
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
    /// A model switch changes runtime configuration, not the user's draft.
    ModelSwitchAccepted,
    SubmitRejected(String),
    EnqueueAccepted,
    EnqueueRejected(String),
    CompactAccepted,
    SteerAccepted,
    SteerRejected(String),
    /// The host delivered picker rows.
    SessionListed(Vec<ion_core::SessionSummary>),
    /// A session switch completed; the loop re-attaches.
    SessionSwitched {
        session: ion_core::SessionId,
        title: String,
    },
    /// A session command failed; the draft is restored for retry.
    SessionCommandFailed(String),
    RenameAccepted,
    /// The external editor returned the edited draft (empty output
    /// keeps the current composer unchanged).
    ExternalEdited(String),
    /// The queued prompt was removed and returned to the composer
    /// (alt+up). `None` means the queue was already empty.
    Dequeued(Option<String>),
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
    InsertNewline,
    Complete,
    SteerCurrent,
    ToggleToolOutput,
    ToggleThinking,
    OpenModelSelector,
    CycleModelForward,
    CycleModelBackward,
    /// Cycle the thinking level (pi: app.thinking.cycle, shift+tab).
    CycleThinking,
    HistoryPrevious,
    HistoryNext,
    CursorLeft,
    CursorRight,
    /// Word-wise motion (pi: tui.editor.cursorWordLeft/Right).
    CursorWordLeft,
    CursorWordRight,
    CursorHome,
    CursorEnd,
    KillToEnd,
    KillToStart,
    KillWord,
    /// Delete the word after the cursor into the kill ring (pi:
    /// tui.editor.deleteWordForward, alt+d).
    KillWordForward,
    Yank,
    /// Cycle the kill ring after a yank (pi: tui.editor.yankPop, alt+y).
    YankPop,
    Undo,
    /// Queue a follow-up after the active operation completes (pi:
    /// app.message.followUp, alt+enter). Distinct from steering, which
    /// joins at the next reasoning boundary.
    QueueFollowUp,
    /// Restore the queued prompt to the editor (pi:
    /// app.message.dequeue, alt+up).
    DequeueFollowUp,
    /// Open the composer in $VISUAL/$EDITOR (pi: app.editor.external,
    /// ctrl+g).
    ExternalEditor,
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
            Action::InsertNewline,
            KeyCode::Char('j'),
            Modifiers::CONTROL,
        );
        bind(
            &mut bindings,
            Action::Complete,
            KeyCode::Tab,
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
        // Word-wise motion (pi defaults: ctrl/alt+left, ctrl/alt+right,
        // alt+b, alt+f).
        for (action, code, modifiers) in [
            (Action::CursorWordLeft, KeyCode::Left, Modifiers::ALT),
            (Action::CursorWordLeft, KeyCode::Left, Modifiers::CONTROL),
            (Action::CursorWordLeft, KeyCode::Char('b'), Modifiers::ALT),
            (Action::CursorWordRight, KeyCode::Right, Modifiers::ALT),
            (Action::CursorWordRight, KeyCode::Right, Modifiers::CONTROL),
            (Action::CursorWordRight, KeyCode::Char('f'), Modifiers::ALT),
            (Action::KillWordForward, KeyCode::Char('d'), Modifiers::ALT),
            (Action::YankPop, KeyCode::Char('y'), Modifiers::ALT),
            (
                Action::ExternalEditor,
                KeyCode::Char('g'),
                Modifiers::CONTROL,
            ),
            (Action::QueueFollowUp, KeyCode::Enter, Modifiers::ALT),
            (Action::DequeueFollowUp, KeyCode::Up, Modifiers::ALT),
            (Action::CycleThinking, KeyCode::Tab, Modifiers::SHIFT),
        ] {
            bind(&mut bindings, action, code, modifiers);
        }
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
            Action::Undo,
            KeyCode::Char('_'),
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
        bind(
            &mut bindings,
            Action::OpenModelSelector,
            KeyCode::Char('l'),
            Modifiers::CONTROL,
        );
        bind(
            &mut bindings,
            Action::CycleModelForward,
            KeyCode::Char('p'),
            Modifiers::CONTROL,
        );
        bind(
            &mut bindings,
            Action::CycleModelBackward,
            KeyCode::Char('p'),
            Modifiers::CONTROL | Modifiers::SHIFT,
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
        rebind(&mut map, Action::InsertNewline, &overrides.insert_newline)?;
        rebind(&mut map, Action::Complete, &overrides.complete)?;
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
        rebind(
            &mut map,
            Action::KillWordForward,
            &overrides.kill_word_forward,
        )?;
        rebind(&mut map, Action::Yank, &overrides.yank)?;
        rebind(&mut map, Action::YankPop, &overrides.yank_pop)?;
        rebind(
            &mut map,
            Action::CursorWordLeft,
            &overrides.cursor_word_left,
        )?;
        rebind(
            &mut map,
            Action::CursorWordRight,
            &overrides.cursor_word_right,
        )?;
        rebind(&mut map, Action::ExternalEditor, &overrides.external_editor)?;
        rebind(&mut map, Action::Undo, &overrides.undo)?;
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

    fn label(&self, action: Action) -> String {
        let labels: Vec<String> = self
            .bindings
            .iter()
            .filter(|(bound, _, _)| *bound == action)
            .map(|(_, code, modifiers)| format_key(*code, *modifiers))
            .collect();
        if labels.is_empty() {
            "unbound".to_owned()
        } else {
            labels.join("/")
        }
    }
}

fn format_key(code: KeyCode, modifiers: Modifiers) -> String {
    let mut label = String::new();
    if modifiers.contains(Modifiers::CONTROL) {
        label.push_str("ctrl+");
    }
    if modifiers.contains(Modifiers::ALT) {
        label.push_str("alt+");
    }
    if modifiers.contains(Modifiers::SHIFT) {
        label.push_str("shift+");
    }
    label.push_str(match code {
        KeyCode::Char(ch) => return format!("{label}{ch}"),
        KeyCode::Enter => "enter",
        KeyCode::Esc => "esc",
        KeyCode::Tab => "tab",
        KeyCode::Backspace => "backspace",
        KeyCode::Delete => "delete",
        KeyCode::Up => "up",
        KeyCode::Down => "down",
        KeyCode::Left => "left",
        KeyCode::Right => "right",
        KeyCode::Home => "home",
        KeyCode::End => "end",
        KeyCode::BackTab => "backtab",
        KeyCode::Insert => "insert",
        KeyCode::F(number) => return format!("{label}f{number}"),
        KeyCode::Other => "key",
    });
    label
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditKind {
    Insert,
    Delete,
    Paste,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EditSnapshot {
    composer: String,
    cursor: usize,
}

const MAX_UNDO_ENTRIES: usize = 128;

/// One started tool effect: its display label plus the bounded output
/// preview from settlement (rendered only while expanded).
#[derive(Debug, Clone)]
struct ToolRow {
    tool: String,
    target: Option<String>,
    state: ToolState,
    /// Latest bounded live output while the call is running.
    progress: Option<String>,
    preview: Option<String>,
}

/// Ephemeral model-picker state. The runtime remains authoritative for the
/// selected model; this only owns the filter, highlight, and editor draft
/// saved while the picker is open.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ModelSelector {
    selected: usize,
    saved_composer: String,
    saved_cursor: usize,
}

/// One session-picker row as presentation data. `summary` carries the
/// durable identity; `label` is the rendered picker line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionRow {
    id: ion_core::SessionId,
    label: String,
    title: String,
    updated_at: u64,
}

/// Ephemeral searchable session picker, when open (mirrors ModelSelector:
/// the composer is the filter query and the saved draft is restored on
/// close).
#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionSelector {
    rows: Vec<SessionRow>,
    selected: usize,
    saved_composer: String,
    saved_cursor: usize,
}

/// Ephemeral `@` file picker (pi parity). The composer is the filter
/// query over host-provided workspace rows; `at_offset` remembers where
/// the `@` token began so acceptance splices `@path ` into the saved
/// draft exactly there, replacing the typed token.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileSelector {
    selected: usize,
    saved_composer: String,
    saved_cursor: usize,
    /// Char offset of the `@` that opened the picker within the saved
    /// draft.
    at_offset: usize,
}

/// One UI state owner (TERMINAL.md). Plain data; no handles, no hidden state.
#[derive(Debug, Clone, Default)]
pub struct UiState {
    /// Composer buffer.
    composer: String,
    /// Cursor position as a char offset into `composer`.
    cursor: usize,
    /// Preferred display column within the composer while moving vertically
    /// through multiline or wrapped drafts; cleared by horizontal movement
    /// or edits.
    preferred_column: Option<usize>,
    /// Terminal width used to map wrapped composer rows for vertical motion.
    /// The render loop refreshes this before drawing and handling input.
    terminal_width: Option<usize>,
    /// Submitted prompts, oldest first; up/down navigates.
    history: Vec<String>,
    /// Position in `history` while browsing; None edits the live
    /// draft.
    history_index: Option<usize>,
    /// The live draft set aside when history browsing starts.
    history_stash: Option<String>,
    /// History entry awaiting runtime admission. Rejected commands remove it
    /// so retrying a preserved draft does not duplicate history.
    pending_history: Option<String>,
    /// Last kill (ctrl-k/u/w); ctrl-y yanks it back.
    kill_buffer: String,
    /// Kill ring for yank-pop (alt+y): older kills, newest last. The
    /// most recent entry lives in `kill_buffer`; consecutive kills
    /// push the previous one here.
    kill_ring: Vec<String>,
    /// Which ring entry (index into [kill_buffer] + kill_ring) the
    /// last yank inserted; `None` until the first yank.
    yank_index: Option<usize>,
    /// Char span of the text inserted by the most recent yank, so
    /// yank-pop replaces exactly that span.
    yank_span: Option<(usize, usize)>,
    /// Bounded local composer history. Adjacent typing/deletion is one edit;
    /// each bracketed paste is one edit.
    undo_stack: Vec<EditSnapshot>,
    last_edit: Option<EditKind>,
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
    /// Host-provided finite model list for the slash-command selector.
    model_catalog: Vec<String>,
    /// Ephemeral searchable model selector, when open.
    model_selector: Option<ModelSelector>,
    /// The durable thinking-level selection for future steps (pi
    /// parity: /thinking, shift+tab). `None` is the adapter default.
    /// Presentation only; the lane config is authoritative.
    thinking_level: Option<String>,
    /// Ephemeral thinking picker, when open.
    thinking_selector: Option<ModelSelector>,
    /// Provisional live output of a running user shell passthrough.
    /// Cleared when the durable settlement entry arrives.
    shell_output: String,
    /// Ephemeral session picker (`/resume`), when open. Rows are
    /// host-supplied snapshots; selection returns a durable id.
    session_selector: Option<SessionSelector>,
    /// Host-provided bounded workspace file list for `@` references and
    /// path completion (pi parity: fd-backed fuzzy file search). The
    /// reducer does no filesystem I/O; the host walks once at launch.
    workspace_files: Vec<String>,
    /// Ephemeral `@` file picker, when open (pi parity: typing `@`
    /// fuzzy-searches project files; selection inserts an `@path`
    /// reference into the composer — the model reads the file itself).
    file_selector: Option<FileSelector>,
    /// Durable identity of the attached session, for `/session` display
    /// and clone/delete guards. Host-owned; the TUI never derives it.
    session_id: Option<ion_core::SessionId>,
    /// Durable session title for `/session` display.
    session_title: Option<String>,
    /// Query stashed between `/resume <q>` and the picker opening with the
    /// host-delivered rows.
    pending_resume_query: Option<String>,
    /// Whether settled tool rows render their output preview
    /// (ctrl+o, pi-parity app.tools.expand).
    tool_output_expanded: bool,
    /// Local discoverability overlay, opened by `?` from an empty idle
    /// composer. It is presentation-only and never reaches the runtime.
    hotkeys_visible: bool,
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
    /// Provider label for the footer (host-provided composition state).
    model_provider: Option<String>,
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
    /// Most recent provider usage, retained for the footer after settlement.
    usage: Option<TokenUsage>,
    /// The durable queued follow-up prompt, when one exists (pi parity:
    /// queued messages stay visible above the composer).
    queued_prompt: Option<String>,
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

    /// Keep cursor movement aligned with the current render width.
    pub(crate) fn set_terminal_width(&mut self, width: usize) {
        self.terminal_width = Some(width.max(1));
    }

    fn composer_width(&self) -> usize {
        self.terminal_width.unwrap_or(80).max(1)
    }

    /// Replace the key bindings with settings-resolved ones.
    pub fn set_keymap(&mut self, keymap: KeyMap) {
        self.keymap = keymap;
    }

    fn reset_composer(&mut self) {
        self.composer.clear();
        self.cursor = 0;
        self.preferred_column = None;
        self.undo_stack.clear();
        self.last_edit = None;
        self.history_index = None;
        self.history_stash = None;
        self.pending_history = None;
    }

    fn open_model_selector(&mut self, query: &str) {
        if self.model_selector.is_some() {
            return;
        }
        let saved_composer = std::mem::take(&mut self.composer);
        let saved_cursor = self.cursor;
        self.composer = query.to_owned();
        self.cursor = self.composer.chars().count();
        self.preferred_column = None;
        self.undo_stack.clear();
        self.last_edit = None;
        let selected = self
            .filtered_model_catalog()
            .iter()
            .position(|model| model.eq_ignore_ascii_case(query))
            .unwrap_or(0);
        self.model_selector = Some(ModelSelector {
            selected,
            saved_composer,
            saved_cursor,
        });
    }

    fn open_thinking_selector(&mut self, query: &str) {
        if self.thinking_selector.is_some() {
            return;
        }
        let saved_composer = std::mem::take(&mut self.composer);
        let saved_cursor = self.cursor;
        self.composer = query.to_owned();
        self.cursor = self.composer.chars().count();
        self.preferred_column = None;
        self.undo_stack.clear();
        self.last_edit = None;
        let selected = THINKING_LEVELS
            .iter()
            .position(|level| *level == self.thinking_level.as_deref().unwrap_or(""))
            .unwrap_or(0);
        self.thinking_selector = Some(ModelSelector {
            selected,
            saved_composer,
            saved_cursor,
        });
    }

    fn close_thinking_selector(&mut self) {
        let Some(selector) = self.thinking_selector.take() else {
            return;
        };
        self.composer = selector.saved_composer;
        self.cursor = selector.saved_cursor.min(self.composer.chars().count());
        self.preferred_column = None;
        self.undo_stack.clear();
        self.last_edit = None;
    }

    fn filtered_thinking_levels(&self) -> Vec<String> {
        let query = self.composer.to_lowercase();
        THINKING_LEVELS
            .iter()
            .filter(|level| level.starts_with(&query))
            .map(|level| (*level).to_owned())
            .collect()
    }

    fn selected_thinking_level(&self) -> Option<String> {
        let selector = self.thinking_selector.as_ref()?;
        self.filtered_thinking_levels()
            .get(selector.selected)
            .cloned()
    }

    fn move_thinking_selection(&mut self, delta: isize) {
        let count = self.filtered_thinking_levels().len();
        let Some(selector) = self.thinking_selector.as_mut() else {
            return;
        };
        if count == 0 {
            selector.selected = 0;
            return;
        }
        selector.selected =
            (selector.selected as isize + delta).rem_euclid(count as isize) as usize;
    }

    fn reset_thinking_selection(&mut self) {
        let selected = self
            .filtered_thinking_levels()
            .iter()
            .position(|level| level == &self.composer.to_lowercase())
            .unwrap_or(0);
        if let Some(selector) = self.thinking_selector.as_mut() {
            selector.selected = selected;
        }
    }

    fn close_model_selector(&mut self) {
        let Some(selector) = self.model_selector.take() else {
            return;
        };
        self.composer = selector.saved_composer;
        self.cursor = selector.saved_cursor.min(self.composer.chars().count());
        self.preferred_column = None;
        self.undo_stack.clear();
        self.last_edit = None;
    }

    fn filtered_model_catalog(&self) -> Vec<String> {
        let query = self.composer.to_lowercase();
        self.model_catalog
            .iter()
            .filter(|model| fuzzy_contains(&model.to_lowercase(), &query))
            .cloned()
            .collect()
    }

    fn selected_model(&self) -> Option<String> {
        let selector = self.model_selector.as_ref()?;
        self.filtered_model_catalog()
            .get(selector.selected)
            .cloned()
    }

    fn move_model_selection(&mut self, delta: isize) {
        let count = self.filtered_model_catalog().len();
        let Some(selector) = self.model_selector.as_mut() else {
            return;
        };
        if count == 0 {
            selector.selected = 0;
            return;
        }
        selector.selected =
            (selector.selected as isize + delta).rem_euclid(count as isize) as usize;
    }

    fn reset_model_selection(&mut self) {
        let selected = self
            .filtered_model_catalog()
            .iter()
            .position(|model| model.eq_ignore_ascii_case(&self.composer))
            .unwrap_or(0);
        if let Some(selector) = self.model_selector.as_mut() {
            selector.selected = selected;
        }
    }

    fn open_file_selector(&mut self, at_offset: usize) {
        if self.file_selector.is_some() {
            return;
        }
        let saved_composer = std::mem::take(&mut self.composer);
        let saved_cursor = self.cursor;
        self.composer.clear();
        self.cursor = 0;
        self.preferred_column = None;
        self.undo_stack.clear();
        self.last_edit = None;
        self.file_selector = Some(FileSelector {
            selected: 0,
            saved_composer,
            saved_cursor,
            at_offset,
        });
    }

    fn close_file_selector(&mut self) {
        let Some(selector) = self.file_selector.take() else {
            return;
        };
        self.composer = selector.saved_composer;
        self.cursor = selector.saved_cursor.min(self.composer.chars().count());
        self.preferred_column = None;
        self.undo_stack.clear();
        self.last_edit = None;
    }

    fn filtered_file_rows(&self) -> Vec<String> {
        let query = self.composer.to_lowercase();
        self.workspace_files
            .iter()
            .filter(|path| fuzzy_contains(&path.to_lowercase(), &query))
            .take(MAX_FILE_SELECTOR_ROWS)
            .cloned()
            .collect()
    }

    fn selected_file_row(&self) -> Option<String> {
        let selector = self.file_selector.as_ref()?;
        self.filtered_file_rows().get(selector.selected).cloned()
    }

    fn move_file_selection(&mut self, delta: isize) {
        let count = self.filtered_file_rows().len();
        let Some(selector) = self.file_selector.as_mut() else {
            return;
        };
        if count == 0 {
            selector.selected = 0;
            return;
        }
        selector.selected =
            (selector.selected as isize + delta).rem_euclid(count as isize) as usize;
    }

    fn reset_file_selection(&mut self) {
        let selected = self
            .filtered_file_rows()
            .iter()
            .position(|path| path.eq_ignore_ascii_case(&self.composer))
            .unwrap_or(0);
        if let Some(selector) = self.file_selector.as_mut() {
            selector.selected = selected;
        }
    }

    /// Splice the selected reference into the saved draft where the `@`
    /// token began, replacing the `@` itself (pi parity: files insert
    /// `@path ` with the cursor after the space; directories insert
    /// `@dir/` and keep the cursor right after it so typing continues
    /// scoped).
    fn accept_file_row(&mut self, path: String) {
        let Some(selector) = self.file_selector.take() else {
            return;
        };
        let saved = selector.saved_composer;
        let char_at = |offset: usize| saved.char_indices().map(|(i, _)| i).nth(offset);
        let insert_at = char_at(selector.at_offset).unwrap_or(saved.len());
        let mut before: String = saved[..insert_at].to_owned();
        let after: String = saved[insert_at..].to_owned();
        let is_dir = path.ends_with('/');
        let reference = format!("@{path}");
        before.push_str(&reference);
        if is_dir {
            // Keep the cursor inside the token for continued scoping.
            self.composer = format!("{before}{after}");
            self.cursor = selector.at_offset + reference.chars().count();
        } else {
            before.push(' ');
            self.composer = format!("{before}{after}");
            self.cursor = selector.at_offset + reference.chars().count() + 1;
        }
        self.preferred_column = None;
        self.undo_stack.clear();
        self.last_edit = None;
    }

    fn open_session_selector(&mut self, rows: Vec<SessionRow>, query: &str) {
        if self.session_selector.is_some() {
            return;
        }
        let saved_composer = std::mem::take(&mut self.composer);
        let saved_cursor = self.cursor;
        self.composer = query.to_owned();
        self.cursor = self.composer.chars().count();
        self.preferred_column = None;
        self.undo_stack.clear();
        self.last_edit = None;
        let selected = self
            .filtered_session_rows()
            .iter()
            .position(|row| row.title.eq_ignore_ascii_case(query))
            .unwrap_or(0);
        self.session_selector = Some(SessionSelector {
            rows,
            selected,
            saved_composer,
            saved_cursor,
        });
    }

    fn close_session_selector(&mut self) {
        let Some(selector) = self.session_selector.take() else {
            return;
        };
        self.composer = selector.saved_composer;
        self.cursor = selector.saved_cursor.min(self.composer.chars().count());
        self.preferred_column = None;
        self.undo_stack.clear();
        self.last_edit = None;
    }

    fn filtered_session_rows(&self) -> Vec<SessionRow> {
        self.session_selector
            .as_ref()
            .map(|selector| {
                let query = self.composer.to_lowercase();
                selector
                    .rows
                    .iter()
                    .filter(|row| fuzzy_contains(&row.label.to_lowercase(), &query))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn selected_session(&self) -> Option<ion_core::SessionId> {
        let selector = self.session_selector.as_ref()?;
        self.filtered_session_rows()
            .get(selector.selected)
            .map(|row| row.id)
    }

    fn move_session_selection(&mut self, delta: isize) {
        let count = self.filtered_session_rows().len();
        let Some(selector) = self.session_selector.as_mut() else {
            return;
        };
        if count == 0 {
            selector.selected = 0;
            return;
        }
        selector.selected =
            (selector.selected as isize + delta).rem_euclid(count as isize) as usize;
    }

    fn reset_session_selection(&mut self) {
        let selected = self
            .filtered_session_rows()
            .iter()
            .position(|row| row.title.eq_ignore_ascii_case(&self.composer))
            .unwrap_or(0);
        if let Some(selector) = self.session_selector.as_mut() {
            selector.selected = selected;
        }
    }

    fn current_model_reference(&self) -> Option<String> {
        Some(format!(
            "{}/{}",
            self.model_provider.as_deref()?,
            self.model_name.as_deref()?
        ))
    }

    /// Presentation-only reset when the run loop attaches to a different
    /// durable session: drafts, live operation state, and per-session
    /// browsing belong to the old session and must not leak into the
    /// new transcript. The composer draft survives only across model
    /// switches; a session switch abandons it (the user asked for a
    /// different conversation).
    fn reset_for_session_switch(&mut self) {
        self.draft.clear();
        self.draft_thinking.clear();
        self.draft_degraded = false;
        self.tool_rows.clear();
        self.status = UiStatus::Idle;
        self.approval = None;
        self.history_index = None;
        self.history_stash = None;
        self.pending_history = None;
        self.hotkeys_visible = false;
        self.model_selector = None;
        self.session_selector = None;
        self.thinking_selector = None;
        self.file_selector = None;
        self.pending_resume_query = None;
        self.reset_composer();
    }

    fn reject_pending_history(&mut self) {
        if let Some(text) = self.pending_history.take()
            && self.history.last() == Some(&text)
        {
            self.history.pop();
        }
    }

    fn break_edit_group(&mut self) {
        self.last_edit = None;
    }

    fn record_edit(&mut self, kind: EditKind) {
        let coalesces = self.last_edit == Some(kind) && kind != EditKind::Paste;
        if !coalesces {
            if self.undo_stack.len() == MAX_UNDO_ENTRIES {
                self.undo_stack.remove(0);
            }
            self.undo_stack.push(EditSnapshot {
                composer: self.composer.clone(),
                cursor: self.cursor,
            });
        }
        self.last_edit = Some(kind);
    }

    fn undo_edit(&mut self) {
        if let Some(snapshot) = self.undo_stack.pop() {
            self.composer = snapshot.composer;
            self.cursor = snapshot.cursor.min(self.composer.chars().count());
            self.preferred_column = None;
            self.exit_history_browse();
        }
        self.break_edit_group();
    }

    /// Re-surface a terminal notice that may have fallen out of the bounded
    /// event ring. Completed output is already represented by durable entries;
    /// indeterminate keeps its stronger persistent warning below.
    fn surface_latest_settlement(&mut self, settlement: Option<&OperationSettlement>) {
        let Some(settlement) = settlement else {
            return;
        };
        match &settlement.outcome {
            OperationOutcome::Completed | OperationOutcome::Indeterminate => {}
            OperationOutcome::Failed(message) => {
                self.pending_scrollback
                    .push(Line::from(format!("! failed: {message}")).red());
            }
            OperationOutcome::Cancelled => {
                self.pending_scrollback
                    .push(Line::from("! cancelled".to_owned()).yellow());
            }
            OperationOutcome::ApprovalRequired { tool } => {
                self.pending_scrollback.push(
                    Line::from(format!(
                        "! approval required: `{tool}` — rerun with --allow {tool}"
                    ))
                    .yellow(),
                );
            }
        }
    }

    /// Queue the authoritative snapshot warning. Lag resynchronization rebuilds
    /// the presentation transcript from durable entries, so this is deliberately
    /// re-queued on every fresh snapshot rather than deduplicated against lines
    /// that may just have been discarded with the old transcript.
    fn surface_indeterminate_warning(&mut self, warning: Option<&ion_core::IndeterminateWarning>) {
        if let Some(warning) = warning {
            self.pending_scrollback.push(
                Line::from(format!(
                    "! indeterminate operation {}: {}",
                    warning.operation_id, warning.message
                ))
                .yellow()
                .bold(),
            );
        }
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
            if state.approval.is_none() {
                insert_text(&mut state, &text, EditKind::Paste);
            }
            (state, None)
        }
        UiMessage::Runtime(event) => (apply_runtime_event(state, event), None),
        UiMessage::SubmitAccepted => {
            state.hotkeys_visible = false;
            state.reset_composer();
            (state, None)
        }
        UiMessage::ModelSwitchAccepted => {
            state.hotkeys_visible = false;
            state.model_selector = None;
            (state, None)
        }
        UiMessage::CompactAccepted => {
            state
                .pending_scrollback
                .push(Line::from("compaction requested at next boundary").dim());
            (state, None)
        }
        UiMessage::SteerAccepted => {
            state.reset_composer();
            (state, None)
        }
        UiMessage::EnqueueAccepted => {
            if let Some(text) = state.pending_history.clone() {
                state.queued_prompt = Some(text);
            }
            state
                .pending_scrollback
                .push(Line::from("queued for the next operation (alt+up restores it)").dim());
            state.reset_composer();
            (state, None)
        }
        UiMessage::SubmitRejected(message)
        | UiMessage::EnqueueRejected(message)
        | UiMessage::SteerRejected(message) => {
            state.reject_pending_history();
            state
                .pending_scrollback
                .push(Line::from(format!("! {message}")).red());
            (state, None)
        }
        UiMessage::SessionListed(summaries) => {
            let query = state.pending_resume_query.take().unwrap_or_default();
            let rows = summaries
                .into_iter()
                .map(|summary| session_row_for_picker(&summary))
                .collect();
            state.open_session_selector(rows, &query);
            (state, None)
        }
        UiMessage::SessionSwitched { session, title } => {
            // The loop re-attaches the runtime and resets presentation;
            // the reducer records durable identity for /session.
            state.session_id = Some(session);
            state.session_title = Some(title);
            (state, None)
        }
        UiMessage::SessionCommandFailed(message) => {
            state
                .pending_scrollback
                .push(Line::from(format!("! {message}")).red());
            (state, None)
        }
        UiMessage::RenameAccepted => {
            state
                .pending_scrollback
                .push(Line::from("session renamed").dim());
            (state, None)
        }
        UiMessage::Dequeued(prompt) => {
            state.queued_prompt = None;
            if let Some(prompt) = prompt {
                state.composer = prompt;
                state.cursor = state.composer.chars().count();
                state.preferred_column = None;
                state.undo_stack.clear();
                state.last_edit = None;
                state.reject_pending_history();
            }
            (state, None)
        }
        UiMessage::ExternalEdited(text) => {
            // Replace the composer wholesale: the editor is authoritative
            // for the whole draft while it was open.
            state.composer = text;
            state.cursor = state.composer.chars().count();
            state.preferred_column = None;
            state.undo_stack.clear();
            state.last_edit = None;
            (state, None)
        }
    }
}

fn handle_model_selector_key(mut state: UiState, key: KeyEvent) -> (UiState, Option<UiEffect>) {
    match key.code {
        KeyCode::Esc if key.modifiers.is_empty() => {
            state.close_model_selector();
            (state, None)
        }
        KeyCode::Enter if key.modifiers.is_empty() => {
            let Some(model) = state.selected_model() else {
                state
                    .pending_scrollback
                    .push(Line::from("no matching models").red());
                return (state, None);
            };
            state.close_model_selector();
            (state, Some(UiEffect::SwitchModel { model }))
        }
        KeyCode::Up if key.modifiers.is_empty() => {
            state.move_model_selection(-1);
            (state, None)
        }
        KeyCode::Down if key.modifiers.is_empty() => {
            state.move_model_selection(1);
            (state, None)
        }
        KeyCode::Backspace if key.modifiers.is_empty() => {
            let (state, _) = handle_backspace(state);
            let mut state = state;
            state.reset_model_selection();
            (state, None)
        }
        KeyCode::Char(ch) if key.modifiers.is_empty() || key.modifiers == Modifiers::SHIFT => {
            insert_at_cursor(&mut state, &ch.to_string());
            state.reset_model_selection();
            (state, None)
        }
        _ => (state, None),
    }
}

fn handle_thinking_selector_key(mut state: UiState, key: KeyEvent) -> (UiState, Option<UiEffect>) {
    match key.code {
        KeyCode::Esc if key.modifiers.is_empty() => {
            state.close_thinking_selector();
            (state, None)
        }
        KeyCode::Enter if key.modifiers.is_empty() => {
            let Some(level) = state.selected_thinking_level() else {
                state
                    .pending_scrollback
                    .push(Line::from("no matching thinking levels").red());
                return (state, None);
            };
            state.close_thinking_selector();
            state.thinking_level = Some(level.clone());
            (
                state,
                Some(UiEffect::SwitchThinking {
                    thinking: Some(level),
                }),
            )
        }
        KeyCode::Up if key.modifiers.is_empty() => {
            state.move_thinking_selection(-1);
            (state, None)
        }
        KeyCode::Down if key.modifiers.is_empty() => {
            state.move_thinking_selection(1);
            (state, None)
        }
        KeyCode::Backspace if key.modifiers.is_empty() => {
            let (state, _) = handle_backspace(state);
            let mut state = state;
            state.reset_thinking_selection();
            (state, None)
        }
        KeyCode::Char(ch) if key.modifiers.is_empty() || key.modifiers == Modifiers::SHIFT => {
            insert_at_cursor(&mut state, &ch.to_string());
            state.reset_thinking_selection();
            (state, None)
        }
        _ => (state, None),
    }
}

fn handle_session_selector_key(mut state: UiState, key: KeyEvent) -> (UiState, Option<UiEffect>) {
    match key.code {
        KeyCode::Esc if key.modifiers.is_empty() => {
            state.close_session_selector();
            (state, None)
        }
        KeyCode::Enter if key.modifiers.is_empty() => {
            let Some(session) = state.selected_session() else {
                state
                    .pending_scrollback
                    .push(Line::from("no matching sessions").red());
                return (state, None);
            };
            state.close_session_selector();
            (state, Some(UiEffect::ResumeSession { session }))
        }
        KeyCode::Up if key.modifiers.is_empty() => {
            state.move_session_selection(-1);
            (state, None)
        }
        KeyCode::Down if key.modifiers.is_empty() => {
            state.move_session_selection(1);
            (state, None)
        }
        KeyCode::Backspace if key.modifiers.is_empty() => {
            let (state, _) = handle_backspace(state);
            let mut state = state;
            state.reset_session_selection();
            (state, None)
        }
        KeyCode::Char(ch) if key.modifiers.is_empty() || key.modifiers == Modifiers::SHIFT => {
            insert_at_cursor(&mut state, &ch.to_string());
            state.reset_session_selection();
            (state, None)
        }
        _ => (state, None),
    }
}

/// The `@` file picker owns the keyboard while open: the composer is
/// the filter query over host-provided rows (pi parity: fuzzy file
/// search). Enter splices the reference into the saved draft; esc
/// restores it untouched.
fn handle_file_selector_key(mut state: UiState, key: KeyEvent) -> (UiState, Option<UiEffect>) {
    match key.code {
        KeyCode::Esc if key.modifiers.is_empty() => {
            state.close_file_selector();
            (state, None)
        }
        KeyCode::Enter if key.modifiers.is_empty() => {
            let Some(path) = state.selected_file_row() else {
                state
                    .pending_scrollback
                    .push(Line::from("no matching files").red());
                return (state, None);
            };
            state.accept_file_row(path);
            (state, None)
        }
        KeyCode::Up if key.modifiers.is_empty() => {
            state.move_file_selection(-1);
            (state, None)
        }
        KeyCode::Down if key.modifiers.is_empty() => {
            state.move_file_selection(1);
            (state, None)
        }
        KeyCode::Backspace if key.modifiers.is_empty() => {
            let (state, _) = handle_backspace(state);
            let mut state = state;
            state.reset_file_selection();
            (state, None)
        }
        KeyCode::Char(ch) if key.modifiers.is_empty() || key.modifiers == Modifiers::SHIFT => {
            insert_at_cursor(&mut state, &ch.to_string());
            state.reset_file_selection();
            (state, None)
        }
        _ => (state, None),
    }
}

fn handle_key(mut state: UiState, key: KeyEvent) -> (UiState, Option<UiEffect>) {
    if state.model_selector.is_some() {
        return handle_model_selector_key(state, key);
    }
    if state.thinking_selector.is_some() {
        return handle_thinking_selector_key(state, key);
    }
    if state.session_selector.is_some() {
        return handle_session_selector_key(state, key);
    }
    if state.file_selector.is_some() {
        return handle_file_selector_key(state, key);
    }
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
    if state.hotkeys_visible {
        if (key.code == KeyCode::Char('?') && key.modifiers.is_empty())
            || (key.code == KeyCode::Esc && key.modifiers.is_empty())
        {
            state.hotkeys_visible = false;
        }
        return (state, None);
    }
    if state.composer.is_empty()
        && matches!(state.status, UiStatus::Idle)
        && key.code == KeyCode::Char('?')
        && key.modifiers.is_empty()
    {
        state.hotkeys_visible = true;
        return (state, None);
    }
    if let Some(action) = state.keymap.action_for(&key) {
        return handle_action(state, action);
    }
    match key.code {
        KeyCode::Backspace => handle_backspace(state),
        KeyCode::Delete => {
            let mut state = state;
            if state.cursor < state.composer.chars().count() {
                state.record_edit(EditKind::Delete);
            }
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
            // Typing '@' at a word start opens the file picker (pi
            // parity: fuzzy project-file search). The '@' itself is not
            // inserted; acceptance splices the full reference in.
            if ch == '@'
                && state.file_selector.is_none()
                && at_word_start(&state.composer, state.cursor)
                && !state.workspace_files.is_empty()
            {
                state.open_file_selector(state.cursor);
                return (state, None);
            }
            insert_at_cursor(&mut state, &ch.to_string());
            (state, None)
        }
        _ => (state, None),
    }
}

/// Whether `offset` starts a new word in `text`: at the beginning or
/// after whitespace. The `@` picker opens only at word starts so paths
/// inside prose stay literal.
fn at_word_start(text: &str, offset: usize) -> bool {
    match text.chars().take(offset).last() {
        None => true,
        Some(previous) => previous.is_whitespace(),
    }
}

fn handle_backspace(mut state: UiState) -> (UiState, Option<UiEffect>) {
    if state.cursor > 0 {
        state.record_edit(EditKind::Delete);
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

/// Slash-command surface. Anything unknown is a visible error, never a
/// silent no-op.
fn handle_command(state: &mut UiState, command: &str) -> (UiState, Option<UiEffect>) {
    let (name, rest) = match command.split_once(' ') {
        Some((name, rest)) => (name, rest.trim()),
        None => (command, ""),
    };
    match name {
        "help" => {
            for line in [
                "/compact [instructions] - summarize the active operation's context",
                "/model [id|number]      - pick or switch the model",
                "/thinking [level]      - pick the thinking level (off..max)",
                "shift+tab · ctrl+l · ctrl+p - cycle thinking, models, model picker",
                "/new · /resume [query] · /clone - session switching",
                "/name <title> · /session - rename; show session identity",
                "enter · shift+enter · ctrl+j - submit, steer, newline",
                "ctrl+g                  - edit the draft in $VISUAL/$EDITOR",
                "alt+left/right · alt+b/f - move by words",
                "alt+d · ctrl+y · alt+y  - kill word; yank; cycle the kill ring",
                "tab                     - complete commands/models",
                "ctrl+p · shift+ctrl+p   - cycle models forward/backward",
                "ctrl+o · ctrl+t · ctrl+_ - tool output, thinking, undo edit",
                "@                       - reference a file (fuzzy search)",
                "! · !!                  - shell passthrough (send / local only)",
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
            if !state.model_switching_available {
                notice(state, "model switching unavailable (scripted provider)");
                return (std::mem::take(state), None);
            }
            if let Ok(index) = rest.parse::<usize>() {
                if index == 0 {
                    notice(state, "model selection must start at 1");
                    return (std::mem::take(state), None);
                }
                let Some(model) = state.model_catalog.get(index - 1).cloned() else {
                    notice(state, &format!("model selection out of range: {rest}"));
                    return (std::mem::take(state), None);
                };
                return (std::mem::take(state), Some(UiEffect::SwitchModel { model }));
            }
            state.open_model_selector(rest);
            (std::mem::take(state), None)
        }
        "thinking" => {
            if let Ok(index) = rest.parse::<usize>() {
                if index == 0 {
                    notice(state, "thinking selection must start at 1");
                    return (std::mem::take(state), None);
                }
                let Some(level) = THINKING_LEVELS.get(index - 1) else {
                    notice(state, &format!("thinking selection out of range: {rest}"));
                    return (std::mem::take(state), None);
                };
                let level = (*level).to_owned();
                state.thinking_level = Some(level.clone());
                return (
                    std::mem::take(state),
                    Some(UiEffect::SwitchThinking {
                        thinking: Some(level),
                    }),
                );
            }
            if rest == "default" {
                state.thinking_level = None;
                return (
                    std::mem::take(state),
                    Some(UiEffect::SwitchThinking { thinking: None }),
                );
            }
            state.open_thinking_selector(rest);
            (std::mem::take(state), None)
        }
        "new" => {
            if matches!(state.status, UiStatus::Working { .. }) {
                notice(
                    state,
                    "cannot start a new session while the current operation is running",
                );
                return (std::mem::take(state), None);
            }
            if state.approval.is_some() {
                notice(state, "decide the pending approval first");
                return (std::mem::take(state), None);
            }
            (std::mem::take(state), Some(UiEffect::NewSession))
        }
        "resume" => {
            if matches!(state.status, UiStatus::Working { .. }) {
                notice(
                    state,
                    "cannot switch sessions while the current operation is running",
                );
                return (std::mem::take(state), None);
            }
            if state.approval.is_some() {
                notice(state, "decide the pending approval first");
                return (std::mem::take(state), None);
            }
            // The host resolves the list and the reducer opens the picker
            // when the rows arrive; the query is stashed for that reopen.
            state.pending_resume_query = (!rest.is_empty()).then(|| rest.to_owned());
            (std::mem::take(state), Some(UiEffect::RequestSessionList))
        }
        "name" => {
            if rest.is_empty() {
                notice(state, "usage: /name <title>");
                return (std::mem::take(state), None);
            }
            (
                std::mem::take(state),
                Some(UiEffect::RenameSession {
                    title: rest.to_owned(),
                }),
            )
        }
        "session" => {
            let id = state
                .session_id
                .map(|id| id.as_uuid().to_string())
                .unwrap_or_else(|| "unknown".to_owned());
            let title = state.session_title.as_deref().unwrap_or("");
            let entries = state.history.len();
            notice(
                state,
                &format!("session {id} · title: {title:?} · prompts this run: {entries}"),
            );
            (std::mem::take(state), None)
        }
        "clone" => {
            if matches!(state.status, UiStatus::Working { .. }) {
                notice(state, "cannot clone while the current operation is running");
                return (std::mem::take(state), None);
            }
            if state.approval.is_some() {
                notice(state, "decide the pending approval first");
                return (std::mem::take(state), None);
            }
            (std::mem::take(state), Some(UiEffect::CloneSession))
        }
        "quit" => (std::mem::take(state), Some(UiEffect::Quit)),
        other => {
            notice(state, &format!("unknown command: /{other} (try /help)"));
            (std::mem::take(state), None)
        }
    }
}

const MAX_COMPLETION_SUGGESTIONS: usize = 16;

/// Rows the `@` file picker filters over (pi caps its fd-backed list at
/// 100 per query; the picker renders a bounded window anyway).
const MAX_FILE_SELECTOR_ROWS: usize = 100;

fn complete_composer(state: &mut UiState) {
    if state.cursor != state.composer.chars().count() {
        return;
    }
    let Some(command) = state.composer.strip_prefix('/') else {
        return;
    };
    let (prefix, mut partial, mut candidates) = match command.split_once(' ') {
        Some((name, rest)) if name == "model" && !rest.chars().any(char::is_whitespace) => (
            "/model ".to_owned(),
            rest.to_owned(),
            state.model_catalog.clone(),
        ),
        Some(_) => return,
        None => (
            "/".to_owned(),
            command.to_owned(),
            [
                "clone", "compact", "help", "model", "name", "new", "resume", "session",
                "thinking", "quit",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        ),
    };
    candidates.retain(|candidate| candidate.starts_with(&partial));
    if candidates.is_empty() {
        return;
    }
    let common = common_prefix(&candidates);
    if candidates.len() == 1 {
        partial = candidates[0].clone();
    } else if common.len() > partial.len() {
        partial = common;
    } else {
        for candidate in candidates.into_iter().take(MAX_COMPLETION_SUGGESTIONS) {
            notice(state, &format!("  {prefix}{candidate}"));
        }
        return;
    }
    let mut completed = format!("{prefix}{partial}");
    if prefix == "/" {
        completed.push(' ');
    }
    if completed != state.composer {
        state.record_edit(EditKind::Insert);
        state.composer = completed;
        state.cursor = state.composer.chars().count();
        state.preferred_column = None;
        state.exit_history_browse();
    }
}

/// Render one store summary as a picker row: the label is searchable
/// (title + short id + relative recency) and carries the durable identity.
fn session_row_for_picker(summary: &ion_core::SessionSummary) -> SessionRow {
    let full_id = summary.id.as_uuid().to_string();
    let short_id = &full_id[..8.min(full_id.len())];
    let title = if summary.title.is_empty() {
        short_id.to_owned()
    } else {
        summary.title.clone()
    };
    let age = relative_age(summary.updated_at);
    let label = if summary.entry_count == 0 {
        format!("{title} · {age} · empty")
    } else {
        format!("{title} · {age} · {} entries", summary.entry_count)
    };
    SessionRow {
        id: summary.id,
        label,
        title,
        updated_at: summary.updated_at,
    }
}

/// Coarse recency for picker rows. Epoch-millisecond timestamps render as
/// a human-relative hint, not a clock.
fn relative_age(updated_at: u64) -> String {
    if updated_at == 0 {
        return "unknown age".to_owned();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    let elapsed = now.saturating_sub(updated_at);
    const MINUTE: u64 = 60_000;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    if elapsed < MINUTE {
        "just now".to_owned()
    } else if elapsed < HOUR {
        format!("{}m ago", elapsed / MINUTE)
    } else if elapsed < DAY {
        format!("{}h ago", elapsed / HOUR)
    } else {
        format!("{}d ago", elapsed / DAY)
    }
}

fn fuzzy_contains(candidate: &str, query: &str) -> bool {
    let mut candidate_chars = candidate.chars();
    query
        .chars()
        .all(|wanted| candidate_chars.by_ref().any(|actual| actual == wanted))
}

fn common_prefix(candidates: &[String]) -> String {
    let Some(first) = candidates.first() else {
        return String::new();
    };
    let length = candidates[1..]
        .iter()
        .map(|candidate| {
            first
                .chars()
                .zip(candidate.chars())
                .take_while(|(left, right)| left == right)
                .count()
        })
        .min()
        .unwrap_or_else(|| first.chars().count());
    first.chars().take(length).collect()
}

fn model_display_parts(model: &str, fallback_provider: Option<&str>) -> (Option<String>, String) {
    for provider in ["openai-codex", "openrouter", "desktop"] {
        let prefix = format!("{provider}/");
        if let Some(model) = model.strip_prefix(&prefix) {
            return (Some(provider.to_owned()), model.to_owned());
        }
    }
    (fallback_provider.map(str::to_owned), model.to_owned())
}

/// Pi's fixed thinking vocabulary (pi: --thinking off..max).
const THINKING_LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

fn cycle_model(state: &mut UiState, delta: isize) -> Option<UiEffect> {
    if !state.model_switching_available {
        notice(state, "model switching unavailable (scripted provider)");
        return None;
    }
    let count = state.model_catalog.len();
    if count == 0 {
        notice(state, "no models are configured");
        return None;
    }
    let current = state.current_model_reference().and_then(|current| {
        state
            .model_catalog
            .iter()
            .position(|model| model == &current)
    });
    let index = current.map_or_else(
        || if delta < 0 { count - 1 } else { 0 },
        |index| (index as isize + delta).rem_euclid(count as isize) as usize,
    );
    Some(UiEffect::SwitchModel {
        model: state.model_catalog[index].clone(),
    })
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
                state.reset_composer();
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
        Action::Undo => {
            state.undo_edit();
            (state, None)
        }
        Action::ExternalEditor => {
            // The loop owns the terminal; it suspends raw mode, runs the
            // editor, and returns the edited draft as a message.
            (state, Some(UiEffect::ExternalEditor))
        }
        Action::QueueFollowUp => {
            // alt+enter always queues after completion, even while idle
            // (pi parity: the follow-up queue is explicit, not implied by
            // the working state like plain enter's queue-while-busy).
            let text = state.composer.trim().to_owned();
            if text.is_empty() {
                return (state, None);
            }
            state.break_edit_group();
            state.history_index = None;
            state.history_stash = None;
            state.history.push(text.clone());
            state.pending_history = Some(text.clone());
            (state, Some(UiEffect::Enqueue { text }))
        }
        Action::DequeueFollowUp => {
            // Only meaningful with a queued prompt; inert otherwise (pi
            // parity: alt+up is a no-op when the queue is empty).
            (state, Some(UiEffect::DequeueNextRun))
        }
        Action::OpenModelSelector => {
            if !state.model_switching_available {
                notice(
                    &mut state,
                    "model switching unavailable (scripted provider)",
                );
            } else {
                state.open_model_selector("");
            }
            (state, None)
        }
        Action::CycleModelForward => {
            let effect = cycle_model(&mut state, 1);
            (state, effect)
        }
        Action::CycleModelBackward => {
            let effect = cycle_model(&mut state, -1);
            (state, effect)
        }
        Action::CycleThinking => {
            // shift+tab cycles the pi vocabulary, wrapping; the current
            // level is durable state so the cycle starts there.
            let current = state
                .thinking_level
                .clone()
                .unwrap_or_else(|| "off".to_owned());
            let index = THINKING_LEVELS
                .iter()
                .position(|level| *level == current)
                .unwrap_or(0);
            let next = THINKING_LEVELS[(index + 1) % THINKING_LEVELS.len()].to_owned();
            state.thinking_level = Some(next.clone());
            (
                state,
                Some(UiEffect::SwitchThinking {
                    thinking: Some(next),
                }),
            )
        }
        Action::ToggleToolOutput => {
            state.tool_output_expanded = !state.tool_output_expanded;
            (state, None)
        }
        Action::ToggleThinking => {
            state.thinking_visible = !state.thinking_visible;
            (state, None)
        }
        Action::InsertNewline => {
            insert_text(&mut state, "\n", EditKind::Insert);
            (state, None)
        }
        Action::Complete => {
            complete_composer(&mut state);
            (state, None)
        }
        Action::Submit => {
            let text = state.composer.trim().to_owned();
            if text.is_empty() {
                return (state, None);
            }
            state.break_edit_group();
            state.history_index = None;
            state.history_stash = None;
            // Shell passthrough (pi parity) is checked first: a text
            // starting with '!' never collides with slash commands.
            // `!cmd` output joins the model context; `!!cmd` stays durable
            // but excluded. Only an idle lane can run one — the runtime
            // refuses otherwise and the notice explains.
            if let Some(command) = text.strip_prefix('!') {
                let exclude_from_context = command.starts_with('!');
                let command = command.strip_prefix('!').unwrap_or(command).trim();
                if command.is_empty() {
                    notice(&mut state, "shell command required after '!'");
                    state.reset_composer();
                    return (state, None);
                }
                if !matches!(state.status, UiStatus::Idle) {
                    notice(
                        &mut state,
                        "shell passthrough needs an idle session — wait for the operation or cancel it",
                    );
                    state.reset_composer();
                    return (state, None);
                }
                state.reset_composer();
                state.history.push(text.clone());
                state.pending_history = Some(text.clone());
                return (
                    state,
                    Some(UiEffect::RunShell {
                        command: command.to_owned(),
                        exclude_from_context,
                    }),
                );
            }
            // Slash commands are frontend presentation over SessionHandle
            // commands - never TUI-only session logic.
            if let Some(command) = text.strip_prefix('/') {
                state.reset_composer();
                return handle_command(&mut state, command);
            }
            state.history.push(text.clone());
            state.pending_history = Some(text.clone());
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
            state.break_edit_group();
            state.history_index = None;
            state.history_stash = None;
            state.history.push(text.clone());
            state.pending_history = Some(text.clone());
            (state, Some(UiEffect::Steer { text }))
        }
        Action::CursorLeft if state.cursor > 0 => {
            state.cursor -= 1;
            state.preferred_column = None;
            state.break_edit_group();
            state.exit_history_browse();
            (state, None)
        }
        Action::CursorRight if state.cursor < state.composer.chars().count() => {
            state.cursor += 1;
            state.preferred_column = None;
            state.break_edit_group();
            state.exit_history_browse();
            (state, None)
        }
        Action::CursorLeft | Action::CursorRight => {
            state.break_edit_group();
            (state, None)
        }
        Action::CursorWordLeft => {
            if state.cursor > 0 {
                state.cursor = word_start(&state.composer, state.cursor);
                state.preferred_column = None;
                state.break_edit_group();
                state.exit_history_browse();
            }
            (state, None)
        }
        Action::CursorWordRight => {
            let chars = state.composer.chars().count();
            if state.cursor < chars {
                state.cursor = word_end(&state.composer, state.cursor);
                state.preferred_column = None;
                state.break_edit_group();
                state.exit_history_browse();
            }
            (state, None)
        }
        Action::CursorHome => {
            state.cursor = 0;
            state.preferred_column = None;
            state.break_edit_group();
            (state, None)
        }
        Action::CursorEnd => {
            state.cursor = state.composer.chars().count();
            state.preferred_column = None;
            state.break_edit_group();
            (state, None)
        }
        Action::HistoryPrevious => {
            if state.composer.contains('\n') {
                move_vertical(&mut state, -1);
            } else {
                browse_history(&mut state, -1);
            }
            (state, None)
        }
        Action::HistoryNext => {
            if state.composer.contains('\n') {
                move_vertical(&mut state, 1);
            } else {
                browse_history(&mut state, 1);
            }
            (state, None)
        }
        Action::KillToEnd => {
            let chars = state.composer.chars().count();
            if state.cursor < chars {
                state.preferred_column = None;
                state.record_edit(EditKind::Delete);
                let killed = split_off_chars(&mut state.composer, state.cursor, chars);
                state.push_kill(killed);
            }
            (state, None)
        }
        Action::KillToStart => {
            if state.cursor > 0 {
                state.preferred_column = None;
                state.record_edit(EditKind::Delete);
                let killed = split_off_chars(&mut state.composer, 0, state.cursor);
                state.push_kill(killed);
                state.cursor = 0;
            }
            (state, None)
        }
        Action::KillWord => {
            let start = word_start(&state.composer, state.cursor);
            if start < state.cursor {
                state.preferred_column = None;
                state.record_edit(EditKind::Delete);
                let killed = split_off_chars(&mut state.composer, start, state.cursor);
                state.push_kill(killed);
                state.cursor = start;
            }
            (state, None)
        }
        Action::KillWordForward => {
            let end = word_end(&state.composer, state.cursor);
            if end > state.cursor {
                state.preferred_column = None;
                state.record_edit(EditKind::Delete);
                let killed = split_off_chars(&mut state.composer, state.cursor, end);
                state.push_kill(killed);
            }
            (state, None)
        }
        Action::Yank => {
            if !state.kill_buffer.is_empty() {
                let yank = state.kill_buffer.clone();
                insert_at_cursor(&mut state, &yank);
                state.yank_index = Some(0);
                state.yank_span = Some((state.cursor - yank.chars().count(), state.cursor));
            }
            (state, None)
        }
        Action::YankPop => {
            // Replace the last yanked span with the next older ring entry.
            let ring_len = 1 + state.kill_ring.len();
            if ring_len <= 1 {
                return (state, None);
            }
            let Some(current) = state.yank_index else {
                return (state, None);
            };
            let Some((start, end)) = state.yank_span else {
                return (state, None);
            };
            let next = (current + 1) % ring_len;
            let entry = state.ring_entry(next).to_owned();
            state.preferred_column = None;
            state.record_edit(EditKind::Delete);
            // Remove the previously yanked span, insert the next entry.
            split_off_chars(&mut state.composer, start, end);
            let byte = char_offset_to_byte(&state.composer, start);
            state.composer.insert_str(byte, &entry);
            state.cursor = start + entry.chars().count();
            state.record_edit(EditKind::Insert);
            state.yank_index = Some(next);
            state.yank_span = Some((start, state.cursor));
            (state, None)
        }
    }
}

fn insert_at_cursor(state: &mut UiState, text: &str) {
    insert_text(state, text, EditKind::Insert);
}

fn insert_text(state: &mut UiState, text: &str, kind: EditKind) {
    if text.is_empty() {
        return;
    }
    state.preferred_column = None;
    state.record_edit(kind);
    let byte = char_offset_to_byte(&state.composer, state.cursor);
    state.composer.insert_str(byte, text);
    state.cursor += text.chars().count();
    state.exit_history_browse();
}

fn delete_at_cursor(state: &mut UiState) {
    let end = char_offset_to_byte(&state.composer, state.cursor + 1);
    let byte = char_offset_to_byte(&state.composer, state.cursor);
    if byte < end {
        state.preferred_column = None;
        state.record_edit(EditKind::Delete);
        state.composer.replace_range(byte..end, "");
    }
}

/// Move the cursor between visual composer rows while retaining the
/// preferred display column when adjacent rows have different widths.
fn move_vertical(state: &mut UiState, direction: i32) {
    if !matches!(direction, -1 | 1) {
        return;
    }
    let positions = render::composer_cursor_positions(&state.composer, state.composer_width());
    let Some(current) = positions
        .iter()
        .find(|position| position.cursor == state.cursor.min(state.composer.chars().count()))
    else {
        return;
    };
    let preferred = state.preferred_column.unwrap_or(current.column);
    let target_row = if direction < 0 {
        current.row.checked_sub(1)
    } else {
        Some(current.row + 1)
    };
    let Some(target_row) = target_row else {
        return;
    };
    let Some(target) = positions
        .iter()
        .filter(|position| position.row == target_row)
        .min_by_key(|position| {
            (
                position.column.abs_diff(preferred),
                position.column,
                position.cursor,
            )
        })
    else {
        return;
    };

    state.cursor = target.cursor;
    state.preferred_column = Some(preferred);
    state.break_edit_group();
    state.exit_history_browse();
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

/// End of the word at/after `cursor` (whitespace-delimited). Inside a
/// word this is that word's end; between words it is the next word's
/// end (emacs forward-word).
fn word_end(buffer: &str, cursor: usize) -> usize {
    let mut chars = buffer.chars().enumerate().skip(cursor).peekable();
    let mut end = cursor;
    // If the cursor sits on a word character, its end is the target.
    if let Some((index, ch)) = chars.peek()
        && !ch.is_whitespace()
    {
        end = *index;
    }
    // Advance over whitespace then the word (or the rest of the current
    // word when the cursor was inside one).
    let mut in_word = false;
    for (index, ch) in chars.by_ref() {
        if ch.is_whitespace() {
            if in_word {
                return index;
            }
        } else {
            in_word = true;
            end = index + 1;
        }
    }
    end
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

    /// Record a kill: the previous head moves onto the ring; the new
    /// text becomes the yank head. The ring is bounded; the oldest
    /// entry drops when full.
    fn push_kill(&mut self, text: String) {
        const KILL_RING_LIMIT: usize = 16;
        if !self.kill_buffer.is_empty() {
            self.kill_ring.push(std::mem::take(&mut self.kill_buffer));
            if self.kill_ring.len() > KILL_RING_LIMIT {
                self.kill_ring.remove(0);
            }
        }
        self.kill_buffer = text;
        self.yank_index = None;
        self.yank_span = None;
    }

    /// Ring entry by index: 0 is the head (`kill_buffer`), older
    /// entries follow in reverse-chronological order.
    fn ring_entry(&self, index: usize) -> &str {
        if index == 0 {
            return &self.kill_buffer;
        }
        self.kill_ring
            .get(self.kill_ring.len().saturating_sub(index))
            .map_or("", String::as_str)
    }
}

/// Step through submitted prompts; direction -1 is older. Leaving the
/// live draft stashes it; stepping past the newest entry restores it.
fn browse_history(state: &mut UiState, direction: i32) {
    if state.history.is_empty() {
        return;
    }
    state.break_edit_group();
    state.preferred_column = None;
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
        RuntimeEvent::ShellStarted {
            command,
            exclude_from_context,
            ..
        } => {
            state.hotkeys_visible = false;
            let prefix = if exclude_from_context { "!!" } else { "!" };
            state
                .pending_scrollback
                .push(Line::from(format!("{prefix}{command}")).style(palette.user_marker));
            state.status = UiStatus::Working {
                operation: "shell".to_owned(),
            };
        }
        RuntimeEvent::ShellOutput { output, .. } => {
            // Provisional display only; bound the retained preview so a
            // chatty command cannot grow UiState without limit. The
            // durable entry keeps the real bounded output.
            const SHELL_PREVIEW_MAX_BYTES: usize = 8 * 1024;
            state.shell_output.push_str(&output);
            if state.shell_output.len() > SHELL_PREVIEW_MAX_BYTES {
                let keep = state
                    .shell_output
                    .char_indices()
                    .rev()
                    .nth(SHELL_PREVIEW_MAX_BYTES)
                    .map_or(0, |(index, _)| index);
                state.shell_output = state.shell_output[keep..].to_owned();
            }
        }
        RuntimeEvent::ShellSettled {
            exit_code,
            cancelled,
            output_preview,
            ..
        } => {
            // The durable entry carries the settled output; the live
            // preview is provisional and ends here. The bounded preview
            // renders immediately; a lagged client rebuilds from entries.
            state.shell_output.clear();
            state.status = UiStatus::Idle;
            if cancelled {
                state
                    .pending_scrollback
                    .push(Line::from("! cancelled".to_owned()).yellow());
            } else if exit_code != Some(0) {
                state
                    .pending_scrollback
                    .push(Line::from(format!("! exited with {exit_code:?}")).yellow());
            }
            for logical_line in output_preview.as_deref().unwrap_or_default().split('\n') {
                if logical_line.is_empty() {
                    continue;
                }
                state
                    .pending_scrollback
                    .push(Line::from(format!("  {logical_line}")).style(palette.system_note));
            }
        }
        RuntimeEvent::OperationStarted { prompt, .. } => {
            state.hotkeys_visible = false;
            state
                .pending_scrollback
                .push(Line::from(format!("> {prompt}")).style(palette.user_marker));
            // The queued prompt became the running operation: its live
            // presentation ends here, before the status line takes over.
            state.queued_prompt = None;
            state.draft.clear();
            state.tool_rows.clear();
            state.usage = None;
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
                progress: None,
                preview: None,
            });
        }
        RuntimeEvent::ToolProgress { output, .. } => {
            if let Some(row) = state.tool_rows.last_mut() {
                row.progress = Some(output);
            }
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
                row.progress = None;
                row.preview = preview;
            }
        }
        RuntimeEvent::UsageUpdate { usage, .. } => {
            state.usage = Some(usage);
        }
        RuntimeEvent::OperationFinished { .. } => {
            state.flush_draft();
            state.approval = None;
            state.status = UiStatus::Idle;
        }
        RuntimeEvent::OperationFailed { message, .. } => {
            state.abandon_draft();
            state
                .pending_scrollback
                .push(Line::from(format!("! failed: {message}")).red());
            state.approval = None;
            state.status = UiStatus::Idle;
        }
        RuntimeEvent::OperationIndeterminate { message, .. } => {
            state.abandon_draft();
            state.pending_scrollback.push(
                Line::from(format!("! indeterminate: {message}"))
                    .yellow()
                    .bold(),
            );
            state.approval = None;
            state.status = UiStatus::Idle;
        }
        RuntimeEvent::OperationCancelled { .. } => {
            state.abandon_draft();
            state
                .pending_scrollback
                .push(Line::from("! cancelled".to_owned()).yellow());
            state.approval = None;
            state.status = UiStatus::Idle;
        }
        RuntimeEvent::OperationApprovalRequired { tool, .. } => {
            state.abandon_draft();
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
            state.hotkeys_visible = false;
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
    fn flush_tool_rows(&mut self) {
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
    }

    fn flush_draft(&mut self) {
        flush_thinking(self);
        self.flush_tool_rows();
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

    /// Remove a model draft that did not reach a durable completion. Its
    /// text is never promoted to ordinary assistant scrollback.
    fn abandon_draft(&mut self) {
        let had_partial = !self.draft.is_empty() || !self.draft_thinking.is_empty();
        self.draft.clear();
        self.draft_thinking.clear();
        self.draft_degraded = false;
        self.flush_tool_rows();
        if had_partial {
            self.pending_scrollback.push(
                Line::from("… partial model output discarded; rerun or use /resume").yellow(),
            );
        }
    }

    /// Rebuild live state from a fresh snapshot after an event lag
    /// (§21.4): the snapshot is authoritative for operation status;
    /// partial deltas and missed tool rows are display-only losses.
    fn resync_after_lag(&mut self, snapshot: &SessionSnapshot) {
        self.close_model_selector();
        self.hotkeys_visible = false;
        self.queued_prompt = snapshot
            .pending_next_run
            .as_ref()
            .map(|next_run| next_run.prompt.clone());
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
        // Restore the runtime-owned projection of the latest durable usage;
        // frontend resynchronization never reads the store directly.
        self.usage = snapshot.latest_usage;
        // A terminal settlement is relevant to the reconstructed foreground
        // only while main is idle. During a newer active operation the retained
        // settlement belongs to an earlier turn and must not be re-announced.
        if matches!(snapshot.operation, OperationStatus::Idle) {
            self.surface_latest_settlement(snapshot.latest_settlement.as_ref());
        }
        self.surface_indeterminate_warning(snapshot.indeterminate.as_ref());
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
                        progress: None,
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

/// Open the composer draft in $VISUAL/$EDITOR (pi parity: ctrl+g). The
/// terminal is suspended for the child, so the editor gets a cooked
/// screen; on return the TUI re-arms and the edited text replaces the
/// composer wholesale. An empty result keeps the draft unchanged.
fn run_external_editor(
    terminal: &mut TerminalSession,
    screen: &mut Screen,
    draft: &str,
) -> Result<String, RuntimeError> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "nano".to_owned());
    // $VISUAL/$EDITOR may carry arguments ("code --wait"); split on
    // whitespace like a shell would for this simple case.
    let mut editor_words = editor.split_whitespace().map(str::to_owned);
    let Some(program) = editor_words.next() else {
        return Err(RuntimeError::OperationFailed(
            "external editor is configured empty".to_owned(),
        ));
    };
    let editor_args: Vec<String> = editor_words.collect();
    let path = std::env::temp_dir().join(format!("ion-compose-{}", std::process::id()));
    if let Err(err) = std::fs::write(&path, draft) {
        return Err(RuntimeError::OperationFailed(format!(
            "could not stage the draft: {err}"
        )));
    }
    terminal
        .suspend()
        .map_err(|err| RuntimeError::OperationFailed(format!("terminal suspend failed: {err}")))?;
    let status = std::process::Command::new(&program)
        .args(&editor_args)
        .arg(&path)
        .status()
        .map_err(|err| RuntimeError::OperationFailed(format!("could not run {program:?}: {err}")));
    let read = std::fs::read_to_string(&path);
    let _ = std::fs::remove_file(&path);
    terminal
        .resume()
        .map_err(|err| RuntimeError::OperationFailed(format!("terminal resume failed: {err}")))?;
    screen.invalidate();
    let status = status?;
    if !status.success() {
        return Err(RuntimeError::OperationFailed(format!(
            "editor {program:?} exited with {status}"
        )));
    }
    let edited = read.map_err(|err| {
        RuntimeError::OperationFailed(format!("could not read the edited draft: {err}"))
    })?;
    // One trailing newline is the editor's line terminator, not content.
    let edited = edited.strip_suffix('\n').unwrap_or(&edited).to_owned();
    Ok(edited)
}

/// The TUI event loop: runtime events and terminal keys into the
/// reducer; effects dispatch straight back into the session. Never
/// blocks rendering on provider/tool I/O (TERMINAL.md, runtime interaction).
/// Session-lifecycle attachment for the run loop: the manager owns
/// runtime switching; `attached` is the current stack, consumed by the
/// loop's close path. `None` marks a host without session switching
/// (tests, embedded frontends).
pub struct SessionHost {
    pub manager: Option<crate::session_manager::SessionManager>,
    pub attached: Option<crate::session_manager::AttachedSession>,
}

pub async fn run(
    session: SessionHandle,
    resume_session: Option<ion_core::SessionId>,
    theme: Theme,
    keymap: KeyMap,
    host: HostConfig,
    session_host: SessionHost,
    mut terminal: TerminalSession,
) -> Result<(), RuntimeError> {
    let SessionHost { manager, attached } = session_host;
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
        // Keep startup quiet. `/help` is the explicit discovery path;
        // contextual notices appear only when an action needs them.
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
    let mut screen = Screen::with_live_height(term_w, origin, term_h, render::LIVE_REGION_MAX_ROWS);

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
    state.model_provider = host.model_provider.clone();
    state.model_catalog = host.model_catalog.clone();
    state.thinking_visible = !host.hide_thinking_block;
    state.cwd_label = host.cwd_label.clone();
    state.workspace_files = host.workspace_files.clone();
    state.branch = host.branch.clone();
    state.model_switching_available = switching_available;
    state.session_id = attached
        .as_ref()
        .map(crate::session_manager::AttachedSession::session_id);
    state.session_title = attached
        .as_ref()
        .map(crate::session_manager::AttachedSession::title)
        .map(str::to_owned);
    if let Some(notice) = host.startup_notice {
        state
            .pending_scrollback
            .push(Line::from(format!("! {notice}")).yellow().bold());
    }
    let (snapshot, events) = session.subscribe().await?;
    let resume_entry_count = snapshot.reopen_entry_count.unwrap_or(0);
    // The session's durable selection is authoritative once subscribed;
    // a resumed session may have switched models in an earlier run.
    // Scripted launches keep the host's display fallback. Real launches
    // must split the durable qualified reference before rendering it or
    // comparing it with the qualified catalog.
    state.usage = snapshot.latest_usage;
    state.thinking_level = snapshot.thinking.clone();
    if host.model_name.is_some() {
        let (provider, model_name) =
            model_display_parts(&snapshot.model_ref, state.model_provider.as_deref());
        state.model_provider = provider;
        state.set_model_name(Some(model_name));
    }
    if matches!(snapshot.operation, OperationStatus::Idle) {
        state.surface_latest_settlement(snapshot.latest_settlement.as_ref());
    }
    state.surface_indeterminate_warning(snapshot.indeterminate.as_ref());
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

    let mut attached = attached;
    let mut session = session;
    let mut events = events;
    let mut resume_session = resume_session;

    loop {
        // Size changes are polled directly: resize events ride the same
        // fragile stream as keys.
        if let Ok((w, h)) = terminal.size() {
            screen.resize(w, h);
            state.set_terminal_width(w as usize);
        }
        let band_height = render::live_region_height(&state).min(screen.size().1 as usize);
        screen.set_live_height(band_height);
        transcript.rewrap_if_needed(screen.size().0);
        // Flush completed turns into the committed transcript, then
        // draw committed history + live band as one line-diff frame
        // (line-diff model, TERMINAL.md).
        if !state.pending_scrollback.is_empty() {
            let flushed = std::mem::take(&mut state.pending_scrollback);
            transcript.extend(flushed);
        }
        let (live, live_cursor) =
            render::build_live_at_height(&state, &palette, screen.size().0 as usize, band_height);
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
                            // The external editor needs the terminal; the
                            // loop owns it, so this effect resolves here
                            // rather than in dispatch.
                            if matches!(effect, UiEffect::ExternalEditor) {
                                let edited = run_external_editor(
                                    &mut terminal,
                                    &mut screen,
                                    &state.composer,
                                );
                                match edited {
                                    Ok(text) => {
                                        let (next, _) = update(
                                            std::mem::take(&mut state),
                                            UiMessage::ExternalEdited(text),
                                        );
                                        state = next;
                                    }
                                    Err(err) => notice(
                                        &mut state,
                                        &format!("external editor failed: {err}"),
                                    ),
                                }
                            } else {
                                let switch = dispatch(
                                    &session,
                                    manager.as_ref(),
                                    &mut state,
                                    active_operation,
                                    effect,
                                )
                                .await;
                                if let Some(switch) = switch {
                                    match switch_session(
                                        manager.as_ref(),
                                        &mut attached,
                                        &mut session,
                                        &mut events,
                                        &mut resume_session,
                                        &mut state,
                                        &mut transcript,
                                        &mut active_operation,
                                        &palette,
                                        switch,
                                    )
                                    .await
                                    {
                                        Ok(()) => {}
                                        Err(err) => {
                                            notice(
                                                &mut state,
                                                &format!("session switch failed: {err}"),
                                            );
                                        }
                                    }
                                }
                            }
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
                            let switch = dispatch(
                                &session,
                                manager.as_ref(),
                                &mut state,
                                active_operation,
                                effect,
                            )
                            .await;
                            if let Some(switch) = switch {
                                match switch_session(
                                    manager.as_ref(),
                                    &mut attached,
                                    &mut session,
                                    &mut events,
                                    &mut resume_session,
                                    &mut state,
                                    &mut transcript,
                                    &mut active_operation,
                                    &palette,
                                    switch,
                                )
                                .await
                                {
                                    Ok(()) => {}
                                    Err(err) => {
                                        notice(
                                            &mut state,
                                            &format!("session switch failed: {err}"),
                                        );
                                    }
                                }
                            }
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
    let close_result = match attached {
        Some(attached) => match attached.close().await {
            Ok(()) => Ok(()),
            Err(err) => Err(err),
        },
        None => match session.close().await {
            Ok(()) | Err(CommandError::Closed) => Ok(()),
            Err(err) => Err(err.into()),
        },
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

/// Perform one session switch inside the run loop: close the attached
/// stack through the manager, adopt the next session, reset all live
/// presentation (transcript, drafts, status), and re-subscribe. Only
/// the loop owns these locals, so the swap is one synchronous step the
/// event stream cannot interleave.
#[allow(clippy::too_many_arguments)]
async fn switch_session(
    manager: Option<&crate::session_manager::SessionManager>,
    attached: &mut Option<crate::session_manager::AttachedSession>,
    session: &mut SessionHandle,
    events: &mut ion_core::EventSubscription,
    resume_session: &mut Option<ion_core::SessionId>,
    state: &mut UiState,
    transcript: &mut Transcript,
    active_operation: &mut Option<ion_core::OperationId>,
    palette: &render::Palette,
    switch: SessionSwitch,
) -> Result<(), RuntimeError> {
    let Some(manager) = manager else {
        return Ok(());
    };
    let Some(current) = attached.take() else {
        return Ok(());
    };
    let start = match switch {
        SessionSwitch::New => crate::session_manager::SessionStart::New,
        SessionSwitch::Resume(session) => crate::session_manager::SessionStart::Resume(session),
        SessionSwitch::Clone(target) => crate::session_manager::SessionStart::Resume(target),
    };
    let next = manager
        .switch(current, start)
        .await
        .map_err(|err| RuntimeError::OperationFailed(err.to_string()))?;

    // Adopt the new attachment and reset presentation wholesale.
    let new_session_id = next.session_id();
    let new_title = next.title().to_owned();
    let handle = next.handle();
    let (snapshot, fresh) = handle
        .subscribe()
        .await
        .map_err(|err| RuntimeError::OperationFailed(err.to_string()))?;
    *attached = Some(next);
    *session = handle;
    *events = fresh;
    *resume_session = Some(new_session_id);

    state.session_id = Some(new_session_id);
    state.session_title = Some(new_title.clone());
    // Presentation-only state that belongs to the previous session's
    // live operation: drafts, tool rows, status, approvals, history.
    state.reset_for_session_switch();

    transcript.clear();
    append_snapshot_entries(
        transcript,
        &snapshot.entries,
        snapshot.reopen_entry_count.unwrap_or(0),
        *resume_session,
        palette,
    );
    *active_operation = match &snapshot.operation {
        OperationStatus::Active { operation_id, .. } => Some(*operation_id),
        OperationStatus::Idle => None,
    };
    state.usage = snapshot.latest_usage;
    state.thinking_level = snapshot.thinking.clone();
    if matches!(snapshot.operation, OperationStatus::Idle) {
        state.surface_latest_settlement(snapshot.latest_settlement.as_ref());
    }
    state.surface_indeterminate_warning(snapshot.indeterminate.as_ref());
    Ok(())
}

/// A session switch the run loop must perform by rebuilding the runtime
/// attachment. Returned by `dispatch` when a session effect completes
/// successfully; the reducer has already accepted the presentation side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionSwitch {
    New,
    Resume(ion_core::SessionId),
    Clone(ion_core::SessionId),
}

/// Execute one reducer effect against the session; acceptance and
/// rejection return to the reducer as messages. Session-switch effects
/// are returned to the caller: only the run loop owns runtime
/// lifecycle, so it performs the switch and re-attaches.
async fn dispatch(
    session: &SessionHandle,
    manager: Option<&crate::session_manager::SessionManager>,
    state: &mut UiState,
    active_operation: Option<ion_core::OperationId>,
    effect: UiEffect,
) -> Option<SessionSwitch> {
    match effect {
        UiEffect::Quit => {
            state.quit_requested = true;
            None
        }
        UiEffect::ExternalEditor => {
            // Resolved by the run loop, which owns the terminal; this
            // arm exists only for match totality.
            None
        }
        UiEffect::RunShell {
            command,
            exclude_from_context,
        } => {
            match session.run_shell(command, exclude_from_context).await {
                Ok(_settlement) => {}
                Err(err) => notice(state, &format!("! failed: {err}")),
            }
            None
        }
        UiEffect::SwitchThinking { thinking } => {
            match session.switch_thinking(thinking.clone()).await {
                Ok(_previous) => {
                    let level = thinking.as_deref().unwrap_or("default");
                    notice(state, &format!("thinking: {level}"));
                }
                Err(err) => notice(state, &format!("thinking switch failed: {err}")),
            }
            None
        }
        UiEffect::DequeueNextRun => {
            match session.dequeue_next_run().await {
                Ok(prompt) => {
                    let (next, _) = update(std::mem::take(state), UiMessage::Dequeued(prompt));
                    *state = next;
                }
                Err(err) => notice(state, &format!("dequeue failed: {err}")),
            }
            None
        }
        UiEffect::Submit { text } => {
            match session.submit_if_idle(text).await {
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
            }
            None
        }
        UiEffect::Enqueue { text } => {
            match session.next_run(text).await {
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
            }
            None
        }
        UiEffect::Compact { instructions } => {
            match session.compact(instructions).await {
                Ok(true) => {
                    let (next, _) = update(std::mem::take(state), UiMessage::CompactAccepted);
                    *state = next;
                }
                Ok(false) => {
                    for line in [
                        "nothing to compact: compaction runs within an active operation",
                        "run /compact while a turn is running, or rely on automatic compaction near the model window",
                    ] {
                        notice(state, line);
                    }
                }
                Err(err) => notice(state, &format!("compact failed: {err}")),
            }
            None
        }
        UiEffect::SwitchModel { model } => {
            match session.switch_model(&model).await {
                Ok(previous) => {
                    let (provider, model_name) =
                        model_display_parts(&model, state.model_provider.as_deref());
                    state.model_provider = provider;
                    state.model_name = Some(model_name);
                    notice(state, &format!("model switched: {previous} -> {model}"));
                    let (next, _) = update(std::mem::take(state), UiMessage::ModelSwitchAccepted);
                    *state = next;
                }
                Err(err) => notice(state, &format!("model switch failed: {err}")),
            }
            None
        }
        UiEffect::Steer { text } => {
            match session.steer(text).await {
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
            }
            None
        }
        UiEffect::Approve | UiEffect::Deny => {
            let allow = matches!(effect, UiEffect::Approve);
            if let Some(operation_id) = active_operation {
                if let Err(err) = session.decide_approval(operation_id, allow).await {
                    notice(state, &format!("approval decision failed: {err}"));
                }
            } else {
                notice(state, "approval: no active operation");
            }
            None
        }
        UiEffect::Cancel => {
            // Esc during a user shell passthrough cancels the command
            // (pi parity); its settlement entry still lands durably.
            if matches!(
                &state.status,
                UiStatus::Working { operation } if operation == "shell"
            ) && active_operation.is_none()
            {
                match session.cancel_shell().await {
                    Ok(true) => {}
                    Ok(false) => {}
                    Err(err) => notice(state, &format!("! cancel failed: {err}")),
                }
                return None;
            }
            if let Some(operation_id) = active_operation
                && let Err(err) = session.cancel(operation_id).await
            {
                notice(state, &format!("cancel failed: {err}"));
            }
            None
        }
        UiEffect::RequestSessionList => {
            let Some(manager) = manager else {
                notice(state, "session switching is unavailable in this host");
                return None;
            };
            match manager.list().await {
                Ok(summaries) => {
                    // The picker never offers the attached session: resuming
                    // it is a no-op with confusing UX.
                    let summaries = summaries
                        .into_iter()
                        .filter(|summary| Some(summary.id) != state.session_id)
                        .collect::<Vec<_>>();
                    let (next, _) =
                        update(std::mem::take(state), UiMessage::SessionListed(summaries));
                    *state = next;
                }
                Err(err) => notice(state, &format!("session list failed: {err}")),
            }
            None
        }
        UiEffect::NewSession => Some(SessionSwitch::New),
        UiEffect::ResumeSession { session } => Some(SessionSwitch::Resume(session)),
        UiEffect::CloneSession => {
            let Some(manager) = manager else {
                notice(state, "session switching is unavailable in this host");
                return None;
            };
            let Some(source) = state.session_id else {
                notice(state, "no session is attached");
                return None;
            };
            let title = state.session_title.clone().unwrap_or_default();
            match manager
                .clone_session(source, &format!("{title} (clone)"))
                .await
            {
                Ok(target) => Some(SessionSwitch::Clone(target)),
                Err(err) => {
                    notice(state, &format!("clone failed: {err}"));
                    None
                }
            }
        }
        UiEffect::RenameSession { title } => {
            let Some(manager) = manager else {
                notice(state, "session switching is unavailable in this host");
                return None;
            };
            let Some(session) = state.session_id else {
                notice(state, "no session is attached");
                return None;
            };
            match manager.rename(session, &title).await {
                Ok(()) => {
                    state.session_title = Some(title);
                    let (next, _) = update(std::mem::take(state), UiMessage::RenameAccepted);
                    *state = next;
                }
                Err(err) => notice(state, &format!("rename failed: {err}")),
            }
            None
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
    fn multiline_composer_moves_vertically_with_a_sticky_column() {
        let mut state = type_text(UiState::new(), "one");
        state = update(state, ctrl('j')).0;
        state = type_text(state, "longer");
        state = update(state, ctrl('j')).0;
        state = type_text(state, "x");
        assert_eq!(state.composer, "one\nlonger\nx");

        state = update(state, key(KeyCode::Up)).0;
        for _ in 0..5 {
            state = update(state, key(KeyCode::Right)).0;
        }
        state = update(state, key(KeyCode::Down)).0;
        assert_eq!(state.cursor, state.composer.chars().count());
        state = update(state, key(KeyCode::Up)).0;
        assert_eq!(state.cursor, 10);
        state = update(state, key(KeyCode::Up)).0;
        assert_eq!(state.cursor, 3);
    }

    #[test]
    fn wrapped_vertical_motion_uses_visual_rows() {
        let mut state = type_text(UiState::new(), "abcdefghijklmn");
        state.set_terminal_width(8);
        state = update(state, ctrl('j')).0;
        state = type_text(state, "z");

        // The first logical line occupies three aligned visual rows at
        // width 8; Up moves to its continuation row rather than skipping
        // the wrap.
        state = update(state, key(KeyCode::Up)).0;
        assert_eq!(state.cursor, 13);
        state = update(state, key(KeyCode::Down)).0;
        assert_eq!(state.cursor, 16);
    }

    #[test]
    fn wrapped_vertical_motion_uses_display_width_for_wide_graphemes() {
        let mut state = type_text(UiState::new(), "界x");
        state.set_terminal_width(20);
        state = update(state, ctrl('j')).0;
        state = type_text(state, "1234");

        // Move to the first line, place the cursor after the wide grapheme,
        // then move down. Display column 2 lands after two ASCII characters;
        // a character-count column would land after only one.
        state = update(state, key(KeyCode::Up)).0;
        state = update(state, key(KeyCode::Left)).0;
        state = update(state, key(KeyCode::Down)).0;
        assert_eq!(state.cursor, 5);
    }

    #[test]
    fn wrapped_vertical_motion_retains_display_column_across_short_rows() {
        let mut state = type_text(UiState::new(), "abcdefghijklmn");
        state.set_terminal_width(8);
        state = update(state, ctrl('j')).0;
        state = type_text(state, "xy");

        // The continuation row has more text than the short next line. The
        // preferred column survives the round trip and returns to the end.
        state = update(state, key(KeyCode::Up)).0;
        assert_eq!(state.cursor, 14);
        state = update(state, key(KeyCode::Down)).0;
        assert_eq!(state.cursor, 17);
    }

    #[test]
    fn empty_question_opens_a_local_hotkey_view() {
        let state = UiState::new();
        let (state, effect) = update(state, key(KeyCode::Char('?')));
        assert!(effect.is_none());
        assert!(state.hotkeys_visible);
        assert!(state.composer.is_empty());

        let (state, effect) = update(state, key(KeyCode::Char('?')));
        assert!(effect.is_none());
        assert!(!state.hotkeys_visible);
    }

    #[test]
    fn question_remains_text_when_composer_is_nonempty_or_working() {
        let state = type_text(UiState::new(), "ask");
        let state = update(state, key(KeyCode::Char('?'))).0;
        assert_eq!(state.composer, "ask?");
        assert!(!state.hotkeys_visible);

        let mut state = UiState::new();
        state.status = UiStatus::Working {
            operation: "thinking".to_owned(),
        };
        let state = update(state, key(KeyCode::Char('?'))).0;
        assert_eq!(state.composer, "?");
        assert!(!state.hotkeys_visible);
    }

    #[test]
    fn hotkey_view_captures_edits_until_closed() {
        let state = update(UiState::new(), key(KeyCode::Char('?'))).0;
        let state = update(state, key(KeyCode::Char('x'))).0;
        assert!(state.composer.is_empty());
        assert!(state.hotkeys_visible);
        let state = update(state, key(KeyCode::Esc)).0;
        assert!(!state.hotkeys_visible);
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
        let state = update(state, UiMessage::SubmitAccepted).0;
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
        let state = update(state, UiMessage::SubmitAccepted).0;
        assert_eq!(state.composer.as_str(), "");
        let state = update(state, key(KeyCode::Up)).0;
        assert_eq!(state.composer.as_str(), "one");
    }

    #[test]
    fn tab_completes_only_safe_commands_and_catalog_models() {
        let state = type_text(UiState::new(), "/he");
        let state = update(state, key(KeyCode::Tab)).0;
        assert_eq!(state.composer, "/help ");

        let mut state = type_text(UiState::new(), "/model b");
        state.model_catalog = vec!["alpha".to_owned(), "beta".to_owned()];
        let state = update(state, key(KeyCode::Tab)).0;
        assert_eq!(state.composer, "/model beta");

        let state = type_text(UiState::new(), "ordinary prose");
        let state = update(state, key(KeyCode::Tab)).0;
        assert_eq!(state.composer, "ordinary prose");

        let state = type_text(UiState::new(), "/");
        let state = update(state, key(KeyCode::Tab)).0;
        assert_eq!(state.composer, "/");
        // Every registered command is offered (Pi parity surface).
        assert_eq!(state.pending_scrollback.len(), 10);
    }

    #[test]
    fn completion_considers_all_model_candidates_before_display_limit() {
        let mut state = UiState::new();
        state.model_catalog = (0..16)
            .map(|index| format!("openrouter/shared-{index}"))
            .chain(std::iter::once("openrouter/other".to_owned()))
            .collect();
        let state = type_text(state, "/model ");
        let state = update(state, key(KeyCode::Tab)).0;
        assert_eq!(state.composer, "/model openrouter/");
        assert_eq!(state.pending_scrollback.len(), 0);
    }

    #[test]
    fn model_command_opens_selector_and_arrow_enter_selects() {
        let mut state = UiState::new();
        state.model_switching_available = true;
        state.model_provider = Some("openrouter".to_owned());
        state.model_name = Some("alpha".to_owned());
        state.model_catalog = vec!["openrouter/alpha".to_owned(), "openrouter/beta".to_owned()];
        let (state, effect) = handle_command(&mut state, "model");
        assert!(effect.is_none());
        assert!(state.model_selector.is_some());
        assert!(state.composer.is_empty());

        let state = update(state, key(KeyCode::Down)).0;
        let (state, effect) = update(state, key(KeyCode::Enter));
        assert_eq!(
            effect,
            Some(UiEffect::SwitchModel {
                model: "openrouter/beta".to_owned()
            })
        );
        assert!(state.model_selector.is_none());
        assert!(state.composer.is_empty());
    }

    #[test]
    fn model_shortcuts_open_and_cycle_the_selector_catalog() {
        let mut state = UiState::new();
        state.model_switching_available = true;
        state.model_provider = Some("openrouter".to_owned());
        state.model_name = Some("alpha".to_owned());
        state.model_catalog = vec![
            "openrouter/alpha".to_owned(),
            "openrouter/beta".to_owned(),
            "desktop/qwen3.8:27b".to_owned(),
        ];

        let (state, effect) = update(state, ctrl('p'));
        assert_eq!(
            effect,
            Some(UiEffect::SwitchModel {
                model: "openrouter/beta".to_owned()
            })
        );
        assert!(state.model_selector.is_none());

        let (state, effect) = update(state, ctrl('l'));
        assert!(effect.is_none());
        assert!(state.model_selector.is_some());
        let (state, effect) = update(state, key(KeyCode::Down));
        assert!(effect.is_none());
        let (state, effect) = update(state, key(KeyCode::Down));
        assert!(effect.is_none());
        let (_state, effect) = update(state, key(KeyCode::Enter));
        assert_eq!(
            effect,
            Some(UiEffect::SwitchModel {
                model: "desktop/qwen3.8:27b".to_owned()
            })
        );
    }

    #[test]
    fn qualified_model_display_stays_single_provider_and_id() {
        assert_eq!(
            model_display_parts("openrouter/z-ai/glm-5.3-flash", Some("desktop")),
            (
                Some("openrouter".to_owned()),
                "z-ai/glm-5.3-flash".to_owned()
            )
        );
        assert_eq!(
            model_display_parts("local-model", Some("desktop")),
            (Some("desktop".to_owned()), "local-model".to_owned())
        );
    }

    #[test]
    fn model_switch_keeps_an_unsubmitted_draft() {
        let mut state = UiState::new();
        state.model_switching_available = true;
        state.model_provider = Some("desktop".to_owned());
        state.model_name = Some("test".to_owned());
        state.model_catalog = vec!["desktop/test".to_owned(), "desktop/next".to_owned()];
        state.composer = "keep this".to_owned();
        state.cursor = state.composer.chars().count();

        let (state, effect) = update(state, ctrl('p'));
        assert!(matches!(effect, Some(UiEffect::SwitchModel { .. })));
        let state = update(state, UiMessage::ModelSwitchAccepted).0;
        assert_eq!(state.composer, "keep this");
        assert_eq!(state.cursor, 9);
    }

    #[test]
    fn model_selector_render_keeps_title_and_navigation_hint_visible() {
        let mut state = UiState::new();
        state.model_switching_available = true;
        state.model_catalog = vec![
            "desktop/test".to_owned(),
            "desktop/next".to_owned(),
            "desktop/third".to_owned(),
        ];
        let state = update(state, ctrl('l')).0;
        let (lines, _) = build_live(&state, &palette(Theme::Dark), 100);
        let rendered = lines
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("select model"), "{rendered}");
        assert!(rendered.contains("desktop/test"), "{rendered}");
        assert!(rendered.contains("enter"), "{rendered}");
    }

    #[test]
    fn model_selector_stays_usable_in_a_four_row_terminal() {
        let mut state = UiState::new();
        state.model_switching_available = true;
        state.model_catalog = vec!["desktop/test".to_owned(), "desktop/next".to_owned()];
        let state = update(state, ctrl('l')).0;
        let (lines, cursor) = render::build_live_at_height(&state, &palette(Theme::Dark), 40, 4);
        assert!(cursor.is_some());
        assert!(lines.len() <= 4);
        let rendered = lines
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("desktop/test"), "{rendered}");
        assert!(rendered.contains("enter"), "{rendered}");
    }

    #[test]
    fn model_selector_truncates_long_rows_without_losing_controls() {
        let mut state = UiState::new();
        state.model_switching_available = true;
        state.model_catalog = vec![
            "openrouter/provider-with-a-very-long-model-name".to_owned(),
            "openrouter/another-long-model-name".to_owned(),
        ];
        let state = update(state, ctrl('l')).0;
        let (lines, _) = build_live(&state, &palette(Theme::Dark), 24);
        assert!(lines.len() <= render::LIVE_REGION_MAX_ROWS);
        let rendered = lines
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("select model"), "{rendered}");
        assert!(rendered.contains("enter"), "{rendered}");
    }

    #[test]
    fn numeric_model_command_selects_catalog_entry() {
        let mut state = UiState::new();
        state.model_switching_available = true;
        state.model_catalog = vec!["openrouter/alpha".to_owned(), "openrouter/beta".to_owned()];
        let (state, effect) = handle_command(&mut state, "model 2");
        assert_eq!(
            effect,
            Some(UiEffect::SwitchModel {
                model: "openrouter/beta".to_owned()
            })
        );
        assert!(state.model_selector.is_none());
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
        let ctrl_underscore = KeyEvent::new(KeyCode::Char('_'), Modifiers::CONTROL);
        assert_eq!(map.action_for(&ctrl_underscore), Some(Action::Undo));
        let ctrl_j = KeyEvent::new(KeyCode::Char('j'), Modifiers::CONTROL);
        assert_eq!(map.action_for(&ctrl_j), Some(Action::InsertNewline));
        let ctrl_l = KeyEvent::new(KeyCode::Char('l'), Modifiers::CONTROL);
        assert_eq!(map.action_for(&ctrl_l), Some(Action::OpenModelSelector));
        let ctrl_p = KeyEvent::new(KeyCode::Char('p'), Modifiers::CONTROL);
        assert_eq!(map.action_for(&ctrl_p), Some(Action::CycleModelForward));
        let reverse_p = KeyEvent::new(KeyCode::Char('p'), Modifiers::CONTROL | Modifiers::SHIFT);
        assert_eq!(map.action_for(&reverse_p), Some(Action::CycleModelBackward));
        assert_eq!(
            map.action_for(&KeyEvent::new(KeyCode::Tab, Modifiers::NONE)),
            Some(Action::Complete)
        );
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
            latest_settlement: None,
            reopen_entry_count: None,
            operation: OperationStatus::Active {
                operation_id: OperationId::generate(),
                prompt: "do things".to_owned(),
                state: ion_core::OperationState::NeedAssistant,
            },
            entries: Vec::new(),
            model_ref: "test-model".to_owned(),
            thinking: None,
            pending_next_run: None,
            latest_usage: Some(TokenUsage {
                input: 100,
                output: 20,
                cache_read: 60,
                cache_write: 4,
            }),
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
        assert_eq!(
            state.usage,
            Some(TokenUsage {
                input: 100,
                output: 20,
                cache_read: 60,
                cache_write: 4,
            })
        );
        assert_eq!(state.draft_thinking, "reasoning so far");
        assert!(!state.draft_degraded);
        assert_eq!(state.tool_rows.len(), 1);
        assert_eq!(state.tool_rows[0].tool, "read");
        assert_eq!(state.tool_rows[0].target.as_deref(), Some("Cargo.toml"));
    }

    #[test]
    fn resync_after_lag_resurfaces_indeterminate_snapshot_warning() {
        let operation_id = OperationId::generate();
        let snapshot = SessionSnapshot {
            cursor: RuntimeCursor::default(),
            runtime_instance_id: RuntimeInstanceId::generate(),
            indeterminate: Some(ion_core::IndeterminateWarning {
                operation_id,
                message: "inspect it before retrying".to_owned(),
            }),
            latest_settlement: Some(OperationSettlement {
                operation_id,
                outcome: OperationOutcome::Indeterminate,
            }),
            reopen_entry_count: None,
            operation: OperationStatus::Idle,
            entries: Vec::new(),
            model_ref: "test-model".to_owned(),
            thinking: None,
            pending_next_run: None,
            latest_usage: None,
            live: None,
        };
        let mut state = UiState::new();
        state.resync_after_lag(&snapshot);

        let rendered = state
            .pending_scrollback
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(rendered.contains(&operation_id.to_string()));
        assert!(rendered.contains("inspect it before retrying"));
    }

    #[test]
    fn resync_after_lag_does_not_resurface_previous_settlement_while_active() {
        let previous = OperationId::generate();
        let current = OperationId::generate();
        let snapshot = SessionSnapshot {
            cursor: RuntimeCursor::default(),
            runtime_instance_id: RuntimeInstanceId::generate(),
            indeterminate: None,
            latest_settlement: Some(OperationSettlement {
                operation_id: previous,
                outcome: OperationOutcome::Failed("old failure".to_owned()),
            }),
            reopen_entry_count: None,
            operation: OperationStatus::Active {
                operation_id: current,
                prompt: "new work".to_owned(),
                state: OperationState::NeedAssistant,
            },
            entries: Vec::new(),
            model_ref: "test-model".to_owned(),
            thinking: None,
            pending_next_run: None,
            latest_usage: None,
            live: None,
        };
        let mut state = UiState::new();
        state.resync_after_lag(&snapshot);
        let rendered = state
            .pending_scrollback
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(!rendered.contains("old failure"));
        assert_eq!(
            state.status,
            UiStatus::Working {
                operation: "working: new work".to_owned(),
            }
        );
    }

    #[test]
    fn resync_after_lag_resurfaces_failed_settlement() {
        let operation_id = OperationId::generate();
        let snapshot = SessionSnapshot {
            cursor: RuntimeCursor::default(),
            runtime_instance_id: RuntimeInstanceId::generate(),
            indeterminate: None,
            latest_settlement: Some(OperationSettlement {
                operation_id,
                outcome: OperationOutcome::Failed("provider failed".to_owned()),
            }),
            reopen_entry_count: None,
            operation: OperationStatus::Idle,
            entries: Vec::new(),
            model_ref: "test-model".to_owned(),
            thinking: None,
            pending_next_run: None,
            latest_usage: None,
            live: None,
        };
        let mut state = UiState::new();
        state.resync_after_lag(&snapshot);
        let rendered = state
            .pending_scrollback
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(rendered.contains("! failed: provider failed"));
    }

    #[test]
    fn resync_after_lag_on_idle_clears_partial_draft() {
        let mut state = UiState::new();
        state.draft = "partial".to_owned();
        let snapshot = SessionSnapshot {
            cursor: RuntimeCursor::default(),
            runtime_instance_id: RuntimeInstanceId::generate(),
            indeterminate: None,
            latest_settlement: None,
            reopen_entry_count: None,
            operation: OperationStatus::Idle,
            entries: Vec::new(),
            model_ref: "test-model".to_owned(),
            thinking: None,
            pending_next_run: None,
            latest_usage: None,
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
            latest_settlement: None,
            reopen_entry_count: None,
            operation: OperationStatus::Active {
                operation_id,
                prompt: "do things".to_owned(),
                state: ion_core::OperationState::NeedAssistant,
            },
            entries: Vec::new(),
            model_ref: "test-model".to_owned(),
            thinking: None,
            pending_next_run: None,
            latest_usage: None,
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
            latest_settlement: None,
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
            thinking: None,
            pending_next_run: None,
            latest_usage: None,
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
    fn rejected_commands_preserve_the_draft_until_retry_or_clear() {
        let state = type_text(UiState::new(), "draft");
        let (state, effect) = update(state, key(KeyCode::Enter));
        assert_eq!(
            effect,
            Some(UiEffect::Submit {
                text: "draft".to_owned()
            })
        );
        assert_eq!(state.composer, "draft");

        let (state, _) = update(state, UiMessage::SubmitRejected("busy".to_owned()));
        assert_eq!(state.composer, "draft");
        assert!(state.history.is_empty());
        let (state, _) = update(state, UiMessage::SubmitAccepted);
        assert!(state.composer.is_empty());

        let mut state = type_text(UiState::new(), "queued");
        state.status = UiStatus::Working {
            operation: "thinking".to_owned(),
        };
        let (state, effect) = update(state, key(KeyCode::Enter));
        assert!(matches!(effect, Some(UiEffect::Enqueue { .. })));
        let (state, _) = update(state, UiMessage::EnqueueRejected("closed".to_owned()));
        assert_eq!(state.composer, "queued");

        let mut state = type_text(UiState::new(), "steer");
        state.status = UiStatus::Working {
            operation: "thinking".to_owned(),
        };
        let (state, effect) = update(
            state,
            UiMessage::Key(KeyEvent::new(KeyCode::Enter, Modifiers::SHIFT)),
        );
        assert!(matches!(effect, Some(UiEffect::Steer { .. })));
        let (state, _) = update(state, UiMessage::SteerRejected("closed".to_owned()));
        assert_eq!(state.composer, "steer");
    }

    #[test]
    fn undo_coalesces_typing_and_deletion() {
        let state = type_text(UiState::new(), "hello");
        let state = update(state, ctrl('_')).0;
        assert!(state.composer.is_empty());

        let state = type_text(UiState::new(), "hello");
        let state = update(
            state,
            UiMessage::Key(KeyEvent::new(KeyCode::Backspace, Modifiers::NONE)),
        )
        .0;
        let state = update(
            state,
            UiMessage::Key(KeyEvent::new(KeyCode::Backspace, Modifiers::NONE)),
        )
        .0;
        let state = update(state, ctrl('_')).0;
        assert_eq!(state.composer, "hello");
        assert_eq!(state.cursor, 5);
    }

    #[test]
    fn paste_is_one_undo_unit_and_is_swallowed_for_approval() {
        let (state, _) = update(UiState::new(), UiMessage::Paste("one\ntwo".to_owned()));
        assert_eq!(state.composer, "one\ntwo");
        let state = update(state, ctrl('_')).0;
        assert!(state.composer.is_empty());

        let mut state = UiState::new();
        state.approval = Some(ApprovalPrompt {
            tool: "bash".to_owned(),
            target: None,
        });
        let (state, _) = update(state, UiMessage::Paste("must not enter".to_owned()));
        assert!(state.composer.is_empty());
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
    fn usage_event_reaches_the_footer_without_inventing_cost() {
        let operation_id = OperationId::generate();
        let state = apply_runtime_event(
            UiState::new(),
            RuntimeEvent::UsageUpdate {
                cursor: Default::default(),
                operation_id,
                usage: TokenUsage {
                    input: 100,
                    output: 20,
                    cache_read: 60,
                    cache_write: 4,
                },
            },
        );
        let (lines, _) = build_live(&state, &palette(Theme::Dark), 120);
        let text: Vec<String> = lines.iter().map(|line| line.to_string()).collect();
        assert!(
            text.iter()
                .any(|line| line.contains("ctx 184 · in 100 · out 20 · cache 60/4")),
            "{text:?}"
        );
        assert!(
            text.iter().all(|line| !line.contains("cost")),
            "cost must stay absent until a provider pricing contract exists: {text:?}"
        );
    }

    #[test]
    fn narrow_footer_keeps_usage_and_provider_separated() {
        // Found live in the 2026-09-01 dogfood: at ~60 columns the
        // usage label and the right-aligned provider/model ran
        // together with no separator because the padding saturated
        // to zero. The footer must keep a visible boundary (drop to
        // an ellipsis) rather than gluing the two segments.
        let operation_id = OperationId::generate();
        let mut state = apply_runtime_event(
            UiState::new(),
            RuntimeEvent::UsageUpdate {
                cursor: Default::default(),
                operation_id,
                usage: TokenUsage {
                    input: 100,
                    output: 20,
                    cache_read: 60,
                    cache_write: 4,
                },
            },
        );
        state.model_name = Some("z-ai/glm-5.3-flash".to_owned());
        state.model_provider = Some("openrouter".to_owned());
        let (lines, _) = build_live(&state, &palette(Theme::Dark), 60);
        let text: Vec<String> = lines.iter().map(|line| line.to_string()).collect();
        let footer = text
            .iter()
            .find(|line| line.contains("ctx 184"))
            .expect("usage footer line");
        assert!(
            footer.contains("\u{2026}"),
            "narrow footer must elide instead of gluing segments: {footer:?}"
        );
        assert!(
            !footer.ends_with(' ') && footer.chars().count() <= 60,
            "footer must not exceed the width or trail spaces: {footer:?}"
        );
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
    fn multiline_composer_renders_each_logical_line_and_cursor() {
        let mut state = UiState::new();
        state.composer = "one\nlonger".to_owned();
        state.cursor = state.composer.chars().count();
        let (lines, cursor) = build_live(&state, &palette(Theme::Dark), 40);
        let text: Vec<String> = lines.iter().map(|line| line.to_string()).collect();
        assert!(text.iter().any(|line| line == "> one"), "{text:?}");
        let (row, col) = cursor.expect("cursor");
        assert_eq!(text[row], "  longer");
        assert_eq!(col as usize, 2 + "longer".width());
    }

    #[test]
    fn live_region_keeps_a_blank_row_above_the_composer() {
        let mut state = UiState::new();
        state.composer = "hello".to_owned();
        state.cursor = state.composer.chars().count();
        let (lines, cursor) = build_live(&state, &palette(Theme::Dark), 80);
        assert_eq!(lines.len(), 6);
        assert!(lines[0].to_string().trim().is_empty());
        assert!(lines[1].to_string().starts_with('─'));
        assert_eq!(lines[2].to_string(), "> hello");
        assert_eq!(cursor, Some((2, 7)));
    }

    #[test]
    fn wrapped_composer_continuations_are_indented() {
        let mut state = UiState::new();
        state.composer = "abcdefghijklmn".to_owned();
        state.cursor = state.composer.chars().count();
        let (lines, _) = build_live(&state, &palette(Theme::Dark), 10);
        assert_eq!(lines[2].to_string(), "> abcdefgh");
        assert_eq!(lines[3].to_string(), "  ijklmn");
    }

    #[test]
    fn live_region_keeps_a_fixed_height_as_content_changes() {
        let mut state = UiState::new();
        for text in ["", "one", "one\ntwo\nthree\nfour"] {
            state.composer = text.to_owned();
            state.cursor = text.chars().count();
            let (lines, _) = build_live(&state, &palette(Theme::Dark), 80);
            assert!(lines.len() <= 10);
        }
    }

    #[test]
    fn cancelled_partial_model_output_is_not_completed_assistant_text() {
        let operation_id = OperationId::generate();
        let mut state = apply_runtime_event(
            UiState::new(),
            RuntimeEvent::AssistantTextDelta {
                cursor: RuntimeCursor::default(),
                operation_id,
                text: "partial answer".to_owned(),
            },
        );
        state = apply_runtime_event(
            state,
            RuntimeEvent::OperationCancelled {
                cursor: RuntimeCursor::default(),
                operation_id,
            },
        );
        let rendered = state
            .pending_scrollback
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!rendered.contains("partial answer"));
        assert!(rendered.contains("partial model output discarded"));
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
    fn running_tool_progress_is_visible_when_expanded() {
        let state = started(UiState::new());
        let state = update(
            state,
            UiMessage::Runtime(RuntimeEvent::ToolProgress {
                cursor: RuntimeCursor::default(),
                operation_id: OperationId::generate(),
                call_id: 1,
                output: "child session started".to_owned(),
            }),
        )
        .0;
        let state = update(state, ctrl('o')).0;
        let (lines, _) = build_live(&state, &palette(Theme::Dark), 80);
        assert!(
            lines
                .iter()
                .any(|line| line.to_string().contains("child session started")),
            "expanded running progress should render"
        );
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

#[cfg(test)]
mod session_command_tests {
    use super::tests::key;
    use super::*;

    fn command(state: UiState, input: &str) -> (UiState, Option<UiEffect>) {
        let mut state = state;
        handle_command(&mut state, input)
    }

    #[test]
    fn session_commands_are_dispatched_with_busy_and_approval_guards() {
        // /new, /resume, /clone refuse while working or an approval waits.
        let mut state = UiState::new();
        state.status = UiStatus::Working {
            operation: "running bash".to_owned(),
        };
        let (state, effect) = command(state, "new");
        assert!(effect.is_none());
        let (state, _) = command(state, "resume");
        assert!(!state.pending_scrollback.is_empty(), "busy guard notices");
        let (_state, effect) = command(state, "clone");
        assert!(effect.is_none());

        let mut state = UiState::new();
        state.approval = Some(ApprovalPrompt {
            tool: "bash".to_owned(),
            target: None,
        });
        let (state, effect) = command(state, "new");
        assert!(effect.is_none());
        let (state, _) = command(state, "resume");
        assert!(!state.pending_scrollback.is_empty());
        let (_state, effect) = command(state, "clone");
        assert!(effect.is_none());
    }

    #[test]
    fn new_and_clone_return_session_switch_effects() {
        let mut state = UiState::new();
        state.session_id = Some(
            ion_core::SessionId::parse("01928fa1-0000-7000-8000-000000000001").expect("session id"),
        );
        state.session_title = Some("root session".to_owned());
        let (state, effect) = command(state, "new");
        assert_eq!(effect, Some(UiEffect::NewSession));
        let (_state, effect) = command(state, "clone");
        assert_eq!(effect, Some(UiEffect::CloneSession));
    }

    #[test]
    fn resume_requests_the_session_list_and_opens_the_picker_with_rows() {
        let mut state = UiState::new();
        state.session_id = Some(
            ion_core::SessionId::parse("01928fa1-0000-7000-8000-000000000001").expect("session id"),
        );
        let (state, effect) = command(state, "resume");
        assert_eq!(effect, Some(UiEffect::RequestSessionList));
        assert!(state.session_selector.is_none(), "picker waits for rows");

        // The host delivers only other sessions; the reducer opens the
        // picker and preserves the query.
        let summaries = vec![ion_core::SessionSummary {
            id: ion_core::SessionId::parse("01928fa1-0000-7000-8000-000000000002").expect("id"),
            title: "debug parser".to_owned(),
            updated_at: 0,
            entry_count: 4,
        }];
        let (state, _) = update(state, UiMessage::SessionListed(summaries));
        assert!(state.session_selector.is_some());
        assert_eq!(state.filtered_session_rows().len(), 1);

        // Arrow + enter selects the durable id; escape restores the draft.
        let (state, _) = update(state, key(KeyCode::Down));
        let (state, effect) = update(state, key(KeyCode::Enter));
        let session = ion_core::SessionId::parse("01928fa1-0000-7000-8000-000000000002")
            .expect("selected session");
        assert_eq!(effect, Some(UiEffect::ResumeSession { session }));
        assert!(state.session_selector.is_none());

        let mut state = UiState::new();
        state.composer = "draft survives".to_owned();
        state.cursor = state.composer.chars().count();
        let (state, _) = command(state, "resume");
        let (state, _) = update(
            state,
            UiMessage::SessionListed(vec![ion_core::SessionSummary {
                id: ion_core::SessionId::parse("01928fa1-0000-7000-8000-000000000002").expect("id"),
                title: "other".to_owned(),
                updated_at: 0,
                entry_count: 1,
            }]),
        );
        let (state, _) = update(state, key(KeyCode::Esc));
        assert_eq!(state.composer, "draft survives");
        assert!(state.session_selector.is_none());
    }

    #[test]
    fn name_and_session_commands_rename_and_display_identity() {
        let mut state = UiState::new();
        state.session_id = Some(
            ion_core::SessionId::parse("01928fa1-0000-7000-8000-000000000003").expect("session id"),
        );
        state.session_title = Some("old".to_owned());
        let (state, effect) = command(state, "name  deep dive  ");
        assert_eq!(
            effect,
            Some(UiEffect::RenameSession {
                title: "deep dive".to_owned(),
            })
        );
        let (_state, effect) = command(state, "name");
        assert_eq!(effect, None);

        let mut state = UiState::new();
        state.session_id = Some(
            ion_core::SessionId::parse("01928fa1-0000-7000-8000-000000000003").expect("session id"),
        );
        state.session_title = Some("titled".to_owned());
        let (state, _) = command(state, "session");
        let rendered = state
            .pending_scrollback
            .iter()
            .map(ToString::to_string)
            .collect::<String>();
        assert!(rendered.contains("titled"), "{rendered}");
    }

    #[test]
    fn quit_command_requests_quit() {
        let (state, effect) = command(UiState::new(), "quit");
        assert_eq!(effect, Some(UiEffect::Quit));
        assert!(!state.quit_requested, "dispatch sets quit, not the reducer");
    }

    #[test]
    fn switch_reset_clears_live_state_but_keeps_display_config() {
        let mut state = UiState::new();
        state.draft = "partial output".to_owned();
        state.draft_thinking = "thinking".to_owned();
        state.tool_rows.push(ToolRow {
            tool: "bash".to_owned(),
            target: Some("cargo build".to_owned()),
            state: ToolState::Running,
            progress: None,
            preview: None,
        });
        state.status = UiStatus::Working {
            operation: "running".to_owned(),
        };
        state.model_catalog = vec!["provider/model".to_owned()];
        state.model_switching_available = true;
        state.reset_for_session_switch();
        assert!(state.draft.is_empty());
        assert!(state.tool_rows.is_empty());
        assert_eq!(state.status, UiStatus::Idle);
        assert_eq!(state.model_catalog.len(), 1, "display config survives");
        assert!(state.model_switching_available);
    }

    #[test]
    fn picker_row_renders_title_and_recency() {
        let summary = ion_core::SessionSummary {
            id: ion_core::SessionId::parse("01928fa1-0000-7000-8000-000000000004").expect("id"),
            title: String::new(),
            updated_at: 0,
            entry_count: 0,
        };
        let row = session_row_for_picker(&summary);
        assert!(row.label.contains("unknown age"), "{}", row.label);
        assert!(row.label.ends_with("empty"), "{}", row.label);
    }
}

#[cfg(test)]
mod editor_parity_tests {
    use super::tests::{ctrl, key, type_text};
    use super::*;
    use ion_terminal::Modifiers;

    fn word_key(code: KeyCode, modifiers: Modifiers) -> UiMessage {
        UiMessage::Key(KeyEvent::new(code, modifiers))
    }

    #[test]
    fn word_motion_moves_by_words_in_both_directions() {
        let state = type_text(UiState::new(), "one two three");
        // Cursor at 13 (end). alt+left → start of "three" (8).
        let (state, _) = update(state, word_key(KeyCode::Left, Modifiers::ALT));
        assert_eq!(state.cursor, 8, "alt+left lands on the word start");
        // alt+right from inside "three" → its end (13).
        let (state, _) = update(state, word_key(KeyCode::Right, Modifiers::ALT));
        assert_eq!(state.cursor, 13);
        // ctrl+left from 13 → 8 ("three" start), again → 4 ("two" start),
        // a third time → 0 ("one" start).
        let (state, _) = update(state, word_key(KeyCode::Left, Modifiers::CONTROL));
        assert_eq!(state.cursor, 8);
        let (state, _) = update(state, word_key(KeyCode::Left, Modifiers::CONTROL));
        assert_eq!(state.cursor, 4);
        let (state, _) = update(state, word_key(KeyCode::Left, Modifiers::CONTROL));
        assert_eq!(state.cursor, 0);
        // alt+b/alt+f behave the same.
        let (state, _) = update(state, word_key(KeyCode::Char('f'), Modifiers::ALT));
        assert_eq!(state.cursor, 3, "alt+f moves to the word end");
        let (state, _) = update(state, word_key(KeyCode::Char('b'), Modifiers::ALT));
        assert_eq!(state.cursor, 0, "alt+b moves to the word start");
    }

    #[test]
    fn word_motion_skips_whitespace_gaps() {
        let state = type_text(UiState::new(), "alpha   beta");
        // Cursor at 5 (inside the gap). alt+right → end of "beta" (12).
        let mut state = state;
        state.cursor = 5;
        let (state, _) = update(state, word_key(KeyCode::Right, Modifiers::ALT));
        assert_eq!(state.cursor, 12);
        // Backward from "beta"'s end lands on its start (8); a second
        // alt+left crosses the gap to "alpha"'s start (0).
        let (state, _) = update(state, word_key(KeyCode::Left, Modifiers::ALT));
        assert_eq!(state.cursor, 8, "alt+left lands on the word start");
        let (state, _) = update(state, word_key(KeyCode::Left, Modifiers::ALT));
        assert_eq!(state.cursor, 0, "alt+left across the gap");
    }

    #[test]
    fn kill_word_forward_removes_the_next_word() {
        let state = type_text(UiState::new(), "keep drop this");
        let mut state = state;
        state.cursor = 5;
        let (state, _) = update(state, word_key(KeyCode::Char('d'), Modifiers::ALT));
        assert_eq!(state.composer, "keep  this");
        assert_eq!(state.kill_buffer, "drop");
    }

    #[test]
    fn kill_ring_yank_pop_cycles_older_kills() {
        // Kill "two" (ctrl+w), then kill the rest of the line (ctrl+k);
        // the ring keeps both and yank-pop cycles them.
        let mut state = type_text(UiState::new(), "one two");
        state.cursor = 7;
        let (state, _) = update(state, ctrl('w'));
        assert_eq!(state.kill_buffer, "two");
        // Home first so ctrl+k removes the remaining word.
        let (state, _) = update(state, key(KeyCode::Home));
        let (state, _) = update(state, ctrl('k'));
        assert_eq!(state.kill_buffer, "one ");
        assert_eq!(state.kill_ring, vec!["two".to_owned()]);

        // Yank inserts the head; yank-pop replaces it with the older kill.
        let (state, _) = update(state, ctrl('y'));
        assert_eq!(state.composer, "one ");
        let (state, _) = update(state, word_key(KeyCode::Char('y'), Modifiers::ALT));
        assert_eq!(
            state.composer, "two",
            "yank-pop replaced the head with the older kill"
        );
        // A second pop returns to the head.
        let (state, _) = update(state, word_key(KeyCode::Char('y'), Modifiers::ALT));
        assert_eq!(state.composer, "one ");
    }

    #[test]
    fn external_editor_effect_is_loop_terminally_owned() {
        // ctrl+g produces the effect; the reducer never owns the terminal.
        let state = type_text(UiState::new(), "draft");
        let (_state, effect) = update(state, ctrl('g'));
        assert_eq!(effect, Some(UiEffect::ExternalEditor));
    }

    #[test]
    fn external_edit_replaces_the_whole_composer() {
        let mut state = type_text(UiState::new(), "old draft");
        state.undo_stack.push(EditSnapshot {
            composer: "x".to_owned(),
            cursor: 1,
        });
        let (state, _) = update(state, UiMessage::ExternalEdited("replaced".to_owned()));
        assert_eq!(state.composer, "replaced");
        assert_eq!(state.cursor, 8);
        assert!(state.undo_stack.is_empty(), "editor text is not undoable");
    }
}

#[cfg(test)]
mod queue_parity_tests {
    use super::tests::type_text;
    use super::*;
    use ion_terminal::Modifiers;

    fn alt(code: KeyCode) -> UiMessage {
        UiMessage::Key(KeyEvent::new(code, Modifiers::ALT))
    }

    #[test]
    fn alt_enter_queues_a_follow_up_even_while_idle() {
        let state = type_text(UiState::new(), "later question");
        let (state, effect) = update(state, alt(KeyCode::Enter));
        assert_eq!(
            effect,
            Some(UiEffect::Enqueue {
                text: "later question".to_owned()
            })
        );
        assert_eq!(
            state.history.last().map(String::as_str),
            Some("later question")
        );
    }

    #[test]
    fn alt_enter_rejects_empty_text() {
        let (_state, effect) = update(UiState::new(), alt(KeyCode::Enter));
        assert_eq!(effect, None);
    }

    #[test]
    fn alt_up_dequeues_the_queued_prompt_back_to_the_editor() {
        let mut state = type_text(UiState::new(), "first");
        state.pending_history = Some("first".to_owned());
        let (state, _) = update(state, UiMessage::EnqueueAccepted);
        assert_eq!(state.queued_prompt.as_deref(), Some("first"));
        assert!(state.composer.is_empty(), "composer cleared by enqueue");

        // alt+up produces the dequeue effect; the host resolves it and the
        // prompt returns to the editor.
        let (state, effect) = update(state, alt(KeyCode::Up));
        assert_eq!(effect, Some(UiEffect::DequeueNextRun));
        let (state, _) = update(state, UiMessage::Dequeued(Some("first".to_owned())));
        assert_eq!(state.composer, "first");
        assert_eq!(state.cursor, 5);
        assert!(state.queued_prompt.is_none(), "indicator cleared");
    }

    #[test]
    fn dequeue_with_empty_queue_keeps_the_composer() {
        let mut state = type_text(UiState::new(), "untouched");
        state.queued_prompt = None;
        let (state, _) = update(state, UiMessage::Dequeued(None));
        assert_eq!(state.composer, "untouched");
    }

    #[test]
    fn queued_prompt_is_cleared_when_the_operation_starts() {
        let mut state = UiState::new();
        state.queued_prompt = Some("next up".to_owned());
        let palette = crate::tui::render::palette(crate::settings::Theme::Dark);
        let (state, _) = update(
            state,
            UiMessage::Runtime(RuntimeEvent::OperationStarted {
                cursor: ion_core::RuntimeCursor::default(),
                operation_id: ion_core::OperationId::generate(),
                prompt: "next up".to_owned(),
            }),
        );
        assert!(
            state.queued_prompt.is_none(),
            "started op consumes the queue"
        );
        let _ = palette;
    }

    #[test]
    fn snapshot_resync_restores_the_queued_prompt() {
        let mut state = UiState::new();
        let mut snapshot = ion_core::SessionSnapshot {
            cursor: ion_core::RuntimeCursor::default(),
            runtime_instance_id: ion_core::RuntimeInstanceId::generate(),
            indeterminate: None,
            latest_settlement: None,
            reopen_entry_count: None,
            operation: ion_core::OperationStatus::Idle,
            entries: Vec::new(),
            model_ref: "desktop/test".to_owned(),
            thinking: None,
            pending_next_run: Some(ion_core::NextRunInput {
                entry_id: ion_core::EntryId::generate(),
                prompt: "lag-visible queue".to_owned(),
            }),
            latest_usage: None,
            live: None,
        };
        let _ = &mut snapshot;
        state.resync_after_lag(&snapshot);
        assert_eq!(state.queued_prompt.as_deref(), Some("lag-visible queue"));
    }
}

#[cfg(test)]
mod thinking_parity_tests {
    use super::tests::{key, type_text};
    use super::*;
    use ion_terminal::Modifiers;

    fn shift_tab() -> UiMessage {
        UiMessage::Key(KeyEvent::new(KeyCode::Tab, Modifiers::SHIFT))
    }

    #[test]
    fn shift_tab_cycles_the_pi_thinking_vocabulary() {
        let state = UiState::new();
        let (state, effect) = update(state, shift_tab());
        assert_eq!(
            effect,
            Some(UiEffect::SwitchThinking {
                thinking: Some("minimal".to_owned())
            })
        );
        assert_eq!(state.thinking_level.as_deref(), Some("minimal"));

        // Each press advances one level, wrapping at max.
        let mut state = state;
        for expected in ["low", "medium", "high", "xhigh", "max", "off", "minimal"] {
            let (next, effect) = update(state, shift_tab());
            assert_eq!(
                effect,
                Some(UiEffect::SwitchThinking {
                    thinking: Some(expected.to_owned())
                }),
                "cycle reaches {expected}"
            );
            state = next;
        }
    }

    #[test]
    fn thinking_command_picks_numerically_or_opens_the_picker() {
        let (_state, effect) = handle_command(&mut UiState::new(), "thinking 3");
        assert_eq!(
            effect,
            Some(UiEffect::SwitchThinking {
                thinking: Some("low".to_owned())
            })
        );
        let (_, effect) = handle_command(&mut UiState::new(), "thinking 0");
        assert_eq!(effect, None);

        let mut ui = UiState::new();
        let (ui, _) = handle_command(&mut ui, "thinking hi");
        assert!(ui.thinking_selector.is_some(), "picker opened with query");
        assert!(ui.filtered_thinking_levels().contains(&"high".to_owned()));

        // Arrow + enter selects.
        let (ui, _) = update(ui, key(KeyCode::Down));
        let (ui, effect) = update(ui, key(KeyCode::Enter));
        assert_eq!(
            effect,
            Some(UiEffect::SwitchThinking {
                thinking: Some("high".to_owned())
            })
        );
        assert!(ui.thinking_selector.is_none());
        assert_eq!(ui.thinking_level.as_deref(), Some("high"));

        // `default` clears the selection back to the adapter default.
        let (ui, effect) = handle_command(&mut { ui }, "thinking default");
        assert_eq!(effect, Some(UiEffect::SwitchThinking { thinking: None }));
        assert_eq!(ui.thinking_level, None);
    }

    #[test]
    fn thinking_picker_esc_restores_the_draft() {
        let mut ui = type_text(UiState::new(), "draft survives");
        let (ui, _) = handle_command(&mut ui, "thinking");
        let (ui, _) = update(ui, key(KeyCode::Esc));
        assert_eq!(ui.composer, "draft survives");
        assert!(ui.thinking_selector.is_none());
    }

    #[test]
    fn snapshot_seeds_the_thinking_level() {
        let mut ui = UiState::new();
        let mut snapshot = ion_core::SessionSnapshot {
            cursor: ion_core::RuntimeCursor::default(),
            runtime_instance_id: ion_core::RuntimeInstanceId::generate(),
            indeterminate: None,
            latest_settlement: None,
            reopen_entry_count: None,
            operation: ion_core::OperationStatus::Idle,
            entries: Vec::new(),
            model_ref: "desktop/test".to_owned(),
            thinking: Some("xhigh".to_owned()),
            pending_next_run: None,
            latest_usage: None,
            live: None,
        };
        ui.thinking_level = snapshot.thinking.clone();
        assert_eq!(ui.thinking_level.as_deref(), Some("xhigh"));
        let _ = &mut snapshot;
        // Shift+tab continues from the durable level, not from "off".
        let (_, effect) = update(ui, shift_tab());
        assert_eq!(
            effect,
            Some(UiEffect::SwitchThinking {
                thinking: Some("max".to_owned())
            })
        );
    }
}

#[cfg(test)]
mod shell_passthrough_tests {
    use super::tests::{key, type_text};
    use super::*;

    #[test]
    fn bang_prefix_routes_to_shell_passthrough() {
        let (state, effect) = update(type_text(UiState::new(), "!echo hi"), key(KeyCode::Enter));
        assert_eq!(
            effect,
            Some(UiEffect::RunShell {
                command: "echo hi".to_owned(),
                exclude_from_context: false,
            })
        );
        assert!(
            state.composer.is_empty(),
            "composer resets after passthrough"
        );
        assert_eq!(state.history.last().map(String::as_str), Some("!echo hi"));
    }

    #[test]
    fn double_bang_excludes_output_from_context() {
        let (_, effect) = update(
            type_text(UiState::new(), "!!secret-cmd"),
            key(KeyCode::Enter),
        );
        assert_eq!(
            effect,
            Some(UiEffect::RunShell {
                command: "secret-cmd".to_owned(),
                exclude_from_context: true,
            })
        );
    }

    #[test]
    fn bare_bang_and_working_session_refuse() {
        // Bare '!' is a usage notice, not an effect.
        let (state, effect) = update(type_text(UiState::new(), "!"), key(KeyCode::Enter));
        assert_eq!(effect, None);
        assert!(!state.pending_scrollback.is_empty(), "notice queued");

        // A working session refuses: the runtime would reject anyway, but
        // the reducer explains before the round trip.
        let mut working = UiState::new();
        working.status = UiStatus::Working {
            operation: "thinking".to_owned(),
        };
        let (_, effect) = update(type_text(working, "!cargo test"), key(KeyCode::Enter));
        assert_eq!(effect, None);
    }

    #[test]
    fn shell_events_drive_the_live_band_and_status() {
        let mut state = UiState::new();
        state = apply_runtime_event(
            state,
            RuntimeEvent::ShellStarted {
                cursor: ion_core::RuntimeCursor::default(),
                lane_name: "main".to_owned(),
                command: "long-job".to_owned(),
                exclude_from_context: false,
            },
        );
        assert!(
            matches!(&state.status, UiStatus::Working { operation, .. } if operation == "shell")
        );

        state = apply_runtime_event(
            state,
            RuntimeEvent::ShellOutput {
                cursor: ion_core::RuntimeCursor::default(),
                lane_name: "main".to_owned(),
                output: "partial\n".to_owned(),
            },
        );
        assert_eq!(state.shell_output, "partial\n");

        // Settlement restores idle and clears the provisional preview.
        state = apply_runtime_event(
            state,
            RuntimeEvent::ShellSettled {
                cursor: ion_core::RuntimeCursor::default(),
                lane_name: "main".to_owned(),
                command: "long-job".to_owned(),
                exit_code: Some(0),
                cancelled: false,
                exclude_from_context: false,
                output_preview: Some("done\n".to_owned()),
            },
        );
        assert_eq!(state.status, UiStatus::Idle);
        assert!(state.shell_output.is_empty());
    }

    #[test]
    fn at_char_opens_the_file_picker_and_acceptance_splices_a_reference() {
        let mut state = UiState::new();
        state.workspace_files = vec![
            "crates/ion/src/main.rs".to_owned(),
            "crates/ion/src/tui.rs".to_owned(),
            "DESIGN.md".to_owned(),
        ];
        // Typing '@' at a word start opens the picker without inserting
        // the '@'; the saved draft is restored and spliced on accept.
        let (state, _) = update(state, key(KeyCode::Char('r')));
        let (state, _) = update(state, key(KeyCode::Char('e')));
        let (state, _) = update(state, key(KeyCode::Char('a')));
        let (state, _) = update(state, key(KeyCode::Char('d')));
        let (state, _) = update(state, key(KeyCode::Char(' ')));
        assert!(state.file_selector.is_none(), "not yet at @");

        let (state, _) = update(state, key(KeyCode::Char('@')));
        assert!(state.file_selector.is_some(), "@ opens the picker");
        assert_eq!(state.composer, "", "composer becomes the query");

        // Filter down to tui.rs, then accept.
        let state = type_text(state, "tui");
        assert!(state.file_selector.is_some());
        assert_eq!(state.filtered_file_rows().len(), 1);
        let (state, _) = update(state, key(KeyCode::Enter));
        assert!(state.file_selector.is_none(), "accept closes the picker");
        assert_eq!(state.composer, "read @crates/ion/src/tui.rs ");
        assert_eq!(
            state.cursor,
            state.composer.chars().count(),
            "cursor sits after the inserted reference"
        );
    }

    #[test]
    fn at_char_mid_word_stays_literal_and_empty_workspaces_do_not_open() {
        // An '@' inside prose (not at a word start) is literal text.
        let mut state = UiState::new();
        state.workspace_files = vec!["a.txt".to_owned()];
        let (state, _) = update(state, key(KeyCode::Char('m')));
        let (state, _) = update(state, key(KeyCode::Char('a')));
        let (state, _) = update(state, key(KeyCode::Char('i')));
        let (state, _) = update(state, key(KeyCode::Char('l')));
        let (state, _) = update(state, key(KeyCode::Char('@')));
        assert!(state.file_selector.is_none(), "mid-word @ is literal");
        assert_eq!(state.composer, "mail@");

        // No workspace listing: the '@' stays a plain character.
        let (state, _) = update(
            UiState::new(),
            UiMessage::Key(KeyEvent::new(KeyCode::Char('@'), Modifiers::NONE)),
        );
        assert!(state.file_selector.is_none());
        assert_eq!(state.composer, "@");
    }

    #[test]
    fn file_picker_escape_restores_the_draft_and_arrows_move_selection() {
        let mut state = UiState::new();
        state.workspace_files = vec!["DESIGN.md".to_owned(), "crates/ion/src/main.rs".to_owned()];
        let (state, _) = update(state, key(KeyCode::Char('l')));
        let (state, _) = update(state, key(KeyCode::Char('o')));
        let (state, _) = update(state, key(KeyCode::Char('o')));
        let (state, _) = update(state, key(KeyCode::Char('k')));
        let (state, _) = update(state, key(KeyCode::Char(' ')));
        let (state, _) = update(state, key(KeyCode::Char('@')));
        assert!(state.file_selector.is_some());

        let (state, _) = update(state, key(KeyCode::Down));
        let (state, _) = update(state, key(KeyCode::Up));
        let (state, _) = update(state, key(KeyCode::Esc));
        assert!(state.file_selector.is_none());
        assert_eq!(state.composer, "look ");
        assert_eq!(state.cursor, "look ".chars().count());
    }

    #[test]
    fn file_picker_directory_selection_keeps_the_cursor_scoped() {
        let mut state = UiState::new();
        state.workspace_files = vec!["crates/ion/src/tui.rs".to_owned(), "docs/".to_owned()];
        let (state, _) = update(state, key(KeyCode::Char('@')));
        assert!(state.file_selector.is_some());
        let (state, _) = update(state, key(KeyCode::Enter));
        // First row is the file; walk to the directory row.
        // (rows order = workspace_files order filtered by empty query)
        assert!(state.file_selector.is_none());
        assert_eq!(state.composer, "@crates/ion/src/tui.rs ");
        assert_eq!(state.cursor, state.composer.chars().count());

        // Reopen and select the directory: the reference keeps the
        // cursor directly after the trailing '/' so typing continues
        // the scoped query.
        let mut state = UiState::new();
        state.workspace_files = vec!["docs/".to_owned()];
        let (state, _) = update(state, key(KeyCode::Char('@')));
        let (state, _) = update(state, key(KeyCode::Enter));
        assert!(state.file_selector.is_none());
        assert_eq!(state.composer, "@docs/");
        assert_eq!(
            state.cursor,
            "@docs/".chars().count(),
            "directory accept leaves the cursor after the slash"
        );
    }

    #[test]
    fn file_picker_no_matches_reports_and_enter_is_inert() {
        let mut state = UiState::new();
        state.workspace_files = vec!["a.txt".to_owned()];
        let (state, _) = update(state, key(KeyCode::Char('@')));
        let state = type_text(state, "zzz");
        assert!(state.filtered_file_rows().is_empty());
        let (state, _) = update(state, key(KeyCode::Enter));
        assert!(
            state.file_selector.is_some(),
            "enter with no matches keeps the picker open"
        );
        assert!(
            state
                .pending_scrollback
                .iter()
                .any(|line| line.to_string().contains("no matching files"))
        );
    }
}
