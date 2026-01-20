mod fuzzy;
mod highlight;
pub mod message_list;
pub mod model_picker;
pub mod provider_picker;
pub mod widgets;

use crate::agent::{Agent, AgentEvent};
use crate::cli::PermissionSettings;
use crate::config::Config;
use crate::provider::{Client, LlmApi, ModelRegistry, Provider};
use crate::session::Session;
use crate::session::SessionStore;
use crate::tool::{ApprovalHandler, ApprovalResponse, ToolMode, ToolOrchestrator};
use crate::tui::message_list::{MessageList, Sender};
use crate::tui::model_picker::ModelPicker;
use crate::tui::provider_picker::ProviderPicker;
use async_trait::async_trait;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use rat_text::event::TextOutcome;
use rat_text::text_area::{self, TextArea, TextAreaState, TextWrap};
use rat_text::text_input::{self, TextInput};
use rat_text::{HasScreenCursor, TextPosition};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

const CANCEL_WINDOW: Duration = Duration::from_millis(1500);
const QUEUED_PREVIEW_LINES: usize = 5;

/// Format token count as human-readable (e.g., 1500 -> "1.5k")
fn format_tokens(n: usize) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

/// Normalize errors for status line display.
fn format_status_error(msg: &str) -> String {
    let mut out = msg.trim().to_string();
    for prefix in ["Completion error: ", "Stream error: ", "API error: "] {
        if let Some(rest) = out.strip_prefix(prefix) {
            out = rest.to_string();
        }
    }
    if out.to_lowercase().contains("operation timed out") {
        return "Network timeout".to_string();
    }
    out
}

/// Thinking budget level for extended reasoning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThinkingLevel {
    /// No extended thinking (default)
    #[default]
    Off,
    /// Standard budget (4k tokens)
    Standard,
    /// Extended budget (16k tokens)
    Extended,
}

impl ThinkingLevel {
    /// Cycle to the next level
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::Standard,
            Self::Standard => Self::Extended,
            Self::Extended => Self::Off,
        }
    }

    /// Get the token budget for this level, None if Off
    pub fn budget_tokens(self) -> Option<u32> {
        match self {
            Self::Off => None,
            Self::Standard => Some(4096),
            Self::Extended => Some(16384),
        }
    }

    /// Display label for the status line (empty string when off)
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "",
            Self::Standard => "[think:4k]",
            Self::Extended => "[think:16k]",
        }
    }
}

/// Modal states for the TUI. The default is Input (no mode switching required).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Standard input mode (always active unless a modal is open)
    #[default]
    Input,
    /// Tool approval prompt
    Approval,
    /// Bottom-anchored selector shell (provider/model)
    Selector,
    /// Keybinding help overlay (Ctrl+H)
    HelpOverlay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectorPage {
    Provider,
    Model,
}

pub struct ApprovalRequest {
    pub tool_name: String,
    pub args: serde_json::Value,
    pub response_tx: oneshot::Sender<ApprovalResponse>,
}

pub struct App {
    pub mode: Mode,
    pub selector_page: SelectorPage,
    pub should_quit: bool,
    pub input_state: TextAreaState,
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
    pub agent: Arc<Agent>,
    pub session: Session,
    pub orchestrator: Arc<ToolOrchestrator>,
    pub agent_tx: mpsc::Sender<AgentEvent>,
    pub agent_rx: mpsc::Receiver<AgentEvent>,
    pub approval_rx: mpsc::Receiver<ApprovalRequest>,
    pub session_rx: mpsc::Receiver<Session>,
    pub session_tx: mpsc::Sender<Session>,
    pub pending_approval: Option<ApprovalRequest>,
    pub is_running: bool,
    /// Session persistence store
    pub store: SessionStore,
    /// Model picker state
    pub model_picker: ModelPicker,
    /// Model registry for fetching available models
    pub model_registry: Arc<ModelRegistry>,
    /// Config for accessing preferences
    pub config: Config,
    /// TUI frame counter for animations
    pub frame_count: u64,
    /// First-time setup in progress (blocks normal input until complete)
    pub needs_setup: bool,
    /// Whether we've started fetching models for setup (prevents duplicate fetches)
    setup_fetch_started: bool,
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
    /// Timestamp of first Ctrl+C press for double-tap quit/cancel
    pub cancel_pending: Option<Instant>,
    /// Permission settings from CLI flags
    pub permissions: PermissionSettings,
    /// Last completed task summary (for brief display after completion)
    pub last_task_summary: Option<TaskSummary>,
    /// Request to open input in external editor (Ctrl+G)
    pub editor_requested: bool,
}

/// Summary of a completed task for post-completion display
#[derive(Clone)]
pub struct TaskSummary {
    pub elapsed: std::time::Duration,
    pub input_tokens: usize,
    pub output_tokens: usize,
    pub was_cancelled: bool,
}

struct TuiApprovalHandler {
    request_tx: mpsc::Sender<ApprovalRequest>,
}

#[async_trait]
impl ApprovalHandler for TuiApprovalHandler {
    async fn ask_approval(&self, tool_name: &str, args: &serde_json::Value) -> ApprovalResponse {
        let (tx, rx) = oneshot::channel();
        let request = ApprovalRequest {
            tool_name: tool_name.to_string(),
            args: args.clone(),
            response_tx: tx,
        };

        if self.request_tx.send(request).await.is_err() {
            return ApprovalResponse::No;
        }

        rx.await.unwrap_or(ApprovalResponse::No)
    }
}

use tracing::{debug, error};

impl App {
    fn input_text(&self) -> String {
        self.input_state.text()
    }

    fn clear_input(&mut self) {
        self.input_state.clear();
    }

    pub fn set_input_text(&mut self, text: &str) {
        self.input_state.set_text(text);
        self.move_input_cursor_to_end();
    }

    fn handle_input_event_with_history(&mut self, key: KeyEvent) -> TextOutcome {
        let outcome = self.handle_input_event(key);
        if matches!(outcome, TextOutcome::TextChanged) {
            self.history_index = self.input_history.len();
            self.history_draft = None;
            let cursor = self.input_state.value.cursor();
            if cursor.y > 0 && self.input_state.value.line_width(cursor.y).unwrap_or(0) == 0 {
                let prev = cursor.y.saturating_sub(1);
                let prev_col = self.input_state.value.line_width(prev).unwrap_or(0);
                let _ = self
                    .input_state
                    .value
                    .set_cursor(TextPosition::new(prev_col, prev), false);
                self.input_state.scroll_cursor_to_visible();
            }
        }
        outcome
    }

    fn move_input_cursor_to_end(&mut self) {
        let lines = self.input_state.value.len_lines();
        if lines == 0 {
            return;
        }
        let last_line = lines.saturating_sub(1);
        let last_col = self.input_state.value.line_width(last_line).unwrap_or(0);
        let pos = TextPosition::new(last_col, last_line);
        let _ = self.input_state.value.set_cursor(pos, false);
        self.input_state.scroll_cursor_to_visible();
    }

    fn input_cursor_line(&self) -> u32 {
        self.input_state.value.cursor().y
    }

    fn input_last_line(&self) -> u32 {
        let lines = self.input_state.value.len_lines();
        if lines <= 1 {
            return 0;
        }
        let last = lines.saturating_sub(1);
        if self.input_state.value.line_width(last).unwrap_or(0) == 0 {
            return last.saturating_sub(1);
        }
        last
    }

    fn has_queued_messages(&self) -> bool {
        self.message_queue.as_ref().is_some_and(|queue| {
            if let Ok(q) = queue.lock() {
                !q.is_empty()
            } else {
                false
            }
        })
    }

    fn startup_header_lines(&self) -> Vec<Line<'static>> {
        let version = format!("v{}", env!("CARGO_PKG_VERSION"));
        vec![
            Line::from(Span::styled("ION", Style::default().bold())),
            Line::from(Span::styled(version, Style::default().dim())),
            Line::from(""),
        ]
    }

    fn handle_input_event(&mut self, key: KeyEvent) -> TextOutcome {
        let event = Event::Key(key);
        text_area::handle_events(&mut self.input_state, true, &event)
    }

    fn handle_input_up(&mut self) -> bool {
        let input_empty = self.input_state.is_empty();
        if self.input_cursor_line() != 0 {
            return false;
        }

        if self.is_running && input_empty {
            let queued = self.message_queue.as_ref().and_then(|queue| {
                if let Ok(mut q) = queue.lock() {
                    if q.is_empty() {
                        None
                    } else {
                        Some(q.drain(..).collect::<Vec<_>>())
                    }
                } else {
                    None
                }
            });

            if let Some(queued) = queued {
                self.set_input_text(&queued.join("\n\n"));
                return true;
            }
        }

        if self.history_index == self.input_history.len() && self.history_draft.is_none() {
            self.history_draft = Some(self.input_text());
        }

        if !self.input_history.is_empty() && self.history_index > 0 {
            self.history_index -= 1;
            let entry = self.input_history[self.history_index].clone();
            self.set_input_text(&entry);
            return true;
        }

        input_empty
    }

    fn handle_input_down(&mut self) -> bool {
        if self.input_cursor_line() < self.input_last_line() {
            return false;
        }

        if self.history_index < self.input_history.len() {
            self.history_index += 1;
            if self.history_index == self.input_history.len() {
                if let Some(draft) = self.history_draft.take() {
                    self.set_input_text(&draft);
                } else {
                    self.clear_input();
                }
            } else {
                let entry = self.input_history[self.history_index].clone();
                self.set_input_text(&entry);
            }
            return true;
        }

        !self.input_state.is_empty()
    }
    pub async fn new() -> Self {
        Self::with_permissions(PermissionSettings::default()).await
    }

    pub async fn with_permissions(permissions: PermissionSettings) -> Self {
        let config = Config::load().expect("Failed to load config");

        // Initialize logging - write to file if ION_LOG is set
        if std::env::var("ION_LOG").is_ok() {
            use std::fs::File;
            use tracing_subscriber::prelude::*;
            let file = File::create("ion.log").expect("Failed to create log file");
            let file_layer = tracing_subscriber::fmt::layer()
                .with_writer(file)
                .with_ansi(false);
            let filter = tracing_subscriber::EnvFilter::new("ion=debug");
            let _ = tracing_subscriber::registry()
                .with(file_layer.with_filter(filter))
                .try_init();
        } else if std::env::var("RUST_LOG").is_ok() {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
                .try_init();
        }

        // Determine active provider from config
        let api_provider = config
            .provider
            .as_deref()
            .and_then(Provider::from_id)
            .unwrap_or(Provider::OpenRouter);

        // Get API key (env var first, then config)
        let api_key = config.api_key_for(api_provider.id()).unwrap_or_default();

        let provider_impl: Arc<dyn LlmApi> = Arc::new(
            Client::new(api_provider, api_key.clone()).expect("Failed to create LLM client"),
        );

        let (approval_tx, approval_rx) = mpsc::channel(100);
        let mut orchestrator = ToolOrchestrator::with_builtins(permissions.mode);

        // Only set approval handler if not auto-approving
        if !permissions.auto_approve {
            orchestrator.set_approval_handler(Arc::new(TuiApprovalHandler {
                request_tx: approval_tx,
            }));
        }

        // Initialize MCP servers
        let mut mcp_manager = crate::mcp::McpManager::new();
        let mut all_mcp_servers = config.mcp_servers.clone();

        // Load project-local .mcp.json
        let local_mcp_path = std::env::current_dir()
            .unwrap_or_default()
            .join(".mcp.json");
        if local_mcp_path.exists()
            && let Ok(content) = std::fs::read_to_string(&local_mcp_path)
        {
            #[derive(Deserialize)]
            struct LocalMcpConfig {
                #[serde(rename = "mcpServers")]
                mcp_servers: HashMap<String, crate::mcp::McpServerConfig>,
            }
            if let Ok(local_config) = serde_json::from_str::<LocalMcpConfig>(&content) {
                for (name, srv_config) in local_config.mcp_servers {
                    all_mcp_servers.insert(name, srv_config);
                }
            }
        }

        for (name, mcp_config) in &all_mcp_servers {
            if let Err(e) = mcp_manager.add_server(name, mcp_config.clone()).await {
                error!("Failed to start MCP server {}: {}", name, e);
            }
        }
        let mcp_tools = mcp_manager.get_all_tools().await;
        for tool in mcp_tools {
            orchestrator.register_tool(tool);
        }

        let orchestrator = Arc::new(orchestrator);

        let agent = Arc::new(Agent::new(provider_impl, orchestrator.clone()));

        // Open session store
        let store =
            SessionStore::open(&config.sessions_db_path()).expect("Failed to open session store");

        // Create new session with current directory
        let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let model = config.model.clone().unwrap_or_default();
        let mut session = Session::new(working_dir, model);
        session.no_sandbox = permissions.no_sandbox;

        let (agent_tx, agent_rx) = mpsc::channel(100);
        let (session_tx, session_rx) = mpsc::channel(1);

        // Model registry for picker
        let model_registry = Arc::new(ModelRegistry::new(api_key, config.model_cache_ttl_secs));

        // Detect if first-time setup is needed
        let needs_setup = config.needs_setup();
        let (initial_mode, selector_page) = if needs_setup {
            if config.provider.is_none() {
                (Mode::Selector, SelectorPage::Provider)
            } else {
                (Mode::Selector, SelectorPage::Model)
            }
        } else {
            (Mode::Input, SelectorPage::Provider)
        };

        let mut input_state = TextAreaState::default();
        input_state.set_text_wrap(TextWrap::Word(1));
        input_state.set_auto_indent(false);
        input_state.set_auto_quote(false);

        let mut this = Self {
            mode: initial_mode,
            selector_page,
            should_quit: false,
            input_state,
            input_history: Vec::new(),
            history_index: 0,
            history_draft: None,
            tool_mode: permissions.mode,
            api_provider,
            provider_picker: ProviderPicker::new(),
            message_list: MessageList::new(),
            agent,
            session,
            orchestrator,
            agent_tx,
            agent_rx,
            approval_rx,
            session_rx,
            session_tx,
            pending_approval: None,
            is_running: false,
            store,
            model_picker: ModelPicker::new(config.provider_prefs.clone()),
            model_registry,
            config,
            frame_count: 0,
            needs_setup,
            setup_fetch_started: false,
            thinking_level: ThinkingLevel::Off,
            token_usage: None,
            model_context_window: None,
            last_error: None,
            message_queue: None,
            task_start_time: None,
            input_tokens: 0,
            output_tokens: 0,
            current_tool: None,
            cancel_pending: None,
            permissions,
            last_task_summary: None,
            editor_requested: false,
        };

        // Set initial API provider name on model picker
        this.model_picker.set_api_provider(api_provider.name());

        // Load persisted input history
        if let Ok(history) = this.store.load_input_history() {
            this.input_history = history;
            this.history_index = this.input_history.len();
        }

        // Initialize setup flow if needed
        if this.needs_setup {
            match this.selector_page {
                SelectorPage::Provider => {
                    this.provider_picker.refresh();
                }
                SelectorPage::Model => {
                    this.model_picker.is_loading = true;
                    // Models will be fetched when run loop starts
                }
            }
        }

        this
    }

    /// Resume an existing session by ID.
    pub fn resume_session(
        &mut self,
        session_id: &str,
    ) -> Result<(), crate::session::SessionStoreError> {
        let loaded = self.store.load(session_id)?;
        self.message_list.load_from_messages(&loaded.messages);
        self.session = loaded;
        Ok(())
    }

    /// List recent sessions for display.
    pub fn list_recent_sessions(&self, limit: usize) -> Vec<crate::session::SessionSummary> {
        self.store.list_recent(limit).unwrap_or_default()
    }

    pub fn update(&mut self) {
        self.frame_count = self.frame_count.wrapping_add(1);

        // Re-trigger setup flow if needed (e.g., user pressed Esc)
        if self.needs_setup && self.mode == Mode::Input {
            if self.config.provider.is_none() {
                self.open_provider_selector();
            } else {
                self.open_model_selector();
            }
        }

        // Start fetching models if in setup mode and model selector needs them
        if self.needs_setup
            && self.mode == Mode::Selector
            && self.selector_page == SelectorPage::Model
            && !self.model_picker.has_models()
            && !self.setup_fetch_started
            && self.model_picker.error.is_none()
        {
            self.setup_fetch_started = true;
            self.model_picker.is_loading = true;
            self.fetch_models();
        }

        // Poll agent events
        while let Ok(event) = self.agent_rx.try_recv() {
            match &event {
                AgentEvent::Finished(_) => {
                    self.save_task_summary(false);
                    self.is_running = false;
                    self.cancel_pending = None;
                    self.last_error = None;
                    self.message_queue = None;
                    self.task_start_time = None;
                    self.current_tool = None;
                    // Auto-scroll to bottom so user sees completion
                    self.message_list.scroll_to_bottom();
                }
                AgentEvent::Error(msg) => {
                    // Check if this was a cancellation
                    let was_cancelled = msg.contains("Cancelled");
                    self.save_task_summary(was_cancelled);
                    self.is_running = false;
                    self.cancel_pending = None;
                    self.message_queue = None;
                    self.task_start_time = None;
                    self.current_tool = None;
                    if !was_cancelled {
                        self.last_error = Some(format_status_error(msg));
                        // Auto-scroll to bottom so user sees error
                        self.message_list.scroll_to_bottom();
                        self.message_list.push_event(event);
                    }
                }
                AgentEvent::ModelsFetched(models) => {
                    debug!("Received ModelsFetched event with {} models", models.len());
                    self.model_picker.set_models(models.clone());
                    if let Some(model) = models.iter().find(|m| m.id == self.session.model) {
                        if model.context_window > 0 {
                            self.model_context_window = Some(model.context_window as usize);
                        }
                    }
                    self.last_error = None; // Clear error on success
                    // Show all models directly (user can type to filter/search)
                    self.model_picker.start_all_models();
                }
                AgentEvent::ModelFetchError(err) => {
                    debug!("Received ModelFetchError: {}", err);
                    self.model_picker.set_error(err.clone());
                    self.last_error = Some(err.clone());
                }
                AgentEvent::TokenUsage { used, max } => {
                    self.token_usage = Some((*used, *max));
                }
                AgentEvent::InputTokens(count) => {
                    self.input_tokens = *count;
                }
                AgentEvent::OutputTokensDelta(count) => {
                    self.output_tokens += count;
                }
                AgentEvent::ToolCallStart(_, name, _) => {
                    self.current_tool = Some(name.clone());
                    self.message_list.push_event(event);
                }
                AgentEvent::ToolCallResult(..) => {
                    self.current_tool = None;
                    self.message_list.push_event(event);
                }
                _ => {
                    self.message_list.push_event(event);
                }
            }
        }

        // Poll session updates (preserves conversation history)
        if let Ok(updated_session) = self.session_rx.try_recv() {
            self.save_task_summary(false);
            self.is_running = false;
            self.cancel_pending = None;
            self.message_queue = None;
            self.task_start_time = None;
            self.current_tool = None;

            // Auto-save to persistent storage
            if let Err(e) = self.store.save(&updated_session) {
                tracing::warn!("Failed to save session: {}", e);
            }
            self.session = updated_session;
        }

        // Clear expired cancel prompt
        if let Some(when) = self.cancel_pending
            && when.elapsed() > CANCEL_WINDOW
        {
            self.cancel_pending = None;
        }

        // Poll approval requests
        if self.pending_approval.is_none()
            && let Ok(request) = self.approval_rx.try_recv()
        {
            self.pending_approval = Some(request);
            self.mode = Mode::Approval;
        }
    }

    pub fn handle_event(&mut self, event: Event) {
        if let Event::Key(key) = event {
            match self.mode {
                Mode::Input => self.handle_input_mode(key),
                Mode::Approval => self.handle_approval_mode(key),
                Mode::Selector => self.handle_selector_mode(key),
                Mode::HelpOverlay => {
                    self.mode = Mode::Input;
                }
            }
        }
    }

    /// Main input handler - always active unless a modal is open
    fn handle_input_mode(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        match key.code {
            // Esc: Cancel running task
            KeyCode::Esc => {
                if self.is_running && !self.session.abort_token.is_cancelled() {
                    self.session.abort_token.cancel();
                    self.cancel_pending = None;
                }
            }
            // Ctrl+C: Clear input, cancel running task, or quit
            KeyCode::Char('c') if ctrl => {
                if !self.input_state.is_empty() {
                    self.clear_input();
                    self.cancel_pending = None;
                } else if let Some(when) = self.cancel_pending
                    && when.elapsed() <= CANCEL_WINDOW
                {
                    if self.is_running {
                        if !self.session.abort_token.is_cancelled() {
                            self.session.abort_token.cancel();
                        }
                    } else {
                        self.quit();
                    }
                    self.cancel_pending = None;
                } else {
                    self.cancel_pending = Some(Instant::now());
                }
            }

            // Ctrl+D: Quit if input empty
            KeyCode::Char('d') if ctrl => {
                if self.input_state.is_empty() {
                    self.quit();
                }
            }

            // Shift+Tab: Cycle tool mode (Read ↔ Write, or Read → Write → Agi if --agi)
            KeyCode::BackTab => {
                self.tool_mode = if self.permissions.agi_enabled {
                    // AGI mode available: Read → Write → Agi → Read
                    match self.tool_mode {
                        ToolMode::Read => ToolMode::Write,
                        ToolMode::Write => ToolMode::Agi,
                        ToolMode::Agi => ToolMode::Read,
                    }
                } else {
                    // Normal mode: Read ↔ Write only
                    match self.tool_mode {
                        ToolMode::Read => ToolMode::Write,
                        ToolMode::Write => ToolMode::Read,
                        ToolMode::Agi => ToolMode::Read, // Shouldn't happen, but handle it
                    }
                };
                // Update the orchestrator
                let orchestrator = self.orchestrator.clone();
                let mode = self.tool_mode;
                tokio::spawn(async move {
                    orchestrator.set_tool_mode(mode).await;
                });
            }

            // Ctrl+M: Open model picker (current provider only)
            KeyCode::Char('m') if ctrl => {
                if !self.is_running {
                    self.open_model_selector();
                }
            }

            // Ctrl+P: Provider → Model picker (two-stage)
            KeyCode::Char('p') if ctrl => {
                if !self.is_running {
                    self.open_provider_selector();
                }
            }

            // Ctrl+H: Open help overlay
            KeyCode::Char('h') if ctrl => {
                self.mode = Mode::HelpOverlay;
            }

            // Ctrl+T: Cycle thinking level (off → standard → extended → off)
            KeyCode::Char('t') if ctrl => {
                self.thinking_level = self.thinking_level.next();
            }

            // Ctrl+S: Take UI snapshot (Debug/Agent only)
            KeyCode::Char('s') if ctrl => {
                self.take_snapshot();
            }

            // Ctrl+G: Open input in external editor
            KeyCode::Char('g') if ctrl => {
                self.editor_requested = true;
            }

            // Shift+Enter: Insert newline (requires Kitty keyboard protocol)
            KeyCode::Enter if shift => {
                self.input_state.insert_newline();
            }

            // Enter: Send message or queue for mid-task steering
            KeyCode::Enter => {
                if !self.input_state.is_empty() {
                    let input = self.input_text();
                    if self.is_running {
                        // Queue message for injection at next turn
                        if let Some(queue) = self.message_queue.as_ref()
                            && let Ok(mut q) = queue.lock()
                        {
                            q.push(input);
                        }
                        self.clear_input();
                    } else {
                        // Check for slash commands
                        if input.starts_with('/') {
                            const COMMANDS: [&str; 5] =
                                ["/model", "/provider", "/clear", "/quit", "/help"];
                            let cmd_line = input.trim().to_lowercase();
                            let cmd_name = cmd_line.split_whitespace().next().unwrap_or("");
                            match cmd_name {
                                "/model" | "/models" => {
                                    self.clear_input();
                                    self.open_model_selector();
                                    return;
                                }
                                "/provider" | "/providers" => {
                                    self.clear_input();
                                    self.open_provider_selector();
                                    return;
                                }
                                "/quit" | "/exit" | "/q" => {
                                    self.clear_input();
                                    self.quit();
                                    return;
                                }
                                "/clear" => {
                                    self.clear_input();
                                    self.message_list.clear();
                                    self.session.messages.clear();
                                    return;
                                }
                                "/help" | "/?" => {
                                    self.clear_input();
                                    self.mode = Mode::HelpOverlay;
                                    return;
                                }
                                _ => {
                                    if !cmd_name.is_empty() {
                                        let suggestions = fuzzy::top_matches(
                                            cmd_name,
                                            COMMANDS.iter().copied(),
                                            3,
                                        );
                                        let message = if suggestions.is_empty() {
                                            format!("Unknown command {}", cmd_name)
                                        } else {
                                            format!(
                                                "Unknown command {}. Did you mean {}?",
                                                cmd_name,
                                                suggestions.join(", ")
                                            )
                                        };
                                        self.message_list.push_entry(
                                            crate::tui::message_list::MessageEntry::new(
                                                Sender::System,
                                                message,
                                            ),
                                        );
                                    }
                                    self.clear_input();
                                    return;
                                }
                            }
                        }

                        // Send message
                        self.input_history.push(input.clone());
                        self.history_index = self.input_history.len();
                        self.history_draft = None;
                        self.clear_input();
                        // Persist to database
                        let _ = self.store.add_input_history(&input);
                        self.message_list.push_user_message(input.clone());
                        self.run_agent_task(input);
                    }
                }
            }

            // Page Up/Down: Scroll chat history
            KeyCode::PageUp => self.message_list.scroll_up(10),
            KeyCode::PageDown => self.message_list.scroll_down(10),

            // Arrow Up: Move cursor up, recall queued messages, or recall history
            KeyCode::Up => {
                if !self.handle_input_up() {
                    self.handle_input_event_with_history(key);
                }
            }

            // Arrow Down: Move cursor down, or restore newer history
            KeyCode::Down => {
                if !self.handle_input_down() {
                    self.handle_input_event_with_history(key);
                }
            }

            // ? shows help when input is empty
            KeyCode::Char('?') if self.input_state.is_empty() => {
                self.mode = Mode::HelpOverlay;
            }

            _ => {
                self.handle_input_event_with_history(key);
            }
        }
    }

    fn handle_approval_mode(&mut self, key: KeyEvent) {
        if let Some(request) = self.pending_approval.take() {
            let response = match key.code {
                KeyCode::Char('y') | KeyCode::Enter => Some(ApprovalResponse::Yes),
                KeyCode::Char('n') => Some(ApprovalResponse::No),
                KeyCode::Char('a') => Some(ApprovalResponse::AlwaysSession),
                KeyCode::Char('A') => Some(ApprovalResponse::AlwaysPermanent),
                KeyCode::Esc => Some(ApprovalResponse::No),
                _ => None,
            };

            if let Some(resp) = response {
                let _ = request.response_tx.send(resp);
                self.mode = Mode::Input;
            } else {
                self.pending_approval = Some(request);
            }
        }
    }

    /// Set the active API provider and re-create the agent.
    fn set_provider(&mut self, api_provider: Provider) {
        // Get API key (env var first, then config)
        let api_key = self
            .config
            .api_key_for(api_provider.id())
            .unwrap_or_default();

        let provider: Arc<dyn LlmApi> = Arc::new(
            Client::new(api_provider, api_key.clone()).expect("Failed to create LLM client"),
        );

        self.api_provider = api_provider;

        // Save provider to config
        self.config.provider = Some(api_provider.id().to_string());
        if let Err(e) = self.config.save() {
            tracing::warn!("Failed to save config: {}", e);
        }

        // Re-create agent with new provider but same orchestrator
        self.agent = Arc::new(Agent::new(provider, self.orchestrator.clone()));

        // Update model registry with new key/base if it's OpenRouter
        if api_provider == Provider::OpenRouter {
            self.model_registry = Arc::new(ModelRegistry::new(
                api_key,
                self.config.model_cache_ttl_secs,
            ));
        }

        // Set API provider name on model picker
        self.model_picker.set_api_provider(api_provider.name());

        // Clear old models when switching providers
        self.model_picker.set_models(vec![]);
        self.model_picker.is_loading = true;
        self.setup_fetch_started = false;
    }

    /// Open model selector (Ctrl+M or during setup)
    fn open_model_selector(&mut self) {
        self.mode = Mode::Selector;
        self.selector_page = SelectorPage::Model;
        self.model_picker.error = None;

        if self.model_picker.has_models() {
            // Show all models directly (user can type to filter)
            self.model_picker.start_all_models();
        } else {
            // Need to fetch models first - update() will configure picker when they arrive
            self.model_picker.is_loading = true;
            self.setup_fetch_started = true;
            self.fetch_models();
        }
    }

    /// Open API provider selector (Ctrl+P)
    fn open_provider_selector(&mut self) {
        self.mode = Mode::Selector;
        self.selector_page = SelectorPage::Provider;
        self.provider_picker.refresh();
        self.provider_picker.select_provider(self.api_provider);
    }

    fn handle_selector_mode(&mut self, key: KeyEvent) {
        use crate::tui::model_picker::PickerStage;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let mut handled = true;

        match key.code {
            // Ctrl+C: Close selector, double-tap to quit
            KeyCode::Char('c') if ctrl => {
                if let Some(when) = self.cancel_pending
                    && when.elapsed() <= CANCEL_WINDOW
                {
                    self.should_quit = true;
                    self.cancel_pending = None;
                } else {
                    self.cancel_pending = Some(Instant::now());
                    if self.needs_setup && self.selector_page == SelectorPage::Model {
                        self.model_picker.reset();
                        self.open_provider_selector();
                    } else {
                        self.model_picker.reset();
                        self.mode = Mode::Input;
                    }
                }
            }

            // Navigation
            KeyCode::Up => match self.selector_page {
                SelectorPage::Provider => self.provider_picker.move_up(1),
                SelectorPage::Model => self.model_picker.move_up(1),
            },
            KeyCode::Down => match self.selector_page {
                SelectorPage::Provider => self.provider_picker.move_down(1),
                SelectorPage::Model => self.model_picker.move_down(1),
            },
            KeyCode::PageUp => match self.selector_page {
                SelectorPage::Provider => self.provider_picker.move_up(10),
                SelectorPage::Model => self.model_picker.move_up(10),
            },
            KeyCode::PageDown => match self.selector_page {
                SelectorPage::Provider => self.provider_picker.move_down(10),
                SelectorPage::Model => self.model_picker.move_down(10),
            },
            KeyCode::Home => match self.selector_page {
                SelectorPage::Provider => self.provider_picker.jump_to_top(),
                SelectorPage::Model => self.model_picker.jump_to_top(),
            },
            KeyCode::End => match self.selector_page {
                SelectorPage::Provider => self.provider_picker.jump_to_bottom(),
                SelectorPage::Model => self.model_picker.jump_to_bottom(),
            },

            // Selection
            KeyCode::Enter => match self.selector_page {
                SelectorPage::Provider => {
                    if let Some(status) = self.provider_picker.selected()
                        && status.authenticated
                    {
                        let provider = status.provider;
                        self.set_provider(provider);
                        self.open_model_selector();
                    }
                }
                SelectorPage::Model => match self.model_picker.stage {
                    PickerStage::Provider => {
                        self.model_picker.select_provider();
                    }
                    PickerStage::Model => {
                        if let Some(model) = self.model_picker.selected_model() {
                            self.session.model = model.id.clone();
                            if model.context_window > 0 {
                                self.model_context_window = Some(model.context_window as usize);
                            } else {
                                self.model_context_window = None;
                            }
                            // Persist selection to config
                            self.config.model = Some(model.id.clone());
                            if let Err(e) = self.config.save() {
                                tracing::warn!("Failed to save config: {}", e);
                            }
                            self.model_picker.reset();
                            // Complete setup if this was the setup flow
                            if self.needs_setup {
                                self.needs_setup = false;
                            }
                            self.mode = Mode::Input;
                        }
                    }
                },
            },

            // Backspace: when empty on model stage, go back to providers (if allowed)
            KeyCode::Backspace if self.selector_page == SelectorPage::Model => {
                if self.model_picker.filter_input.text().is_empty()
                    && self.model_picker.stage == PickerStage::Model
                    && !self.needs_setup
                {
                    self.model_picker.back_to_providers();
                } else {
                    handled = false;
                }
            }

            // Cancel / Back
            KeyCode::Esc => {
                if self.needs_setup {
                    if self.selector_page == SelectorPage::Model {
                        self.model_picker.reset();
                        self.open_provider_selector();
                    }
                } else {
                    self.model_picker.reset();
                    self.mode = Mode::Input;
                }
            }

            // Tab: switch pages
            KeyCode::Tab => match self.selector_page {
                SelectorPage::Provider => self.open_model_selector(),
                SelectorPage::Model => self.open_provider_selector(),
            },
            KeyCode::Char('p') if ctrl => {
                self.open_provider_selector();
            }
            KeyCode::Char('m') if ctrl => {
                self.open_model_selector();
            }

            _ => {
                handled = false;
            }
        }

        if !handled {
            let event = Event::Key(key);
            let outcome = match self.selector_page {
                SelectorPage::Provider => {
                    text_input::handle_events(&mut self.provider_picker.filter_input, true, &event)
                }
                SelectorPage::Model => {
                    text_input::handle_events(&mut self.model_picker.filter_input, true, &event)
                }
            };

            if matches!(outcome, TextOutcome::TextChanged) {
                match self.selector_page {
                    SelectorPage::Provider => self.provider_picker.apply_filter(),
                    SelectorPage::Model => self.model_picker.apply_filter(),
                }
            }
        }
    }

    /// Fetch models asynchronously
    fn fetch_models(&self) {
        debug!("Starting model fetch");
        let registry = self.model_registry.clone();
        let provider = self.api_provider;
        let prefs = self.config.provider_prefs.clone();
        let agent_tx = self.agent_tx.clone();

        tokio::spawn(async move {
            debug!("Model fetch task started for {:?}", provider);
            match model_picker::fetch_models_for_picker(&registry, provider, &prefs).await {
                Ok(models) => {
                    debug!("Fetched {} models", models.len());
                    let _ = agent_tx.send(AgentEvent::ModelsFetched(models)).await;
                }
                Err(e) => {
                    debug!("Model fetch error: {}", e);
                    let _ = agent_tx
                        .send(AgentEvent::ModelFetchError(e.to_string()))
                        .await;
                }
            }
        });
    }

    /// Save task summary before clearing task state
    fn save_task_summary(&mut self, was_cancelled: bool) {
        if let Some(start) = self.task_start_time {
            self.last_task_summary = Some(TaskSummary {
                elapsed: start.elapsed(),
                input_tokens: self.input_tokens,
                output_tokens: self.output_tokens,
                was_cancelled,
            });
        }
    }

    fn run_agent_task(&mut self, input: String) {
        self.is_running = true;
        self.task_start_time = Some(Instant::now());
        self.input_tokens = 0;
        self.output_tokens = 0;
        self.last_task_summary = None;
        self.last_error = None;

        // Reset cancellation token for new task (tokens are single-use)
        self.session.abort_token = CancellationToken::new();

        // Create shared message queue for mid-task steering
        let queue = Arc::new(std::sync::Mutex::new(Vec::new()));
        self.message_queue = Some(queue.clone());

        let agent = self.agent.clone();
        let session = self.session.clone();
        let event_tx = self.agent_tx.clone();
        let session_tx = self.session_tx.clone();

        // Build thinking config from current level
        let thinking =
            self.thinking_level
                .budget_tokens()
                .map(|budget| crate::provider::ThinkingConfig {
                    enabled: true,
                    budget_tokens: Some(budget),
                });

        tokio::spawn(async move {
            match agent
                .run_task(session, input, event_tx.clone(), Some(queue), thinking)
                .await
            {
                Ok(updated_session) => {
                    // Send completion event
                    let _ = event_tx
                        .send(AgentEvent::Finished("Task completed".to_string()))
                        .await;
                    // Send updated session back to preserve conversation history
                    let _ = session_tx.send(updated_session).await;
                }
                Err(e) => {
                    let _ = event_tx.send(AgentEvent::Error(e.to_string())).await;
                }
            }
        });
    }

    /// Take a snapshot of the current TUI state for debugging.
    pub fn take_snapshot(&mut self) {
        let area = Rect::new(0, 0, 120, 40); // Standard "debug" terminal size
        let backend = ratatui::backend::TestBackend::new(area.width, area.height);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|f| self.draw(f)).unwrap();

        let buffer = terminal.backend().buffer();
        let mut snapshot = String::new();

        for y in 0..area.height {
            for x in 0..area.width {
                let cell = &buffer[(x, y)];
                snapshot.push_str(cell.symbol());
            }
            snapshot.push('\n');
        }

        let path = PathBuf::from("ai/tmp/tui_snapshot.txt");
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, snapshot);
    }

    /// Calculate the height needed for the input box based on content.
    /// Returns height including borders (min 3, max 10).
    fn calculate_input_height(&self, terminal_width: u16) -> u16 {
        const MIN_HEIGHT: u16 = 3;
        const MAX_HEIGHT: u16 = 10;
        const BORDER_OVERHEAD: u16 = 2; // Top and bottom borders
        const GUTTER_WIDTH: u16 = 3; // " > " prompt gutter

        if self.input_state.is_empty() {
            return MIN_HEIGHT;
        }

        // Available width for text (subtract prompt gutter)
        let text_width = terminal_width.saturating_sub(GUTTER_WIDTH) as usize;
        if text_width == 0 {
            return MIN_HEIGHT;
        }

        // Count lines: explicit newlines + wrapped lines
        let input = self.input_state.text();
        let mut lines: Vec<&str> = input.split('\n').collect();
        if lines.len() > 1 && lines.last().is_some_and(|line| line.is_empty()) {
            lines.pop();
        }
        let mut line_count: u16 = 0;
        for line in lines {
            // Each line takes at least 1 row, plus wrapping
            let line_len = line.chars().count();
            let wrapped_lines = if line_len == 0 {
                1
            } else {
                line_len.div_ceil(text_width) as u16
            };
            line_count += wrapped_lines;
        }

        // Add border overhead and clamp to bounds
        (line_count + BORDER_OVERHEAD).clamp(MIN_HEIGHT, MAX_HEIGHT)
    }

    fn input_header_line(&self, width: u16) -> String {
        if width == 0 {
            return String::new();
        }
        "─".repeat(width as usize)
    }

    fn quit(&mut self) {
        self.should_quit = true;

        // Final session save
        if let Err(e) = self.store.save(&self.session) {
            error!("Failed to save session on quit: {}", e);
        }

        // Push a system message about session end
        let end_msg = format!(
            "Session {} closed. {} messages saved.",
            self.session.id,
            self.session.messages.len()
        );
        self.message_list
            .push_entry(crate::tui::message_list::MessageEntry::new(
                crate::tui::Sender::System,
                end_msg,
            ));
    }

    pub fn draw(&mut self, frame: &mut Frame) {
        let show_header = self.message_list.entries.is_empty()
            && !self.is_running
            && self.last_task_summary.is_none()
            && !self.has_queued_messages();
        let header_lines = if show_header {
            self.startup_header_lines()
        } else {
            Vec::new()
        };
        let header_height = header_lines.len() as u16;

        // Calculate input box height based on content
        let input_height = self.calculate_input_height(frame.area().width);

        // Progress line: running, or showing completion summary (until next task)
        let progress_height = if self.is_running || self.last_task_summary.is_some() {
            1
        } else {
            0
        };

        let has_chat = !self.message_list.entries.is_empty() || self.has_queued_messages();

        let chunks = if has_chat || self.is_running || self.last_task_summary.is_some() {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(header_height),
                    Constraint::Min(0),                  // Chat
                    Constraint::Length(progress_height), // Progress line (Ionizing...)
                    Constraint::Length(input_height),    // Input
                    Constraint::Length(1),               // Status line
                ])
                .split(frame.area())
        } else {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(header_height),
                    Constraint::Length(input_height), // Input
                    Constraint::Length(1),            // Status line
                    Constraint::Min(0),               // Spacer
                ])
                .split(frame.area())
        };

        let header_area = chunks[0];
        let (chat_area, progress_area, input_area, status_area) =
            if has_chat || self.is_running || self.last_task_summary.is_some() {
                (chunks[1], chunks[2], chunks[3], chunks[4])
            } else {
                (Rect::default(), Rect::default(), chunks[1], chunks[2])
            };

        if !header_lines.is_empty() {
            frame.render_widget(Paragraph::new(header_lines), header_area);
        }

        let viewport_height = if self.message_list.scroll_offset == 0
            && !chat_lines.is_empty()
            && chat_lines.len() < chat_area.height as usize
        {
            chat_lines.len()
        } else {
            chat_area.height as usize
        };

        let mut chat_lines = Vec::new();
        for entry in &self.message_list.entries {
            match entry.sender {
                Sender::User => {
                    let mut combined = String::new();
                    for part in &entry.parts {
                        if let crate::tui::message_list::MessagePart::Text(text) = part {
                            combined.push_str(text);
                        }
                    }
                    let md = tui_markdown::from_str(&combined);
                    for line in &md.lines {
                        let mut padded = vec![Span::styled("> ", Style::default().fg(Color::Cyan))];
                        padded.extend(
                            line.spans
                                .iter()
                                .map(|span| Span::styled(span.content.to_string(), span.style)),
                        );
                        chat_lines.push(Line::from(padded));
                    }
                }
                Sender::Agent => {
                    for part in &entry.parts {
                        match part {
                            crate::tui::message_list::MessagePart::Text(text) => {
                                // Use custom markdown renderer with syntax highlighting for code blocks
                                let highlighted_lines =
                                    highlight::highlight_markdown_with_code(text);
                                for line in highlighted_lines {
                                    let mut padded = vec![Span::raw(" ")];
                                    padded.extend(line.spans);
                                    chat_lines.push(Line::from(padded));
                                }
                            }
                            crate::tui::message_list::MessagePart::Thinking(thinking) => {
                                for line in thinking.lines() {
                                    chat_lines.push(Line::from(vec![
                                        Span::raw(" "),
                                        Span::styled(line.to_string(), Style::default().dim()),
                                    ]));
                                }
                            }
                        }
                    }
                }
                Sender::Tool => {
                    let content = entry.content_as_markdown();
                    let has_error = content
                        .lines()
                        .any(|line| line.starts_with("⎿ Error:") || line.starts_with("  Error:"));
                    let tool_prefix = if has_error {
                        Span::styled("• ", Style::default().fg(Color::Red))
                    } else {
                        Span::raw("• ")
                    };
                    // Tool messages: first line is call, rest are results
                    let mut lines = content.lines();

                    // First line: **tool_name**(args) - Claude Code style
                    // Also extract tool name and file path for syntax highlighting
                    let mut syntax_name: Option<&str> = None;
                    let mut is_edit_tool = false;

                    if let Some(first_line) = lines.next() {
                        // Parse tool_name(args) format
                        if let Some(paren_pos) = first_line.find('(') {
                            let tool_name = &first_line[..paren_pos];
                            let args = &first_line[paren_pos..];

                            // For read/grep, try to detect syntax from file path
                            if tool_name == "read" || tool_name == "grep" {
                                // Extract path from args: (path) or (path, ...)
                                let path = args
                                    .trim_start_matches('(')
                                    .split(&[',', ')'][..])
                                    .next()
                                    .unwrap_or("");
                                syntax_name = highlight::detect_syntax(path);
                            } else if tool_name == "edit" || tool_name == "write" {
                                is_edit_tool = true;
                            }

                            chat_lines.push(Line::from(vec![
                                tool_prefix.clone(),
                                Span::styled(tool_name, Style::default().bold()),
                                Span::raw(args),
                            ]));
                        } else {
                            // No args, just tool name
                            chat_lines.push(Line::from(vec![
                                tool_prefix.clone(),
                                Span::styled(first_line, Style::default().bold()),
                            ]));
                        }
                    }

                    // Remaining lines: results with styling
                    for line in lines {
                        // For edit/write tools, detect diff lines by content
                        let is_diff_line = is_edit_tool
                            && (line.starts_with('+')
                                || line.starts_with('-')
                                || line.starts_with('@')
                                || line.starts_with(' '));

                        if line.starts_with("⎿ Error:") || line.starts_with("  Error:") {
                            // Error lines in red
                            chat_lines.push(Line::from(vec![
                                Span::raw("  "),
                                Span::styled(line.to_string(), Style::default().fg(Color::Red)),
                            ]));
                        } else if line.starts_with("⎿") || line.starts_with("  … +") {
                            // Success marker and overflow indicators dimmed
                            chat_lines.push(Line::from(vec![
                                Span::raw("  "),
                                Span::styled(line.to_string(), Style::default().dim()),
                            ]));
                        } else if is_diff_line {
                            // Apply diff highlighting for edit/write tool output
                            let mut highlighted = highlight::highlight_diff_line(line);
                            highlighted.spans.insert(0, Span::raw("    "));
                            chat_lines.push(highlighted);
                        } else if line.contains("\x1b[") {
                            // ANSI escape sequences
                            use ansi_to_tui::IntoText;
                            if let Ok(ansi_text) = line.as_bytes().into_text() {
                                for ansi_line in ansi_text.lines {
                                    let mut padded = vec![Span::raw("  ")];
                                    padded.extend(ansi_line.spans.clone());
                                    chat_lines.push(Line::from(padded));
                                }
                            } else {
                                chat_lines.push(Line::from(vec![
                                    Span::raw("  "),
                                    Span::raw(strip_ansi(line)),
                                ]));
                            }
                        } else if let Some(syntax) = syntax_name {
                            // Apply syntax highlighting for code content
                            let code_line = line.strip_prefix("  ").unwrap_or(line);
                            let mut highlighted = highlight::highlight_line(code_line, syntax);
                            // Prepend indent
                            highlighted.spans.insert(0, Span::raw("    "));
                            chat_lines.push(highlighted);
                        } else {
                            // Continuation lines dimmed
                            chat_lines.push(Line::from(vec![
                                Span::raw("  "),
                                Span::styled(line.to_string(), Style::default().dim()),
                            ]));
                        }
                    }
                }
                Sender::System => {
                    let content = entry.content_as_markdown();
                    if content.lines().count() <= 1 {
                        let trimmed = content.trim();
                        if trimmed.starts_with("Error:") {
                            chat_lines.push(Line::from(vec![Span::styled(
                                trimmed.to_string(),
                                Style::default().fg(Color::Red),
                            )]));
                        } else {
                            let text = format!("[{}]", trimmed);
                            chat_lines.push(Line::from(vec![Span::styled(
                                text,
                                Style::default().dim(),
                            )]));
                        }
                    } else {
                        let md = tui_markdown::from_str(content);
                        for line in &md.lines {
                            let mut padded = vec![Span::raw(" ")];
                            padded.extend(line.spans.clone());
                            chat_lines.push(Line::from(padded));
                        }
                    }
                }
            }
            chat_lines.push(Line::from(""));
        }

        // Show queued messages at bottom of chat (dimmed, pending)
        if let Some(ref queue) = self.message_queue
            && let Ok(q) = queue.lock()
        {
            let prefix_style = Style::default().dim();
            let queued_style = Style::default().dim().italic();
            for queued in q.iter() {
                let lines: Vec<&str> = queued.lines().collect();
                let shown = lines.len().min(QUEUED_PREVIEW_LINES);
                for (idx, line) in lines.iter().take(shown).enumerate() {
                    let prefix = if idx == 0 { " > " } else { "   " };
                    chat_lines.push(Line::from(vec![
                        Span::styled(prefix, prefix_style),
                        Span::styled((*line).to_string(), queued_style),
                    ]));
                }
                if lines.len() > shown {
                    chat_lines.push(Line::from(vec![
                        Span::styled("   ", prefix_style),
                        Span::styled("…", queued_style),
                    ]));
                }
                chat_lines.push(Line::from(""));
            }
        }

        // Add a spacer line above the progress line
        if !chat_lines.is_empty() {
            chat_lines.push(Line::from(""));
        }

        // Calculate scroll position
        // scroll_offset is lines from bottom (0 = at bottom)
        let total_lines = chat_lines.len();
        let max_scroll = total_lines.saturating_sub(viewport_height);
        if self.message_list.scroll_offset > max_scroll {
            self.message_list.scroll_offset = max_scroll;
        }
        let scroll_y = max_scroll.saturating_sub(self.message_list.scroll_offset);

        let chat_para = Paragraph::new(chat_lines)
            .wrap(Wrap { trim: true })
            .scroll((scroll_y as u16, 0));
        if chat_area.height > 0 {
            let mut chat_render_area = Rect {
                x: chat_area.x.saturating_add(1),
                y: chat_area.y,
                width: chat_area.width.saturating_sub(2),
                height: chat_area.height,
            };
            if self.message_list.scroll_offset == 0 {
                let content_height = (total_lines.min(chat_render_area.height as usize)) as u16;
                if content_height > 0 && content_height < chat_render_area.height {
                    chat_render_area.y =
                        chat_render_area.y + (chat_render_area.height - content_height);
                    chat_render_area.height = content_height;
                }
            }
            frame.render_widget(chat_para, chat_render_area);
        }

        if self.is_running {
            let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let symbol = spinner[(self.frame_count % spinner.len() as u64) as usize];

            // Show status: cancelling > running tool > ionizing
            let (label, color) = if self.session.abort_token.is_cancelled() {
                ("Cancelling...".to_string(), Color::Red)
            } else if let Some(tool) = &self.current_tool {
                (format!("Running {}...", tool), Color::Cyan)
            } else {
                ("Ionizing...".to_string(), Color::Cyan)
            };

            let mut progress_spans = vec![
                Span::styled(format!(" {} ", symbol), Style::default().fg(color)),
                Span::styled(label, Style::default().fg(color)),
            ];

            // Build stats in parens: (elapsed · ↑input · ↓output)
            let mut stats = Vec::new();

            // Elapsed time
            if let Some(start) = self.task_start_time {
                let elapsed = start.elapsed();
                let secs = elapsed.as_secs();
                if secs >= 60 {
                    stats.push(format!("{}m {}s", secs / 60, secs % 60));
                } else {
                    stats.push(format!("{}s", secs));
                }
            }

            if self.input_tokens > 0 {
                stats.push(format!("↑ {}", format_tokens(self.input_tokens)));
            }
            if self.output_tokens > 0 {
                stats.push(format!("↓ {}", format_tokens(self.output_tokens)));
            }

            if !stats.is_empty() {
                progress_spans.push(Span::styled(
                    format!(" ({})", stats.join(" · ")),
                    Style::default().dim(),
                ));
            }

            let progress_line = Line::from(progress_spans);
            frame.render_widget(Paragraph::new(progress_line), progress_area);
        } else if let Some(summary) = &self.last_task_summary {
            // Show completion/cancellation summary until next task starts
            let secs = summary.elapsed.as_secs();
            let elapsed_str = if secs >= 60 {
                format!("{}m {}s", secs / 60, secs % 60)
            } else {
                format!("{}s", secs)
            };

            let mut stats = vec![elapsed_str];
            if summary.input_tokens > 0 {
                stats.push(format!("↑ {}", format_tokens(summary.input_tokens)));
            }
            if summary.output_tokens > 0 {
                stats.push(format!("↓ {}", format_tokens(summary.output_tokens)));
            }

            let (symbol, label, color) = if self.last_error.is_some() {
                (" ✗ ", "Error", Color::Red)
            } else if summary.was_cancelled {
                (" ✗ ", "Cancelled", Color::Red)
            } else {
                (" ✓ ", "Completed", Color::Green)
            };

            let summary_line = Line::from(vec![
                Span::styled(symbol, Style::default().fg(color)),
                Span::styled(label, Style::default().fg(color)),
                Span::styled(format!(" ({})", stats.join(" · ")), Style::default().dim()),
            ]);
            frame.render_widget(Paragraph::new(summary_line), progress_area);
        }

        if self.mode == Mode::Selector {
            self.render_selector_shell(frame);
        } else {
            // Input or Approval Prompt (input always visible except during approval)
            if self.mode == Mode::Approval {
                if let Some(req) = &self.pending_approval {
                    let prompt = format!(
                        " [Approval] Allow {}? (y)es / (n)o / (a)lways / (A)lways permanent ",
                        req.tool_name
                    );
                    let approval_block = Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Red).bold())
                        .title(" Action Required ");
                    let approval_para = Paragraph::new(prompt).block(approval_block);
                    frame.render_widget(approval_para, input_area);
                }
            } else {
                // Input box always visible
                let input_area = input_area;
                if input_area.width > 0 && input_area.height > 1 {
                    let top_area = Rect {
                        x: input_area.x,
                        y: input_area.y,
                        width: input_area.width,
                        height: 1,
                    };
                    let bottom_area = Rect {
                        x: input_area.x,
                        y: input_area.y + input_area.height - 1,
                        width: input_area.width,
                        height: 1,
                    };
                    let text_area = Rect {
                        x: input_area.x,
                        y: input_area.y + 1,
                        width: input_area.width,
                        height: input_area.height.saturating_sub(2),
                    };

                    let header = self.input_header_line(input_area.width);
                    let bar_style = Style::default().fg(Color::Cyan);
                    frame.render_widget(
                        Paragraph::new(Line::from(Span::styled(header, bar_style))),
                        top_area,
                    );
                    frame.render_widget(
                        Paragraph::new("─".repeat(input_area.width as usize))
                            .style(bar_style),
                        bottom_area,
                    );

                    if text_area.width > 0 && text_area.height > 0 {
                        let gutter_width = text_area.width.min(3);
                        if gutter_width > 0 {
                            let prompt_area = Rect {
                                x: text_area.x,
                                y: text_area.y,
                                width: gutter_width,
                                height: text_area.height,
                            };
                            let prompt = match gutter_width {
                                1 => ">".to_string(),
                                2 => "> ".to_string(),
                                _ => " > ".to_string(),
                            };
                            let blank = " ".repeat(gutter_width as usize);
                            let prompt_lines: Vec<Line> = (0..text_area.height)
                                .map(|row| {
                                    let symbol = if row == 0 { &prompt } else { &blank };
                                    Line::from(Span::styled(symbol.clone(), Style::default().dim()))
                                })
                                .collect();
                            frame.render_widget(Paragraph::new(prompt_lines), prompt_area);
                        }

                        let entry_area = Rect {
                            x: text_area.x + gutter_width,
                            y: text_area.y,
                            width: text_area.width.saturating_sub(gutter_width),
                            height: text_area.height,
                        };

                        let input = TextArea::new().text_wrap(TextWrap::Word(1));
                        frame.render_stateful_widget(input, entry_area, &mut self.input_state);
                    }
                }

                if let Some(cursor) = self.input_state.screen_cursor() {
                    frame.set_cursor_position(cursor);
                }
            }

            // Status line: model · context%

            // Simplify model name (remove provider prefix if present)
            let model_name = self
                .session
                .model
                .split('/')
                .next_back()
                .unwrap_or(&self.session.model);

            // Build status line left side
            let mode_label = match self.tool_mode {
                ToolMode::Read => "READ",
                ToolMode::Write => "WRITE",
                ToolMode::Agi => "AGI",
            };
            let mode_color = match self.tool_mode {
                ToolMode::Read => Color::Cyan,
                ToolMode::Write => Color::Yellow,
                ToolMode::Agi => Color::Red,
            };

            let mut left_spans: Vec<Span> = vec![
                Span::raw(" "),
                Span::raw("["),
                Span::styled(mode_label, Style::default().fg(mode_color)),
                Span::raw("]"),
                Span::raw(" · "),
                Span::raw(model_name),
            ];

            // Context usage display with token counts
            if let Some((used, _max)) = self.token_usage {
                let format_k = |n: usize| -> String {
                    if n >= 1000 {
                        format!("{}k", n / 1000)
                    } else {
                        n.to_string()
                    }
                };
                if let Some(max) = self.model_context_window {
                    let pct = if max > 0 { (used * 100) / max } else { 0 };
                    left_spans.push(Span::raw(" · "));
                    left_spans.push(Span::raw(format!(
                        "{}% ({}/{})",
                        pct,
                        format_k(used),
                        format_k(max)
                    )));
                } else {
                    left_spans.push(Span::raw(" · "));
                    left_spans.push(Span::raw(format_k(used)));
                }
            }

            // Calculate left side length for padding
            let left_len: usize = left_spans.iter().map(|s| s.content.chars().count()).sum();

            // Right side: help hint
            let (right, right_style) = ("? help ".to_string(), Style::default().dim());

            // Calculate padding for right alignment
            let width = status_area.width as usize;
            let right_len = right.chars().count();
            let padding = width.saturating_sub(left_len + right_len);

            // Build final status line: left spans + padding + right
            let mut status_spans = left_spans;
            status_spans.push(Span::raw(" ".repeat(padding)));
            status_spans.push(Span::styled(right, right_style));
            let status_line = Line::from(status_spans);

            frame.render_widget(Paragraph::new(status_line), status_area);
        }

        // Render overlays on top if active
        if self.mode == Mode::HelpOverlay {
            self.render_help_overlay(frame);
        }
    }

    fn render_selector_shell(&mut self, frame: &mut Frame) {
        let area = frame.area();

        let (title, description, list_len) = match self.selector_page {
            SelectorPage::Provider => (
                "Providers",
                "Select a provider",
                self.provider_picker.filtered.len(),
            ),
            SelectorPage::Model => (
                "Models",
                "Select a model",
                self.model_picker.filtered_models.len(),
            ),
        };

        let reserved_height = 1 + 1 + 3 + 1;
        let max_list_height = area.height.saturating_sub(reserved_height);
        let list_height = (list_len as u16).clamp(3, max_list_height.max(3));
        let total_height = reserved_height + list_height;

        let y = area.height.saturating_sub(total_height);
        let shell_area = Rect::new(0, y, area.width, total_height);

        frame.render_widget(Clear, shell_area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // Tabs
                Constraint::Length(1), // Description
                Constraint::Length(3), // Search
                Constraint::Length(list_height),
                Constraint::Length(1), // Hint
            ])
            .split(shell_area);

        let (provider_style, model_style) = match self.selector_page {
            SelectorPage::Provider => (
                Style::default().fg(Color::Yellow).bold(),
                Style::default().dim(),
            ),
            SelectorPage::Model => (
                Style::default().dim(),
                Style::default().fg(Color::Yellow).bold(),
            ),
        };

        let tabs = Line::from(vec![
            Span::raw(" "),
            Span::styled("Providers", provider_style),
            Span::raw("  "),
            Span::styled("Models", model_style),
        ]);
        frame.render_widget(Paragraph::new(tabs), chunks[0]);

        frame.render_widget(
            Paragraph::new(Line::from(vec![Span::raw(" "), Span::raw(description)])),
            chunks[1],
        );

        let search_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(format!(" {} ", title));

        let search_input = TextInput::new().block(search_block);
        match self.selector_page {
            SelectorPage::Provider => {
                frame.render_stateful_widget(
                    search_input,
                    chunks[2],
                    &mut self.provider_picker.filter_input,
                );
                if let Some(cursor) = self.provider_picker.filter_input.screen_cursor() {
                    frame.set_cursor_position(cursor);
                }
            }
            SelectorPage::Model => {
                frame.render_stateful_widget(
                    search_input,
                    chunks[2],
                    &mut self.model_picker.filter_input,
                );
                if let Some(cursor) = self.model_picker.filter_input.screen_cursor() {
                    frame.set_cursor_position(cursor);
                }
            }
        }

        match self.selector_page {
            SelectorPage::Provider => {
                let items: Vec<ListItem> = self
                    .provider_picker
                    .filtered
                    .iter()
                    .map(|status| {
                        let (icon, icon_style, name_style) = if status.authenticated {
                            (
                                "●",
                                Style::default().fg(Color::Green),
                                Style::default().fg(Color::White).bold(),
                            )
                        } else {
                            ("○", Style::default().dim(), Style::default().dim())
                        };

                        let auth_hint = if !status.authenticated {
                            format!(
                                " set {}",
                                status.provider.env_vars().first().unwrap_or(&"API_KEY")
                            )
                        } else {
                            String::new()
                        };

                        ListItem::new(Line::from(vec![
                            Span::styled(icon, icon_style),
                            Span::raw(" "),
                            Span::styled(status.provider.name(), name_style),
                            Span::styled(auth_hint, Style::default().fg(Color::Red).dim()),
                        ]))
                    })
                    .collect();

                let count = self.provider_picker.filtered.len();
                let total = self.provider_picker.providers.len();
                let list = List::new(items)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(format!(" Providers ({}/{}) ", count, total)),
                    )
                    .highlight_style(
                        Style::default()
                            .bg(Color::DarkGray)
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    )
                    .highlight_symbol("▸ ");

                frame.render_stateful_widget(list, chunks[3], &mut self.provider_picker.list_state);
            }
            SelectorPage::Model => {
                if self.model_picker.is_loading {
                    let provider_name = self
                        .model_picker
                        .api_provider_name
                        .as_deref()
                        .unwrap_or("provider");
                    let loading =
                        Paragraph::new(format!("Loading models from {}...", provider_name))
                            .style(Style::default().fg(Color::Yellow))
                            .block(Block::default().borders(Borders::ALL).title(" Loading "));
                    frame.render_widget(loading, chunks[3]);
                } else if let Some(ref err) = self.model_picker.error {
                    let error = Paragraph::new(format!("Error: {}", err))
                        .style(Style::default().fg(Color::Red))
                        .block(Block::default().borders(Borders::ALL).title(" Error "));
                    frame.render_widget(error, chunks[3]);
                } else {
                    let items: Vec<ListItem> = self
                        .model_picker
                        .filtered_models
                        .iter()
                        .map(|model| {
                            let context_k = model.context_window / 1000;
                            ListItem::new(Line::from(vec![
                                Span::styled(model.id.clone(), Style::default().fg(Color::White)),
                                Span::styled(
                                    format!("  {}k ctx", context_k),
                                    Style::default().dim(),
                                ),
                            ]))
                        })
                        .collect();

                    let count = self.model_picker.filtered_models.len();
                    let total = self.model_picker.all_models.len();
                    let list = List::new(items)
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .title(format!(" Models ({}/{}) ", count, total)),
                        )
                        .highlight_style(
                            Style::default()
                                .bg(Color::DarkGray)
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD),
                        )
                        .highlight_symbol("▸ ");

                    frame.render_stateful_widget(
                        list,
                        chunks[3],
                        &mut self.model_picker.model_state,
                    );
                }
            }
        }

        let hint = Paragraph::new(" Type to filter · Enter to select · Esc to close ")
            .style(Style::default().dim());
        frame.render_widget(hint, chunks[4]);
    }

    fn render_help_overlay(&self, frame: &mut Frame) {
        let area = frame.area();
        // Fixed size modal, centered (40 inner width for clean columns)
        let width = 44.min(area.width.saturating_sub(4));
        let height = 20.min(area.height.saturating_sub(4));
        let x = (area.width.saturating_sub(width)) / 2;
        let y = (area.height.saturating_sub(height)) / 2;
        let help_area = Rect::new(x, y, width, height);

        frame.render_widget(ratatui::widgets::Clear, help_area);

        // Helper to create a row: key (col 1-18), description (col 19+)
        let row = |key: &str, desc: &str| {
            Line::from(vec![
                Span::styled(format!(" {:<17}", key), Style::default().fg(Color::Cyan)),
                Span::raw(desc.to_string()),
            ])
        };

        let help_text = vec![
            Line::from(Span::styled(
                "Keybindings",
                Style::default().fg(Color::Yellow).bold(),
            ))
            .alignment(ratatui::layout::Alignment::Center),
            row("Enter", "Send message"),
            row("Shift+Enter", "Insert newline"),
            row("Shift+Tab", "Cycle mode"),
            row("Ctrl+G", "External editor"),
            row("Ctrl+M", "Model selector"),
            row("Ctrl+P", "Provider selector"),
            row("Ctrl+T", "Thinking toggle"),
            row("Ctrl+C", "Clear (double-tap cancel/quit)"),
            row("PgUp/PgDn", "Scroll chat"),
            Line::from(""),
            Line::from(Span::styled(
                "Commands",
                Style::default().fg(Color::Yellow).bold(),
            ))
            .alignment(ratatui::layout::Alignment::Center),
            row("/model", "Select model"),
            row("/provider", "Select provider"),
            row("/clear", "Clear chat"),
            row("/quit", "Exit"),
            Line::from(""),
            Line::from(Span::styled(
                "Press any key to close",
                Style::default().dim(),
            ))
            .alignment(ratatui::layout::Alignment::Center),
        ];

        let help_para = Paragraph::new(help_text)
            .block(Block::default().borders(Borders::ALL).title(" ? Help "));

        frame.render_widget(help_para, help_area);
    }
}

/// Strip ANSI escape sequences from a string.
fn strip_ansi(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut in_escape = false;
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            in_escape = true;
            chars.next(); // consume '['
            continue;
        }
        if in_escape {
            // End of escape sequence on 'm' or other command char
            if c.is_ascii_alphabetic() {
                in_escape = false;
            }
            continue;
        }
        result.push(c);
    }
    result
}
