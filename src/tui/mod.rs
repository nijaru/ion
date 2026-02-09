//! TUI module for the ion agent interface.

mod app_state;
mod chat_renderer;
mod command_completer;
mod completer_state;
pub mod composer;
mod events;
mod file_completer;
mod filter_input;
mod fuzzy;
mod highlight;
mod attachment;
mod input;
pub mod message_list;
pub mod model_picker;
mod picker_trait;
pub mod provider_picker;
mod render;
mod render_state;
mod run;
mod session;
pub mod session_picker;
mod table;
pub mod terminal;
mod types;
mod util;

pub use picker_trait::PickerNavigation;

// Re-export public types
pub use message_list::Sender;
pub use run::{ResumeOption, run};
pub use types::{
    HistorySearchState, Mode, SelectionState, SelectorPage, TaskSummary, ThinkingLevel,
};

// Re-export internal utilities for sibling modules
pub(crate) use types::QUEUED_PREVIEW_LINES;
pub(crate) use util::sanitize_for_display;

use crate::agent::Agent;
use crate::cli::PermissionSettings;
use crate::config::Config;
use crate::provider::{ModelPricing, ModelRegistry, Provider};
use crate::session::{Session, SessionStore};
use crate::tool::{ToolMode, ToolOrchestrator};
use crate::tui::app_state::{InteractionState, TaskState};
use crate::tui::command_completer::CommandCompleter;
use crate::tui::composer::{ComposerBuffer, ComposerState};
use crate::tui::file_completer::FileCompleter;
use crate::tui::message_list::MessageList;
use crate::tui::model_picker::ModelPicker;
use crate::tui::provider_picker::ProviderPicker;
use crate::tui::render_state::RenderState;
use crate::tui::session_picker::SessionPicker;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Main TUI application state.
#[allow(clippy::struct_excessive_bools)] // TUI state flags are naturally boolean
pub struct App {
    pub mode: Mode,
    pub selector_page: SelectorPage,
    pub should_quit: bool,
    pub input_buffer: ComposerBuffer,
    pub input_state: ComposerState,
    /// Input history for arrow-up recall
    pub input_history: Vec<String>,
    /// Current position in history (`input_history.len()` = current input)
    pub history_index: usize,
    /// Draft input before entering history navigation
    pub history_draft: Option<String>,
    /// Current tool permission mode (Read/Write)
    pub tool_mode: ToolMode,
    /// Shared atomic mode for subagent live reads
    pub shared_tool_mode: crate::tool::builtin::spawn_subagent::SharedToolMode,
    /// Currently selected API provider
    pub api_provider: Provider,
    /// API provider picker state
    pub provider_picker: ProviderPicker,
    pub message_list: MessageList,
    /// Render state for chat positioning and incremental updates
    pub render_state: RenderState,
    pub agent: Arc<Agent>,
    pub session: Session,
    pub orchestrator: Arc<ToolOrchestrator>,
    pub agent_tx: mpsc::Sender<crate::agent::AgentEvent>,
    pub agent_rx: mpsc::Receiver<crate::agent::AgentEvent>,
    pub session_rx: mpsc::Receiver<Session>,
    pub session_tx: mpsc::Sender<Session>,
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
    /// State for the current agent task (timing, tokens, tool)
    pub task: TaskState,
    /// State for user interaction tracking (double-tap, editor requests)
    pub interaction: InteractionState,
    /// Permission settings from CLI flags
    pub permissions: PermissionSettings,
    /// Last completed task summary (for brief display after completion)
    pub last_task_summary: Option<TaskSummary>,
    /// File path autocomplete state
    pub file_completer: FileCompleter,
    /// Command autocomplete state
    pub command_completer: CommandCompleter,
    /// Ctrl+R history search state
    pub history_search: HistorySearchState,
    /// Pending provider change (deferred until model selection)
    pub pending_provider: Option<Provider>,
    /// Pricing for the current model (per million tokens).
    pub model_pricing: ModelPricing,
    /// Accumulated cost for the current session (USD).
    pub session_cost: f64,
}

// Position-related accessors are on render_state.position directly.
