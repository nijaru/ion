//! Owned inspection and binding completions, polled by the terminal's input pump.
use std::{future::Future, path::PathBuf, pin::Pin};

use anyhow::{Context, Result};
use ion_app::{BindingChange, BindingFinished, BindingOperationId, BindingResult, SessionControl};
#[cfg(test)]
use ion_core::SessionView;
use ion_core::{PresentationCursor, Session, SessionPresentation};

use super::{EditorDraft, Frontend, conversation};

#[cfg(test)]
mod tests;

type ReadFuture = Pin<Box<dyn Future<Output = Result<ReadOutput>> + Send>>;

pub(super) enum ReadOutput {
    History(Box<History>),
    Inspection {
        inspection: Box<super::evidence::Inspection>,
        from: Option<super::evidence::Position>,
    },
    Status(String),
    Notice(String),
    CredentialRemoved(String),
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
    Reload(Box<Presentation>),
    SessionInfo(String),
    Context(super::navigation::ContextPickerValue),
    Fork(super::navigation::ForkPickerValue),
    Switch(PathBuf),
    Exported(PathBuf),
    OutputSaved {
        name: String,
        incomplete: bool,
    },
    RecoverySaved {
        id: u64,
        path: PathBuf,
    },
    RecoverySaveRefused {
        id: u64,
        path: PathBuf,
        error: anyhow::Error,
    },
    Copy(String),
}

pub(super) struct History {
    projection: conversation::Prepared,
    prompts: Vec<String>,
    label: String,
    reasoning: ion_ai::Reasoning,
    context: Option<Result<ion_core::ContextReport, ion_core::AgentError>>,
    usage_totals: ion_core::UsageTotals,
    unfinished: bool,
}

impl History {
    fn committed(snapshot: SessionPresentation) -> Self {
        Self {
            projection: conversation::Prepared::committed(
                snapshot.transcript,
                snapshot.id,
                snapshot.selection,
                snapshot.head_entry,
                snapshot.after,
                snapshot.checkpoint,
            ),
            prompts: snapshot.prompts,
            label: label(snapshot.id, snapshot.name),
            reasoning: snapshot.reasoning,
            context: None,
            usage_totals: snapshot.usage,
            unfinished: snapshot.unfinished,
        }
    }

    #[cfg(test)]
    pub(super) fn prepare(view: SessionView) -> Self {
        let projection = conversation::Prepared::new(&view);
        let usage_totals = view.usage().total();
        let prompts = view
            .entries
            .iter()
            .filter_map(|entry| match entry {
                ion_core::SessionEntry::TurnStarted {
                    input, completion, ..
                } if completion.is_empty() => Some(input),
                ion_core::SessionEntry::Steering { input, .. } => Some(input),
                _ => None,
            })
            .filter_map(|input| {
                input.content.iter().find_map(|part| match part {
                    ion_ai::Content::Text(text) => Some(text.clone()),
                    _ => None,
                })
            })
            .collect();
        let label = label(view.id, view.name);
        let unfinished = view.unfinished_turn.is_some() || view.unfinished_user_shell.is_some();
        Self {
            projection,
            prompts,
            label,
            reasoning: view.reasoning,
            context: None,
            usage_totals,
            unfinished,
        }
    }
}

fn label(id: uuid::Uuid, name: Option<String>) -> String {
    name.filter(|name| !name.is_empty())
        .unwrap_or_else(|| format!("Session {}", &id.to_string()[..8]))
}

struct Read {
    session: uuid::Uuid,
    generation: u64,
    path: PathBuf,
    future: ReadFuture,
    source: Option<EditorDraft>,
    projection_revision: Option<u64>,
    writes_files: bool,
    binding_intent: bool,
    reload_presentation: bool,
}

pub(super) struct ReadFinished {
    retired: bool,
    read: Read,
    result: Result<ReadOutput>,
}

pub(super) struct Presentation {
    pub(super) keys: crate::keybindings::KeyBindings,
    pub(super) themes: crate::themes::Themes,
    pub(super) footer: super::orientation::Footer,
}

pub(super) struct Binding {
    operation: BindingOperationId,
    origin: super::recovery::Origin,
    pub(super) source: Option<EditorDraft>,
    restore: Option<ion_ai::Message>,
    presentation: Option<Box<Presentation>>,
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
    retired_refresh: Option<Read>,
    pub(super) blocked: bool,
    pub(super) refresh_error: Option<String>,
    pub(super) binding: Option<Binding>,
    restore: Option<ion_ai::Message>,
}

impl SessionIo {
    pub(super) fn has_read(&self) -> bool {
        self.read.is_some() || self.retired_refresh.is_some()
    }

    pub(super) fn command_slot_occupied(&self) -> bool {
        self.binding.is_some() || (self.read.is_some() && !self.passive_refresh_in_read_slot())
    }

    fn passive_refresh_in_read_slot(&self) -> bool {
        !self.blocks_input()
            && self
                .read
                .as_ref()
                .is_some_and(|read| read.projection_revision.is_some())
    }

    pub(super) fn has_command_read(&self) -> bool {
        self.read
            .as_ref()
            .is_some_and(|read| read.projection_revision.is_none())
    }

    pub(super) fn reports_work(&self) -> bool {
        // A required refresh delays input. A routine refresh after settlement
        // must not put the terminal tab back into its working state.
        if self.blocked {
            self.read.is_some() || self.refresh_pending
        } else {
            self.has_command_read()
        }
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
        self.schedule_refresh();
    }

    /// Refresh the view without revoking input admission for a settled Turn
    /// whose committed transcript is already present.
    pub(super) fn schedule_refresh(&mut self) {
        self.refresh_pending = true;
    }

    pub(super) fn refreshing(&self) -> bool {
        self.refresh_pending
            || self
                .read
                .as_ref()
                .is_some_and(|read| read.projection_revision.is_some())
    }

    pub(super) fn start_refresh(
        &mut self,
        control: &SessionControl<EditorDraft>,
        conversation: &conversation::Conversation,
    ) -> Result<()> {
        if !self.refresh_pending
            || self.has_read()
            || self.binding.is_some()
            || (!self.blocked && control.is_busy())
        {
            return Ok(());
        }
        self.refresh_pending = false;
        let session = control.session();
        let io = session.io()?;
        let agent = control.agent().clone();
        let instructions = control.instructions().to_owned();
        let after = conversation.checkpoint(session);
        self.start(session, async move {
            Ok(ReadOutput::History(
                io.run(move |session| {
                    let snapshot = session.presentation(after)?;
                    let revision = snapshot.revision;
                    let context = agent.context_report(session, &instructions);
                    let mut history = History::committed(snapshot);
                    // A concurrent append can separate these read-only cuts. Never
                    // attach another revision's estimate to the displayed history.
                    if !context
                        .as_ref()
                        .is_ok_and(|report| report.revision != revision)
                    {
                        history.context = Some(context);
                    }
                    Ok(Box::new(history))
                })
                .await?,
            ))
        })?;
        self.read
            .as_mut()
            .expect("started refresh")
            .projection_revision = Some(conversation.revision());
        Ok(())
    }

    pub(super) fn start(
        &mut self,
        session: &Session,
        future: impl Future<Output = Result<ReadOutput>> + Send + 'static,
    ) -> Result<()> {
        if self.passive_refresh_in_read_slot() {
            assert!(
                self.retired_refresh.is_none(),
                "only one routine refresh can be in flight"
            );
            self.retired_refresh = self.read.take();
            self.schedule_refresh();
        }
        anyhow::ensure!(
            self.read.is_none() && self.binding.is_none(),
            "Wait for Session inspection or binding to finish"
        );
        self.read = Some(Read {
            session: session.id(),
            generation: self.generation,
            path: session.file_path()?.to_owned(),
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
        let retired = self.read.is_none();
        let slot = if retired {
            &mut self.retired_refresh
        } else {
            &mut self.read
        };
        let result = slot
            .as_mut()
            .expect("guarded Session inspection")
            .future
            .as_mut()
            .await;
        ReadFinished {
            read: slot.take().expect("completed Session inspection"),
            result,
            retired,
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
    let origin = super::recovery::Origin::capture(runtime.session());
    let operation = runtime.start_binding(change)?;
    // Invalidate in-flight reads even for same-Session context/model changes.
    ui.session_io.generation += 1;
    ui.session_io.blocked = true;
    ui.session_io.binding = Some(Binding {
        operation,
        origin,
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
    // Filesystem commands and completion follow the actual binding even when
    // its canonical history is unreadable. A display snapshot never owns cwd.
    ui.cwd = control
        .session()
        .cwd()
        .context("terminal coding requires a working directory")?
        .to_owned();
    ui.session_io.generation += 1;
    ui.session_io.request_refresh();
    match finished.result {
        Ok(result) => {
            if let Some(pending) = pending {
                ui.session_io.restore = pending.restore;
                if let Some(presentation) = pending.presentation {
                    ui.editor.break_edit_focus();
                    ui.keys = presentation.keys;
                    ui.footer = presentation.footer;
                    ui.install_reloaded_theme(presentation.themes);
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
            if let Some(pending) = pending
                && let Some(source) = pending.source
            {
                let id = ui.recovery.command(
                    source,
                    pending.origin,
                    format!("Session change failed: {error:#}"),
                );
                ui.note(format!(
                    "Session change outcome requires inspection · /recovery {id} · not replayed"
                ));
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
    let ReadFinished {
        read,
        result,
        retired,
    } = finished;
    if retired {
        return Ok(None);
    }
    if read.session != runtime.session().id() || read.generation != ui.session_io.generation {
        // A read owns its original coordinates, never a lookup of the new binding.
        if let Some(source) = read.source {
            let cause = match &result {
                Ok(ReadOutput::RecoverySaveRefused { id, path, error }) => format!(
                    "Recovery backup {id} was refused before file creation at {} after its original Session scope changed: {error:#}",
                    path.display()
                ),
                Ok(_) => {
                    "Inspection/write completed after its original Session scope changed".into()
                }
                Err(error) => format!(
                    "Inspection/write failed after its original Session scope changed: {error:#}"
                ),
            };
            let id = ui.recovery.command(
                source,
                super::recovery::Origin {
                    session: read.session,
                    path: Some(read.path),
                },
                cause,
            );
            ui.note(format!("Late Session operation retained · /recovery {id}"));
        }
        return Ok(None);
    }
    let source = read.source;
    match result {
        Ok(ReadOutput::History(history)) => {
            if read.projection_revision != Some(ui.conversation.revision())
                || (!ui.session_io.blocked && runtime.is_busy())
            {
                // A newer observation can supersede the read's cut. Also avoid
                // installing a background snapshot ahead of the next Turn's events.
                ui.session_io.schedule_refresh();
                return Ok(None);
            }
            let interrupted = history.unfinished && !runtime.is_busy();
            ui.apply_history(&read.path, *history);
            if interrupted {
                ui.status = "Interrupted; effects unknown. Send a message to continue.".into();
            }
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
        Ok(ReadOutput::Inspection { inspection, from }) => {
            if from.is_none()
                || from
                    == ui
                        .details
                        .as_ref()
                        .and_then(super::evidence::Inspection::position)
            {
                ui.details = Some(*inspection);
            }
        }
        Ok(ReadOutput::Status(status)) => ui.status = status,
        Ok(ReadOutput::Notice(notice)) => ui.print(notice),
        Ok(ReadOutput::CredentialRemoved(provider)) => {
            ui.provider_usage.credential_changed(&provider);
            ui.print(format!("Removed saved {provider} credential"));
        }
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
        Ok(ReadOutput::OutputSaved { name, incomplete }) => {
            let note = if incomplete {
                " (incomplete capture)"
            } else {
                ""
            };
            ui.status = format!("Output saved to {name}{note}");
        }
        Ok(ReadOutput::RecoverySaved { id, path }) => {
            ui.status = format!(
                "Recovery {id} inspection backup saved to {}; not replay/import authority",
                path.display()
            );
        }
        Ok(ReadOutput::RecoverySaveRefused { id, path, error }) => {
            if let Some(source) = source {
                ui.return_command_source(source);
            }
            ui.status = format!(
                "Recovery backup was not saved for {id} at {}: {error:#}",
                path.display()
            );
        }
        Ok(ReadOutput::Copy(text)) => return Ok(Some(UiEffect::Copy(text))),
        Err(error) => {
            if let Some(source) = source {
                if read.writes_files {
                    let id = ui.recovery.command(
                        source,
                        super::recovery::Origin {
                            session: read.session,
                            path: Some(read.path),
                        },
                        format!("File operation failed: {error:#}"),
                    );
                    ui.note(format!(
                        "File operation failed; original retained · /recovery {id} · not replayed"
                    ));
                } else {
                    ui.return_command_source(source);
                }
            }
            if read.writes_files {
                ui.status =
                    format!("File operation failed: {error:#}; inspect /recovery before retrying");
            } else if read.projection_revision.is_some() {
                // A failed history read is not a harmless stale estimate.
                ui.session_io.blocked = true;
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
    let ReadFinished {
        read,
        result,
        retired,
    } = finished;
    if retired {
        return;
    }
    let result = match result {
        Ok(ReadOutput::RecoverySaveRefused { id, path, error }) => {
            if let Some(source) = read.source {
                ui.return_command_source(source);
            }
            super::activity::combine_error(
                outcome,
                error.context(format!(
                    "Recovery backup {id} was not saved at {} during shutdown",
                    path.display()
                )),
            );
            return;
        }
        result => result,
    };
    if let Some(source) = read.source {
        let cause = match &result {
            Ok(_) => "Session inspection/write joined during shutdown".into(),
            Err(error) => format!("Session inspection/write failed during shutdown: {error:#}"),
        };
        ui.recovery.command(
            source,
            super::recovery::Origin {
                session: read.session,
                path: Some(read.path),
            },
            cause,
        );
    }
    if let Err(error) = result {
        super::activity::combine_error(outcome, error);
    }
}

pub(super) async fn final_history(
    session: &std::sync::Arc<Session>,
    after: Option<PresentationCursor>,
) -> Result<History> {
    Ok(session
        .reader()?
        .map_presentation(after, History::committed)
        .await?)
}

impl Frontend {
    pub(super) fn apply_history(&mut self, path: &std::path::Path, history: History) {
        let History {
            projection,
            prompts,
            label,
            reasoning,
            context,
            usage_totals,
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
                self.history_view = None;
            }
            self.view.follow_end();
        }
        if let Some(details) = &mut self.details
            && matches!(details.source, super::evidence::Source::Live)
        {
            details.view.invalidate();
        }
        self.editor.replace_prompt_history(prompts, same_session);
        self.session_label = label;
        self.reasoning_preference = Some(reasoning);
        self.context_estimate = context;
        self.usage_totals = usage_totals;
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
        self.apply_history(session.file_path().unwrap(), History::prepare(view.clone()));
    }
}
