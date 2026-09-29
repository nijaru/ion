//! Terminal view over the same coding loop used by headless and library hosts.
use std::{
    collections::{HashSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, ensure};
use ignore::WalkBuilder;
use ion_ai::{Content, Message, ModelRef, Role};
use ion_core::{
    CodingAgent, CodingAgentEvent, CodingSession, CodingToolHost, ForkPoint, SessionEntry,
    SessionView, SteeringInbox, TurnEndReason,
};
use ion_host::image_input::LoadedImage;
use ion_host::{CredentialStatus, CredentialStore, Host, Resources, Selection, SessionCatalog};
use ion_terminal::{
    InputEvent, InputStream, KeyCode, KeyEvent, Modifiers, MouseKind, Screen, TerminalSession,
    install_panic_hook,
};
use ratatui::text::Line;
use tokio::time::{Duration, interval};
use tokio_util::sync::CancellationToken;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const MAX_DRAFT: usize = 64 * 1024;
const MAX_PREVIEW: usize = 64 * 1024;
const MAX_ROWS: usize = 4096;

#[derive(Default)]
struct Frontend {
    draft: String,
    images: Vec<LoadedImage>,
    cursor: usize,
    history: Vec<Message>,
    scroll: usize,
    status: String,
    notices: Vec<String>,
    picker: Option<Picker>,
    session_label: String,
    cwd_label: String,
    context_label: String,
    context_window_tokens: Option<u32>,
    pending: VecDeque<PendingInput>,
    prompt_history: Vec<String>,
    history_cursor: Option<usize>,
    saved_draft: String,
    tool_view: Option<ToolView>,
    clipboard_job: Option<tokio::task::JoinHandle<Result<PreparedPaste>>>,
    cwd: PathBuf,
}

struct ToolView {
    label: String,
    output: String,
    scroll: usize,
}

struct PendingInput {
    prompt: String,
    images: Vec<LoadedImage>,
}

impl PendingInput {
    fn from_message(input: Message) -> Self {
        let mut prompt = String::new();
        let mut images = Vec::new();
        for part in input.content {
            match part {
                Content::Text(text) => {
                    if !prompt.is_empty() {
                        prompt.push('\n');
                    }
                    prompt.push_str(&text);
                }
                Content::Image(content) => images.push(LoadedImage {
                    content,
                    note: None,
                }),
                Content::ToolCall(_) | Content::ToolResult(_) => {}
            }
        }
        Self { prompt, images }
    }
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

struct ChatRuntime {
    session: Arc<CodingSession>,
    agent: Arc<CodingAgent>,
    selected: Selection,
    instructions: String,
    resources: Resources,
    sessions: SessionCatalog,
    host: Arc<Host>,
    external_tools: Option<Arc<dyn CodingToolHost>>,
}

impl ChatRuntime {
    fn switch_session(&mut self, path: PathBuf) -> Result<()> {
        if fs::canonicalize(self.session.path())? == fs::canonicalize(&path)? {
            return Ok(());
        }
        let session = Arc::new(CodingSession::open(&path)?);
        anyhow::ensure!(
            session.cwd() == self.session.cwd(),
            "session belongs to another directory"
        );
        let selected = self.host.models().choose(
            None,
            None,
            session.view()?.last_model,
            self.host.credentials(),
        )?;
        selected.require_access(self.host.credentials())?;
        let agent = self.host.agent_with_optional_tools(
            &session,
            &selected,
            self.external_tools.clone(),
        )?;
        let instructions = self.resources.instructions().to_owned();
        self.session = session;
        self.selected = selected;
        self.agent = agent;
        self.instructions = instructions;
        Ok(())
    }

    fn new_session(&mut self) -> Result<()> {
        let selected = self
            .host
            .models()
            .choose(None, None, None, self.host.credentials())?;
        selected.require_access(self.host.credentials())?;
        let instructions = self.resources.instructions().to_owned();
        let path = self.sessions.new_path()?;
        let session = Arc::new(CodingSession::create(&path, self.session.cwd())?);
        let agent = self.host.agent_with_optional_tools(
            &session,
            &selected,
            self.external_tools.clone(),
        )?;
        session.select_model(selected.identity())?;
        self.session = session;
        self.selected = selected;
        self.agent = agent;
        self.instructions = instructions;
        Ok(())
    }

    fn clone_session(&mut self) -> Result<String> {
        let path = self.sessions.new_path()?;
        let session = Arc::new(self.session.clone_to(&path)?);
        let id = path
            .file_stem()
            .context("cloned session has no ID")?
            .to_string_lossy()
            .into_owned();
        self.session = session;
        Ok(id)
    }

    fn fork_session(&mut self, point: ForkPoint) -> Result<String> {
        let turn = match point {
            ForkPoint::BeforeTurn(turn) | ForkPoint::AfterTurn(turn) => turn,
        };
        let selected_model = self
            .session
            .view()?
            .turns()
            .into_iter()
            .find(|item| item.turn == turn)
            .context("selected Turn does not exist")?
            .model;
        let selected = self.host.models().resolve_identity(&selected_model)?;
        selected.require_access(self.host.credentials())?;
        let path = self.sessions.new_path()?;
        let session = Arc::new(self.session.fork_to(&path, point)?);
        let agent = self.host.agent_with_optional_tools(
            &session,
            &selected,
            self.external_tools.clone(),
        )?;
        let id = path
            .file_stem()
            .context("forked session has no ID")?
            .to_string_lossy()
            .into_owned();
        self.session = session;
        self.selected = selected;
        self.agent = agent;
        Ok(id)
    }

    fn select_model(&mut self, model: ModelRef) -> Result<()> {
        let selected = self.host.models().resolve_identity(&model)?;
        selected.require_access(self.host.credentials())?;
        let agent = self.host.agent_with_optional_tools(
            &self.session,
            &selected,
            self.external_tools.clone(),
        )?;
        self.session.select_model(model)?;
        self.selected = selected;
        self.agent = agent;
        Ok(())
    }

    fn reload_resources(&mut self) -> Result<()> {
        self.resources = self.host.resources(self.session.cwd())?;
        self.instructions = self.resources.instructions().to_owned();
        Ok(())
    }
}

#[derive(Default)]
struct Progress {
    text: String,
    events: Vec<String>,
}

impl Progress {
    fn observe(&mut self, event: CodingAgentEvent) {
        match event {
            CodingAgentEvent::TurnAccepted { .. } => {}
            CodingAgentEvent::TextDelta(text) => {
                self.text.push_str(&text);
                if self.text.len() > MAX_PREVIEW {
                    let mut start = self.text.len() - MAX_PREVIEW;
                    while !self.text.is_char_boundary(start) {
                        start += 1;
                    }
                    self.text.drain(..start);
                }
            }
            CodingAgentEvent::ProviderRetry {
                attempt,
                max_retries,
                delay_ms,
            } => self.events.push(format!(
                "Provider retry {attempt}/{max_retries} in {delay_ms}ms"
            )),
            CodingAgentEvent::ToolStarted {
                name, arguments, ..
            } => self
                .events
                .push(format!("→ {name} {}", brief(&arguments.to_string(), 2048))),
            CodingAgentEvent::ToolFinished { name, output, .. } => self.events.push(format!(
                "← {name} {}: {}{}",
                if output.is_error { "error" } else { "done" },
                brief(&output.value.to_string(), 2048),
                image_markers(&output.images)
            )),
            CodingAgentEvent::ToolRejected { name, output, .. } => self.events.push(format!(
                "↛ {name} skipped: {}",
                brief(&output.value.to_string(), 2048)
            )),
            CodingAgentEvent::InterruptedCalls(n) => self.events.push(format!(
                "{n} previous tool call(s) had unknown effects; inspect before retrying"
            )),
            CodingAgentEvent::ContextCompacted { through_entry } => {
                self.text.clear();
                self.events
                    .push(format!("Context summarized through entry {through_entry}"));
            }
            CodingAgentEvent::ResponseRestarted => {
                self.text.clear();
                self.events
                    .push("Incomplete response discarded; retrying".into());
            }
            CodingAgentEvent::Final(_) => {}
        }
        if self.events.len() > 16 {
            self.events.drain(..self.events.len() - 16);
        }
    }
}

pub struct ChatInit {
    pub session: Arc<CodingSession>,
    pub agent: Arc<CodingAgent>,
    pub selected: Selection,
    pub resources: Resources,
    pub images: Vec<LoadedImage>,
    pub sessions: SessionCatalog,
    pub host: Arc<Host>,
    pub external_tools: Option<Arc<dyn CodingToolHost>>,
}

pub async fn chat(init: ChatInit) -> Result<()> {
    let ChatInit {
        session,
        agent,
        selected,
        resources,
        images,
        sessions,
        host,
        external_tools,
    } = init;
    let instructions = resources.instructions().to_owned();
    let mut runtime = ChatRuntime {
        session,
        agent,
        selected,
        instructions,
        resources,
        sessions,
        host,
        external_tools,
    };
    install_panic_hook();
    let mut terminal = TerminalSession::enter().context("interactive chat requires a terminal")?;
    terminal.enter_alt_screen()?;
    let (width, height) = terminal.size()?;
    let mut screen = Screen::new(width, 0, height);
    let mut input = terminal.input()?;
    let mut ui = Frontend {
        status: "Enter to send · Shift-Enter newline · Ctrl-C clear/quit".into(),
        context_window_tokens: runtime.selected.context_window_tokens,
        images,
        ..Frontend::default()
    };
    ui.refresh_session(&runtime.session)?;
    loop {
        ui.context_window_tokens = runtime.selected.context_window_tokens;
        ui.update_context(&runtime.session)?;
        if let Some(PendingInput { prompt, images }) = ui.pending.pop_front() {
            let prompt = match expand_resource_input(&runtime.resources, prompt) {
                Ok(prompt) => prompt,
                Err((original, error)) => {
                    ui.images.splice(0..0, images);
                    ui.draft = if ui.draft.is_empty() {
                        original
                    } else {
                        format!("{original}\n\n{}", ui.draft)
                    };
                    ui.cursor = ui.draft.len();
                    ui.status = format!("{error:#}");
                    continue;
                }
            };
            ui.status = "Working · Enter steers · Alt-Enter queues · Ctrl-C cancels".into();
            ui.scroll = 0;
            run_turn(
                &mut terminal,
                &mut screen,
                &mut input,
                &mut ui,
                &runtime.session,
                &runtime.agent,
                &runtime.selected,
                &runtime.instructions,
                &runtime.resources,
                prompt,
                images,
            )
            .await?;
            continue;
        }
        draw(
            &mut terminal,
            &mut screen,
            &ui,
            None,
            &runtime.selected.identity(),
            false,
        )?;
        let Some(event) = input.next().await else {
            break;
        };
        match event? {
            InputEvent::Key(key) => match ui.key(key) {
                Action::None => {}
                Action::Quit => break,
                Action::Submit(prompt) => {
                    ui.status = "Working · Enter steers · Alt-Enter queues · Ctrl-C cancels".into();
                    ui.scroll = 0;
                    let images = std::mem::take(&mut ui.images);
                    run_turn(
                        &mut terminal,
                        &mut screen,
                        &mut input,
                        &mut ui,
                        &runtime.session,
                        &runtime.agent,
                        &runtime.selected,
                        &runtime.instructions,
                        &runtime.resources,
                        prompt,
                        images,
                    )
                    .await?;
                }
                Action::Shell(command, exclude_from_context) => {
                    if let Err(error) = run_user_shell(
                        &mut terminal,
                        &mut screen,
                        &mut input,
                        &mut ui,
                        &runtime.session,
                        &runtime.selected,
                        command.clone(),
                        exclude_from_context,
                    )
                    .await
                    {
                        ui.note(format!("Shell command: {command}"));
                        ui.status = format!(
                            "Shell result uncertain: {error:#}; inspect the working directory before retrying"
                        );
                    }
                }
                Action::Command(command) => {
                    if let Some(provider) = command.strip_prefix("/login ") {
                        match login_in_terminal(
                            &mut terminal,
                            &mut screen,
                            &mut input,
                            runtime.host.credentials(),
                            provider.trim(),
                        )? {
                            Ok(()) => ui.status = "Credential saved".into(),
                            Err(error) => ui.status = format!("{error:#}"),
                        }
                    } else if command == "/compact" {
                        run_compaction(
                            &mut terminal,
                            &mut screen,
                            &mut input,
                            &mut ui,
                            &runtime.session,
                            &runtime.agent,
                            &runtime.selected,
                        )
                        .await?;
                    } else if command == "/copy" {
                        match copy_last_answer(&runtime.session, &mut terminal).await {
                            Ok(crate::clipboard::CopyOutcome::Copied) => {
                                ui.status = "Copied last assistant answer".into()
                            }
                            Ok(crate::clipboard::CopyOutcome::RequestedFromTerminal) => {
                                ui.status = "Sent clipboard request to terminal".into()
                            }
                            Err(error) => ui.status = format!("Copy failed: {error:#}"),
                        }
                    } else if command == "/editor" {
                        match edit_draft_in_terminal(
                            &mut terminal,
                            &mut screen,
                            &mut input,
                            &mut ui,
                        )
                        .await
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
                                ui.status =
                                    "Working · Enter steers · Alt-Enter queues · Ctrl-C cancels"
                                        .into();
                                ui.scroll = 0;
                                let images = std::mem::take(&mut ui.images);
                                run_turn(
                                    &mut terminal,
                                    &mut screen,
                                    &mut input,
                                    &mut ui,
                                    &runtime.session,
                                    &runtime.agent,
                                    &runtime.selected,
                                    &runtime.instructions,
                                    &runtime.resources,
                                    prompt,
                                    images,
                                )
                                .await?;
                            }
                            Ok(None) => {}
                            Err(error) => ui.status = format!("{error:#}"),
                        }
                    }
                }
                Action::Queue(prompt) => ui.pending.push_back(PendingInput {
                    prompt,
                    images: std::mem::take(&mut ui.images),
                }),
                Action::PasteClipboard => {
                    if let Err(error) = paste_clipboard(&mut ui, &runtime.selected).await {
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
                            ui.refresh_session(&runtime.session)?;
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
    terminal.restore()?;
    Ok(())
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
    terminal
        .enter_alt_screen()
        .context("restore chat screen after login")?;
    let (width, height) = terminal.size().context("read terminal size after login")?;
    *screen = Screen::new(width, 0, height);
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

async fn edit_draft_in_terminal(
    terminal: &mut TerminalSession,
    screen: &mut Screen,
    input: &mut InputStream,
    ui: &mut Frontend,
) -> Result<()> {
    input
        .suspend()
        .context("release terminal input for editor")?;
    terminal.suspend().context("suspend terminal for editor")?;
    let edited = crate::external_editor::edit(&ui.draft, MAX_DRAFT).await;
    terminal.resume().context("resume terminal after editor")?;
    terminal
        .enter_alt_screen()
        .context("restore chat screen after editor")?;
    let (width, height) = terminal.size()?;
    *screen = Screen::new(width, 0, height);
    *input = terminal
        .input()
        .context("resume terminal input after editor")?;
    let edited = edited?;
    ui.draft = edited;
    ui.cursor = ui.draft.len();
    Ok(())
}

fn apply_fork(
    runtime: &mut ChatRuntime,
    ui: &mut Frontend,
    point: ForkPoint,
    restore: Option<Message>,
) -> Result<()> {
    let id = runtime.fork_session(point)?;
    ui.refresh_session(&runtime.session)?;
    let mut too_large_to_restore = false;
    if let Some(input) = restore {
        let draft = input
            .content
            .iter()
            .filter_map(|part| match part {
                Content::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        if draft.len() > MAX_DRAFT {
            too_large_to_restore = true;
            ui.draft.clear();
            ui.cursor = 0;
            ui.images.clear();
        } else {
            ui.draft = draft;
            ui.cursor = ui.draft.len();
            ui.images = input
                .content
                .into_iter()
                .filter_map(|part| match part {
                    Content::Image(content) => Some(LoadedImage {
                        content,
                        note: None,
                    }),
                    _ => None,
                })
                .collect();
        }
    }
    ui.status = if too_large_to_restore {
        format!(
            "Forked as {}; selected input exceeds editor limit; inspect source to copy it",
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
    runtime: &mut ChatRuntime,
    ui: &mut Frontend,
    command: &str,
) -> Result<Option<String>> {
    let (name, args) = command.split_once(' ').unwrap_or((command, ""));
    let args = args.trim();
    match name {
        "/help" => ui.note(
            "/new /clone /fork [TURN] /fork-after TURN /resume /session /name NAME /model /compact /tools /tool [N] /image PATH /copy /editor /export PATH /skills /prompts /reload /login PROVIDER /logout PROVIDER /quit\nCtrl-V pastes files, image or text from the host clipboard. !COMMAND runs shell and shares result with model; !!COMMAND keeps it out of model context".into(),
        ),
        "/image" => {
            anyhow::ensure!(!args.is_empty(), "use /image PATH");
            let path = Path::new(args);
            let path = if path.is_absolute() { path.to_owned() } else { runtime.session.cwd().join(path) };
            ui.images.push(ion_host::image_input::load_image(&runtime.selected, &path)?);
            ui.status = format!("{} image(s) attached to the next prompt", ui.images.len());
        }
        "/skills" => ui.note(runtime.resources.skills().map(|skill| format!("{} — {}", skill.name, skill.description)).collect::<Vec<_>>().join("\n")),
        "/prompts" => ui.note(runtime.resources.templates().map(|template| format!("/{} — {}", template.name, template.description)).collect::<Vec<_>>().join("\n")),
        "/reload" => {
            runtime.reload_resources()?;
            ui.note(format!("Reloaded resources ({} diagnostic(s))", runtime.resources.diagnostics().len()));
        }
        "/session" => {
            let view = runtime.session.view()?;
            ui.note(format!(
                "Session {} · {} turn(s) · {}",
                runtime.session.path().display(),
                view.entries
                    .iter()
                    .filter(|entry| matches!(entry, ion_core::SessionEntry::TurnStarted { .. }))
                    .count(),
                view.name.unwrap_or_else(|| "unnamed".into()),
            ));
        }
        "/export" => {
            anyhow::ensure!(!args.is_empty(), "use /export PATH");
            let target = Path::new(args);
            let target = if target.is_absolute() { target.to_owned() } else { runtime.session.cwd().join(target) };
            crate::transcript::save_new(&runtime.session.view()?, &target)?;
            ui.status = format!("Transcript saved to {}", target.display());
        }
        "/new" => {
            runtime.new_session()?;
            ui.refresh_session(&runtime.session)?;
            ui.note("Started a new session".into());
        }
        "/clone" => {
            let id = runtime.clone_session()?;
            ui.refresh_session(&runtime.session)?;
            ui.note(format!(
                "Cloned conversation as {id}; both sessions use the same working directory"
            ));
        }
        "/fork" => {
            let turns = runtime.session.view()?.turns();
            if args.is_empty() {
                let items = turns.into_iter().map(|item| PickerItem {
                    label: format!("Turn {}  {}", item.turn, crate::preview_input(&item.input)),
                    value: PickerValue::ForkBefore { turn: item.turn, input: item.input },
                }).collect();
                ui.picker = Some(Picker { title: "Fork before Turn", query: String::new(), selected: 0, items });
            } else {
                let turn: u64 = args.parse().context("use /fork TURN")?;
                let input = turns.into_iter().find(|item| item.turn == turn).context("selected Turn does not exist")?.input;
                apply_fork(runtime, ui, ForkPoint::BeforeTurn(turn), Some(input))?;
            }
        }
        "/fork-after" => {
            let turn: u64 = args.parse().context("use /fork-after TURN")?;
            apply_fork(runtime, ui, ForkPoint::AfterTurn(turn), None)?;
        }
        "/resume" => {
            if !args.is_empty() {
                runtime.switch_session(runtime.sessions.by_id(args)?)?;
                ui.refresh_session(&runtime.session)?;
            } else {
                let items = runtime
                    .sessions
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
        "/name" => {
            if args.is_empty() {
                ui.note(
                    runtime
                        .session
                        .view()?
                        .name
                        .unwrap_or_else(|| "Session has no name".into()),
                );
            } else {
                runtime.session.set_name(Some(args))?;
                ui.refresh_session(&runtime.session)?;
            }
        }
        "/model" => {
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
                    .host
                    .models()
                    .choices(runtime.host.credentials())?
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
        "/logout" => {
            anyhow::ensure!(!args.is_empty(), "use /logout PROVIDER");
            runtime.host.credentials().remove(args)?;
            ui.status = format!("Removed saved {args} credential");
        }
        "/tools" => ui.list_tools(),
        "/tool" => {
            let number = if args.is_empty() {
                None
            } else {
                Some(args.parse::<usize>().context("use /tool [N]")?)
            };
            ui.open_tool(number);
        }
        _ => {
            if let Some(prompt) = runtime.resources.expand_command(command) {
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
    session: &CodingSession,
    agent: &CodingAgent,
    selected: &Selection,
) -> Result<()> {
    let model = selected.identity();
    ui.status = "Summarizing context · Ctrl-C cancels".into();
    let stop = CancellationToken::new();
    let mut input_ended = false;
    let result = {
        let compact = agent.compact(session, model.clone(), stop.clone(), |_| {});
        tokio::pin!(compact);
        let mut tick = interval(Duration::from_millis(50));
        loop {
            tokio::select! {
                result = &mut compact => break result,
                event = input.next(), if !input_ended => match event {
                    Some(Ok(InputEvent::Key(KeyEvent { code: KeyCode::Char('c'), modifiers }))) if modifiers.contains(Modifiers::CONTROL) => { stop.cancel(); ui.status = "Cancelling…".into(); },
                    Some(Ok(InputEvent::Key(key))) if is_clipboard_shortcut(key) && ui.picker.is_none() && ui.tool_view.is_none() => {
                        start_clipboard_paste(ui, selected);
                    },
                    Some(Ok(InputEvent::Key(key))) => busy_key(ui, key, &stop, None, None),
                    Some(Ok(InputEvent::Paste(text))) => ui.insert(&text),
                    Some(Ok(InputEvent::Resize(size))) => screen.resize(size.columns, size.rows),
                    Some(Ok(InputEvent::Mouse(mouse))) => match mouse.kind() {
                        MouseKind::ScrollUp => ui.scroll = ui.scroll.saturating_add(3),
                        MouseKind::ScrollDown => ui.scroll = ui.scroll.saturating_sub(3),
                        _ => {},
                    },
                    Some(Err(error)) => { stop.cancel(); input_ended = true; ui.status = format!("Input failed: {error}. Cancelling…"); },
                    None => { stop.cancel(); input_ended = true; },
                },
                _ = tick.tick() => { finish_ready_clipboard_paste(ui).await; draw(terminal, screen, ui, None, &model, true)?; },
            }
        }
    };
    finish_pending_clipboard_paste(ui).await;
    if result.is_err() {
        return_pending_to_editor(ui);
    }
    ui.update_context(session)?;
    ui.status = match result {
        Ok(true) => "Context summarized; raw history retained".into(),
        Ok(false) => "No settled history to summarize".into(),
        Err(error) => format!("Compaction ended: {error}"),
    };
    if input_ended {
        return Err(anyhow::anyhow!("terminal input ended during compaction"));
    }
    Ok(())
}

fn expand_resource_input(
    resources: &Resources,
    prompt: String,
) -> std::result::Result<String, (String, anyhow::Error)> {
    match resources.expand_command(&prompt) {
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
) {
    let action = ui.key(key);
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
                ui.pending.push_back(PendingInput {
                    prompt,
                    images: std::mem::take(&mut ui.images),
                });
                ui.status = format!("{} follow-up(s) queued", ui.pending.len());
            }
        }
        Action::Queue(prompt) => {
            ui.pending.push_back(PendingInput {
                prompt,
                images: std::mem::take(&mut ui.images),
            });
            ui.status = format!("{} follow-up(s) queued", ui.pending.len());
        }
        Action::Command(command) => {
            if command == "/copy" || command == "/editor" {
                ui.status = "This action is available after the operation".into();
                return;
            }
            if let (Some(steering), Some(resources)) = (steering, resources)
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
            ui.draft = command;
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
    let mut restored = Vec::new();
    let remaining = ui
        .pending
        .drain(..)
        .map(|pending| {
            restored.extend(pending.images);
            pending.prompt
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
    session: &CodingSession,
    selected: &Selection,
    command: String,
    exclude_from_context: bool,
) -> Result<()> {
    let model = selected.identity();
    let stop = CancellationToken::new();
    let mut tick = interval(Duration::from_millis(50));
    let mut input_ended = false;
    ui.status = "Running shell · Ctrl-C cancels".into();
    let output = {
        let running = session.run_user_shell(&command, stop.clone(), exclude_from_context);
        tokio::pin!(running);
        loop {
            tokio::select! {
                result = &mut running => break result,
                event = input.next(), if !input_ended => match event {
                    Some(Ok(InputEvent::Key(KeyEvent { code: KeyCode::Char('c'), modifiers }))) if modifiers.contains(Modifiers::CONTROL) => {
                        stop.cancel();
                        ui.status = "Cancelling shell…".into();
                    }
                    Some(Ok(InputEvent::Key(key))) if is_clipboard_shortcut(key) && ui.picker.is_none() && ui.tool_view.is_none() => {
                        start_clipboard_paste(ui, selected);
                    },
                    Some(Ok(InputEvent::Key(key))) => busy_key(ui, key, &stop, None, None),
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
                        ui.status = format!("Input failed: {error}. Cancelling shell…");
                    }
                    None => { stop.cancel(); input_ended = true; },
                },
                _ = tick.tick() => { finish_ready_clipboard_paste(ui).await; draw(terminal, screen, ui, None, &model, true)?; },
            }
        }
    };
    finish_pending_clipboard_paste(ui).await;
    let output = output?;
    let view = session.view()?;
    ui.context_label = context_label(&view, ui.context_window_tokens);
    ui.load_history(&view);
    ui.scroll = 0;
    ui.status = if output.is_error {
        "Shell finished with an error"
    } else {
        "Shell finished"
    }
    .into();
    if input_ended {
        ui.status = "Terminal input ended after the shell result was saved".into();
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_turn(
    terminal: &mut TerminalSession,
    screen: &mut Screen,
    input: &mut InputStream,
    ui: &mut Frontend,
    session: &CodingSession,
    agent: &CodingAgent,
    selected: &Selection,
    instructions: &str,
    resources: &Resources,
    prompt: String,
    attached: Vec<LoadedImage>,
) -> Result<()> {
    let model = selected.identity();
    let prior_entry_count = match session.entry_count() {
        Ok(count) => count as usize,
        Err(error) => {
            ui.images.splice(0..0, attached);
            ui.draft = if ui.draft.is_empty() {
                prompt
            } else {
                format!("{prompt}\n\n{}", ui.draft)
            };
            ui.cursor = ui.draft.len();
            return Err(error.into());
        }
    };
    let user_message = Message::user_input(prompt.clone(), attached.iter().cloned());
    let progress = Arc::new(Mutex::new(Progress::default()));
    let observer = progress.clone();
    let stop = CancellationToken::new();
    let steering = SteeringInbox::default();
    let mut input_ended = false;
    let result = {
        let turn = agent.submit_message_with_steering(
            session,
            model.clone(),
            user_message,
            instructions.to_owned(),
            stop.clone(),
            &steering,
            move |event| {
                observer
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .observe(event);
            },
        );
        tokio::pin!(turn);
        let mut tick = interval(Duration::from_millis(50));
        loop {
            tokio::select! {
                result = &mut turn => break result,
                event = input.next(), if !input_ended => match event {
                    Some(Ok(InputEvent::Key(KeyEvent { code: KeyCode::Char('c'), modifiers }))) if modifiers.contains(Modifiers::CONTROL) => { stop.cancel(); ui.status = "Cancelling…".into(); },
                    Some(Ok(InputEvent::Key(key))) if is_clipboard_shortcut(key) && ui.picker.is_none() && ui.tool_view.is_none() => {
                        start_clipboard_paste(ui, selected);
                    },
                    Some(Ok(InputEvent::Key(key))) => busy_key(ui, key, &stop, Some(&steering), Some(resources)),
                    Some(Ok(InputEvent::Paste(text))) => ui.insert(&text),
                    Some(Ok(InputEvent::Resize(size))) => screen.resize(size.columns, size.rows),
                    Some(Ok(InputEvent::Mouse(mouse))) => match mouse.kind() {
                        MouseKind::ScrollUp => ui.scroll = ui.scroll.saturating_add(3),
                        MouseKind::ScrollDown => ui.scroll = ui.scroll.saturating_sub(3),
                        _ => {},
                    },
                    Some(Err(error)) => { stop.cancel(); input_ended = true; ui.status = format!("Input failed: {error}. Cancelling…"); },
                    None => { stop.cancel(); input_ended = true; },
                },
                _ = tick.tick() => {
                    finish_ready_clipboard_paste(ui).await;
                    let preview = progress.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    draw(terminal, screen, ui, Some(&preview), &model, true)?;
                }
            }
        }
    };
    finish_pending_clipboard_paste(ui).await;
    for input in steering.take_uncommitted() {
        ui.pending.push_back(PendingInput::from_message(input));
    }
    if result.is_err() {
        return_pending_to_editor(ui);
    }
    let view = session.view()?;
    if result.is_err()
        && !view.entries[prior_entry_count..]
            .iter()
            .any(|entry| matches!(entry, ion_core::SessionEntry::TurnStarted { .. }))
    {
        ui.images.splice(0..0, attached);
        ui.draft = if ui.draft.is_empty() {
            prompt
        } else {
            format!("{prompt}\n\n{}", ui.draft)
        };
        ui.cursor = ui.draft.len();
    }
    ui.context_label = context_label(&view, ui.context_window_tokens);
    ui.load_history(&view);
    ui.scroll = 0;
    ui.status = match result {
        Ok(_) => "Ready · Enter to send · Ctrl-C to quit".into(),
        Err(error) => format!("Turn ended: {error}"),
    };
    if input_ended {
        return Err(anyhow::anyhow!("terminal input ended during the turn"));
    }
    Ok(())
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
    fn refresh_session(&mut self, session: &CodingSession) -> Result<()> {
        let view = session.view()?;
        self.context_label = context_label(&view, self.context_window_tokens);
        self.load_history(&view);
        self.tool_view = None;
        self.notices.clear();
        self.scroll = 0;
        self.cwd_label = session.cwd().display().to_string();
        self.cwd = session.cwd().to_path_buf();
        let id = session.path().file_stem().map_or_else(
            || "session".into(),
            |stem| stem.to_string_lossy().into_owned(),
        );
        self.session_label = format!(
            "{}{}",
            &id[..id.len().min(8)],
            view.name
                .map_or_else(String::new, |name| format!(" {name}"))
        );
        if view.unfinished_turn.is_some() {
            self.status = "Previous turn interrupted; tool effects may be unknown".into();
        }
        Ok(())
    }

    fn load_history(&mut self, view: &SessionView) {
        self.history = view.display_messages();
        self.prompt_history = view
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
        self.history_cursor = None;
        self.saved_draft.clear();
    }

    fn update_context(&mut self, session: &CodingSession) -> Result<()> {
        self.context_label = context_label(&session.view()?, self.context_window_tokens);
        Ok(())
    }

    fn note(&mut self, message: String) {
        self.notices.push(message);
        if self.notices.len() > 16 {
            self.notices.remove(0);
        }
        self.scroll = 0;
    }

    fn key(&mut self, key: KeyEvent) -> Action {
        if self.tool_view.is_some() {
            return self.tool_view_key(key);
        }
        if self.picker.is_some() {
            return self.picker_key(key);
        }
        match key {
            KeyEvent {
                code: KeyCode::Char('c'),
                modifiers,
            } if modifiers.contains(Modifiers::CONTROL) => {
                if self.draft.is_empty() {
                    Action::Quit
                } else {
                    self.draft.clear();
                    self.cursor = 0;
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
                self.open_tool(None);
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
                let prompt = self.draft.trim().to_owned();
                self.draft.clear();
                self.cursor = 0;
                if prompt.is_empty() {
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
                let prompt = self.draft.trim().to_owned();
                self.draft.clear();
                self.cursor = 0;
                if prompt.is_empty() && self.images.is_empty() {
                    Action::None
                } else if prompt == "/exit" || prompt == "/quit" {
                    Action::Quit
                } else if prompt.starts_with('/') {
                    Action::Command(prompt)
                } else if let Some(command) = prompt.strip_prefix("!!") {
                    if command.trim().is_empty() {
                        self.draft = prompt;
                        self.cursor = self.draft.len();
                        self.status = "Type a shell command after !!".into();
                        Action::None
                    } else {
                        Action::Shell(command.trim().to_owned(), true)
                    }
                } else if let Some(command) = prompt.strip_prefix('!') {
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
                self.history_cursor = None;
                let start = previous_grapheme(&self.draft, self.cursor);
                self.draft.replace_range(start..self.cursor, "");
                self.cursor = start;
                Action::None
            }
            KeyEvent {
                code: KeyCode::Delete,
                ..
            } => {
                self.history_cursor = None;
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
        }
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

    fn tool_view_key(&mut self, key: KeyEvent) -> Action {
        let view = self.tool_view.as_mut().expect("tool output view is active");
        match key {
            KeyEvent {
                code: KeyCode::Esc, ..
            }
            | KeyEvent {
                code: KeyCode::Char('o'),
                modifiers: Modifiers::CONTROL,
            } => {
                self.tool_view = None;
                self.status = "Tool output closed".into();
            }
            KeyEvent {
                code: KeyCode::Up, ..
            } => view.scroll = view.scroll.saturating_add(1),
            KeyEvent {
                code: KeyCode::Down,
                ..
            } => view.scroll = view.scroll.saturating_sub(1),
            KeyEvent {
                code: KeyCode::PageUp,
                ..
            } => view.scroll = view.scroll.saturating_add(12),
            KeyEvent {
                code: KeyCode::PageDown,
                ..
            } => view.scroll = view.scroll.saturating_sub(12),
            _ => {}
        }
        Action::None
    }

    fn list_tools(&mut self) {
        let names = self
            .history
            .iter()
            .flat_map(|message| message.content.iter())
            .filter_map(|content| match content {
                Content::ToolResult(result) => Some(result.name.as_str()),
                _ => None,
            })
            .enumerate()
            .map(|(index, name)| format!("{}:{name}", index + 1))
            .collect::<Vec<_>>();
        self.note(if names.is_empty() {
            "No tool results in this session".into()
        } else {
            format!("Tool results: {}", names.join(" · "))
        });
    }

    fn open_tool(&mut self, number: Option<usize>) {
        let results = self
            .history
            .iter()
            .flat_map(|message| message.content.iter())
            .filter_map(|content| match content {
                Content::ToolResult(result) => Some(result),
                _ => None,
            })
            .collect::<Vec<_>>();
        let index = number.unwrap_or(results.len());
        if index == 0 || index > results.len() {
            self.status = "Tool result not found; use /tools to list results".into();
            return;
        }
        let result = results[index - 1];
        self.tool_view = Some(ToolView {
            label: format!("Tool {index}: {} · Esc or Ctrl-O closes", result.name),
            output: format!(
                "{}{}",
                serde_json::to_string_pretty(&result.result)
                    .unwrap_or_else(|_| result.result.to_string()),
                image_markers(&result.images)
            ),
            scroll: 0,
        });
        self.status = format!("Viewing tool result {index}");
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
        self.history_cursor = None;
    }
    fn insert(&mut self, text: &str) {
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
        self.history_cursor = None;
        self.draft.insert_str(self.cursor, &clean);
        self.cursor += clean.len();
    }

    fn history_previous(&mut self) {
        if self.prompt_history.is_empty() {
            return;
        }
        let index = match self.history_cursor {
            Some(0) => 0,
            Some(index) => index - 1,
            None => {
                self.saved_draft = self.draft.clone();
                self.prompt_history.len() - 1
            }
        };
        self.history_cursor = Some(index);
        self.draft = self.prompt_history[index].clone();
        self.cursor = self.draft.len();
    }

    fn history_next(&mut self) {
        let Some(index) = self.history_cursor else {
            return;
        };
        if index + 1 < self.prompt_history.len() {
            self.history_cursor = Some(index + 1);
            self.draft = self.prompt_history[index + 1].clone();
        } else {
            self.history_cursor = None;
            self.draft = std::mem::take(&mut self.saved_draft);
        }
        self.cursor = self.draft.len();
    }

    fn dequeue(&mut self) {
        if let Some(PendingInput { prompt, images }) = self.pending.pop_back() {
            self.images.extend(images);
            if self.draft.is_empty() {
                self.draft = prompt;
            } else {
                self.draft = format!("{}\n\n{prompt}", self.draft);
            }
            self.cursor = self.draft.len();
            self.history_cursor = None;
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
    ui: &Frontend,
    progress: Option<&Progress>,
    model: &ModelRef,
    _busy: bool,
) -> Result<()> {
    let (width, height) = terminal.size()?;
    screen.resize(width, height);
    let width = width.max(1) as usize;
    let height = height.max(1) as usize;
    let (draft, cursor) = ui.picker.as_ref().map_or((&ui.draft, ui.cursor), |picker| {
        (&picker.query, picker.query.len())
    });
    let composer = wrap_input(draft, cursor, width);
    let chrome_height = if height >= 5 {
        3
    } else if height >= 3 {
        2
    } else {
        0
    };
    let composer_height = composer.lines.len().min(4).min(height - chrome_height);
    let composer_start = composer
        .cursor_row
        .saturating_sub(composer_height - 1)
        .min(composer.lines.len().saturating_sub(composer_height));
    let history_height = height - composer_height - chrome_height;
    let mut history = if let Some(view) = &ui.tool_view {
        let mut rows = vec![view.label.clone()];
        push_wrapped(&mut rows, &view.output, width);
        rows
    } else if let Some(picker) = &ui.picker {
        let matching = picker.matches();
        let mut rows = vec![format!("{} · {} match(es)", picker.title, matching.len())];
        let visible = history_height.saturating_sub(1);
        let start = picker.selected.saturating_sub(visible.saturating_sub(1));
        for (index, item) in matching.iter().enumerate().skip(start).take(visible) {
            let label = &picker.items[*item].label;
            rows.push(format!(
                "{} {}",
                if index == picker.selected { '›' } else { ' ' },
                brief(label, width.saturating_sub(2))
            ));
        }
        rows
    } else {
        let mut rows = history_rows(&ui.history, width);
        for notice in &ui.notices {
            push_wrapped(&mut rows, notice, width);
        }
        if let Some(progress) = progress {
            if !progress.text.is_empty() {
                push_wrapped(&mut rows, &format!("ion> {}", progress.text), width);
            }
            for event in &progress.events {
                push_wrapped(&mut rows, event, width);
            }
        }
        rows
    };
    if ui.tool_view.is_none() && history.len() > MAX_ROWS {
        history.drain(..history.len() - MAX_ROWS);
    }
    let scroll = ui.tool_view.as_ref().map_or(ui.scroll, |view| view.scroll);
    let end = history.len().saturating_sub(if ui.picker.is_some() {
        0
    } else {
        scroll.min(history.len())
    });
    let start = end.saturating_sub(history_height);
    let mut rows = vec![Line::raw(""); height];
    let padding = history_height.saturating_sub(end - start);
    for (i, row) in history[start..end].iter().enumerate() {
        rows[padding + i] = Line::raw(row.clone());
    }
    if chrome_height > 0 {
        rows[history_height] = Line::raw("─".repeat(width));
        if chrome_height == 3 {
            let attachment_label = if ui.images.is_empty() {
                String::new()
            } else {
                format!(" · {} image(s) attached", ui.images.len())
            };
            rows[history_height + 1] =
                Line::raw(brief(&format!("{}{}", ui.status, attachment_label), width));
            rows[history_height + 2] = Line::raw(brief(
                &format!(
                    "{} · {} · {}/{} · {}",
                    ui.cwd_label, ui.session_label, model.provider, model.model, ui.context_label
                ),
                width,
            ));
        } else {
            rows[history_height + 1] = Line::raw(brief(
                &format!("{} / {} · {}", model.provider, model.model, ui.status),
                width,
            ));
        }
    }
    let composer_row = height - composer_height;
    for i in 0..composer_height {
        rows[composer_row + i] = Line::raw(composer.lines[composer_start + i].clone());
    }
    let row = composer_row + composer.cursor_row.saturating_sub(composer_start);
    let cursor = (ui.tool_view.is_none() && row < height)
        .then_some((row, composer.cursor_col.min(width.saturating_sub(1)) as u16));
    screen.draw_fullscreen(terminal.output(), &rows, cursor)?;
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

fn history_rows(messages: &[Message], width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    for message in messages {
        match message.role {
            Role::User => {
                for item in &message.content {
                    match item {
                        Content::Text(text) => push_wrapped(
                            &mut rows,
                            &format!("you> {}", brief(text, MAX_PREVIEW)),
                            width,
                        ),
                        Content::Image(image) => push_wrapped(
                            &mut rows,
                            &format!("you> [image: {}]", image.mime_type().as_str()),
                            width,
                        ),
                        Content::ToolCall(_) | Content::ToolResult(_) => {}
                    }
                }
            }
            Role::Assistant => {
                for item in &message.content {
                    match item {
                        Content::Text(text) => push_wrapped(
                            &mut rows,
                            &format!("ion> {}", brief(text, MAX_PREVIEW)),
                            width,
                        ),
                        Content::ToolCall(call) => push_wrapped(
                            &mut rows,
                            &format!(
                                "→ {} {}",
                                call.name,
                                brief(&call.arguments.to_string(), 2048)
                            ),
                            width,
                        ),
                        Content::Image(_) => {}
                        Content::ToolResult(_) => {}
                    }
                }
            }
            Role::Tool => {
                for item in &message.content {
                    if let Content::ToolResult(result) = item {
                        push_wrapped(
                            &mut rows,
                            &format!(
                                "← {} {}{}",
                                result.name,
                                brief(&result.result.to_string(), 2048),
                                image_markers(&result.images)
                            ),
                            width,
                        );
                    }
                }
            }
        }
    }
    rows
}

fn push_wrapped(rows: &mut Vec<String>, text: &str, width: usize) {
    let width = width.max(1);
    let mut line = String::new();
    let mut col = 0;
    for grapheme in text.graphemes(true) {
        if grapheme == "\n" || grapheme == "\r" {
            rows.push(std::mem::take(&mut line));
            col = 0;
            continue;
        }
        let display = if grapheme == "\t" {
            "    "
        } else if grapheme.chars().any(char::is_control) {
            "�"
        } else {
            grapheme
        };
        let size = UnicodeWidthStr::width(display).max(1);
        if col + size > width && col > 0 {
            rows.push(std::mem::take(&mut line));
            col = 0;
        }
        line.push_str(display);
        col += size;
    }
    rows.push(line);
}
fn image_markers(images: &[ion_ai::ImageContent]) -> String {
    images
        .iter()
        .map(|image| format!("\n[image: {}]", image.mime_type().as_str()))
        .collect()
}

fn brief(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.into();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
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
    fn copy_uses_the_last_completed_answer() {
        let assistant = |turn, text: &str| SessionEntry::Assistant {
            turn,
            message: Message {
                role: Role::Assistant,
                content: vec![Content::Text(text.into())],
                provider_replay: None,
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
            last_end: None,
            last_model: None,
            compacted_through: None,
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
            ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::ALT)),
            Action::Queue(prompt) if prompt == "follow up"
        ));
        assert!(ui.draft.is_empty());
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
            ui.key(KeyEvent::new(KeyCode::Char('v'), Modifiers::CONTROL)),
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
            wire: ion_core::HttpWire::ChatCompletions,
            api_key_env: String::new(),
            max_output_tokens: 1024,
            context_window_tokens: None,
            requires_key: false,
            image_input: true,
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
        let steering = SteeringInbox::default();
        busy_key(
            &mut ui,
            KeyEvent::new(KeyCode::Enter, Modifiers::NONE),
            &CancellationToken::new(),
            Some(&steering),
            None,
        );
        let queued = steering.take_uncommitted();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].content.len(), 2);
        assert_eq!(
            queued[0].content[0],
            Content::Text("describe the picture".into())
        );
        assert!(matches!(queued[0].content[1], Content::Image(_)));
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
        let steering = SteeringInbox::default();
        busy_key(
            &mut ui,
            KeyEvent::new(KeyCode::Enter, Modifiers::NONE),
            &CancellationToken::new(),
            Some(&steering),
            None,
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
        let steering = SteeringInbox::default();
        busy_key(
            &mut ui,
            KeyEvent::new(KeyCode::Enter, Modifiers::NONE),
            &CancellationToken::new(),
            Some(&steering),
            Some(&resources),
        );
        assert!(ui.draft.is_empty());
        let queued = steering.take_uncommitted();
        assert_eq!(queued.len(), 1);
        let Content::Text(prompt) = &queued[0].content[0] else {
            panic!("expected text steering");
        };
        assert!(prompt.contains("AUDIT_MARKER"));
        assert!(prompt.contains("User request: src/lib.rs"));
        assert!(matches!(queued[0].content[1], Content::Image(_)));
        assert!(ui.images.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prompt_history_restores_unsent_draft_and_moves_between_lines() {
        let mut ui = Frontend {
            prompt_history: vec!["first".into(), "second\nline".into()],
            ..Frontend::default()
        };
        ui.insert("unsent");
        ui.key(KeyEvent::new(KeyCode::Up, Modifiers::NONE));
        assert_eq!(ui.draft, "second\nline");
        ui.key(KeyEvent::new(KeyCode::Up, Modifiers::NONE));
        assert_eq!(ui.cursor, "line".len());
        ui.key(KeyEvent::new(KeyCode::Up, Modifiers::NONE));
        assert_eq!(ui.draft, "first");
        ui.key(KeyEvent::new(KeyCode::Down, Modifiers::NONE));
        assert_eq!(ui.draft, "second\nline");
        ui.key(KeyEvent::new(KeyCode::Down, Modifiers::NONE));
        assert_eq!(ui.draft, "unsent");
    }

    #[test]
    fn tool_view_keeps_output_beyond_the_compact_preview() {
        let output = format!("{}END_MARKER", "x".repeat(4_000));
        let mut ui = Frontend {
            history: vec![Message {
                role: Role::Tool,
                content: vec![Content::ToolResult(ion_ai::ToolResult {
                    call_id: "call".into(),
                    name: "exec".into(),
                    result: serde_json::json!({"stdout": output}),
                    images: Vec::new(),
                    is_error: false,
                })],
                provider_replay: None,
            }],
            ..Frontend::default()
        };
        ui.open_tool(None);
        let view = ui.tool_view.as_ref().unwrap();
        assert!(view.output.contains("END_MARKER"));
        assert!(view.output.len() > 2_048);
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
        ui.key(KeyEvent::new(KeyCode::Char('@'), Modifiers::NONE));
        ui.key(KeyEvent::new(KeyCode::Char('m'), Modifiers::NONE));
        let chosen = ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE));
        let Action::Pick(PickerValue::File { path, start, end }) = chosen else {
            panic!("file picker did not choose a path")
        };
        ui.insert_file(path, start, end);
        assert_eq!(ui.draft, "Read @src/main.rs");
        std::fs::remove_dir_all(root).unwrap();
    }
}
