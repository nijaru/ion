//! Terminal view over the same coding loop used by headless and library hosts.
#[cfg(test)]
use ion_ai::Role;
#[cfg(test)]
use std::fs;
use std::{
    collections::{HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use crate::display_text::{fit_line, push_wrapped};
use crate::terminal_commands::{Builtin, Completion};
use crate::transcript_detail::{DetailView, tools};
use crate::transcript_render::kind_label;
use anyhow::{Context, Result, ensure};
use ignore::WalkBuilder;
use ion_ai::{Content, Message, ModelRef};
use ion_core::{
    AcceptedInput, AgentLimits, CodingSession, ForkPoint, InputBudget, InputReservation,
    LiveTranscript, SessionEntry, SessionView, SteeringInbox, TranscriptItem, TranscriptProjection,
    TurnEndReason,
};
use ion_host::image_input::LoadedImage;
use ion_host::{CredentialStatus, CredentialStore, Resources, Selection};
use ion_terminal::{
    Frame, InputEvent, InputStream, KeyCode, KeyEvent, Modifiers, MouseKind, Screen,
    TerminalSession,
};
use ratatui::text::Line;
use tokio::time::{Duration, interval};
use tokio_util::sync::CancellationToken;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const MAX_DRAFT: usize = 64 * 1024;
const LIVE_REGION_MAX_ROWS: usize = 12;
const RESUME_TURN_LIMIT: usize = 6;
const RESUME_ENTRY_LIMIT_WITHOUT_TURNS: usize = 32;

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

#[derive(Clone, Copy)]
enum ActiveOperation<'a> {
    Coding(&'a CancellationToken),
    Shell(&'a CancellationToken),
    Compaction(&'a CancellationToken),
}

impl ActiveOperation<'_> {
    fn is_cancelled(self) -> bool {
        match self {
            Self::Coding(stop) | Self::Shell(stop) | Self::Compaction(stop) => stop.is_cancelled(),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Coding(stop) if stop.is_cancelled() => "Cancelling turn…",
            Self::Shell(stop) if stop.is_cancelled() => "Cancelling shell…",
            Self::Compaction(stop) if stop.is_cancelled() => "Cancelling compaction…",
            Self::Coding(_) => "Working",
            Self::Shell(_) => "Running shell",
            Self::Compaction(_) => "Summarizing context",
        }
    }
}

#[derive(Default)]
struct Frontend {
    mode: TuiMode,
    output_detail: crate::tool_output::OutputDetail,
    thinking_visibility: crate::transcript_render::ThinkingVisibility,
    history: TranscriptProjection,
    fullscreen_rows: usize,
    fullscreen_width: usize,
    draft: String,
    images: Vec<LoadedImage>,
    cursor: usize,
    history_session: Option<PathBuf>,
    history_published_items: usize,
    pending_history_items: Vec<TranscriptItem>,
    pending_history_target: usize,
    pending_history_banner: Option<String>,
    scroll: usize,
    status: String,
    notices: Vec<String>,
    picker: Option<Picker>,
    completion: Completion,
    pending: VecDeque<TurnInput>,
    input_budget: InputBudget,
    prompt_history: Vec<String>,
    browsing: Option<PromptBrowse>,
    details: Option<DetailView>,
    clipboard_job: Option<tokio::task::JoinHandle<Result<PreparedPaste>>>,
    cwd: PathBuf,
}

struct PromptBrowse {
    index: usize,
    end: usize,
    draft: String,
    cursor: usize,
}

struct TurnInput {
    prompt: String,
    images: Vec<LoadedImage>,
    _reservation: Option<InputReservation>,
}

impl TurnInput {
    fn direct(prompt: String, images: Vec<LoadedImage>) -> Self {
        Self {
            prompt,
            images,
            _reservation: None,
        }
    }

    fn from_accepted(input: AcceptedInput) -> Self {
        let (input, reservation) = input.into_parts();
        // The private inbox only accepts messages built by Message::user_input.
        let (prompt, images) = split_editor_input(input)
            .expect("terminal inbox must retain the prompt and image-note layout");
        Self {
            prompt,
            images,
            _reservation: Some(reservation),
        }
    }
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

enum PreparedPaste {
    Files(Vec<PathBuf>),
    Image(LoadedImage),
    Text(String),
}

enum PickerValue {
    Session(PathBuf),
    Model(ModelRef),
    ForkBefore {
        turn: u64,
        input: Message,
    },
    File {
        path: String,
        start: usize,
        end: usize,
    },
}

struct PickerItem {
    label: String,
    value: PickerValue,
}

struct Picker {
    title: &'static str,
    query: String,
    selected: usize,
    items: Vec<PickerItem>,
}

impl Picker {
    fn matches(&self) -> Vec<usize> {
        let query = self.query.to_ascii_lowercase();
        self.items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                item.label
                    .to_ascii_lowercase()
                    .contains(&query)
                    .then_some(index)
            })
            .collect()
    }
}

pub struct ChatInit {
    pub binding: ion_host::SessionBinding,
    pub images: Vec<LoadedImage>,
    pub startup_diagnostics: Vec<String>,
    pub tui_mode: TuiMode,
}

pub async fn chat(init: ChatInit) -> Result<()> {
    let tui_mode = init.tui_mode;
    let mut runtime = init.binding;
    let images = init.images;
    let mut terminal = TerminalSession::enter().context("interactive chat requires a terminal")?;
    let mut screen = new_inline_screen(&mut terminal)?;
    let mut input = terminal.input()?;
    let mut ui = Frontend {
        mode: tui_mode,
        images,
        ..Frontend::default()
    };
    ui.refresh_session(runtime.session())?;
    for diagnostic in init.startup_diagnostics {
        ui.note(diagnostic);
    }
    loop {
        terminal
            .check_active()
            .context("terminal lifecycle failed before input dispatch")?;
        if let Some(incoming) = ui.pending.pop_front() {
            ui.status.clear();
            ui.scroll = 0;
            run_turn(
                &mut terminal,
                &mut screen,
                &mut input,
                &mut ui,
                &runtime,
                incoming,
            )
            .await?;
            continue;
        }
        draw(&mut terminal, &mut screen, &mut ui, None, None)?;
        #[cfg(debug_assertions)]
        if std::env::var_os("ION_SMOKE_PANIC_AFTER_FIRST_DRAW").is_some() {
            panic!("ION smoke panic after first terminal draw");
        }
        let Some(event) = input.next().await else {
            break;
        };
        terminal
            .check_active()
            .context("terminal lifecycle failed while awaiting input")?;
        match event? {
            InputEvent::Key(key) => match ui.key(key, Some(runtime.resources())) {
                Action::None => {}
                Action::Quit => break,
                Action::Submit(prompt) => {
                    ui.status.clear();
                    ui.scroll = 0;
                    let images = std::mem::take(&mut ui.images);
                    run_turn(
                        &mut terminal,
                        &mut screen,
                        &mut input,
                        &mut ui,
                        &runtime,
                        TurnInput::direct(prompt, images),
                    )
                    .await?;
                }
                Action::Shell(command, exclude_from_context) => {
                    if let Err(error) = run_user_shell(
                        &mut terminal,
                        &mut screen,
                        &mut input,
                        &mut ui,
                        &runtime,
                        command.clone(),
                        exclude_from_context,
                    )
                    .await?
                    {
                        ui.status = format!(
                            "Shell operation failed: {error:#}; inspect the Session and working directory before retrying\nShell command: {command}"
                        );
                    }
                }
                Action::Command(command) => {
                    let (name, args) = command
                        .split_once(char::is_whitespace)
                        .unwrap_or((&command, ""));
                    let args = args.trim();
                    let builtin = Builtin::parse(name);
                    if builtin == Some(Builtin::Login) && !args.is_empty() {
                        match login_in_terminal(
                            &mut terminal,
                            &mut screen,
                            &mut input,
                            runtime.host().credentials(),
                            args,
                        )? {
                            Ok(()) => ui.status = "Credential saved".into(),
                            Err(error) => ui.status = format!("{error:#}"),
                        }
                    } else if builtin == Some(Builtin::Compact) && args.is_empty() {
                        run_compaction(&mut terminal, &mut screen, &mut input, &mut ui, &runtime)
                            .await?;
                    } else if builtin == Some(Builtin::Copy) && args.is_empty() {
                        match copy_last_answer(runtime.session(), &mut terminal).await {
                            Ok(crate::clipboard::CopyOutcome::Copied) => {
                                ui.status = "Copied last assistant answer".into()
                            }
                            Ok(crate::clipboard::CopyOutcome::RequestedFromTerminal) => {
                                ui.status = "Sent clipboard request to terminal".into()
                            }
                            Err(error) => ui.status = format!("Copy failed: {error:#}"),
                        }
                    } else if builtin == Some(Builtin::Editor) && args.is_empty() {
                        ui.completion.clear();
                        match edit_draft_in_terminal(
                            &mut terminal,
                            &mut screen,
                            &mut input,
                            &mut ui,
                        )
                        .await?
                        {
                            Ok(()) => ui.status = "Draft returned from editor".into(),
                            Err(error) => {
                                ui.status =
                                    format!("Editor failed: {error:#}; original draft retained")
                            }
                        }
                    } else {
                        match handle_command(&mut runtime, &mut ui, &command) {
                            Ok(Some(prompt)) => {
                                ui.status.clear();
                                ui.scroll = 0;
                                let images = std::mem::take(&mut ui.images);
                                run_turn(
                                    &mut terminal,
                                    &mut screen,
                                    &mut input,
                                    &mut ui,
                                    &runtime,
                                    TurnInput::direct(prompt, images),
                                )
                                .await?;
                            }
                            Ok(None) => {}
                            Err(error) => ui.status = format!("{error:#}"),
                        }
                    }
                }
                Action::Queue(prompt) => {
                    ui.queue_follow_up(prompt, runtime.agent().limits(), Some(runtime.resources()))
                }
                Action::PasteClipboard => {
                    if let Err(error) = paste_clipboard(&mut ui, runtime.selected()).await {
                        ui.status = format!("Paste failed: {error:#}");
                    }
                }
                Action::Pick(value) => {
                    if let PickerValue::File { path, start, end } = value {
                        ui.insert_file(path, start, end);
                        continue;
                    }
                    if let PickerValue::ForkBefore { turn, input } = value {
                        if let Err(error) = apply_fork(
                            &mut runtime,
                            &mut ui,
                            ForkPoint::BeforeTurn(turn),
                            Some(input),
                        ) {
                            ui.status = format!("{error:#}");
                        }
                        continue;
                    }
                    let result = match value {
                        PickerValue::Session(path) => runtime.switch_session(path),
                        PickerValue::Model(model) => runtime.select_model(model),
                        PickerValue::File { .. } | PickerValue::ForkBefore { .. } => {
                            unreachable!("handled above")
                        }
                    };
                    match result {
                        Ok(()) => {
                            ui.refresh_session(runtime.session())?;
                            ui.status = "Ready".into();
                        }
                        Err(error) => ui.status = format!("{error:#}"),
                    }
                }
            },
            InputEvent::Paste(text) => ui.insert(&text),
            InputEvent::Resize(size) => screen.resize(size.columns, size.rows),
            InputEvent::Mouse(mouse) => match mouse.kind() {
                MouseKind::ScrollUp => ui.scroll = ui.scroll.saturating_add(3),
                MouseKind::ScrollDown => ui.scroll = ui.scroll.saturating_sub(3),
                _ => {}
            },
        }
    }
    input.suspend()?;
    if terminal.is_alt_screen()? {
        terminal.leave_alt_screen()?;
        screen.invalidate();
    }
    screen.finish(&mut terminal.output()?)?;
    terminal.restore()?;
    Ok(())
}

fn new_inline_screen(terminal: &mut TerminalSession) -> Result<Screen> {
    if terminal.is_alt_screen()? {
        terminal.leave_alt_screen()?;
    }
    let (width, height) = terminal.size().context("read terminal size")?;
    let (_, cursor_row) = terminal
        .cursor_position()
        .context("read terminal cursor position")?;
    Ok(Screen::with_live_height(
        width,
        cursor_row.min(height.saturating_sub(1)),
        height,
        1,
    ))
}

async fn paste_clipboard(ui: &mut Frontend, selected: &Selection) -> Result<()> {
    start_clipboard_paste(ui, selected);
    finish_clipboard_paste(ui).await
}

fn start_clipboard_paste(ui: &mut Frontend, selected: &Selection) {
    if ui.clipboard_job.is_some() {
        ui.status = "Clipboard paste is already in progress".into();
        return;
    }
    let selected = selected.clone();
    ui.clipboard_job = Some(tokio::spawn(async move {
        let content = tokio::time::timeout(Duration::from_secs(3), crate::clipboard::read())
            .await
            .context("clipboard read timed out")??;
        tokio::task::spawn_blocking(move || prepare_clipboard(&selected, content))
            .await
            .context("clipboard image preparation stopped")?
    }));
    ui.status = "Reading clipboard…".into();
}

async fn finish_clipboard_paste(ui: &mut Frontend) -> Result<()> {
    let job = ui
        .clipboard_job
        .take()
        .context("no clipboard paste is pending")?;
    let content = job.await.context("clipboard reader stopped")??;
    if matches!(
        ui.status.as_str(),
        "Reading clipboard…" | "Wait for clipboard paste, then send the prompt"
    ) {
        ui.status.clear();
    }
    apply_clipboard(ui, content)
}

async fn finish_ready_clipboard_paste(ui: &mut Frontend) {
    if ui
        .clipboard_job
        .as_ref()
        .is_some_and(tokio::task::JoinHandle::is_finished)
    {
        finish_pending_clipboard_paste(ui).await;
    }
}

async fn finish_pending_clipboard_paste(ui: &mut Frontend) {
    if ui.clipboard_job.is_some()
        && let Err(error) = finish_clipboard_paste(ui).await
    {
        ui.status = format!("Paste failed: {error:#}");
    }
}

fn prepare_clipboard(
    selected: &Selection,
    content: crate::clipboard::PasteContent,
) -> Result<PreparedPaste> {
    match content {
        crate::clipboard::PasteContent::Files(paths) => Ok(PreparedPaste::Files(paths)),
        crate::clipboard::PasteContent::Image {
            width,
            height,
            rgba,
        } => Ok(PreparedPaste::Image(ion_host::image_input::load_rgba(
            selected, width, height, rgba,
        )?)),
        crate::clipboard::PasteContent::Text(text) => Ok(PreparedPaste::Text(text)),
    }
}

fn apply_clipboard(ui: &mut Frontend, content: PreparedPaste) -> Result<()> {
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
            ui.images.push(image);
            ui.status = format!("{} image(s) attached to the next prompt", ui.images.len());
        }
        PreparedPaste::Text(text) => ui.insert(&text),
    }
    Ok(())
}

fn clipboard_paths(paths: &[PathBuf], shell: bool) -> Result<String> {
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

fn login_in_terminal(
    terminal: &mut TerminalSession,
    screen: &mut Screen,
    input: &mut InputStream,
    credentials: &CredentialStore,
    provider: &str,
) -> Result<Result<()>> {
    anyhow::ensure!(!provider.is_empty(), "use /login PROVIDER");
    input
        .suspend()
        .context("release terminal input for login")?;
    terminal.suspend().context("suspend terminal for login")?;
    let result = (|| -> Result<()> {
        let key = rpassword::prompt_password(format!("{provider} API key: "))
            .context("read login credential")?;
        credentials.save_api_key(provider, &key)
    })();
    terminal.resume().context("resume terminal after login")?;
    *screen = new_inline_screen(terminal).context("restore inline chat after login")?;
    *input = terminal
        .input()
        .context("resume terminal input after login")?;
    Ok(result)
}

async fn copy_last_answer(
    session: &CodingSession,
    terminal: &mut TerminalSession,
) -> Result<crate::clipboard::CopyOutcome> {
    let answer = last_committed_answer(&session.view()?)?;
    crate::clipboard::copy(&answer, terminal).await
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
    view.entries
        .iter()
        .rev()
        .find_map(|entry| match entry {
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
    input: &mut InputStream,
    ui: &mut Frontend,
) -> Result<Result<()>> {
    input
        .suspend()
        .context("release terminal input for editor")?;
    terminal.suspend().context("suspend terminal for editor")?;
    let edited = crate::external_editor::edit(&ui.draft, MAX_DRAFT).await;
    terminal.resume().context("resume terminal after editor")?;
    *screen = new_inline_screen(terminal).context("restore inline chat after editor")?;
    *input = terminal
        .input()
        .context("resume terminal input after editor")?;
    Ok(edited.map(|edited| ui.replace_draft(edited)))
}

fn apply_fork(
    runtime: &mut ion_host::SessionBinding,
    ui: &mut Frontend,
    point: ForkPoint,
    restore: Option<Message>,
) -> Result<()> {
    let id = runtime.fork_session(point)?;
    ui.refresh_session(runtime.session())?;
    let restore_error = restore.and_then(|input| ui.restore_input(input).err());
    ui.status = if let Some(error) = restore_error {
        format!(
            "Forked as {}; {error}; draft retained; inspect source to copy selected input",
            &id[..id.len().min(12)]
        )
    } else {
        format!(
            "Forked as {}; both sessions use the same working directory",
            &id[..id.len().min(12)]
        )
    };
    Ok(())
}

fn handle_command(
    runtime: &mut ion_host::SessionBinding,
    ui: &mut Frontend,
    command: &str,
) -> Result<Option<String>> {
    let (name, args) = command
        .split_once(char::is_whitespace)
        .unwrap_or((command, ""));
    let args = args.trim();
    match Builtin::parse(name) {
        Some(Builtin::Help) => ui.note(Builtin::help()),
        Some(Builtin::Settings) => ui.output_settings(args)?,
        Some(Builtin::Tui) => match args {
            "" => ui.note(format!(
                "TUI mode: {}. Use /tui inline or /tui fullscreen",
                ui.mode.label()
            )),
            "inline" | "regular" => {
                ui.mode = TuiMode::Inline;
                ui.scroll = 0;
                ui.status = "Inline TUI · native terminal scrollback".into();
            }
            "fullscreen" => {
                ui.mode = TuiMode::Fullscreen;
                ui.scroll = 0;
                ui.fullscreen_rows = 0;
                ui.status = "Fullscreen TUI · PageUp/PageDown or mouse wheel scrolls".into();
            }
            _ => anyhow::bail!("use /tui inline or /tui fullscreen"),
        },
        Some(Builtin::Image) => {
            anyhow::ensure!(!args.is_empty(), "use /image PATH");
            let path = Path::new(args);
            let path = if path.is_absolute() {
                path.to_owned()
            } else {
                runtime.session().cwd().join(path)
            };
            ui.images.push(ion_host::image_input::load_image(
                runtime.selected(),
                &path,
            )?);
            ui.status = format!("{} image(s) attached to the next prompt", ui.images.len());
        }
        Some(Builtin::Skills) => ui.note(
            runtime
                .resources()
                .skills()
                .map(|skill| format!("{} — {}", skill.name, skill.description))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        Some(Builtin::Prompts) => ui.note(
            runtime
                .resources()
                .templates()
                .map(|template| format!("/{} — {}", template.name, template.description))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        Some(Builtin::Reload) => {
            runtime.reload_resources()?;
            ui.note(format!(
                "Reloaded resources ({} diagnostic(s))",
                runtime.resources().diagnostics().len()
            ));
        }
        Some(Builtin::Session) => {
            let view = runtime.session().view()?;
            ui.note(format!(
                "Session {} · {} turn(s) · {} · {}",
                runtime.session().path().display(),
                view.entries
                    .iter()
                    .filter(|entry| matches!(entry, ion_core::SessionEntry::TurnStarted { .. }))
                    .count(),
                view.name.as_deref().unwrap_or("unnamed"),
                context_label(&view, runtime.selected().context_window_tokens),
            ));
        }
        Some(Builtin::Export) => {
            anyhow::ensure!(!args.is_empty(), "use /export PATH");
            let target = Path::new(args);
            let target = if target.is_absolute() {
                target.to_owned()
            } else {
                runtime.session().cwd().join(target)
            };
            crate::transcript::save_new(&runtime.session().view()?, &target)?;
            ui.status = format!("Transcript saved to {}", target.display());
        }
        Some(Builtin::New) => {
            runtime.new_session()?;
            ui.refresh_session(runtime.session())?;
            ui.note("Started a new session".into());
        }
        Some(Builtin::Clone) => {
            let id = runtime.clone_session()?;
            ui.refresh_session(runtime.session())?;
            ui.note(format!(
                "Cloned conversation as {id}; both sessions use the same working directory"
            ));
        }
        Some(Builtin::Fork) => {
            let turns = runtime.session().view()?.turns();
            if args.is_empty() {
                let items = turns
                    .into_iter()
                    .map(|item| PickerItem {
                        label: format!("Turn {}  {}", item.turn, crate::preview_input(&item.input)),
                        value: PickerValue::ForkBefore {
                            turn: item.turn,
                            input: item.input,
                        },
                    })
                    .collect();
                ui.picker = Some(Picker {
                    title: "Fork before Turn",
                    query: String::new(),
                    selected: 0,
                    items,
                });
            } else {
                let turn: u64 = args.parse().context("use /fork TURN")?;
                let input = turns
                    .into_iter()
                    .find(|item| item.turn == turn)
                    .context("selected Turn does not exist")?
                    .input;
                apply_fork(runtime, ui, ForkPoint::BeforeTurn(turn), Some(input))?;
            }
        }
        Some(Builtin::ForkAfter) => {
            let turn: u64 = args.parse().context("use /fork-after TURN")?;
            apply_fork(runtime, ui, ForkPoint::AfterTurn(turn), None)?;
        }
        Some(Builtin::Resume) => {
            if !args.is_empty() {
                runtime.switch_session(runtime.catalog().by_id(args)?)?;
                ui.refresh_session(runtime.session())?;
            } else {
                let items = runtime
                    .catalog()
                    .list()?
                    .into_iter()
                    .map(|session| PickerItem {
                        label: format!(
                            "{}  {}  {}",
                            &session.id[..session.id.len().min(12)],
                            session.name.as_deref().unwrap_or(""),
                            session.preview.as_deref().unwrap_or("")
                        ),
                        value: PickerValue::Session(session.path),
                    })
                    .collect();
                ui.picker = Some(Picker {
                    title: "Resume session",
                    query: String::new(),
                    selected: 0,
                    items,
                });
            }
        }
        Some(Builtin::Name) => {
            if args.is_empty() {
                ui.note(
                    runtime
                        .session()
                        .view()?
                        .name
                        .unwrap_or_else(|| "Session has no name".into()),
                );
            } else {
                runtime.session().set_name(Some(args))?;
                ui.refresh_session(runtime.session())?;
            }
        }
        Some(Builtin::Model) => {
            if !args.is_empty() {
                let (provider, model) =
                    args.split_once('/').context("use /model PROVIDER/MODEL")?;
                runtime.select_model(ModelRef {
                    provider: provider.into(),
                    model: model.into(),
                })?;
                ui.status = format!("Selected {args}");
            } else {
                let items = runtime
                    .host()
                    .models()
                    .choices(runtime.host().credentials())?
                    .into_iter()
                    .map(|choice| {
                        let option = choice.selected;
                        Ok(PickerItem {
                            label: format!(
                                "{}/{}  {}  {}",
                                option.provider,
                                option.model,
                                choice.label,
                                if choice.credential == CredentialStatus::Missing {
                                    "no credential"
                                } else {
                                    "ready"
                                }
                            ),
                            value: PickerValue::Model(option.identity()),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                ui.picker = Some(Picker {
                    title: "Choose model",
                    query: String::new(),
                    selected: 0,
                    items,
                });
            }
        }
        Some(Builtin::Logout) => {
            anyhow::ensure!(!args.is_empty(), "use /logout PROVIDER");
            runtime.host().credentials().remove(args)?;
            ui.status = format!("Removed saved {args} credential");
        }
        Some(Builtin::Tools) => ui.list_tools(),
        Some(Builtin::Tool) => {
            let number = if args.is_empty() {
                tools(&ui.history).count()
            } else {
                args.parse::<usize>().context("use /tool [N]")?
            };
            ui.open_details(Some(number));
        }
        Some(kind) => anyhow::bail!("use {}", kind.usage()),
        None => {
            if let Some(prompt) = runtime.resources().expand_command(command) {
                return prompt.map(Some);
            }
            ui.status = format!("Unknown command: {name}. Type /help");
        }
    }
    Ok(None)
}

async fn run_compaction(
    terminal: &mut TerminalSession,
    screen: &mut Screen,
    input: &mut InputStream,
    ui: &mut Frontend,
    runtime: &ion_host::SessionBinding,
) -> Result<()> {
    terminal
        .check_active()
        .context("terminal lifecycle failed before compaction")?;
    let session = runtime.session();
    let agent = runtime.agent();
    let selected = runtime.selected();
    ui.status.clear();
    let stop = CancellationToken::new();
    let mut input_ended = false;
    let mut output_error = None;
    let result = {
        let compact = agent.compact(session, stop.clone(), |_| {});
        tokio::pin!(compact);
        let mut tick = interval(Duration::from_millis(50));
        loop {
            if output_error.is_none()
                && let Err(error) = terminal.check_active()
            {
                output_error = Some(error.into());
                input_ended = true;
                stop.cancel();
            }
            tokio::select! {
                result = &mut compact => break result,
                event = input.next(), if !input_ended => {
                    if let Err(error) = terminal.check_active() {
                        output_error = Some(error.into());
                        input_ended = true;
                        stop.cancel();
                        continue;
                    }
                    match event {
                    Some(Ok(InputEvent::Key(KeyEvent { code: KeyCode::Char('c'), modifiers }))) if modifiers.contains(Modifiers::CONTROL) => stop.cancel(),
                    Some(Ok(InputEvent::Key(key))) if is_clipboard_shortcut(key) && ui.picker.is_none() && ui.details.is_none() => {
                        start_clipboard_paste(ui, selected);
                    },
                    Some(Ok(InputEvent::Key(key))) => busy_key(ui, key, &stop, None, Some(runtime.resources()), runtime.agent().limits()),
                    Some(Ok(InputEvent::Paste(text))) => ui.insert(&text),
                    Some(Ok(InputEvent::Resize(size))) => screen.resize(size.columns, size.rows),
                    Some(Ok(InputEvent::Mouse(mouse))) => match mouse.kind() {
                        MouseKind::ScrollUp => ui.scroll = ui.scroll.saturating_add(3),
                        MouseKind::ScrollDown => ui.scroll = ui.scroll.saturating_sub(3),
                        _ => {},
                    },
                    Some(Err(error)) => { stop.cancel(); input_ended = true; ui.status = format!("Input failed: {error}"); },
                    None => { stop.cancel(); input_ended = true; },
                    }
                },
                _ = tick.tick(), if output_error.is_none() => {
                    finish_ready_clipboard_paste(ui).await;
                    if let Err(error) = draw(terminal, screen, ui, None, Some(ActiveOperation::Compaction(&stop))) {
                        output_error = Some(error);
                        input_ended = true;
                        stop.cancel();
                    }
                },
            }
        }
    };
    finish_pending_clipboard_paste(ui).await;
    if result.is_err() {
        return_pending_to_editor(ui);
    }
    ui.status = match result {
        Ok(true) => "Context summarized; raw history retained".into(),
        Ok(false) => "No settled history to summarize".into(),
        Err(error) => format!("Compaction ended: {error}"),
    };
    if let Some(error) = output_error {
        return Err(error.context("terminal output failed after operation settlement"));
    }
    if input_ended {
        return Err(anyhow::anyhow!("terminal input ended during compaction"));
    }
    Ok(())
}

fn expand_resource_input(
    resources: &Resources,
    prompt: String,
) -> std::result::Result<String, (String, anyhow::Error)> {
    match resources.expand_command(prompt.trim()) {
        Some(Ok(expanded)) => Ok(expanded),
        Some(Err(error)) => Err((prompt, error)),
        None => Ok(prompt),
    }
}

fn busy_key(
    ui: &mut Frontend,
    key: KeyEvent,
    stop: &CancellationToken,
    steering: Option<&SteeringInbox>,
    resources: Option<&Resources>,
    limits: AgentLimits,
) {
    let raw_command = (key.code == KeyCode::Enter
        && !key.modifiers.contains(Modifiers::SHIFT)
        && !key.modifiers.contains(Modifiers::CONTROL)
        && ui.draft.trim_start().starts_with('/'))
    .then(|| ui.draft.clone());
    let action = ui.key(key, resources);
    let action = if ui.clipboard_job.is_some() {
        match action {
            Action::Submit(prompt) | Action::Queue(prompt) => {
                ui.draft = if ui.draft.is_empty() {
                    prompt
                } else {
                    format!("{prompt}\n\n{}", ui.draft)
                };
                ui.cursor = ui.draft.len();
                ui.status = "Wait for clipboard paste, then send the prompt".into();
                return;
            }
            other => other,
        }
    } else {
        action
    };
    match action {
        Action::Submit(prompt) => {
            if let Some(steering) = steering {
                match steering.push_message(Message::user_input(prompt.clone(), ui.images.clone()))
                {
                    Ok(()) => {
                        ui.images.clear();
                        ui.status = "Steering sent for the next model step".into();
                    }
                    Err(error) => {
                        ui.draft = prompt;
                        ui.cursor = ui.draft.len();
                        ui.status = format!("Steering was not queued: {error}");
                    }
                }
            } else {
                ui.queue_follow_up(prompt, limits, resources);
            }
        }
        Action::Queue(prompt) => ui.queue_follow_up(prompt, limits, resources),
        Action::Command(command) => {
            let (name, args) = command
                .split_once(char::is_whitespace)
                .unwrap_or((&command, ""));
            let builtin = Builtin::parse(name);
            if builtin == Some(Builtin::Settings) {
                if let Err(error) = ui.output_settings(args.trim()) {
                    ui.status = format!("{error:#}");
                }
                return;
            }
            if matches!(builtin, Some(Builtin::Copy | Builtin::Editor)) {
                ui.status = "This action is available after the operation".into();
                return;
            }
            if builtin.is_some() {
                ui.status = "Commands are available after this operation".into();
            } else if let (Some(steering), Some(resources)) = (steering, resources)
                && let Some(expanded) = resources.expand_command(&command)
            {
                match expanded {
                    Ok(prompt) => {
                        match steering.push_message(Message::user_input(prompt, ui.images.clone()))
                        {
                            Ok(()) => {
                                ui.images.clear();
                                ui.status = "Steering sent for the next model step".into();
                                return;
                            }
                            Err(error) => ui.status = format!("Steering was not queued: {error}"),
                        }
                    }
                    Err(error) => ui.status = format!("{error:#}"),
                }
            } else {
                ui.status = "Commands are available after this operation".into();
            }
            ui.draft = raw_command.unwrap_or(command);
            ui.cursor = ui.draft.len();
        }
        Action::Shell(command, exclude_from_context) => {
            ui.draft = format!(
                "{}{}",
                if exclude_from_context { "!!" } else { "!" },
                command
            );
            ui.cursor = ui.draft.len();
            ui.status = "Shell commands are available after this operation".into();
        }
        Action::Quit => stop.cancel(),
        Action::PasteClipboard => ui.status = "Paste is unavailable during this operation".into(),
        Action::Pick(PickerValue::File { path, start, end }) => {
            ui.insert_file(path, start, end);
        }
        Action::Pick(_) | Action::None => {}
    }
}

fn is_clipboard_shortcut(key: KeyEvent) -> bool {
    key.code == KeyCode::Char('v') && key.modifiers.contains(Modifiers::CONTROL)
}

fn return_pending_to_editor(ui: &mut Frontend) {
    if ui.pending.is_empty() {
        return;
    }
    ui.restore_browsed_draft();
    let mut restored = Vec::new();
    let remaining = ui
        .pending
        .drain(..)
        .map(|TurnInput { prompt, images, .. }| {
            restored.extend(images);
            prompt
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    restored.append(&mut ui.images);
    ui.images = restored;
    if ui.draft.is_empty() {
        ui.draft = remaining;
    } else {
        ui.draft = format!("{remaining}\n\n{}", ui.draft);
    }
    ui.cursor = ui.draft.len();
}

fn scan_files(cwd: &Path) -> Vec<String> {
    let mut files = Vec::new();
    for entry in WalkBuilder::new(cwd)
        .follow_links(false)
        .require_git(false)
        .build()
        .flatten()
    {
        if entry.file_type().is_some_and(|kind| kind.is_file())
            && let Ok(relative) = entry.path().strip_prefix(cwd)
        {
            files.push(relative.to_string_lossy().into_owned());
            if files.len() >= 20_000 {
                break;
            }
        }
    }
    files.sort();
    files
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

#[allow(clippy::too_many_arguments)]
async fn run_user_shell(
    terminal: &mut TerminalSession,
    screen: &mut Screen,
    input: &mut InputStream,
    ui: &mut Frontend,
    runtime: &ion_host::SessionBinding,
    command: String,
    exclude_from_context: bool,
) -> Result<Result<()>> {
    terminal
        .check_active()
        .context("terminal lifecycle failed before shell admission")?;
    let stop = CancellationToken::new();
    let mut tick = interval(Duration::from_millis(50));
    let mut input_ended = false;
    let mut output_error = None;
    ui.status.clear();
    let output = {
        let running = runtime.run_user_shell(&command, stop.clone(), exclude_from_context);
        tokio::pin!(running);
        loop {
            if output_error.is_none()
                && let Err(error) = terminal.check_active()
            {
                output_error = Some(error.into());
                input_ended = true;
                stop.cancel();
            }
            tokio::select! {
                result = &mut running => break result,
                event = input.next(), if !input_ended => {
                    if let Err(error) = terminal.check_active() {
                        output_error = Some(error.into());
                        input_ended = true;
                        stop.cancel();
                        continue;
                    }
                    match event {
                    Some(Ok(InputEvent::Key(KeyEvent { code: KeyCode::Char('c'), modifiers }))) if modifiers.contains(Modifiers::CONTROL) => {
                        stop.cancel();
                    }
                    Some(Ok(InputEvent::Key(key))) if is_clipboard_shortcut(key) && ui.picker.is_none() && ui.details.is_none() => {
                        start_clipboard_paste(ui, runtime.selected());
                    },
                    Some(Ok(InputEvent::Key(key))) => busy_key(ui, key, &stop, None, Some(runtime.resources()), runtime.agent().limits()),
                    Some(Ok(InputEvent::Paste(text))) => ui.insert(&text),
                    Some(Ok(InputEvent::Resize(size))) => screen.resize(size.columns, size.rows),
                    Some(Ok(InputEvent::Mouse(mouse))) => match mouse.kind() {
                        MouseKind::ScrollUp => ui.scroll = ui.scroll.saturating_add(3),
                        MouseKind::ScrollDown => ui.scroll = ui.scroll.saturating_sub(3),
                        _ => {},
                    },
                    Some(Err(error)) => {
                        stop.cancel();
                        input_ended = true;
                        ui.status = format!("Input failed: {error}");
                    }
                    None => { stop.cancel(); input_ended = true; },
                    }
                },
                _ = tick.tick(), if output_error.is_none() => {
                    finish_ready_clipboard_paste(ui).await;
                    if let Err(error) = draw(terminal, screen, ui, None, Some(ActiveOperation::Shell(&stop))) {
                        output_error = Some(error);
                        input_ended = true;
                        stop.cancel();
                    }
                },
            }
        }
    };
    finish_pending_clipboard_paste(ui).await;
    // Terminal failure is fatal even when Session inspection or shell settlement
    // also failed. The operation has settled; queued input has no new authority.
    if let Some(error) = output_error {
        return Err(error.context("terminal output failed after operation settlement"));
    }
    if input_ended {
        return Err(anyhow::anyhow!(
            "terminal input ended during the shell operation"
        ));
    }
    Ok((|| {
        let view = runtime.session().view()?;
        ui.load_history(runtime.session(), &view);
        ui.scroll = 0;
        let output = output?;
        ui.status = if output.is_error {
            "Shell finished with an error"
        } else {
            "Shell finished"
        }
        .into();
        Ok(())
    })())
}

async fn run_turn(
    terminal: &mut TerminalSession,
    screen: &mut Screen,
    input: &mut InputStream,
    ui: &mut Frontend,
    runtime: &ion_host::SessionBinding,
    incoming: TurnInput,
) -> Result<()> {
    terminal
        .check_active()
        .context("terminal lifecycle failed before turn admission")?;
    let session = runtime.session();
    let selected = runtime.selected();
    let user_message =
        Message::user_input(incoming.prompt.clone(), incoming.images.iter().cloned());
    // Core's admission event transfers custody. A later history read cannot
    // decide whether the original input (and its reservation) still belongs here.
    let mut unaccepted = Some(incoming);
    let progress = Arc::new(Mutex::new(LiveTranscript::with_user_input(&user_message)));
    let observer = progress.clone();
    let stop = CancellationToken::new();
    let steering = SteeringInbox::new(runtime.agent().limits(), ui.input_budget.clone());
    let mut input_ended = false;
    let mut output_error = None;
    let result = {
        let turn = runtime.agent().submit_message_with_steering(
            session,
            user_message,
            runtime.instructions().to_owned(),
            stop.clone(),
            &steering,
            |event| {
                if matches!(&event, ion_core::CodingAgentEvent::TurnAccepted { .. }) {
                    drop(unaccepted.take());
                }
                observer
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .observe(event);
            },
        );
        tokio::pin!(turn);
        let mut tick = interval(Duration::from_millis(50));
        loop {
            if output_error.is_none()
                && let Err(error) = terminal.check_active()
            {
                output_error = Some(error.into());
                input_ended = true;
                stop.cancel();
            }
            tokio::select! {
                result = &mut turn => break result,
                event = input.next(), if !input_ended => {
                    if let Err(error) = terminal.check_active() {
                        output_error = Some(error.into());
                        input_ended = true;
                        stop.cancel();
                        continue;
                    }
                    match event {
                    Some(Ok(InputEvent::Key(KeyEvent { code: KeyCode::Char('c'), modifiers }))) if modifiers.contains(Modifiers::CONTROL) => stop.cancel(),
                    Some(Ok(InputEvent::Key(key))) if is_clipboard_shortcut(key) && ui.picker.is_none() && ui.details.is_none() => {
                        start_clipboard_paste(ui, selected);
                    },
                    Some(Ok(InputEvent::Key(key))) => busy_key(ui, key, &stop, Some(&steering), Some(runtime.resources()), runtime.agent().limits()),
                    Some(Ok(InputEvent::Paste(text))) => ui.insert(&text),
                    Some(Ok(InputEvent::Resize(size))) => screen.resize(size.columns, size.rows),
                    Some(Ok(InputEvent::Mouse(mouse))) => match mouse.kind() {
                        MouseKind::ScrollUp => ui.scroll = ui.scroll.saturating_add(3),
                        MouseKind::ScrollDown => ui.scroll = ui.scroll.saturating_sub(3),
                        _ => {},
                    },
                    Some(Err(error)) => { stop.cancel(); input_ended = true; ui.status = format!("Input failed: {error}"); },
                    None => { stop.cancel(); input_ended = true; },
                    }
                },
                _ = tick.tick(), if output_error.is_none() => {
                    finish_ready_clipboard_paste(ui).await;
                    let preview = progress.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    if let Err(error) = draw(terminal, screen, ui, Some(&preview), Some(ActiveOperation::Coding(&stop))) {
                        output_error = Some(error);
                        input_ended = true;
                        stop.cancel();
                    }
                }
            }
        }
    };
    finish_pending_clipboard_paste(ui).await;
    for input in steering.take_uncommitted() {
        ui.pending.push_back(TurnInput::from_accepted(input));
    }
    if let Some(incoming) = unaccepted {
        ui.pending.push_front(incoming);
        return_pending_to_editor(ui);
    } else if result.is_err() {
        return_pending_to_editor(ui);
    }
    ui.status = match result {
        Ok(_) => String::new(),
        Err(error) => format!("Turn ended: {error}"),
    };
    match session.view() {
        Ok(view) => ui.load_history(session, &view),
        Err(error) => {
            // Stop queued dispatch, but retain the editor and report the read
            // failure. Started work has already settled; accepted input is never
            // restored or replayed merely because presentation cannot refresh.
            return_pending_to_editor(ui);
            ui.note(format!("Session history refresh failed: {error}"));
        }
    }
    ui.scroll = 0;
    if let Some(error) = output_error {
        return Err(error.context("terminal output failed after operation settlement"));
    }
    if input_ended {
        return Err(anyhow::anyhow!("terminal input ended during the turn"));
    }
    Ok(())
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
    Shell(String, bool),
    Queue(String),
    PasteClipboard,
    Command(String),
    Pick(PickerValue),
    Quit,
}

impl Frontend {
    fn restore_input(&mut self, input: Message) -> Result<(), &'static str> {
        let (draft, images) = split_editor_input(input)?;
        if draft.len() > MAX_DRAFT {
            return Err("selected input exceeds editor limit");
        }
        self.replace_draft(draft);
        self.images = images;
        Ok(())
    }

    fn refresh_session(&mut self, session: &CodingSession) -> Result<()> {
        self.completion.clear();
        let view = session.view()?;
        self.load_history(session, &view);
        self.details = None;
        self.notices.clear();
        self.scroll = 0;
        self.cwd = session.cwd().to_path_buf();
        if view.unfinished_turn.is_some() {
            self.status = "Previous turn interrupted; tool effects may be unknown".into();
        }
        Ok(())
    }

    fn load_history(&mut self, session: &CodingSession, view: &SessionView) {
        let history = TranscriptProjection::from_session(view);
        let session_path = session.path().to_path_buf();
        let same_session = self.history_session.as_ref() == Some(&session_path);
        if !same_session {
            let had_previous_session = self.history_session.is_some();
            let label = session_path.file_stem().map_or_else(
                || "session".into(),
                |stem| stem.to_string_lossy().into_owned(),
            );
            let tail = resume_history_tail(&history.items);
            self.history_published_items = tail.start;
            self.pending_history_banner = if !history.items.is_empty() {
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
        let start = self.history_published_items.min(history.items.len());
        self.pending_history_items = history.items[start..].to_vec();
        self.pending_history_target = history.items.len();
        self.history_session = Some(session_path);
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
    }

    fn replace_prompt_history(&mut self, prompts: Vec<String>, same_session: bool) {
        if self.browsing.as_ref().is_some_and(|browse| {
            !same_session || !prompts.starts_with(&self.prompt_history[..browse.end])
        }) {
            self.restore_browsed_draft();
        }
        self.prompt_history = prompts;
    }

    fn restore_browsed_draft(&mut self) {
        if let Some(browse) = self.browsing.take() {
            self.draft = browse.draft;
            self.cursor = browse.cursor;
        }
    }

    fn replace_draft(&mut self, draft: String) {
        self.browsing = None;
        self.draft = draft;
        self.cursor = self.draft.len();
    }

    fn pending_history_rows(&self, width: usize) -> Vec<Line<'static>> {
        let mut rows = Vec::new();
        if let Some(banner) = &self.pending_history_banner {
            rows.push(Line::default());
            rows.extend(notice_rows(banner, width));
            rows.push(Line::default());
        } else if self.history_published_items > 0 && !self.pending_history_items.is_empty() {
            rows.push(Line::default());
        }
        if !self.pending_history_items.is_empty() {
            rows.extend(crate::transcript_render::rows(
                &TranscriptProjection {
                    items: self.pending_history_items.clone(),
                },
                width.saturating_sub(1).max(1),
                self.output_detail,
                self.thinking_visibility,
            ));
        }
        rows
    }

    fn finish_history_commit(&mut self) {
        self.history_published_items = self.pending_history_target;
        self.pending_history_items.clear();
        self.pending_history_banner = None;
    }

    fn output_settings(&mut self, args: &str) -> Result<()> {
        match args {
            "" => self.note(format!("Tool output: {}. Use /settings compact or /settings expanded.\nThinking: {}. Use /settings thinking show or /settings thinking hide; independent of generation effort.\nTerminal-local: affects fullscreen and future publication, not existing native scrollback. Inline progress stays compact. Ctrl-O always inspects recorded source.", self.output_detail.label(), self.thinking_visibility.label())),
            "compact" | "expanded" => {
                self.output_detail = if args == "compact" { crate::tool_output::OutputDetail::Compact } else { crate::tool_output::OutputDetail::Expanded };
                self.scroll = 0;
                self.fullscreen_rows = 0;
                self.status = format!("Tool output: {} · fullscreen and future publication; native history unchanged", self.output_detail.label());
            }
            "thinking show" | "thinking hide" => {
                self.thinking_visibility = if args == "thinking show" { crate::transcript_render::ThinkingVisibility::Visible } else { crate::transcript_render::ThinkingVisibility::Hidden };
                self.scroll = 0;
                self.fullscreen_rows = 0;
                self.status = format!("Thinking: {} · presentation only; native history unchanged", self.thinking_visibility.label());
            }
            _ => anyhow::bail!("use /settings compact|expanded or /settings thinking show|hide"),
        }
        Ok(())
    }

    fn note(&mut self, message: String) {
        self.notices.push(message);
        if self.notices.len() > 16 {
            self.notices.remove(0);
        }
        self.scroll = 0;
    }

    fn key(&mut self, key: KeyEvent, resources: Option<&Resources>) -> Action {
        if self.details.is_some() {
            self.completion.clear();
            return self.detail_key(key);
        }
        if self.picker.is_some() {
            self.completion.clear();
            return self.picker_key(key);
        }
        let menu_was_visible = self.completion.visible();
        self.completion.refresh(
            &self.draft,
            self.cursor,
            resources,
            key.code == KeyCode::Tab && key.modifiers.is_empty(),
        );
        if self.completion.active() && key.code == KeyCode::Esc && key.modifiers.is_empty() {
            self.completion.dismiss();
            return Action::None;
        }
        if key.modifiers.is_empty()
            && (self.completion.visible() || (self.completion.active() && key.code == KeyCode::Tab))
        {
            match key.code {
                KeyCode::Up | KeyCode::Down => {
                    self.completion.move_selection(key.code == KeyCode::Down);
                    return Action::None;
                }
                KeyCode::Tab | KeyCode::Enter => {
                    if key.code == KeyCode::Tab && !menu_was_visible && !self.completion.unique() {
                        return Action::None;
                    }
                    if let Some((range, text, cursor)) = self.completion.replacement(&self.draft) {
                        if self.draft.len() - range.len() + text.len() <= MAX_DRAFT {
                            self.draft.replace_range(range, &text);
                            self.cursor = cursor;
                            self.browsing = None;
                        } else {
                            self.status = format!("Prompt is limited to {MAX_DRAFT} bytes");
                        }
                    }
                    self.completion.clear();
                    return Action::None;
                }
                _ => {}
            }
        }
        let action = match key {
            KeyEvent {
                code: KeyCode::Char('c'),
                modifiers,
            } if modifiers.contains(Modifiers::CONTROL) => {
                if self.draft.is_empty() {
                    Action::Quit
                } else {
                    self.replace_draft(String::new());
                    Action::None
                }
            }
            KeyEvent {
                code: KeyCode::Char('d'),
                modifiers,
            } if modifiers.contains(Modifiers::CONTROL) && self.draft.is_empty() => Action::Quit,
            KeyEvent {
                code: KeyCode::Tab, ..
            } => {
                let start = self.draft[..self.cursor]
                    .rfind(char::is_whitespace)
                    .map_or(0, |at| at + 1);
                let token = &self.draft[start..self.cursor];
                if let Some(query) = token.strip_prefix('@') {
                    self.open_file_picker(start, self.cursor, query.to_owned());
                } else {
                    self.insert("\t");
                }
                Action::None
            }
            KeyEvent {
                code: KeyCode::Char('o'),
                modifiers,
            } if modifiers.contains(Modifiers::CONTROL) => {
                self.open_details(None);
                Action::None
            }
            KeyEvent {
                code: KeyCode::Char('x'),
                modifiers,
            } if modifiers.contains(Modifiers::CONTROL) => Action::Command("/copy".into()),
            KeyEvent {
                code: KeyCode::Char('g'),
                modifiers,
            } if modifiers.contains(Modifiers::CONTROL) => Action::Command("/editor".into()),
            KeyEvent {
                code: KeyCode::Char('v'),
                modifiers,
            } if modifiers.contains(Modifiers::CONTROL) => Action::PasteClipboard,
            KeyEvent {
                code: KeyCode::Enter,
                modifiers,
            } if modifiers.contains(Modifiers::SHIFT) || modifiers.contains(Modifiers::CONTROL) => {
                self.insert("\n");
                Action::None
            }
            KeyEvent {
                code: KeyCode::Enter,
                modifiers,
            } if modifiers.contains(Modifiers::ALT) => {
                self.browsing = None;
                let prompt = std::mem::take(&mut self.draft);
                self.cursor = 0;
                if prompt.trim().is_empty() {
                    Action::None
                } else {
                    Action::Queue(prompt)
                }
            }
            KeyEvent {
                code: KeyCode::Char('j'),
                modifiers,
            } if modifiers.contains(Modifiers::CONTROL) => {
                self.insert("\n");
                Action::None
            }
            KeyEvent {
                code: KeyCode::Char('@'),
                modifiers,
            } if !modifiers.contains(Modifiers::CONTROL) && !modifiers.contains(Modifiers::ALT) => {
                let start = self.cursor;
                self.insert("@");
                self.open_file_picker(start, self.cursor, String::new());
                Action::None
            }
            KeyEvent {
                code: KeyCode::Enter,
                ..
            } => {
                self.browsing = None;
                let prompt = std::mem::take(&mut self.draft);
                self.cursor = 0;
                let syntax = prompt.trim();
                if syntax.is_empty() && self.images.is_empty() {
                    Action::None
                } else if Builtin::parse(syntax) == Some(Builtin::Quit) {
                    Action::Quit
                } else if syntax.starts_with('/') {
                    Action::Command(syntax.to_owned())
                } else if let Some(command) = syntax.strip_prefix("!!") {
                    if command.trim().is_empty() {
                        self.draft = prompt;
                        self.cursor = self.draft.len();
                        self.status = "Type a shell command after !!".into();
                        Action::None
                    } else {
                        Action::Shell(command.trim().to_owned(), true)
                    }
                } else if let Some(command) = syntax.strip_prefix('!') {
                    if command.trim().is_empty() {
                        self.draft = prompt;
                        self.cursor = self.draft.len();
                        self.status = "Type a shell command after !".into();
                        Action::None
                    } else {
                        Action::Shell(command.trim().to_owned(), false)
                    }
                } else {
                    Action::Submit(prompt)
                }
            }
            KeyEvent {
                code: KeyCode::Backspace,
                ..
            } => {
                self.browsing = None;
                let start = previous_grapheme(&self.draft, self.cursor);
                self.draft.replace_range(start..self.cursor, "");
                self.cursor = start;
                Action::None
            }
            KeyEvent {
                code: KeyCode::Delete,
                ..
            } => {
                self.browsing = None;
                let end = next_grapheme(&self.draft, self.cursor);
                self.draft.replace_range(self.cursor..end, "");
                Action::None
            }
            KeyEvent {
                code: KeyCode::Left,
                ..
            } => {
                self.cursor = previous_grapheme(&self.draft, self.cursor);
                Action::None
            }
            KeyEvent {
                code: KeyCode::Right,
                ..
            } => {
                self.cursor = next_grapheme(&self.draft, self.cursor);
                Action::None
            }
            KeyEvent {
                code: KeyCode::Up,
                modifiers,
            } if modifiers.contains(Modifiers::ALT) => {
                self.dequeue();
                Action::None
            }
            KeyEvent {
                code: KeyCode::Up, ..
            } => {
                if let Some(cursor) = vertical_cursor(&self.draft, self.cursor, false) {
                    self.cursor = cursor;
                } else {
                    self.history_previous();
                }
                Action::None
            }
            KeyEvent {
                code: KeyCode::Down,
                ..
            } => {
                if let Some(cursor) = vertical_cursor(&self.draft, self.cursor, true) {
                    self.cursor = cursor;
                } else {
                    self.history_next();
                }
                Action::None
            }
            KeyEvent {
                code: KeyCode::Home,
                ..
            } => {
                self.cursor = self.draft[..self.cursor].rfind('\n').map_or(0, |at| at + 1);
                Action::None
            }
            KeyEvent {
                code: KeyCode::End, ..
            } => {
                self.cursor = self.draft[self.cursor..]
                    .find('\n')
                    .map_or(self.draft.len(), |at| self.cursor + at);
                Action::None
            }
            KeyEvent {
                code: KeyCode::PageUp,
                ..
            } => {
                self.scroll = self.scroll.saturating_add(10);
                Action::None
            }
            KeyEvent {
                code: KeyCode::PageDown,
                ..
            } => {
                self.scroll = self.scroll.saturating_sub(10);
                Action::None
            }
            KeyEvent {
                code: KeyCode::Char(ch),
                modifiers,
            } if !modifiers.contains(Modifiers::CONTROL) && !modifiers.contains(Modifiers::ALT) => {
                let mut bytes = [0; 4];
                self.insert(ch.encode_utf8(&mut bytes));
                Action::None
            }
            _ => Action::None,
        };
        if self.picker.is_some() || self.details.is_some() {
            self.completion.clear();
        } else {
            self.completion
                .refresh(&self.draft, self.cursor, resources, false);
        }
        action
    }

    fn picker_key(&mut self, key: KeyEvent) -> Action {
        let picker = self.picker.as_mut().expect("picker is active");
        match key.code {
            KeyCode::Esc => {
                self.picker = None;
                Action::None
            }
            KeyCode::Char('c') if key.modifiers.contains(Modifiers::CONTROL) => {
                self.picker = None;
                Action::None
            }
            KeyCode::Up => {
                picker.selected = picker.selected.saturating_sub(1);
                Action::None
            }
            KeyCode::Down => {
                picker.selected =
                    (picker.selected + 1).min(picker.matches().len().saturating_sub(1));
                Action::None
            }
            KeyCode::Backspace => {
                picker.query.pop();
                picker.selected = 0;
                Action::None
            }
            KeyCode::Char(ch)
                if !key.modifiers.contains(Modifiers::CONTROL)
                    && !key.modifiers.contains(Modifiers::ALT) =>
            {
                picker.query.push(ch);
                picker.selected = 0;
                Action::None
            }
            KeyCode::Enter => {
                let matching = picker.matches();
                let selected = matching
                    .get(picker.selected)
                    .and_then(|index| picker.items.get(*index));
                let value = selected.map(|item| match &item.value {
                    PickerValue::Session(path) => PickerValue::Session(path.clone()),
                    PickerValue::Model(model) => PickerValue::Model(model.clone()),
                    PickerValue::ForkBefore { turn, input } => PickerValue::ForkBefore {
                        turn: *turn,
                        input: input.clone(),
                    },
                    PickerValue::File { path, start, end } => PickerValue::File {
                        path: path.clone(),
                        start: *start,
                        end: *end,
                    },
                });
                self.picker = None;
                value.map_or(Action::None, Action::Pick)
            }
            _ => Action::None,
        }
    }

    fn detail_key(&mut self, key: KeyEvent) -> Action {
        if key.code == KeyCode::Esc
            || (key.code == KeyCode::Char('o') && key.modifiers == Modifiers::CONTROL)
        {
            self.details = None;
            self.status = "Details closed".into();
        } else if let Some(view) = &mut self.details {
            view.key(key.code);
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
        self.note(if names.is_empty() {
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

    fn open_file_picker(&mut self, start: usize, end: usize, query: String) {
        let files = scan_files(&self.cwd);
        if files.is_empty() {
            self.status = "No project files available for completion".into();
            return;
        }
        let items = files
            .into_iter()
            .map(|path| PickerItem {
                label: path.clone(),
                value: PickerValue::File { path, start, end },
            })
            .collect();
        self.picker = Some(Picker {
            title: "Choose file",
            query,
            selected: 0,
            items,
        });
    }

    fn insert_file(&mut self, path: String, start: usize, end: usize) {
        if end > self.draft.len() || start > end {
            return;
        }
        let mention = if path.chars().any(char::is_whitespace) {
            format!("@\"{path}\"")
        } else {
            format!("@{path}")
        };
        if self.draft.len() - (end - start) + mention.len() > MAX_DRAFT {
            self.status = format!("Prompt is limited to {MAX_DRAFT} bytes");
            return;
        }
        self.draft.replace_range(start..end, &mention);
        self.cursor = start + mention.len();
        self.browsing = None;
    }
    fn insert(&mut self, text: &str) {
        self.completion.clear();
        if let Some(picker) = &mut self.picker {
            picker.query.push_str(
                &text
                    .chars()
                    .filter(|ch| !ch.is_control())
                    .collect::<String>(),
            );
            picker.selected = 0;
            return;
        }
        let clean = text
            .chars()
            .filter_map(|ch| match ch {
                '\n' | '\t' => Some(ch),
                ch if ch.is_control() => None,
                ch => Some(ch),
            })
            .collect::<String>();
        if self.draft.len().saturating_add(clean.len()) > MAX_DRAFT {
            self.status = format!("Prompt is limited to {MAX_DRAFT} bytes");
            return;
        }
        self.browsing = None;
        self.draft.insert_str(self.cursor, &clean);
        self.cursor += clean.len();
    }

    fn history_previous(&mut self) {
        if self.prompt_history.is_empty() {
            return;
        }
        let browse = self.browsing.get_or_insert_with(|| PromptBrowse {
            index: self.prompt_history.len(),
            end: self.prompt_history.len(),
            draft: self.draft.clone(),
            cursor: self.cursor,
        });
        browse.index = browse.index.saturating_sub(1);
        self.draft = self.prompt_history[browse.index].clone();
        self.cursor = self.draft.len();
    }

    fn history_next(&mut self) {
        let Some(browse) = &mut self.browsing else {
            return;
        };
        if browse.index + 1 < browse.end {
            browse.index += 1;
            self.draft = self.prompt_history[browse.index].clone();
            self.cursor = self.draft.len();
        } else {
            self.restore_browsed_draft();
        }
    }

    fn queue_follow_up(
        &mut self,
        original: String,
        limits: AgentLimits,
        resources: Option<&Resources>,
    ) {
        let name = original.split_whitespace().next().unwrap_or("");
        if Builtin::parse(name).is_some() {
            self.draft = original;
            self.cursor = self.draft.len();
            self.status =
                "Terminal commands cannot be queued; submit a prompt or resource command".into();
            return;
        }
        let prompt = match resources {
            Some(resources) => match expand_resource_input(resources, original.clone()) {
                Ok(prompt) => prompt,
                Err((_, error)) => {
                    self.draft = original;
                    self.cursor = self.draft.len();
                    self.status = format!("Follow-up was not queued: {error:#}");
                    return;
                }
            },
            None => original.clone(),
        };
        let message = Message::user_input(prompt.clone(), self.images.clone());
        match self.input_budget.reserve(&message, &(), limits) {
            Ok(reservation) => {
                self.pending.push_back(TurnInput {
                    prompt,
                    images: std::mem::take(&mut self.images),
                    _reservation: Some(reservation),
                });
                self.status = format!("{} follow-up(s) queued", self.pending.len());
            }
            Err(error) => {
                self.draft = original;
                self.cursor = self.draft.len();
                self.status = format!("Follow-up was not queued: {error}");
            }
        }
    }

    fn dequeue(&mut self) {
        if let Some(TurnInput { prompt, images, .. }) = self.pending.pop_back() {
            self.restore_browsed_draft();
            self.images.extend(images);
            if self.draft.is_empty() {
                self.draft = prompt;
            } else {
                self.draft = format!("{}\n\n{prompt}", self.draft);
            }
            self.cursor = self.draft.len();
            self.browsing = None;
            self.status = format!("{} follow-up(s) remain queued", self.pending.len());
        }
    }
}

fn vertical_cursor(draft: &str, cursor: usize, down: bool) -> Option<usize> {
    let line_start = draft[..cursor].rfind('\n').map_or(0, |at| at + 1);
    let column = draft[line_start..cursor].graphemes(true).count();
    if down {
        let next_start = draft[cursor..].find('\n').map(|at| cursor + at + 1)?;
        let next_end = draft[next_start..]
            .find('\n')
            .map_or(draft.len(), |at| next_start + at);
        Some(
            draft[next_start..next_end]
                .grapheme_indices(true)
                .nth(column)
                .map_or(next_end, |(at, _)| next_start + at),
        )
    } else {
        let previous_end = line_start.checked_sub(1)?;
        let previous_start = draft[..previous_end].rfind('\n').map_or(0, |at| at + 1);
        Some(
            draft[previous_start..previous_end]
                .grapheme_indices(true)
                .nth(column)
                .map_or(previous_end, |(at, _)| previous_start + at),
        )
    }
}

fn draw(
    terminal: &mut TerminalSession,
    screen: &mut Screen,
    ui: &mut Frontend,
    progress: Option<&LiveTranscript>,
    operation: Option<ActiveOperation<'_>>,
) -> Result<()> {
    let (width, height) = terminal.size()?;
    screen.resize(width, height);

    if ui.details.is_some() || ui.picker.is_some() {
        terminal.enter_alt_screen()?;
        return draw_modal_fullscreen(terminal, screen, ui, progress, operation, width, height);
    }
    if ui.mode == TuiMode::Fullscreen {
        terminal.enter_alt_screen()?;
        return draw_chat_fullscreen(terminal, screen, ui, progress, operation, width, height);
    }

    let mut surface_reset = false;
    if terminal.is_alt_screen()? {
        terminal.leave_alt_screen()?;
        screen.invalidate();
        surface_reset = true;
    }

    let commit_rows = ui.pending_history_rows(width.max(1) as usize);
    let history_committed = !commit_rows.is_empty();
    if history_committed {
        screen.commit_lines(&mut terminal.output()?, &commit_rows)?;
    }
    if !ui.pending_history_items.is_empty() || ui.pending_history_banner.is_some() {
        ui.finish_history_commit();
    }

    let width = width.max(1) as usize;
    let row_budget = LIVE_REGION_MAX_ROWS.min(height.max(1) as usize);
    let mut chrome = Vec::new();
    let notices = progress.map_or(&[][..], LiveTranscript::notices);
    if operation.is_none() {
        // Idle command output (help, resources, diagnostics) is content, not
        // busy chrome. Keep its wrapped rows rather than a one-line preview.
        for notice in &ui.notices {
            push_wrapped(&mut chrome, notice, width);
        }
    } else if let Some(notice) = notices.last().or(ui.notices.last()) {
        let count = notices.len() + ui.notices.len();
        let label = if count == 1 {
            "Notice".into()
        } else {
            format!("{count} notices")
        };
        chrome.push(fit_line(&format!("{label} · {notice}"), width));
    }
    if let Some(status) = visible_status(ui, operation) {
        if operation.is_some() {
            chrome.push(fit_line(&status, width));
        } else {
            push_wrapped(&mut chrome, &status, width);
        }
    }

    let composer = wrap_input(&ui.draft, ui.cursor, width);
    let composer_height = composer.lines.len().min(4);
    let composer_start = composer
        .cursor_row
        .saturating_sub(composer_height.saturating_sub(1))
        .min(composer.lines.len().saturating_sub(composer_height));
    let progress_height = usize::from(operation.is_some()) * 2;
    let status_height = usize::from(visible_status(ui, operation).is_some());
    let completions = ui.completion.rows(
        width,
        row_budget.saturating_sub(composer_height + progress_height + status_height),
    );
    let chrome_budget =
        row_budget.saturating_sub(composer_height + progress_height + completions.len());
    // Discovery is temporary composer chrome. Older notices must not hide a
    // keyboard-active menu or displace the operation's stop/status controls.
    if chrome.len() > chrome_budget {
        chrome.drain(..chrome.len() - chrome_budget);
    }
    chrome.extend(completions);
    let content_budget = row_budget.saturating_sub(composer_height + chrome.len());
    let mut live_rows = progress.map_or_else(Vec::new, |progress| {
        crate::transcript_render::live_rows(
            progress.projection(),
            width,
            content_budget,
            ui.thinking_visibility,
        )
    });
    live_rows.extend(chrome.into_iter().map(Line::raw));
    let composer_offset = live_rows.len();
    for line in composer
        .lines
        .iter()
        .skip(composer_start)
        .take(composer_height)
    {
        live_rows.push(Line::raw(line.clone()));
    }
    let mut cursor_row = composer_offset + composer.cursor_row.saturating_sub(composer_start);

    let desired_live_height = live_rows.len().clamp(1, row_budget);
    if desired_live_height > screen.live_height() {
        screen.ensure_live_height(&mut terminal.output()?, desired_live_height)?;
    } else if desired_live_height < screen.live_height() && (history_committed || surface_reset) {
        // The settled-history commit or fullscreen exit erased/replaced the
        // mutable surface, so shrinking cannot leak stale rows into scrollback.
        screen.set_live_height(desired_live_height);
    }
    let live_height = screen.live_height();
    if live_rows.len() > live_height {
        let drop = live_rows.len() - live_height;
        live_rows.drain(..drop);
        cursor_row = cursor_row.saturating_sub(drop);
    }

    let live = live_rows;
    let cursor = (cursor_row < live.len()).then_some((
        cursor_row,
        composer.cursor_col.min(width.saturating_sub(1)) as u16,
    ));
    terminal.render(
        screen,
        &Frame {
            live: &live,
            cursor,
        },
    )?;
    Ok(())
}

fn visible_status(ui: &Frontend, operation: Option<ActiveOperation<'_>>) -> Option<String> {
    let idle = ui.status.is_empty();
    let attachment = if ui.images.is_empty() {
        String::new()
    } else {
        format!(" · {} image(s) attached", ui.images.len())
    };
    let status = if idle {
        attachment.trim_start_matches(" · ").to_owned()
    } else {
        format!("{}{}", ui.status, attachment)
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
            let controls = match operation {
                ActiveOperation::Coding(_) => "Enter steers · Alt-Enter queues · Ctrl-C cancels",
                _ => "Ctrl-C cancels",
            };
            format!("{label} · {controls}")
        } else {
            format!("{label} · Ctrl-C cancels · {status}")
        });
    }
    (!status.is_empty()).then_some(status)
}

fn draw_chat_fullscreen(
    terminal: &mut TerminalSession,
    screen: &mut Screen,
    ui: &mut Frontend,
    progress: Option<&LiveTranscript>,
    operation: Option<ActiveOperation<'_>>,
    width: u16,
    height: u16,
) -> Result<()> {
    let width = width.max(1) as usize;
    let height = height.max(1) as usize;
    let mut content = crate::transcript_render::rows(
        &ui.history,
        width,
        ui.output_detail,
        ui.thinking_visibility,
    );

    for notice in &ui.notices {
        if !content.is_empty() && content.last().is_some_and(|row| row.width() > 0) {
            content.push(Line::default());
        }
        content.extend(notice_rows(notice, width));
    }
    if let Some(progress) = progress {
        let live = crate::transcript_render::rows(
            progress.projection(),
            width,
            ui.output_detail,
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

    let composer = wrap_input(&ui.draft, ui.cursor, width);
    let composer_height = composer.lines.len().min(4);
    let composer_start = composer
        .cursor_row
        .saturating_sub(composer_height.saturating_sub(1))
        .min(composer.lines.len().saturating_sub(composer_height));
    let status = visible_status(ui, operation);
    let status_height = usize::from(status.is_some());
    let completions = ui.completion.rows(
        width,
        height.saturating_sub(composer_height + status_height),
    );
    let viewport = height.saturating_sub(composer_height + status_height + completions.len());

    let end = content.len().saturating_sub(ui.scroll);
    let start = end.saturating_sub(viewport);
    let mut rows = vec![Line::raw(""); height];
    let padding = viewport.saturating_sub(end.saturating_sub(start));
    for (index, row) in content[start..end].iter().enumerate() {
        rows[padding + index] = row.clone();
    }

    let mut next_row = viewport;
    if let Some(status) = status
        && next_row < height
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
    for (index, line) in composer
        .lines
        .iter()
        .skip(composer_start)
        .take(composer_height)
        .enumerate()
    {
        if next_row + index < height {
            rows[next_row + index] = Line::raw(line.clone());
        }
    }
    let cursor_row = next_row + composer.cursor_row.saturating_sub(composer_start);
    let cursor = (cursor_row < height).then_some((
        cursor_row,
        composer.cursor_col.min(width.saturating_sub(1)) as u16,
    ));

    screen.draw_fullscreen(&mut terminal.output()?, &rows, cursor)?;
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
    progress: Option<&LiveTranscript>,
    operation: Option<ActiveOperation<'_>>,
    width: u16,
    height: u16,
) -> Result<()> {
    let width = width.max(1) as usize;
    let height = height.max(1) as usize;
    let mut content = Vec::new();
    let mut composer = None;

    if let Some(view) = &mut ui.details {
        view.prepare(&ui.history, progress, width);
    }
    if let Some(picker) = &ui.picker {
        let matching = picker.matches();
        content.push(format!("{} · {} match(es)", picker.title, matching.len()));
        let visible = height.saturating_sub(2);
        let start = picker.selected.saturating_sub(visible.saturating_sub(1));
        for (index, item) in matching.iter().enumerate().skip(start).take(visible) {
            let label = &picker.items[*item].label;
            content.push(format!(
                "{} {}",
                if index == picker.selected { '›' } else { ' ' },
                fit_line(label, width.saturating_sub(2))
            ));
        }
        composer = Some(wrap_input(&picker.query, picker.query.len(), width));
    }

    let content = ui
        .details
        .as_ref()
        .map_or(content.as_slice(), DetailView::rows);
    let controls = ui.details.as_ref().map(DetailView::controls);
    let status = visible_status(ui, operation);
    let composer_height = composer
        .as_ref()
        .map_or(0, |composer| composer.lines.len().min(3));
    let status_height = usize::from(controls.is_some()) + usize::from(status.is_some());
    let viewport = height.saturating_sub(composer_height + status_height);
    let scroll = ui.details.as_ref().map_or(0, |view| view.scroll);
    let end = content.len().saturating_sub(scroll.min(content.len()));
    let start = end.saturating_sub(viewport);
    let mut rows = vec![Line::raw(""); height];
    let padding = viewport.saturating_sub(end - start);
    for (index, row) in content[start..end].iter().enumerate() {
        rows[padding + index] = Line::raw(row.clone());
    }

    let mut cursor = None;
    let mut next_row = viewport;
    for label in controls.iter().chain(status.iter()) {
        if next_row < height {
            rows[next_row] = Line::raw(fit_line(label, width));
            next_row += 1;
        }
    }
    if let Some(composer) = composer {
        let start = composer
            .cursor_row
            .saturating_sub(composer_height.saturating_sub(1))
            .min(composer.lines.len().saturating_sub(composer_height));
        for (index, line) in composer
            .lines
            .iter()
            .skip(start)
            .take(composer_height)
            .enumerate()
        {
            if next_row + index < height {
                rows[next_row + index] = Line::raw(line.clone());
            }
        }
        let row = next_row + composer.cursor_row.saturating_sub(start);
        if row < height {
            cursor = Some((row, composer.cursor_col.min(width.saturating_sub(1)) as u16));
        }
    }

    screen.draw_fullscreen(&mut terminal.output()?, &rows, cursor)?;
    Ok(())
}

struct WrappedInput {
    lines: Vec<String>,
    cursor_row: usize,
    cursor_col: usize,
}
fn wrap_input(draft: &str, cursor: usize, width: usize) -> WrappedInput {
    let width = width.max(3);
    let mut lines = Vec::new();
    let mut line = "› ".to_owned();
    let mut col = 2;
    let mut position = (0, 2);
    for (byte, grapheme) in draft.grapheme_indices(true) {
        if grapheme == "\n" {
            if byte == cursor {
                position = (lines.len(), col);
            }
            lines.push(line);
            line = "  ".into();
            col = 2;
            continue;
        }
        let display = if grapheme == "\t" { "    " } else { grapheme };
        let size = UnicodeWidthStr::width(display).max(1);
        if col + size > width && col > 2 {
            lines.push(line);
            line = "  ".into();
            col = 2;
        }
        if byte == cursor {
            position = (lines.len(), col);
        }
        line.push_str(display);
        col += size;
    }
    if cursor == draft.len() {
        if col >= width {
            lines.push(line);
            line = "  ".into();
            col = 2;
        }
        position = (lines.len(), col);
    }
    lines.push(line);
    WrappedInput {
        lines,
        cursor_row: position.0,
        cursor_col: position.1,
    }
}

fn previous_grapheme(text: &str, cursor: usize) -> usize {
    text[..cursor]
        .grapheme_indices(true)
        .next_back()
        .map_or(0, |(at, _)| at)
}
fn next_grapheme(text: &str, cursor: usize) -> usize {
    text[cursor..]
        .graphemes(true)
        .next()
        .map_or(cursor, |g| cursor + g.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_history_tail_keeps_the_last_six_turns() {
        let items = (1..=9)
            .map(|turn| {
                TranscriptItem::User(ion_core::TranscriptMessage {
                    turn: Some(turn),
                    steering: false,
                    parts: vec![ion_core::TranscriptPart::Text(format!("turn-{turn}"))],
                })
            })
            .collect::<Vec<_>>();
        let tail = resume_history_tail(&items);
        assert_eq!(tail.start, 3);
        assert_eq!(tail.omitted_turns, 3);
        assert_eq!(transcript_item_turn(&items[tail.start]), Some(4));
    }

    #[test]
    fn resume_history_tail_bounds_shell_only_history() {
        let items = (0..40)
            .map(|index| {
                TranscriptItem::UserShell(ion_core::UserShellActivity {
                    command: format!("echo {index}"),
                    outcome: ion_core::UserShellOutcome::Observed {
                        output: serde_json::Value::Null,
                        is_error: false,
                    },
                    exclude_from_context: false,
                })
            })
            .collect::<Vec<_>>();
        let tail = resume_history_tail(&items);
        assert_eq!(tail.start, 8);
        assert_eq!(tail.omitted_turns, 0);
        assert_eq!(tail.omitted_entries, 8);
    }

    #[test]
    fn copy_uses_the_last_completed_answer() {
        let assistant = |turn, text: &str| SessionEntry::Assistant {
            turn,
            message: Message {
                role: Role::Assistant,
                content: vec![Content::Text(text.into())],
                provider_replay: None,
            },
            tool_activities: Vec::new(),
            execution: ion_ai::ModelExecution {
                route: ion_ai::ModelRoute::direct(
                    ModelRef {
                        provider: "test".into(),
                        model: "test".into(),
                    },
                    ion_ai::ModelRouteReason::UserRequest,
                ),
                returned_model: None,
            },
            usage: ion_ai::Usage::unknown(),
            termination: ion_ai::ResponseTermination::Completed,
        };
        let view = SessionView {
            cwd: PathBuf::from("/tmp"),
            name: None,
            entries: vec![
                assistant(1, "finished"),
                SessionEntry::TurnEnded {
                    turn: 1,
                    reason: TurnEndReason::Completed,
                },
                assistant(2, "partial"),
            ],
            messages: vec![],
            unfinished_turn: Some(2),
            unfinished_user_shell: None,
            last_end: None,
            last_model: None,
            last_effective_model: None,
            last_context: None,
            compacted_through: None,
            last_execution: None,
            last_usage: None,
        };
        assert_eq!(last_committed_answer(&view).unwrap(), "finished");
    }
    #[test]
    fn composer_keeps_unicode_cursor_across_lines() {
        let draft = "ab🦀\nnext";
        let input = wrap_input(draft, draft.len(), 8);
        assert_eq!(input.lines, vec!["› ab🦀", "  next"]);
        assert_eq!((input.cursor_row, input.cursor_col), (1, 6));
        assert_eq!(previous_grapheme(draft, 6), 2);
    }
    #[test]
    fn display_replaces_terminal_controls() {
        let mut rows = Vec::new();
        push_wrapped(&mut rows, "safe\u{1b}[31m", 30);
        assert_eq!(rows, vec!["safe�[31m"]);
    }
    #[test]
    fn alt_enter_queues_a_followup_without_discarding_the_editor() {
        let mut ui = Frontend::default();
        ui.insert("follow up");
        assert!(matches!(
            ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::ALT), None),
            Action::Queue(prompt) if prompt == "follow up"
        ));
        assert!(ui.draft.is_empty());
    }

    #[test]
    fn output_settings_during_work_do_not_republish_or_admit_input() {
        let mut ui = Frontend {
            history_published_items: 7,
            pending_history_target: 7,
            ..Frontend::default()
        };
        let stop = CancellationToken::new();
        ui.draft = "/settings expanded".into();
        ui.cursor = ui.draft.len();
        busy_key(
            &mut ui,
            KeyEvent::new(KeyCode::Enter, Modifiers::NONE),
            &stop,
            None,
            None,
            AgentLimits::default(),
        );
        assert_eq!(ui.output_detail, crate::tool_output::OutputDetail::Expanded);
        assert!(!stop.is_cancelled() && ui.pending.is_empty());
        assert!(ui.draft.is_empty());
        assert_eq!(
            (ui.history_published_items, ui.pending_history_target),
            (7, 7)
        );
        assert!(ui.pending_history_items.is_empty() && ui.pending_history_banner.is_none());
        assert!(ui.status.contains("native history unchanged"));
        assert!(ui.output_settings("unknown").is_err());
        assert_eq!(ui.output_detail, crate::tool_output::OutputDetail::Expanded);
        ui.output_settings("").unwrap();
        assert!(
            ui.notices
                .last()
                .unwrap()
                .contains("Inline progress stays compact")
        );
        ui.output_settings("compact").unwrap();
        assert_eq!(ui.output_detail, crate::tool_output::OutputDetail::Compact);
        ui.draft = "/settings thinking show".into();
        ui.cursor = ui.draft.len();
        busy_key(
            &mut ui,
            KeyEvent::new(KeyCode::Enter, Modifiers::NONE),
            &stop,
            None,
            None,
            AgentLimits::default(),
        );
        assert_eq!(
            ui.thinking_visibility,
            crate::transcript_render::ThinkingVisibility::Visible
        );
        assert_eq!(ui.output_detail, crate::tool_output::OutputDetail::Compact);
        assert!(
            !stop.is_cancelled() && ui.pending.is_empty() && ui.pending_history_items.is_empty()
        );
        assert_eq!(
            (ui.history_published_items, ui.pending_history_target),
            (7, 7)
        );
        assert!(ui.output_settings("thinking sometimes").is_err());
        assert_eq!(
            ui.thinking_visibility,
            crate::transcript_render::ThinkingVisibility::Visible
        );
        ui.output_settings("thinking hide").unwrap();
        assert_eq!(
            ui.thinking_visibility,
            crate::transcript_render::ThinkingVisibility::Hidden
        );
    }

    #[test]
    fn operation_status_survives_notices_and_tracks_cancellation() {
        let stop = CancellationToken::new();
        let operations = [
            (
                ActiveOperation::Coding(&stop),
                "Working",
                "Cancelling turn…",
            ),
            (
                ActiveOperation::Shell(&stop),
                "Running shell",
                "Cancelling shell…",
            ),
            (
                ActiveOperation::Compaction(&stop),
                "Summarizing context",
                "Cancelling compaction…",
            ),
        ];
        let mut ui = Frontend::default();
        assert_eq!(visible_status(&ui, None), None);
        for notice in [
            "Details closed",
            "Steering sent for the next model step",
            "1 follow-up(s) queued",
        ] {
            ui.status = notice.into();
            for (operation, label, _) in operations {
                let status = visible_status(&ui, Some(operation)).unwrap();
                assert!(status.starts_with(label), "{status}");
                assert!(status.contains(notice), "{status}");
                assert!(status.contains("Ctrl-C cancels"), "{status}");
            }
        }
        stop.cancel();
        ui.status = "Details closed".into();
        for (operation, _, label) in operations {
            let status = visible_status(&ui, Some(operation)).unwrap();
            assert!(status.starts_with(label), "{status}");
            assert!(status.contains("Details closed"), "{status}");
            assert!(!status.contains("Ctrl-C cancels"), "{status}");
        }
        assert_eq!(visible_status(&ui, None).unwrap(), "Details closed");
    }

    #[test]
    fn history_refresh_preserves_browsed_draft_and_cursor() {
        let mut ui = Frontend {
            prompt_history: vec!["older".into()],
            draft: "unsent 🦀 draft".into(),
            cursor: "unsent ".len(),
            ..Frontend::default()
        };
        ui.history_previous();
        assert_eq!(ui.draft, "older");
        // Completion/model refresh updates history, not ownership of the editor.
        ui.replace_prompt_history(vec!["older".into(), "newly committed".into()], true);
        ui.history_next();
        assert_eq!(ui.draft, "unsent 🦀 draft");
        assert_eq!(ui.cursor, "unsent ".len());
        ui.history_previous();
        ui.replace_prompt_history(vec!["different session".into()], false);
        assert_eq!(ui.draft, "unsent 🦀 draft");
        assert_eq!(ui.cursor, "unsent ".len());
    }

    #[test]
    fn recovered_input_and_explicit_clear_end_history_browsing() {
        let mut ui = Frontend {
            prompt_history: vec!["older".into()],
            draft: "unsent".into(),
            cursor: 6,
            ..Frontend::default()
        };
        ui.history_previous();
        ui.pending
            .push_back(TurnInput::direct("recovered".into(), vec![]));
        return_pending_to_editor(&mut ui);
        ui.history_next();
        assert_eq!(ui.draft, "recovered\n\nunsent");
        ui.history_previous();
        ui.key(KeyEvent::new(KeyCode::Char('c'), Modifiers::CONTROL), None);
        ui.history_next();
        assert!(ui.draft.is_empty());
        ui.insert("another unsent");
        ui.history_previous();
        ui.replace_draft("editor replacement".into());
        ui.history_next();
        assert_eq!(ui.draft, "editor replacement");
    }

    #[test]
    fn clipboard_paths_are_quoted_for_shell_and_reject_controls() {
        let paths = vec![PathBuf::from("/tmp/a b.png"), PathBuf::from("/tmp/code.rs")];
        assert_eq!(
            clipboard_paths(&paths, false).unwrap(),
            "/tmp/a b.png\n/tmp/code.rs"
        );
        assert_eq!(
            clipboard_paths(&paths, true).unwrap(),
            "'/tmp/a b.png' /tmp/code.rs"
        );
        assert!(clipboard_paths(&[PathBuf::from("/tmp/bad\nname")], false).is_err());
        let mut ui = Frontend::default();
        assert!(matches!(
            ui.key(KeyEvent::new(KeyCode::Char('v'), Modifiers::CONTROL), None),
            Action::PasteClipboard
        ));
    }

    #[tokio::test]
    async fn completed_file_paste_clears_reader_status() {
        let mut ui = Frontend {
            status: "Reading clipboard…".into(),
            clipboard_job: Some(tokio::spawn(async {
                Ok(PreparedPaste::Files(vec![PathBuf::from(
                    "/tmp/path with spaces.txt",
                )]))
            })),
            ..Frontend::default()
        };
        finish_clipboard_paste(&mut ui).await.unwrap();
        assert_eq!(ui.draft, "/tmp/path with spaces.txt");
        assert!(ui.status.is_empty());
    }

    #[test]
    fn clipboard_image_during_turn_stays_with_typed_steering() {
        let selected = Selection {
            provider: "local".into(),
            model: "vision".into(),
            endpoint: "http://127.0.0.1:1".into(),
            wire: ion_host::HttpWire::ChatCompletions,
            api_key_env: None,
            max_output_tokens: 1024,
            context_window_tokens: None,
            requires_key: false,
            image_input: true,
            capabilities: ion_host::catalog::ModelCapabilities::conservative(),
        };
        let mut ui = Frontend::default();
        let prepared = prepare_clipboard(
            &selected,
            crate::clipboard::PasteContent::Image {
                width: 2,
                height: 1,
                rgba: vec![255, 0, 0, 255, 0, 0, 255, 255],
            },
        )
        .unwrap();
        apply_clipboard(&mut ui, prepared).unwrap();
        ui.insert("describe the picture");
        let steering = SteeringInbox::new(
            AgentLimits {
                image_input: true,
                ..AgentLimits::default()
            },
            ui.input_budget.clone(),
        );
        busy_key(
            &mut ui,
            KeyEvent::new(KeyCode::Enter, Modifiers::NONE),
            &CancellationToken::new(),
            Some(&steering),
            None,
            AgentLimits {
                image_input: true,
                ..AgentLimits::default()
            },
        );
        let queued = steering.take_uncommitted();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].message().content.len(), 2);
        assert_eq!(
            queued[0].message().content[0],
            Content::Text("describe the picture".into())
        );
        assert!(matches!(queued[0].message().content[1], Content::Image(_)));
        assert!(ui.pending.is_empty());
        assert!(ui.images.is_empty());
    }

    #[tokio::test]
    async fn busy_submit_waits_for_pending_clipboard_read() {
        let mut ui = Frontend::default();
        ui.insert("describe this");
        ui.status = "Reading clipboard…".into();
        ui.clipboard_job = Some(tokio::spawn(async {
            Ok(PreparedPaste::Text(" image".into()))
        }));
        let steering = SteeringInbox::new(
            AgentLimits {
                image_input: true,
                ..AgentLimits::default()
            },
            ui.input_budget.clone(),
        );
        busy_key(
            &mut ui,
            KeyEvent::new(KeyCode::Enter, Modifiers::NONE),
            &CancellationToken::new(),
            Some(&steering),
            None,
            AgentLimits {
                image_input: true,
                ..AgentLimits::default()
            },
        );
        assert_eq!(ui.draft, "describe this");
        assert!(steering.take_uncommitted().is_empty());
        assert!(ui.pending.is_empty());
        finish_pending_clipboard_paste(&mut ui).await;
        assert_eq!(ui.draft, "describe this image");
        assert!(ui.status.is_empty());
    }

    #[test]
    fn skill_command_during_a_turn_is_expanded_before_steering() {
        let root = std::env::temp_dir().join(format!(
            "ion-terminal-resource-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let skill = root.join(".agents/skills/ion-terminal-audit-test");
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(&skill).unwrap();
        fs::write(
            skill.join("SKILL.md"),
            "---\nname: ion-terminal-audit-test\ndescription: Audit a change.\n---\nAUDIT_MARKER\n",
        )
        .unwrap();
        let resources = Resources::load(&root, &root.join("config")).unwrap();
        let mut ui = Frontend::default();
        ui.images
            .push(ion_ai::normalize_rgba(1, 1, vec![255, 0, 0, 255]).unwrap());
        ui.insert("/skill:ion-terminal-audit-test src/lib.rs");
        let steering = SteeringInbox::new(
            AgentLimits {
                image_input: true,
                ..AgentLimits::default()
            },
            ui.input_budget.clone(),
        );
        busy_key(
            &mut ui,
            KeyEvent::new(KeyCode::Enter, Modifiers::NONE),
            &CancellationToken::new(),
            Some(&steering),
            Some(&resources),
            AgentLimits {
                image_input: true,
                ..AgentLimits::default()
            },
        );
        assert!(ui.draft.is_empty());
        let queued = steering.take_uncommitted();
        assert_eq!(queued.len(), 1);
        let Content::Text(prompt) = &queued[0].message().content[0] else {
            panic!("expected text steering");
        };
        assert!(prompt.contains("AUDIT_MARKER"));
        assert!(prompt.contains("User request: src/lib.rs"));
        assert!(matches!(queued[0].message().content[1], Content::Image(_)));
        assert!(ui.images.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn queue_rejection_preserves_editor_and_restored_steering_keeps_its_budget() {
        let limits = AgentLimits {
            image_input: true,
            ..AgentLimits::default()
        };
        let message = Message::user_input("first".into(), []);
        let budget = InputBudget::new(serde_json::to_vec(&(&message, &())).unwrap().len());
        let mut ui = Frontend {
            input_budget: budget.clone(),
            ..Frontend::default()
        };
        ui.queue_follow_up("first".into(), limits, None);
        let image = ion_ai::normalize_rgba(1, 1, vec![255, 0, 0, 255]).unwrap();
        ui.images.push(image.clone());
        ui.insert("  second\n");
        busy_key(
            &mut ui,
            KeyEvent::new(KeyCode::Enter, Modifiers::ALT),
            &CancellationToken::new(),
            None,
            None,
            limits,
        );
        assert_eq!(ui.draft, "  second\n");
        assert_eq!(ui.images.len(), 1);
        assert_eq!(ui.images[0].content, image.content);
        assert_eq!(ui.pending.len(), 1);
        assert!(ui.status.contains("not queued"));
        let steering = SteeringInbox::new(limits, budget);
        assert!(matches!(
            steering.push("x".into()),
            Err(ion_core::CodingAgentError::InputQueueFull { .. })
        ));
        ui.dequeue();
        assert_eq!(ui.draft, "  second\n\n\nfirst");
        steering.push("x".into()).unwrap();
        let accepted = steering.take_uncommitted().pop().unwrap();
        ui.pending.push_back(TurnInput::from_accepted(accepted));
        assert!(matches!(
            steering.push("x".into()),
            Err(ion_core::CodingAgentError::InputQueueFull { .. })
        ));
        ui.dequeue();
        assert_eq!(ui.draft, "  second\n\n\nfirst\n\nx");
        assert_eq!(ui.images.len(), 1);
        steering.push("x".into()).unwrap();
    }

    #[test]
    fn recovered_steering_preserves_prompt_and_each_image_note() {
        let mut first = ion_ai::normalize_rgba(1, 1, vec![255, 0, 0, 255]).unwrap();
        first.note = Some("First image coordinates".into());
        let mut second = ion_ai::normalize_rgba(1, 1, vec![0, 255, 0, 255]).unwrap();
        second.note = Some("Second image coordinates".into());
        let message = Message::user_input("  inspect both\n".into(), [first, second]);
        let accepted = InputBudget::default()
            .admit(
                message.clone(),
                &(),
                AgentLimits {
                    image_input: true,
                    ..AgentLimits::default()
                },
            )
            .unwrap();
        let recovered = TurnInput::from_accepted(accepted);
        assert_eq!(
            Message::user_input(recovered.prompt, recovered.images),
            message
        );
        let mut ui = Frontend::default();
        ui.restore_input(message.clone()).unwrap();
        assert_eq!(Message::user_input(ui.draft, ui.images), message);
    }

    #[test]
    fn fork_restore_refuses_lossy_shapes_without_discarding_editor() {
        let mut multipart = Message::user_input("first".into(), []);
        multipart.content.push(Content::Text("second".into()));
        for input in [
            multipart,
            Message::user_input("x".repeat(MAX_DRAFT + 1), []),
        ] {
            let mut ui = Frontend {
                draft: "keep unsent".into(),
                cursor: 4,
                images: vec![ion_ai::normalize_rgba(1, 1, vec![0, 255, 0, 255]).unwrap()],
                ..Frontend::default()
            };
            assert!(ui.restore_input(input).is_err());
            assert_eq!(ui.draft, "keep unsent");
            assert_eq!(ui.cursor, 4);
            assert_eq!(ui.images.len(), 1);
        }
    }

    #[test]
    fn command_completion_edits_without_dispatch_or_argument_loss() {
        let mut ui = Frontend {
            history_published_items: 7,
            ..Frontend::default()
        };
        for ch in "/hel".chars() {
            assert!(matches!(
                ui.key(KeyEvent::new(KeyCode::Char(ch), Modifiers::NONE), None),
                Action::None
            ));
        }
        assert!(ui.completion.active());
        assert!(matches!(
            ui.key(KeyEvent::new(KeyCode::Tab, Modifiers::NONE), None),
            Action::None
        ));
        assert_eq!(ui.draft, "/help ");
        assert!(ui.pending.is_empty());
        assert_eq!(ui.history_published_items, 7);
        assert!(
            matches!(ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), None), Action::Command(command) if command == "/help")
        );

        ui.insert("  /help focus 🦀");
        ui.cursor = "  /hel".len();
        ui.key(KeyEvent::new(KeyCode::Tab, Modifiers::NONE), None);
        assert_eq!(ui.draft, "  /help focus 🦀");
        assert_eq!(ui.cursor, "  /help ".len());
        ui.draft.clear();
        ui.cursor = 0;
        ui.insert("/re");
        // Pasted ambiguous names open discovery before accepting a proposal.
        ui.key(KeyEvent::new(KeyCode::Tab, Modifiers::NONE), None);
        assert_eq!(ui.draft, "/re");
        assert!(ui.completion.active());
        ui.key(KeyEvent::new(KeyCode::Esc, Modifiers::NONE), None);
        assert!(!ui.completion.active());
        assert_eq!(ui.draft, "/re");
        ui.draft.clear();
        ui.cursor = 0;
        ui.insert("literal");
        ui.key(KeyEvent::new(KeyCode::Tab, Modifiers::NONE), None);
        assert_eq!(ui.draft, "literal\t");
        ui.draft.clear();
        ui.cursor = 0;
        ui.insert("/fork");
        ui.key(KeyEvent::new(KeyCode::Tab, Modifiers::NONE), None);
        assert!(ui.completion.active());
        ui.completion.rows(80, 5);
        ui.key(KeyEvent::new(KeyCode::Down, Modifiers::NONE), None);
        ui.completion.rows(80, 5);
        assert!(matches!(
            ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), None),
            Action::None
        ));
        assert_eq!(ui.draft, "/fork-after ");
        ui.draft.clear();
        ui.cursor = 0;
        ui.insert("/re");
        ui.completion.refresh(&ui.draft, ui.cursor, None, false);
        assert!(ui.completion.rows(80, 1).is_empty());
        // A menu that cannot fit is not an invisible keyboard target.
        assert!(
            matches!(ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), None), Action::Command(command) if command == "/re")
        );
    }

    #[test]
    fn resource_discovery_and_busy_completion_preserve_admission_authority() {
        let root = std::env::temp_dir().join(format!("ion-menu-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(root.join(".ion/prompts")).unwrap();
        fs::create_dir(root.join(".git")).unwrap();
        fs::create_dir_all(root.join(".agents/skills/zzmenu-audit")).unwrap();
        fs::write(root.join(".agents/skills/zzmenu-audit/SKILL.md"), "---\nname: zzmenu-audit\ndescription: Explicit audit\ndisable-model-invocation: true\n---\nAudit instructions\n").unwrap();
        let template = root.join(".ion/prompts/zzmenu-check.md");
        fs::write(&template, "ESC\x1b[31m 🦀\nFirst $1\n").unwrap();
        fs::write(
            root.join(".ion/prompts/new.md"),
            "Must not shadow terminal control\n",
        )
        .unwrap();
        let resources = Resources::load(&root, &root.join("config")).unwrap();
        let stop = CancellationToken::new();
        let mut ui = Frontend {
            history_published_items: 7,
            ..Frontend::default()
        };
        let steering = SteeringInbox::new(AgentLimits::default(), ui.input_budget.clone());
        ui.insert("/zzmenu-ch");
        ui.completion
            .refresh(&ui.draft, ui.cursor, Some(&resources), false);
        for width in [1, 8, 24, 80] {
            let rows = ui.completion.rows(width, 5);
            assert!(rows.len() <= 5);
            assert!(
                rows.iter()
                    .all(|row| !row.contains('\x1b') && row.width() <= width)
            );
        }
        busy_key(
            &mut ui,
            KeyEvent::new(KeyCode::Tab, Modifiers::NONE),
            &stop,
            Some(&steering),
            Some(&resources),
            AgentLimits::default(),
        );
        assert_eq!(ui.draft, "/zzmenu-check ");
        assert!(steering.take_uncommitted().is_empty());
        ui.insert("concurrency");
        busy_key(
            &mut ui,
            KeyEvent::new(KeyCode::Enter, Modifiers::NONE),
            &stop,
            Some(&steering),
            Some(&resources),
            AgentLimits::default(),
        );
        fs::write(&template, "Changed $1\n").unwrap();
        let reloaded = Resources::load(&root, &root.join("config")).unwrap();
        let queued = steering.take_uncommitted();
        assert_eq!(queued.len(), 1);
        assert!(
            matches!(&queued[0].message().content[0], Content::Text(text) if text.contains("First concurrency") && !text.contains("Changed"))
        );
        drop(queued);
        assert_eq!(ui.history_published_items, 7);
        assert!(!stop.is_cancelled());
        ui.insert("/zzmenu-au");
        ui.key(
            KeyEvent::new(KeyCode::Tab, Modifiers::NONE),
            Some(&reloaded),
        );
        assert_eq!(ui.draft, "/skill:zzmenu-audit ");
        ui.draft.clear();
        ui.cursor = 0;
        ui.insert("/ne");
        ui.completion
            .refresh(&ui.draft, ui.cursor, Some(&reloaded), false);
        assert!(
            ui.completion
                .rows(100, 5)
                .iter()
                .all(|row| !row.contains("[prompt]"))
        );
        ui.draft.clear();
        ui.cursor = 0;
        ui.insert("/new");
        busy_key(
            &mut ui,
            KeyEvent::new(KeyCode::Enter, Modifiers::NONE),
            &stop,
            Some(&steering),
            Some(&reloaded),
            AgentLimits::default(),
        );
        assert_eq!(ui.draft, "/new");
        assert!(steering.take_uncommitted().is_empty());
        busy_key(
            &mut ui,
            KeyEvent::new(KeyCode::Enter, Modifiers::ALT),
            &stop,
            Some(&steering),
            Some(&reloaded),
            AgentLimits::default(),
        );
        assert_eq!(ui.draft, "/new");
        assert!(ui.pending.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prompt_history_restores_unsent_draft_and_moves_between_lines() {
        let mut ui = Frontend {
            prompt_history: vec!["first".into(), "second\nline".into()],
            ..Frontend::default()
        };
        ui.insert("unsent");
        ui.key(KeyEvent::new(KeyCode::Up, Modifiers::NONE), None);
        assert_eq!(ui.draft, "second\nline");
        ui.key(KeyEvent::new(KeyCode::Up, Modifiers::NONE), None);
        assert_eq!(ui.cursor, "line".len());
        ui.key(KeyEvent::new(KeyCode::Up, Modifiers::NONE), None);
        assert_eq!(ui.draft, "first");
        ui.key(KeyEvent::new(KeyCode::Down, Modifiers::NONE), None);
        assert_eq!(ui.draft, "second\nline");
        ui.key(KeyEvent::new(KeyCode::Down, Modifiers::NONE), None);
        assert_eq!(ui.draft, "unsent");
    }

    #[test]
    fn file_picker_inserts_a_selected_project_path() {
        let root = std::env::temp_dir().join(format!("ion-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(root.join("src/skip.rs"), "ignored").unwrap();
        std::fs::write(root.join(".gitignore"), "src/skip.rs\n").unwrap();
        let mut ui = Frontend {
            cwd: root.clone(),
            ..Frontend::default()
        };
        assert!(scan_files(&root).contains(&"src/main.rs".into()));
        assert!(!scan_files(&root).contains(&"src/skip.rs".into()));
        ui.insert("Read ");
        ui.key(KeyEvent::new(KeyCode::Char('@'), Modifiers::NONE), None);
        ui.key(KeyEvent::new(KeyCode::Char('m'), Modifiers::NONE), None);
        let chosen = ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), None);
        let Action::Pick(PickerValue::File { path, start, end }) = chosen else {
            panic!("file picker did not choose a path")
        };
        ui.insert_file(path, start, end);
        assert_eq!(ui.draft, "Read @src/main.rs");
        std::fs::remove_dir_all(root).unwrap();
    }
}
