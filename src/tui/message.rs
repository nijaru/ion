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

    // ── App control ───────────────────────────────────────────────────────────
    Resize(u16, u16),
    Quit,
    /// Toggle tool permission mode: read ↔ write.
    ToggleMode,
    ScrollUp,
    ScrollDown,
    /// Periodic tick — drives inner App::update() polling.
    Tick,
}
