//! Session management, provider setup, and agent task execution.

use crate::agent::subagent::SubagentRegistry;
use crate::agent::{Agent, AgentEvent};
use crate::cli::PermissionSettings;
use crate::config::{Config, subagents_dir};
use crate::provider::{Client, ContentBlock, LlmApi, ModelRegistry, Provider, Role};
use crate::session::{Session, SessionStore};
use crate::tool::ToolOrchestrator;
use crate::tool::builtin::SpawnSubagentTool;
use crate::tui::App;
use crate::tui::command_completer::CommandCompleter;
use crate::tui::composer::{ComposerBuffer, ComposerState};
use crate::tui::file_completer::FileCompleter;
use crate::tui::image_attachment::parse_image_attachments;
use crate::tui::message_list::{
    MessageEntry, MessageList, Sender, sanitize_tool_name, strip_error_prefixes,
};
use crate::tui::model_picker::{self, ModelPicker};
use crate::tui::provider_picker::ProviderPicker;
use crate::tui::render_state::RenderState;
use crate::tui::session_picker::SessionPicker;
use crate::tui::types::{Mode, SelectorPage, TaskSummary, ThinkingLevel, TuiApprovalHandler};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error};

impl App {
    /// Create a new App with default permissions.
    pub async fn new() -> Result<Self> {
        Self::with_permissions(PermissionSettings::default()).await
    }

    /// Create a new App with the given permission settings.
    pub async fn with_permissions(permissions: PermissionSettings) -> Result<Self> {
        let config = Config::load().context("Failed to load config")?;

        // Initialize logging - write to file if ION_LOG is set
        if std::env::var("ION_LOG").is_ok() {
            use std::fs::File;
            use tracing_subscriber::prelude::*;
            match File::create("ion.log") {
                Ok(file) => {
                    let file_layer = tracing_subscriber::fmt::layer()
                        .with_writer(file)
                        .with_ansi(false);
                    let filter = tracing_subscriber::EnvFilter::new("ion=debug");
                    let _ = tracing_subscriber::registry()
                        .with(file_layer.with_filter(filter))
                        .try_init();
                }
                Err(err) => {
                    eprintln!("Failed to create log file: {err}");
                }
            }
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

        // Create LLM client - OAuth providers use stored credentials, others use API keys
        let (provider_impl, api_key): (Arc<dyn LlmApi>, String) = if api_provider.is_oauth() {
            let client = Client::from_provider(api_provider)
                .await
                .context("Failed to create OAuth client - run 'ion login' first")?;
            (Arc::new(client), String::new())
        } else {
            let api_key = config.api_key_for(api_provider.id()).unwrap_or_default();
            let client = Client::new(api_provider, api_key.clone())
                .context("Failed to create LLM client")?;
            (Arc::new(client), api_key)
        };

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

        // Load subagent configurations
        let mut subagent_registry = SubagentRegistry::new();
        let subagents_path = subagents_dir();
        if subagents_path.exists()
            && let Ok(count) = subagent_registry.load_directory(&subagents_path)
            && count > 0
        {
            debug!("Loaded {} subagent configurations", count);
        }
        let subagent_registry = Arc::new(tokio::sync::RwLock::new(subagent_registry));

        // Register spawn_subagent tool
        orchestrator.register_tool(Box::new(SpawnSubagentTool::new(
            subagent_registry,
            provider_impl.clone(),
        )));

        let orchestrator = Arc::new(orchestrator);

        let mut agent = Agent::new(provider_impl, orchestrator.clone());
        if let Some(ref prompt) = config.system_prompt {
            agent = agent.with_system_prompt(prompt.clone());
        }
        let agent = Arc::new(agent);

        // Open session store
        let store = SessionStore::open(&config.sessions_db_path())
            .context("Failed to open session store")?;

        // Create new session with current directory
        let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let model = config.model.clone().unwrap_or_default();
        let mut session = Session::new(working_dir.clone(), model);
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

        let input_buffer = ComposerBuffer::new();
        let input_state = ComposerState::new();

        let mut this = Self {
            mode: initial_mode,
            selector_page,
            should_quit: false,
            input_buffer,
            input_state,
            input_history: Vec::new(),
            history_index: 0,
            history_draft: None,
            tool_mode: permissions.mode,
            api_provider,
            provider_picker: ProviderPicker::new(),
            message_list: MessageList::new(),
            render_state: RenderState::new(),
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
            session_picker: SessionPicker::new(),
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
            retry_status: None,
            cancel_pending: None,
            esc_pending: None,
            permissions,
            last_task_summary: None,
            editor_requested: false,
            thinking_start: None,
            last_thinking_duration: None,
            file_completer: FileCompleter::new(working_dir.clone()),
            command_completer: CommandCompleter::new(),
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
                SelectorPage::Session => {
                    // Session picker not used in setup flow
                }
            }
        }

        Ok(this)
    }

    /// Resume an existing session by ID.
    pub fn resume_session(
        &mut self,
        session_id: &str,
    ) -> Result<(), crate::session::SessionStoreError> {
        let loaded = self.store.load(session_id)?;
        self.message_list.load_from_messages(&loaded.messages);
        self.render_state.reset_for_session_load();
        self.session = loaded;
        Ok(())
    }

    /// List recent sessions for display.
    pub fn list_recent_sessions(&self, limit: usize) -> Vec<crate::session::SessionSummary> {
        self.store.list_recent(limit).unwrap_or_default()
    }

    /// Update state on each frame (poll events, check timeouts).
    pub fn update(&mut self) {
        use crate::tui::types::CANCEL_WINDOW;
        use crate::tui::util::format_status_error;

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
                    self.retry_status = None;
                    // End thinking tracking
                    if let Some(start) = self.thinking_start.take() {
                        self.last_thinking_duration = Some(start.elapsed());
                    }
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
                    self.retry_status = None;
                    self.thinking_start = None;
                    self.last_thinking_duration = None;
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
                    if let Some(model) = models.iter().find(|m| m.id == self.session.model)
                        && model.context_window > 0
                    {
                        let ctx_window = model.context_window as usize;
                        self.model_context_window = Some(ctx_window);
                        // Update agent's compaction config
                        self.agent.set_context_window(ctx_window);
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
                    // Store latest turn's input (context size), not accumulated
                    self.input_tokens = *count;
                }
                AgentEvent::OutputTokensDelta(count) => {
                    self.output_tokens += count;
                }
                AgentEvent::ToolCallStart(_, name, _) => {
                    self.current_tool = Some(name.clone());
                    // End thinking if in progress
                    if let Some(start) = self.thinking_start.take() {
                        self.last_thinking_duration = Some(start.elapsed());
                    }
                    self.message_list.push_event(event);
                }
                AgentEvent::ToolCallResult(..) => {
                    self.current_tool = None;
                    self.message_list.push_event(event);
                }
                AgentEvent::ThinkingDelta(_) => {
                    // Start tracking thinking time if not already
                    if self.thinking_start.is_none() {
                        self.thinking_start = Some(Instant::now());
                    }
                    // Don't push to message_list - we don't render thinking content
                }
                AgentEvent::TextDelta(_) => {
                    // End thinking if in progress (text output started)
                    if let Some(start) = self.thinking_start.take() {
                        self.last_thinking_duration = Some(start.elapsed());
                    }
                    // Clear retry status (retry succeeded)
                    self.retry_status = None;
                    self.message_list.push_event(event);
                }
                AgentEvent::Retry(reason, delay) => {
                    // Show retry status in progress line (not in chat)
                    self.retry_status = Some((reason.clone(), *delay));
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

    /// Set the active API provider and re-create the agent.
    pub(super) fn set_provider(&mut self, api_provider: Provider) -> Result<()> {
        // For OAuth providers, use from_provider_sync (no token refresh in sync context).
        // Token refresh happens at startup via from_provider() which is async.
        // For regular providers, get API key from config.
        let (provider, api_key): (Arc<dyn LlmApi>, String) = if api_provider.is_oauth() {
            let client = Client::from_provider_sync(api_provider)
                .context("Failed to create OAuth client - run 'ion login' first")?;
            (Arc::new(client), String::new())
        } else {
            let api_key = self
                .config
                .api_key_for(api_provider.id())
                .unwrap_or_default();
            let client = Client::new(api_provider, api_key.clone())
                .context("Failed to create LLM client")?;
            (Arc::new(client), api_key)
        };

        self.api_provider = api_provider;

        // Save provider to config
        self.config.provider = Some(api_provider.id().to_string());
        if let Err(e) = self.config.save() {
            tracing::warn!("Failed to save config: {}", e);
        }

        // Re-create agent with new provider but same orchestrator
        let mut agent = Agent::new(provider, self.orchestrator.clone());
        if let Some(ref prompt) = self.config.system_prompt {
            agent = agent.with_system_prompt(prompt.clone());
        }
        self.agent = Arc::new(agent);

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
        Ok(())
    }

    /// Open model selector (Ctrl+M or during setup).
    pub(super) fn open_model_selector(&mut self) {
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

    /// Open API provider selector (Ctrl+P).
    pub(super) fn open_provider_selector(&mut self) {
        self.mode = Mode::Selector;
        self.selector_page = SelectorPage::Provider;
        self.provider_picker.refresh();
        self.provider_picker.select_provider(self.api_provider);
    }

    /// Open session selector (/resume).
    pub fn open_session_selector(&mut self) {
        self.mode = Mode::Selector;
        self.selector_page = SelectorPage::Session;
        self.session_picker.load_sessions(&self.store, 50);
    }

    /// Load a session by ID and restore its state.
    pub fn load_session(&mut self, session_id: &str) -> Result<()> {
        let loaded = self.store.load(session_id)?;

        // Restore session state
        self.session = Session {
            id: loaded.id,
            working_dir: loaded.working_dir.clone(),
            model: loaded.model.clone(),
            messages: loaded.messages,
            abort_token: CancellationToken::new(),
            no_sandbox: self.permissions.no_sandbox,
        };

        // Update file completer working directory
        self.file_completer.set_working_dir(loaded.working_dir);

        // Update model display
        self.config.model = Some(loaded.model);

        // Rebuild message list from session messages
        self.message_list.clear();
        self.render_state.reset_for_session_load();
        for msg in &self.session.messages {
            match msg.role {
                Role::User => {
                    for block in msg.content.iter() {
                        if let ContentBlock::Text { text } = block {
                            self.message_list.push_user_message(text.clone());
                        }
                    }
                }
                Role::Assistant => {
                    for block in msg.content.iter() {
                        match block {
                            ContentBlock::Text { text } => {
                                self.message_list
                                    .push_entry(MessageEntry::new(Sender::Agent, text.clone()));
                            }
                            ContentBlock::ToolCall {
                                name, arguments, ..
                            } => {
                                // Sanitize tool name (models sometimes embed args or XML artifacts)
                                let clean_name = sanitize_tool_name(name);
                                // Format tool call with key argument, same as live display
                                let key_arg = crate::tui::message_list::extract_key_arg(
                                    clean_name, arguments,
                                );
                                let display = if key_arg.is_empty() {
                                    clean_name.to_string()
                                } else {
                                    format!("{clean_name}({key_arg})")
                                };
                                self.message_list
                                    .push_entry(MessageEntry::new(Sender::Tool, display));
                            }
                            _ => {}
                        }
                    }
                }
                Role::ToolResult => {
                    for block in msg.content.iter() {
                        if let ContentBlock::ToolResult {
                            content, is_error, ..
                        } = block
                        {
                            let display = if *is_error {
                                let msg = strip_error_prefixes(content).trim();
                                let first_line = msg.lines().next().unwrap_or("");
                                format!("⎿ Error: {first_line}")
                            } else {
                                let line_count = content.lines().count();
                                if line_count > 1 {
                                    format!("⎿ {line_count} lines")
                                } else {
                                    format!("⎿ {}", content.chars().take(60).collect::<String>())
                                }
                            };
                            // Append to previous tool entry if exists
                            if let Some(last) = self.message_list.entries.last_mut()
                                && last.sender == Sender::Tool
                            {
                                last.append_text(&format!("\n{display}"));
                                continue;
                            }
                            self.message_list
                                .push_entry(MessageEntry::new(Sender::Tool, display));
                        }
                    }
                }
                #[allow(clippy::match_wildcard_for_single_variants)]
                _ => {} // System messages not displayed in chat
            }
        }

        Ok(())
    }

    /// Fetch models asynchronously.
    pub(super) fn fetch_models(&self) {
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

    /// Save task summary before clearing task state.
    pub(super) fn save_task_summary(&mut self, was_cancelled: bool) {
        if let Some(start) = self.task_start_time {
            self.last_task_summary = Some(TaskSummary {
                elapsed: start.elapsed(),
                input_tokens: self.input_tokens,
                output_tokens: self.output_tokens,
                was_cancelled,
            });
        }
    }

    /// Run an agent task with the given input.
    pub(super) fn run_agent_task(&mut self, input: String) {
        self.is_running = true;
        self.task_start_time = Some(Instant::now());
        self.input_tokens = 0;
        self.output_tokens = 0;
        self.last_task_summary = None;
        self.last_error = None;
        self.thinking_start = None;
        self.last_thinking_duration = None;

        // Reset cancellation token for new task (tokens are single-use)
        self.session.abort_token = CancellationToken::new();

        // Create shared message queue for mid-task steering
        let queue = Arc::new(std::sync::Mutex::new(Vec::new()));
        self.message_queue = Some(queue.clone());

        // Parse image attachments from input
        let user_content = parse_image_attachments(&input, &self.session.working_dir);

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
            let (updated_session, error) = agent
                .run_task(
                    session,
                    user_content,
                    event_tx.clone(),
                    Some(queue),
                    thinking,
                )
                .await;

            if let Some(e) = error {
                let _ = event_tx.send(AgentEvent::Error(e.to_string())).await;
            } else {
                let _ = event_tx
                    .send(AgentEvent::Finished("Task completed".to_string()))
                    .await;
            }
            // Always send session back - contains whatever work was done
            let _ = session_tx.send(updated_session).await;
        });
    }

    /// Quit the application, saving session.
    pub(super) fn quit(&mut self) {
        self.should_quit = true;

        // Final session save (skip empty sessions)
        if let Err(e) = self.store.save(&self.session) {
            error!("Failed to save session on quit: {}", e);
        }
    }
}
