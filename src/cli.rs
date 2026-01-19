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

/// Extract the most relevant argument from a tool call for display.
fn extract_key_arg(tool_name: &str, args: &serde_json::Value) -> String {
    let obj = match args.as_object() {
        Some(o) => o,
        None => return String::new(),
    };

    // Tool-specific key arguments
    let key = match tool_name {
        "read" | "write" | "edit" => "file_path",
        "bash" => "command",
        "glob" => "pattern",
        "grep" => "pattern",
        _ => {
            // Fall back to first string argument
            return obj
                .values()
                .find_map(|v| v.as_str())
                .map(|s| truncate_arg(s, 50))
                .unwrap_or_default();
        }
    };

    obj.get(key)
        .and_then(|v| v.as_str())
        .map(|s| truncate_arg(s, 60))
        .unwrap_or_default()
}

/// Truncate a string for display, showing the end for paths.
fn truncate_arg(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else if s.contains('/') {
        // For paths, show the end
        format!("...{}", &s[s.len().saturating_sub(max - 3)..])
    } else {
        // For other strings, show the beginning
        format!("{}...", &s[..max - 3])
    }
}

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
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Run a one-shot prompt (non-interactive)
    Run(RunArgs),
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

/// Deny handler for CLI mode without --yes flag (restricted tools will fail with clear message)
struct DenyApprovalHandler;

#[async_trait]
impl ApprovalHandler for DenyApprovalHandler {
    async fn ask_approval(&self, tool_name: &str, _args: &serde_json::Value) -> ApprovalResponse {
        eprintln!(
            "Tool '{}' requires approval. Use --yes flag to auto-approve.",
            tool_name
        );
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

/// Run the CLI one-shot mode
pub async fn run(args: RunArgs, auto_approve: bool) -> ExitCode {
    match run_inner(args, auto_approve).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("Error: {}", e);
            ExitCode::from(1)
        }
    }
}

async fn run_inner(args: RunArgs, auto_approve: bool) -> Result<ExitCode> {
    // Initialize tracing for CLI mode
    if args.verbose || std::env::var("ION_LOG").is_ok() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(std::io::stderr)
            .try_init();
    }

    // Load config
    let config = Config::load()?;

    // Determine working directory
    let working_dir = args
        .cwd
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    // Read prompt (handle stdin with "-")
    let prompt = if args.prompt == "-" {
        let mut buffer = String::new();
        io::stdin().read_to_string(&mut buffer)?;
        buffer.trim().to_string()
    } else {
        args.prompt
    };

    // Optionally prepend context file
    let prompt = if let Some(file_path) = args.context_file {
        let content = std::fs::read_to_string(&file_path)?;
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
        eprintln!("Error: Empty prompt");
        return Ok(ExitCode::from(1));
    }

    // Determine provider from config (or default to openrouter)
    let provider_id = config.provider.as_deref().unwrap_or("openrouter");
    let provider = Provider::from_id(provider_id).unwrap_or(Provider::OpenRouter);

    // Get API key (env var first, then config)
    let api_key = match config.api_key_for(provider_id) {
        Some(key) => key,
        None => {
            eprintln!(
                "Error: No API key for {}. Set {} or configure in ~/.ion/config.toml, or run `ion` to set up.",
                provider_id,
                match provider_id {
                    "anthropic" => "ANTHROPIC_API_KEY",
                    "openai" => "OPENAI_API_KEY",
                    "google" => "GOOGLE_API_KEY",
                    "groq" => "GROQ_API_KEY",
                    _ => "OPENROUTER_API_KEY",
                }
            );
            return Ok(ExitCode::from(1));
        }
    };

    // Determine model
    let model = args
        .model
        .or(config.model.clone())
        .unwrap_or_else(|| "anthropic/claude-sonnet-4".to_string());

    // Create LLM client
    let llm_client: Arc<dyn LlmApi> = Arc::new(Client::new(provider, api_key));

    // Create orchestrator
    // Note: --yes grants Write mode with auto-approve (no approval handler)
    // Use --agi for full autonomy (AGI mode + no sandbox)
    let orchestrator = if args.no_tools {
        // Truly disable all tools - empty orchestrator
        Arc::new(ToolOrchestrator::new(ToolMode::Read))
    } else if auto_approve {
        // Write mode with auto-approve (no approval handler = auto-approve)
        Arc::new(ToolOrchestrator::with_builtins(ToolMode::Write))
    } else {
        // Write mode with deny handler (restricted tools fail with clear message)
        let mut orch = ToolOrchestrator::with_builtins(ToolMode::Write);
        orch.set_approval_handler(Arc::new(DenyApprovalHandler));
        Arc::new(orch)
    };

    // Create agent
    let agent = Arc::new(Agent::new(llm_client, orchestrator));

    // Create session
    let session = Session::new(working_dir, model);
    let abort_token = session.abort_token.clone();

    // Create event channel
    let (tx, mut rx) = mpsc::channel::<AgentEvent>(100);

    // Run agent in background
    let agent_clone = agent.clone();
    let session_clone = session;
    let prompt_clone = prompt.clone();
    let max_turns = args.max_turns;

    let agent_handle = tokio::spawn(async move {
        agent_clone
            .run_task(session_clone, prompt_clone, tx, None, None)
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
                        print!("{}", text);
                        io::stdout().flush()?;
                    }
                    OutputFormat::StreamJson => {
                        let json =
                            serde_json::to_string(&JsonEvent::TextDelta { text: text.clone() })?;
                        println!("{}", json);
                    }
                    _ => {}
                }
            }
            AgentEvent::ThinkingDelta(text) => {
                if verbose {
                    match output_format {
                        OutputFormat::Text => {
                            eprint!("[thinking] {}", text);
                        }
                        OutputFormat::StreamJson => {
                            let json = serde_json::to_string(&JsonEvent::ThinkingDelta {
                                text: text.clone(),
                            })?;
                            println!("{}", json);
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
                    eprintln!("\nMax turns ({}) reached", max);
                    abort_token.cancel(); // Signal agent to stop
                    interrupted = true;
                    break;
                }
                if !quiet {
                    // Extract key argument for display
                    let key_arg = extract_key_arg(name, args);
                    match output_format {
                        OutputFormat::Text => {
                            eprintln!("\n> {}({})", name, key_arg);
                        }
                        OutputFormat::StreamJson => {
                            let json = serde_json::to_string(&JsonEvent::ToolCallStart {
                                id: id.clone(),
                                name: name.clone(),
                            })?;
                            println!("{}", json);
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
                                format!("{}...", truncated)
                            } else {
                                content.clone()
                            };
                            eprintln!("  -> {}", preview);
                        }
                        OutputFormat::StreamJson => {
                            let json = serde_json::to_string(&JsonEvent::ToolCallResult {
                                id: id.clone(),
                                content: content.clone(),
                                is_error: *is_error,
                            })?;
                            println!("{}", json);
                        }
                        _ => {}
                    }
                }
            }
            AgentEvent::Error(e) => {
                match output_format {
                    OutputFormat::Text => eprintln!("Error: {}", e),
                    OutputFormat::StreamJson | OutputFormat::Json => {
                        let json = serde_json::to_string(&JsonEvent::Error { message: e.clone() })?;
                        println!("{}", json);
                    }
                }
                return Ok(ExitCode::from(1));
            }
            _ => {}
        }
    }

    // Wait for agent to finish
    let result = agent_handle.await?;

    // Output final result
    match output_format {
        OutputFormat::Text => {
            if quiet {
                // In quiet mode, print the full response at the end
                println!("{}", response);
            } else if !response.ends_with('\n') {
                // In normal mode, just ensure trailing newline
                println!();
            }
        }
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(&JsonEvent::Done { response })?;
            println!("{}", json);
        }
        OutputFormat::StreamJson => {
            let json = serde_json::to_string(&JsonEvent::Done { response })?;
            println!("{}", json);
        }
    }

    // Return appropriate exit code
    if interrupted {
        Ok(ExitCode::from(3)) // Max turns reached
    } else {
        match result {
            Ok(_) => Ok(ExitCode::from(0)), // Success
            Err(e) => {
                eprintln!("Error: {}", e);
                Ok(ExitCode::from(1)) // Error
            }
        }
    }
}
