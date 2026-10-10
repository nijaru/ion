//! Owned input jobs and publication into the editable draft; no terminal I/O.
use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use ion_host::{Selection, image_input::LoadedImage};
use tokio_util::sync::CancellationToken;

use super::Frontend;

pub(super) enum PreparedPaste {
    Files(Vec<PathBuf>),
    Image(LoadedImage),
    Text(String),
}

pub(super) struct ClipboardJob {
    pub(super) stop: CancellationToken,
    pub(super) task: tokio::task::JoinHandle<Result<PreparedPaste>>,
}

pub(super) struct ImageJob {
    pub(super) stop: CancellationToken,
    pub(super) task: tokio::task::JoinHandle<Result<Vec<LoadedImage>>>,
}

pub(super) fn start_image_preparation(
    runtime: &ion_host::SessionBinding,
    ui: &mut Frontend,
    path: PathBuf,
) -> Result<()> {
    ensure!(
        ui.image_job.is_none() && ui.clipboard_job.is_none(),
        "input preparation is already in progress"
    );
    ion_host::image_input::require_image_input(runtime.selected())?;
    let stop = CancellationToken::new();
    let preparation = runtime.prepare_images(
        vec![ion_host::image_input::ImageSource::Path(path)],
        stop.clone(),
    );
    ui.image_job = Some(ImageJob {
        stop,
        task: tokio::spawn(preparation),
    });
    ui.status = "Preparing image…".into();
    Ok(())
}

pub(super) fn finish_image_preparation(
    ui: &mut Frontend,
    joined: std::result::Result<Result<Vec<LoadedImage>>, tokio::task::JoinError>,
) {
    let job = ui.image_job.take().expect("joined image preparation");
    let result = joined
        .context("image preparation stopped")
        .and_then(|result| result);
    if job.stop.is_cancelled() && result.is_ok() {
        ui.status = "Image preparation cancelled".into();
    } else {
        match result {
            Ok(images) => {
                if let Err(error) = ui.attach_images(images) {
                    ui.status = format!("Image preparation failed: {error:#}");
                }
            }
            Err(error) => ui.status = format!("Image preparation failed: {error:#}"),
        }
    }
}

pub(super) async fn cancel_image_preparation(ui: &mut Frontend) -> Result<()> {
    if let Some(job) = ui.image_job.take() {
        job.stop.cancel();
        let _discarded = job
            .task
            .await
            .context("image preparation stopped during exit")?;
    }
    Ok(())
}

pub(super) fn start_clipboard_paste(ui: &mut Frontend, selected: &Selection) {
    if ui.clipboard_job.is_some() || ui.image_job.is_some() {
        ui.status = "Input preparation is already in progress".into();
        return;
    }
    let selected = selected.clone();
    let stop = CancellationToken::new();
    let reader_stop = stop.clone();
    ui.clipboard_job = Some(ClipboardJob {
        stop,
        task: tokio::spawn(async move {
            let content = crate::clipboard_reader::read(&reader_stop).await?;
            prepare_clipboard(&selected, content)
        }),
    });
    ui.status = "Reading clipboard…".into();
}

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
    joined: std::result::Result<Result<PreparedPaste>, tokio::task::JoinError>,
) -> Result<()> {
    let job = ui
        .clipboard_job
        .take()
        .context("no clipboard paste is pending")?;
    let content = joined.context("clipboard reader stopped")??;
    anyhow::ensure!(!job.stop.is_cancelled(), "clipboard paste cancelled");
    if matches!(
        ui.status.as_str(),
        "Reading clipboard…" | "Wait for clipboard paste, then send the prompt"
    ) {
        ui.status.clear();
    }
    apply_clipboard(ui, content)
}

pub(super) async fn finish_ready_clipboard_paste(ui: &mut Frontend) {
    if ui
        .clipboard_job
        .as_ref()
        .is_some_and(|job| job.task.is_finished())
    {
        finish_pending_clipboard_paste(ui).await;
    }
}

pub(super) async fn finish_pending_clipboard_paste(ui: &mut Frontend) {
    if ui.clipboard_job.is_some()
        && let Err(error) = finish_clipboard_paste(ui).await
    {
        ui.status = format!("Paste failed: {error:#}");
    }
}

pub(super) async fn cancel_clipboard_paste(ui: &mut Frontend) -> Result<()> {
    if let Some(job) = ui.clipboard_job.take() {
        job.stop.cancel();
        // The input is being discarded, but its task must finish. An ordinary
        // clipboard refusal/cancellation is not a terminal-exit failure.
        let _discarded = job
            .task
            .await
            .context("clipboard reader stopped during exit")?;
    }
    Ok(())
}

pub(super) fn prepare_clipboard(
    selected: &Selection,
    content: crate::clipboard_reader::PasteContent,
) -> Result<PreparedPaste> {
    match content {
        crate::clipboard_reader::PasteContent::Files(paths) => Ok(PreparedPaste::Files(paths)),
        crate::clipboard_reader::PasteContent::Image { content, note } => {
            ion_host::image_input::require_image_input(selected)?;
            Ok(PreparedPaste::Image(LoadedImage { content, note }))
        }
        crate::clipboard_reader::PasteContent::Text(text) => Ok(PreparedPaste::Text(text)),
    }
}

pub(super) fn apply_clipboard(ui: &mut Frontend, content: PreparedPaste) -> Result<()> {
    match content {
        PreparedPaste::Files(paths) => {
            let shell = ui.draft.trim_start().starts_with('!');
            let text = clipboard_paths(&paths, shell)?;
            let before = ui.draft[..ui.cursor].chars().next_back();
            let after = ui.draft[ui.cursor..].chars().next();
            let prefix = before.filter(|ch| !ch.is_whitespace()).map_or("", |_| " ");
            let suffix = after.filter(|ch| !ch.is_whitespace()).map_or("", |_| " ");
            ui.insert(&format!("{prefix}{text}{suffix}"));
        }
        PreparedPaste::Image(image) => {
            ui.attach_images(vec![image])?;
        }
        PreparedPaste::Text(text) => ui.insert(&text),
    }
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
