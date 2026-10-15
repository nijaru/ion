//! Translate editor actions into typed Host admission; commands remain terminal UI.
use anyhow::Result;
use ion_host::{Delivery, SessionControl, UserInput};
use ion_terminal::{Screen, TerminalSession};

use super::{
    Builtin, EditorDraft, ForkPoint, Frontend, PickerValue, apply_fork, copy_last_answer,
    edit_draft_in_terminal, handle_command, login_in_terminal,
};

pub(super) fn editor_delivery(runtime: &SessionControl<EditorDraft>) -> Delivery {
    match runtime.state().operation.map(|operation| operation.kind) {
        Some(ion_host::OperationKind::Shell | ion_host::OperationKind::Compact) => {
            Delivery::FollowUp
        }
        _ => runtime.current_operation().map_or_else(
            || {
                if runtime.is_busy() {
                    Delivery::FollowUp
                } else {
                    Delivery::Start
                }
            },
            Delivery::Steer,
        ),
    }
}

pub(super) fn submit_editor(
    runtime: &mut SessionControl<EditorDraft>,
    ui: &mut Frontend,
    delivery: Delivery,
    source: EditorDraft,
) {
    if delivery == Delivery::FollowUp
        && Builtin::parse(source.literal.split_whitespace().next().unwrap_or("")).is_some()
    {
        ui.restore_editor(source);
        ui.status =
            "Terminal commands cannot be queued; submit a prompt or resource command".into();
        return;
    }
    let input = UserInput::Prompt {
        text: source.literal.clone(),
        images: std::mem::take(&mut ui.images),
    };
    match runtime.submit(delivery, input, source) {
        Ok(_) => {
            ui.scroll = 0;
            ui.status.clear();
        }
        Err(rejected) => {
            let rejected = *rejected;
            let UserInput::Prompt { images, .. } = rejected.input else {
                unreachable!("terminal submits only raw prompts")
            };
            ui.restore_editor(rejected.metadata);
            ui.images = images;
            ui.status = format!("Input was not queued: {:#}", rejected.reason);
        }
    }
}

pub(super) async fn dispatch_command(
    runtime: &mut SessionControl<EditorDraft>,
    ui: &mut Frontend,
    terminal: &mut TerminalSession,
    screen: &mut Screen,
    command: String,
    source: Option<EditorDraft>,
) -> Result<()> {
    let (name, args) = command
        .split_once(char::is_whitespace)
        .unwrap_or((&command, ""));
    let args = args.trim();
    let builtin = Builtin::parse(name);
    if ui.clipboard_job.is_some()
        && !matches!(
            builtin,
            Some(Builtin::Help | Builtin::Tui | Builtin::Settings | Builtin::Copy)
        )
    {
        if let Some(source) = source {
            ui.restore_editor(source);
        }
        ui.status = "Wait for clipboard paste or cancel it with Ctrl-C".into();
        return Ok(());
    }
    // Modal external I/O cannot monopolize foreground controls while work runs.
    // Host itself enforces drained replacement for model/session operations.
    if runtime.is_busy()
        && (matches!(
            builtin,
            Some(Builtin::Login | Builtin::Logout | Builtin::Editor)
        ) || (builtin == Some(Builtin::Name) && !args.is_empty()))
    {
        if let Some(source) = source {
            ui.restore_editor(source);
        }
        ui.status = "This action is available after the operation".into();
        return Ok(());
    }
    if builtin == Some(Builtin::Login) && !args.is_empty() {
        match login_in_terminal(terminal, screen, runtime.host().credentials(), args).await? {
            Ok(()) => ui.status = "Credential saved".into(),
            // Credential entry is modal, never a recoverable model prompt.
            Err(error) => ui.status = format!("{error:#}"),
        }
    } else if builtin == Some(Builtin::Compact) && args.is_empty() {
        let source = source.unwrap_or_else(|| EditorDraft::at_end(command));
        match runtime.compact(source) {
            Ok(_) => {
                ui.status.clear();
            }
            Err((source, error)) => {
                ui.restore_editor(source);
                ui.status = format!("Compaction was not started: {error:#}");
            }
        }
    } else if builtin == Some(Builtin::Copy) && args.is_empty() {
        ui.status = match copy_last_answer(runtime.session(), terminal).await {
            Ok(crate::clipboard::CopyOutcome::Copied) => "Copied last assistant answer".into(),
            Ok(crate::clipboard::CopyOutcome::RequestedFromTerminal) => {
                "Sent clipboard request to terminal".into()
            }
            Err(error) => {
                if error.chain().any(|cause| cause.is::<std::io::Error>()) {
                    return Err(error.context("terminal clipboard output failed"));
                }
                format!("Copy failed: {error:#}")
            }
        };
    } else if builtin == Some(Builtin::Editor) && args.is_empty() {
        ui.completion.clear();
        match edit_draft_in_terminal(terminal, screen, ui).await? {
            Ok(()) => ui.status = "Draft returned from editor".into(),
            Err(error) => ui.status = format!("Editor failed: {error:#}; original draft retained"),
        }
    } else {
        match handle_command(runtime, ui, &command) {
            Ok(true) => {
                let delivery = editor_delivery(runtime);
                submit_editor(
                    runtime,
                    ui,
                    delivery,
                    source.unwrap_or_else(|| EditorDraft::at_end(command)),
                );
            }
            Ok(false) => {}
            Err(error) => {
                if let Some(source) = source {
                    ui.restore_editor(source);
                }
                ui.status = format!("{error:#}");
            }
        }
    }
    Ok(())
}

pub(super) fn apply_picker(
    runtime: &mut SessionControl<EditorDraft>,
    ui: &mut Frontend,
    value: PickerValue,
) -> Result<()> {
    if let PickerValue::File { path, start, end } = value {
        ui.insert_file(path, start, end);
        return Ok(());
    }
    if ui.clipboard_job.is_some() {
        ui.status = "Wait for clipboard paste or cancel it with Ctrl-C".into();
        return Ok(());
    }
    let result = match value {
        PickerValue::ForkBefore { turn, input } => {
            return match apply_fork(runtime, ui, ForkPoint::BeforeTurn(turn), Some(input)) {
                Ok(()) => Ok(()),
                Err(error) => {
                    ui.status = format!("{error:#}");
                    Ok(())
                }
            };
        }
        PickerValue::Session(path) => runtime.switch_session(path),
        PickerValue::Model(model) => runtime.select_model(model),
        PickerValue::File { .. } => unreachable!("handled above"),
    };
    match result {
        Ok(()) => {
            ui.refresh_session(runtime.session())?;
            ui.status = "Ready".into();
        }
        Err(error) => ui.status = format!("{error:#}"),
    }
    Ok(())
}
