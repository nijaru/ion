//! App initialization and construction.

use crate::agent::Agent;
use crate::agent::subagent::SubagentRegistry;
use crate::cli::PermissionSettings;
use crate::config::{Config, subagents_dir};
use crate::provider::{Client, LlmApi, ModelRegistry, Provider};
use crate::session::{Session, SessionStore};
use crate::tool::ToolOrchestrator;
use crate::tool::builtin::SpawnSubagentTool;
use crate::tui::App;
use crate::tui::app_state::{InteractionState, TaskState};
use crate::tui::command_completer::CommandCompleter;
use crate::tui::composer::{ComposerBuffer, ComposerState};
use crate::tui::file_completer::FileCompleter;
use crate::tui::message_list::MessageList;
use crate::tui::model_picker::ModelPicker;
use crate::tui::provider_picker::ProviderPicker;
use crate::tui::render_state::RenderState;
use crate::tui::session_picker::SessionPicker;
use crate::tui::types::{HistorySearchState, Mode, SelectorPage, ThinkingLevel};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error};

impl App {
    /// Create a new App with default permissions.
    pub async fn new() -> Result<Self> {
        Self::with_permissions(PermissionSettings::default()).await
    }

    /// Create a new App with the given permission settings.
    #[allow(clippy::too_many_lines)]
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

        let mut orchestrator = ToolOrchestrator::with_builtins(permissions.mode);

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
        // Lazy MCP loading: index tools but don't register them individually.
        // The model discovers tools via mcp_tools search and calls them via fallback.
        mcp_manager.build_index().await;
        if mcp_manager.has_tools() {
            debug!(
                "Indexed {} MCP tools (lazy loading)",
                mcp_manager.tool_count()
            );
            let mcp_manager = Arc::new(mcp_manager);
            orchestrator.set_mcp_fallback(mcp_manager.clone());
            orchestrator.register_tool(Box::new(crate::tool::builtin::McpToolsTool::new(
                mcp_manager,
            )));
        }

        // Load subagent configurations (defaults first, user YAML overrides by name)
        let mut subagent_registry = SubagentRegistry::with_defaults();
        let subagents_path = subagents_dir();
        if subagents_path.exists()
            && let Ok(count) = subagent_registry.load_directory(&subagents_path)
            && count > 0
        {
            debug!("Loaded {} subagent configurations", count);
        }
        let subagent_registry = Arc::new(subagent_registry);

        // Shared atomic mode so subagents read the live value after Shift+Tab toggle
        let shared_tool_mode =
            crate::tool::builtin::spawn_subagent::shared_tool_mode(permissions.mode);

        // Register spawn_subagent tool
        orchestrator.register_tool(Box::new(SpawnSubagentTool::new(
            subagent_registry,
            provider_impl.clone(),
            shared_tool_mode.clone(),
        )));

        let orchestrator = Arc::new(orchestrator);

        // Register config-driven hooks
        for hook_cfg in &config.hooks {
            if let Some(hook) = crate::hook::CommandHook::from_config(
                &hook_cfg.event,
                hook_cfg.command.clone(),
                hook_cfg.tool_pattern.as_deref(),
            ) {
                orchestrator.register_hook(Arc::new(hook)).await;
            } else {
                error!(
                    "Invalid hook event '{}', expected 'pre_tool_use' or 'post_tool_use'",
                    hook_cfg.event
                );
            }
        }

        let has_mcp_tools = orchestrator.has_mcp_fallback();
        let mut agent = Agent::new(provider_impl, orchestrator.clone());
        // system_prompt = full override; instructions = append to default
        if let Some(ref prompt) = config.system_prompt {
            agent = agent.with_system_prompt(prompt.clone());
        } else if let Some(ref extra) = config.instructions {
            agent = agent.with_extra_instructions(extra);
        }
        if has_mcp_tools {
            agent.context_manager().set_has_mcp_tools(true);
        }
        let agent = Arc::new(agent);

        // Open session store
        let store = SessionStore::open(&config.sessions_db_path())
            .context("Failed to open session store")?;

        // Cleanup old sessions and empty sessions
        if config.session_retention_days > 0 {
            match store.cleanup_old_sessions(config.session_retention_days) {
                Ok(n) if n > 0 => tracing::info!("Cleaned up {n} expired sessions"),
                Err(e) => tracing::warn!("Session cleanup failed: {e}"),
                _ => {}
            }
        }
        if let Err(e) = store.prune_empty_sessions() {
            tracing::warn!("Empty session pruning failed: {e}");
        }

        // Create new session with current directory
        let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let model = config.model.clone().unwrap_or_default();
        let mut session = Session::new(working_dir.clone(), model);
        session.provider = api_provider.id().to_string();
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
            shared_tool_mode,
            api_provider,
            provider_picker: ProviderPicker::new(),
            message_list: MessageList::new(),
            render_state: RenderState::new(),
            startup_header_lines: Self::startup_header_lines(&working_dir),
            git_branch: Self::git_branch_for(&working_dir),
            git_diff_stat: Self::git_diff_stat_for(&working_dir),
            agent,
            session,
            orchestrator,
            agent_tx,
            agent_rx,
            session_rx,
            session_tx,
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
            task: TaskState::default(),
            interaction: InteractionState::default(),
            permissions,
            last_task_summary: None,
            file_completer: FileCompleter::new(working_dir.clone()),
            command_completer: CommandCompleter::new(),
            history_search: HistorySearchState::new(),
            pending_provider: None,
            model_pricing: crate::provider::ModelPricing::default(),
            session_cost: 0.0,
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
}
