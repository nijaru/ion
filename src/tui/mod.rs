pub mod message_list;
pub mod model_picker;
pub mod provider_picker;
pub mod widgets;

use crate::agent::{Agent, AgentEvent};
use crate::cli::PermissionSettings;
use crate::config::Config;
use crate::provider::{ApiProvider, ModelRegistry};
use crate::session::Session;
use crate::session::SessionStore;
use crate::tool::{ApprovalHandler, ApprovalResponse, ToolMode, ToolOrchestrator};
use crate::tui::message_list::{MessageList, Sender};
use crate::tui::model_picker::ModelPicker;
use crate::tui::provider_picker::ProviderPicker;
use async_trait::async_trait;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

const CANCEL_WINDOW: Duration = Duration::from_millis(1500);
const SUMMARY_DISPLAY: Duration = Duration::from_secs(5);

/// Format token count as human-readable (e.g., 1500 -> "1.5k")
fn format_tokens(n: usize) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
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
    /// Model selection modal (Ctrl+M)
    ModelPicker,
    /// API provider selection modal (Ctrl+P)
    ProviderPicker,
    /// Keybinding help overlay (Ctrl+H)
    HelpOverlay,
}

pub struct ApprovalRequest {
    pub tool_name: String,
    pub args: serde_json::Value,
    pub response_tx: oneshot::Sender<ApprovalResponse>,
}

pub struct App {
    pub mode: Mode,
    pub should_quit: bool,
    pub input: String,
    /// Cursor position within the input string
    pub cursor_pos: usize,
    /// Input history for arrow-up recall
    pub input_history: Vec<String>,
    /// Current position in history (input_history.len() = current input)
    pub history_index: usize,
    /// Current tool permission mode (Read/Write/Agi)
    pub tool_mode: ToolMode,
    /// Currently selected API provider
    pub api_provider: ApiProvider,
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
    /// Timestamp of first Ctrl+C press for double-Ctrl+C quit
    pub cancel_pending: Option<Instant>,
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
    /// Permission settings from CLI flags
    pub permissions: PermissionSettings,
    /// Last completed task summary (for brief display after completion)
    pub last_task_summary: Option<TaskSummary>,
}

/// Summary of a completed task for brief post-completion display
#[derive(Clone)]
pub struct TaskSummary {
    pub elapsed: std::time::Duration,
    pub input_tokens: usize,
    pub output_tokens: usize,
    pub completed_at: Instant,
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

use unicode_segmentation::UnicodeSegmentation;

impl App {
    fn move_cursor_left(&mut self) {
        let new_pos = self.input[..self.cursor_pos]
            .grapheme_indices(true)
            .rev()
            .next()
            .map(|(i, _)| i)
            .unwrap_or(0);
        self.cursor_pos = new_pos;
    }

    fn move_cursor_right(&mut self) {
        let new_pos = self.input[self.cursor_pos..]
            .grapheme_indices(true)
            .next()
            .map(|(_, g)| self.cursor_pos + g.len())
            .unwrap_or(self.input.len());
        self.cursor_pos = new_pos;
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
        } else {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
                .try_init();
        }

        // Determine active provider and key
        let (api_provider, api_key) = if let Some(key) = config.openrouter_api_key.clone() {
            (ApiProvider::OpenRouter, key)
        } else if let Some(key) = config.anthropic_api_key.clone() {
            (ApiProvider::Anthropic, key)
        } else {
            (ApiProvider::OpenRouter, "".to_string())
        };

        let provider_impl = crate::provider::create_provider(
            api_provider,
            api_key.clone(),
            config.provider_prefs.clone(),
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
        if local_mcp_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&local_mcp_path) {
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
        let initial_mode = if needs_setup {
            if !config.has_api_key() {
                Mode::ProviderPicker
            } else {
                Mode::ModelPicker
            }
        } else {
            Mode::Input
        };

        let mut this = Self {
            mode: initial_mode,
            should_quit: false,
            input: String::new(),
            cursor_pos: 0,
            input_history: Vec::new(),
            history_index: 0,
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
            cancel_pending: None,
            store,
            model_picker: ModelPicker::new(config.provider_prefs.clone()),
            model_registry,
            config,
            frame_count: 0,
            needs_setup,
            setup_fetch_started: false,
            thinking_level: ThinkingLevel::Off,
            token_usage: None,
            last_error: None,
            message_queue: None,
            task_start_time: None,
            input_tokens: 0,
            output_tokens: 0,
            current_tool: None,
            permissions,
            last_task_summary: None,
        };

        // Initialize setup flow if needed
        if this.needs_setup {
            if this.mode == Mode::ProviderPicker {
                this.provider_picker.refresh();
            } else if this.mode == Mode::ModelPicker {
                this.model_picker.is_loading = true;
                // Models will be fetched when run loop starts
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
            self.mode = Mode::ProviderPicker;
            self.provider_picker.refresh();
        }

        // Start fetching models if in setup mode and model picker needs them
        if self.needs_setup
            && self.mode == Mode::ModelPicker
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
                AgentEvent::Finished(_) | AgentEvent::Error(_) => {
                    self.save_task_summary();
                    self.is_running = false;
                    self.cancel_pending = None;
                    self.message_queue = None;
                    self.task_start_time = None;
                    self.current_tool = None;
                    self.message_list.push_event(event);
                }
                AgentEvent::ModelsFetched(models) => {
                    debug!("Received ModelsFetched event with {} models", models.len());
                    self.model_picker.set_models(models.clone());
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
                AgentEvent::ToolCallStart(_, name) => {
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
            self.save_task_summary();
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
                Mode::ModelPicker => self.handle_model_picker_mode(key),
                Mode::ProviderPicker => self.handle_provider_picker_mode(key),
                Mode::HelpOverlay => {
                    self.mode = Mode::Input;
                }
            }
        }
    }

    /// Main input handler - always active unless a modal is open
    fn handle_input_mode(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            // Ctrl+C: Cancel running task / clear input / quit
            KeyCode::Char('c') if ctrl => {
                if self.is_running {
                    // Immediately cancel running task - no double-tap needed
                    self.session.abort_token.cancel();
                    self.cancel_pending = None;
                } else if !self.input.is_empty() {
                    // Clear input if not empty
                    self.input.clear();
                    self.cursor_pos = 0;
                } else {
                    // Empty input, not running - double-tap to quit
                    if let Some(when) = self.cancel_pending {
                        if when.elapsed() <= CANCEL_WINDOW {
                            self.quit();
                        } else {
                            self.cancel_pending = Some(Instant::now());
                        }
                    } else {
                        self.cancel_pending = Some(Instant::now());
                    }
                }
            }

            // Ctrl+D: Quit if input empty
            KeyCode::Char('d') if ctrl => {
                if self.input.is_empty() {
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
                    self.open_model_picker();
                }
            }

            // Ctrl+P: Provider → Model picker (two-stage)
            KeyCode::Char('p') if ctrl => {
                if !self.is_running {
                    self.open_provider_picker();
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

            // Shift+Enter: Insert newline (requires Kitty keyboard protocol)
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.input.insert(self.cursor_pos, '\n');
                self.cursor_pos += 1;
            }

            // Enter: Send message or queue for mid-task steering
            KeyCode::Enter => {
                if !self.input.is_empty() {
                    if self.is_running {
                        // Queue message for injection at next turn
                        if let Some(ref queue) = self.message_queue {
                            let msg = std::mem::take(&mut self.input);
                            self.cursor_pos = 0;
                            if let Ok(mut q) = queue.lock() {
                                q.push(msg);
                            }
                        }
                    } else {
                        // Check for slash commands
                        if self.input.starts_with('/') {
                            let cmd = self.input.trim().to_lowercase();
                            match cmd.as_str() {
                                "/model" | "/models" => {
                                    self.input.clear();
                                    self.cursor_pos = 0;
                                    self.open_model_picker();
                                    return;
                                }
                                "/provider" | "/providers" => {
                                    self.input.clear();
                                    self.cursor_pos = 0;
                                    self.open_provider_picker();
                                    return;
                                }
                                "/quit" | "/exit" | "/q" => {
                                    self.input.clear();
                                    self.quit();
                                    return;
                                }
                                "/clear" => {
                                    self.input.clear();
                                    self.cursor_pos = 0;
                                    self.message_list.clear();
                                    self.session.messages.clear();
                                    return;
                                }
                                "/help" | "/?" => {
                                    self.input.clear();
                                    self.cursor_pos = 0;
                                    self.mode = Mode::HelpOverlay;
                                    return;
                                }
                                _ => {} // Fall through to normal message if unknown command
                            }
                        }

                        // Send message
                        let input = std::mem::take(&mut self.input);
                        self.input_history.push(input.clone());
                        self.history_index = self.input_history.len();
                        self.cursor_pos = 0;
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
                // Check if cursor is at start of input (or no newlines above)
                let at_top = self.cursor_pos == 0 || !self.input[..self.cursor_pos].contains('\n');
                if at_top {
                    // If running and queue has messages, pop from queue first
                    if self.is_running && self.input.is_empty() {
                        if let Some(ref queue) = self.message_queue {
                            if let Ok(mut q) = queue.lock() {
                                if let Some(msg) = q.pop() {
                                    self.input = msg;
                                    self.cursor_pos = self.input.len();
                                    return;
                                }
                            }
                        }
                    }
                    // Fall back to input history
                    if !self.input_history.is_empty() && self.history_index > 0 {
                        self.history_index -= 1;
                        self.input = self.input_history[self.history_index].clone();
                        self.cursor_pos = self.input.len();
                    }
                } else {
                    // Move cursor up one line
                    self.move_cursor_vertically(-1);
                }
            }

            // Arrow Down: Move cursor down, or restore newer history
            KeyCode::Down => {
                let at_bottom = self.cursor_pos == self.input.len()
                    || !self.input[self.cursor_pos..].contains('\n');
                if at_bottom {
                    if self.history_index < self.input_history.len() {
                        self.history_index += 1;
                        if self.history_index == self.input_history.len() {
                            self.input.clear();
                        } else {
                            self.input = self.input_history[self.history_index].clone();
                        }
                        self.cursor_pos = self.input.len();
                    }
                } else {
                    self.move_cursor_vertically(1);
                }
            }

            // Arrow Left/Right: Move cursor
            KeyCode::Left => {
                if ctrl {
                    // Word-by-word movement
                    self.cursor_pos = self.find_word_boundary_left();
                } else {
                    self.move_cursor_left();
                }
            }
            KeyCode::Right => {
                if ctrl {
                    self.cursor_pos = self.find_word_boundary_right();
                } else {
                    self.move_cursor_right();
                }
            }

            // Home/End: Line start/end
            KeyCode::Home => {
                // Find start of current line
                self.cursor_pos = self.input[..self.cursor_pos]
                    .rfind('\n')
                    .map(|i| i + 1)
                    .unwrap_or(0);
            }
            KeyCode::End => {
                // Find end of current line
                self.cursor_pos = self.input[self.cursor_pos..]
                    .find('\n')
                    .map(|i| self.cursor_pos + i)
                    .unwrap_or(self.input.len());
            }

            // Backspace: Delete char before cursor
            KeyCode::Backspace => {
                if self.cursor_pos > 0 {
                    self.move_cursor_left();
                    self.input.remove(self.cursor_pos);
                }
            }

            // Delete: Delete char at cursor
            KeyCode::Delete => {
                if self.cursor_pos < self.input.len() {
                    self.input.remove(self.cursor_pos);
                }
            }

            // ? shows help when input is empty
            KeyCode::Char('?') if self.input.is_empty() => {
                self.mode = Mode::HelpOverlay;
            }

            // Character input
            KeyCode::Char(c) => {
                if !ctrl {
                    self.input.insert(self.cursor_pos, c);
                    self.cursor_pos += c.len_utf8();
                }
            }

            _ => {}
        }
    }

    /// Move cursor vertically by n lines (negative = up)
    fn move_cursor_vertically(&mut self, delta: i32) {
        if delta == 0 || self.input.is_empty() {
            return;
        }

        let lines: Vec<&str> = self.input.split('\n').collect();
        let mut pos = 0;
        let mut current_line = 0;
        let mut col = 0;

        // Find current line and column
        for (i, line) in lines.iter().enumerate() {
            if pos + line.len() >= self.cursor_pos {
                current_line = i;
                col = self.cursor_pos - pos;
                break;
            }
            pos += line.len() + 1; // +1 for newline
        }

        // Calculate target line
        let target_line = (current_line as i32 + delta).clamp(0, lines.len() as i32 - 1) as usize;

        // Calculate new position
        let mut new_pos = 0;
        for line in &lines[..target_line] {
            new_pos += line.len() + 1;
        }
        new_pos += col.min(lines[target_line].len());

        self.cursor_pos = new_pos;
    }

    fn find_word_boundary_left(&self) -> usize {
        if self.cursor_pos == 0 {
            return 0;
        }
        let bytes = self.input.as_bytes();
        let mut pos = self.cursor_pos - 1;

        // Skip whitespace
        while pos > 0 && bytes[pos].is_ascii_whitespace() {
            pos -= 1;
        }
        // Skip word chars
        while pos > 0 && !bytes[pos - 1].is_ascii_whitespace() {
            pos -= 1;
        }
        pos
    }

    fn find_word_boundary_right(&self) -> usize {
        let len = self.input.len();
        if self.cursor_pos >= len {
            return len;
        }
        let bytes = self.input.as_bytes();
        let mut pos = self.cursor_pos;

        // Skip current word
        while pos < len && !bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        // Skip whitespace
        while pos < len && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        pos
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

    fn handle_model_picker_mode(&mut self, key: KeyEvent) {
        use crate::tui::model_picker::PickerStage;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            // Ctrl+C: Quit (always works, even during setup)
            KeyCode::Char('c') if ctrl => {
                self.should_quit = true;
            }

            // Navigation: Arrow keys primary, j/k as alternatives (only without ctrl)
            KeyCode::Up => self.model_picker.move_up(1),
            KeyCode::Down => self.model_picker.move_down(1),
            KeyCode::Char('k') if !ctrl => self.model_picker.move_up(1),
            KeyCode::Char('j') if !ctrl => self.model_picker.move_down(1),

            // Page navigation
            KeyCode::PageUp => self.model_picker.move_up(10),
            KeyCode::PageDown => self.model_picker.move_down(10),
            KeyCode::Home => self.model_picker.jump_to_top(),
            KeyCode::End => self.model_picker.jump_to_bottom(),

            // Selection
            KeyCode::Enter => match self.model_picker.stage {
                PickerStage::Provider => {
                    self.model_picker.select_provider();
                }
                PickerStage::Model => {
                    if let Some(model) = self.model_picker.selected_model() {
                        self.session.model = model.id.clone();
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

            // Back navigation
            KeyCode::Backspace if self.model_picker.filter.is_empty() => {
                if self.model_picker.stage == PickerStage::Model {
                    // During setup, don't allow going back to provider stage from model picker
                    // (they already selected provider in previous step)
                    if !self.needs_setup {
                        self.model_picker.back_to_providers();
                    }
                }
            }
            KeyCode::Backspace => {
                self.model_picker.pop_char();
            }

            // Cancel / Back
            KeyCode::Esc => {
                if self.needs_setup {
                    // During setup, Esc goes back to provider picker
                    self.model_picker.reset();
                    self.mode = Mode::ProviderPicker;
                    self.provider_picker.refresh();
                } else {
                    self.model_picker.reset();
                    self.mode = Mode::Input;
                }
            }

            // Tab: Switch to provider picker
            KeyCode::Tab => {
                self.model_picker.reset();
                self.mode = Mode::ProviderPicker;
                self.provider_picker.refresh();
            }

            // Type to filter (only regular chars without ctrl modifier)
            KeyCode::Char(c) if !ctrl => {
                self.model_picker.push_char(c);
            }

            _ => {}
        }
    }

    /// Switch the active API provider and re-create the agent.
    fn switch_provider(&mut self, api_provider: ApiProvider) {
        if let Some(api_key) = api_provider.api_key() {
            self.api_provider = api_provider;

            let provider = crate::provider::create_provider(
                api_provider,
                api_key.clone(),
                self.config.provider_prefs.clone(),
            );

            // Re-create agent with new provider but same orchestrator
            self.agent = Arc::new(Agent::new(provider, self.orchestrator.clone()));

            // Update model registry with new key/base if it's OpenRouter
            // Future: Support direct model fetching for other providers
            if api_provider == ApiProvider::OpenRouter {
                self.model_registry = Arc::new(ModelRegistry::new(
                    api_key,
                    self.config.model_cache_ttl_secs,
                ));
            }

            self.mode = Mode::Input;
            self.open_model_picker();
        }
    }

    /// Open model picker (Ctrl+M or during setup)
    fn open_model_picker(&mut self) {
        self.mode = Mode::ModelPicker;
        self.model_picker.error = None;

        if self.model_picker.has_models() {
            // Show all models directly (user can type to filter)
            self.model_picker.start_all_models();
        } else {
            // Need to fetch models first - update() will configure picker when they arrive
            self.model_picker.is_loading = true;
            self.fetch_models();
        }
    }

    /// Open API provider picker (Ctrl+P)
    fn open_provider_picker(&mut self) {
        self.mode = Mode::ProviderPicker;
        self.provider_picker.refresh();
    }

    /// Handle API provider picker mode
    fn handle_provider_picker_mode(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            // Ctrl+C: Quit (always works, even during setup)
            KeyCode::Char('c') if ctrl => {
                self.should_quit = true;
            }

            // Navigation
            KeyCode::Up => self.provider_picker.move_up(1),
            KeyCode::Down => self.provider_picker.move_down(1),
            KeyCode::PageUp => self.provider_picker.move_up(10),
            KeyCode::PageDown => self.provider_picker.move_down(10),
            KeyCode::Home => self.provider_picker.jump_to_top(),
            KeyCode::End => self.provider_picker.jump_to_bottom(),

            // Selection
            KeyCode::Enter => {
                if let Some(status) = self.provider_picker.selected() {
                    if status.authenticated && status.implemented {
                        let provider = status.provider;
                        self.switch_provider(provider);
                        // During setup, chain to model picker
                        if self.needs_setup {
                            self.open_model_picker();
                        }
                    }
                    // If not authenticated/implemented, do nothing (can't select)
                }
            }

            // Cancel - always allow, update() will re-trigger setup if needed
            KeyCode::Esc => {
                self.mode = Mode::Input;
            }

            // Tab: Switch to model picker (only if models are loaded)
            KeyCode::Tab => {
                if self.model_picker.has_models() {
                    self.model_picker.start_all_models();
                    self.mode = Mode::ModelPicker;
                }
            }

            // Type to filter (only without ctrl, except Ctrl+W for delete word)
            KeyCode::Char('w') if ctrl => {
                self.provider_picker.delete_word();
            }
            KeyCode::Char(c) if !ctrl => {
                self.provider_picker.push_char(c);
            }
            KeyCode::Backspace => {
                self.provider_picker.pop_char();
            }

            _ => {}
        }
    }

    /// Fetch models asynchronously
    fn fetch_models(&self) {
        debug!("Starting model fetch");
        let registry = self.model_registry.clone();
        let provider = self.agent.provider();
        let prefs = self.config.provider_prefs.clone();
        let agent_tx = self.agent_tx.clone();

        tokio::spawn(async move {
            debug!("Model fetch task started");
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
    fn save_task_summary(&mut self) {
        if let Some(start) = self.task_start_time {
            self.last_task_summary = Some(TaskSummary {
                elapsed: start.elapsed(),
                input_tokens: self.input_tokens,
                output_tokens: self.output_tokens,
                completed_at: Instant::now(),
            });
        }
    }

    fn run_agent_task(&mut self, input: String) {
        self.is_running = true;
        self.task_start_time = Some(Instant::now());
        self.input_tokens = 0;
        self.output_tokens = 0;
        self.last_task_summary = None;

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
        let thinking = self.thinking_level.budget_tokens().map(|budget| {
            crate::provider::ThinkingConfig {
                enabled: true,
                budget_tokens: Some(budget),
            }
        });

        tokio::spawn(async move {
            match agent
                .run_task(session, input, event_tx.clone(), Some(queue), thinking)
                .await
            {
                Ok(updated_session) => {
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
                snapshot.push_str(&cell.symbol());
            }
            snapshot.push('\n');
        }

        let path = PathBuf::from("ai/tmp/tui_snapshot.txt");
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, snapshot);
    }

    /// Calculate cursor row and column for multi-line input.
    /// Returns (row, col) where row is 0-indexed from top of input area.
    fn calculate_cursor_position(&self, text_width: usize) -> (usize, usize) {
        if text_width == 0 {
            return (0, 0);
        }

        let text_before_cursor = &self.input[..self.cursor_pos];
        let mut row = 0;
        let mut col = 0;

        for ch in text_before_cursor.chars() {
            if ch == '\n' {
                row += 1;
                col = 0;
            } else {
                col += 1;
                // Handle line wrapping
                if col >= text_width {
                    row += 1;
                    col = 0;
                }
            }
        }

        (row, col)
    }

    /// Calculate the height needed for the input box based on content.
    /// Returns height including borders (min 3, max 10).
    fn calculate_input_height(&self, terminal_width: u16) -> u16 {
        const MIN_HEIGHT: u16 = 3;
        const MAX_HEIGHT: u16 = 10;
        const BORDER_OVERHEAD: u16 = 2; // Top and bottom borders
        const PADDING: u16 = 3; // Left padding + margins

        if self.input.is_empty() {
            return MIN_HEIGHT;
        }

        // Available width for text (subtract borders and padding)
        let text_width = terminal_width.saturating_sub(BORDER_OVERHEAD + PADDING) as usize;
        if text_width == 0 {
            return MIN_HEIGHT;
        }

        // Count lines: explicit newlines + wrapped lines
        let mut line_count: u16 = 0;
        for line in self.input.split('\n') {
            // Each line takes at least 1 row, plus wrapping
            let line_len = line.chars().count();
            let wrapped_lines = if line_len == 0 {
                1
            } else {
                ((line_len + text_width - 1) / text_width) as u16
            };
            line_count += wrapped_lines;
        }

        // Add border overhead and clamp to bounds
        (line_count + BORDER_OVERHEAD).clamp(MIN_HEIGHT, MAX_HEIGHT)
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
        // Calculate input box height based on content
        let input_height = self.calculate_input_height(frame.area().width);

        // Progress line: running, or showing completion summary
        let show_summary = self
            .last_task_summary
            .as_ref()
            .is_some_and(|s| s.completed_at.elapsed() < SUMMARY_DISPLAY);
        let progress_height = if self.is_running || show_summary { 1 } else { 0 };

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(0),        // Chat
                Constraint::Length(progress_height), // Progress line (Ionizing...)
                Constraint::Length(input_height),    // Input
                Constraint::Length(1),     // Status line
            ])
            .split(frame.area());

        let at_bottom = self.message_list.is_at_bottom();

        let chat_block =
            Block::default()
                .borders(Borders::ALL)
                .border_style(if self.cancel_pending.is_some() {
                    Style::default().fg(Color::Yellow)
                } else if !at_bottom {
                    Style::default().fg(Color::Blue)
                } else {
                    Style::default()
                });

        // Title with modern indicators
        let mut title_spans = vec![Span::raw(" ion ")];
        if self.is_running {
            title_spans.push(Span::styled(
                " RUNNING ",
                Style::default().fg(Color::Yellow).bold(),
            ));
        }
        if !at_bottom {
            title_spans.push(Span::styled(
                format!(" [+{}] ", self.message_list.scroll_offset),
                Style::default().fg(Color::Blue),
            ));
        }

        let chat_block = chat_block.title(Line::from(title_spans));

        let height = (chunks[0].height as usize).saturating_sub(2);
        let (start, end) = self.message_list.visible_range(height);

        // Borrow cached markdown content to ensure it lives long enough for the Paragraph
        let entries_with_content: Vec<(&crate::tui::message_list::MessageEntry, &str)> =
            self.message_list.entries[start..end]
                .iter()
                .map(|e| (e, e.content_as_markdown()))
                .collect();

        // Simplified model name for display
        let display_model = self
            .session
            .model
            .split('/')
            .last()
            .unwrap_or(&self.session.model);

        let mut chat_lines = Vec::new();
        for (entry, content) in &entries_with_content {
            match entry.sender {
                Sender::User => {
                    chat_lines.push(Line::from(vec![
                        Span::styled(" ↑ ", Style::default().fg(Color::Cyan)),
                        Span::styled("You", Style::default().fg(Color::Cyan).bold()),
                    ]));
                    let md = tui_markdown::from_str(content);
                    for line in &md.lines {
                        let mut padded = vec![Span::raw(" ")];
                        padded.extend(line.spans.clone());
                        chat_lines.push(Line::from(padded));
                    }
                }
                Sender::Agent => {
                    chat_lines.push(Line::from(vec![
                        Span::styled(" ↓ ", Style::default().fg(Color::Green)),
                        Span::styled(display_model, Style::default().fg(Color::Green).bold()),
                    ]));
                    let md = tui_markdown::from_str(content);
                    for line in &md.lines {
                        let mut padded = vec![Span::raw(" ")];
                        padded.extend(line.spans.clone());
                        chat_lines.push(Line::from(padded));
                    }
                }
                Sender::Tool => {
                    chat_lines.push(Line::from(vec![
                        Span::styled(" ⏺ ", Style::default().fg(Color::Magenta).dim()),
                        Span::styled("tool", Style::default().fg(Color::Magenta).dim()),
                    ]));
                    // Check for ANSI escape sequences (ESC[)
                    if content.contains("\x1b[") {
                        // Parse ANSI codes to ratatui styles
                        use ansi_to_tui::IntoText;
                        if let Ok(ansi_text) = content.as_bytes().into_text() {
                            for line in ansi_text.lines {
                                let mut padded = vec![Span::raw(" ")];
                                padded.extend(line.spans.clone());
                                chat_lines.push(Line::from(padded));
                            }
                        } else {
                            // Fallback: strip ANSI and render plain
                            let stripped = strip_ansi(content);
                            chat_lines.push(Line::from(vec![
                                Span::raw(" "),
                                Span::styled(stripped, Style::default().fg(Color::Magenta).dim()),
                            ]));
                        }
                    } else {
                        let md = tui_markdown::from_str(content);
                        for line in &md.lines {
                            let mut styled_line = line.clone();
                            for span in styled_line.spans.iter_mut() {
                                span.style =
                                    span.style.patch(Style::default().fg(Color::Magenta).dim());
                            }
                            let mut padded = vec![Span::raw(" ")];
                            padded.extend(styled_line.spans);
                            chat_lines.push(Line::from(padded));
                        }
                    }
                }
                Sender::System => {
                    chat_lines.push(Line::from(vec![
                        Span::styled(" ! ", Style::default().fg(Color::Yellow).dim()),
                        Span::styled(*content, Style::default().fg(Color::Yellow).dim().italic()),
                    ]));
                }
            }
            chat_lines.push(Line::from(""));
        }

        // Show queued messages at bottom of chat (dimmed, pending)
        if let Some(ref queue) = self.message_queue {
            if let Ok(q) = queue.lock() {
                for queued in q.iter() {
                    chat_lines.push(Line::from(vec![
                        Span::styled(" > ", Style::default().fg(Color::Yellow).dim()),
                        Span::styled(queued.clone(), Style::default().dim().italic()),
                    ]));
                    chat_lines.push(Line::from(""));
                }
            }
        }

        let chat_para = Paragraph::new(chat_lines)
            .block(chat_block)
            .wrap(Wrap { trim: true });
        frame.render_widget(chat_para, chunks[0]);

        // Progress line (only when running)
        if self.is_running {
            let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let symbol = spinner[(self.frame_count % spinner.len() as u64) as usize];

            // Show status: cancelling > running tool > ionizing
            let (label, color) = if self.session.abort_token.is_cancelled() {
                ("Cancelling...".to_string(), Color::Red)
            } else if let Some(tool) = &self.current_tool {
                (format!("Running {}...", tool), Color::Cyan)
            } else {
                ("Ionizing...".to_string(), Color::Yellow)
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
            frame.render_widget(Paragraph::new(progress_line), chunks[1]);
        } else if let Some(summary) = &self.last_task_summary {
            // Show completion summary briefly
            if summary.completed_at.elapsed() < SUMMARY_DISPLAY {
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

                let summary_line = Line::from(vec![
                    Span::styled(" ✓ ", Style::default().fg(Color::Green)),
                    Span::styled("Done", Style::default().fg(Color::Green)),
                    Span::styled(
                        format!(" ({})", stats.join(" · ")),
                        Style::default().dim(),
                    ),
                ]);
                frame.render_widget(Paragraph::new(summary_line), chunks[1]);
            }
        }

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
                frame.render_widget(approval_para, chunks[2]);
            }
        } else {
            // Input box always visible
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

            let mut input_block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(mode_color))
                .title(format!(" [{}] ", mode_label));

            // Show thinking level on right side
            let thinking_label = self.thinking_level.label();
            if !thinking_label.is_empty() {
                input_block = input_block.title(
                    Line::from(Span::styled(
                        format!(" {} ", thinking_label),
                        Style::default().fg(Color::Magenta),
                    ))
                    .right_aligned(),
                );
            }

            // Build input text with cursor (1-space left padding)
            let input_text = format!(" {}", self.input);
            let input_para = Paragraph::new(input_text)
                .block(input_block)
                .wrap(Wrap { trim: false });
            frame.render_widget(input_para, chunks[2]);

            // Calculate cursor position for multi-line input
            let inner_width = chunks[2].width.saturating_sub(3) as usize; // borders + padding
            let (cursor_row, cursor_col) =
                self.calculate_cursor_position(inner_width);

            let cursor_x = chunks[2].x + 2 + cursor_col as u16;
            let cursor_y = chunks[2].y + 1 + cursor_row as u16;

            // Only show cursor if within bounds
            if cursor_x < chunks[2].x + chunks[2].width - 1
                && cursor_y < chunks[2].y + chunks[2].height - 1
            {
                frame.set_cursor_position((cursor_x, cursor_y));
            }
        }

        // Status line: model · context% · [branch] · cwd
        let cwd = self
            .session
            .working_dir
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "~".to_string());

        let branch = get_git_branch(&self.session.working_dir).unwrap_or_default();
        let branch_part = if branch.is_empty() {
            String::new()
        } else {
            format!("[{}] · ", branch)
        };

        // Context % display with token counts: 56% (112k/200k)
        let context_part = if let Some((used, max)) = self.token_usage {
            let pct = if max > 0 {
                (used * 100) / max
            } else {
                0
            };
            // Format tokens as k (thousands)
            let format_k = |n: usize| -> String {
                if n >= 1000 {
                    format!("{}k", n / 1000)
                } else {
                    n.to_string()
                }
            };
            format!("{}% ({}/{}) · ", pct, format_k(used), format_k(max))
        } else {
            String::new()
        };

        // Simplify model name (remove provider prefix if present)
        let model_name = self
            .session
            .model
            .split('/')
            .last()
            .unwrap_or(&self.session.model);

        let left = format!(" {} · {}{}{}", model_name, context_part, branch_part, cwd);

        // Right side: error or help hint
        let (right, right_style) = if let Some(ref err) = self.last_error {
            // Truncate error for status line
            let max_err_len = 40;
            let err_display = if err.len() > max_err_len {
                format!("{}...", &err[..max_err_len])
            } else {
                err.clone()
            };
            (format!("ERR: {} ", err_display), Style::default().fg(Color::Red))
        } else {
            ("? help ".to_string(), Style::default().dim())
        };

        // Calculate padding for right alignment
        let width = chunks[3].width as usize;
        let left_len = left.chars().count();
        let right_len = right.chars().count();
        let padding = width.saturating_sub(left_len + right_len);

        let status_line = Line::from(vec![
            Span::styled(left, Style::default()),
            Span::raw(" ".repeat(padding)),
            Span::styled(right, right_style),
        ]);

        frame.render_widget(Paragraph::new(status_line), chunks[3]);

        // Render modals on top if active
        match self.mode {
            Mode::ModelPicker => self.model_picker.render(frame),
            Mode::ProviderPicker => self.provider_picker.render(frame),
            Mode::HelpOverlay => self.render_help_overlay(frame),
            _ => {}
        }
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
            row("Ctrl+M", "Model picker"),
            row("Ctrl+P", "Provider picker"),
            row("Ctrl+T", "Thinking toggle"),
            row("Ctrl+C", "Clear / Quit"),
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
            Line::from(Span::styled("Press any key to close", Style::default().dim()))
                .alignment(ratatui::layout::Alignment::Center),
        ];

        let help_para = Paragraph::new(help_text)
            .block(Block::default().borders(Borders::ALL).title(" ? Help "));

        frame.render_widget(help_para, help_area);
    }
}

/// Get the current git branch name, if in a git repository.
fn get_git_branch(working_dir: &std::path::Path) -> Option<String> {
    let head_path = working_dir.join(".git/HEAD");
    if let Ok(content) = std::fs::read_to_string(head_path) {
        let content = content.trim();
        if let Some(branch) = content.strip_prefix("ref: refs/heads/") {
            return Some(branch.to_string());
        }
        // Detached HEAD - show short hash
        if content.len() >= 7 {
            return Some(content[..7].to_string());
        }
    }
    None
}

/// Strip ANSI escape sequences from a string.
fn strip_ansi(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut in_escape = false;
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                in_escape = true;
                chars.next(); // consume '['
                continue;
            }
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
