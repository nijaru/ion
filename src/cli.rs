//! CLI module for one-shot/non-interactive mode.

use crate::agent::{Agent, AgentEvent};
use crate::config::Config;
use crate::provider::{Client, LlmApi, Provider};
use crate::session::Session;
use crate::tool::{ApprovalHandler, ApprovalResponse, ToolMode, ToolOrchestrator};
use anyhow::Result;
use async_trait::async_trait;
use clap::{Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use tokio::sync::mpsc;

// Re-use shared helper from message_list
use crate::tui::message_list::extract_key_arg;

/// Permission settings resolved from CLI flags and config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionSettings {
    /// The effective tool mode (Read, Write, AGI)
    pub mode: ToolMode,
    /// Auto-approve all tool calls without prompting
    pub auto_approve: bool,
    /// Allow operations outside CWD (sandbox disabled)
    pub no_sandbox: bool,
    /// AGI mode was explicitly enabled (allows TUI mode cycling to AGI)
    pub agi_enabled: bool,
}

impl Default for PermissionSettings {
    fn default() -> Self {
        Self {
            mode: ToolMode::Write,
            auto_approve: false,
            no_sandbox: false,
            agi_enabled: false,
        }
    }
}

impl Cli {
    /// Resolve effective permission settings from CLI flags and config.
    ///
    /// Precedence (highest to lowest):
    /// 1. CLI flags (--agi, --yes, --no-sandbox, -r, -w)
    /// 2. Config file settings
    /// 3. Built-in defaults (write mode, sandboxed, approval required)
    #[must_use]
    pub fn resolve_permissions(&self, config: &Config) -> PermissionSettings {
        // Start with config defaults
        let mut settings = PermissionSettings {
            mode: config.permissions.mode(),
            auto_approve: config.permissions.auto_approve.unwrap_or(false),
            no_sandbox: config.permissions.allow_outside_cwd.unwrap_or(false),
            agi_enabled: config.permissions.mode() == ToolMode::Agi,
        };

        // --agi is the ultimate override
        if self.agi_mode {
            settings.mode = ToolMode::Agi;
            settings.auto_approve = true;
            settings.no_sandbox = true;
            settings.agi_enabled = true;
            return settings;
        }

        // CLI flags override config
        if self.no_sandbox {
            settings.no_sandbox = true;
        }

        // --yes implies write mode and auto-approve
        if self.auto_approve {
            settings.auto_approve = true;
            settings.mode = ToolMode::Write;
        }

        // Explicit mode flags (last specified wins, but we can only check presence)
        // -r takes precedence for safety if both specified
        if self.read_mode {
            settings.mode = ToolMode::Read;
            if self.auto_approve || settings.auto_approve {
                eprintln!("Warning: --yes / auto_approve is ignored in read mode");
                settings.auto_approve = false;
            }
        } else if self.write_mode {
            settings.mode = ToolMode::Write;
        }

        // Check for --yes --no-sandbox without --agi (same effect, enable AGI in TUI)
        if settings.auto_approve && settings.no_sandbox && !self.agi_mode {
            settings.agi_enabled = true;
        }

        settings
    }
}

/// Fast, lightweight, open-source coding agent
#[derive(Parser, Debug)]
#[command(name = "ion", version, about)]
#[allow(clippy::struct_excessive_bools)] // CLI flags are naturally boolean
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,

    // Global permission flags (apply to TUI mode)
    /// Read-only mode (no writes, no bash)
    #[arg(short = 'r', long = "read", global = true)]
    pub read_mode: bool,

    /// Write mode (explicit, default)
    #[arg(short = 'w', long = "write", global = true)]
    pub write_mode: bool,

    /// Auto-approve all tool calls (implies write mode)
    #[arg(short = 'y', long = "yes", global = true)]
    pub auto_approve: bool,

    /// Allow operations outside current directory
    #[arg(long = "no-sandbox", global = true)]
    pub no_sandbox: bool,

    /// Full autonomy mode (--yes + --no-sandbox)
    #[arg(long = "agi", global = true)]
    pub agi_mode: bool,

    /// Continue the most recent session from current directory
    #[arg(long = "continue", conflicts_with = "resume")]
    pub continue_session: bool,

    /// Resume a session (open selector if omitted)
    #[arg(
        long = "resume",
        value_name = "SESSION_ID",
        num_args = 0..=1,
        default_missing_value = "__SELECT__",
        conflicts_with = "continue_session"
    )]
    pub resume: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Run a one-shot prompt (non-interactive)
    Run(RunArgs),
    /// Login to an OAuth provider (`ChatGPT` Plus/Pro, Google AI)
    Login(LoginArgs),
    /// Logout from an OAuth provider
    Logout(LogoutArgs),
    /// View or modify configuration
    Config(ConfigArgs),
}

#[derive(Parser, Debug)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub action: Option<ConfigAction>,
}

#[derive(Subcommand, Debug)]
pub enum ConfigAction {
    /// Get a configuration value
    Get {
        /// Key to get (provider, model)
        key: String,
    },
    /// Set a configuration value
    Set {
        /// Key to set (provider, model)
        key: String,
        /// Value to set
        value: String,
    },
    /// Show config file path
    Path,
}

#[derive(Parser, Debug)]
pub struct RunArgs {
    /// The prompt to execute (use "-" to read from stdin)
    #[arg(required = true)]
    pub prompt: String,

    /// Model to use (provider/model format, e.g., "anthropic/claude-sonnet-4")
    #[arg(short, long)]
    pub model: Option<String>,

    /// Output format
    #[arg(short = 'o', long, default_value = "text", value_enum)]
    pub output_format: OutputFormat,

    /// Quiet mode (response only, no progress)
    #[arg(short, long)]
    pub quiet: bool,

    /// Maximum agentic turns before stopping
    #[arg(long)]
    pub max_turns: Option<usize>,

    /// Include file content as context
    #[arg(short = 'f', long = "file")]
    pub context_file: Option<PathBuf>,

    /// Working directory
    #[arg(long)]
    pub cwd: Option<PathBuf>,

    /// Disable all tools (pure chat mode)
    #[arg(long)]
    pub no_tools: bool,

    /// Verbose output
    #[arg(short, long)]
    pub verbose: bool,
}

#[derive(ValueEnum, Clone, Debug, Default)]
pub enum OutputFormat {
    #[default]
    Text,
    Json,
    StreamJson,
}

/// OAuth provider for login/logout
#[derive(ValueEnum, Clone, Debug)]
pub enum AuthProvider {
    /// `ChatGPT` (`OpenAI` OAuth - Plus/Pro subscription)
    #[value(name = "chatgpt")]
    ChatGpt,
    /// Gemini (Google OAuth)
    #[value(name = "gemini")]
    Gemini,
}

impl From<AuthProvider> for crate::auth::OAuthProvider {
    fn from(p: AuthProvider) -> Self {
        match p {
            AuthProvider::ChatGpt => Self::OpenAI,
            AuthProvider::Gemini => Self::Google,
        }
    }
}

#[derive(Parser, Debug)]
pub struct LoginArgs {
    /// Provider to login to
    #[arg(value_enum)]
    pub provider: AuthProvider,
}

#[derive(Parser, Debug)]
pub struct LogoutArgs {
    /// Provider to logout from
    #[arg(value_enum)]
    pub provider: AuthProvider,
}

/// Auto-approve handler for CLI mode with --yes flag
struct AutoApproveHandler;

#[async_trait]
impl ApprovalHandler for AutoApproveHandler {
    async fn ask_approval(&self, _tool_name: &str, _args: &serde_json::Value) -> ApprovalResponse {
        ApprovalResponse::Yes
    }
}

/// Deny handler for CLI mode without --yes flag (restricted tools will fail with clear message)
struct DenyApprovalHandler;

#[async_trait]
impl ApprovalHandler for DenyApprovalHandler {
    async fn ask_approval(&self, tool_name: &str, _args: &serde_json::Value) -> ApprovalResponse {
        eprintln!("Tool '{tool_name}' requires approval. Use --yes flag to auto-approve.");
        ApprovalResponse::No
    }
}

/// JSON output structure for json/stream-json modes
#[derive(Serialize)]
#[serde(tag = "type")]
enum JsonEvent {
    #[serde(rename = "text_delta")]
    TextDelta { text: String },
    #[serde(rename = "thinking_delta")]
    ThinkingDelta { text: String },
    #[serde(rename = "tool_call_start")]
    ToolCallStart { id: String, name: String },
    #[serde(rename = "tool_call_result")]
    ToolCallResult {
        id: String,
        content: String,
        is_error: bool,
    },
    #[serde(rename = "done")]
    Done { response: String },
    #[serde(rename = "error")]
    Error { message: String },
}

/// Components needed for CLI agent execution.
struct CliAgentSetup {
    agent: Arc<Agent>,
    session: Session,
    prompt: String,
}

/// Setup CLI agent: config, provider, client, orchestrator, agent, session.
fn setup_cli_agent(args: &RunArgs, auto_approve: bool) -> Result<CliAgentSetup> {
    // Load config
    let config = Config::load()?;

    // Determine working directory
    let working_dir = args
        .cwd
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    // Read prompt (handle stdin with "-")
    let prompt = if args.prompt == "-" {
        let mut buffer = String::new();
        io::stdin().read_to_string(&mut buffer)?;
        buffer.trim().to_string()
    } else {
        args.prompt.clone()
    };

    // Optionally prepend context file
    let prompt = if let Some(ref file_path) = args.context_file {
        let content = std::fs::read_to_string(file_path)?;
        format!(
            "Context from {}:\n```\n{}\n```\n\n{}",
            file_path.display(),
            content,
            prompt
        )
    } else {
        prompt
    };

    if prompt.is_empty() {
        anyhow::bail!("Empty prompt");
    }

    // Determine provider from config (or default to openrouter)
    let provider_id = config.provider.as_deref().unwrap_or("openrouter");
    let provider = Provider::from_id(provider_id).unwrap_or(Provider::OpenRouter);

    // Get API key (env var first, then config)
    let api_key = config.api_key_for(provider_id).ok_or_else(|| {
        anyhow::anyhow!(
            "No API key for {}. Set {} or configure in ~/.ion/config.toml",
            provider_id,
            match provider_id {
                "anthropic" => "ANTHROPIC_API_KEY",
                "openai" => "OPENAI_API_KEY",
                "google" => "GOOGLE_API_KEY",
                "groq" => "GROQ_API_KEY",
                _ => "OPENROUTER_API_KEY",
            }
        )
    })?;

    // Determine model
    let model = args
        .model
        .clone()
        .or(config.model.clone())
        .unwrap_or_else(|| "anthropic/claude-sonnet-4".to_string());

    // Create LLM client
    let llm_client: Arc<dyn LlmApi> = Arc::new(Client::new(provider, api_key)?);

    // Create orchestrator
    let orchestrator = if args.no_tools {
        Arc::new(ToolOrchestrator::new(ToolMode::Read))
    } else if auto_approve {
        let mut orch = ToolOrchestrator::with_builtins(ToolMode::Write);
        orch.set_approval_handler(Arc::new(AutoApproveHandler));
        Arc::new(orch)
    } else {
        let mut orch = ToolOrchestrator::with_builtins(ToolMode::Write);
        orch.set_approval_handler(Arc::new(DenyApprovalHandler));
        Arc::new(orch)
    };

    // Create agent
    let mut agent = Agent::new(llm_client, orchestrator);
    if let Some(ref system_prompt) = config.system_prompt {
        agent = agent.with_system_prompt(system_prompt.clone());
    }

    // Create session
    let session = Session::new(working_dir, model);

    Ok(CliAgentSetup {
        agent: Arc::new(agent),
        session,
        prompt,
    })
}

/// Output final result based on format.
fn output_result(
    response: &str,
    output_format: &OutputFormat,
    quiet: bool,
    interrupted: bool,
    error: Option<anyhow::Error>,
) -> Result<ExitCode> {
    match output_format {
        OutputFormat::Text => {
            if quiet {
                println!("{response}");
            } else if !response.ends_with('\n') {
                println!();
            }
        }
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(&JsonEvent::Done {
                response: response.to_string(),
            })?;
            println!("{json}");
        }
        OutputFormat::StreamJson => {
            let json = serde_json::to_string(&JsonEvent::Done {
                response: response.to_string(),
            })?;
            println!("{json}");
        }
    }

    if interrupted {
        Ok(ExitCode::from(3))
    } else if let Some(e) = error {
        match output_format {
            OutputFormat::Text => eprintln!("Error: {e}"),
            OutputFormat::Json | OutputFormat::StreamJson => {
                let json = serde_json::to_string(&JsonEvent::Error {
                    message: e.to_string(),
                })?;
                println!("{json}");
            }
        }
        Ok(ExitCode::from(1))
    } else {
        Ok(ExitCode::from(0))
    }
}

/// Run the CLI one-shot mode
pub async fn run(args: RunArgs, auto_approve: bool) -> ExitCode {
    match run_inner(args, auto_approve).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::from(1)
        }
    }
}

#[allow(clippy::match_wildcard_for_single_variants, clippy::too_many_lines)]
async fn run_inner(args: RunArgs, auto_approve: bool) -> Result<ExitCode> {
    // Initialize tracing for CLI mode
    if args.verbose || std::env::var("ION_LOG").is_ok() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(std::io::stderr)
            .try_init();
    }

    let CliAgentSetup {
        agent,
        session,
        prompt,
    } = setup_cli_agent(&args, auto_approve)?;
    let abort_token = session.abort_token.clone();

    // Create event channel
    let (tx, mut rx) = mpsc::channel::<AgentEvent>(100);

    // Run agent in background
    let agent_clone = agent.clone();
    let session_clone = session;
    let prompt_clone = prompt.clone();
    let max_turns = args.max_turns;

    let agent_handle = tokio::spawn(async move {
        let content = vec![crate::provider::ContentBlock::Text { text: prompt_clone }];
        agent_clone
            .run_task(session_clone, content, tx, None, None)
            .await
    });

    // Collect response
    let mut response = String::new();
    let mut turn_count = 0;
    let mut interrupted = false;
    let output_format = args.output_format;
    let quiet = args.quiet;
    let verbose = args.verbose;

    // Handle events
    while let Some(event) = rx.recv().await {
        match &event {
            AgentEvent::TextDelta(text) => {
                response.push_str(text);
                match output_format {
                    OutputFormat::Text if !quiet => {
                        print!("{text}");
                        io::stdout().flush()?;
                    }
                    OutputFormat::StreamJson => {
                        let json =
                            serde_json::to_string(&JsonEvent::TextDelta { text: text.clone() })?;
                        println!("{json}");
                    }
                    _ => {}
                }
            }
            AgentEvent::ThinkingDelta(text) => {
                if verbose {
                    match output_format {
                        OutputFormat::Text => {
                            eprint!("[thinking] {text}");
                        }
                        OutputFormat::StreamJson => {
                            let json = serde_json::to_string(&JsonEvent::ThinkingDelta {
                                text: text.clone(),
                            })?;
                            println!("{json}");
                        }
                        _ => {}
                    }
                }
            }
            AgentEvent::ToolCallStart(id, name, args) => {
                turn_count += 1;
                if let Some(max) = max_turns
                    && turn_count >= max
                {
                    eprintln!("\nMax turns ({max}) reached");
                    abort_token.cancel(); // Signal agent to stop
                    interrupted = true;
                    break;
                }
                if !quiet {
                    // Extract key argument for display
                    let key_arg = extract_key_arg(name, args);
                    match output_format {
                        OutputFormat::Text => {
                            eprintln!("\n> {name}({key_arg})");
                        }
                        OutputFormat::StreamJson => {
                            let json = serde_json::to_string(&JsonEvent::ToolCallStart {
                                id: id.clone(),
                                name: name.clone(),
                            })?;
                            println!("{json}");
                        }
                        _ => {}
                    }
                }
            }
            AgentEvent::ToolCallResult(id, content, is_error) => {
                if verbose {
                    match output_format {
                        OutputFormat::Text => {
                            // Use char-safe truncation to avoid UTF-8 panic
                            let preview = if content.chars().count() > 200 {
                                let truncated: String = content.chars().take(200).collect();
                                format!("{truncated}...")
                            } else {
                                content.clone()
                            };
                            eprintln!("  -> {preview}");
                        }
                        OutputFormat::StreamJson => {
                            let json = serde_json::to_string(&JsonEvent::ToolCallResult {
                                id: id.clone(),
                                content: content.clone(),
                                is_error: *is_error,
                            })?;
                            println!("{json}");
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    // Wait for agent to finish
    let (_session, error) = agent_handle.await?;

    output_result(&response, &output_format, quiet, interrupted, error)
}

/// Run the login command
pub async fn login(args: LoginArgs) -> ExitCode {
    let provider: crate::auth::OAuthProvider = args.provider.into();
    let display_name = provider.display_name();
    match crate::auth::login(provider).await {
        Ok(()) => {
            println!("Successfully logged in to {display_name}");
            ExitCode::from(0)
        }
        Err(e) => {
            eprintln!("Login failed: {e}");
            ExitCode::from(1)
        }
    }
}

/// Run the logout command
#[must_use]
pub fn logout(args: LogoutArgs) -> ExitCode {
    let provider: crate::auth::OAuthProvider = args.provider.into();
    let display_name = provider.display_name();
    match crate::auth::logout(provider) {
        Ok(()) => {
            println!("Logged out from {display_name}");
            ExitCode::from(0)
        }
        Err(e) => {
            eprintln!("Logout failed: {e}");
            ExitCode::from(1)
        }
    }
}

/// Run the config command
#[must_use]
pub fn config(args: ConfigArgs) -> ExitCode {
    use crate::config::Config;

    let config = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error loading config: {e}");
            return ExitCode::from(1);
        }
    };

    match args.action {
        None => {
            // Show current config
            println!("provider: {}", config.provider.as_deref().unwrap_or("(not set)"));
            println!("model: {}", config.model.as_deref().unwrap_or("(not set)"));
            ExitCode::from(0)
        }
        Some(ConfigAction::Path) => {
            let path = crate::config::ion_config_dir();
            println!("{}", path.join("config.toml").display());
            ExitCode::from(0)
        }
        Some(ConfigAction::Get { key }) => {
            let value = match key.as_str() {
                "provider" => config.provider.as_deref(),
                "model" => config.model.as_deref(),
                _ => {
                    eprintln!("Unknown key: {key}. Valid keys: provider, model");
                    return ExitCode::from(1);
                }
            };
            println!("{}", value.unwrap_or("(not set)"));
            ExitCode::from(0)
        }
        Some(ConfigAction::Set { key, value }) => {
            let mut config = config;
            match key.as_str() {
                "provider" => {
                    // Validate provider
                    if crate::provider::Provider::from_id(&value).is_none() {
                        eprintln!("Unknown provider: {value}");
                        eprintln!("Valid providers: anthropic, openrouter, openai, google, groq, kimi, ollama, chatgpt, gemini");
                        return ExitCode::from(1);
                    }
                    config.provider = Some(value);
                }
                "model" => {
                    config.model = Some(value);
                }
                _ => {
                    eprintln!("Unknown key: {key}. Valid keys: provider, model");
                    return ExitCode::from(1);
                }
            };
            if let Err(e) = config.save() {
                eprintln!("Failed to save config: {e}");
                return ExitCode::from(1);
            }
            println!("Updated {key}");
            ExitCode::from(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    // --- CLI parsing tests ---

    #[test]
    fn test_parse_no_args() {
        let cli = Cli::try_parse_from(["ion"]).unwrap();
        assert!(cli.command.is_none());
        assert!(!cli.read_mode);
        assert!(!cli.write_mode);
        assert!(!cli.auto_approve);
    }

    #[test]
    fn test_parse_read_mode() {
        let cli = Cli::try_parse_from(["ion", "-r"]).unwrap();
        assert!(cli.read_mode);
        assert!(!cli.write_mode);
    }

    #[test]
    fn test_parse_write_mode() {
        let cli = Cli::try_parse_from(["ion", "-w"]).unwrap();
        assert!(cli.write_mode);
        assert!(!cli.read_mode);
    }

    #[test]
    fn test_parse_auto_approve() {
        let cli = Cli::try_parse_from(["ion", "-y"]).unwrap();
        assert!(cli.auto_approve);
    }

    #[test]
    fn test_parse_agi_mode() {
        let cli = Cli::try_parse_from(["ion", "--agi"]).unwrap();
        assert!(cli.agi_mode);
    }

    #[test]
    fn test_parse_no_sandbox() {
        let cli = Cli::try_parse_from(["ion", "--no-sandbox"]).unwrap();
        assert!(cli.no_sandbox);
    }

    #[test]
    fn test_parse_continue() {
        let cli = Cli::try_parse_from(["ion", "--continue"]).unwrap();
        assert!(cli.continue_session);
    }

    #[test]
    fn test_parse_resume_without_id() {
        let cli = Cli::try_parse_from(["ion", "--resume"]).unwrap();
        assert_eq!(cli.resume, Some("__SELECT__".to_string()));
    }

    #[test]
    fn test_parse_resume_with_id() {
        let cli = Cli::try_parse_from(["ion", "--resume", "abc123"]).unwrap();
        assert_eq!(cli.resume, Some("abc123".to_string()));
    }

    #[test]
    fn test_continue_and_resume_conflict() {
        let result = Cli::try_parse_from(["ion", "--continue", "--resume"]);
        assert!(result.is_err());
    }

    // --- Run command tests ---

    #[test]
    fn test_parse_run_basic() {
        let cli = Cli::try_parse_from(["ion", "run", "hello"]).unwrap();
        if let Some(Commands::Run(args)) = cli.command {
            assert_eq!(args.prompt, "hello");
            assert!(args.model.is_none());
            assert!(!args.quiet);
        } else {
            panic!("Expected Run command");
        }
    }

    #[test]
    fn test_parse_run_with_model() {
        let cli = Cli::try_parse_from(["ion", "run", "-m", "anthropic/claude-3", "prompt"]).unwrap();
        if let Some(Commands::Run(args)) = cli.command {
            assert_eq!(args.model, Some("anthropic/claude-3".to_string()));
        } else {
            panic!("Expected Run command");
        }
    }

    #[test]
    fn test_parse_run_with_quiet() {
        let cli = Cli::try_parse_from(["ion", "run", "-q", "prompt"]).unwrap();
        if let Some(Commands::Run(args)) = cli.command {
            assert!(args.quiet);
        } else {
            panic!("Expected Run command");
        }
    }

    #[test]
    fn test_parse_run_with_output_format() {
        let cli = Cli::try_parse_from(["ion", "run", "-o", "json", "prompt"]).unwrap();
        if let Some(Commands::Run(args)) = cli.command {
            assert!(matches!(args.output_format, OutputFormat::Json));
        } else {
            panic!("Expected Run command");
        }
    }

    #[test]
    fn test_parse_run_with_max_turns() {
        let cli = Cli::try_parse_from(["ion", "run", "--max-turns", "5", "prompt"]).unwrap();
        if let Some(Commands::Run(args)) = cli.command {
            assert_eq!(args.max_turns, Some(5));
        } else {
            panic!("Expected Run command");
        }
    }

    #[test]
    fn test_parse_run_global_flags() {
        let cli = Cli::try_parse_from(["ion", "-y", "run", "prompt"]).unwrap();
        assert!(cli.auto_approve);
        assert!(cli.command.is_some());
    }

    // --- Login/Logout command tests ---

    #[test]
    fn test_parse_login_chatgpt() {
        let cli = Cli::try_parse_from(["ion", "login", "chatgpt"]).unwrap();
        if let Some(Commands::Login(args)) = cli.command {
            assert!(matches!(args.provider, AuthProvider::ChatGpt));
        } else {
            panic!("Expected Login command");
        }
    }

    #[test]
    fn test_parse_login_gemini() {
        let cli = Cli::try_parse_from(["ion", "login", "gemini"]).unwrap();
        if let Some(Commands::Login(args)) = cli.command {
            assert!(matches!(args.provider, AuthProvider::Gemini));
        } else {
            panic!("Expected Login command");
        }
    }

    #[test]
    fn test_parse_logout() {
        let cli = Cli::try_parse_from(["ion", "logout", "chatgpt"]).unwrap();
        if let Some(Commands::Logout(args)) = cli.command {
            assert!(matches!(args.provider, AuthProvider::ChatGpt));
        } else {
            panic!("Expected Logout command");
        }
    }

    // --- Permission settings tests ---

    #[test]
    fn test_permission_settings_default() {
        let settings = PermissionSettings::default();
        assert!(matches!(settings.mode, ToolMode::Write));
        assert!(!settings.auto_approve);
        assert!(!settings.no_sandbox);
        assert!(!settings.agi_enabled);
    }

    // Helper function tests moved to message_list.rs (shared implementation)
}
