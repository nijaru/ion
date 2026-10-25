//! Interactive model setup before Session, tool or MCP admission.
use anyhow::{Context, Result};
use ion_host::{Host, SavedSelection, Selection};
use ion_terminal::{InputEvent, KeyCode, Modifiers, Screen, TerminalSession};

use super::{
    Frontend, Picker, PickerValue, ProgramStatusState, draw, login_in_terminal, new_inline_screen,
};

/// Returns None on cancellation. Ready launches never acquire a terminal here.
pub async fn setup_model(host: &Host, selected: Option<Selection>) -> Result<Option<Selection>> {
    let selected = match selected {
        Some(selection) if !selection.credential_missing(host.credentials())? => {
            return Ok(Some(selection));
        }
        selected => selected,
    };
    let mut terminal = TerminalSession::enter().await?;
    let outcome = async {
        let mut screen = new_inline_screen(&mut terminal).await?;
        run(host, selected, &mut terminal, &mut screen).await
    }
    .await;
    // This UI only paints transient picker rows on the alternate screen. Secret
    // entry owns its primary-screen prompt/newline and quarantines its suffix.
    match (outcome, terminal.restore()) {
        (Ok(selected), Ok(())) => Ok(selected),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error.into()),
        (Err(error), Err(cleanup)) => {
            Err(error.context(format!("Terminal restoration also failed: {cleanup}")))
        }
    }
}

async fn run(
    host: &Host,
    mut selected: Option<Selection>,
    terminal: &mut TerminalSession,
    screen: &mut Screen,
) -> Result<Option<Selection>> {
    let mut ui = Frontend {
        status: "Select a model · type to filter · Enter selects · Esc exits".into(),
        ..Frontend::default()
    };
    let mut program = ProgramStatusState::default();
    let mut save_default = false;
    loop {
        if let Some(mut selection) = selected.take() {
            let login = if selection.credential_missing(host.credentials())? {
                login_in_terminal(
                    terminal,
                    screen,
                    host.credentials(),
                    &selection.provider,
                    &mut program,
                )
                .await?
            } else {
                Ok(())
            };
            match login {
                Ok(()) => {
                    selection.require_access(host.credentials())?;
                    if save_default {
                        selection = host.models().save_default(&SavedSelection {
                            provider: selection.provider,
                            model: selection.model,
                            endpoint: None,
                            wire: None,
                            api_key_env: None,
                            context_window_tokens: None,
                            max_output_tokens: None,
                            image_input: false,
                        })?;
                        selection.require_access(host.credentials())?;
                    }
                    return Ok(Some(selection));
                }
                Err(error) => {
                    ui.status = format!("{error:#} · choose a model to retry or Esc to exit");
                }
            }
        }
        if ui.pickers.active().is_none() {
            let mut picker = Picker::models(host, None)?;
            picker.title = "Choose default model";
            ui.pickers.open(picker);
        }
        program.publish(terminal, false)?;
        draw(terminal, screen, &mut ui, None, None, None).await?;
        let event = terminal
            .next_input()
            .await
            .context("model setup input ended")??;
        match event {
            InputEvent::Key(key)
                if key.modifiers.contains(Modifiers::CONTROL) && key.code == KeyCode::Char('z') =>
            {
                terminal.suspend_to_job_control().await?;
                screen.invalidate();
                program.published = None;
            }
            InputEvent::Key(key) => {
                if let Some(PickerValue::Model(model)) = ui.pickers.key(key) {
                    selected = Some(host.models().resolve_identity(&model)?);
                    save_default = true;
                } else if ui.pickers.active().is_none() {
                    return Ok(None);
                }
            }
            InputEvent::Paste(text) => {
                ui.pickers.insert(&text);
            }
            InputEvent::Rejected(error) => ui.status = error.to_string(),
            InputEvent::Resize(_) => screen.note_resize(),
            InputEvent::Mouse(_) => {}
        }
    }
}
