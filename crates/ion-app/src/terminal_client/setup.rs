//! Startup selection before Session, tool or MCP admission.
use anyhow::{Context, Result};
use ion_host::{Host, SavedSelection, Selection, SessionCatalog};
use ion_terminal::{InputEvent, KeyCode, Modifiers, Screen, TerminalSession};
use std::path::PathBuf;

use super::{
    Frontend, Picker, PickerValue, ProgramStatusState, draw, login_in_terminal, new_inline_screen,
};

/// Returns None on cancellation. Ready launches never acquire a terminal here.
pub async fn setup_model(
    host: &Host,
    selected: Option<Selection>,
    choose_default: bool,
) -> Result<Option<Selection>> {
    let selected = match selected {
        Some(selection) if !selection.credential_missing(host.credentials())? => {
            return Ok(Some(selection));
        }
        selected => selected,
    };
    let mut terminal = TerminalSession::enter().await?;
    let outcome = async {
        let mut screen = new_inline_screen(&mut terminal).await?;
        run(host, selected, choose_default, &mut terminal, &mut screen).await
    }
    .await;
    // This UI only paints transient picker rows on the alternate screen. Secret
    // entry owns its primary-screen prompt/newline and quarantines its suffix.
    finish(
        outcome,
        terminal.restore().context("restore startup terminal"),
    )
}

/// Browse without allocating a Session, choosing a model or admitting tools.
/// Escape/Ctrl-C returns None; discovery is joined even after terminal failure.
pub async fn choose_session(catalog: SessionCatalog) -> Result<Option<PathBuf>> {
    let mut terminal = TerminalSession::enter().await?;
    let mut ui = Frontend {
        status: "Choose a Session · type to filter · Enter resumes · Esc exits".into(),
        ..Frontend::default()
    };
    ui.pickers
        .open_sessions(catalog, super::picker::SessionBrowser::Launch);
    let outcome = async {
        let mut screen = new_inline_screen(&mut terminal).await?;
        let mut program = ProgramStatusState::default();
        loop {
            program.publish(&mut terminal, false)?;
            draw(&mut terminal, &mut screen, &mut ui, None, None, None).await?;
            let event = tokio::select! {
                notice = ui.pickers.complete(), if ui.pickers.has_job() => {
                    // Current-scope notices already render in the picker. A
                    // cancelled worker's failure must still be visible while
                    // its replacement is loading, without duplicating notices.
                    if ui.pickers.loading() && let Some(notice) = notice {
                        ui.status = notice;
                    }
                    continue;
                },
                event = terminal.next_input() => event,
            };
            terminal
                .check_active()
                .context("Session picker terminal failed")?;
            match event.context("Session picker input ended")?? {
                InputEvent::Key(key)
                    if key.modifiers.contains(Modifiers::CONTROL)
                        && key.code == KeyCode::Char('z') =>
                {
                    terminal.suspend_to_job_control().await?;
                    screen.invalidate();
                    program.published = None;
                }
                InputEvent::Key(key) => {
                    if let Some(PickerValue::Session(path)) = ui.pickers.key(key) {
                        return Ok(Some(path));
                    }
                    if ui.pickers.active().is_none() {
                        return Ok(None);
                    }
                }
                InputEvent::Paste(text) => {
                    ui.pickers.insert(&text);
                }
                InputEvent::Rejected(error) => ui.status = error.to_string(),
                InputEvent::Resize(_) => {
                    ui.pickers.invalidate_visibility();
                    screen.note_resize();
                }
                InputEvent::Mouse(_) => {}
            }
        }
    }
    .await;
    let outcome = finish(
        outcome,
        ui.pickers
            .shutdown()
            .await
            .context("join Session discovery"),
    );
    finish(
        outcome,
        terminal.restore().context("restore startup terminal"),
    )
}

fn finish<T>(outcome: Result<T>, cleanup: Result<()>) -> Result<T> {
    match (outcome, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => {
            Err(error.context(format!("Startup cleanup also failed: {cleanup:#}")))
        }
    }
}

async fn run(
    host: &Host,
    mut selected: Option<Selection>,
    choose_default: bool,
    terminal: &mut TerminalSession,
    screen: &mut Screen,
) -> Result<Option<Selection>> {
    let mut ui = Frontend {
        status: "Select a model · type to filter · Enter selects · Esc exits".into(),
        ..Frontend::default()
    };
    let mut program = ProgramStatusState::default();
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
                    if choose_default {
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
            picker.title = if choose_default {
                "Choose default model"
            } else {
                "Choose Session model"
            };
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
