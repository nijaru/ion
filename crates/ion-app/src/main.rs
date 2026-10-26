//! Local CLI and terminal host for the same coding Agent and Session.
use std::{
    collections::BTreeSet,
    io::{self, IsTerminal, Write},
    path::PathBuf,
    sync::Arc,
};

use anyhow::{Context, Result, bail, ensure};
use ion_ai::{Content, Message};
use ion_core::{CodingSession, CodingToolSource, ForkPoint};
use ion_host::{
    Application, CredentialStatus, Host, McpHttpServer, McpServer, McpStdioServer, Resources,
    SavedSelection,
};

mod agent_events;
mod cli;
mod clipboard;
mod clipboard_reader;
mod display_text;
mod edit_diff;
mod external_editor;
mod headless;
mod inspection;
mod keybindings;
mod markdown;
mod presentation_style;
mod publication;
mod rpc;
mod session_usage;
mod stdio;
mod terminal_client;
mod terminal_commands;
mod terminal_completion;
mod tool_output;
mod transcript;
mod transcript_detail;
mod transcript_render;

use cli::{Action, CommandsAction, Invocation, McpAction};

fn configured_cwd(cwd: Option<PathBuf>) -> Result<PathBuf> {
    let path = cwd.unwrap_or(std::env::current_dir()?);
    let path = path
        .canonicalize()
        .with_context(|| format!("cannot resolve working directory {}", path.display()))?;
    ensure!(path.is_dir(), "working directory is not a directory");
    Ok(path)
}

#[tokio::main]
async fn main() {
    let result = match cli::parse() {
        Ok(invocation) => run_cli(invocation).await,
        Err(error) => Err(error),
    };
    if let Err(error) = result {
        // Reporting to a broken stderr must not replace the original failure
        // with a printing panic after controlled cleanup has completed.
        // Errors can quote repository metadata before trust is granted.
        let diagnostic = display_text::diagnostic(&format!("{error:#}"));
        let _ = writeln!(io::stderr().lock(), "ion: {diagnostic}");
        std::process::exit(1);
    }
}

async fn run_cli(cli: Invocation) -> Result<()> {
    if matches!(cli.action, Action::ClipboardRead) {
        return clipboard_reader::write_native();
    }
    let tool_selection = cli.tool_selection;
    if matches!(cli.action, Action::SyntaxLicenses) {
        let mut notices = String::from("# Bundled syntax definitions\n\n");
        for license in two_face::acknowledgement::listing().for_syntaxes() {
            license.write_md(&mut notices);
        }
        return Ok(io::stdout().lock().write_all(notices.as_bytes())?);
    }
    let host = Arc::new(Host::from_environment()?);
    let credentials = host.credentials();
    let models = host.models();
    match cli.action {
        Action::ClipboardRead | Action::SyntaxLicenses => {
            unreachable!("standalone command handled before Host setup")
        }
        Action::Models { query } => {
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
        Action::Use {
            provider,
            model,
            endpoint,
            wire,
            api_key_env,
            context_window,
            max_output_tokens,
            images,
        } => {
            let saved = SavedSelection {
                provider,
                model,
                endpoint,
                wire,
                api_key_env,
                context_window_tokens: context_window,
                max_output_tokens,
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
        Action::Login { provider } => {
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
            let key = key?;
            credentials.save_api_key(&provider, &key)?;
            print_stdout(format_args!(
                "Saved {} credential.",
                metadata_field(&provider)
            ))?;
            Ok(())
        }
        Action::Logout { provider } => {
            credentials.remove(&provider)?;
            print_stdout(format_args!(
                "Removed saved {} credential. Any environment key remains active.",
                metadata_field(&provider)
            ))?;
            Ok(())
        }
        Action::Auth => {
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
        Action::Sessions => {
            let cwd = cli.cwd.unwrap_or(std::env::current_dir()?).canonicalize()?;
            let catalog = host.sessions(Some(cwd));
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
        Action::Resources => {
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
        Action::Commands { action } => {
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
        Action::Trust => {
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
        Action::Untrust => {
            let cwd = configured_cwd(cli.cwd.clone())?;
            let config = host.mcp_config_for(&cwd)?;
            config.untrust_project()?;
            print_stdout(format_args!("Removed project MCP trust"))?;
            Ok(())
        }
        Action::Mcp { action } => {
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
            let keys = if cli.resume_session
                || (matches!(action, Action::Chat { .. })
                    && io::stdin().is_terminal()
                    && io::stdout().is_terminal())
            {
                keybindings::KeyBindings::load(host.config_root())?
            } else {
                keybindings::KeyBindings::default()
            };
            let explicit_cwd = cli.cwd.is_some();
            let cwd = cli.cwd.unwrap_or(std::env::current_dir()?).canonicalize()?;
            ensure!(cwd.is_dir(), "working directory is not a directory");
            let catalog = host.sessions(Some(cwd.clone()));
            let path = if cli.resume_session {
                let Some(path) = terminal_client::choose_session(catalog.clone(), &keys).await?
                else {
                    return Ok(());
                };
                path
            } else if let Some(explicit) = cli.session {
                catalog.resolve_explicit(explicit)?
            } else if cli.continue_session {
                catalog.latest()?
            } else if matches!(
                action,
                Action::Inspect
                    | Action::Usage
                    | Action::Export { .. }
                    | Action::Turns
                    | Action::Compact
                    | Action::Clone
                    | Action::Fork { .. }
            ) {
                bail!(
                    "use --continue, --resume in a terminal, or --session ID; run `ion sessions` to find one"
                )
            } else {
                catalog.new_path()?
            };
            let existing = if cli.resume_session || cli.continue_session || path.is_file() {
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
            // Copies and navigation belong to the stored Session's namespace,
            // not the directory that happened to launch this executable.
            let catalog = existing
                .as_ref()
                .map_or(catalog, |view| host.sessions(view.cwd.clone()));
            if matches!(action, Action::Inspect) {
                ensure!(existing.is_some(), "session does not exist");
                let view = CodingSession::inspect(&path)?;
                inspection::write(&view, &mut io::stdout().lock())?;
                return Ok(());
            }
            if matches!(action, Action::Usage) {
                ensure!(existing.is_some(), "session does not exist");
                let view = CodingSession::inspect(&path)?;
                print_stdout(format_args!(
                    "Session {}\n{}",
                    view.id,
                    session_usage::render(&view.usage())
                ))?;
                return Ok(());
            }
            if let Action::Export { path: output } = &action {
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
            if matches!(action, Action::Turns) {
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
            if matches!(action, Action::Clone) {
                ensure!(existing.is_some(), "session does not exist");
                let source = CodingSession::open(&path)?;
                let target = catalog.new_path()?;
                let cloned = source.clone_to(&target)?;
                let id = cloned.id().to_string();
                print_stdout(format_args!("Cloned session as {}", metadata_field(&id)))?;
                return Ok(());
            }
            if let Action::Fork { turn, after } = &action {
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
            if matches!(action, Action::Compact) {
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
            let interactive_setup = matches!(action, Action::Chat { .. })
                && io::stdin().is_terminal()
                && io::stdout().is_terminal();
            let selection = models.choose(cli.model, previous, credentials);
            let selected = if interactive_setup {
                let selected = match selection {
                    Ok(selected) => Some(selected),
                    Err(error) if error.is::<ion_host::ModelSelectionRequired>() => None,
                    Err(error) => return Err(error),
                };
                let choose_default = existing.is_none() && selected.is_none();
                let Some(selected) =
                    terminal_client::setup_model(&host, selected, choose_default, &keys).await?
                else {
                    return Ok(());
                };
                selected
            } else {
                selection?
            };
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
            let session = Arc::new(if existing.is_some() {
                CodingSession::open(&path)?
            } else {
                CodingSession::create(&path, Some(&cwd))?
            });
            if let Some(view) = &existing {
                ensure!(
                    session.id() == view.id && session.cwd() == view.cwd.as_deref(),
                    "saved Session identity or working directory changed during startup; reopen it explicitly"
                );
            }
            let startup = if matches!(action, Action::Compact) {
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
            // Terminal and bidirectional RPC own a live question responder.
            // One-shot headless, compaction and managed children have no answer
            // channel and therefore no declared question tool.
            let questions = matches!(&action, Action::Chat { .. } | Action::Rpc)
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
                if !matches!(&action, Action::Chat { .. }) {
                    for diagnostic in &startup_diagnostics {
                        print_stderr(format_args!("{diagnostic}"))?;
                    }
                }
                enum Client {
                    Headless(headless::Request, bool),
                    Rpc,
                    Terminal {
                        prompt: Option<String>,
                        images: Vec<ion_ai::LoadedImage>,
                    },
                }
                // Complete fallible preflight before transferring cleanup custody.
                // Headless resource expansion precedes piped context; its prepared
                // message remains literal when admitted by shared control.
                let client = match action {
                    Action::Run { prompt } => {
                        let prompt = cli::with_piped_input(
                            expand_input(
                                setup.resources(),
                                prompt.unwrap_or_default(),
                                setup.agent().limits().max_request_bytes,
                            )?,
                            cli.piped_input,
                        );
                        Client::Headless(
                            headless::Request::Prompt(Message::user_input(prompt, images)),
                            cli.json,
                        )
                    }
                    Action::Compact => Client::Headless(headless::Request::Compact, false),
                    Action::Rpc => Client::Rpc,
                    Action::Chat { prompt } => Client::Terminal { prompt, images },
                    _ => unreachable!("non-agent action handled above"),
                };
                // Each client immediately owns shared control and always joins it.
                // Keeping the original Arc here would pin its exclusive writer
                // lock after a client switches to a different Session.
                drop(preflight_session.take());
                let setup = setup.with_completion_delivery(cli.completion_delivery);
                match client {
                    Client::Headless(request, json) => headless::run(setup, request, json).await,
                    Client::Rpc => rpc::run(setup, host).await,
                    Client::Terminal { prompt, images } => {
                        terminal_client::chat(terminal_client::ChatInit {
                            setup,
                            services: host,
                            images,
                            startup_diagnostics,
                            initial_prompt: prompt,
                            tui_mode: cli.tui_mode,
                            keys,
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
            Content::ToolResult(result) => Some(result.name.as_str()),
            Content::ToolCall(call) => Some(call.name.as_str()),
            _ => None,
        })
        .unwrap_or(
            if input
                .content
                .iter()
                .any(|part| matches!(part, Content::Image(_)))
            {
                "[image]"
            } else {
                "[no text]"
            },
        );
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
