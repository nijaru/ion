//! Startup selection before Session, tool or MCP admission.
use crate::keybindings::{KeyAction, KeyBindings};
use anyhow::{Context, Result};
use ion_host::{Host, SavedSelection, Selection, SessionCatalog};
use ion_terminal::{InputEvent, Screen, TerminalSession};
use std::path::PathBuf;

use super::{
    Frontend, PickerValue, ProgramStatusState, draw, invalidate_resized_input, login_in_terminal,
    new_inline_screen, release_surface,
};

/// Returns None on cancellation. Ready launches never acquire a terminal here.
pub async fn setup_model(
    host: &Host,
    selected: Option<Selection>,
    choose_default: bool,
    keys: &KeyBindings,
    themes: &crate::themes::Themes,
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
        let outcome = run(
            host,
            selected,
            choose_default,
            keys,
            themes,
            &mut terminal,
            &mut screen,
        )
        .await;
        finish(outcome, release_surface(&mut terminal, &mut screen).await)
    }
    .await;
    // Secret entry owns its primary prompt/newline and quarantines its suffix.
    finish(
        outcome,
        terminal.restore().context("restore startup terminal"),
    )
}

/// Browse without allocating a Session, choosing a model or admitting tools.
/// Cancellation returns None; discovery is joined even after terminal failure.
pub async fn choose_session(
    catalog: SessionCatalog,
    keys: &KeyBindings,
    themes: &crate::themes::Themes,
) -> Result<Option<PathBuf>> {
    let mut terminal = TerminalSession::enter().await?;
    let mut ui = Frontend {
        keys: keys.clone(),
        themes: themes.clone(),
        status: "Choose a Session · type to filter".into(),
        ..Frontend::default()
    };
    ui.pickers
        .open_sessions(catalog, super::picker::SessionBrowser::Launch);
    let outcome = async {
        let mut screen = new_inline_screen(&mut terminal).await?;
        let outcome = async {
        let mut program = ProgramStatusState::default();
        loop {
            program.publish(&mut terminal, false)?;
            let painted = draw(&mut terminal, &mut screen, &mut ui, None, None).await?;
            let event = tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_millis(50)), if !painted => continue,
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
            invalidate_resized_input(&terminal, &mut screen, &mut ui)?;
            match event.context("Session picker input ended")?? {
                InputEvent::Key(key) if keys.matches(KeyAction::Suspend, key) => {
                    terminal.suspend_to_job_control().await?;
                    screen.invalidate();
                    program.published = None;
                }
                InputEvent::Key(key) => {
                    if let Some(PickerValue::Session { path, .. }) = ui.pickers.key(key, keys) {
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
                InputEvent::Mouse(_) | InputEvent::ColorsChanged => {}
            }
        }
        }.await;
        finish(outcome, release_surface(&mut terminal, &mut screen).await)
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
    keys: &KeyBindings,
    themes: &crate::themes::Themes,
    terminal: &mut TerminalSession,
    screen: &mut Screen,
) -> Result<Option<Selection>> {
    let mut ui = Frontend {
        keys: keys.clone(),
        themes: themes.clone(),
        status: "Select a model · type to filter".into(),
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
                    ui.status = format!("{error:#} · choose a model to retry");
                }
            }
        }
        if ui.pickers.active().is_none() {
            ui.pickers.open_models(
                host,
                None,
                if choose_default {
                    "Choose default model"
                } else {
                    "Choose Session model"
                },
            )?;
        }
        program.publish(terminal, false)?;
        let painted = draw(terminal, screen, &mut ui, None, None).await?;
        let event = tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(50)), if !painted => continue,
            event = terminal.next_input() => event.context("model setup input ended")??,
        };
        terminal
            .check_active()
            .context("model setup terminal failed")?;
        invalidate_resized_input(terminal, screen, &mut ui)?;
        match event {
            InputEvent::Key(key) if keys.matches(KeyAction::Suspend, key) => {
                terminal.suspend_to_job_control().await?;
                screen.invalidate();
                program.published = None;
            }
            InputEvent::Key(key) => {
                if let Some(PickerValue::Model(model)) = ui.pickers.key(key, keys) {
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
            InputEvent::Mouse(_) | InputEvent::ColorsChanged => {}
        }
    }
}
