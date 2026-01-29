//! TUI module for the ion agent interface.

mod chat_renderer;
pub mod composer;
mod events;
mod filter_input;
mod fuzzy;
mod highlight;
mod input;
pub mod message_list;
pub mod model_picker;
pub mod provider_picker;
mod render;
mod session;
mod table;
pub mod session_picker;
pub mod terminal;
mod types;
mod util;

// Re-export public types
pub use message_list::Sender;
pub use types::{ApprovalRequest, Mode, SelectionState, SelectorPage, TaskSummary, ThinkingLevel};

// Re-export internal utilities for sibling modules
pub(crate) use types::QUEUED_PREVIEW_LINES;
pub(crate) use util::sanitize_for_display;

use crate::agent::Agent;
use crate::cli::PermissionSettings;
use crate::config::Config;
use crate::provider::{ModelRegistry, Provider};
use crate::session::{Session, SessionStore};
use crate::tool::{ToolMode, ToolOrchestrator};
use crate::tui::composer::{ComposerBuffer, ComposerState};
use crate::tui::message_list::MessageList;
use crate::tui::model_picker::ModelPicker;
use crate::tui::provider_picker::ProviderPicker;
use crate::tui::session_picker::SessionPicker;
use crate::tui::terminal::StyledLine;
use crate::tui::types::ApprovalRequest as ApprovalRequestInternal;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

/// Main TUI application state.
pub struct App {
    pub mode: Mode,
    pub selector_page: SelectorPage,
    pub should_quit: bool,
    pub input_buffer: ComposerBuffer,
    pub input_state: ComposerState,
    /// Input history for arrow-up recall
    pub input_history: Vec<String>,
    /// Current position in history (input_history.len() = current input)
    pub history_index: usize,
    /// Draft input before entering history navigation
    pub history_draft: Option<String>,
    /// Current tool permission mode (Read/Write/Agi)
    pub tool_mode: ToolMode,
    /// Currently selected API provider
    pub api_provider: Provider,
    /// API provider picker state
    pub provider_picker: ProviderPicker,
    pub message_list: MessageList,
    /// Number of chat entries already inserted into scrollback
    pub rendered_entries: usize,
    /// Buffered chat lines while selector is open
    pub buffered_chat_lines: Vec<StyledLine>,
    pub agent: Arc<Agent>,
    pub session: Session,
    pub orchestrator: Arc<ToolOrchestrator>,
    pub agent_tx: mpsc::Sender<crate::agent::AgentEvent>,
    pub agent_rx: mpsc::Receiver<crate::agent::AgentEvent>,
    pub approval_rx: mpsc::Receiver<ApprovalRequestInternal>,
    pub session_rx: mpsc::Receiver<Session>,
    pub session_tx: mpsc::Sender<Session>,
    pub pending_approval: Option<ApprovalRequestInternal>,
    pub is_running: bool,
    /// Session persistence store
    pub store: SessionStore,
    /// Model picker state
    pub model_picker: ModelPicker,
    /// Session picker state
    pub session_picker: SessionPicker,
    /// Model registry for fetching available models
    pub model_registry: Arc<ModelRegistry>,
    /// Config for accessing preferences
    pub config: Config,
    /// TUI frame counter for animations
    pub frame_count: u64,
    /// First-time setup in progress (blocks normal input until complete)
    pub needs_setup: bool,
    /// Whether we've started fetching models for setup (prevents duplicate fetches)
    pub(crate) setup_fetch_started: bool,
    /// Current thinking budget level (Ctrl+T to cycle)
    pub thinking_level: ThinkingLevel,
    /// Current token usage (used, max) for context % display
    pub token_usage: Option<(usize, usize)>,
    /// Model context window (for status display when known)
    pub model_context_window: Option<usize>,
    /// Last error message for status line display
    pub last_error: Option<String>,
    /// Shared message queue for mid-task steering (TUI pushes, agent drains)
    pub message_queue: Option<Arc<std::sync::Mutex<Vec<String>>>>,
    /// When the current task started (for elapsed time display)
    pub task_start_time: Option<Instant>,
    /// Input tokens sent to model (current task)
    pub input_tokens: usize,
    /// Output tokens received from model (current task)
    pub output_tokens: usize,
    /// Currently executing tool name (for interrupt handling)
    pub current_tool: Option<String>,
    /// Retry status (reason, delay_seconds) - shown in progress line
    pub retry_status: Option<(String, u64)>,
    /// Timestamp of first Ctrl+C press for double-tap quit/cancel
    pub cancel_pending: Option<Instant>,
    /// Timestamp of first Esc press for double-tap clear input
    pub(crate) esc_pending: Option<Instant>,
    /// Permission settings from CLI flags
    pub permissions: PermissionSettings,
    /// Last completed task summary (for brief display after completion)
    pub last_task_summary: Option<TaskSummary>,
    /// Request to open input in external editor (Ctrl+G)
    pub editor_requested: bool,
    /// Whether the startup header has been inserted into scrollback
    pub(crate) header_inserted: bool,
    /// When thinking started (for progress display)
    pub thinking_start: Option<Instant>,
    /// Duration of last completed thinking (for "thought for Xs" display)
    pub last_thinking_duration: Option<std::time::Duration>,
    /// Last render state for detecting changes that need extra clearing
    pub(crate) last_render_width: Option<u16>,
    pub(crate) last_ui_start: Option<u16>,
    /// UI anchor row for startup before first message (keeps UI near header)
    pub(crate) startup_ui_anchor: Option<u16>,
}

impl App {
    pub fn header_inserted(&self) -> bool {
        self.header_inserted
    }

    pub fn set_header_inserted(&mut self, value: bool) {
        self.header_inserted = value;
    }

    pub fn startup_ui_anchor(&self) -> Option<u16> {
        self.startup_ui_anchor
    }

    pub fn set_startup_ui_anchor(&mut self, value: Option<u16>) {
        self.startup_ui_anchor = value;
    }
}
