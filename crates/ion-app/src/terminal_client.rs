//! Terminal view over the same coding loop used by headless and library hosts.
#[cfg(test)]
use ion_ai::Role;
#[cfg(test)]
use ion_terminal::{KeyCode, Modifiers};
#[cfg(test)]
use std::fs;
use std::{
    collections::{HashSet, VecDeque},
    path::PathBuf,
};

use crate::display_text::{fit_line, push_wrapped};
use crate::keybindings::{self, COMPLETION, COMPOSER, DETAILS, KeyAction, KeyBindings};
use crate::terminal_commands::Builtin;
use crate::terminal_completion::Completion;
use crate::transcript_detail::{DetailView, tools};
use crate::transcript_render::kind_label;
use anyhow::{Context, Result};
use ion_ai::{Content, Message, ModelRef};
use ion_core::{
    CodingSession, LiveTranscript, SessionEntry, SessionView, TranscriptItem, TranscriptProjection,
    TurnEndReason,
};
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
mod clipboard_input;
mod command;
mod composer;
mod configuration;
mod draft;
mod editing;
mod editor_recovery;
mod file_discovery;
mod interaction;
mod navigation;
mod orientation;
mod output;
mod picker;
mod question;
mod setup;
mod tasks;
use clipboard_input::*;
use command::{CommandInput, dispatch_command};
use composer::input_window;
use editing::{Edit, vertical_cursor};
use editor_recovery::EditorRecovery;
use interaction::{apply_picker, submit_editor};
use picker::{Picker, PickerValue, Pickers};
use question::{QuestionAction, QuestionModal};
pub use setup::{choose_session, setup_model};

pub(crate) const MAX_DRAFT: usize = 64 * 1024;
const LIVE_REGION_MAX_ROWS: usize = 12;
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

use activity::{ActiveOperation, Activity};

#[derive(Default)]
struct Frontend {
    keys: KeyBindings,
    mode: TuiMode,
    output_detail: crate::tool_output::OutputDetail,
    thinking_visibility: crate::transcript_render::ThinkingVisibility,
    history: TranscriptProjection,
    fullscreen_rows: usize,
    fullscreen_width: usize,
    draft: String,
    images: Vec<ImageSource>,
    files: Vec<ion_host::prompt_files::TextFile>,
    cursor: usize,
    recovery: EditorRecovery,
    history_session: Option<PathBuf>,
    history_selection: u64,
    history_published_items: usize,
    pending_history_target: usize,
    pending_history_banner: Option<String>,
    scroll: usize,
    status: String,
    output: output::Output,
    pickers: Pickers,
    completion: Completion,
    recovered: VecDeque<RecoveredDraft>,
    prompt_history: Vec<String>,
    browsing: Option<PromptBrowse>,
    details: Option<DetailView>,
    clipboard_job: Option<ClipboardJob>,
    question: Option<QuestionModal>,
    cwd: PathBuf,
    session_label: String,
}

struct PromptBrowse {
    index: usize,
    end: usize,
    draft: String,
    cursor: usize,
}

#[derive(Debug, serde::Serialize)]
struct EditorDraft {
    literal: String,
    cursor: usize,
}

impl EditorDraft {
    fn at_end(literal: String) -> Self {
        Self {
            cursor: literal.len(),
            literal,
        }
    }
}

// Returned inputs are UI recovery only, never an execution queue. Each item
// retains its own literal editor text, cursor and attachment associations.
struct RecoveredDraft {
    source: EditorDraft,
    images: Vec<ImageSource>,
    files: Vec<ion_host::prompt_files::TextFile>,
}

// Inverse of Message::user_input, not a lossy flattening of arbitrary multipart
// messages from embedded clients. Notes belong to the immediately following image.
fn split_editor_input(input: Message) -> Result<(String, Vec<LoadedImage>), &'static str> {
    if input.role != ion_ai::Role::User || input.provider_replay.is_some() {
        return Err("selected input cannot be represented in the editor");
    }
    let mut parts = input.content.into_iter();
    let Some(Content::Text(prompt)) = parts.next() else {
        return Err("selected input cannot be represented in the editor");
    };
    let mut images = Vec::new();
    while let Some(part) = parts.next() {
        match part {
            Content::Image(content) => images.push(LoadedImage {
                content,
                note: None,
            }),
            Content::Text(note) => {
                let Some(Content::Image(content)) = parts.next() else {
                    return Err("selected input cannot be represented in the editor");
                };
                images.push(LoadedImage {
                    content,
                    note: Some(note),
                });
            }
            Content::Thinking(_) | Content::ToolCall(_) | Content::ToolResult(_) => {
                return Err("selected input cannot be represented in the editor");
            }
        }
    }
    Ok((prompt, images))
}

pub struct ChatInit {
    pub services: std::sync::Arc<ion_host::Host>,
    pub setup: ion_host::SessionSetup,
    pub images: Vec<LoadedImage>,
    pub files: Vec<ion_host::prompt_files::TextFile>,
    pub startup_diagnostics: Vec<String>,
    pub initial_prompt: Option<String>,
    pub tui_mode: TuiMode,
    pub(crate) keys: KeyBindings,
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
        mode: init.tui_mode,
        keys: init.keys,
        images: init.images.into_iter().map(Into::into).collect(),
        files: init.files,
        ..Frontend::default()
    };
    let mut initial_prompt = init.initial_prompt;
    let mut activity = Activity::default();
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
        loop {
            terminal.check_active().context("terminal lifecycle failed")?;
            ui.sync_question(questions.as_ref());
            ui.refresh_completion(Some(runtime.resources()), Some(runtime.commands()), false);
            program.blocked_question = ui.question_owns_input();
            program.publish(terminal, runtime.is_busy())?;
            draw(terminal, screen, &mut ui, activity.live(), activity.view(&runtime), Some(runtime.agent().model())).await?;
            #[cfg(debug_assertions)]
            if std::env::var_os("ION_SMOKE_PANIC_AFTER_FIRST_DRAW").is_some() {
                panic!("ION smoke panic after first terminal draw");
            }
            // CLI text is a prompt, not a simulated editor key: never interpret
            // it as a terminal command or direct shell invocation. Attempt once
            // after terminal entry/first draw; normal Host rejection/return keeps
            // its literal source and attachments available in the editor.
            if let Some(prompt) = initial_prompt.take() {
                submit_editor(&mut runtime, &mut ui, Delivery::Start, EditorDraft::at_end(prompt));
                poll_control = true;
                continue;
            }
            let retry_inline = screen.reconciliation_pending() && !terminal.is_alt_screen()?;
            let readiness = runtime.completion_changes();
            let event = tokio::select! {
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
                    let _visible_settlement_error = activity.update(&mut ui, &runtime, update)?;
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
                _ = tokio::time::sleep(Duration::from_millis(50)), if retry_inline => continue,
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
            match event? {
                InputEvent::Key(key) => {
                    if ui.keys.matches(KeyAction::Suspend, key) {
                        ui.break_edit_focus();
                        release_inline_surface(terminal, screen).await?;
                        terminal.suspend_to_job_control().await.context("suspend terminal job")?;
                        program.published = None;
                        *screen = new_inline_screen(terminal).await.context("restore chat after job suspension")?;
                        poll_control = true;
                        continue;
                    }
                    if ui.keys.matches(KeyAction::Interrupt, key)
                        && (runtime.is_busy() || activity.view(&runtime).is_some())
                    {
                        ui.break_edit_focus();
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
                        apply_question_action(&mut ui, questions.as_ref(), action);
                        poll_control = true;
                        continue;
                    }
                    let cursor = ui.cursor;
                    match ui.key(
                        key,
                        Some(runtime.resources()),
                        Some(runtime.commands()),
                    ) {
                        Action::None => {},
                        Action::Quit => break,
                        Action::Submit(prompt) => {
                            let delivery = interaction::editor_delivery(&runtime);
                            submit_editor(&mut runtime, &mut ui, delivery, EditorDraft { literal: prompt, cursor });
                        },
                        Action::Queue(prompt) => {
                            submit_editor(&mut runtime, &mut ui, Delivery::FollowUp, EditorDraft { literal: prompt, cursor });
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
                        apply_question_action(&mut ui, questions.as_ref(), action);
                        poll_control = true;
                    } else {
                        ui.insert(&text);
                    }
                },
                InputEvent::Rejected(error) => {
                    ui.break_edit_focus();
                    ui.status = error.to_string();
                },
                InputEvent::Resize(_) => {
                    ui.completion.invalidate_visibility();
                    ui.pickers.invalidate_visibility();
                    screen.note_resize();
                },
                InputEvent::Mouse(mouse) => ui.mouse(mouse.kind()),
            }
        }
        Ok(())
    }.await;
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
        activity.cleanup_update(&mut ui, &runtime, update, &mut outcome);
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
                draw(terminal, screen, &mut ui, None, None, None).await?;
                if terminal.is_alt_screen()? {
                    terminal.leave_alt_screen()?;
                    screen.invalidate();
                }
                if terminal.prepare_inline(screen).await?.is_some() {
                    terminal.paint_inline(screen, |screen, out| screen.release_inline(out))?;
                }
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

// Yield only reconciled, unpublished primary cells before another terminal
// consumer writes at the cursor. In particular, a secret prompt must not be
// appended to an old editor line or leave stale identity chrome below it.
async fn release_inline_surface(terminal: &mut TerminalSession, screen: &mut Screen) -> Result<()> {
    terminal.leave_alt_screen()?;
    screen.invalidate();
    loop {
        if terminal.prepare_inline(screen).await?.is_some()
            && terminal.paint_inline(screen, |screen, out| screen.release_inline(out))?
        {
            return Ok(());
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
    release_inline_surface(terminal, screen).await?;
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
    session: &CodingSession,
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
async fn edit_draft_in_terminal(
    terminal: &mut TerminalSession,
    screen: &mut Screen,
    ui: &mut Frontend,
) -> Result<Result<()>> {
    terminal.suspend().context("suspend terminal for editor")?;
    let edited = crate::external_editor::edit(&ui.draft, MAX_DRAFT).await;
    terminal
        .resume()
        .await
        .context("resume terminal after editor")?;
    *screen = new_inline_screen(terminal)
        .await
        .context("restore inline chat after editor")?;
    Ok(edited.map(|edited| ui.replace_draft_atomic(edited)))
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
    Submit(String),
    Shell {
        command: String,
        exclude_from_context: bool,
        source: EditorDraft,
    },
    Queue(String),
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
                    self.completion.clear();
                    self.break_edit_focus();
                }
            }
            Ok(None) => {
                if self.question.take().is_some() {
                    self.status = "Question closed".into();
                    self.break_edit_focus();
                }
            }
            Err(_) => {
                self.question = None;
            }
        }
    }

    fn question_owns_input(&self) -> bool {
        self.question.is_some() && self.details.is_none() && self.pickers.active().is_none()
    }

    fn attach_image(&mut self, image: ImageSource) {
        self.reset_edit_baseline();
        self.images.push(image);
        self.status = format!("{} image(s) attached to the next prompt", self.images.len());
    }

    fn refresh_session(&mut self, session: &CodingSession) -> Result<()> {
        self.completion.clear();
        self.pickers.close();
        let view = session.view()?;
        self.load_history(session, &view);
        self.session_label = view
            .name
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| format!("Session {}", &view.id.to_string()[..8]));
        self.details = None;
        self.scroll = 0;
        self.cwd = session
            .cwd()
            .context("terminal coding requires a working directory")?
            .to_path_buf();
        if view.unfinished_turn.is_some() || view.unfinished_user_shell.is_some() {
            self.status = "Previous operation interrupted; effects may be unknown".into();
        }
        Ok(())
    }

    fn load_history(&mut self, session: &CodingSession, view: &SessionView) {
        let history = TranscriptProjection::from_session(view);
        let session_path = session.path().to_path_buf();
        let same_session = self.history_session.as_ref() == Some(&session_path);
        let selection = view
            .entries
            .iter()
            .rposition(|entry| matches!(entry, SessionEntry::ContextSelected { .. }))
            .map_or(0, |offset| offset as u64 + 1);
        let same_history = same_session && self.history_selection == selection;
        if !same_history {
            // Text recovery belongs to the unsent draft baseline, not the
            // Session's durable facts. Same-Session resync must preserve it.
            if !same_session {
                self.reset_edit_baseline();
            }
            self.output = output::Output::default();
            let had_previous_session = self.history_session.is_some();
            let label = session.id().to_string();
            let tail = resume_history_tail(&history.items);
            self.history_published_items = tail.start;
            self.pending_history_banner = if same_session {
                Some(format!(
                    "— selected context at entry {} · alternatives retained · files unchanged —",
                    view.context_position().head_entry
                ))
            } else if !history.items.is_empty() {
                Some(if tail.omitted_turns > 0 {
                    format!(
                        "— resumed session {label} · {} earlier turn(s) retained —",
                        tail.omitted_turns
                    )
                } else if tail.omitted_entries > 0 {
                    format!(
                        "— resumed session {label} · {} earlier entr{} retained —",
                        tail.omitted_entries,
                        if tail.omitted_entries == 1 {
                            "y"
                        } else {
                            "ies"
                        }
                    )
                } else {
                    format!("— resumed session {label} —")
                })
            } else if had_previous_session {
                Some(format!("— session {label} —"))
            } else {
                None
            };
        }
        self.pending_history_target = history.items.len();
        self.history_session = Some(session_path);
        self.history_selection = selection;
        self.history = history;
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
        self.replace_prompt_history(prompt_history, same_session);
        self.defer_unsettled_publication(view);
    }

    // Inspection may include a running next operation or interrupted work.
    // Recovery can still change those rows: keep them as mutable previews on
    // initial load as well as resync, never as anchors for local output.
    fn defer_unsettled_publication(&mut self, view: &SessionView) {
        let unfinished = view
            .unfinished_turn
            .and_then(|turn| {
                self.history
                    .items
                    .iter()
                    .position(|item| transcript_item_turn(item) == Some(turn))
            })
            .or_else(|| {
                view.unfinished_user_shell.as_ref().and_then(|_| {
                    self.history
                        .items
                        .iter()
                        .rposition(|item| matches!(item, TranscriptItem::UserShell(_)))
                })
            });
        if let Some(end) = unfinished {
            let end = end.max(self.history_published_items);
            self.pending_history_target = end;
        }
    }

    fn pending_history_rows(&self, width: usize) -> Vec<Line<'static>> {
        let mut rows = Vec::new();
        if let Some(banner) = &self.pending_history_banner {
            rows.push(Line::default());
            rows.extend(notice_rows(banner, width));
            rows.push(Line::default());
        }
        let content = self.output.rows(
            &self.history.items,
            self.history_published_items..self.pending_history_target,
            true,
            width.saturating_sub(1).max(1),
            &crate::tool_output::OutputOptions::new(self.output_detail, &self.keys),
            self.thinking_visibility,
        );
        if self.pending_history_banner.is_none()
            && self.history_published_items > 0
            && !content.is_empty()
        {
            rows.push(Line::default());
        }
        rows.extend(content);
        rows
    }

    fn finish_history_commit(&mut self) {
        self.history_published_items = self.pending_history_target;
        self.output.finish_commit(self.pending_history_target);
        self.pending_history_banner = None;
    }

    fn print(&mut self, message: String) {
        self.output
            .push(message, self.pending_history_target, false);
        self.scroll = 0;
    }

    fn note(&mut self, message: String) {
        self.output.push(message, self.pending_history_target, true);
        self.scroll = 0;
    }

    fn refresh_completion(
        &mut self,
        resources: Option<&Resources>,
        commands: Option<&ion_host::CommandCatalog>,
        force: bool,
    ) {
        if self.details.is_some() || self.pickers.active().is_some() || self.question_owns_input() {
            self.completion.clear();
            self.pickers.cancel_files();
            return;
        }
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let scope =
            self.completion
                .file_scope(&self.draft, self.cursor, &self.cwd, home.as_deref(), force);
        self.completion.refresh(
            &self.draft,
            self.cursor,
            resources,
            commands,
            scope.as_ref().and_then(|scope| self.pickers.files(scope)),
            force,
        );
        if self.completion.wants_files()
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
        if self.details.is_some() {
            self.break_edit_focus();
            self.completion.clear();
            return self.detail_key(key);
        }
        if self.pickers.active().is_some() {
            self.break_edit_focus();
            self.completion.clear();
            return self
                .pickers
                .key(key, &self.keys)
                .map_or(Action::None, Action::Pick);
        }
        let input = self.keys.action(COMPOSER, key);
        let completion_key = self.keys.action(COMPLETION, key);
        let menu_was_visible = self.completion.visible();
        self.refresh_completion(resources, commands, input == Some(KeyAction::Tab));
        if (self.completion.active() || self.completion.wants_files())
            && completion_key == Some(KeyAction::Cancel)
        {
            self.break_edit_focus();
            self.completion.dismiss();
            self.pickers.cancel_files();
            return Action::None;
        }
        if self.completion.visible()
            || (self.completion.active() && completion_key == Some(KeyAction::Tab))
        {
            match completion_key {
                Some(KeyAction::SelectUp | KeyAction::SelectDown) => {
                    self.break_edit_focus();
                    self.completion
                        .move_selection(completion_key == Some(KeyAction::SelectDown));
                    return Action::None;
                }
                Some(KeyAction::Tab | KeyAction::Confirm) => {
                    self.break_edit_focus();
                    if completion_key == Some(KeyAction::Tab)
                        && !menu_was_visible
                        && !self.completion.unique()
                    {
                        return Action::None;
                    }
                    if let Some((range, text, cursor)) = self.completion.replacement(&self.draft) {
                        self.replace_text(range, &text, cursor);
                    }
                    self.completion.clear();
                    return Action::None;
                }
                _ => {}
            }
        }
        self.prepare_edit_key(key);
        // Preparing clipboard attachments never acknowledges a future prompt.
        if self.clipboard_job.is_some()
            && matches!(input, Some(KeyAction::Submit | KeyAction::FollowUp))
            && Builtin::parse(self.draft.split_whitespace().next().unwrap_or("")).is_none()
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
        if self.undo_key(key) || self.yank_keys(key) {
            self.refresh_completion(resources, commands, false);
            return Action::None;
        }
        if let Some(edit) =
            input.and_then(|action| editing::apply(&self.draft, self.cursor, action))
        {
            match edit {
                Edit::Move(cursor) => {
                    self.break_edit_focus();
                    self.cursor = cursor;
                }
                Edit::Delete { range, kill } => self.delete_text(range, kill),
            }
            self.refresh_completion(resources, commands, false);
            return Action::None;
        }
        let action = match input {
            Some(KeyAction::Interrupt) => {
                if self.draft.is_empty() {
                    Action::Quit
                } else {
                    self.clear_draft_text();
                    Action::None
                }
            }
            Some(KeyAction::Exit) if self.draft.is_empty() => Action::Quit,
            Some(KeyAction::Tab) => {
                if !self.completion.wants_files() {
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
            Some(KeyAction::Paste) => Action::PasteClipboard,
            Some(KeyAction::NewLine) => {
                self.insert("\n");
                Action::None
            }
            Some(KeyAction::FollowUp) => {
                if self.draft.trim().is_empty() && self.images.is_empty() && self.files.is_empty() {
                    Action::None
                } else {
                    self.begin_draft_handoff();
                    self.browsing = None;
                    let prompt = std::mem::take(&mut self.draft);
                    self.cursor = 0;
                    Action::Queue(prompt)
                }
            }
            Some(KeyAction::Submit) => {
                let syntax = self.draft.trim();
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
                if syntax.is_empty() && self.images.is_empty() && self.files.is_empty() {
                    Action::None
                } else if let Some(status) = invalid_shell {
                    self.status = status.into();
                    Action::None
                } else {
                    self.begin_draft_handoff();
                    self.browsing = None;
                    let cursor = self.cursor;
                    let prompt = std::mem::take(&mut self.draft);
                    self.cursor = 0;
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
                        Action::Submit(prompt)
                    }
                }
            }
            Some(KeyAction::Dequeue) => Action::Recover,
            Some(KeyAction::CursorUp) => {
                if let Some(cursor) = vertical_cursor(&self.draft, self.cursor, false) {
                    self.cursor = cursor;
                } else {
                    self.history_previous();
                }
                Action::None
            }
            Some(KeyAction::CursorDown) => {
                if let Some(cursor) = vertical_cursor(&self.draft, self.cursor, true) {
                    self.cursor = cursor;
                } else {
                    self.history_next();
                }
                Action::None
            }
            Some(KeyAction::PageUp) => {
                self.scroll = self.scroll.saturating_add(10);
                Action::None
            }
            Some(KeyAction::PageDown) => {
                self.scroll = self.scroll.saturating_sub(10);
                Action::None
            }
            None => {
                if let Some(ch) = keybindings::printable(key) {
                    let mut bytes = [0; 4];
                    self.insert_typed(ch.encode_utf8(&mut bytes));
                }
                Action::None
            }
            _ => Action::None,
        };
        self.refresh_completion(resources, commands, false);
        action
    }

    fn mouse(&mut self, kind: MouseKind) {
        // Scrolling is a user viewport action: it ends direct yank-pop and
        // typed-word affinity while retaining draft text and undo history.
        if matches!(kind, MouseKind::ScrollUp | MouseKind::ScrollDown) {
            self.break_edit_focus();
        }
        match kind {
            MouseKind::ScrollUp => self.scroll = self.scroll.saturating_add(3),
            MouseKind::ScrollDown => self.scroll = self.scroll.saturating_sub(3),
            _ => {}
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
        let names = tools(&self.history)
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
                if tools(&self.history).nth(index - 1).is_none() {
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
    if ui.question_owns_input()
        || ui.details.is_some()
        || ui.pickers.active().is_some()
        || ui.mode == TuiMode::Fullscreen
    {
        terminal.enter_alt_screen()?;
        let (width, height) = terminal.size()?;
        screen.resize_fullscreen(width, height);
        return Ok(Some((width, height, false)));
    }
    let surface_reset = terminal.is_alt_screen()?;
    if surface_reset {
        // Restore first: the alternate cursor is not evidence about primary
        // ownership. Keep the previous primary cells through reconciliation.
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
    progress: Option<&LiveTranscript>,
    operation: Option<ActiveOperation>,
    model: Option<&ModelRef>,
) -> Result<()> {
    // Layout is not publication: a deferred geometry reconciliation cannot
    // leave an old menu authorized as a keyboard target.
    ui.completion.invalidate_visibility();
    ui.pickers.invalidate_visibility();
    let Some(geometry) = prepare_draw(terminal, screen, ui).await? else {
        return Ok(());
    };
    draw_prepared(terminal, screen, ui, progress, operation, model, geometry)
}

fn draw_prepared(
    terminal: &mut TerminalSession,
    screen: &mut Screen,
    ui: &mut Frontend,
    progress: Option<&LiveTranscript>,
    operation: Option<ActiveOperation>,
    model: Option<&ModelRef>,
    (width, height, surface_reset): (u16, u16, bool),
) -> Result<()> {
    if ui.details.is_some() || ui.pickers.active().is_some() {
        return draw_modal_fullscreen(terminal, screen, ui, progress, operation, width, height);
    }
    if ui.question_owns_input() {
        return draw_question_fullscreen(terminal, screen, ui, width, height);
    }
    if ui.mode == TuiMode::Fullscreen {
        return draw_chat_fullscreen(
            terminal,
            screen,
            ui,
            progress,
            operation,
            model,
            (width, height),
        );
    }

    let commit_rows = ui.pending_history_rows(width.max(1) as usize);
    let history_committed = !commit_rows.is_empty();
    let width = width.max(1) as usize;
    let row_budget = LIVE_REGION_MAX_ROWS.min(height.max(1) as usize);
    let mut chrome = Vec::new();
    let notices = progress.map_or(&[][..], LiveTranscript::notices);
    if let Some(notice) = notices.last() {
        let count = notices.len();
        let label = if count == 1 {
            "Notice".into()
        } else {
            format!("{count} notices")
        };
        chrome.push(fit_line(&format!("{label} · {notice}"), width));
    }
    let status = visible_status(ui, operation);
    if let Some(status) = &status {
        if operation.is_some() {
            chrome.push(fit_line(status, width));
        } else {
            push_wrapped(&mut chrome, status, width);
        }
    }

    // Keep an editable cursor line first, then a status/control row when space
    // allows. Select the composer window only after reserving that real budget.
    let status_height = usize::from(status.is_some()).min(row_budget - 1);
    let composer = input_window(
        &ui.draft,
        ui.cursor,
        width,
        (row_budget - status_height).min(4),
    );
    let composer_height = composer.lines.len();
    let progress_height =
        (usize::from(operation.is_some()) * 2).min(row_budget - composer_height - status_height);
    let completions = ui.completion.rows(
        width,
        row_budget.saturating_sub(composer_height + progress_height + status_height),
        &ui.keys,
    );
    // Identity is lower priority than the editor, stop controls and discovery.
    let footer = orientation::rows(
        ui,
        model,
        width,
        row_budget
            .saturating_sub(composer_height + progress_height + completions.len() + chrome.len()),
    );
    let chrome_budget = row_budget
        .saturating_sub(composer_height + progress_height + completions.len() + footer.len());
    // Discovery is temporary composer chrome. Older notices must not hide a
    // keyboard-active menu or displace the operation's stop/status controls.
    if chrome.len() > chrome_budget {
        chrome.drain(..chrome.len() - chrome_budget);
    }
    chrome.extend(completions);
    let content_budget = row_budget.saturating_sub(composer_height + chrome.len() + footer.len());
    // Reopened or resynchronized unfinished facts stay visible as mutable
    // previews. They are not stable output anchors or irreversible scrollback.
    let live_items = progress.map_or_else(
        || &ui.history.items[ui.pending_history_target..],
        |progress| progress.projection().items.as_slice(),
    );
    let mut live_rows = crate::transcript_render::live_rows(
        live_items,
        width,
        content_budget,
        ui.thinking_visibility,
        &crate::tool_output::OutputOptions::new(
            crate::tool_output::OutputDetail::Compact,
            &ui.keys,
        ),
    );
    live_rows.extend(chrome.into_iter().map(Line::raw));
    let composer_offset = live_rows.len();
    for line in &composer.lines {
        live_rows.push(Line::raw(line.as_str()));
    }
    let cursor_row = composer_offset + composer.cursor_row;
    live_rows.extend(footer);

    let desired_live_height = live_rows.len().clamp(1, row_budget);
    let cursor = Some((
        cursor_row,
        composer.cursor_col.min(width.saturating_sub(1)) as u16,
    ));
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
        screen.draw(
            out,
            &Frame {
                live: &live_rows,
                cursor,
            },
        )
    })?;
    if painted {
        ui.finish_history_commit();
    } else {
        ui.completion.invalidate_visibility();
    }
    Ok(())
}

fn settlement_program_status(outcome: &ion_host::OperationOutcome) -> ProgramStatus {
    use ion_host::OperationOutcome;
    match outcome {
        OperationOutcome::Turn(Err(ion_core::CodingAgentError::Cancelled))
        | OperationOutcome::Compact(Err(ion_core::CodingAgentError::Cancelled)) => {
            ProgramStatus::Idle
        }
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
    let idle = ui.status.is_empty();
    let mut attachment = if ui.images.is_empty() {
        String::new()
    } else {
        format!(" · {} image(s) attached", ui.images.len())
    };
    if !ui.files.is_empty() {
        attachment.push_str(&format!(" · {} text file(s) attached", ui.files.len()));
    }
    let recovery = if ui.recovered.is_empty() {
        String::new()
    } else {
        format!(
            " · {} recovered draft(s) · {} cycles",
            ui.recovered.len(),
            ui.keys.hint(KeyAction::Dequeue)
        )
    };
    let queued = operation
        .filter(|operation| operation.pending_follow_ups != 0)
        .map_or_else(String::new, |operation| {
            format!(" · {} follow-up(s) queued", operation.pending_follow_ups)
        });
    let status = if idle {
        format!("{attachment}{recovery}{queued}")
            .trim_start_matches(" · ")
            .to_owned()
    } else {
        format!("{}{attachment}{recovery}{queued}", ui.status)
    };
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
            format!("{label} · {controls}")
        } else {
            format!(
                "{label} · {} cancels · {status}",
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
    progress: Option<&LiveTranscript>,
    operation: Option<ActiveOperation>,
    model: Option<&ModelRef>,
    (width, height): (u16, u16),
) -> Result<()> {
    let width = width.max(1) as usize;
    let height = height.max(1) as usize;
    let mut content = ui.output.rows(
        &ui.history.items,
        0..ui.history.items.len(),
        false,
        width,
        &crate::tool_output::OutputOptions::new(ui.output_detail, &ui.keys),
        ui.thinking_visibility,
    );
    if let Some(progress) = progress {
        let live = crate::transcript_render::rows(
            &progress.projection().items,
            width,
            &crate::tool_output::OutputOptions::new(ui.output_detail, &ui.keys),
            ui.thinking_visibility,
        );
        if !live.is_empty()
            && !content.is_empty()
            && content.last().is_some_and(|row| row.width() > 0)
        {
            content.push(Line::default());
        }
        content.extend(live);
        for notice in progress.notices() {
            content.extend(notice_rows(notice, width));
        }
    }

    // Keep a scrolled-up viewport visually stable while new streaming rows arrive.
    // Width changes can reflow the whole transcript, so start a new row-count
    // baseline rather than guessing how the old offset maps to the new wrapping.
    if ui.fullscreen_width == width && ui.scroll > 0 && content.len() > ui.fullscreen_rows {
        ui.scroll = ui
            .scroll
            .saturating_add(content.len().saturating_sub(ui.fullscreen_rows));
    }
    ui.fullscreen_width = width;
    ui.fullscreen_rows = content.len();
    ui.scroll = ui.scroll.min(content.len());

    let status = visible_status(ui, operation);
    let status_height = usize::from(status.is_some()).min(height - 1);
    let composer = input_window(&ui.draft, ui.cursor, width, (height - status_height).min(4));
    let composer_height = composer.lines.len();
    let completions = ui.completion.rows(
        width,
        height.saturating_sub(composer_height + status_height),
        &ui.keys,
    );
    let footer = orientation::rows(
        ui,
        model,
        width,
        height.saturating_sub(composer_height + status_height + completions.len()),
    );
    let viewport =
        height.saturating_sub(composer_height + status_height + completions.len() + footer.len());

    let end = content.len().saturating_sub(ui.scroll);
    let start = end.saturating_sub(viewport);
    let mut rows = vec![Line::raw(""); height];
    let padding = viewport.saturating_sub(end.saturating_sub(start));
    for (index, row) in content[start..end].iter().enumerate() {
        rows[padding + index] = row.clone();
    }

    let mut next_row = viewport;
    if let Some(status) = status
        && status_height > 0
    {
        rows[next_row] = Line::raw(fit_line(&status, width));
        next_row += 1;
    }
    for row in completions {
        if next_row < height {
            rows[next_row] = Line::raw(row);
            next_row += 1;
        }
    }
    for (index, line) in composer.lines.iter().enumerate() {
        if next_row + index < height {
            rows[next_row + index] = Line::raw(line.as_str());
        }
    }
    for (index, line) in footer.into_iter().enumerate() {
        rows[next_row + composer_height + index] = line;
    }
    let cursor_row = next_row + composer.cursor_row;
    let cursor = (cursor_row < height).then_some((
        cursor_row,
        composer.cursor_col.min(width.saturating_sub(1)) as u16,
    ));

    screen.draw_fullscreen(&mut terminal.output()?, &rows, cursor)?;
    Ok(())
}

fn draw_question_fullscreen(
    terminal: &mut TerminalSession,
    screen: &mut Screen,
    ui: &mut Frontend,
    width: u16,
    height: u16,
) -> Result<()> {
    let frame = ui
        .question
        .as_mut()
        .expect("question owns input")
        .render(width, height, &ui.keys);
    screen.draw_fullscreen(&mut terminal.output()?, &frame.rows, frame.cursor)?;
    Ok(())
}

fn apply_question_action(
    ui: &mut Frontend,
    responder: Option<&QuestionResponder>,
    action: QuestionAction,
) {
    match action {
        QuestionAction::None => {}
        QuestionAction::Answer(request, answers) => {
            let Some(responder) = responder else {
                ui.question = None;
                ui.status = "Question responder is unavailable".into();
                return;
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
                return;
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
    progress: Option<&LiveTranscript>,
    operation: Option<ActiveOperation>,
    width: u16,
    height: u16,
) -> Result<()> {
    let (rows, cursor) = modal_rows(ui, progress, operation, width, height);
    screen.draw_fullscreen(&mut terminal.output()?, &rows, cursor)?;
    Ok(())
}

fn modal_rows(
    ui: &mut Frontend,
    progress: Option<&LiveTranscript>,
    operation: Option<ActiveOperation>,
    width: u16,
    height: u16,
) -> (Vec<Line<'static>>, Option<(usize, u16)>) {
    let width = width.max(1) as usize;
    let height = height.max(1) as usize;
    if let Some(view) = &mut ui.details {
        view.prepare(&ui.history, progress, width);
    }
    let controls = ui
        .details
        .as_ref()
        .map(|view| view.controls(&ui.keys))
        .or_else(|| ui.pickers.controls(&ui.keys));
    let status = visible_status(ui, operation);
    let status = if ui.question.is_some() {
        Some(status.map_or_else(
            || "Question waiting · close this view to answer".into(),
            |status| format!("{status} · Question waiting · close this view to answer"),
        ))
    } else {
        status
    };
    let picker = ui.pickers.active();
    // Leave a selected result visible alongside the query when at least two
    // physical rows exist. Status is less important than choosing visibly.
    let reserve_content = usize::from(picker.is_some() && height > 1);
    let status_height = (usize::from(controls.is_some()) + usize::from(status.is_some()))
        .min(height - usize::from(picker.is_some()) - reserve_content);
    let composer = picker.map(|picker| {
        input_window(
            &picker.query,
            picker.query.len(),
            width,
            (height - status_height - reserve_content).min(3),
        )
    });
    let composer_height = composer.as_ref().map_or(0, |input| input.lines.len());
    let viewport = height.saturating_sub(composer_height + status_height);
    // Picker rows are only physical when the details view is not covering them.
    let picker_rows = if ui.details.is_none() {
        ui.pickers.rows(width, viewport)
    } else {
        Vec::new()
    };
    let content = ui
        .details
        .as_ref()
        .map_or(picker_rows.as_slice(), DetailView::rows);
    let scroll = ui.details.as_ref().map_or(0, |view| view.scroll);
    let end = content.len().saturating_sub(scroll.min(content.len()));
    let start = end.saturating_sub(viewport);
    let mut rows = vec![Line::raw(""); height];
    let padding = viewport.saturating_sub(end - start);
    for (index, row) in content[start..end].iter().enumerate() {
        rows[padding + index] = Line::raw(fit_line(row, width));
    }

    let mut cursor = None;
    let mut next_row = viewport;
    for label in controls.iter().chain(status.iter()).take(status_height) {
        if next_row < height {
            rows[next_row] = Line::raw(fit_line(label, width));
            next_row += 1;
        }
    }
    if let Some(composer) = composer {
        for (index, line) in composer.lines.into_iter().enumerate() {
            if next_row + index < height {
                rows[next_row + index] = Line::raw(line);
            }
        }
        let row = next_row + composer.cursor_row;
        if row < height {
            cursor = Some((row, composer.cursor_col.min(width.saturating_sub(1)) as u16));
        }
    }

    (rows, cursor)
}

#[cfg(test)]
mod keybinding_tests;
#[cfg(test)]
mod presentation_tests;
#[cfg(test)]
mod tests;
