//! Local CLI and terminal host for the same coding Agent and Session.
use std::{
    collections::BTreeSet,
    io::{self, IsTerminal, Read, Write},
    path::PathBuf,
    sync::Arc,
};

use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, Subcommand};
use ion_ai::{Content, Message};
use ion_core::{CodingSession, CodingToolSource, ForkPoint, ToolSelection};
use ion_host::{
    Application, CredentialStatus, Host, McpExposure, McpHttpServer, McpServer, McpStdioServer,
    Resources, SavedSelection, Wire,
};

mod agent_events;
mod clipboard;
mod clipboard_reader;
mod display_text;
mod edit_diff;
mod external_editor;
mod headless;
mod inspection;
mod markdown;
mod presentation_style;
mod publication;
mod rpc;
mod stdio;
mod terminal_client;
mod terminal_commands;
mod tool_output;
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
    /// Enable bounded JavaScript composition alongside direct tools.
    #[arg(long, global = true)]
    code_mode: bool,
    /// Limit model-visible tools to comma-separated names or `*` patterns.
    #[arg(long, global = true, value_delimiter = ',')]
    tools: Vec<String>,
    /// Remove model-visible tools by comma-separated names or `*` patterns.
    #[arg(long, global = true, value_delimiter = ',')]
    exclude_tools: Vec<String>,
    /// Use only read-only model tools; excludes native mutations, direct shell and managed children.
    #[arg(long, global = true, conflicts_with_all = ["tools", "exclude_tools"])]
    read_only: bool,
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
    /// Generation effort: default, off, low, medium, high or budget:TOKENS; route support varies.
    #[arg(long, global = true)]
    reasoning: Option<ion_ai::Reasoning>,
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
    /// Private clipboard transfer helper; never initializes model or Session services.
    #[command(name = "__clipboard-read", hide = true)]
    ClipboardRead,
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
    /// Inspect explicit executable user commands.
    Commands {
        #[command(subcommand)]
        action: Option<CommandsAction>,
    },
    /// Trust the current project's MCP configuration for its current contents.
    Trust,
    /// Remove trust for the current project's MCP configuration.
    Untrust,
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

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum ExposureArg {
    Direct,
    Deferred,
    Hidden,
}

impl From<ExposureArg> for McpExposure {
    fn from(value: ExposureArg) -> Self {
        match value {
            ExposureArg::Direct => Self::Direct,
            ExposureArg::Deferred => Self::Deferred,
            ExposureArg::Hidden => Self::Hidden,
        }
    }
}

#[derive(Subcommand)]
enum CommandsAction {
    /// List enabled commands and origin; untrusted project entries are inactive.
    List,
    /// Trust the current project commands manifest for its exact contents.
    Trust,
    /// Remove trust for the current project commands manifest.
    Untrust,
}

#[derive(Subcommand)]
enum McpAction {
    List,
    Add {
        name: String,
        command: String,
        /// Write to the project `.ion/mcp.json` instead of user configuration.
        #[arg(long)]
        project: bool,
        #[arg(long, value_enum, default_value = "direct")]
        exposure: ExposureArg,
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },
    AddHttp {
        name: String,
        url: String,
        #[arg(long)]
        bearer_token_env: Option<String>,
        #[arg(long)]
        project: bool,
        #[arg(long, value_enum, default_value = "direct")]
        exposure: ExposureArg,
    },
    Remove {
        name: String,
        #[arg(long)]
        project: bool,
    },
}

fn parse_tool_selection(cli: &Cli) -> Result<ToolSelection> {
    if cli.read_only {
        return Ok(ToolSelection::all());
    }
    let include = validated_tool_patterns(&cli.tools)?;
    let exclude = validated_tool_patterns(&cli.exclude_tools)?;
    let selection = if include.is_empty() {
        ToolSelection::all()
    } else {
        ToolSelection::include(include)
    };
    Ok(selection.with_exclude(exclude))
}

fn configured_cwd(cwd: Option<PathBuf>) -> Result<PathBuf> {
    let path = cwd.unwrap_or(std::env::current_dir()?);
    let path = path
        .canonicalize()
        .with_context(|| format!("cannot resolve working directory {}", path.display()))?;
    ensure!(path.is_dir(), "working directory is not a directory");
    Ok(path)
}

fn validated_tool_patterns(values: &[String]) -> Result<Vec<String>> {
    values
        .iter()
        .map(|value| {
            let value = value.trim();
            ensure!(!value.is_empty(), "tool pattern is empty");
            Ok(value.to_owned())
        })
        .collect()
}

#[tokio::main]
async fn main() {
    if let Err(error) = run_cli(Cli::parse()).await {
        // Reporting to a broken stderr must not replace the original failure
        // with a printing panic after controlled cleanup has completed.
        let _ = writeln!(io::stderr().lock(), "ion: {error:#}");
        std::process::exit(1);
    }
}

async fn run_cli(cli: Cli) -> Result<()> {
    if matches!(cli.action, Some(Action::ClipboardRead)) {
        return clipboard_reader::write_native();
    }
    if cli.reasoning.is_some()
        && !matches!(
            &cli.action,
            Some(Action::Run { .. } | Action::Chat | Action::Rpc | Action::Compact) | None
        )
    {
        bail!("--reasoning requires a coding prompt, chat, RPC or compaction");
    }
    if !cli.image.is_empty()
        && !matches!(
            &cli.action,
            Some(Action::Run { .. }) | Some(Action::Chat) | None
        )
    {
        bail!("--image requires a coding prompt or chat");
    }
    if (cli.read_only || !cli.tools.is_empty() || !cli.exclude_tools.is_empty())
        && !matches!(
            &cli.action,
            Some(Action::Run { .. } | Action::Chat | Action::Rpc) | None
        )
    {
        bail!("--tools, --exclude-tools and --read-only require a coding client");
    }
    if cli.json
        && (!matches!(&cli.action, Some(Action::Run { .. }) | None)
            || cli.action.is_none() && cli.print.is_none())
    {
        bail!("--json requires `run PROMPT` or `--print PROMPT`");
    }
    let tool_selection = parse_tool_selection(&cli)?;
    let host = Arc::new(Host::from_environment()?);
    let credentials = host.credentials();
    let models = host.models();
    match cli.action {
        Some(Action::ClipboardRead) => unreachable!("clipboard helper handled before Host setup"),
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
                print_stdout(format_args!(
                    "{}/{}\t{}\t{}",
                    metadata_field(&model.provider),
                    metadata_field(&model.model),
                    metadata_field(choice.label),
                    status_label(choice.credential)
                ))?;
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
            print_stdout(format_args!(
                "Selected {}/{}",
                metadata_field(&resolved.provider),
                metadata_field(&resolved.model)
            ))?;
            Ok(())
        }
        Some(Action::Login { provider }) => {
            ensure!(
                io::stdin().is_terminal() && io::stdout().is_terminal(),
                "API-key login requires a terminal; use an environment variable in headless mode"
            );
            let mut terminal = ion_terminal::TerminalSession::with_requirements(
                ion_terminal::TerminalRequirements {
                    keyboard_enhancement: false,
                    ..Default::default()
                },
            )
            .await?;
            // Keep input validation intact: unsafe provider text is rejected
            // by the secret reader, not transformed into a different prompt.
            let key = terminal
                .read_secret(&format!("{provider} API key: "))
                .await?;
            terminal.restore()?;
            credentials.save_api_key(&provider, &key)?;
            print_stdout(format_args!(
                "Saved {} credential.",
                metadata_field(&provider)
            ))?;
            Ok(())
        }
        Some(Action::Logout { provider }) => {
            credentials.remove(&provider)?;
            print_stdout(format_args!(
                "Removed saved {} credential. Any environment key remains active.",
                metadata_field(&provider)
            ))?;
            Ok(())
        }
        Some(Action::Auth) => {
            let mut seen = BTreeSet::new();
            for choice in models.choices(credentials)? {
                let model = choice.selected;
                if seen.insert(model.provider.clone()) {
                    print_stdout(format_args!(
                        "{}: {}",
                        metadata_field(&model.provider),
                        status_label(choice.credential)
                    ))?;
                }
            }
            Ok(())
        }
        Some(Action::Sessions) => {
            let cwd = cli.cwd.unwrap_or(std::env::current_dir()?).canonicalize()?;
            let catalog = host.sessions(cwd);
            for session in catalog.list()? {
                print_stdout(format_args!(
                    "{}\t{}\t{} turn(s)\t{}\t{}",
                    metadata_field(&session.id),
                    metadata_field(session.name.as_deref().unwrap_or("")),
                    session.turns,
                    session
                        .model
                        .as_ref()
                        .map_or_else(String::new, |model| format!(
                            "{}/{}",
                            metadata_field(&model.provider),
                            metadata_field(&model.model)
                        )),
                    metadata_field(session.preview.as_deref().unwrap_or(""))
                ))?;
            }
            Ok(())
        }
        Some(Action::Resources) => {
            let cwd = cli.cwd.unwrap_or(std::env::current_dir()?).canonicalize()?;
            let resources = host.resources(&cwd)?;
            for skill in resources.skills() {
                print_stdout(format_args!(
                    "skill\t{}\t{}\t{}",
                    metadata_field(&skill.name),
                    metadata_field(&skill.description),
                    metadata_field(&skill.path.to_string_lossy())
                ))?;
            }
            for template in resources.templates() {
                print_stdout(format_args!(
                    "prompt\t{}\t{}\t{}",
                    metadata_field(&template.name),
                    metadata_field(&template.description),
                    metadata_field(&template.path.to_string_lossy())
                ))?;
            }
            for diagnostic in resources.diagnostics() {
                print_stderr(format_args!(
                    "[resource: {}: {}]",
                    metadata_field(&diagnostic.path.to_string_lossy()),
                    metadata_field(&diagnostic.message)
                ))?;
            }
            Ok(())
        }
        Some(Action::Commands { action }) => {
            let cwd = configured_cwd(cli.cwd.clone())?;
            let commands = ion_host::CommandConfig::with_project(host.config_root(), &cwd)?;
            match action.unwrap_or(CommandsAction::List) {
                CommandsAction::List => {
                    if let Some(path) = commands.project_path().filter(|path| path.is_file()) {
                        print_stderr(format_args!(
                            "[commands project: {}]",
                            metadata_field(&path.to_string_lossy())
                        ))?;
                    }
                    let catalog = commands.list()?;
                    for diagnostic in catalog.diagnostics() {
                        print_stderr(format_args!("[commands: {}]", metadata_field(diagnostic)))?;
                    }
                    for command in catalog.iter() {
                        print_stdout(format_args!(
                            "{}\t{}\t{}",
                            metadata_field(&command.name),
                            metadata_field(&command.description),
                            command.origin
                        ))?;
                    }
                }
                CommandsAction::Trust => {
                    let path = commands.trust_project()?;
                    print_stdout(format_args!(
                        "Trusted project commands manifest {}",
                        metadata_field(&path.to_string_lossy())
                    ))?;
                }
                CommandsAction::Untrust => {
                    commands.untrust_project()?;
                    print_stdout(format_args!("Removed project commands trust"))?;
                }
            }
            Ok(())
        }
        Some(Action::Trust) => {
            let cwd = configured_cwd(cli.cwd.clone())?;
            let config = host.mcp_config_for(&cwd)?;
            config.trust_project()?;
            let path = config
                .project_path()
                .context("project MCP configuration path is unavailable")?;
            print_stdout(format_args!(
                "Trusted project MCP configuration {}",
                metadata_field(&path.to_string_lossy())
            ))?;
            Ok(())
        }
        Some(Action::Untrust) => {
            let cwd = configured_cwd(cli.cwd.clone())?;
            let config = host.mcp_config_for(&cwd)?;
            config.untrust_project()?;
            print_stdout(format_args!("Removed project MCP trust"))?;
            Ok(())
        }
        Some(Action::Mcp { action }) => {
            let cwd = configured_cwd(cli.cwd.clone())?;
            let config = host.mcp_config_for(&cwd)?;
            match action {
                McpAction::List => {
                    if let Some(path) = config.project_path().filter(|path| path.is_file()) {
                        print_stderr(format_args!(
                            "[mcp project: {}]",
                            metadata_field(&path.to_string_lossy())
                        ))?;
                    }
                    let listing = config.list()?;
                    for diagnostic in listing.diagnostics {
                        print_stderr(format_args!(
                            "[mcp config: {}]",
                            metadata_field(&diagnostic)
                        ))?;
                    }
                    for (name, server) in listing.servers {
                        match server {
                            McpServer::Stdio(server) => {
                                print_stdout(format_args!(
                                    "{}\t{}\t{} {}",
                                    metadata_field(&name),
                                    server.exposure.as_str(),
                                    metadata_field(&server.command),
                                    metadata_field(&server.args.join(" "))
                                ))?;
                            }
                            McpServer::Http(server) => {
                                print_stdout(format_args!(
                                    "{}\t{}\t{}",
                                    metadata_field(&name),
                                    server.exposure.as_str(),
                                    metadata_field(&server.url)
                                ))?;
                            }
                        }
                    }
                }
                McpAction::Add {
                    name,
                    command,
                    project,
                    exposure,
                    args,
                } => {
                    let server = McpServer::Stdio(McpStdioServer {
                        command,
                        args,
                        exposure: exposure.into(),
                    });
                    if project {
                        config.add_project(&name, server)?;
                        print_stdout(format_args!(
                            "Added project MCP server {}; run `ion trust` to enable it",
                            metadata_field(&name)
                        ))?;
                    } else {
                        config.add(&name, server)?;
                        print_stdout(format_args!("Added MCP server {}", metadata_field(&name)))?;
                    }
                }
                McpAction::AddHttp {
                    name,
                    url,
                    bearer_token_env,
                    project,
                    exposure,
                } => {
                    let server = McpServer::Http(McpHttpServer {
                        url,
                        bearer_token_env,
                        exposure: exposure.into(),
                    });
                    if project {
                        config.add_project(&name, server)?;
                        print_stdout(format_args!(
                            "Added project MCP server {}; run `ion trust` to enable it",
                            metadata_field(&name)
                        ))?;
                    } else {
                        config.add(&name, server)?;
                        print_stdout(format_args!("Added MCP server {}", metadata_field(&name)))?;
                    }
                }
                McpAction::Remove { name, project } => {
                    if project {
                        config.remove_project(&name)?;
                        print_stdout(format_args!(
                            "Removed project MCP server {}",
                            metadata_field(&name)
                        ))?;
                    } else {
                        config.remove(&name)?;
                        print_stdout(format_args!("Removed MCP server {}", metadata_field(&name)))?;
                    }
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
                Some(CodingSession::inspect_metadata(&path)?)
            } else {
                None
            };
            if let Some(view) = &existing {
                ensure!(
                    !explicit_cwd || view.cwd.as_deref() == Some(cwd.as_path()),
                    "--cwd does not match the session's working directory ({})",
                    view.cwd
                        .as_deref()
                        .map_or_else(|| "(none)".into(), |cwd| cwd.display().to_string())
                );
            }
            if matches!(action, Some(Action::Inspect)) {
                ensure!(existing.is_some(), "session does not exist");
                let view = CodingSession::inspect(&path)?;
                inspection::write(&view, &mut io::stdout().lock())?;
                return Ok(());
            }
            if let Some(Action::Export { path: output }) = &action {
                ensure!(existing.is_some(), "session does not exist");
                let view = CodingSession::inspect(&path)?;
                if let Some(output) = output {
                    let target = if output.is_absolute() {
                        output.clone()
                    } else {
                        view.cwd
                            .as_deref()
                            .context("relative export path requires a working directory")?
                            .join(output)
                    };
                    transcript::save_new(&view, &target).with_context(|| {
                        format!("cannot save transcript to {}", target.display())
                    })?;
                    print_stdout(format_args!("{}", target.display()))?;
                } else {
                    io::stdout().write_all(transcript::render(&view).as_bytes())?;
                }
                return Ok(());
            }
            if matches!(action, Some(Action::Turns)) {
                ensure!(existing.is_some(), "session does not exist");
                let view = CodingSession::inspect(&path)?;
                for turn in view.turns() {
                    print_stdout(format_args!(
                        "{}\t{}\t{}",
                        turn.turn,
                        if turn.end.is_some() {
                            "ended"
                        } else {
                            "active"
                        },
                        preview_input(&turn.input)
                    ))?;
                }
                return Ok(());
            }
            if matches!(action, Some(Action::Clone)) {
                ensure!(existing.is_some(), "session does not exist");
                let source = CodingSession::open(&path)?;
                let target = catalog.new_path()?;
                let cloned = source.clone_to(&target)?;
                let id = cloned.id().to_string();
                print_stdout(format_args!("Cloned session as {}", metadata_field(&id)))?;
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
                let id = fork.id().to_string();
                print_stdout(format_args!("Forked session as {}", metadata_field(&id)))?;
                return Ok(());
            }
            if matches!(action, Some(Action::Compact)) {
                ensure!(existing.is_some(), "session does not exist");
            }
            let session_cwd = match &existing {
                Some(view) => view
                    .cwd
                    .as_deref()
                    .context("coding requires a working directory")?,
                None => cwd.as_path(),
            };
            let previous = existing.as_ref().and_then(|view| view.last_model.clone());
            let selected = models.choose(cli.provider, cli.model, previous, credentials)?;
            selected.require_access(credentials)?;
            let sources = cli
                .image
                .iter()
                .map(|path| {
                    ion_host::image_input::ImageSource::Path(if path.is_absolute() {
                        path.clone()
                    } else {
                        session_cwd.join(path)
                    })
                })
                .collect();
            // Startup has not admitted a Session operation or taken terminal
            // custody. Preserve default SIGINT here; registering Tokio's handler
            // would also change the later interactive terminal's signal behavior.
            let images = ion_host::image_input::prepare_images(
                &selected.identity(),
                selected.image_input,
                sources,
                ion_core::AgentLimits::default().max_request_bytes,
                tokio_util::sync::CancellationToken::new(),
            )
            .await?;
            let session = Arc::new(if path.is_file() {
                CodingSession::open(&path)?
            } else {
                CodingSession::create(&path, Some(&cwd))?
            });
            let startup = if matches!(action, Some(Action::Compact)) {
                ion_host::McpStartup::default()
            } else {
                host.external_tools(
                    session
                        .cwd()
                        .context("coding requires a working directory")?,
                )
                .await
            };
            let external_mcp = startup.tools;
            let mut preflight_session = Some(session);
            let external_tools: Option<Arc<dyn CodingToolSource>> = external_mcp
                .as_ref()
                .map(|tools| tools.clone() as Arc<dyn CodingToolSource>);
            // The question capability exists only for the actual terminal
            // client. Headless, RPC, compaction and managed children get no
            // consumer and therefore no declared question tool.
            let questions = (cli.print.is_none() && matches!(&action, Some(Action::Chat) | None))
                .then(ion_host::QuestionCustody::new);
            let mut application = ion_host::CodingApplication::new(host.clone(), external_tools)
                .with_code_mode(cli.code_mode);
            if cli.read_only {
                application = application.with_read_only_tools();
            } else {
                application = application.with_tool_selection(tool_selection);
            }
            let application_owner = application.owner();
            // Every fallible setup step after MCP startup belongs to the same
            // exit scope as the client, so preflight errors still join cleanup.
            let result = async {
                let mut startup_diagnostics = startup
                    .diagnostics
                    .into_iter()
                    .map(|diagnostic| format!("[mcp: {}]", metadata_field(&diagnostic)))
                    .collect::<Vec<_>>();
                let setup = match application.setup(
                    preflight_session.as_ref().expect("unbound Session").clone(),
                    selected,
                    cli.reasoning,
                    questions,
                ) {
                    Ok(setup) => setup,
                    Err(error) => {
                        // Failed-server cleanup diagnostics must not disappear
                        // just because independent binding preflight also fails.
                        for diagnostic in &startup_diagnostics {
                            if let Err(delivery) = print_stderr(format_args!("{diagnostic}")) {
                                return Err(error.context(format!(
                                    "startup diagnostic delivery also failed: {delivery:#}"
                                )));
                            }
                        }
                        return Err(error);
                    }
                };
                for diagnostic in setup.resources().diagnostics() {
                    startup_diagnostics.push(format!(
                        "[resource: {}: {}]",
                        metadata_field(&diagnostic.path.to_string_lossy()),
                        metadata_field(&diagnostic.message)
                    ));
                }
                for diagnostic in setup.commands().diagnostics() {
                    startup_diagnostics.push(format!("[commands: {}]", metadata_field(diagnostic)));
                }
                if cli.print.is_some() || !matches!(&action, Some(Action::Chat) | None) {
                    for diagnostic in &startup_diagnostics {
                        print_stderr(format_args!("{diagnostic}"))?;
                    }
                }
                enum Client {
                    Headless(headless::Request, bool),
                    Rpc,
                    Terminal(Vec<ion_ai::LoadedImage>),
                }
                // Complete fallible preflight before transferring cleanup custody.
                // Headless resource expansion precedes piped context; its prepared
                // message remains literal when admitted by shared control.
                let client = match action {
                    Some(Action::Run { prompt }) => {
                        let prompt = with_piped_input(expand_input(
                            setup.resources(),
                            prompt,
                            setup.agent().limits().max_request_bytes,
                        )?)?;
                        Client::Headless(
                            headless::Request::Prompt(Message::user_input(prompt, images)),
                            cli.json,
                        )
                    }
                    Some(Action::Compact) => Client::Headless(headless::Request::Compact, false),
                    Some(Action::Rpc) => Client::Rpc,
                    Some(Action::Chat) | None if cli.print.is_none() => Client::Terminal(images),
                    None => {
                        let prompt = with_piped_input(expand_input(
                            setup.resources(),
                            cli.print.expect("matched Some"),
                            setup.agent().limits().max_request_bytes,
                        )?)?;
                        Client::Headless(
                            headless::Request::Prompt(Message::user_input(prompt, images)),
                            cli.json,
                        )
                    }
                    _ => unreachable!("non-agent action handled above"),
                };
                // Each client immediately owns shared control and always joins it.
                // Keeping the original Arc here would pin its exclusive writer
                // lock after a client switches to a different Session.
                drop(preflight_session.take());
                match client {
                    Client::Headless(request, json) => headless::run(setup, request, json).await,
                    Client::Rpc => rpc::run(setup, host).await,
                    Client::Terminal(images) => {
                        terminal_client::chat(terminal_client::ChatInit {
                            setup,
                            services: host,
                            images,
                            startup_diagnostics,
                            tui_mode: cli.tui_mode,
                        })
                        .await
                    }
                }
            }
            .await;
            // Unconditional outer fallback, including preflight/client failure.
            // Join children and programs before releasing their captured MCP environment.
            let result = if let Some(owner) = &application_owner {
                match (result, owner.join_shutdown().await) {
                    (result, Ok(())) => result,
                    (Ok(()), Err(error)) => Err(error),
                    (Err(error), Err(cleanup)) => {
                        Err(error.context(format!("application shutdown also failed: {cleanup}")))
                    }
                }
            } else {
                result
            };
            // Preflight failures still own the original Session. After handoff,
            // the client alone owns current-session cleanup, including replacement.
            let shutdown = match preflight_session {
                Some(session) => session.shutdown().await,
                None => Ok(()),
            };
            let result = match (result, shutdown) {
                (result, Ok(())) => result,
                (Ok(()), Err(error)) => Err(error.into()),
                (Err(error), Err(shutdown)) => {
                    Err(error.context(format!("Session shutdown also failed: {shutdown}")))
                }
            };
            let cleanup = match external_mcp {
                Some(tools) => tools.shutdown().await,
                None => Ok(()),
            };
            match (result, cleanup) {
                (result, Ok(())) => result,
                (Ok(()), Err(error)) => Err(error),
                (Err(error), Err(cleanup)) => {
                    Err(error.context(format!("MCP cleanup also failed: {cleanup:#}")))
                }
            }
        }
    }
}

// Keep each lock inside synchronous publication, never across client awaits.
// A failed confirmation does not roll back an already-applied host effect.
fn print_stdout(arguments: std::fmt::Arguments<'_>) -> Result<()> {
    writeln!(io::stdout().lock(), "{arguments}").context("cannot write CLI stdout")
}

fn print_stderr(arguments: std::fmt::Arguments<'_>) -> Result<()> {
    writeln!(io::stderr().lock(), "{arguments}").context("cannot write CLI stderr")
}

fn expand_input(resources: &Resources, prompt: String, max_output_bytes: usize) -> Result<String> {
    resources
        .expand_command(&prompt, max_output_bytes)
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
        .map(metadata_character)
        .collect()
}

// Plain one-line metadata only: keep formatter delimiters outside these fields.
// Identities and machine-readable/durable data remain exact.
fn metadata_field(text: &str) -> String {
    text.chars().map(metadata_character).collect()
}

fn metadata_character(ch: char) -> char {
    if ch.is_control() { ' ' } else { ch }
}

fn status_label(status: CredentialStatus) -> &'static str {
    match status {
        CredentialStatus::Environment => "environment key",
        CredentialStatus::Saved => "saved login",
        CredentialStatus::Missing => "no credential",
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
