//! IonMsg — the message type for the new `IonApp` event loop.

use tui::event::KeyEvent;

/// All messages handled by the `IonApp` update function.
#[derive(Debug)]
pub enum IonMsg {
    // ── Agent events (pushed by the spawned agent task) ───────────────────────
    /// A text token arrived from the model.
    TokenReceived(String),
    /// A tool call started.
    ToolStarted {
        id: String,
        name: String,
        /// Short human-readable label shown in the header line.
        label: String,
    },
    /// A tool call completed (or errored).
    ToolCompleted {
        id: String,
        output: String,
        is_error: bool,
    },
    /// Agent turn finished successfully.
    StreamingDone,
    /// Agent turn ended with an error.
    AgentError(String),

    // ── Input events (from handle_event) ─────────────────────────────────────
    /// A key was pressed while the input is focused.
    InputKey(KeyEvent),
    /// The user submitted input (Enter pressed).
    InputSubmit(String),
    /// Pasted text (bracketed paste).
    Paste(String),

    // ── App control ───────────────────────────────────────────────────────────
    Resize(u16, u16),
    Quit,
    ScrollUp,
    ScrollDown,
    /// Periodic tick — drives inner App::update() polling.
    Tick,

    // ── Keybinding actions ────────────────────────────────────────────────────
    /// Esc — cancel running task; double-tap clears input.
    CancelTask,
    /// Ctrl+C / Ctrl+D — clear input if non-empty; double-tap quit when idle.
    ClearInputOrQuit,
    /// Shift+Tab — toggle read/write tool mode.
    ToggleMode,
    /// Ctrl+M — open model picker.
    OpenModelPicker,
    /// Ctrl+P — open provider picker.
    OpenProviderPicker,
    /// Ctrl+H — toggle help overlay.
    OpenHelp,
    /// Ctrl+T — cycle thinking level (off → standard → extended → off).
    CycleThinking,
    /// Ctrl+G — open input in external editor.
    OpenEditor,
    /// Ctrl+O — toggle tool output expansion (collapsed ↔ expanded).
    ToggleToolExpansion,
    /// Ctrl+R — open history search.
    OpenHistorySearch,
    /// Terminal gained focus.
    FocusGained,
    /// Terminal lost focus.
    FocusLost,
}
