//! Local CLI and terminal host for the same coding Agent and Session.
use std::{
    collections::BTreeSet,
    io::{self, IsTerminal, Read, Write},
    path::PathBuf,
    sync::Arc,
};

use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, Subcommand};
use ion_ai::{Content, Message, ModelRef};
use ion_core::{
    CodingAgent, CodingAgentError, CodingAgentEvent, CodingSession, CodingToolSource, ForkPoint,
};
use ion_host::image_input::LoadedImage;
use ion_host::{
    CredentialStatus, Host, McpHttpServer, McpServer, McpStdioServer, Resources, SavedSelection,
    SessionBinding, Wire,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

mod agent_events;
mod clipboard;
mod external_editor;
mod rpc;
mod terminal_client;
mod transcript;
mod transcript_detail;
mod transcript_render;

#[derive(Parser)]
#[command(about = "Ion: a local coding agent")]
struct Cli {
    /// Run one prompt headlessly, then exit.
    #[arg(short = 'p', long = "print", global = true)]
    print: Option<String>,
    /// Emit JSONL progress records for a headless prompt.
    #[arg(long, global = true)]
    json: bool,
    /// Working directory for a new session (defaults to the current directory).
    #[arg(long, global = true)]
    cwd: Option<PathBuf>,
    /// Session database path or ID; new work starts a fresh session by default.
    #[arg(long, global = true, conflicts_with = "continue_session")]
    session: Option<PathBuf>,
    /// Continue the most recently active session in this working directory.
    #[arg(short = 'c', long = "continue", global = true)]
    continue_session: bool,
    /// Provider for this invocation; use with --model.
    #[arg(long, global = true)]
    provider: Option<String>,
    /// Exact model ID for this invocation; use with --provider.
    #[arg(long, global = true)]
    model: Option<String>,
    /// Attach an image file to the first submitted prompt (repeatable).
    #[arg(long, global = true)]
    image: Vec<PathBuf>,
    /// Interactive transcript surface: inline native scrollback (default) or fullscreen.
    #[arg(long, value_enum, global = true, default_value = "inline")]
    tui_mode: terminal_client::TuiMode,
    #[command(subcommand)]
    action: Option<Action>,
}

#[derive(Subcommand)]
enum Action {
    /// List models Ion can route to; account access may differ.
    Models { query: Option<String> },
    /// Select a catalog model, or configure a custom compatible endpoint.
    Use {
        provider: String,
        model: String,
        #[arg(long)]
        endpoint: Option<String>,
        #[arg(long, value_enum)]
        wire: Option<Wire>,
        #[arg(long)]
        api_key_env: Option<String>,
        /// Declare that a custom endpoint's model accepts image input.
        #[arg(long)]
        images: bool,
    },
    /// Save a provider API key entered at a masked terminal prompt.
    Login { provider: String },
    /// Remove a saved credential; an environment key stays active.
    Logout { provider: String },
    /// Show credential sources without displaying secrets.
    Auth,
    /// List saved sessions for the working directory.
    Sessions,
    /// List skills and prompt templates available in the working directory.
    Resources,
    /// Submit one prompt and print the committed final answer.
    Run { prompt: String },
    /// Open the terminal chat client.
    Chat,
    /// Configure explicitly connected MCP tool servers.
    Mcp {
        #[command(subcommand)]
        action: McpAction,
    },
    /// Control a persistent coding session over JSONL stdin/stdout.
    Rpc,
    /// Inspect committed Session history without running a model or tool.
    Inspect,
    /// Print a readable committed transcript, or save it to a new file.
    Export { path: Option<PathBuf> },
    /// List numbered Turns in a saved Session.
    Turns,
    /// Copy a saved conversation into an independent Session in this directory.
    Clone,
    /// Fork a saved Session before a Turn, or after it with --after.
    Fork {
        turn: u64,
        #[arg(long)]
        after: bool,
    },
    /// Summarize settled history for continued work, retaining the raw log.
    Compact,
}

#[derive(Subcommand)]
enum McpAction {
    List,
    Add {
        name: String,
        command: String,
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },
    AddHttp {
        name: String,
        url: String,
        #[arg(long)]
        bearer_token_env: Option<String>,
    },
    Remove {
        name: String,
    },
}

#[tokio::main]
async fn main() {
    if let Err(error) = run_cli(Cli::parse()).await {
        eprintln!("ion: {error:#}");
        std::process::exit(1);
    }
}

async fn run_cli(cli: Cli) -> Result<()> {
    if !cli.image.is_empty()
        && !matches!(
            &cli.action,
            Some(Action::Run { .. }) | Some(Action::Chat) | None
        )
    {
        bail!("--image requires a coding prompt or chat");
    }
    if cli.json
        && (!matches!(&cli.action, Some(Action::Run { .. }) | None)
            || cli.action.is_none() && cli.print.is_none())
    {
        bail!("--json requires `run PROMPT` or `--print PROMPT`");
    }
    let host = Arc::new(Host::from_environment()?);
    let credentials = host.credentials();
    let models = host.models();
    match cli.action {
        Some(_) if cli.print.is_some() => bail!("--print cannot be combined with a subcommand"),
        Some(Action::Models { query }) => {
            let query = query.unwrap_or_default().to_ascii_lowercase();
            for choice in models.choices(credentials)? {
                let model = choice.selected;
                if !model.provider.contains(&query)
                    && !model.model.to_ascii_lowercase().contains(&query)
                    && !choice.label.to_ascii_lowercase().contains(&query)
                {
                    continue;
                }
                println!(
                    "{}/{}\t{}\t{}",
                    model.provider,
                    model.model,
                    choice.label,
                    status_label(choice.credential)
                );
            }
            Ok(())
        }
        Some(Action::Use {
            provider,
            model,
            endpoint,
            wire,
            api_key_env,
            images,
        }) => {
            let saved = SavedSelection {
                provider,
                model,
                endpoint,
                wire,
                api_key_env,
                image_input: images,
            };
            let resolved = models.save_default(&saved)?;
            println!("Selected {}/{}", resolved.provider, resolved.model);
            Ok(())
        }
        Some(Action::Login { provider }) => {
            ensure!(
                io::stdin().is_terminal(),
                "API-key login requires a terminal; use an environment variable in headless mode"
            );
            let key = rpassword::prompt_password(format!("{provider} API key: "))?;
            credentials.save_api_key(&provider, &key)?;
            println!("Saved {provider} credential.");
            Ok(())
        }
        Some(Action::Logout { provider }) => {
            credentials.remove(&provider)?;
            println!("Removed saved {provider} credential. Any environment key remains active.");
            Ok(())
        }
        Some(Action::Auth) => {
            let mut seen = BTreeSet::new();
            for choice in models.choices(credentials)? {
                let model = choice.selected;
                if seen.insert(model.provider.clone()) {
                    println!("{}: {}", model.provider, status_label(choice.credential));
                }
            }
            Ok(())
        }
        Some(Action::Sessions) => {
            let cwd = cli.cwd.unwrap_or(std::env::current_dir()?).canonicalize()?;
            let catalog = host.sessions(cwd);
            for session in catalog.list()? {
                println!(
                    "{}\t{}\t{} turn(s)\t{}\t{}",
                    session.id,
                    session.name.as_deref().unwrap_or(""),
                    session.turns,
                    session
                        .model
                        .as_ref()
                        .map_or_else(String::new, |model| format!(
                            "{}/{}",
                            model.provider, model.model
                        )),
                    session.preview.as_deref().unwrap_or("")
                );
            }
            Ok(())
        }
        Some(Action::Resources) => {
            let cwd = cli.cwd.unwrap_or(std::env::current_dir()?).canonicalize()?;
            let resources = host.resources(&cwd)?;
            for skill in resources.skills() {
                println!(
                    "skill\t{}\t{}\t{}",
                    skill.name,
                    skill.description,
                    skill.path.display()
                );
            }
            for template in resources.templates() {
                println!(
                    "prompt\t{}\t{}\t{}",
                    template.name,
                    template.description,
                    template.path.display()
                );
            }
            for diagnostic in resources.diagnostics() {
                eprintln!(
                    "[resource: {}: {}]",
                    diagnostic.path.display(),
                    diagnostic.message
                );
            }
            Ok(())
        }
        Some(Action::Mcp { action }) => {
            let config = host.mcp_config();
            match action {
                McpAction::List => {
                    let listing = config.list()?;
                    for diagnostic in listing.diagnostics {
                        eprintln!("[mcp config: {diagnostic}]");
                    }
                    for (name, server) in listing.servers {
                        match server {
                            McpServer::Stdio(server) => {
                                println!("{name}\t{} {}", server.command, server.args.join(" "));
                            }
                            McpServer::Http(server) => {
                                println!("{name}\t{}", server.url);
                            }
                        }
                    }
                }
                McpAction::Add {
                    name,
                    command,
                    args,
                } => {
                    config.add(&name, McpServer::Stdio(McpStdioServer { command, args }))?;
                    println!("Added MCP server {name}");
                }
                McpAction::AddHttp {
                    name,
                    url,
                    bearer_token_env,
                } => {
                    config.add(
                        &name,
                        McpServer::Http(McpHttpServer {
                            url,
                            bearer_token_env,
                        }),
                    )?;
                    println!("Added MCP server {name}");
                }
                McpAction::Remove { name } => {
                    config.remove(&name)?;
                    println!("Removed MCP server {name}");
                }
            }
            Ok(())
        }
        action => {
            let explicit_cwd = cli.cwd.is_some();
            let cwd = cli.cwd.unwrap_or(std::env::current_dir()?).canonicalize()?;
            ensure!(cwd.is_dir(), "working directory is not a directory");
            let catalog = host.sessions(cwd.clone());
            let path = if let Some(explicit) = cli.session {
                catalog.resolve_explicit(explicit)?
            } else if cli.continue_session {
                catalog.latest()?
            } else if matches!(
                action,
                Some(
                    Action::Inspect
                        | Action::Export { .. }
                        | Action::Turns
                        | Action::Compact
                        | Action::Clone
                        | Action::Fork { .. }
                )
            ) {
                bail!("use --continue or --session ID; run `ion sessions` to find one")
            } else {
                catalog.new_path()?
            };
            let existing = if path.is_file() {
                Some(CodingSession::inspect(&path)?)
            } else {
                None
            };
            if let Some(view) = &existing {
                ensure!(
                    !explicit_cwd || view.cwd == cwd,
                    "--cwd does not match the session's working directory ({})",
                    view.cwd.display()
                );
            }
            if matches!(action, Some(Action::Inspect)) {
                let view = existing.context("session does not exist")?;
                let mut output = serde_json::to_value(view)?;
                redact_image_payloads(&mut output);
                println!("{}", serde_json::to_string_pretty(&output)?);
                return Ok(());
            }
            if let Some(Action::Export { path: output }) = &action {
                let view = existing.as_ref().context("session does not exist")?;
                if let Some(output) = output {
                    let target = if output.is_absolute() {
                        output.clone()
                    } else {
                        view.cwd.join(output)
                    };
                    transcript::save_new(view, &target).with_context(|| {
                        format!("cannot save transcript to {}", target.display())
                    })?;
                    println!("{}", target.display());
                } else {
                    io::stdout().write_all(transcript::render(view).as_bytes())?;
                }
                return Ok(());
            }
            if matches!(action, Some(Action::Turns)) {
                let view = existing.context("session does not exist")?;
                for turn in view.turns() {
                    println!(
                        "{}\t{}\t{}",
                        turn.turn,
                        if turn.end.is_some() {
                            "ended"
                        } else {
                            "active"
                        },
                        preview_input(&turn.input)
                    );
                }
                return Ok(());
            }
            if matches!(action, Some(Action::Clone)) {
                ensure!(existing.is_some(), "session does not exist");
                let source = CodingSession::open(&path)?;
                let target = catalog.new_path()?;
                let cloned = source.clone_to(&target)?;
                let id = cloned
                    .path()
                    .file_stem()
                    .context("cloned session has no ID")?;
                println!("Cloned session as {}", id.to_string_lossy());
                return Ok(());
            }
            if let Some(Action::Fork { turn, after }) = &action {
                ensure!(existing.is_some(), "session does not exist");
                let source = CodingSession::open(&path)?;
                let target = catalog.new_path()?;
                let fork = source.fork_to(
                    &target,
                    if *after {
                        ForkPoint::AfterTurn(*turn)
                    } else {
                        ForkPoint::BeforeTurn(*turn)
                    },
                )?;
                let id = fork
                    .path()
                    .file_stem()
                    .context("forked session has no ID")?;
                println!("Forked session as {}", id.to_string_lossy());
                return Ok(());
            }
            if matches!(action, Some(Action::Compact)) {
                ensure!(existing.is_some(), "session does not exist");
            }
            let session_cwd = existing.as_ref().map_or(&cwd, |view| &view.cwd);
            let previous = existing.as_ref().and_then(|view| view.last_model.clone());
            let selected = models.choose(cli.provider, cli.model, previous, credentials)?;
            selected.require_access(credentials)?;
            let model = selected.identity();
            let images = cli
                .image
                .iter()
                .map(|path| {
                    let path = if path.is_absolute() {
                        path.clone()
                    } else {
                        session_cwd.join(path)
                    };
                    ion_host::image_input::load_image(&selected, &path)
                })
                .collect::<Result<Vec<_>>>()?;
            let session = Arc::new(if path.is_file() {
                CodingSession::open(&path)?
            } else {
                CodingSession::create(&path, &cwd)?
            });
            let startup = if matches!(action, Some(Action::Compact)) {
                ion_host::McpStartup::default()
            } else {
                host.external_tools(session.cwd()).await
            };
            let mut startup_diagnostics = startup
                .diagnostics
                .into_iter()
                .map(|diagnostic| format!("[mcp: {diagnostic}]"))
                .collect::<Vec<_>>();
            let external_mcp = startup.tools;
            let external_tools: Option<Arc<dyn CodingToolSource>> = external_mcp
                .as_ref()
                .map(|tools| tools.clone() as Arc<dyn CodingToolSource>);
            let binding = SessionBinding::new(host, session, selected, external_tools)?;
            for diagnostic in binding.resources().diagnostics() {
                startup_diagnostics.push(format!(
                    "[resource: {}: {}]",
                    diagnostic.path.display(),
                    diagnostic.message
                ));
            }
            if cli.print.is_some() || !matches!(&action, Some(Action::Chat) | None) {
                for diagnostic in &startup_diagnostics {
                    eprintln!("{diagnostic}");
                }
            }
            let result = async {
                match action {
                    Some(Action::Run { prompt }) => {
                        headless(
                            binding.session().clone(),
                            binding.agent().clone(),
                            model,
                            binding.instructions().to_owned(),
                            with_piped_input(expand_input(binding.resources(), prompt)?)?,
                            images,
                            cli.json,
                        )
                        .await
                    }
                    Some(Action::Compact) => {
                        let stop = CancellationToken::new();
                        let signal_stop = stop.clone();
                        let signal = tokio::spawn(async move {
                            if tokio::signal::ctrl_c().await.is_ok() {
                                signal_stop.cancel();
                            }
                        });
                        let result = binding
                            .agent()
                            .compact(binding.session(), model, stop, |_| {})
                            .await;
                        signal.abort();
                        if result? {
                            println!("Context summarized; raw Session history retained");
                        } else {
                            println!("No settled history to summarize");
                        }
                        Ok(())
                    }
                    Some(Action::Clone | Action::Turns | Action::Fork { .. }) => {
                        unreachable!("session action handled before model selection")
                    }
                    Some(Action::Rpc) => rpc::run(binding).await,
                    Some(Action::Chat) | None if cli.print.is_none() => {
                        terminal_client::chat(terminal_client::ChatInit {
                            binding,
                            images,
                            startup_diagnostics,
                            tui_mode: cli.tui_mode,
                        })
                        .await
                    }
                    None => {
                        headless(
                            binding.session().clone(),
                            binding.agent().clone(),
                            model,
                            binding.instructions().to_owned(),
                            with_piped_input(expand_input(
                                binding.resources(),
                                cli.print.expect("matched Some"),
                            )?)?,
                            images,
                            cli.json,
                        )
                        .await
                    }
                    _ => unreachable!("non-agent action handled above"),
                }
            }
            .await;
            if let Some(tools) = external_mcp {
                tools.shutdown().await;
            }
            result
        }
    }
}

fn expand_input(resources: &Resources, prompt: String) -> Result<String> {
    resources
        .expand_command(&prompt)
        .transpose()
        .map(|expanded| expanded.unwrap_or(prompt))
}

fn preview_input(input: &Message) -> String {
    let text = input
        .content
        .iter()
        .find_map(|part| match part {
            Content::Text(text) if !text.trim().is_empty() => Some(text.as_str()),
            _ => None,
        })
        .unwrap_or("[image]");
    text.lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(120)
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect()
}

fn status_label(status: CredentialStatus) -> &'static str {
    match status {
        CredentialStatus::Environment => "environment key",
        CredentialStatus::Saved => "saved login",
        CredentialStatus::Missing => "no credential",
    }
}

async fn headless(
    session: Arc<CodingSession>,
    agent: Arc<CodingAgent>,
    model: ModelRef,
    instructions: String,
    prompt: String,
    images: Vec<LoadedImage>,
    json_output: bool,
) -> Result<()> {
    let id = session
        .path()
        .file_stem()
        .context("session has no ID")?
        .to_string_lossy();
    if json_output {
        write_json_record(&json!({"type":"session","id":id,"cwd":session.cwd()}))?;
    } else {
        eprintln!("[session: {id}]");
    }
    let stop = CancellationToken::new();
    let output_stop = stop.clone();
    let signal_stop = stop.clone();
    let signal = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal_stop.cancel();
        }
    });
    let mut output_error = None;
    let input = Message::user_input(prompt, images);
    let result = agent
        .submit_message(&session, model, input, instructions, stop, |event| {
            if json_output {
                let record = agent_events::event_record(event);
                if output_error.is_none()
                    && let Err(error) = write_json_record(&record)
                {
                    output_error = Some(error);
                    output_stop.cancel();
                }
                return;
            }
            match event {
                CodingAgentEvent::TurnAccepted { .. }
                | CodingAgentEvent::TextDelta(_)
                | CodingAgentEvent::AssistantCommitted { .. }
                | CodingAgentEvent::SteeringCommitted { .. } => {}
                CodingAgentEvent::ProviderRetry {
                    attempt,
                    max_retries,
                    delay_ms,
                } => {
                    eprintln!("[provider retry {attempt}/{max_retries} in {delay_ms}ms]")
                }
                CodingAgentEvent::ToolStarted { name, .. } => eprintln!("[tool: {name}]"),
                CodingAgentEvent::ToolFinished { name, output, .. } => {
                    eprintln!("[tool: {name}] {}", output.value);
                    for image in &output.images {
                        eprintln!("[tool image: {}]", image.mime_type().as_str());
                    }
                }
                CodingAgentEvent::ToolRejected { name, output, .. } => {
                    eprintln!("[tool skipped: {name}] {}", output.value)
                }
                CodingAgentEvent::InterruptedCalls(count) => {
                    eprintln!("[recovered {count} incomplete tool call(s); effects unknown]")
                }
                CodingAgentEvent::ContextCompacted { through_entry } => {
                    eprintln!("[context summarized through entry {through_entry}]")
                }
                CodingAgentEvent::ProviderReplayRebased => {
                    eprintln!("[provider reasoning context reset]")
                }
                CodingAgentEvent::ProviderReplayNotice {
                    action,
                    reason,
                    count,
                } => eprintln!("[provider reasoning {action}: {count} block(s), {reason}]"),
                CodingAgentEvent::ResponseRestarted => {
                    eprintln!("[incomplete response discarded; retrying]");
                }
                CodingAgentEvent::ToolCatalogWarning(message) => {
                    eprintln!("[tool catalog: {message}]");
                }
                CodingAgentEvent::Final(_) => {}
            }
        })
        .await;
    signal.abort();
    if json_output {
        if let Some(error) = output_error {
            return Err(error.into());
        }
        match &result {
            Ok(_) => write_json_record(&json!({"type":"run_end","status":"completed"}))?,
            Err(CodingAgentError::Cancelled) => write_json_record(
                &json!({"type":"run_end","status":"cancelled","error":"turn was cancelled"}),
            )?,
            Err(error) => write_json_record(
                &json!({"type":"run_end","status":"failed","error":error.to_string()}),
            )?,
        }
        result?;
        return Ok(());
    }
    let answer = result?;
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{answer}")?;
    stdout.flush()?;
    Ok(())
}

fn write_json_record(value: &serde_json::Value) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    bytes.push(b'\n');
    let mut stdout = io::stdout().lock();
    stdout.write_all(&bytes)?;
    stdout.flush()
}

fn redact_image_payloads(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(fields) => {
            if fields
                .get("mime_type")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|mime| mime.starts_with("image/"))
                && let Some(serde_json::Value::String(data)) = fields.get_mut("data")
            {
                *data = format!("[base64 image data omitted: {} characters]", data.len());
            }
            for child in fields.values_mut() {
                redact_image_payloads(child);
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                redact_image_payloads(child);
            }
        }
        _ => {}
    }
}

fn with_piped_input(prompt: String) -> Result<String> {
    if io::stdin().is_terminal() {
        return Ok(prompt);
    }
    const MAX_STDIN: u64 = 8 * 1024 * 1024;
    let mut bytes = Vec::new();
    io::stdin().take(MAX_STDIN + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= MAX_STDIN, "piped stdin exceeds 8 MiB");
    if bytes.is_empty() {
        return Ok(prompt);
    }
    let input = String::from_utf8(bytes).context("piped stdin is not UTF-8")?;
    Ok(format!("{input}\n{prompt}"))
}
