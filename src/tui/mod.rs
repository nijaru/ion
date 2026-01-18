pub mod message_list;
pub mod model_picker;
pub mod provider_picker;
pub mod widgets;

use crate::agent::{Agent, AgentEvent};
use crate::config::Config;
use crate::memory::MemorySystem;
use crate::memory::embedding::{OpenAIConfig, OpenAIProvider};
use crate::provider::{ApiProvider, ModelRegistry};
use crate::session::Session;
use crate::session::SessionStore;
use crate::tool::{ApprovalHandler, ApprovalResponse, ToolMode, ToolOrchestrator};
use crate::tui::message_list::{MessageList, Sender};
use crate::tui::model_picker::ModelPicker;
use crate::tui::provider_picker::ProviderPicker;
use crate::tui::widgets::LoadingIndicator;
use async_trait::async_trait;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

const CANCEL_WINDOW: Duration = Duration::from_millis(1500);

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

/// How the model picker was opened (affects behavior after models load)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PickerIntent {
    /// Ctrl+P: Start at provider selection
    #[default]
    ProviderFirst,
    /// Ctrl+M: Jump directly to model selection for current provider
    ModelOnly,
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
    /// How the model picker was opened (for handling async model fetch)
    pub picker_intent: PickerIntent,
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
    /// Last memory retrieval count for status display
    pub last_memory_count: Option<usize>,
    /// Model picker state
    pub model_picker: ModelPicker,
    /// Model registry for fetching available models
    pub model_registry: Arc<ModelRegistry>,
    /// Config for accessing preferences
    pub config: Config,
    /// TUI frame counter for animations
    pub frame_count: u64,
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

use tracing::error;

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
        let config = Config::load().expect("Failed to load config");

        // Initialize logging
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .try_init();

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
        let mut orchestrator = ToolOrchestrator::with_builtins(ToolMode::Write);
        orchestrator.set_approval_handler(Arc::new(TuiApprovalHandler {
            request_tx: approval_tx,
        }));

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

        // Initialize embedding provider (Prioritize Local)
        let model_dir = config
            .data_dir
            .join("models")
            .join("snowflake-arctic-embed-s");
        let embedding: Option<Arc<dyn crate::memory::embedding::EmbeddingProvider>> =
            match crate::memory::embedding::SnowflakeArcticProvider::load(model_dir).await {
                Ok(local) => Some(Arc::new(local)),
                Err(e) => {
                    error!(
                        "Failed to load local embeddings: {}. Falling back to OpenAI if available.",
                        e
                    );
                    std::env::var("OPENAI_API_KEY").ok().map(|key| {
                        Arc::new(OpenAIProvider::new(OpenAIConfig {
                            api_key: key,
                            model: "text-embedding-3-small".to_string(),
                            dimension: 1536,
                        }))
                            as Arc<dyn crate::memory::embedding::EmbeddingProvider>
                    })
                }
            };

        // Initialize memory system
        let memory_path = config.data_dir.join("memory");
        let dimension = embedding.as_ref().map(|e| e.dimension()).unwrap_or(384);
        let memory = MemorySystem::new(&memory_path, dimension)
            .ok()
            .map(|ms| Arc::new(Mutex::new(ms)));

        let mut agent = Agent::new(provider_impl, orchestrator.clone());
        if let (Some(m), Some(e)) = (&memory, &embedding) {
            agent = agent.with_memory(m.clone(), e.clone());
            let worker = agent
                .indexing_worker()
                .expect("Worker should be present after with_memory");
            let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let explorer = Arc::new(crate::agent::explorer::Explorer::new(worker, working_dir));

            agent = agent.with_explorer(explorer);
        }
        let agent = Arc::new(agent);

        // Open session store
        let store =
            SessionStore::open(&config.sessions_db_path()).expect("Failed to open session store");

        // Create new session with current directory
        let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let session = Session::new(working_dir, config.default_model.clone());

        let (agent_tx, agent_rx) = mpsc::channel(100);
        let (session_tx, session_rx) = mpsc::channel(1);

        // Model registry for picker
        let model_registry = Arc::new(ModelRegistry::new(api_key, config.model_cache_ttl_secs));

        Self {
            mode: Mode::Input,
            should_quit: false,
            input: String::new(),
            cursor_pos: 0,
            input_history: Vec::new(),
            history_index: 0,
            tool_mode: ToolMode::Write,
            picker_intent: PickerIntent::ProviderFirst,
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
            last_memory_count: None,
            model_picker: ModelPicker::new(config.provider_prefs.clone()),
            model_registry,
            config,
            frame_count: 0,
        }
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
        // Poll agent events
        while let Ok(event) = self.agent_rx.try_recv() {
            match &event {
                AgentEvent::Finished(_) | AgentEvent::Error(_) => {
                    self.is_running = false;
                    self.cancel_pending = None;
                    self.message_list.push_event(event);
                }
                AgentEvent::ModelsFetched(models) => {
                    self.model_picker.set_models(models.clone());
                    // Configure picker based on how it was opened
                    match self.picker_intent {
                        PickerIntent::ModelOnly => {
                            let provider = self.current_provider();
                            self.model_picker.start_model_only(&provider);
                        }
                        PickerIntent::ProviderFirst => {
                            self.model_picker.reset();
                        }
                    }
                }
                AgentEvent::ModelFetchError(err) => {
                    self.model_picker.set_error(err.clone());
                }
                AgentEvent::MemoryRetrieval { results_count, .. } => {
                    self.last_memory_count = Some(*results_count);
                }
                _ => {
                    self.message_list.push_event(event);
                }
            }
        }

        // Poll session updates (preserves conversation history)
        if let Ok(updated_session) = self.session_rx.try_recv() {
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
            // Ctrl+C: Clear input / quit if empty / interrupt if running
            KeyCode::Char('c') if ctrl => {
                if self.is_running {
                    // Interrupt running task
                    if let Some(when) = self.cancel_pending {
                        if when.elapsed() <= CANCEL_WINDOW {
                            self.session.abort_token.cancel();
                            self.cancel_pending = None;
                        } else {
                            self.cancel_pending = Some(Instant::now());
                        }
                    } else {
                        self.cancel_pending = Some(Instant::now());
                    }
                } else if self.input.is_empty() {
                    self.quit();
                } else {
                    self.input.clear();
                    self.cursor_pos = 0;
                }
            }

            // Ctrl+D: Quit if input empty
            KeyCode::Char('d') if ctrl => {
                if self.input.is_empty() {
                    self.quit();
                }
            }

            // Tab: Cycle tool mode (Read → Write → Agi)
            KeyCode::Tab => {
                self.tool_mode = match self.tool_mode {
                    ToolMode::Read => ToolMode::Write,
                    ToolMode::Write => ToolMode::Agi,
                    ToolMode::Agi => ToolMode::Read,
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

            // Ctrl+S: Take UI snapshot (Debug/Agent only)
            KeyCode::Char('s') if ctrl => {
                self.take_snapshot();
            }

            // Shift+Enter: Insert newline
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.input.insert(self.cursor_pos, '\n');
                self.cursor_pos += 1;
            }

            // Enter: Send message
            KeyCode::Enter => {
                if !self.input.is_empty() && !self.is_running {
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
                            "/index" => {
                                let parts: Vec<&str> = self.input.split_whitespace().collect();

                                let path = if parts.len() > 1 {
                                    Some(parts[1].to_string())
                                } else {
                                    None
                                };

                                self.input.clear();
                                self.cursor_pos = 0;
                                self.message_list.push_entry(
                                    crate::tui::message_list::MessageEntry::new(
                                        crate::tui::Sender::System,
                                        format!(
                                            "Indexing {}...",
                                            path.as_deref().unwrap_or("working directory")
                                        ),
                                    ),
                                );
                                let agent = self.agent.clone();
                                tokio::spawn(async move {
                                    let path_buf = path.map(PathBuf::from);
                                    if let Err(e) = agent.reindex(path_buf.as_deref()).await {
                                        error!("Indexing failed: {}", e);
                                    }
                                });
                                return;
                            }
                            _ => {} // Fall through to normal message if unknown command
                        }
                    }

                    // Enter: Send message
                    let input = std::mem::take(&mut self.input);
                    self.input_history.push(input.clone());
                    self.history_index = self.input_history.len();
                    self.cursor_pos = 0;
                    self.message_list.push_user_message(input.clone());
                    self.run_agent_task(input);
                }
            }

            // Page Up/Down: Scroll chat history
            KeyCode::PageUp => self.message_list.scroll_up(10),
            KeyCode::PageDown => self.message_list.scroll_down(10),

            // Arrow Up: Move cursor up, or recall history if at top
            KeyCode::Up => {
                // Check if cursor is at start of input (or no newlines above)
                let at_top = self.cursor_pos == 0 || !self.input[..self.cursor_pos].contains('\n');
                if at_top && !self.input_history.is_empty() {
                    // Recall previous history
                    if self.history_index > 0 {
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

        match key.code {
            // Navigation: Arrow keys primary, j/k as alternatives
            KeyCode::Up | KeyCode::Char('k') => self.model_picker.move_up(1),
            KeyCode::Down | KeyCode::Char('j') => self.model_picker.move_down(1),

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
                        self.model_picker.reset();
                        self.mode = Mode::Input;
                    }
                }
            },

            // Back navigation
            KeyCode::Backspace if self.model_picker.filter.is_empty() => {
                if self.model_picker.stage == PickerStage::Model {
                    self.model_picker.back_to_providers();
                }
            }
            KeyCode::Backspace => {
                self.model_picker.pop_char();
            }

            // Cancel
            KeyCode::Esc => {
                self.model_picker.reset();
                self.mode = Mode::Input;
            }

            // Type to filter
            KeyCode::Char(c) => {
                self.model_picker.push_char(c);
            }

            _ => {}
        }
    }

    /// Extract provider name from model ID (e.g., "anthropic/claude-sonnet-4" → "Anthropic")
    fn current_provider(&self) -> String {
        self.session
            .model
            .split('/')
            .next()
            .unwrap_or("unknown")
            .to_string()
            // Capitalize first letter for display
            .chars()
            .enumerate()
            .map(|(i, c): (usize, char)| if i == 0 { c.to_ascii_uppercase() } else { c })
            .collect()
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

    /// Open model picker for current provider only (Ctrl+M)
    fn open_model_picker(&mut self) {
        self.mode = Mode::ModelPicker;
        self.model_picker.error = None;
        self.picker_intent = PickerIntent::ModelOnly;

        if self.model_picker.has_models() {
            // Models already loaded, jump directly to model selection
            let provider = self.current_provider();
            self.model_picker.start_model_only(&provider);
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
        match key.code {
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
                    }
                    // If not authenticated/implemented, do nothing (can't select)
                }
            }

            // Cancel
            KeyCode::Esc => {
                self.mode = Mode::Input;
            }

            // Type to filter
            KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.provider_picker.delete_word();
            }
            KeyCode::Char(c) => {
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
        let registry = self.model_registry.clone();
        let provider = self.agent.provider();
        let prefs = self.config.provider_prefs.clone();
        let agent_tx = self.agent_tx.clone();

        tokio::spawn(async move {
            match model_picker::fetch_models_for_picker(&registry, provider, &prefs).await {
                Ok(models) => {
                    let _ = agent_tx.send(AgentEvent::ModelsFetched(models)).await;
                }
                Err(e) => {
                    let _ = agent_tx
                        .send(AgentEvent::ModelFetchError(e.to_string()))
                        .await;
                }
            }
        });
    }

    fn run_agent_task(&mut self, input: String) {
        self.is_running = true;

        // Reset cancellation token for new task (tokens are single-use)
        self.session.abort_token = CancellationToken::new();

        let agent = self.agent.clone();
        let session = self.session.clone();
        let event_tx = self.agent_tx.clone();
        let session_tx = self.session_tx.clone();

        tokio::spawn(async move {
            match agent.run_task(session, input, event_tx.clone()).await {
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
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(0),
                Constraint::Length(3),
                Constraint::Length(1),
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
        if let Some(count) = self.last_memory_count {
            title_spans.push(Span::styled(
                format!(" M:{} ", count),
                Style::default().fg(Color::Cyan).dim(),
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

        let mut chat_lines = Vec::new();
        for (entry, content) in &entries_with_content {
            match entry.sender {
                Sender::User => {
                    chat_lines.push(Line::from(vec![
                        Span::styled(" < ", Style::default().fg(Color::Cyan)),
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
                        Span::styled(" > ", Style::default().fg(Color::Green)),
                        Span::styled("ion", Style::default().fg(Color::Green).bold()),
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
                        Span::styled(" ~ ", Style::default().fg(Color::Magenta).dim()),
                        Span::styled("tool", Style::default().fg(Color::Magenta).dim()),
                    ]));
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
                Sender::System => {
                    chat_lines.push(Line::from(vec![
                        Span::styled(" ! ", Style::default().fg(Color::Yellow).dim()),
                        Span::styled(*content, Style::default().fg(Color::Yellow).dim().italic()),
                    ]));
                }
            }
            chat_lines.push(Line::from(""));
        }

        let chat_para = Paragraph::new(chat_lines)
            .block(chat_block)
            .wrap(Wrap { trim: true });
        frame.render_widget(chat_para, chunks[0]);

        // Input or Approval Prompt
        if self.mode == Mode::Approval {
            if let Some(req) = &self.pending_approval {
                let prompt = format!(
                    " [APPROVAL] Allow {}? (y)es / (n)o / (a)lways / (A)lways permanent ",
                    req.tool_name
                );
                let approval_block = Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Red).bold())
                    .title(" ACTION REQUIRED ");
                let approval_para = Paragraph::new(prompt).block(approval_block);
                frame.render_widget(approval_para, chunks[1]);
            }
        } else if self.is_running {
            let loading = LoadingIndicator {
                label: "Ionizing...".to_string(),
                frame_count: self.frame_count,
            };
            frame.render_widget(loading, chunks[1]);
        } else {
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
            let input_block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(mode_color))
                .title(format!(" [{}] ", mode_label));

            // Build input text with cursor (1-space left padding)
            let input_text = format!(" {}", self.input);
            let input_para = Paragraph::new(input_text).block(input_block);
            frame.render_widget(input_para, chunks[1]);

            // Set cursor position (account for 1-space padding)
            let cursor_x = chunks[1].x + 2 + self.cursor_pos as u16;
            let cursor_y = chunks[1].y + 1;
            if cursor_x < chunks[1].x + chunks[1].width - 1 {
                frame.set_cursor_position((cursor_x, cursor_y));
            }
        }

        // Left side: model · [branch] · cwd
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

        // Simplify model name (remove provider prefix if present)
        let model_name = self
            .session
            .model
            .split('/')
            .last()
            .unwrap_or(&self.session.model);

        let left = format!(" {} · {}{}", model_name, branch_part, cwd);

        // Right side: minimal hints
        let right = "? help ";

        // Calculate padding for right alignment
        let width = chunks[2].width as usize;
        let left_len = left.chars().count();
        let right_len = right.chars().count();
        let padding = width.saturating_sub(left_len + right_len);

        let status_line = Line::from(vec![
            Span::styled(left, Style::default()),
            Span::raw(" ".repeat(padding)),
            Span::styled(right, Style::default().dim()),
        ]);

        frame.render_widget(Paragraph::new(status_line), chunks[2]);

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
            row("Tab", "Cycle mode"),
            row("^M", "Model picker"),
            row("^P", "Provider picker"),
            row("^C", "Clear input / Quit"),
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
            row("/index", "Index codebase"),
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
