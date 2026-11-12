//! Clipboard acquisition and publication into the editor. Host prepares images.
use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use ion_app::image_input::LoadedImage;
use ion_core::Agent;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use super::Frontend;

pub(super) enum PreparedPaste {
    Files(Vec<PathBuf>),
    Image(LoadedImage),
    Text(String),
}

pub(super) struct ClipboardJob {
    pub(super) stop: CancellationToken,
    pub(super) task: tokio::task::JoinHandle<Result<Option<PreparedPaste>>>,
}

impl Frontend {
    pub(super) fn clipboard_cancel_target(&self) -> Option<tokio::task::Id> {
        self.clipboard_job
            .as_ref()
            .filter(|_| self.input_target() == super::interaction::InputTarget::Composer)
            .map(|job| job.task.id())
    }
}

pub(super) fn start_clipboard_paste(ui: &mut Frontend, agent: &Arc<Agent>) {
    if ui.clipboard_job.is_some() {
        ui.status = "Clipboard acquisition is already in progress".into();
        return;
    }
    let agent = agent.clone();
    let stop = CancellationToken::new();
    let reader_stop = stop.clone();
    ui.clipboard_job = Some(ClipboardJob {
        stop,
        task: tokio::spawn(async move {
            let content = crate::clipboard_reader::read(&reader_stop).await?;
            content
                .map(|content| {
                    prepare_clipboard(agent.model(), agent.limits().image_input, content)
                })
                .transpose()
        }),
    });
    ui.status = "Reading clipboard…".into();
}

#[cfg(test)]
pub(super) async fn finish_clipboard_paste(ui: &mut Frontend) -> Result<()> {
    let joined = (&mut ui
        .clipboard_job
        .as_mut()
        .context("no clipboard paste is pending")?
        .task)
        .await;
    finish_joined_clipboard_paste(ui, joined)
}

pub(super) fn finish_joined_clipboard_paste(
    ui: &mut Frontend,
    joined: std::result::Result<Result<Option<PreparedPaste>>, tokio::task::JoinError>,
) -> Result<()> {
    let job = ui
        .clipboard_job
        .take()
        .context("no clipboard paste is pending")?;
    let content = joined.context("clipboard reader stopped")??;
    let Some(content) = content.filter(|_| !job.stop.is_cancelled()) else {
        ui.status = "Paste cancelled".into();
        return Ok(());
    };
    if matches!(
        ui.status.as_str(),
        "Reading clipboard…" | "Wait for clipboard paste, then send the prompt"
    ) {
        ui.status.clear();
    }
    apply_clipboard(ui, content)
}

pub(super) async fn cancel_clipboard_paste(ui: &mut Frontend) -> Result<()> {
    if let Some(job) = ui.clipboard_job.take() {
        job.stop.cancel();
        // A refusal/cancellation is recoverable; a failed join is not. Do not
        // publish a late successful paste into a draft whose client is closing.
        let _discarded = job
            .task
            .await
            .context("clipboard reader stopped during exit")?;
    }
    Ok(())
}

pub(super) fn prepare_clipboard(
    model: &ion_ai::ModelRef,
    image_input: bool,
    content: crate::clipboard_reader::PasteContent,
) -> Result<PreparedPaste> {
    match content {
        crate::clipboard_reader::PasteContent::Files(paths) => Ok(PreparedPaste::Files(paths)),
        crate::clipboard_reader::PasteContent::Image { content, note } => {
            ion_app::image_input::require_image_input(model, image_input)?;
            Ok(PreparedPaste::Image(LoadedImage { content, note }))
        }
        crate::clipboard_reader::PasteContent::Text(text) => Ok(PreparedPaste::Text(text)),
    }
}

pub(super) fn apply_clipboard(ui: &mut Frontend, content: PreparedPaste) -> Result<()> {
    let text = match content {
        PreparedPaste::Files(paths) => {
            let shell = ui.editor.draft.trim_start().starts_with('!');
            let text = clipboard_paths(&paths, shell)?;
            let before = ui.editor.draft[..ui.editor.cursor].chars().next_back();
            let after = ui.editor.draft[ui.editor.cursor..].chars().next();
            let prefix = before.filter(|ch| !ch.is_whitespace()).map_or("", |_| " ");
            let suffix = after.filter(|ch| !ch.is_whitespace()).map_or("", |_| " ");
            format!("{prefix}{text}{suffix}")
        }
        PreparedPaste::Image(image) => {
            ui.attach_image(image.into());
            return Ok(());
        }
        PreparedPaste::Text(text) => text,
    };
    // This read was started by the composer. A later picker must not capture
    // the asynchronous result as though it were fresh input to its search box.
    ui.gestures.invalidate();
    ui.editor.insert(&text);
    Ok(())
}

pub(super) fn clipboard_paths(paths: &[PathBuf], shell: bool) -> Result<String> {
    let mut formatted = Vec::with_capacity(paths.len());
    for path in paths {
        let path = path.to_str().context("clipboard path is not UTF-8")?;
        ensure!(
            !path.chars().any(char::is_control),
            "clipboard path contains control characters"
        );
        formatted.push(if shell {
            shlex::try_quote(path)
                .context("clipboard path cannot be shell quoted")?
                .into_owned()
        } else {
            path.to_owned()
        });
    }
    Ok(formatted.join(if shell { " " } else { "\n" }))
}
