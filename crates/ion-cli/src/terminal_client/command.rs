//! Command origin, lexical identity and terminal-only dispatch.
//!
//! Editor commands own their exact source; shortcuts leave the editor untouched.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ion_app::{ModelCycleDirection, SessionControl};
use ion_terminal::{Screen, TerminalSession};

use super::{
    Builtin, EditorDraft, Frontend, Picker, ProgramStatusState, TuiMode, context_label,
    edit_text_in_terminal,
    interaction::{editor_delivery, submit_editor},
    login_in_terminal, navigation, tasks, tools,
};
use ion_app::image_input::ImageSource;

pub(super) enum CommandInput {
    Editor(EditorDraft),
    Shortcut(Builtin),
    CycleModel(ModelCycleDirection),
}

#[derive(Debug, PartialEq, Eq)]
enum CommandKind<'a> {
    Builtin(Builtin),
    CycleModel(ModelCycleDirection),
    UserCommand(&'a str),
}

struct ParsedCommand<'a> {
    kind: Option<CommandKind<'a>>,
    args: &'a str,
}

impl CommandInput {
    fn parsed(&self) -> ParsedCommand<'_> {
        match self {
            Self::Shortcut(builtin) => ParsedCommand {
                kind: Some(CommandKind::Builtin(*builtin)),
                args: "",
            },
            Self::CycleModel(direction) => ParsedCommand {
                kind: Some(CommandKind::CycleModel(*direction)),
                args: "",
            },
            Self::Editor(source) => {
                let syntax = source.literal.trim();
                let (name, args) = syntax
                    .split_once(char::is_whitespace)
                    .unwrap_or((syntax, ""));
                let kind = Builtin::parse(name)
                    .map(CommandKind::Builtin)
                    .or_else(|| name.strip_prefix("/command:").map(CommandKind::UserCommand));
                ParsedCommand {
                    kind,
                    args: args.trim(),
                }
            }
        }
    }

    fn into_editor(self) -> Option<EditorDraft> {
        match self {
            Self::Editor(source) => Some(source),
            Self::Shortcut(_) | Self::CycleModel(_) => None,
        }
    }
}

pub(super) async fn dispatch_command(
    runtime: &mut SessionControl<EditorDraft>,
    services: &std::sync::Arc<ion_app::Host>,
    ui: &mut Frontend,
    terminal: &mut TerminalSession,
    screen: &mut Screen,
    program: &mut ProgramStatusState,
    input: CommandInput,
) -> Result<()> {
    let ParsedCommand { kind, args } = input.parsed();
    let Some(kind) = kind else {
        // Only recognized commands consume slash input. Host owns resource
        // expansion; every other literal uses ordinary prompt admission.
        let delivery = editor_delivery(runtime);
        submit_editor(
            runtime,
            ui,
            delivery,
            input.into_editor().expect("prompt originates in editor"),
        );
        return Ok(());
    };
    if ui.clipboard_job.is_some()
        && !matches!(
            kind,
            CommandKind::Builtin(
                Builtin::Help | Builtin::Tui | Builtin::Settings | Builtin::Copy | Builtin::Tasks
            )
        )
    {
        if let Some(source) = input.into_editor() {
            ui.restore_editor(source);
        }
        ui.status = format!(
            "Wait for clipboard paste or cancel it with {}",
            ui.keys.hint(crate::keybindings::KeyAction::Interrupt)
        );
        return Ok(());
    }
    if (ui.session_io.has_read() || ui.session_io.binding.is_some())
        && !(kind == CommandKind::Builtin(Builtin::Editor) && !ui.session_io.has_pending_effect())
        && !matches!(
            kind,
            CommandKind::Builtin(Builtin::Help | Builtin::Tui | Builtin::Tasks)
        )
    {
        if let Some(source) = input.into_editor() {
            ui.restore_editor(source);
        }
        ui.status = "Wait for Session inspection or binding; interrupt remains available".into();
        return Ok(());
    }
    if ui.session_io.blocks_input()
        && matches!(
            kind,
            CommandKind::UserCommand(_) | CommandKind::Builtin(Builtin::Compact)
        )
    {
        if let Some(source) = input.into_editor() {
            ui.restore_editor(source);
        }
        ui.status = "Command retained; not started".into();
        return Ok(());
    }
    // Modal external I/O cannot monopolize foreground controls while work runs.
    // Host itself enforces drained replacement for model/session operations.
    if runtime.is_busy()
        && (matches!(
            kind,
            CommandKind::Builtin(Builtin::Login | Builtin::Logout | Builtin::Editor)
        ) || (kind == CommandKind::Builtin(Builtin::Name) && !args.is_empty()))
    {
        if let Some(source) = input.into_editor() {
            ui.restore_editor(source);
        }
        ui.status = "This action is available after the operation".into();
        return Ok(());
    }
    match kind {
        CommandKind::CycleModel(direction) => {
            if let Err(error) = super::configuration::select_model(
                runtime,
                ui,
                services.clone(),
                super::configuration::ModelRequest::Cycle(direction),
            ) {
                ui.status = format!("{error:#}");
            }
        }
        CommandKind::Builtin(Builtin::Login) if !args.is_empty() => {
            match login_in_terminal(terminal, screen, services.credentials(), args, program).await?
            {
                Ok(()) => ui.status = "Credential saved".into(),
                // Credential entry is modal, never a recoverable model prompt.
                Err(error) => ui.status = format!("{error:#}"),
            }
        }
        CommandKind::Builtin(Builtin::Model) if !args.is_empty() => {
            let result = super::configuration::select_model(
                runtime,
                ui,
                services.clone(),
                super::configuration::ModelRequest::Pattern(args.to_owned()),
            );
            let source = input.into_editor();
            match result {
                Ok(()) => ui.session_io.retain_source(source),
                Err(error) => {
                    if let Some(source) = source {
                        ui.restore_editor(source);
                    }
                    ui.status = format!("{error:#}");
                }
            }
        }
        CommandKind::Builtin(Builtin::Mcp) => {
            let args = args.to_owned();
            let source = input
                .into_editor()
                .unwrap_or_else(|| EditorDraft::at_end("/mcp".into()));
            super::mcp::command(runtime, ui, &args, source);
        }
        CommandKind::Builtin(Builtin::Compact) => {
            let instructions = (!args.is_empty()).then(|| args.to_owned());
            let source = input
                .into_editor()
                .unwrap_or_else(|| EditorDraft::at_end("/compact".into()));
            match runtime.compact(instructions, source) {
                Ok(_) => ui.status.clear(),
                Err((source, error)) => {
                    ui.restore_editor(source);
                    ui.status = format!("Compaction was not started: {error:#}");
                }
            }
        }
        CommandKind::Builtin(Builtin::Copy) if args.is_empty() => {
            let reader = runtime.session().reader()?;
            let result = ui.session_io.start(runtime.session(), async move {
                Ok(super::session_io::ReadOutput::Copy(
                    reader
                        .map_view(|view| super::last_committed_answer(&view))
                        .await??,
                ))
            });
            match result {
                Ok(()) => ui.session_io.retain_source(input.into_editor()),
                Err(error) => {
                    if let Some(source) = input.into_editor() {
                        ui.restore_editor(source);
                    }
                    ui.status = format!("Copy failed: {error:#}");
                }
            }
        }
        CommandKind::Builtin(Builtin::Attach) => {
            match super::attach::run(runtime, terminal, screen, args).await {
                Ok(Some(status)) => ui.status = status,
                Ok(None) => ui.status.clear(),
                Err(error) => ui.status = format!("Attachment failed: {error:#}"),
            }
        }
        CommandKind::Builtin(Builtin::Editor) if args.is_empty() => {
            ui.editor.completion.clear();
            match edit_text_in_terminal(terminal, screen, &ui.editor.draft, super::MAX_DRAFT)
                .await?
            {
                Ok(text) => {
                    ui.replace_draft_atomic(text);
                    ui.status = "Draft returned from editor".into();
                }
                Err(error) => {
                    ui.status = format!("Editor failed: {error:#}; original draft retained")
                }
            }
        }
        CommandKind::UserCommand(name) => {
            let argv = match ion_app::parse_command_args(args) {
                Ok(argv) => argv,
                Err(error) => {
                    ui.restore_editor(
                        input
                            .into_editor()
                            .expect("user command originates in editor"),
                    );
                    ui.status = format!("Command was not started: {error:#}");
                    return Ok(());
                }
            };
            // Host retains the original draft independently of the command identity.
            let name = name.to_owned();
            let draft = input
                .into_editor()
                .expect("user command originates in editor");
            match runtime.command(&name, &argv, draft) {
                Ok(_) => ui.status.clear(),
                Err((draft, error)) => {
                    ui.restore_editor(draft);
                    ui.status = format!("Command was not started: {error:#}");
                }
            }
        }
        CommandKind::Builtin(builtin) => {
            match handle_builtin(runtime, services, ui, builtin, args) {
                Ok(()) => {
                    if matches!(
                        builtin,
                        Builtin::Reasoning
                            | Builtin::Reload
                            | Builtin::Session
                            | Builtin::Export
                            | Builtin::New
                            | Builtin::Clone
                            | Builtin::Tree
                            | Builtin::Fork
                            | Builtin::ForkAfter
                            | Builtin::Resume
                            | Builtin::Name
                            | Builtin::Model
                            | Builtin::Login
                            | Builtin::Logout
                    ) {
                        ui.session_io.retain_source(input.into_editor());
                    }
                }
                Err(error) => {
                    if let Some(source) = input.into_editor() {
                        ui.restore_editor(source);
                    }
                    ui.status = format!("{error:#}");
                }
            }
        }
    }
    Ok(())
}

pub(super) fn handle_builtin(
    runtime: &mut SessionControl<EditorDraft>,
    services: &std::sync::Arc<ion_app::Host>,
    ui: &mut Frontend,
    builtin: Builtin,
    args: &str,
) -> Result<()> {
    match builtin {
        Builtin::Help => {
            let help = if args.is_empty() {
                Builtin::help(&ui.keys)
            } else {
                Builtin::help_for(args, &ui.keys)
                    .context("unknown help topic; use /help or type / to discover commands")?
            };
            ui.print(help);
        }
        Builtin::Settings => {
            if let Some(policy) = args.strip_prefix("completion ") {
                let policy = policy.parse().map_err(anyhow::Error::msg)?;
                runtime.set_completion_delivery(policy)?;
                ui.status = format!(
                    "Managed-work completion: {policy} · prior grants revoked; reports remain inspectable"
                );
            } else {
                ui.output_settings(args, runtime.completion_delivery())?;
            }
        }
        Builtin::Reasoning => {
            if args.is_empty() {
                let reader = runtime.session().reader()?;
                let agent = runtime.agent().clone();
                ui.session_io.start(runtime.session(), async move {
                    Ok(super::session_io::ReadOutput::Picker(Box::new(
                        reader
                            .map_view(move |view| Picker::reasoning_for(&agent, view.reasoning))
                            .await??,
                    )))
                })?;
            } else {
                let reasoning = args
                    .parse::<ion_ai::Reasoning>()
                    .map_err(anyhow::Error::msg)?;
                super::session_io::start_binding(
                    runtime,
                    ui,
                    ion_app::BindingChange::SelectReasoning(reasoning),
                    None,
                )?;
            }
        }
        Builtin::Tui => match args {
            "" => ui.print(format!(
                "TUI mode: {}. Use /tui inline or /tui fullscreen",
                ui.mode.label()
            )),
            "inline" | "regular" => {
                ui.mode = TuiMode::Inline;
                ui.view.follow_end();
                ui.status = "Inline TUI · native terminal scrollback".into();
            }
            "fullscreen" => {
                ui.mode = TuiMode::Fullscreen;
                ui.view.follow_end();
                ui.status = format!(
                    "Fullscreen TUI · {} / {} or mouse wheel scrolls",
                    ui.keys.label(crate::keybindings::KeyAction::PageUp),
                    ui.keys.label(crate::keybindings::KeyAction::PageDown)
                );
            }
            _ => anyhow::bail!("use /tui inline or /tui fullscreen"),
        },
        Builtin::Image => {
            anyhow::ensure!(!args.is_empty(), "use /image PATH");
            ion_app::image_input::require_image_input(
                runtime.agent().model(),
                runtime.agent().limits().image_input,
            )?;
            ui.attach_image(ImageSource::Path(PathBuf::from(args)));
        }
        Builtin::Skills => ui.print(super::resource_commands::skills(
            runtime.resources(),
            &ui.cwd,
        )),
        Builtin::Prompts => ui.print(super::resource_commands::prompts(
            runtime.resources(),
            &ui.cwd,
        )),
        Builtin::Reload => {
            anyhow::ensure!(
                !ui.theme_io.has_action(),
                "Wait for the requested theme operation to finish"
            );
            ui.dismiss_theme_picker();
            let root = services.config_root().to_owned();
            let themes = ui.themes.clone();
            let io = runtime.session().io()?;
            ui.session_io
                .start_presentation_reload(runtime.session(), async move {
                    let (keys, themes) = io
                        .run(move |_| {
                            Ok((|| -> Result<_> {
                                Ok((
                                    crate::keybindings::KeyBindings::load(&root)?,
                                    themes.reloaded()?,
                                ))
                            })())
                        })
                        .await??;
                    Ok(super::session_io::ReadOutput::Reload(Box::new((
                        keys, themes,
                    ))))
                })?;
        }
        Builtin::Session => {
            let reader = runtime.session().reader()?;
            let path = runtime.session().path().to_owned();
            let window = runtime.agent().limits().context_window_tokens;
            ui.session_io.start(runtime.session(), async move {
                Ok(reader
                    .map_view(move |view| {
                        super::session_io::ReadOutput::SessionInfo(format!(
                            "Session {} · {} turn(s) · {} · {}\n{}",
                            path.display(),
                            view.entries
                                .iter()
                                .filter(|entry| matches!(
                                    entry,
                                    ion_core::SessionEntry::TurnStarted { .. }
                                ))
                                .count(),
                            view.name.as_deref().unwrap_or("unnamed"),
                            context_label(&view, window),
                            crate::session_usage::render(&view.usage()),
                        ))
                    })
                    .await?)
            })?;
        }
        Builtin::Export => {
            let requested = if args.is_empty() {
                format!("ion-session-{}.html", uuid::Uuid::now_v7())
            } else if args.starts_with(['"', '\'']) {
                let mut paths = shlex::split(args).context("export path has unclosed quotes")?;
                anyhow::ensure!(paths.len() == 1, "use /export [PATH]; quote a single path");
                paths.remove(0)
            } else {
                args.to_owned()
            };
            let target = Path::new(&requested);
            let target = if target.is_absolute() {
                target.to_owned()
            } else {
                runtime
                    .session()
                    .cwd()
                    .context("relative export path requires a working directory")?
                    .join(target)
            };
            let io = runtime.session().io()?;
            ui.session_io
                .start_file_write(runtime.session(), async move {
                    Ok(io
                        .run(move |session| {
                            let view = session.view()?;
                            Ok(crate::transcript::save_new(&view, &target)
                                .map(|()| super::session_io::ReadOutput::Exported(target)))
                        })
                        .await??)
                })?;
        }
        Builtin::New => {
            super::session_io::start_binding(
                runtime,
                ui,
                ion_app::BindingChange::NewSession,
                None,
            )?;
        }
        Builtin::Clone => {
            super::session_io::start_binding(
                runtime,
                ui,
                ion_app::BindingChange::CloneSession,
                None,
            )?;
        }
        Builtin::Tree => navigation::tree(runtime, ui, args)?,
        Builtin::Fork => navigation::fork(runtime, ui, args)?,
        Builtin::ForkAfter => navigation::fork_after(runtime, ui, args)?,
        Builtin::Resume => navigation::resume(runtime, ui, args)?,
        Builtin::Name => {
            if args.is_empty() {
                let reader = runtime.session().reader()?;
                ui.session_io.start(runtime.session(), async move {
                    Ok(reader
                        .map_view(|view| {
                            super::session_io::ReadOutput::Notice(
                                view.name.unwrap_or_else(|| "Session has no name".into()),
                            )
                        })
                        .await?)
                })?;
            } else {
                super::session_io::start_binding(
                    runtime,
                    ui,
                    ion_app::BindingChange::SetName(Some(args.to_owned())),
                    None,
                )?;
            }
        }
        Builtin::Model if args.is_empty() => {
            let services = services.clone();
            let current = runtime.agent().model().clone();
            let io = runtime.session().io()?;
            ui.session_io.start(runtime.session(), async move {
                let choices = io
                    .run(move |_| Ok(services.models().selection_choices(services.credentials())))
                    .await??;
                Ok(super::session_io::ReadOutput::Models { choices, current })
            })?;
        }
        Builtin::Login if args.is_empty() => {
            let services = services.clone();
            let io = runtime.session().io()?;
            ui.session_io.start(runtime.session(), async move {
                Ok(super::session_io::ReadOutput::Picker(Box::new(
                    io.run(move |_| Ok(Picker::providers(&services))).await??,
                )))
            })?;
        }
        Builtin::Logout => {
            anyhow::ensure!(!args.is_empty(), "use /logout PROVIDER");
            let services = services.clone();
            let provider = args.to_owned();
            let io = runtime.session().io()?;
            ui.session_io
                .start_file_write(runtime.session(), async move {
                    io.run(move |_| {
                        Ok((|| -> Result<_> {
                            services.credentials().remove(&provider)?;
                            Ok(super::session_io::ReadOutput::Notice(format!(
                                "Removed saved {provider} credential"
                            )))
                        })())
                    })
                    .await?
                })?;
        }
        Builtin::Mcp => anyhow::bail!("use /mcp [reconnect [SERVER]]"),
        Builtin::Tasks => {
            let owner = runtime
                .background()
                .context("application has no background owner")?;
            tasks::command(ui, &owner, args)?;
        }
        Builtin::Commands => {
            if !args.is_empty() {
                anyhow::bail!("use /commands");
            }
            let catalog = runtime.commands();
            let mut lines = Vec::new();
            for command in catalog.iter() {
                lines.push(format!(
                    "/command:{} — {} · {}",
                    command.name, command.description, command.origin
                ));
            }
            for diagnostic in catalog.diagnostics() {
                lines.push(format!("[commands: {diagnostic}]"));
            }
            ui.print(if lines.is_empty() {
                "No enabled user commands. Configure commands.json or a trusted project .ion/commands.json".into()
            } else {
                lines.join("\n")
            });
        }
        Builtin::Tools => ui.list_tools(),
        Builtin::Tool => {
            let number = if args.is_empty() {
                tools(ui.conversation.items()).count()
            } else {
                args.parse::<usize>().context("use /tool [N]")?
            };
            ui.open_details(Some(number));
        }
        kind => anyhow::bail!("use {}", kind.usage()),
    }
    Ok(())
}
