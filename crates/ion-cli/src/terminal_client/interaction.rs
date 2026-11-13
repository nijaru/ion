//! Translate editor actions into typed Host admission; commands remain terminal UI.
use anyhow::{Context, Result};
use ion_app::{Delivery, Host, SessionControl, UserInput};
use ion_terminal::{Screen, TerminalSession};

use super::{Builtin, EditorDraft, Frontend, PickerValue, navigation::apply_fork};

/// Physical editor/attachment leases run their own input pump; this target is
/// derived only while the conversation client owns the physical terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InputTarget {
    Details,
    Picker,
    Question,
    Composer,
}

impl Frontend {
    pub(super) fn input_target(&self) -> InputTarget {
        if self.details.is_some() {
            InputTarget::Details
        } else if self.pickers.active().is_some() {
            InputTarget::Picker
        } else if self.question.is_some() {
            InputTarget::Question
        } else {
            InputTarget::Composer
        }
    }

    pub(super) fn invalidate_interaction_paint(&mut self) {
        self.editor.completion.invalidate_visibility();
        self.pickers.invalidate_visibility();
        if let Some(question) = &mut self.question {
            question.invalidate_visibility();
        }
    }
}

pub(super) fn editor_delivery(runtime: &SessionControl<EditorDraft>) -> Delivery {
    match runtime.state().operation.map(|operation| operation.kind) {
        Some(ion_app::OperationKind::Shell | ion_app::OperationKind::Compact) => Delivery::FollowUp,
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
    if ui.session_io.blocks_input() {
        ui.restore_editor(source);
        ui.status = "Prompt retained; not sent".into();
        return;
    }
    if ui
        .unresolved_inputs
        .iter()
        .any(|input| input.session == runtime.session().id())
        || ui
            .unresolved_requests
            .iter()
            .any(|input| input.session == runtime.session().id())
        || ui
            .unresolved_recoveries
            .iter()
            .any(|failure| failure.point.session == runtime.session().id())
    {
        ui.restore_editor(source);
        ui.status =
            "Input commit unresolved; reconcile this Session before sending another prompt".into();
        return;
    }
    if delivery == Delivery::FollowUp
        && (Builtin::parse(source.literal.split_whitespace().next().unwrap_or("")).is_some()
            || source.literal.trim_start().starts_with("/command:"))
    {
        ui.restore_editor(source);
        ui.status =
            "Terminal commands cannot be queued; submit a prompt or resource command".into();
        return;
    }
    let (images, files) = ui.editor.take_attachments();
    let input = UserInput::Prompt {
        text: source.literal.clone(),
        images,
        files,
    };
    match runtime.submit(delivery, input, source) {
        Ok(_) => {
            ui.view.follow_end();
            ui.status.clear();
        }
        Err(rejected) => {
            let rejected = *rejected;
            let UserInput::Prompt { images, files, .. } = rejected.input else {
                unreachable!("terminal submits only raw prompts")
            };
            ui.restore_editor(rejected.metadata);
            ui.editor.images = images;
            ui.editor.files = files;
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
    if ui.session_io.blocks_input() {
        let (mut images, mut files) = ui.editor.take_attachments();
        let mut drafts = prompts.into_iter().map(|literal| super::RecoveredDraft {
            source: EditorDraft::at_end(literal),
            images: std::mem::take(&mut images),
            files: std::mem::take(&mut files),
        });
        if let Some(first) = drafts.next() {
            ui.show_draft(first);
        }
        ui.editor.recovered.extend(drafts);
        ui.status = "Session display is stale; launch prompts retained, not submitted".into();
        return;
    }
    let (mut images, mut files) = ui.editor.take_attachments();
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
            ui.view.follow_end();
            ui.status.clear();
        }
        Err(rejected) => {
            let ion_app::RejectedBatch { inputs, reason } = *rejected;
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
            ui.editor.recovered.extend(drafts);
            ui.status = format!("Input was not queued: {reason:#}");
        }
    }
}

pub(super) async fn apply_picker(
    runtime: &mut SessionControl<EditorDraft>,
    services: &std::sync::Arc<Host>,
    ui: &mut Frontend,
    terminal: &mut TerminalSession,
    screen: &mut Screen,
    program: &mut super::ProgramStatusState,
    value: PickerValue,
) -> Result<()> {
    let theme_result = match &value {
        PickerValue::Themes => Some(ui.open_theme_picker()),
        PickerValue::Theme(name) => Some(ui.choose_theme(name)),
        PickerValue::ThemeAction(action) => Some(ui.theme_action(*action)),
        _ => None,
    };
    if let Some(result) = theme_result {
        if let Err(error) = result {
            if ui.pickers.active().is_none() {
                ui.cancel_theme_picker();
            }
            ui.status = format!("{error:#}");
        }
        return Ok(());
    }
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
    if let PickerValue::KeepContext = value {
        ui.status = "Context unchanged; workspace files unchanged".into();
        return Ok(());
    }
    if ui.clipboard_pending() {
        ui.status = format!(
            "Wait for clipboard paste or cancel it with {}",
            ui.keys.hint(crate::keybindings::KeyAction::Interrupt)
        );
        return Ok(());
    }
    if let PickerValue::Mcp(choice) = value {
        if let Err(error) = super::mcp::apply(runtime, ui, choice) {
            ui.status = format!("{error:#}");
        }
        return Ok(());
    }
    let result = match value {
        PickerValue::Context { target, .. } => {
            if let Err(error) = super::navigation::apply_context(runtime, ui, target) {
                ui.status = format!("{error:#}");
            }
            return Ok(());
        }
        PickerValue::Fork { target, .. } => {
            return match apply_fork(runtime, ui, target) {
                Ok(()) => Ok(()),
                Err(error) => {
                    ui.status = format!("{error:#}");
                    Ok(())
                }
            };
        }
        PickerValue::Session { path, .. } => super::session_io::start_binding(
            runtime,
            ui,
            ion_app::BindingChange::SwitchSession(path),
            None,
        ),
        PickerValue::Model(model) => super::configuration::select_model(
            runtime,
            ui,
            services.clone(),
            super::configuration::ModelRequest::Identity(model),
        ),
        PickerValue::Reasoning(reasoning) => {
            if let Err(error) = super::session_io::start_binding(
                runtime,
                ui,
                ion_app::BindingChange::SelectReasoning(reasoning),
                None,
            ) {
                ui.status = format!("{error:#}");
            }
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
                &ui.termination,
            )
            .await?
            {
                Ok(()) => format!("Saved {provider} API key; environment keys take precedence"),
                Err(error) => format!("{error:#}"),
            };
            return Ok(());
        }
        PickerValue::Setting(_)
        | PickerValue::KeepContext
        | PickerValue::Task(_)
        | PickerValue::Mcp(_)
        | PickerValue::CompletionDelivery(_)
        | PickerValue::Themes
        | PickerValue::Theme(_)
        | PickerValue::ThemeAction(_) => {
            unreachable!("handled above")
        }
    };
    match result {
        Ok(()) => {}
        Err(error) => ui.status = format!("{error:#}"),
    }
    Ok(())
}
