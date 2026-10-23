//! Translate editor actions into typed Host admission; commands remain terminal UI.
use anyhow::Result;
use ion_host::{Delivery, SessionControl, UserInput};

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
