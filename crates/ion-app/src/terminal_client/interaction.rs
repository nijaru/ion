//! Translate editor actions into typed Host admission; commands remain terminal UI.
use anyhow::{Context, Result};
use ion_host::{Delivery, Host, SessionControl, UserInput};
use ion_terminal::{Screen, TerminalSession};

use super::{Builtin, EditorDraft, Frontend, PickerValue, navigation::apply_fork};

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
        && (Builtin::parse(source.literal.split_whitespace().next().unwrap_or("")).is_some()
            || source.literal.trim_start().starts_with("/command:"))
    {
        ui.restore_editor(source);
        ui.status =
            "Terminal commands cannot be queued; submit a prompt or resource command".into();
        return;
    }
    let input = UserInput::Prompt {
        text: source.literal.clone(),
        images: std::mem::take(&mut ui.images),
        files: std::mem::take(&mut ui.files),
    };
    match runtime.submit(delivery, input, source) {
        Ok(_) => {
            ui.scroll = 0;
            ui.status.clear();
        }
        Err(rejected) => {
            let rejected = *rejected;
            let UserInput::Prompt { images, files, .. } = rejected.input else {
                unreachable!("terminal submits only raw prompts")
            };
            ui.restore_editor(rejected.metadata);
            ui.images = images;
            ui.files = files;
            ui.status = format!("Input was not queued: {:#}", rejected.reason);
        }
    }
}

pub(super) fn submit_initial_prompts(
    runtime: &mut SessionControl<EditorDraft>,
    ui: &mut Frontend,
    prompts: Vec<String>,
) {
    if prompts.is_empty() {
        return;
    }
    let mut images = std::mem::take(&mut ui.images);
    let mut files = std::mem::take(&mut ui.files);
    let inputs = prompts
        .into_iter()
        .map(|literal| {
            let source = EditorDraft::at_end(literal);
            let input = UserInput::Prompt {
                text: source.literal.clone(),
                images: std::mem::take(&mut images),
                files: std::mem::take(&mut files),
            };
            (input, source)
        })
        .collect();
    match runtime.submit_batch(inputs) {
        Ok(_) => {
            ui.scroll = 0;
            ui.status.clear();
        }
        Err(rejected) => {
            let ion_host::RejectedBatch { inputs, reason } = *rejected;
            let mut drafts = inputs.into_iter().map(|(input, source)| {
                let UserInput::Prompt { images, files, .. } = input else {
                    unreachable!("terminal launch submits raw prompts")
                };
                super::RecoveredDraft {
                    source,
                    images,
                    files,
                }
            });
            if let Some(first) = drafts.next() {
                ui.show_draft(first);
            }
            ui.recovered.extend(drafts);
            ui.status = format!("Input was not queued: {reason:#}");
        }
    }
}

pub(super) async fn apply_picker(
    runtime: &mut SessionControl<EditorDraft>,
    services: &Host,
    ui: &mut Frontend,
    terminal: &mut TerminalSession,
    screen: &mut Screen,
    program: &mut super::ProgramStatusState,
    value: PickerValue,
) -> Result<()> {
    if let PickerValue::CompletionDelivery(policy) = value {
        match runtime.set_completion_delivery(policy) {
            Ok(()) => {
                ui.status = format!(
                    "Managed-work completion: {policy} · prior grants revoked; reports remain inspectable"
                );
                ui.open_settings(runtime.completion_delivery());
            }
            Err(error) => ui.status = format!("{error:#}"),
        }
        return Ok(());
    }
    if let PickerValue::Setting(setting) = value {
        ui.apply_setting(setting);
        ui.open_settings(runtime.completion_delivery());
        return Ok(());
    }
    if let PickerValue::Task(choice) = value {
        let result = runtime
            .background()
            .context("application has no background owner")
            .and_then(|owner| super::tasks::apply(ui, &owner, choice));
        if let Err(error) = result {
            ui.status = format!("{error:#}");
        }
        return Ok(());
    }
    if ui.clipboard_job.is_some() {
        ui.status = format!(
            "Wait for clipboard paste or cancel it with {}",
            ui.keys.hint(crate::keybindings::KeyAction::Interrupt)
        );
        return Ok(());
    }
    let result = match value {
        PickerValue::Context(value) => {
            if let Err(error) = super::navigation::apply_context(runtime, ui, value) {
                if error.is::<super::navigation::CommittedContextRefresh>() {
                    return Err(error);
                }
                ui.status = format!("{error:#}");
            }
            return Ok(());
        }
        PickerValue::Fork(value) => {
            return match apply_fork(runtime, ui, value) {
                Ok(()) => Ok(()),
                Err(error) => {
                    ui.status = format!("{error:#}");
                    Ok(())
                }
            };
        }
        PickerValue::Session(path) => runtime.switch_session(path),
        PickerValue::Model(model) => match services.models().resolve_identity(&model) {
            Ok(selection) => {
                super::configuration::select_model(
                    runtime, services, terminal, screen, program, selection,
                )
                .await?
            }
            Err(error) => Err(error),
        },
        PickerValue::Reasoning(reasoning) => {
            let result = runtime.select_reasoning(reasoning);
            ui.status = match result {
                Ok(()) => format!("Generation effort: {reasoning}"),
                Err(error) => format!("{error:#}"),
            };
            return Ok(());
        }
        PickerValue::Login(provider) => {
            if runtime.is_busy() {
                ui.status = "This action is available after the operation".into();
                return Ok(());
            }
            ui.status = match super::login_in_terminal(
                terminal,
                screen,
                services.credentials(),
                &provider,
                program,
            )
            .await?
            {
                Ok(()) => format!("Saved {provider} API key; environment keys take precedence"),
                Err(error) => format!("{error:#}"),
            };
            return Ok(());
        }
        PickerValue::Setting(_) | PickerValue::Task(_) | PickerValue::CompletionDelivery(_) => {
            unreachable!("handled above")
        }
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
