//! CLI module for one-shot/non-interactive mode.

use crate::agent::{Agent, AgentEvent};
use crate::config::Config;
use crate::provider::{ApiProvider, create_provider};
use crate::session::Session;
use crate::tool::{ApprovalHandler, ApprovalResponse, ToolMode, ToolOrchestrator};
use anyhow::Result;
use async_trait::async_trait;
use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use tokio::sync::mpsc;

/// ion - Local-first TUI coding agent
#[derive(Parser, Debug)]
#[command(name = "ion", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
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

    /// Auto-approve all tool calls
    #[arg(short = 'y', long)]
    pub yes: bool,

    /// Maximum agentic turns before stopping
    #[arg(long)]
    pub max_turns: Option<usize>,

    /// Include file content as context
    #[arg(short = 'f', long = "file")]
    pub context_file: Option<PathBuf>,

    /// Working directory
    #[arg(long)]
    pub cwd: Option<PathBuf>,

    /// Don't persist session
    #[arg(long)]
    pub no_session: bool,

    /// Disable tools (pure chat mode)
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

/// Auto-approve handler for CLI mode with --yes flag
struct AutoApproveHandler;

#[async_trait]
impl ApprovalHandler for AutoApproveHandler {
    async fn ask_approval(&self, _tool_name: &str, _args: &serde_json::Value) -> ApprovalResponse {
        ApprovalResponse::Yes
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
pub async fn run(args: RunArgs) -> ExitCode {
    match run_inner(args).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("Error: {}", e);
            ExitCode::from(1)
        }
    }
}

async fn run_inner(args: RunArgs) -> Result<ExitCode> {
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

    // Determine provider and API key (config file, then env vars)
    let (api_provider, api_key) = if let Some(key) = config.openrouter_api_key.clone() {
        (ApiProvider::OpenRouter, key)
    } else if let Some(key) = config.anthropic_api_key.clone() {
        (ApiProvider::Anthropic, key)
    } else if let Ok(key) = std::env::var("OPENROUTER_API_KEY") {
        (ApiProvider::OpenRouter, key)
    } else if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        (ApiProvider::Anthropic, key)
    } else {
        eprintln!("Error: No API key configured. Set OPENROUTER_API_KEY or ANTHROPIC_API_KEY, or run `ion` to set up.");
        return Ok(ExitCode::from(1));
    };

    // Determine model
    let model = args
        .model
        .or(config.default_model.clone())
        .unwrap_or_else(|| "anthropic/claude-sonnet-4".to_string());

    // Create provider
    let provider = create_provider(api_provider, api_key, config.provider_prefs.clone());

    // Create orchestrator
    let tool_mode = if args.no_tools {
        ToolMode::Read // No tools, but still safe
    } else if args.yes {
        ToolMode::Agi // Full autonomy
    } else {
        ToolMode::Write // Will prompt for approval (not ideal for CLI)
    };

    let mut orchestrator = ToolOrchestrator::with_builtins(tool_mode);

    // Set auto-approve handler if --yes
    if args.yes {
        orchestrator.set_approval_handler(Arc::new(AutoApproveHandler));
    }

    let orchestrator = Arc::new(orchestrator);

    // Create agent
    let agent = Arc::new(Agent::new(provider, orchestrator));

    // Create session
    let session = Session::new(working_dir, model);

    // Create event channel
    let (tx, mut rx) = mpsc::channel::<AgentEvent>(100);

    // Run agent in background
    let agent_clone = agent.clone();
    let session_clone = session.clone();
    let prompt_clone = prompt.clone();
    let max_turns = args.max_turns;

    let agent_handle = tokio::spawn(async move {
        agent_clone
            .run_task(session_clone, prompt_clone, tx, None)
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
                        let json = serde_json::to_string(&JsonEvent::TextDelta {
                            text: text.clone(),
                        })?;
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
            AgentEvent::ToolCallStart(id, name) => {
                turn_count += 1;
                if let Some(max) = max_turns {
                    if turn_count >= max {
                        eprintln!("\nMax turns ({}) reached", max);
                        interrupted = true;
                        break;
                    }
                }
                if !quiet {
                    match output_format {
                        OutputFormat::Text => {
                            eprintln!("\n> {}({})", name, id);
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
                            let preview = if content.len() > 200 {
                                format!("{}...", &content[..200])
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
                        let json = serde_json::to_string(&JsonEvent::Error {
                            message: e.clone(),
                        })?;
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
            let json = serde_json::to_string(&JsonEvent::Done {
                response: response.clone(),
            })?;
            println!("{}", json);
        }
    }

    // Return appropriate exit code
    if interrupted {
        Ok(ExitCode::from(3)) // Max turns reached
    } else if result.is_err() {
        Ok(ExitCode::from(1)) // Error
    } else {
        Ok(ExitCode::from(0)) // Success
    }
}
