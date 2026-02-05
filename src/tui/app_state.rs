//! Grouped state structs for the TUI App.

use std::time::{Duration, Instant};

/// State for the current agent task.
///
/// Groups fields related to task execution progress, timing, and token usage.
#[derive(Debug, Default)]
pub struct TaskState {
    /// When the current task started (for elapsed time display).
    pub start_time: Option<Instant>,
    /// Input tokens sent to model (current task).
    pub input_tokens: usize,
    /// Output tokens received from model (current task).
    pub output_tokens: usize,
    /// Currently executing tool name (for interrupt handling).
    pub current_tool: Option<String>,
    /// Retry status (reason, `delay_seconds`, started_at) - shown in progress line.
    pub retry_status: Option<(String, u64, Instant)>,
    /// When thinking started (for progress display).
    pub thinking_start: Option<Instant>,
    /// Duration of last completed thinking (for "thought for Xs" display).
    pub last_thinking_duration: Option<Duration>,
}

impl TaskState {
    /// Reset task state for a new task.
    pub fn reset(&mut self) {
        self.start_time = Some(Instant::now());
        self.input_tokens = 0;
        self.output_tokens = 0;
        self.current_tool = None;
        self.retry_status = None;
        self.thinking_start = None;
        self.last_thinking_duration = None;
    }

    /// Clear task state when task completes.
    pub fn clear(&mut self) {
        self.start_time = None;
        self.current_tool = None;
        self.retry_status = None;
        self.thinking_start = None;
    }
}

/// State for user interaction tracking.
///
/// Groups fields related to pending user actions (double-tap detection, editor requests).
#[derive(Debug, Default)]
pub struct InteractionState {
    /// Timestamp of first Ctrl+C press for double-tap quit/cancel.
    pub cancel_pending: Option<Instant>,
    /// Timestamp of first Esc press for double-tap clear input.
    pub esc_pending: Option<Instant>,
    /// Request to open input in external editor (Ctrl+G).
    pub editor_requested: bool,
}
