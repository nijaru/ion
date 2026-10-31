//! Native coding application over shared Host control and the reusable agent runtime.
#[cfg(test)]
use ion_ai::{Message, Role};
#[cfg(test)]
use ion_terminal::{KeyCode, Modifiers};
#[cfg(test)]
use std::fs;
use std::{collections::HashSet, path::PathBuf};

use crate::display_text::{fit_line, push_wrapped};
use crate::keybindings::{self, COMPLETION, COMPOSER, DETAILS, KeyAction, KeyBindings};
use crate::terminal_commands::Builtin;
use crate::transcript_detail::{DetailView, tools};
use crate::transcript_render::kind_label;
use anyhow::{Context, Result};
use ion_ai::{Content, ModelRef};
use ion_core::{LiveTranscript, Session, SessionEntry, SessionView, TranscriptItem, TurnEndReason};
use ion_host::image_input::{ImageSource, LoadedImage};
use ion_host::{CredentialStore, Delivery, QuestionResponder, Resources};
use ion_terminal::{
    Frame, InputEvent, KeyEvent, MouseKind, ProgramStatus, Screen, TerminalSession,
};
use ratatui::text::Line;
use tokio::time::Duration;
#[cfg(test)]
use tokio_util::sync::CancellationToken;
mod activity;
mod attach;
mod block_presenter;
mod clipboard_input;
mod command;
mod configuration;
mod conversation;
mod dock;
mod draft;
mod editing;
mod editor;
mod editor_control;
mod editor_recovery;
mod file_discovery;
mod interaction;
mod mcp;
mod navigation;
mod orientation;
pub(crate) mod paint;
mod picker;
mod question;
mod session_search;
mod setup;
mod tasks;
mod theme_selection;
use clipboard_input::*;
use command::{CommandInput, dispatch_command};
use editor::{Editor, EditorDraft, RecoveredDraft};
use interaction::{apply_picker, submit_editor, submit_initial_prompts};
use picker::{Picker, PickerValue, Pickers};
use question::{QuestionAction, QuestionModal};
pub use setup::{choose_session, setup_model};

pub(crate) const MAX_DRAFT: usize = 64 * 1024;
const RESUME_TURN_LIMIT: usize = 6;
const RESUME_ENTRY_LIMIT_WITHOUT_TURNS: usize = 32;

#[derive(Default)]
pub(super) struct ProgramStatusState {
    // Preparation and modal activity overlay this result; neither consumes it.
    settled: Option<ProgramStatus>,
    blocked_auth: bool,
    blocked_question: bool,
    published: Option<ProgramStatus>,
}

impl ProgramStatusState {
    fn settle(&mut self, status: ProgramStatus) {
        self.settled = Some(status);
    }

    fn observe<M>(&mut self, update: &ion_host::ControlUpdate<M>) {
        if let ion_host::ControlUpdate::Settled(settlement) = update {
            self.settle(settlement_program_status(&settlement.outcome));
        }
    }

    fn status(&self, busy: bool) -> ProgramStatus {
        if self.blocked_auth {
            ProgramStatus::BlockedAuth
        } else if self.blocked_question {
            ProgramStatus::BlockedQuestion
        } else if busy {
            ProgramStatus::Working
        } else {
            self.settled.unwrap_or(ProgramStatus::Idle)
        }
    }

    fn publish(&mut self, terminal: &mut TerminalSession, busy: bool) -> Result<()> {
        let status = self.status(busy);
        if self.published == Some(status) {
            return Ok(());
        }
        terminal
            .publish_status(status)
            .context("publish terminal program status")?;
        self.published = Some(status);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum TuiMode {
    #[default]
    #[value(alias = "regular")]
    Inline,
    Fullscreen,
}

impl TuiMode {
    const fn label(self) -> &'static str {
        match self {
            Self::Inline => "inline",
            Self::Fullscreen => "fullscreen",
        }
    }
}

use activity::ActiveOperation;

#[derive(Default)]
struct Frontend {
    keys: KeyBindings,
    themes: crate::themes::Themes,
    theme_preview_name: Option<String>,
    theme_picker: Option<theme_selection::ThemePicker>,
    mode: TuiMode,
    output_detail: crate::tool_output::OutputDetail,
    thinking_visibility: crate::transcript_render::ThinkingVisibility,
    conversation: conversation::Conversation,
    presenter: block_presenter::BlockPresenter,
    view: block_presenter::ConversationView,
    editor: Editor,
    status: String,
    pickers: Pickers,
    details: Option<DetailView>,
    clipboard_job: Option<ClipboardJob>,
    question: Option<QuestionModal>,
    cwd: PathBuf,
    session_label: String,
}

pub struct ChatInit {
    pub services: std::sync::Arc<ion_host::Host>,
    pub setup: ion_host::SessionSetup,
    pub images: Vec<LoadedImage>,
    pub files: Vec<ion_host::prompt_files::TextFile>,
    pub startup_diagnostics: Vec<String>,
    pub initial_prompts: Vec<String>,
    pub tui_mode: TuiMode,
    pub(crate) keys: KeyBindings,
    pub(crate) themes: crate::themes::Themes,
    pub(crate) interrupt: tokio::signal::unix::Signal,
}

pub async fn chat(init: ChatInit) -> Result<()> {
    let mut runtime = init.setup.into_control::<EditorDraft>();
    let questions = runtime
        .attach_question_responder()
        .context("attach the terminal question consumer")?;
    let application_owner = runtime.owner();
    let background = runtime.background();
    let mut background_revision = 0;
    let mut completion_revision = 0;
    let mut ui = Frontend {
        editor: Editor {
            images: init.images.into_iter().map(Into::into).collect(),
            files: init.files,
            ..Editor::default()
        },
        mode: init.tui_mode,
        keys: init.keys,
        themes: init.themes,
        ..Frontend::default()
    };
    let mut interrupt = init.interrupt;
    let mut initial_prompts = Some(init.initial_prompts).filter(|prompts| !prompts.is_empty());
    let mut program = ProgramStatusState::default();
    let mut terminal_owner = None;
    let mut screen_owner = None;
    let mut outcome = async {
        // Even terminal entry and the first history read are inside the shared
        // shutdown boundary: no post-Session error bypasses retained cleanup.
        terminal_owner = Some(TerminalSession::enter()
            .await
            .context("interactive chat requires a terminal")?);
        let terminal = terminal_owner.as_mut().expect("entered terminal");
        screen_owner = Some(new_inline_screen(terminal).await?);
        let screen = screen_owner.as_mut().expect("initialized screen");
        ui.refresh_session(runtime.session())?;
        for diagnostic in init.startup_diagnostics {
            ui.note(diagnostic);
        }
        let mut poll_control = true;
        let mut theme_poll = tokio::time::interval(Duration::from_secs(1));
        theme_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            terminal.check_active().context("terminal lifecycle failed")?;
            ui.sync_question(questions.as_ref());
            mcp::refresh(&runtime, &mut ui);
            ui.refresh_completion(Some(runtime.resources()), Some(runtime.commands()), false);
            program.blocked_question = ui.question_owns_input();
            program.publish(terminal, runtime.is_busy())?;
            let painted = draw(terminal, screen, &mut ui, ActiveOperation::from_state(runtime.state()), Some(runtime.agent().model())).await?;
            #[cfg(debug_assertions)]
            if std::env::var_os("ION_SMOKE_PANIC_AFTER_FIRST_DRAW").is_some() {
                panic!("ION smoke panic after first terminal draw");
            }
            // CLI text is not simulated editor input. Admit the ordered batch
            // once after first draw, without interpreting terminal commands.
            // Host custody owns all subsequent Turns, rejection and Stop.
            tokio::select! {
                biased;
                _ = interrupt.recv() => anyhow::bail!("terminal interrupted"),
                () = std::future::ready(()) => {}
            }
            if let Some(prompts) = initial_prompts.take() {
                submit_initial_prompts(&mut runtime, &mut ui, prompts);
                poll_control = true;
                continue;
            }
            let retry_paint = !painted;
            let readiness = runtime.completion_changes();
            let event = tokio::select! {
                _ = interrupt.recv() => anyhow::bail!("terminal interrupted"),
                _ = readiness.changed(completion_revision) => {
                    completion_revision = readiness.revision();
                    if let Some(owner) = &background { tasks::refresh(&mut ui, owner); }
                    poll_control = true;
                    continue;
                },
                // Idle is eager. Disable this branch until another UI action
                // can have produced work or reliable updates.
                update = runtime.next(), if poll_control => {
                    poll_control = !matches!(update, ion_host::ControlUpdate::Idle);
                    program.observe(&update);
                    let _visible_settlement_error = activity::apply(&mut ui, &runtime, update)?;
                    continue;
                },
                revision = async { background.as_ref().expect("guarded owner").changed(background_revision).await }, if background.is_some() => {
                    background_revision = revision;
                    let owner = background.as_ref().expect("guarded owner");
                    owner.check()?;
                    let summary = owner.summary();
                    tasks::refresh(&mut ui, owner);
                    ui.status = format!("Background: {} running · {} retained results · /tasks", summary.running, summary.joined);
                    continue;
                },
                _ = tokio::time::sleep(Duration::from_millis(50)), if retry_paint => continue,
                notice = async {
                    loop {
                        theme_poll.tick().await;
                        if let Some(notice) = ui.themes.check_files() { break notice; }
                    }
                }, if ui.themes.watches_file() => {
                    ui.status = notice;
                    continue;
                },
                event = terminal.next_input() => event,
                joined = async { (&mut ui.clipboard_job.as_mut().expect("pending clipboard").task).await }, if ui.clipboard_job.is_some() => {
                    if let Err(error) = finish_joined_clipboard_paste(&mut ui, joined) {
                        ui.status = format!("Paste failed: {error:#}");
                    }
                    continue;
                },
                notice = ui.pickers.complete(), if ui.pickers.has_job() => {
                    if let Some(notice) = notice { ui.note(notice); }
                    continue;
                }
            };
            terminal.check_active().context("terminal lifecycle failed while awaiting input")?;
            let event = event.context("terminal input ended unexpectedly")?;
            invalidate_resized_input(terminal, screen, &mut ui)?;
            match event? {
                InputEvent::Key(key) => {
                    if ui.keys.matches(KeyAction::Suspend, key) {
                        ui.editor.break_edit_focus();
                        terminal.release_inline(screen).await?;
                        terminal.suspend_to_job_control().await.context("suspend terminal job")?;
                        program.published = None;
                        *screen = new_inline_screen(terminal).await.context("restore chat after job suspension")?;
                        poll_control = true;
                        continue;
                    }
                    if ui.keys.matches(KeyAction::Interrupt, key)
                        && (runtime.is_busy() || ActiveOperation::from_state(runtime.state()).is_some())
                    {
                        ui.editor.break_edit_focus();
                        runtime.request_stop();
                        poll_control = true;
                        continue;
                    }
                    if ui.question_owns_input() {
                        let action = ui
                            .question
                            .as_mut()
                            .expect("question owns input")
                            .key(key, &ui.keys);
                        apply_question_action(&mut ui, questions.as_ref(), action, terminal, screen).await?;
                        poll_control = true;
                        continue;
                    }
                    match ui.key(
                        key,
                        Some(runtime.resources()),
                        Some(runtime.commands()),
                    ) {
                        Action::None => {},
                        Action::Quit => break,
                        Action::Submit(source) => {
                            let delivery = interaction::editor_delivery(&runtime);
                            submit_editor(&mut runtime, &mut ui, delivery, source);
                        },
                        Action::Queue(source) => {
                            submit_editor(&mut runtime, &mut ui, Delivery::FollowUp, source);
                        },
                        Action::Recover => {
                            if runtime.withdraw_last_follow_up().is_none() {
                                ui.recover_draft();
                            }
                        },
                        Action::Shell { command, exclude_from_context, source } => {
                            match runtime.shell(command, exclude_from_context, source) {
                                Ok(_) => {
                                    ui.status.clear();
                                }
                                Err((source, error)) => {
                                    program.settle(ProgramStatus::Error);
                                    ui.restore_editor(source);
                                    ui.status = format!("Shell was not started: {error:#}");
                                }
                            }
                        },
                        Action::CopyDetail => {
                            copy_detail_evidence(&mut ui, terminal).await?;
                        },
                        Action::Command(command) => {
                            dispatch_command(
                                &mut runtime,
                                &init.services,
                                &mut ui,
                                terminal,
                                screen,
                                &mut program,
                                command,
                            )
                            .await?;
                        },
                        Action::PasteClipboard => start_clipboard_paste(&mut ui, runtime.agent()),
                        Action::Pick(value) => apply_picker(&mut runtime, &init.services, &mut ui, terminal, screen, &mut program, value).await?,
                    }
                    poll_control = true;
                },
                InputEvent::Paste(text) => {
                    if ui.question_owns_input() {
                        let action = ui
                            .question
                            .as_mut()
                            .expect("question owns input")
                            .paste(&text);
                        apply_question_action(&mut ui, questions.as_ref(), action, terminal, screen).await?;
                        poll_control = true;
                    } else if ui.input_target() != interaction::InputTarget::Details {
                        ui.insert(&text);
                    }
                },
                InputEvent::Rejected(error) => {
                    ui.editor.break_edit_focus();
                    ui.status = error.to_string();
                },
                InputEvent::Resize(_) => {
                    ui.editor.completion.invalidate_visibility();
                    ui.pickers.invalidate_visibility();
                    if let Some(question) = &mut ui.question {
                        question.invalidate_visibility();
                    }
                    screen.note_resize();
                },
                InputEvent::Mouse(mouse) => ui.mouse(mouse.kind()),
                InputEvent::ColorsChanged => {},
            }
        }
        Ok(())
    }.await;
    ui.themes.cancel_preview();
    ui.pickers.close();
    // shutdown closes admission, stops and joins both preparation and Core work.
    // Process every reliable return/settlement even when presentation has failed.
    if let Some(owner) = &application_owner {
        owner.request_shutdown();
    }
    let report = runtime.shutdown().await;
    if let Some(owner) = &application_owner
        && let Err(error) = owner.join_shutdown().await
    {
        activity::combine_error(&mut outcome, error);
    }
    for update in report.updates {
        activity::cleanup_update(&mut ui, &runtime, update, &mut outcome);
    }
    for error in report.cleanup_errors {
        activity::combine_error(&mut outcome, error.into());
    }
    for cleanup in [
        cancel_clipboard_paste(&mut ui).await,
        ui.pickers.shutdown().await,
    ] {
        if let Err(error) = cleanup {
            activity::combine_error(&mut outcome, error);
        }
    }
    if let Some(terminal) = terminal_owner.as_mut() {
        let cleanup: Result<()> = async {
            if let Some(screen) = screen_owner.as_mut()
                && outcome.is_ok()
            {
                // Publish pending output, but do not retain identity chrome
                // after the interactive client relinquishes its surface.
                draw(terminal, screen, &mut ui, None, None).await?;
                release_surface(terminal, screen).await?;
            }
            Ok(())
        }
        .await;
        if let Err(error) = cleanup {
            activity::combine_error(&mut outcome, error);
        }
        if let Err(error) = terminal.restore() {
            activity::combine_error(&mut outcome, error.into());
        }
    }
    outcome
}

// Shorten display labels only; selectors and current-Session matching use full IDs.
fn short_session_id(id: &str) -> &str {
    let end = id
        .char_indices()
        .nth(12)
        .map_or(id.len(), |(index, _)| index);
    &id[..end]
}

fn invalidate_resized_input(
    terminal: &TerminalSession,
    screen: &mut Screen,
    ui: &mut Frontend,
) -> Result<()> {
    // A key or paste can beat SIGWINCH in chat and before Session admission.
    let published = if terminal.is_alt_screen()? {
        screen.fullscreen_size()
    } else {
        screen.primary_size()
    };
    if terminal.size()? != published {
        ui.invalidate_interaction_paint();
        ui.editor.invalidate_geometry();
        screen.note_resize();
    }
    Ok(())
}

async fn release_surface(terminal: &mut TerminalSession, screen: &mut Screen) -> Result<()> {
    if terminal.is_alt_screen()? {
        terminal.leave_alt_screen()?;
        screen.invalidate();
    }
    // Closing must not archive picker chrome or erase an unqualified surface.
    // Allow the existing resize/query recovery to settle, but never hold exit
    // indefinitely when the terminal stops answering geometry queries.
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if terminal.prepare_inline(screen).await?.is_some()
                && terminal
                    .paint_inline(screen, |screen, out| screen.release_inline(out))?
                    .is_some()
            {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("terminal surface could not be qualified for cleanup")?
}

async fn new_inline_screen(terminal: &mut TerminalSession) -> Result<Screen> {
    if terminal.is_alt_screen()? {
        terminal.leave_alt_screen()?;
    }
    loop {
        if let Some(((width, height), (cursor_column, cursor_row))) = terminal
            .primary_geometry()
            .await
            .context("read primary terminal geometry")?
        {
            anyhow::ensure!(
                cursor_column < width && cursor_row < height,
                "primary cursor reply is outside terminal geometry"
            );
            return Ok(Screen::with_live_height(width, cursor_row, height, 1));
        }
        tokio::task::yield_now().await;
    }
}

async fn login_in_terminal(
    terminal: &mut TerminalSession,
    screen: &mut Screen,
    credentials: &CredentialStore,
    provider: &str,
    program: &mut ProgramStatusState,
) -> Result<Result<()>> {
    anyhow::ensure!(!provider.is_empty(), "use /login PROVIDER");
    terminal.release_inline(screen).await?;
    // Command admission allows credential entry only while foreground control is idle.
    program.blocked_auth = true;
    program.publish(terminal, false)?;
    let entry = terminal
        .read_secret(&format!("{provider} API key: "))
        .await
        .context("read login credential")?;
    let result = entry
        .map_err(anyhow::Error::new)
        .context("read login credential")
        .and_then(|key| credentials.save_api_key(provider, &key));
    let restored = new_inline_screen(terminal).await;
    program.blocked_auth = false;
    program.publish(terminal, false)?;
    *screen = restored.context("restore inline chat after login")?;
    Ok(result)
}

async fn copy_last_answer(
    session: &Session,
    terminal: &mut TerminalSession,
) -> Result<crate::clipboard::CopyOutcome> {
    let answer = last_committed_answer(&session.view()?)?;
    crate::clipboard::copy(&answer, terminal).await
}

async fn copy_detail_evidence(ui: &mut Frontend, terminal: &mut TerminalSession) -> Result<()> {
    let Some((number, evidence)) = ui.details.as_ref().and_then(DetailView::copy_evidence) else {
        ui.status = "No selected tool evidence; open /tool N to copy".into();
        return Ok(());
    };
    let outcome = crate::clipboard::copy(evidence, terminal).await;
    ui.status = match outcome {
        Ok(crate::clipboard::CopyOutcome::Copied) => {
            format!("Copied tool {number} evidence")
        }
        Ok(crate::clipboard::CopyOutcome::RequestedFromTerminal) => {
            format!("Sent tool {number} clipboard request to terminal")
        }
        Err(error) => crate::clipboard::failure_status(error)?,
    };
    Ok(())
}

fn last_committed_answer(view: &SessionView) -> Result<String> {
    let completed: HashSet<u64> = view
        .entries
        .iter()
        .filter_map(|entry| match entry {
            SessionEntry::TurnEnded {
                turn,
                reason: TurnEndReason::Completed,
            } => Some(*turn),
            _ => None,
        })
        .collect();
    let selected: HashSet<_> = view.context_path.iter().copied().collect();
    view.entries
        .iter()
        .enumerate()
        .rev()
        .filter(|(offset, _)| selected.contains(&(*offset as u64 + 1)))
        .find_map(|(_, entry)| match entry {
            SessionEntry::Assistant { turn, message, .. } if completed.contains(turn) => {
                let text = message
                    .content
                    .iter()
                    .filter_map(|part| match part {
                        Content::Text(text) => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                (!text.trim().is_empty()).then_some(text)
            }
            _ => None,
        })
        .context("no completed assistant answer to copy")
}

// The outer result owns terminal lifecycle; only the editor's own failure is
// recoverable. Never resume admission after losing input or physical output.
async fn edit_text_in_terminal(
    terminal: &mut TerminalSession,
    screen: &mut Screen,
    text: &str,
    max_bytes: usize,
) -> Result<Result<String>> {
    terminal.suspend().context("suspend terminal for editor")?;
    let edited = crate::external_editor::edit(text, max_bytes).await;
    terminal
        .resume()
        .await
        .context("resume terminal after editor")?;
    *screen = new_inline_screen(terminal)
        .await
        .context("restore inline chat after editor")?;
    Ok(edited)
}

fn context_label(view: &ion_core::SessionView, window: Option<u32>) -> String {
    let mut label = match (view.last_usage.and_then(|usage| usage.input_tokens), window) {
        (Some(input), Some(window)) => format!("input {input}/{window} tokens"),
        (Some(input), None) => format!("input {input} tokens"),
        (None, Some(window)) => format!("context ≤{window} tokens · usage unknown"),
        (None, None) => "usage unknown".into(),
    };
    if let Some(through) = view.compacted_through {
        label.push_str(&format!(" · summary through {through}"));
    }
    label
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResumeHistoryTail {
    start: usize,
    omitted_turns: usize,
    omitted_entries: usize,
}

fn transcript_item_turn(item: &TranscriptItem) -> Option<u64> {
    match item {
        TranscriptItem::User(message) | TranscriptItem::Assistant(message) => message.turn,
        TranscriptItem::ActivityGroup(group) => Some(group.turn),
        TranscriptItem::Readiness { turn, .. } => Some(*turn),
        TranscriptItem::UserShell(_) => None,
    }
}

fn resume_history_tail(items: &[TranscriptItem]) -> ResumeHistoryTail {
    let mut turns = Vec::new();
    for item in items {
        if let Some(turn) = transcript_item_turn(item)
            && turns.last().copied() != Some(turn)
        {
            turns.push(turn);
        }
    }

    if turns.len() > RESUME_TURN_LIMIT {
        let first_turn = turns[turns.len() - RESUME_TURN_LIMIT];
        let start = items
            .iter()
            .position(|item| transcript_item_turn(item) == Some(first_turn))
            .unwrap_or(0);
        return ResumeHistoryTail {
            start,
            omitted_turns: turns.len() - RESUME_TURN_LIMIT,
            omitted_entries: start,
        };
    }

    if turns.is_empty() && items.len() > RESUME_ENTRY_LIMIT_WITHOUT_TURNS {
        let start = items.len() - RESUME_ENTRY_LIMIT_WITHOUT_TURNS;
        return ResumeHistoryTail {
            start,
            omitted_turns: 0,
            omitted_entries: start,
        };
    }

    ResumeHistoryTail {
        start: 0,
        omitted_turns: 0,
        omitted_entries: 0,
    }
}

enum Action {
    None,
    Submit(EditorDraft),
    Shell {
        command: String,
        exclude_from_context: bool,
        source: EditorDraft,
    },
    Queue(EditorDraft),
    Recover,
    PasteClipboard,
    CopyDetail,
    Command(CommandInput),
    Pick(PickerValue),
    Quit,
}

impl Frontend {
    fn sync_question(&mut self, responder: Option<&QuestionResponder>) {
        let Some(responder) = responder else {
            return;
        };
        match responder.pending() {
            Ok(Some(pending)) => {
                if self
                    .question
                    .as_ref()
                    .is_none_or(|question| question.request() != pending.request)
                {
                    self.question = Some(QuestionModal::new(pending.request, pending.questions));
                    self.editor.completion.clear();
                    self.editor.break_edit_focus();
                }
            }
            Ok(None) => {
                if self.question.take().is_some() {
                    self.status = "Question closed".into();
                    self.editor.break_edit_focus();
                }
            }
            Err(_) => {
                self.question = None;
            }
        }
    }

    fn question_owns_input(&self) -> bool {
        self.input_target() == interaction::InputTarget::Question
    }

    fn question_visible(&self) -> bool {
        self.question_owns_input()
            && self
                .question
                .as_ref()
                .is_some_and(|question| !question.is_collapsed())
    }

    fn attach_image(&mut self, image: ImageSource) {
        self.editor.attach_image(image);
        self.status = format!(
            "{} image(s) attached to the next prompt",
            self.editor.images.len()
        );
    }

    fn refresh_session(&mut self, session: &Session) -> Result<()> {
        self.editor.completion.clear();
        self.pickers.close();
        let view = session.view()?;
        self.load_history(session, &view);
        self.session_label = view
            .name
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| format!("Session {}", &view.id.to_string()[..8]));
        self.details = None;
        self.view.follow_end();
        self.cwd = session
            .cwd()
            .context("terminal coding requires a working directory")?
            .to_path_buf();
        if view.unfinished_turn.is_some() || view.unfinished_user_shell.is_some() {
            self.status = "Previous operation interrupted; effects may be unknown".into();
        }
        Ok(())
    }

    fn load_history(&mut self, session: &Session, view: &SessionView) {
        let change = self.conversation.reconcile(session, view);
        let same_session = change != conversation::Change::Session;
        if !same_session {
            self.editor.reset_edit_baseline();
        }
        if let Some(details) = &mut self.details {
            details.invalidate();
        }
        let prompt_history = view
            .entries
            .iter()
            .filter_map(|entry| match entry {
                SessionEntry::TurnStarted { input, .. } => {
                    input.content.iter().find_map(|part| match part {
                        Content::Text(text) => Some(text.clone()),
                        _ => None,
                    })
                }
                SessionEntry::Steering { input, .. } => {
                    input.content.iter().find_map(|part| match part {
                        Content::Text(text) => Some(text.clone()),
                        _ => None,
                    })
                }
                _ => None,
            })
            .collect();
        self.editor
            .replace_prompt_history(prompt_history, same_session);
    }

    fn pending_history_rows(&mut self, width: usize) -> Vec<Line<'static>> {
        let mut rows = Vec::new();
        if let Some(banner) = self.conversation.banner() {
            rows.push(Line::default());
            rows.extend(notice_rows(banner, width));
            rows.push(Line::default());
        }
        let content = self.presenter.publication(
            &self.conversation,
            width.saturating_sub(1).max(1),
            &crate::tool_output::RenderOptions::new(
                self.output_detail,
                &self.keys,
                self.themes.theme(),
                self.thinking_visibility,
            ),
        );
        rows.extend(content);
        rows
    }

    fn print(&mut self, message: String) {
        self.conversation.note(message, false);
        self.view.follow_end();
    }

    fn note(&mut self, message: String) {
        self.conversation.note(message, true);
        self.view.follow_end();
    }

    fn refresh_completion(
        &mut self,
        resources: Option<&Resources>,
        commands: Option<&ion_host::CommandCatalog>,
        force: bool,
    ) {
        if self.details.is_some() || self.pickers.active().is_some() || self.question_owns_input() {
            self.editor.completion.clear();
            self.pickers.cancel_files();
            return;
        }
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let scope = self.editor.completion.file_scope(
            &self.editor.draft,
            self.editor.cursor,
            &self.cwd,
            home.as_deref(),
            force,
        );
        self.editor.completion.refresh(
            &self.editor.draft,
            self.editor.cursor,
            resources,
            commands,
            scope.as_ref().and_then(|scope| self.pickers.files(scope)),
            force,
        );
        if self.editor.completion.wants_files()
            && let Some(scope) = scope
        {
            self.pickers.request_files(&scope);
        } else {
            self.pickers.cancel_files();
        }
    }

    fn key(
        &mut self,
        key: KeyEvent,
        resources: Option<&Resources>,
        commands: Option<&ion_host::CommandCatalog>,
    ) -> Action {
        if self.input_target() == interaction::InputTarget::Details {
            self.invalidate_interaction_paint();
            self.editor.break_edit_focus();
            self.editor.completion.clear();
            return self.detail_key(key);
        }
        // The real input pump dispatches question actions to Host. Direct
        // frontend callers must never fall through into composer editing.
        if self.input_target() == interaction::InputTarget::Question {
            return Action::None;
        }
        if self.theme_picker.is_some()
            && matches!(
                self.keys.action(crate::keybindings::PICKER, key),
                Some(KeyAction::Cancel | KeyAction::Interrupt)
            )
        {
            self.cancel_theme_picker();
            return Action::None;
        }
        if self.pickers.active().is_some() {
            self.editor.break_edit_focus();
            self.editor.completion.clear();
            return self
                .pickers
                .key(key, &self.keys)
                .map_or(Action::None, Action::Pick);
        }
        let input = self.keys.action(COMPOSER, key);
        let completion_key = self.keys.action(COMPLETION, key);
        self.refresh_completion(resources, commands, input == Some(KeyAction::Tab));
        if (self.editor.completion.active() || self.editor.completion.wants_files())
            && completion_key == Some(KeyAction::Cancel)
        {
            self.editor.break_edit_focus();
            self.editor.completion.dismiss();
            self.pickers.cancel_files();
            return Action::None;
        }
        if self.editor.completion.visible()
            || (self.editor.completion.active() && completion_key == Some(KeyAction::Tab))
        {
            match completion_key {
                Some(KeyAction::SelectUp | KeyAction::SelectDown) => {
                    self.editor.break_edit_focus();
                    self.editor
                        .completion
                        .move_selection(completion_key == Some(KeyAction::SelectDown));
                    return Action::None;
                }
                Some(KeyAction::Tab | KeyAction::Confirm) => {
                    self.editor.break_edit_focus();
                    if !self.editor.completion.visible() {
                        return Action::None;
                    }
                    if let Some((range, text, cursor)) =
                        self.editor.completion.replacement(&self.editor.draft)
                    {
                        self.editor.replace_text(range, &text, cursor);
                    }
                    self.editor.completion.clear();
                    return Action::None;
                }
                _ => {}
            }
        }
        self.editor.prepare_edit_key(key, &self.keys);
        // Preparing clipboard attachments never acknowledges a future prompt.
        if self.clipboard_job.is_some()
            && matches!(input, Some(KeyAction::Submit | KeyAction::FollowUp))
            && Builtin::parse(self.editor.draft.split_whitespace().next().unwrap_or("")).is_none()
        {
            self.status = "Wait for clipboard paste, then send the prompt".into();
            return Action::None;
        }
        if input == Some(KeyAction::Interrupt)
            && let Some(stop) = self.clipboard_job.as_ref().map(|job| &job.stop)
            && !stop.is_cancelled()
        {
            stop.cancel();
            self.status = "Cancelling input preparation…".into();
            return Action::None;
        }
        if self.editor.undo_key(key, &self.keys) || self.editor.yank_keys(key, &self.keys) {
            self.refresh_completion(resources, commands, false);
            return Action::None;
        }
        if input.is_some_and(|action| self.editor.apply(action)) {
            self.refresh_completion(resources, commands, false);
            return Action::None;
        }
        let action = match input {
            Some(KeyAction::Interrupt) => {
                if self.editor.draft.is_empty() {
                    Action::Quit
                } else {
                    self.editor.clear_draft_text();
                    Action::None
                }
            }
            Some(KeyAction::Exit) if self.editor.draft.is_empty() => Action::Quit,
            Some(KeyAction::Tab) => {
                if !self.editor.completion.wants_files() {
                    self.insert("\t");
                }
                Action::None
            }
            Some(KeyAction::Detail) => {
                self.open_details(None);
                Action::None
            }
            Some(KeyAction::Copy) => Action::Command(CommandInput::Shortcut(Builtin::Copy)),
            Some(KeyAction::External) => Action::Command(CommandInput::Shortcut(Builtin::Editor)),
            Some(KeyAction::Model) => Action::Command(CommandInput::Shortcut(Builtin::Model)),
            Some(KeyAction::ModelForward) => Action::Command(CommandInput::CycleModel(
                ion_host::ModelCycleDirection::Forward,
            )),
            Some(KeyAction::ModelBackward) => Action::Command(CommandInput::CycleModel(
                ion_host::ModelCycleDirection::Backward,
            )),
            Some(KeyAction::Paste) => Action::PasteClipboard,
            Some(KeyAction::NewLine) => {
                self.insert("\n");
                Action::None
            }
            Some(KeyAction::FollowUp) => {
                if self.editor.draft.trim().is_empty()
                    && self.editor.images.is_empty()
                    && self.editor.files.is_empty()
                {
                    Action::None
                } else {
                    Action::Queue(self.editor.take_literal())
                }
            }
            Some(KeyAction::Submit) => {
                let syntax = self.editor.draft.trim();
                let invalid_shell = if let Some(command) = syntax.strip_prefix("!!") {
                    command
                        .trim()
                        .is_empty()
                        .then_some("Type a shell command after !!")
                } else if let Some(command) = syntax.strip_prefix('!') {
                    command
                        .trim()
                        .is_empty()
                        .then_some("Type a shell command after !")
                } else {
                    None
                };
                if syntax.is_empty()
                    && self.editor.images.is_empty()
                    && self.editor.files.is_empty()
                {
                    Action::None
                } else if let Some(status) = invalid_shell {
                    self.status = status.into();
                    Action::None
                } else {
                    let EditorDraft {
                        literal: prompt,
                        cursor,
                    } = self.editor.take_literal();
                    let syntax = prompt.trim();
                    if Builtin::parse(syntax) == Some(Builtin::Quit) {
                        Action::Quit
                    } else if syntax.starts_with('/') {
                        Action::Command(CommandInput::Editor(EditorDraft {
                            literal: prompt,
                            cursor,
                        }))
                    } else if let Some(command) = syntax.strip_prefix("!!") {
                        Action::Shell {
                            command: command.trim().to_owned(),
                            exclude_from_context: true,
                            source: EditorDraft {
                                literal: prompt,
                                cursor,
                            },
                        }
                    } else if let Some(command) = syntax.strip_prefix('!') {
                        Action::Shell {
                            command: command.trim().to_owned(),
                            exclude_from_context: false,
                            source: EditorDraft {
                                literal: prompt,
                                cursor,
                            },
                        }
                    } else {
                        Action::Submit(EditorDraft {
                            literal: prompt,
                            cursor,
                        })
                    }
                }
            }
            Some(KeyAction::Dequeue) => Action::Recover,
            Some(KeyAction::CursorUp | KeyAction::CursorDown) => {
                self.editor
                    .move_vertical(input == Some(KeyAction::CursorDown));
                Action::None
            }
            Some(KeyAction::PageUp) => {
                self.view.scroll(-10);
                Action::None
            }
            Some(KeyAction::PageDown) => {
                self.view.scroll(10);
                Action::None
            }
            None => {
                if let Some(ch) = keybindings::printable(key) {
                    let mut bytes = [0; 4];
                    self.editor.insert_typed(ch.encode_utf8(&mut bytes));
                }
                Action::None
            }
            _ => Action::None,
        };
        self.refresh_completion(resources, commands, false);
        action
    }

    fn mouse(&mut self, kind: MouseKind) {
        let down = match kind {
            MouseKind::ScrollUp => false,
            MouseKind::ScrollDown => true,
            _ => return,
        };
        self.invalidate_interaction_paint();
        match self.input_target() {
            interaction::InputTarget::Details => {
                let view = self.details.as_mut().expect("derived inspector focus");
                view.scroll = if down {
                    view.scroll.saturating_sub(3)
                } else {
                    view.scroll.saturating_add(3)
                };
            }
            interaction::InputTarget::Picker => self.pickers.move_selection(down),
            interaction::InputTarget::Question => self.view.scroll(if down { 3 } else { -3 }),
            interaction::InputTarget::Composer => {
                self.editor.break_edit_focus();
                self.view.scroll(if down { 3 } else { -3 });
            }
        }
    }

    fn detail_key(&mut self, key: KeyEvent) -> Action {
        match self.keys.action(DETAILS, key) {
            Some(KeyAction::Cancel | KeyAction::Detail) => {
                self.details = None;
                self.status = "Details closed".into();
            }
            Some(KeyAction::Copy) => {
                if self
                    .details
                    .as_ref()
                    .and_then(DetailView::copy_evidence)
                    .is_some()
                {
                    return Action::CopyDetail;
                }
                self.status = "No selected tool evidence; open /tool N to copy".into();
            }
            Some(action) => {
                if let Some(view) = &mut self.details {
                    view.navigate(action);
                }
            }
            None => {}
        }
        Action::None
    }

    fn list_tools(&mut self) {
        let names = tools(self.conversation.items())
            .enumerate()
            .map(|(index, activity)| {
                format!(
                    "{}:{} ({})",
                    index + 1,
                    activity.name,
                    kind_label(activity.activity.kind)
                )
            })
            .collect::<Vec<_>>();
        self.print(if names.is_empty() {
            "No tools in this session".into()
        } else {
            format!("Tools: {}", names.join(" · "))
        });
    }

    fn open_details(&mut self, number: Option<usize>) {
        let selected = match number {
            None => None,
            Some(index) => {
                let Some(number) = std::num::NonZeroUsize::new(index) else {
                    self.status = "Tool result not found; use /tools to list tools".into();
                    return;
                };
                if tools(self.conversation.items()).nth(index - 1).is_none() {
                    self.status = "Tool result not found; use /tools to list tools".into();
                    return;
                }
                Some(number)
            }
        };
        self.details = Some(DetailView::new(selected));
    }
}

async fn prepare_draw(
    terminal: &mut TerminalSession,
    screen: &mut Screen,
    ui: &Frontend,
) -> Result<Option<(u16, u16, bool)>> {
    if ui.input_target() == interaction::InputTarget::Details || ui.mode == TuiMode::Fullscreen {
        terminal.enter_alt_screen(screen).await?;
        let (width, height) = terminal.size()?;
        screen.resize_fullscreen(width, height);
        return Ok(Some((width, height, false)));
    }
    let surface_reset = terminal.is_alt_screen()?;
    if surface_reset {
        // Restore first: the alternate cursor is not evidence about primary
        // ownership. Reconcile the parked, cleared primary tail before repaint.
        terminal.leave_alt_screen()?;
        screen.invalidate();
    }
    let Some((width, height)) = terminal.prepare_inline(screen).await? else {
        return Ok(None);
    };
    Ok(Some((width, height, surface_reset)))
}

async fn draw(
    terminal: &mut TerminalSession,
    screen: &mut Screen,
    ui: &mut Frontend,
    operation: Option<ActiveOperation>,
    model: Option<&ModelRef>,
) -> Result<bool> {
    if terminal.colors_need_refresh() {
        terminal.refresh_colors().await?;
    }
    ui.themes.observe(terminal.colors());
    ui.sync_theme_preview();
    // Layout is not publication: a deferred geometry reconciliation cannot
    // leave an old menu authorized as a keyboard target.
    ui.invalidate_interaction_paint();
    let Some(geometry) = prepare_draw(terminal, screen, ui).await? else {
        return Ok(false);
    };
    draw_prepared(terminal, screen, ui, operation, model, geometry)
}

fn draw_prepared(
    terminal: &mut TerminalSession,
    screen: &mut Screen,
    ui: &mut Frontend,
    operation: Option<ActiveOperation>,
    model: Option<&ModelRef>,
    (width, height, surface_reset): (u16, u16, bool),
) -> Result<bool> {
    if ui.input_target() == interaction::InputTarget::Details {
        return draw_modal_fullscreen(terminal, screen, ui, operation, width, height);
    }
    if ui.mode == TuiMode::Fullscreen {
        return draw_chat_fullscreen(terminal, screen, ui, operation, model, (width, height));
    }

    let publication = ui.conversation.prepare_publication();
    let commit_rows = ui.pending_history_rows(width.max(1) as usize);
    let history_committed = !commit_rows.is_empty();
    let width = width.max(1) as usize;
    let physical_budget = height.max(1) as usize;
    let dock = dock::Dock::prepare(ui, operation, model, width, physical_budget);
    let mut live_rows = ui.presenter.window(
        &ui.conversation,
        &mut ui.view,
        (width, dock.body_rows),
        true,
        operation.is_some(),
        &crate::tool_output::RenderOptions::new(
            ui.output_detail,
            &ui.keys,
            ui.themes.theme(),
            ui.thinking_visibility,
        ),
    );
    let body_height = live_rows.len();
    let cursor = dock.cursor.map(|(row, col)| (body_height + row, col));
    live_rows.extend(dock.rows);
    let frame = Frame {
        live: &live_rows,
        cursor,
    };
    let desired_live_height = frame.required_rows(width as u16).min(physical_budget);
    let painted = terminal.paint_inline(screen, |screen, out| {
        if history_committed {
            screen.commit_lines(out, &commit_rows)?;
        }
        if desired_live_height > screen.live_height() {
            screen.ensure_live_height(out, desired_live_height)?;
        } else if desired_live_height < screen.live_height() && (history_committed || surface_reset)
        {
            screen.set_live_height(desired_live_height);
        }
        screen.draw(out, &frame)
    })?;
    let Some(receipt) = painted else {
        return Ok(false);
    };
    dock.paint.observe(ui, receipt);
    assert!(
        ui.conversation.acknowledge(publication),
        "paint acknowledges its prepared source prefix"
    );
    Ok(true)
}

fn settlement_program_status(outcome: &ion_host::OperationOutcome) -> ProgramStatus {
    use ion_host::OperationOutcome;
    match outcome {
        OperationOutcome::Turn(Err(ion_core::AgentError::Cancelled))
        | OperationOutcome::Compact(Err(ion_core::AgentError::Cancelled)) => ProgramStatus::Idle,
        OperationOutcome::Shell(Ok(output)) if output.value["cancelled"] == true => {
            ProgramStatus::Idle
        }
        OperationOutcome::Shell(Ok(output)) if output.is_error => ProgramStatus::Error,
        OperationOutcome::Turn(Err(_))
        | OperationOutcome::Compact(Err(_))
        | OperationOutcome::Shell(Err(_)) => ProgramStatus::Error,
        _ => ProgramStatus::Done,
    }
}

fn visible_status(ui: &Frontend, operation: Option<ActiveOperation>) -> Option<String> {
    let editor_error = (ui.input_target() == interaction::InputTarget::Composer)
        .then(|| ui.editor.error.map(|error| error.to_string()))
        .flatten();
    let notice = editor_error.as_deref().unwrap_or(&ui.status);
    let idle = notice.is_empty();
    let mut attachment = if ui.editor.images.is_empty() {
        String::new()
    } else {
        format!(" · {} image(s) attached", ui.editor.images.len())
    };
    if !ui.editor.files.is_empty() {
        attachment.push_str(&format!(
            " · {} text file(s) attached",
            ui.editor.files.len()
        ));
    }
    let recovery = if ui.editor.recovered.is_empty() {
        String::new()
    } else {
        format!(
            " · {} recovered draft(s) · {} cycles",
            ui.editor.recovered.len(),
            ui.keys.hint(KeyAction::Dequeue)
        )
    };
    let queued = operation
        .filter(|operation| operation.pending_follow_ups != 0)
        .map_or_else(String::new, |operation| {
            format!(" · {} follow-up(s) queued", operation.pending_follow_ups)
        });
    let mut status = if idle {
        format!("{attachment}{recovery}{queued}")
            .trim_start_matches(" · ")
            .to_owned()
    } else {
        format!("{notice}{attachment}{recovery}{queued}")
    };
    if ui.question_owns_input() && !ui.question_visible() {
        let hint = QuestionModal::collapsed_hint(&ui.keys);
        if status.is_empty() {
            status = hint;
        } else {
            status.push_str(" · ");
            status.push_str(&hint);
        }
    }
    if let Some(operation) = operation {
        let label = operation.label();
        return Some(if operation.is_cancelled() {
            if status.is_empty() {
                label.to_owned()
            } else {
                format!("{label} · {status}")
            }
        } else if status.is_empty() {
            let controls = operation.controls(&ui.keys);
            format!("{controls} · {label}")
        } else {
            format!(
                "{} cancels · {label} · {status}",
                ui.keys.hint(KeyAction::Interrupt)
            )
        });
    }
    (!status.is_empty()).then_some(status)
}

fn draw_chat_fullscreen(
    terminal: &mut TerminalSession,
    screen: &mut Screen,
    ui: &mut Frontend,
    operation: Option<ActiveOperation>,
    model: Option<&ModelRef>,
    (width, height): (u16, u16),
) -> Result<bool> {
    let width = width.max(1) as usize;
    let height = height.max(1) as usize;
    let dock = dock::Dock::prepare(ui, operation, model, width, height);
    let content = ui.presenter.window(
        &ui.conversation,
        &mut ui.view,
        (width, dock.body_rows),
        false,
        operation.is_some(),
        &crate::tool_output::RenderOptions::new(
            ui.output_detail,
            &ui.keys,
            ui.themes.theme(),
            ui.thinking_visibility,
        ),
    );
    let mut rows = vec![Line::default(); height];
    let padding = dock.body_rows.saturating_sub(content.len());
    for (index, row) in content.into_iter().enumerate() {
        rows[padding + index] = row;
    }
    let cursor = dock.cursor.map(|(row, col)| (dock.body_rows + row, col));
    for (index, row) in dock.rows.into_iter().enumerate() {
        rows[dock.body_rows + index] = row;
    }

    let Some(receipt) = terminal.paint_fullscreen(screen, |screen, out| {
        screen.draw_fullscreen(out, &rows, cursor)
    })?
    else {
        return Ok(false);
    };
    dock.paint.observe(ui, receipt);
    Ok(true)
}
async fn apply_question_action(
    ui: &mut Frontend,
    responder: Option<&QuestionResponder>,
    action: QuestionAction,
    terminal: &mut TerminalSession,
    screen: &mut Screen,
) -> Result<()> {
    match action {
        QuestionAction::None => {}
        QuestionAction::Scroll { up } => {
            ui.view.scroll(if up { -10 } else { 10 });
        }
        QuestionAction::Edit {
            request,
            index,
            text,
        } => {
            let edited =
                edit_text_in_terminal(terminal, screen, &text, ion_host::MAX_ANSWER_BYTES).await?;
            // Core work runs independently. The question may have expired while
            // the native editor owned the terminal; never reuse its old lease.
            ui.sync_question(responder);
            if let Some(question) = &mut ui.question {
                question.finish_external_edit(request, index, edited);
            }
        }
        QuestionAction::Answer(request, answers) => {
            let Some(responder) = responder else {
                ui.question = None;
                ui.status = "Question responder is unavailable".into();
                return Ok(());
            };
            match responder.answer(request, answers) {
                Ok(()) => {
                    ui.question = None;
                    ui.status.clear();
                }
                Err(error) => {
                    if let Some(question) = &mut ui.question {
                        question.set_status(format!("Answer was not accepted: {error}"));
                    }
                }
            }
        }
        QuestionAction::Cancel(request) => {
            let Some(responder) = responder else {
                ui.question = None;
                ui.status = "Question responder is unavailable".into();
                return Ok(());
            };
            match responder.cancel(request) {
                Ok(()) => {
                    ui.question = None;
                    ui.status = "Question cancelled".into();
                }
                Err(error) => {
                    if let Some(question) = &mut ui.question {
                        question.set_status(format!("Question was not cancelled: {error}"));
                    }
                }
            }
        }
    }
    Ok(())
}

fn notice_rows(text: &str, width: usize) -> Vec<Line<'static>> {
    let mut rows = Vec::new();
    push_wrapped(&mut rows, text, width);
    rows.into_iter().map(Line::raw).collect()
}

fn draw_modal_fullscreen(
    terminal: &mut TerminalSession,
    screen: &mut Screen,
    ui: &mut Frontend,
    operation: Option<ActiveOperation>,
    width: u16,
    height: u16,
) -> Result<bool> {
    let (rows, cursor) = modal_rows(ui, operation, width, height);
    Ok(terminal
        .paint_fullscreen(screen, |screen, out| {
            screen.draw_fullscreen(out, &rows, cursor)
        })?
        .is_some())
}

fn modal_rows(
    ui: &mut Frontend,
    operation: Option<ActiveOperation>,
    width: u16,
    height: u16,
) -> (Vec<Line<'static>>, Option<(usize, u16)>) {
    ui.invalidate_interaction_paint();
    let width = width.max(1) as usize;
    let height = height.max(1) as usize;
    ui.details
        .as_mut()
        .expect("inspector owns viewport")
        .prepare(
            ui.conversation.items(),
            ui.conversation.live().map_or(&[], LiveTranscript::notices),
            ui.conversation.revision(),
            width,
        );
    let controls = ui.details.as_ref().map(|view| view.controls(&ui.keys));
    let status = visible_status(ui, operation);
    let status = if ui.question.is_some() {
        Some(status.map_or_else(
            || "Question waiting · close this view to answer".into(),
            |status| format!("{status} · Question waiting · close this view to answer"),
        ))
    } else {
        status
    };
    let status_height =
        (usize::from(controls.is_some()) + usize::from(status.is_some())).min(height);
    let viewport = height.saturating_sub(status_height);
    let content: Vec<_> = ui
        .details
        .as_ref()
        .expect("inspector owns viewport")
        .rows()
        .iter()
        .map(|row| Line::raw(fit_line(row, width)))
        .collect();
    let scroll = ui.details.as_ref().map_or(0, |view| view.scroll);
    let end = content.len().saturating_sub(scroll.min(content.len()));
    let start = end.saturating_sub(viewport);
    let mut rows = vec![Line::raw(""); height];
    let padding = viewport.saturating_sub(end - start);
    for (index, row) in content[start..end].iter().enumerate() {
        rows[padding + index] = row.clone();
    }

    let mut next_row = viewport;
    for label in controls.iter().chain(status.iter()).take(status_height) {
        if next_row < height {
            rows[next_row] = Line::styled(
                fit_line(label, width),
                ui.themes
                    .theme()
                    .style(crate::presentation_style::Role::Secondary),
            );
            next_row += 1;
        }
    }
    (rows, None)
}

#[cfg(test)]
fn painted_dock_rows(
    ui: &mut Frontend,
    operation: Option<ActiveOperation>,
    width: u16,
    height: u16,
) -> (Vec<Line<'static>>, Option<(usize, u16)>) {
    let dock = dock::Dock::prepare(
        ui,
        operation,
        None,
        usize::from(width.max(1)),
        usize::from(height.max(1)),
    );
    dock.paint.observe_simulated(
        ui,
        (width.max(1), height.max(1)),
        ui.mode == TuiMode::Fullscreen,
    );
    (dock.rows, dock.cursor)
}
#[cfg(test)]
mod interaction_tests;
#[cfg(test)]
mod keybinding_tests;
#[cfg(test)]
mod presentation_tests;

#[cfg(test)]
pub(crate) fn measured_window_fixture(
    items: &[TranscriptItem],
    width: usize,
    budget: usize,
    options: &crate::tool_output::RenderOptions<'_>,
) -> Vec<Line<'static>> {
    let conversation = conversation::fixtures::document(ion_core::TranscriptProjection {
        items: items.to_vec(),
    });
    block_presenter::BlockPresenter::default().window(
        &conversation,
        &mut block_presenter::ConversationView::default(),
        (width, budget),
        false,
        true,
        options,
    )
}
#[cfg(test)]
mod tests;
