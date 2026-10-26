//! Command origin, lexical identity and terminal-only dispatch.
//!
//! Editor commands own their exact source; shortcuts leave the editor untouched.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ion_host::SessionControl;
use ion_terminal::{Screen, TerminalSession};

use super::{
    Builtin, EditorDraft, Frontend, Picker, ProgramStatusState, TuiMode, context_label,
    copy_last_answer, edit_draft_in_terminal,
    interaction::{editor_delivery, submit_editor},
    login_in_terminal, navigation, tasks, tools,
};
use ion_host::image_input::ImageSource;

pub(super) enum CommandInput {
    Editor(EditorDraft),
    Shortcut(Builtin),
}

#[derive(Debug, PartialEq, Eq)]
enum CommandKind<'a> {
    Builtin(Builtin),
    UserCommand(&'a str),
    Other(&'a str),
}

struct ParsedCommand<'a> {
    kind: CommandKind<'a>,
    args: &'a str,
}

impl CommandInput {
    fn parsed(&self) -> ParsedCommand<'_> {
        match self {
            Self::Shortcut(builtin) => ParsedCommand {
                kind: CommandKind::Builtin(*builtin),
                args: "",
            },
            Self::Editor(source) => {
                let syntax = source.literal.trim();
                let (name, args) = syntax
                    .split_once(char::is_whitespace)
                    .unwrap_or((syntax, ""));
                let kind = if let Some(builtin) = Builtin::parse(name) {
                    CommandKind::Builtin(builtin)
                } else if let Some(name) = name.strip_prefix("/command:") {
                    CommandKind::UserCommand(name)
                } else {
                    CommandKind::Other(name)
                };
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
            Self::Shortcut(_) => None,
        }
    }
}

pub(super) async fn dispatch_command(
    runtime: &mut SessionControl<EditorDraft>,
    services: &ion_host::Host,
    ui: &mut Frontend,
    terminal: &mut TerminalSession,
    screen: &mut Screen,
    program: &mut ProgramStatusState,
    input: CommandInput,
) -> Result<()> {
    let ParsedCommand { kind, args } = input.parsed();
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
        CommandKind::Builtin(Builtin::Login) if !args.is_empty() => {
            match login_in_terminal(terminal, screen, services.credentials(), args, program).await?
            {
                Ok(()) => ui.status = "Credential saved".into(),
                // Credential entry is modal, never a recoverable model prompt.
                Err(error) => ui.status = format!("{error:#}"),
            }
        }
        CommandKind::Builtin(Builtin::Model) if !args.is_empty() => {
            let result = match services
                .models()
                .resolve_pattern(args, None, services.credentials())
            {
                Ok(selection) => {
                    super::configuration::select_model(
                        runtime, services, terminal, screen, program, selection,
                    )
                    .await?
                }
                Err(error) => Err(error),
            };
            match result {
                Ok(()) => {
                    let model = runtime.agent().model();
                    ui.status = format!("Selected {}/{}", model.provider, model.model);
                }
                Err(error) => {
                    if let Some(source) = input.into_editor() {
                        ui.restore_editor(source);
                    }
                    ui.status = format!("{error:#}");
                }
            }
        }
        CommandKind::Builtin(Builtin::Compact) if args.is_empty() => {
            let source = input
                .into_editor()
                .unwrap_or_else(|| EditorDraft::at_end(Builtin::Compact.usage()));
            match runtime.compact(source) {
                Ok(_) => ui.status.clear(),
                Err((source, error)) => {
                    ui.restore_editor(source);
                    ui.status = format!("Compaction was not started: {error:#}");
                }
            }
        }
        CommandKind::Builtin(Builtin::Copy) if args.is_empty() => {
            ui.status = match copy_last_answer(runtime.session(), terminal).await {
                Ok(crate::clipboard::CopyOutcome::Copied) => "Copied last assistant answer".into(),
                Ok(crate::clipboard::CopyOutcome::RequestedFromTerminal) => {
                    "Sent clipboard request to terminal".into()
                }
                Err(error) => crate::clipboard::failure_status(error)?,
            };
        }
        CommandKind::Builtin(Builtin::Attach) => {
            match super::attach::run(runtime, terminal, screen, args).await {
                Ok(Some(status)) => ui.status = status,
                Ok(None) => ui.status.clear(),
                Err(error) => ui.status = format!("Attachment failed: {error:#}"),
            }
        }
        CommandKind::Builtin(Builtin::Editor) if args.is_empty() => {
            ui.completion.clear();
            match edit_draft_in_terminal(terminal, screen, ui).await? {
                Ok(()) => ui.status = "Draft returned from editor".into(),
                Err(error) => {
                    ui.status = format!("Editor failed: {error:#}; original draft retained")
                }
            }
        }
        CommandKind::UserCommand(name) => {
            let argv = match ion_host::parse_command_args(args) {
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
            if let Err(error) = handle_builtin(runtime, services, ui, builtin, args) {
                if error.is::<navigation::CommittedContextRefresh>() {
                    return Err(error);
                }
                if let Some(source) = input.into_editor() {
                    ui.restore_editor(source);
                }
                ui.status = format!("{error:#}");
            }
        }
        CommandKind::Other(name) => {
            // Recognition only, after foreground guards: Host expands once when
            // it admits the original Prompt, including during an operation.
            let resource = runtime
                .resources()
                .skills()
                .any(|skill| name.strip_prefix("/skill:") == Some(skill.name.as_str()))
                || runtime
                    .resources()
                    .templates()
                    .any(|template| name.strip_prefix('/') == Some(template.name.as_str()));
            if resource {
                let delivery = editor_delivery(runtime);
                submit_editor(
                    runtime,
                    ui,
                    delivery,
                    input
                        .into_editor()
                        .expect("resource command originates in editor"),
                );
            } else {
                ui.status = format!("Unknown command: {name}. Type /help");
                ui.restore_editor(
                    input
                        .into_editor()
                        .expect("unknown command originates in editor"),
                );
            }
        }
    }
    Ok(())
}

pub(super) fn handle_builtin(
    runtime: &mut SessionControl<EditorDraft>,
    services: &ion_host::Host,
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
                ui.pickers.open(Picker::reasoning(runtime)?);
            } else {
                let reasoning = args
                    .parse::<ion_ai::Reasoning>()
                    .map_err(anyhow::Error::msg)?;
                runtime.select_reasoning(reasoning)?;
                ui.status = format!("Generation effort: {reasoning}");
            }
        }
        Builtin::Tui => match args {
            "" => ui.print(format!(
                "TUI mode: {}. Use /tui inline or /tui fullscreen",
                ui.mode.label()
            )),
            "inline" | "regular" => {
                ui.mode = TuiMode::Inline;
                ui.scroll = 0;
                ui.status = "Inline TUI · native terminal scrollback".into();
            }
            "fullscreen" => {
                ui.mode = TuiMode::Fullscreen;
                ui.scroll = 0;
                ui.fullscreen_rows = 0;
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
            ion_host::image_input::require_image_input(
                runtime.agent().model(),
                runtime.agent().limits().image_input,
            )?;
            ui.attach_image(ImageSource::Path(PathBuf::from(args)));
        }
        Builtin::Skills => ui.print(
            runtime
                .resources()
                .skills()
                .map(|skill| format!("{} — {}", skill.name, skill.description))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        Builtin::Prompts => ui.print(
            runtime
                .resources()
                .templates()
                .map(|template| format!("/{} — {}", template.name, template.description))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        Builtin::Reload => {
            let keys = crate::keybindings::KeyBindings::load(services.config_root())?;
            runtime.reload_resources()?;
            ui.break_edit_focus();
            ui.keys = keys;
            ui.status.clear();
            ui.print(format!(
                "Reloaded keybindings, resources and commands ({} diagnostic(s))",
                runtime.resources().diagnostics().len() + runtime.commands().diagnostics().len()
            ));
        }
        Builtin::Session => {
            let view = runtime.session().view()?;
            ui.print(format!(
                "Session {} · {} turn(s) · {} · {}\n{}",
                runtime.session().path().display(),
                view.entries
                    .iter()
                    .filter(|entry| matches!(entry, ion_core::SessionEntry::TurnStarted { .. }))
                    .count(),
                view.name.as_deref().unwrap_or("unnamed"),
                context_label(&view, runtime.agent().limits().context_window_tokens),
                crate::session_usage::render(&view.usage()),
            ));
        }
        Builtin::Export => {
            anyhow::ensure!(!args.is_empty(), "use /export PATH");
            let target = Path::new(args);
            let target = if target.is_absolute() {
                target.to_owned()
            } else {
                runtime
                    .session()
                    .cwd()
                    .context("relative export path requires a working directory")?
                    .join(target)
            };
            crate::transcript::save_new(&runtime.session().view()?, &target)?;
            ui.status = format!("Transcript saved to {}", target.display());
        }
        Builtin::New => {
            runtime.new_session()?;
            ui.refresh_session(runtime.session())?;
            ui.print("Started a new session".into());
        }
        Builtin::Clone => {
            let id = runtime.clone_session()?;
            ui.refresh_session(runtime.session())?;
            ui.print(format!(
                "Cloned conversation as {id}; both sessions use the same working directory"
            ));
        }
        Builtin::Tree => navigation::tree(runtime, ui, args)?,
        Builtin::Fork => navigation::fork(runtime, ui, args)?,
        Builtin::ForkAfter => navigation::fork_after(runtime, ui, args)?,
        Builtin::Resume => navigation::resume(runtime, ui, args)?,
        Builtin::Name => {
            if args.is_empty() {
                ui.print(
                    runtime
                        .session()
                        .view()?
                        .name
                        .unwrap_or_else(|| "Session has no name".into()),
                );
            } else {
                runtime.session().set_name(Some(args))?;
                ui.refresh_session(runtime.session())?;
            }
        }
        Builtin::Model if args.is_empty() => {
            ui.pickers
                .open(Picker::models(services, Some(runtime.agent().model()))?);
        }
        Builtin::Login if args.is_empty() => ui.pickers.open(Picker::providers(services)?),
        Builtin::Logout => {
            anyhow::ensure!(!args.is_empty(), "use /logout PROVIDER");
            services.credentials().remove(args)?;
            ui.status = format!("Removed saved {args} credential");
        }
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
                tools(&ui.history).count()
            } else {
                args.parse::<usize>().context("use /tool [N]")?
            };
            ui.open_details(Some(number));
        }
        kind => anyhow::bail!("use {}", kind.usage()),
    }
    Ok(())
}
