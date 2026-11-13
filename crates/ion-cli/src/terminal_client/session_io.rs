//! Owned inspection and binding completions, polled by the terminal's input pump.
use std::{future::Future, path::PathBuf, pin::Pin};

use anyhow::{Context, Result};
use ion_app::{BindingChange, BindingFinished, BindingOperationId, BindingResult, SessionControl};
use ion_core::{Session, SessionView};

use super::{EditorDraft, Frontend, conversation};

#[cfg(test)]
mod tests;

type ReadFuture = Pin<Box<dyn Future<Output = Result<ReadOutput>> + Send>>;

pub(super) enum ReadOutput {
    History(Box<History>),
    Notice(String),
    Picker(Box<super::Picker>),
    Models {
        choices: ion_app::ModelChoices,
        current: ion_ai::ModelRef,
    },
    ModelAccess {
        selection: ion_app::Selection,
        missing: bool,
    },
    SelectModel(ion_ai::ModelRef),
    Reload(Box<(crate::keybindings::KeyBindings, crate::themes::Themes)>),
    SessionInfo(String),
    Context(super::navigation::ContextPickerValue),
    Fork(super::navigation::ForkPickerValue),
    Switch(PathBuf),
    Exported(PathBuf),
    Copy(String),
}

pub(super) struct History {
    projection: conversation::Prepared,
    prompts: Vec<String>,
    label: String,
    unfinished: bool,
}

impl History {
    pub(super) fn prepare(view: SessionView) -> Self {
        let projection = conversation::Prepared::new(&view);
        let prompts = view
            .entries
            .iter()
            .filter_map(|entry| match entry {
                ion_core::SessionEntry::TurnStarted { input, .. }
                | ion_core::SessionEntry::Steering { input, .. } => {
                    input.content.iter().find_map(|part| match part {
                        ion_ai::Content::Text(text) => Some(text.clone()),
                        _ => None,
                    })
                }
                _ => None,
            })
            .collect();
        let label = view
            .name
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| format!("Session {}", &view.id.to_string()[..8]));
        let unfinished = view.unfinished_turn.is_some() || view.unfinished_user_shell.is_some();
        Self {
            projection,
            prompts,
            label,
            unfinished,
        }
    }
}

struct Read {
    session: uuid::Uuid,
    generation: u64,
    path: PathBuf,
    cwd: Option<PathBuf>,
    future: ReadFuture,
    source: Option<EditorDraft>,
    projection_revision: Option<u64>,
    writes_files: bool,
    binding_intent: bool,
    reload_presentation: bool,
}

pub(super) struct ReadFinished {
    read: Read,
    result: Result<ReadOutput>,
}

pub(super) struct Binding {
    operation: BindingOperationId,
    pub(super) source: Option<EditorDraft>,
    restore: Option<ion_ai::Message>,
    presentation: Option<Box<(crate::keybindings::KeyBindings, crate::themes::Themes)>>,
}

pub(super) enum UiEffect {
    Copy(String),
    ModelAccess {
        selection: ion_app::Selection,
        missing: bool,
        source: Option<EditorDraft>,
    },
}

#[derive(Default)]
pub(super) struct SessionIo {
    generation: u64,
    read: Option<Read>,
    refresh_pending: bool,
    // A failed refresh is not evidence that a committed context was rolled back.
    pub(super) blocked: bool,
    pub(super) refresh_error: Option<String>,
    pub(super) binding: Option<Binding>,
    restore: Option<ion_ai::Message>,
}

impl SessionIo {
    pub(super) fn has_read(&self) -> bool {
        self.read.is_some()
    }

    pub(super) fn reloading_presentation(&self) -> bool {
        self.read
            .as_ref()
            .is_some_and(|read| read.reload_presentation)
            || self
                .binding
                .as_ref()
                .is_some_and(|binding| binding.presentation.is_some())
    }

    pub(super) fn has_pending_effect(&self) -> bool {
        self.binding.is_some()
            || self
                .read
                .as_ref()
                .is_some_and(|read| read.binding_intent || read.writes_files)
    }

    pub(super) fn blocks_input(&self) -> bool {
        self.blocked || self.has_pending_effect()
    }

    pub(super) fn request_refresh(&mut self) {
        self.blocked = true;
        self.refresh_pending = true;
    }

    pub(super) fn start_refresh(
        &mut self,
        session: &std::sync::Arc<Session>,
        projection_revision: u64,
    ) -> Result<()> {
        if !self.refresh_pending || self.read.is_some() || self.binding.is_some() {
            return Ok(());
        }
        self.refresh_pending = false;
        let reader = session.reader()?;
        self.start(session, async move {
            Ok(ReadOutput::History(
                reader
                    .map_view(|view| Box::new(History::prepare(view)))
                    .await?,
            ))
        })?;
        self.read
            .as_mut()
            .expect("started refresh")
            .projection_revision = Some(projection_revision);
        Ok(())
    }

    pub(super) fn start(
        &mut self,
        session: &Session,
        future: impl Future<Output = Result<ReadOutput>> + Send + 'static,
    ) -> Result<()> {
        anyhow::ensure!(
            self.read.is_none() && self.binding.is_none(),
            "Wait for Session inspection or binding to finish"
        );
        self.read = Some(Read {
            session: session.id(),
            generation: self.generation,
            path: session.path().to_owned(),
            cwd: session.cwd().map(ToOwned::to_owned),
            future: Box::pin(future),
            source: None,
            projection_revision: None,
            writes_files: false,
            binding_intent: false,
            reload_presentation: false,
        });
        Ok(())
    }

    pub(super) fn start_binding_read(
        &mut self,
        session: &Session,
        future: impl Future<Output = Result<ReadOutput>> + Send + 'static,
    ) -> Result<()> {
        self.start(session, future)?;
        self.read
            .as_mut()
            .expect("started binding inspection")
            .binding_intent = true;
        Ok(())
    }

    pub(super) fn start_presentation_reload(
        &mut self,
        session: &Session,
        future: impl Future<Output = Result<ReadOutput>> + Send + 'static,
    ) -> Result<()> {
        self.start_binding_read(session, future)?;
        self.read
            .as_mut()
            .expect("started presentation reload")
            .reload_presentation = true;
        Ok(())
    }

    pub(super) fn start_file_write(
        &mut self,
        session: &Session,
        future: impl Future<Output = Result<ReadOutput>> + Send + 'static,
    ) -> Result<()> {
        self.start(session, future)?;
        self.read
            .as_mut()
            .expect("started native file operation")
            .writes_files = true;
        Ok(())
    }

    pub(super) fn retain_source(&mut self, source: Option<EditorDraft>) {
        if let Some(binding) = &mut self.binding {
            binding.source = source;
        } else if let Some(read) = &mut self.read {
            read.source = source;
        }
    }

    pub(super) async fn complete(&mut self) -> ReadFinished {
        let result = self
            .read
            .as_mut()
            .expect("guarded Session inspection")
            .future
            .as_mut()
            .await;
        ReadFinished {
            read: self.read.take().expect("completed Session inspection"),
            result,
        }
    }
}

pub(super) fn start_binding(
    runtime: &mut SessionControl<EditorDraft>,
    ui: &mut Frontend,
    change: BindingChange,
    restore: Option<ion_ai::Message>,
) -> Result<()> {
    anyhow::ensure!(
        ui.session_io.binding.is_none(),
        "Wait for the binding operation"
    );
    let operation = runtime.start_binding(change)?;
    // Invalidate in-flight reads even for same-Session context/model changes.
    ui.session_io.generation += 1;
    ui.session_io.blocked = true;
    ui.session_io.binding = Some(Binding {
        operation,
        source: None,
        restore,
        presentation: None,
    });
    ui.status = "Preparing Session change…".into();
    Ok(())
}

pub(super) fn binding_finished(
    ui: &mut Frontend,
    control: &SessionControl<EditorDraft>,
    finished: BindingFinished,
) -> Result<()> {
    let pending = ui.session_io.binding.take();
    if let Some(pending) = &pending {
        anyhow::ensure!(
            pending.operation == finished.operation,
            "binding completion identity differs"
        );
    }
    ui.session_io.generation += 1;
    ui.session_io.request_refresh();
    match finished.result {
        Ok(result) => {
            if let Some(pending) = pending {
                ui.session_io.restore = pending.restore;
                if let Some(presentation) = pending.presentation {
                    let (keys, themes) = *presentation;
                    ui.editor.break_edit_focus();
                    ui.keys = keys;
                    ui.install_reloaded_theme(themes);
                    ui.print(super::resource_commands::reloaded(
                        control.resources(),
                        control.commands(),
                        &ui.cwd,
                    ));
                }
            }
            ui.status = match result {
                BindingResult::ResourcesReloaded => String::new(),
                BindingResult::ModelSelected => {
                    let model = control.agent().model();
                    format!("Selected {}/{}", model.provider, model.model)
                }
                BindingResult::ReasoningSelected => "Generation effort selected".into(),
                BindingResult::SessionNamed { .. } => "Session renamed".into(),
                BindingResult::SessionCreated => "Started a new session".into(),
                BindingResult::SessionCloned { session_id } => {
                    format!("Cloned conversation as {session_id}; workspace files unchanged")
                }
                BindingResult::SessionForked { session_id } => {
                    format!("Forked as {session_id}; workspace files unchanged")
                }
                BindingResult::ContextNavigated(_) => {
                    "Context selected; alternatives retained; workspace files unchanged".into()
                }
                BindingResult::SessionSwitched => "Session resumed".into(),
            };
        }
        Err(error) => {
            if let Some(source) = pending.and_then(|pending| pending.source) {
                ui.note(format!("Session change failed; inspect before retrying. Original command retained separately: {}", source.literal));
                ui.uncertain_commands.push(source);
            }
            ui.status =
                format!("Session change failed: {error:#}; refreshing before further input");
        }
    }
    Ok(())
}

pub(super) fn apply(
    runtime: &mut SessionControl<EditorDraft>,
    ui: &mut Frontend,
    finished: ReadFinished,
) -> Result<Option<UiEffect>> {
    let ReadFinished { read, result } = finished;
    if read.session != runtime.session().id() || read.generation != ui.session_io.generation {
        // A read owns its original coordinates, never a lookup of the new binding.
        if let Some(source) = read.source {
            ui.uncertain_commands.push(source);
        }
        return Ok(None);
    }
    let source = read.source;
    match result {
        Ok(ReadOutput::History(history)) => {
            if read.projection_revision != Some(ui.conversation.revision()) {
                // Live publication may have advanced after this native read's cut.
                // Never replace it with an older canonical prefix.
                ui.session_io.request_refresh();
                return Ok(None);
            }
            let interrupted = history.unfinished && !runtime.is_busy();
            ui.apply_history(&read.path, *history);
            if interrupted {
                ui.status = "Interrupted; effects unknown. Send a message to continue.".into();
            }
            ui.cwd = read
                .cwd
                .context("terminal coding requires a working directory")?;
            if !ui.session_io.refresh_pending {
                ui.session_io.blocked = false;
                ui.session_io.refresh_error = None;
                if let Some(input) = ui.session_io.restore.take()
                    && let Err(error) = ui.restore_input(input)
                {
                    ui.status =
                        format!("Context selected; {error}; draft retained; files unchanged");
                }
            }
        }
        Ok(ReadOutput::Notice(notice)) => ui.print(notice),
        Ok(ReadOutput::Picker(picker)) => ui.pickers.open(*picker),
        Ok(ReadOutput::Models { choices, current }) => {
            ui.pickers
                .apply_models(choices, Some(&current), "Choose model")
        }
        Ok(ReadOutput::ModelAccess { selection, missing }) => {
            return Ok(Some(UiEffect::ModelAccess {
                selection,
                missing,
                source,
            }));
        }
        Ok(ReadOutput::SelectModel(identity)) => {
            let result = start_binding(runtime, ui, BindingChange::SelectModel(identity), None);
            finish_command(ui, source, result);
        }
        Ok(ReadOutput::Reload(presentation)) => {
            match start_binding(runtime, ui, BindingChange::ReloadResources, None) {
                Ok(()) => {
                    let binding = ui.session_io.binding.as_mut().expect("started reload");
                    binding.presentation = Some(presentation);
                    binding.source = source;
                }
                Err(error) => finish_command(ui, source, Err(error)),
            }
        }
        Ok(ReadOutput::SessionInfo(info)) => {
            ui.print(info);
            // /session is also the explicit refresh retry after an inspection fault.
            if ui.session_io.blocked {
                ui.session_io.request_refresh();
            }
        }
        Ok(ReadOutput::Context(value)) => {
            let result = super::navigation::apply_context(runtime, ui, value);
            finish_command(ui, source, result);
        }
        Ok(ReadOutput::Fork(value)) => {
            let result = super::navigation::apply_fork(runtime, ui, value);
            finish_command(ui, source, result);
        }
        Ok(ReadOutput::Switch(path)) => {
            let result = start_binding(runtime, ui, BindingChange::SwitchSession(path), None);
            finish_command(ui, source, result);
        }
        Ok(ReadOutput::Exported(path)) => {
            ui.status = format!("Transcript saved to {}", path.display())
        }
        Ok(ReadOutput::Copy(text)) => return Ok(Some(UiEffect::Copy(text))),
        Err(error) => {
            if let Some(source) = source {
                if read.writes_files {
                    ui.note(format!("File operation failed; inspect before retrying. Original command retained separately: {}", source.literal));
                    ui.uncertain_commands.push(source);
                } else {
                    ui.return_command_source(source);
                }
            }
            if ui.session_io.blocked {
                ui.session_io.refresh_error = Some(format!("{error:#}"));
            } else {
                ui.status = format!("Session inspection failed: {error:#}");
            }
        }
    }
    Ok(None)
}

fn finish_command(ui: &mut Frontend, source: Option<EditorDraft>, result: Result<()>) {
    match result {
        Ok(()) => ui.session_io.retain_source(source),
        Err(error) => {
            if let Some(source) = source {
                ui.return_command_source(source);
            }
            ui.status = format!("Session command was not started: {error:#}");
        }
    }
}

pub(super) fn cleanup(ui: &mut Frontend, finished: ReadFinished, outcome: &mut Result<()>) {
    if let Some(source) = finished.read.source {
        ui.uncertain_commands.push(source);
    }
    if let Err(error) = finished.result {
        super::activity::combine_error(outcome, error);
    }
}

pub(super) async fn final_history(session: &std::sync::Arc<Session>) -> Result<History> {
    Ok(session.reader()?.map_view(History::prepare).await?)
}

impl Frontend {
    pub(super) fn apply_history(&mut self, path: &std::path::Path, history: History) {
        let History {
            projection,
            prompts,
            label,
            ..
        } = history;
        // The first native snapshot installs display facts, not a navigation.
        // Edits and pickers opened while it loaded still belong to this Session.
        let initialized = self.conversation.scope().is_some();
        let change = self.conversation.reconcile_prepared(path, projection);
        let same_session = !initialized || change != conversation::Change::Session;
        if !same_session {
            self.editor.reset_edit_baseline();
        }
        if change != conversation::Change::Unchanged {
            if initialized {
                self.editor.completion.clear();
                self.pickers.close();
                self.details = None;
            }
            self.view.follow_end();
        }
        if let Some(details) = &mut self.details {
            details.invalidate();
        }
        self.editor.replace_prompt_history(prompts, same_session);
        self.session_label = label;
    }

    #[cfg(test)]
    pub(super) fn refresh_session(&mut self, session: &Session) -> Result<()> {
        self.load_history(session, &session.view()?);
        self.cwd = session
            .cwd()
            .context("terminal coding requires a working directory")?
            .to_owned();
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn load_history(&mut self, session: &Session, view: &SessionView) {
        self.apply_history(session.path(), History::prepare(view.clone()));
    }
}
