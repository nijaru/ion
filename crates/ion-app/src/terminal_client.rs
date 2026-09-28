//! Terminal view over the same coding loop used by headless and library hosts.
use std::{
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result};
use ignore::WalkBuilder;
use ion_ai::{Content, Message, ModelRef, Role};
use ion_core::{CodingAgent, CodingAgentEvent, CodingSession, SteeringInbox};
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
    pending: VecDeque<String>,
    prompt_history: Vec<String>,
    history_cursor: Option<usize>,
    saved_draft: String,
    tool_view: Option<ToolView>,
    cwd: PathBuf,
}

struct ToolView {
    label: String,
    output: String,
    scroll: usize,
}

enum PickerValue {
    Session(PathBuf),
    Model(ModelRef),
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
        let agent = self.host.agent(&session, &selected)?;
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
        let agent = self.host.agent(&session, &selected)?;
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

    fn select_model(&mut self, model: ModelRef) -> Result<()> {
        let selected = self.host.models().resolve_identity(&model)?;
        selected.require_access(self.host.credentials())?;
        let agent = self.host.agent(&self.session, &selected)?;
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
                "← {name} {}: {}",
                if output.is_error { "error" } else { "done" },
                brief(&output.value.to_string(), 2048)
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

pub async fn chat(
    session: Arc<CodingSession>,
    agent: Arc<CodingAgent>,
    selected: Selection,
    instructions: String,
    resources: Resources,
    sessions: SessionCatalog,
    host: Arc<Host>,
) -> Result<()> {
    let mut runtime = ChatRuntime {
        session,
        agent,
        selected,
        instructions,
        resources,
        sessions,
        host,
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
        ..Frontend::default()
    };
    ui.refresh_session(&runtime.session)?;
    loop {
        ui.context_window_tokens = runtime.selected.context_window_tokens;
        ui.update_context(&runtime.session)?;
        if let Some(prompt) = ui.pending.pop_front() {
            let prompt = match expand_resource_input(&runtime.resources, prompt) {
                Ok(prompt) => prompt,
                Err((original, error)) => {
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
                runtime.selected.identity(),
                &runtime.instructions,
                &runtime.resources,
                prompt,
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
                    run_turn(
                        &mut terminal,
                        &mut screen,
                        &mut input,
                        &mut ui,
                        &runtime.session,
                        &runtime.agent,
                        runtime.selected.identity(),
                        &runtime.instructions,
                        &runtime.resources,
                        prompt,
                    )
                    .await?;
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
                            runtime.selected.identity(),
                        )
                        .await?;
                    } else {
                        match handle_command(&mut runtime, &mut ui, &command) {
                            Ok(Some(prompt)) => {
                                ui.status =
                                    "Working · Enter steers · Alt-Enter queues · Ctrl-C cancels"
                                        .into();
                                ui.scroll = 0;
                                run_turn(
                                    &mut terminal,
                                    &mut screen,
                                    &mut input,
                                    &mut ui,
                                    &runtime.session,
                                    &runtime.agent,
                                    runtime.selected.identity(),
                                    &runtime.instructions,
                                    &runtime.resources,
                                    prompt,
                                )
                                .await?;
                            }
                            Ok(None) => {}
                            Err(error) => ui.status = format!("{error:#}"),
                        }
                    }
                }
                Action::Queue(prompt) => ui.pending.push_back(prompt),
                Action::Pick(value) => {
                    if let PickerValue::File { path, start, end } = value {
                        ui.insert_file(path, start, end);
                        continue;
                    }
                    let result = match value {
                        PickerValue::Session(path) => runtime.switch_session(path),
                        PickerValue::Model(model) => runtime.select_model(model),
                        PickerValue::File { .. } => unreachable!("handled above"),
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

fn handle_command(
    runtime: &mut ChatRuntime,
    ui: &mut Frontend,
    command: &str,
) -> Result<Option<String>> {
    let (name, args) = command.split_once(' ').unwrap_or((command, ""));
    let args = args.trim();
    match name {
        "/help" => ui.note(
            "/new /clone /resume /session /name NAME /model /compact /tools /tool [N] /skills /prompts /reload /login PROVIDER /logout PROVIDER /quit".into(),
        ),
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
    model: ModelRef,
) -> Result<()> {
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
                _ = tick.tick() => draw(terminal, screen, ui, None, &model, true)?,
            }
        }
    };
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
    match ui.key(key) {
        Action::Submit(prompt) => {
            if let Some(steering) = steering {
                steering.push(prompt);
                ui.status = "Steering sent for the next model step".into();
            } else {
                ui.pending.push_back(prompt);
                ui.status = format!("{} follow-up(s) queued", ui.pending.len());
            }
        }
        Action::Queue(prompt) => {
            ui.pending.push_back(prompt);
            ui.status = format!("{} follow-up(s) queued", ui.pending.len());
        }
        Action::Command(command) => {
            if let (Some(steering), Some(resources)) = (steering, resources)
                && let Some(expanded) = resources.expand_command(&command)
            {
                match expanded {
                    Ok(prompt) => {
                        steering.push(prompt);
                        ui.status = "Steering sent for the next model step".into();
                        return;
                    }
                    Err(error) => ui.status = format!("{error:#}"),
                }
            } else {
                ui.status = "Commands are available after this operation".into();
            }
            ui.draft = command;
            ui.cursor = ui.draft.len();
        }
        Action::Quit => stop.cancel(),
        Action::Pick(PickerValue::File { path, start, end }) => {
            ui.insert_file(path, start, end);
        }
        Action::Pick(_) | Action::None => {}
    }
}

fn return_pending_to_editor(ui: &mut Frontend) {
    if ui.pending.is_empty() {
        return;
    }
    let remaining = ui.pending.drain(..).collect::<Vec<_>>().join("\n\n");
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
async fn run_turn(
    terminal: &mut TerminalSession,
    screen: &mut Screen,
    input: &mut InputStream,
    ui: &mut Frontend,
    session: &CodingSession,
    agent: &CodingAgent,
    model: ModelRef,
    instructions: &str,
    resources: &Resources,
    prompt: String,
) -> Result<()> {
    let progress = Arc::new(Mutex::new(Progress::default()));
    let observer = progress.clone();
    let stop = CancellationToken::new();
    let steering = SteeringInbox::default();
    let mut input_ended = false;
    let result = {
        let turn = agent.submit_with_steering(
            session,
            model.clone(),
            prompt,
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
                    let preview = progress.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    draw(terminal, screen, ui, Some(&preview), &model, true)?;
                }
            }
        }
    };
    for prompt in steering.take_uncommitted() {
        ui.pending.push_back(prompt);
    }
    if result.is_err() {
        return_pending_to_editor(ui);
    }
    let view = session.view()?;
    ui.context_label = context_label(&view, ui.context_window_tokens);
    ui.load_history(view.messages);
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
    Queue(String),
    Command(String),
    Pick(PickerValue),
    Quit,
}

impl Frontend {
    fn refresh_session(&mut self, session: &CodingSession) -> Result<()> {
        let view = session.view()?;
        self.context_label = context_label(&view, self.context_window_tokens);
        self.load_history(view.messages);
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

    fn load_history(&mut self, messages: Vec<Message>) {
        self.history = messages;
        self.prompt_history = self
            .history
            .iter()
            .filter(|message| message.role == Role::User)
            .flat_map(|message| message.content.iter())
            .filter_map(|content| match content {
                Content::Text(text) => Some(text.clone()),
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
                if prompt.is_empty() {
                    Action::None
                } else if prompt == "/exit" || prompt == "/quit" {
                    Action::Quit
                } else if prompt.starts_with('/') {
                    Action::Command(prompt)
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
            output: serde_json::to_string_pretty(&result.result)
                .unwrap_or_else(|_| result.result.to_string()),
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
        if let Some(prompt) = self.pending.pop_back() {
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
            rows[history_height + 1] = Line::raw(brief(&ui.status, width));
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
                    if let Content::Text(text) = item {
                        push_wrapped(
                            &mut rows,
                            &format!("you> {}", brief(text, MAX_PREVIEW)),
                            width,
                        );
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
                                "← {} {}",
                                result.name,
                                brief(&result.result.to_string(), 2048)
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
        assert!(queued[0].contains("AUDIT_MARKER"));
        assert!(queued[0].contains("User request: src/lib.rs"));
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
