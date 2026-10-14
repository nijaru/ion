//! Terminal controls while a borrowed operation retains execution custody.
use std::{future::Future, sync::Mutex};

use anyhow::Result;
use ion_core::{LiveTranscript, SteeringInbox};
use ion_terminal::{InputEvent, KeyCode, KeyEvent, Modifiers, MouseKind, Screen, TerminalSession};
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;

use super::input_preparation::{finish_ready_clipboard_paste, start_clipboard_paste};
use super::{
    ActiveOperation, EditorDraft, Frontend, busy_key, draw, draw_progress, is_clipboard_shortcut,
};

pub(super) enum Activity<'a> {
    Coding {
        progress: &'a Mutex<LiveTranscript>,
        steering: &'a SteeringInbox<EditorDraft>,
    },
    Shell,
    Compaction,
}

impl Activity<'_> {
    fn view<'a>(&self, stop: &'a CancellationToken) -> ActiveOperation<'a> {
        match self {
            Self::Coding { .. } => ActiveOperation::Coding(stop),
            Self::Shell => ActiveOperation::Shell(stop),
            Self::Compaction => ActiveOperation::Compaction(stop),
        }
    }

    fn steering(&self) -> Option<&SteeringInbox<EditorDraft>> {
        match self {
            Self::Coding { steering, .. } => Some(steering),
            Self::Shell | Self::Compaction => None,
        }
    }

    async fn draw(
        &self,
        terminal: &mut TerminalSession,
        screen: &mut Screen,
        ui: &mut Frontend,
        stop: &CancellationToken,
    ) -> Result<()> {
        match self {
            Self::Coding { progress, .. } => {
                draw_progress(terminal, screen, ui, progress, self.view(stop)).await
            }
            Self::Shell | Self::Compaction => {
                draw(terminal, screen, ui, None, Some(self.view(stop))).await
            }
        }
    }
}

/// Never return on terminal failure before polling the operation to settlement.
/// Admission and recovery stay with the operation-specific caller.
pub(super) async fn pump<T, E>(
    terminal: &mut TerminalSession,
    screen: &mut Screen,
    ui: &mut Frontend,
    runtime: &ion_host::SessionBinding,
    stop: &CancellationToken,
    activity: Activity<'_>,
    operation: impl Future<Output = Result<T, E>>,
) -> (Result<T>, Result<()>)
where
    E: Into<anyhow::Error>,
{
    tokio::pin!(operation);
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    let mut terminal_error = None;
    let result = loop {
        if terminal_error.is_none()
            && let Err(error) = terminal.check_active()
        {
            terminal_error = Some(error.into());
        }
        if terminal_error.is_some() {
            stop.cancel();
            ui.pickers.close();
        }
        tokio::select! {
            result = &mut operation => break result,
            notice = ui.pickers.complete(), if ui.pickers.has_job() => {
                if let Some(notice) = notice { ui.note(notice); }
                if terminal_error.is_none() {
                    terminal_error = activity.draw(terminal, screen, ui, stop).await.err();
                }
            },
            event = terminal.next_input(), if terminal_error.is_none() => {
                if let Err(error) = terminal.check_active() {
                    terminal_error = Some(error.into());
                    continue;
                }
                match event {
                    Some(Ok(InputEvent::Key(KeyEvent { code: KeyCode::Char('c'), modifiers }))) if modifiers.contains(Modifiers::CONTROL) => stop.cancel(),
                    Some(Ok(InputEvent::Key(key))) if is_clipboard_shortcut(key) && ui.pickers.active().is_none() && ui.details.is_none() => {
                        start_clipboard_paste(ui, runtime.selected());
                    },
                    Some(Ok(InputEvent::Key(key))) => busy_key(ui, key, stop, activity.steering(), Some(runtime.resources()), runtime.agent().limits()),
                    Some(Ok(InputEvent::Paste(text))) => ui.insert(&text),
                    Some(Ok(InputEvent::Rejected(error))) => ui.status = error.to_string(),
                    Some(Ok(InputEvent::Resize(_))) => screen.note_resize(),
                    Some(Ok(InputEvent::Mouse(mouse))) => match mouse.kind() {
                        MouseKind::ScrollUp => ui.scroll = ui.scroll.saturating_add(3),
                        MouseKind::ScrollDown => ui.scroll = ui.scroll.saturating_sub(3),
                        _ => {},
                    },
                    Some(Err(error)) => {
                        terminal_error = Some(anyhow::Error::new(error).context("terminal input failed"));
                    },
                    None => {
                        terminal_error = Some(anyhow::anyhow!("terminal input ended during operation"));
                    },
                }
            },
            _ = tick.tick(), if terminal_error.is_none() => {
                finish_ready_clipboard_paste(ui).await;
                terminal_error = activity.draw(terminal, screen, ui, stop).await.err();
            },
        }
    };
    finish_ready_clipboard_paste(ui).await;
    let result = result.map_err(Into::into);
    let terminal = terminal_result(terminal_error, &result);
    (result, terminal)
}

fn terminal_result<T>(error: Option<anyhow::Error>, settlement: &Result<T>) -> Result<()> {
    let Some(error) = error else {
        return Ok(());
    };
    let error = error.context("terminal interaction failed after operation settlement");
    // A broken terminal must remain fatal, but cannot hide a failed observation
    // commit or other settlement failure behind the presentation error.
    Err(match settlement {
        Ok(_) => error,
        Err(settlement) => {
            error.context(format!("operation settlement also failed: {settlement:#}"))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_failure_preserves_settlement_failure() {
        let settlement: Result<()> = Err(anyhow::anyhow!("observation commit failed"));
        let error =
            terminal_result(Some(anyhow::anyhow!("output disconnected")), &settlement).unwrap_err();
        let diagnostic = format!("{error:#}");
        assert!(diagnostic.contains("observation commit failed"));
        assert!(diagnostic.contains("output disconnected"));
        assert!(terminal_result(None, &settlement).is_ok());
    }
}
