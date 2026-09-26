//! Inline frontend over the durable Session. The reducer owns drafts and display
//! state; the Session remains the sole owner of turns, effects and transcript.

use std::{
    collections::VecDeque,
    io::{IsTerminal, Write},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, ensure};
use ion_core::{
    AttemptId, DriveExit, DrivePolicy, Entry, EntryData, InputSender, ProgressUpdate,
    SessionProgress, SessionSnapshot, SnapshotRequest, SubmitTurnRequest, SubmittedTurn,
    ToolOutputStream, TranscriptContent, TurnId, TurnPhase,
};
use ion_terminal::{
    Frame, InputEvent, InputStream, KeyCode, KeyEvent, Modifiers, Screen, TerminalSession,
    install_panic_hook,
};
use ratatui::text::Line;
use tokio::time::{Duration, interval};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::{Host, RunArgs, host};

const MAX_DRAFT_BYTES: usize = 64 * 1024;
const MAX_ENTRY_CHARS: usize = 8 * 1024;
const MAX_DISPLAY_ROWS: usize = 4096;
const SNAPSHOT_ENTRIES: usize = 128;

#[derive(Default)]
struct Frontend {
    draft: String,
    cursor: usize,
    rows: VecDeque<String>,
    last_entry: i64,
    status: String,
    /// A drive exit carries information that a structural snapshot cannot recover.
    exit_status: Option<(TurnId, String)>,
    unfinished: Option<TurnId>,
    progress: Option<ProgressPreview>,
    tool_progress: Option<ToolProgressPreview>,
}

struct ProgressPreview {
    attempt: AttemptId,
    text: String,
    omitted_prefix: bool,
}

struct ToolProgressPreview {
    attempt: AttemptId,
    latest: ToolOutputStream,
    stdout: Option<OutputPreview>,
    stderr: Option<OutputPreview>,
}

struct OutputPreview {
    text: String,
    omitted_prefix: bool,
}

enum Action {
    None,
    Submit(String),
    Resume,
    Cancel,
    Quit,
}

impl Frontend {
    fn key(&mut self, key: KeyEvent, driving: bool) -> Action {
        match key {
            KeyEvent {
                code: KeyCode::Char('c'),
                modifiers,
            } if modifiers.contains(Modifiers::CONTROL) => {
                if driving {
                    Action::Cancel
                } else if self.draft.is_empty() {
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
            } if modifiers.contains(Modifiers::CONTROL) && !driving && self.draft.is_empty() => {
                Action::Quit
            }
            KeyEvent {
                code: KeyCode::Enter,
                modifiers,
            } if modifiers.contains(Modifiers::SHIFT) || modifiers.contains(Modifiers::CONTROL) => {
                self.insert("\n");
                Action::None
            }
            KeyEvent {
                code: KeyCode::Char('j'),
                modifiers,
            } if modifiers.contains(Modifiers::CONTROL) => {
                self.insert("\n");
                Action::None
            }
            KeyEvent {
                code: KeyCode::Enter,
                ..
            } if !driving => {
                let text = self.draft.trim().to_owned();
                if text.is_empty() {
                    return Action::None;
                }
                self.draft.clear();
                self.cursor = 0;
                match text.as_str() {
                    "/quit" | "/exit" => Action::Quit,
                    "/resume" => Action::Resume,
                    "/cancel" => Action::Cancel,
                    _ => Action::Submit(text),
                }
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
                code: KeyCode::Backspace,
                ..
            } => {
                let before = previous_grapheme(&self.draft, self.cursor);
                self.draft.replace_range(before..self.cursor, "");
                self.cursor = before;
                Action::None
            }
            KeyEvent {
                code: KeyCode::Delete,
                ..
            } => {
                let after = next_grapheme(&self.draft, self.cursor);
                self.draft.replace_range(self.cursor..after, "");
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

    fn insert(&mut self, text: &str) {
        let cleaned = clean_input(text);
        if self.draft.len().saturating_add(cleaned.len()) > MAX_DRAFT_BYTES {
            self.status = format!("Draft limit: {MAX_DRAFT_BYTES} bytes");
            return;
        }
        self.draft.insert_str(self.cursor, &cleaned);
        self.cursor += cleaned.len();
    }

    fn observe(&mut self, snapshot: &SessionSnapshot, width: usize) {
        self.unfinished = snapshot.unfinished_turn.as_ref().map(|turn| turn.id);
        for entry in &snapshot.transcript_tail {
            if entry.id.get() <= self.last_entry {
                continue;
            }
            self.last_entry = entry.id.get();
            for row in display_entry(entry, width) {
                self.rows.push_back(row);
            }
            while self.rows.len() > MAX_DISPLAY_ROWS {
                self.rows.pop_front();
            }
        }
        if let Some(turn) = &snapshot.unfinished_turn {
            self.update_turn_status(turn.id, turn.is_cancelling(), &turn.phase);
        }
    }

    fn update_turn_status(&mut self, id: TurnId, cancelling: bool, phase: &TurnPhase) {
        self.status = if let Some((saved, message)) = &self.exit_status
            && *saved == id
        {
            message.clone()
        } else if cancelling {
            format!("Turn {}: cancelling", id.get())
        } else {
            format!("Turn {}: {phase:?}", id.get())
        };
    }

    fn observe_progress(&mut self, turn: TurnId, event: SessionProgress) {
        if event.turn != turn {
            return;
        }
        match event.update {
            ProgressUpdate::ModelText {
                text,
                omitted_prefix,
            } => {
                self.tool_progress = None;
                self.progress = Some(ProgressPreview {
                    attempt: event.attempt,
                    text,
                    omitted_prefix,
                });
            }
            ProgressUpdate::ToolOutput {
                stream,
                text,
                omitted_prefix,
            } => {
                self.progress = None;
                if self
                    .tool_progress
                    .as_ref()
                    .is_none_or(|preview| preview.attempt != event.attempt)
                {
                    self.tool_progress = Some(ToolProgressPreview {
                        attempt: event.attempt,
                        latest: stream,
                        stdout: None,
                        stderr: None,
                    });
                }
                let preview = self.tool_progress.as_mut().expect("inserted tool preview");
                preview.latest = stream;
                let output = Some(OutputPreview {
                    text,
                    omitted_prefix,
                });
                match stream {
                    ToolOutputStream::Stdout => preview.stdout = output,
                    ToolOutputStream::Stderr => preview.stderr = output,
                }
            }
            ProgressUpdate::End => {
                if self
                    .progress
                    .as_ref()
                    .is_some_and(|preview| preview.attempt == event.attempt)
                {
                    self.progress = None;
                }
                if self
                    .tool_progress
                    .as_ref()
                    .is_some_and(|preview| preview.attempt == event.attempt)
                {
                    self.tool_progress = None;
                }
            }
        }
    }

    fn render(
        &self,
        terminal: &mut TerminalSession,
        screen: &mut Screen,
        driving: bool,
    ) -> Result<()> {
        let (width, _) = screen.size();
        let width = usize::from(width);
        let committed: Vec<Line<'_>> = self
            .rows
            .iter()
            .map(|row| Line::from(row.as_str()))
            .collect();
        let hint = if driving {
            "Ctrl-C cancel · Shift-Enter newline"
        } else {
            "Enter send · Shift-Enter newline · /resume · Ctrl-D quit"
        };
        let mut live = vec![
            Line::from(truncate_cells(&self.status, width)),
            Line::from(truncate_cells(hint, width)),
        ];
        if driving {
            let mut rows = Vec::new();
            if let Some(progress) = &self.progress {
                rows.extend(progress_lines(
                    "ion",
                    &progress.text,
                    progress.omitted_prefix,
                    width,
                ));
            }
            if let Some(progress) = &self.tool_progress {
                let (label, output) = match progress.latest {
                    ToolOutputStream::Stdout => ("stdout", &progress.stdout),
                    ToolOutputStream::Stderr => ("stderr", &progress.stderr),
                };
                if let Some(output) = output {
                    rows.extend(progress_lines(
                        label,
                        &output.text,
                        output.omitted_prefix,
                        width,
                    ));
                }
            }
            let mut visible: Vec<_> = rows.into_iter().rev().take(3).collect();
            visible.reverse();
            for row in visible {
                live.push(Line::from(row));
            }
        }
        let progress_rows = live.len() - 2;
        let display = format!("› {}", self.draft);
        let (wrapped, cursor) = wrap(&display, width, Some(self.cursor + "› ".len()));
        let shown = wrapped.len().min(if driving { 3 } else { 6 });
        let skip = wrapped.len() - shown;
        for row in wrapped.into_iter().skip(skip) {
            live.push(Line::from(row));
        }
        let cursor = cursor.and_then(|(row, column)| {
            (row >= skip).then_some((committed.len() + 2 + progress_rows + row - skip, column))
        });
        terminal.render(
            screen,
            &Frame {
                committed: &committed,
                live: &live,
                cursor,
            },
        )?;
        Ok(())
    }
}

fn progress_lines(label: &str, text: &str, omitted_prefix: bool, width: usize) -> Vec<String> {
    let mut preview = clean_display(text, MAX_ENTRY_CHARS);
    if omitted_prefix {
        preview.insert(0, '…');
    }
    wrap(&format!("{label} › {preview}"), width, None).0
}

pub(super) async fn chat(args: RunArgs) -> Result<()> {
    ensure!(
        std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
        "interactive chat requires a terminal on stdin and stdout"
    );
    let mut current = host(&args.host, Some(&args)).await?;
    install_panic_hook();
    let mut terminal = TerminalSession::enter().context("interactive chat requires a terminal")?;
    let (columns, rows) = terminal.size()?;
    let (origin, live_height) = reserve_inline_region(&mut terminal, rows)?;
    let mut screen = Screen::with_live_height(columns, origin, rows, live_height);
    let mut input = terminal.input();
    let mut ui = Frontend {
        status: "Ion ready".into(),
        ..Frontend::default()
    };
    let initial = snapshot(&current.session).await?;
    ui.observe(&initial, usize::from(columns));
    if let Some(turn) = ui.unfinished {
        ui.status = format!("Turn {} unfinished; /resume or /cancel", turn.get());
    }
    ui.render(&mut terminal, &mut screen, false)?;
    let mut initial_prompt = args.prompt.clone().filter(|text| !text.trim().is_empty());
    loop {
        let action = if let Some(prompt) = initial_prompt.take() {
            Action::Submit(prompt)
        } else {
            next_action(&mut input, &mut ui, &mut terminal, &mut screen).await?
        };
        match action {
            Action::None => {}
            Action::Quit => break,
            Action::Cancel => {
                if let Some(turn) = ui.unfinished {
                    ui.exit_status = None;
                    current.session.handle().cancel_turn(turn).await?;
                    ui.status = format!("Turn {} cancellation requested", turn.get());
                } else {
                    ui.status = "No active turn".into();
                }
            }
            Action::Submit(prompt) => {
                if ui.unfinished.is_some() {
                    ui.draft = prompt;
                    ui.cursor = ui.draft.len();
                    ui.status = "Finish or cancel the current turn before submitting".into();
                } else {
                    let turn = submit(&current, prompt).await?;
                    let Some(next) = run_turn(
                        current,
                        &args,
                        turn,
                        &mut ui,
                        &mut terminal,
                        &mut screen,
                        &mut input,
                    )
                    .await?
                    else {
                        return Ok(());
                    };
                    current = next;
                }
            }
            Action::Resume => {
                if let Some(turn) = ui.unfinished {
                    let Some(next) = run_turn(
                        current,
                        &args,
                        turn,
                        &mut ui,
                        &mut terminal,
                        &mut screen,
                        &mut input,
                    )
                    .await?
                    else {
                        return Ok(());
                    };
                    current = next;
                } else {
                    ui.status = "No unfinished turn".into();
                }
            }
        }
        let view = snapshot(&current.session).await?;
        ui.observe(&view, usize::from(screen.size().0));
        ui.render(&mut terminal, &mut screen, false)?;
    }
    screen.finish(terminal.output())?;
    terminal.restore()?;
    current.session.close().await?;
    Ok(())
}

/// Advance below the launch line and reserve a stable band without repainting
/// lines that precede it. Query before the input stream exists so the cursor
/// reply has exactly one reader. When near the bottom, scroll the necessary
/// blank lines into place and account for the physical rows that moved.
fn reserve_inline_region(terminal: &mut TerminalSession, rows: u16) -> Result<(u16, usize)> {
    ensure!(rows > 0, "terminal reports zero rows");
    let band = rows.min(9);
    terminal.output().write_all(b"\r\n")?;
    terminal.output().flush()?;
    terminal.output().record_external(b"\x1b[6n")?;
    let (_, row) = terminal
        .cursor_position()
        .context("terminal cursor query failed")?;
    ensure!(row < rows, "terminal cursor is outside reported dimensions");
    let available = rows - row;
    if available >= band {
        return Ok((row, usize::from(band)));
    }
    let scroll = band - available;
    let to_bottom = rows - row - 1;
    let newlines = to_bottom + scroll;
    for _ in 0..newlines {
        terminal.output().write_all(b"\r\n")?;
    }
    terminal.output().flush()?;
    Ok((row - scroll, usize::from(band)))
}

async fn next_action(
    input: &mut InputStream,
    ui: &mut Frontend,
    terminal: &mut TerminalSession,
    screen: &mut Screen,
) -> Result<Action> {
    let Some(event) = input.next().await else {
        return Ok(Action::Quit);
    };
    let action = handle_event(event?, ui, screen, false);
    ui.render(terminal, screen, false)?;
    Ok(action)
}

fn handle_event(
    event: InputEvent,
    ui: &mut Frontend,
    screen: &mut Screen,
    driving: bool,
) -> Action {
    match event {
        InputEvent::Key(key) => ui.key(key, driving),
        InputEvent::Paste(text) => {
            ui.insert(&text);
            Action::None
        }
        InputEvent::Resize(size) => {
            screen.resize(size.columns, size.rows);
            Action::None
        }
        InputEvent::Focus(_) | InputEvent::Mouse(_) => Action::None,
    }
}

async fn run_turn(
    current: Host,
    args: &RunArgs,
    turn: TurnId,
    ui: &mut Frontend,
    terminal: &mut TerminalSession,
    screen: &mut Screen,
    input: &mut InputStream,
) -> Result<Option<Host>> {
    ui.exit_status = None;
    ui.unfinished = Some(turn);
    ui.progress = None;
    ui.tool_progress = None;
    ui.status = format!("Turn {}: running", turn.get());
    ui.render(terminal, screen, true)?;
    let handle = current.session.handle();
    let mut progress = handle.subscribe_progress();
    let mut progress_open = true;
    enum Wait {
        Exit(Result<DriveExit, ion_core::SessionError>),
        InputClosed,
    }
    let result = {
        let drive =
            handle.resume_with_tools(turn, current.models, current.tools, DrivePolicy::default());
        tokio::pin!(drive);
        let mut tick = interval(Duration::from_millis(200));
        loop {
            tokio::select! {
                exit = &mut drive => break Wait::Exit(exit),
                event = input.next() => {
                    match event {
                        Some(Ok(event)) => if matches!(handle_event(event, ui, screen, true), Action::Cancel) {
                            handle.cancel_turn(turn).await?;
                            ui.status = format!("Turn {}: cancelling", turn.get());
                        },
                        Some(Err(_)) | None => break Wait::InputClosed,
                    }
                    ui.render(terminal, screen, true)?;
                },
                event = progress.recv(), if progress_open => match event {
                    Ok(event) => ui.observe_progress(turn, event),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        ui.progress = None;
                        ui.tool_progress = None;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        progress_open = false;
                        ui.progress = None;
                        ui.tool_progress = None;
                    }
                },
                _ = tick.tick() => {
                    let view = snapshot(&current.session).await?;
                    ui.observe(&view, usize::from(screen.size().0));
                    ui.render(terminal, screen, true)?;
                }
            }
        }
    };
    let Wait::Exit(exit) = result else {
        // The frontend is gone. Closing joins any owned drive and leaves its
        // durable outcome or unresolved effect evidence for an explicit resume.
        // A hangup is never a user cancellation decision.
        current.session.close().await?;
        return Ok(None);
    };
    let view = snapshot(&current.session).await?;
    ui.observe(&view, usize::from(screen.size().0));
    ui.progress = None;
    ui.tool_progress = None;
    ui.status = match exit {
        Ok(DriveExit::Settled(outcome)) => format!("Turn {}: {outcome:?}", turn.get()),
        Ok(other) => format!(
            "Turn {}: {other:?}; /resume after checking status",
            turn.get()
        ),
        Err(error) => format!(
            "Turn {}: {error}; /resume after checking status",
            turn.get()
        ),
    };
    if ui.unfinished == Some(turn) {
        ui.exit_status = Some((turn, ui.status.clone()));
    }
    ui.render(terminal, screen, false)?;
    current.session.close().await?;
    Ok(Some(host(&args.host, None).await?))
}

async fn snapshot(session: &ion_core::Session) -> Result<SessionSnapshot> {
    Ok(session
        .handle()
        .snapshot(SnapshotRequest {
            conversation: session.primary_conversation(),
            max_inputs: 8,
            max_entries: SNAPSHOT_ENTRIES,
            max_bytes: 1024 * 1024,
        })
        .await?)
}

async fn submit(host: &Host, text: String) -> Result<TurnId> {
    ensure!(!text.trim().is_empty(), "prompt must be nonempty");
    let admitted_at_unix_ms: i64 = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_millis()
        .try_into()?;
    let submitted = host
        .session
        .handle()
        .submit_turn(SubmitTurnRequest {
            conversation: host.session.primary_conversation(),
            sender: InputSender::User,
            request_key: None,
            text,
            admitted_at_unix_ms,
            wall_deadline_unix_ms: None,
        })
        .await?;
    Ok(match submitted {
        SubmittedTurn::Created(started) => started.turn.id,
        SubmittedTurn::Replayed { turn, .. } => turn.id,
    })
}

fn display_entry(entry: &Entry, width: usize) -> Vec<String> {
    let label = match &entry.data {
        EntryData::UserInput { .. } => "you",
        EntryData::Assistant { .. } => "ion",
        EntryData::ToolResult { .. } => "tool",
        EntryData::ContextBoundary(_) => "context",
        EntryData::Notice { .. } => "notice",
    };
    let mut body = String::new();
    for message in &entry.projection {
        for content in &message.content {
            if !body.is_empty() {
                body.push('\n');
            }
            match content {
                TranscriptContent::Text(text) => body.push_str(text),
                TranscriptContent::ToolCall {
                    name, arguments, ..
                } => body.push_str(&format!("{name} {}", arguments)),
                TranscriptContent::ToolResult { name, result, .. } => {
                    body.push_str(&display_tool_result(name, result))
                }
            }
        }
    }
    if body.is_empty()
        && let EntryData::Notice { kind, detail } = &entry.data
    {
        body = format!("{kind}: {detail}");
    }
    let body = clean_display(&body, MAX_ENTRY_CHARS);
    let text = format!("{label} › {body}");
    wrap(&text, width, None).0
}

fn display_tool_result(name: &str, result: &serde_json::Value) -> String {
    let value = &result["value"];
    if result["is_error"].as_bool() == Some(true) {
        return format!(
            "{name} failed: {}",
            value["error"].as_str().unwrap_or("tool returned an error")
        );
    }
    match name {
        "list" => {
            let Some(entries) = value["entries"].as_array() else {
                return format!("{name} {result}");
            };
            let names = entries
                .iter()
                .filter_map(|entry| entry["name"].as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let more = if value["has_more"].as_bool() == Some(true) {
                " (more available)"
            } else {
                ""
            };
            format!(
                "list {}: {}{more}",
                value["path"].as_str().unwrap_or("."),
                if names.is_empty() { "(empty)" } else { &names }
            )
        }
        "read" => {
            let Some(content) = value["content"].as_str() else {
                return format!("{name} {result}");
            };
            let more = if value["has_more"].as_bool() == Some(true) {
                "\n… more available"
            } else {
                ""
            };
            format!("read:\n{content}{more}")
        }
        "edit" | "create" => {
            let (Some(path), Some(bytes)) = (value["path"].as_str(), value["bytes"].as_u64())
            else {
                return format!("{name} {result}");
            };
            format!("{name} {path}: {bytes} bytes")
        }
        _ => format!("{name} {result}"),
    }
}

fn clean_input(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .filter(|ch| *ch == '\n' || *ch == '\t' || !ch.is_control())
        .collect()
}

fn clean_display(text: &str, max_chars: usize) -> String {
    let mut output = String::new();
    for ch in text.chars().take(max_chars) {
        if ch == '\n' || ch == '\t' {
            output.push(ch);
        } else if ch.is_control() {
            output.push('�');
        } else {
            output.push(ch);
        }
    }
    if text.chars().count() > max_chars {
        output.push_str("… [truncated]");
    }
    output
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

fn truncate_cells(text: &str, width: usize) -> String {
    wrap(&clean_display(text, 512), width, None)
        .0
        .into_iter()
        .next()
        .unwrap_or_default()
}

/// Split at display-cell boundaries and map one source byte offset to a cell.
fn wrap(text: &str, width: usize, cursor: Option<usize>) -> (Vec<String>, Option<(usize, u16)>) {
    let width = width.max(1);
    let mut rows = vec![String::new()];
    let mut cells = 0usize;
    let mut cursor_at = None;
    for (at, grapheme) in text.grapheme_indices(true) {
        if cursor == Some(at) {
            cursor_at = Some((rows.len() - 1, cells.min(width - 1) as u16));
        }
        if grapheme == "\n" {
            rows.push(String::new());
            cells = 0;
            continue;
        }
        // Keep literal tabs in the submitted prompt, but render fixed cells.
        // Emitting a terminal tab would move the cursor outside our layout.
        let shown = if grapheme == "\t" { "    " } else { grapheme };
        let size = UnicodeWidthStr::width(shown).max(1);
        if cells > 0 && cells + size > width {
            rows.push(String::new());
            cells = 0;
        }
        if size <= width {
            rows.last_mut().expect("nonempty rows").push_str(shown);
            cells += size;
        }
    }
    if cursor == Some(text.len()) {
        cursor_at = Some((rows.len() - 1, cells.min(width - 1) as u16));
    }
    (rows, cursor_at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ion_terminal::KeyEvent;

    #[test]
    fn tool_display_shows_useful_output_without_internal_receipt_fields() {
        let listed = serde_json::json!({"is_error":false,"capture":"CompleteInline","value":{"path":"src","entries":[{"name":"main.rs"}],"has_more":false,"workspace_revision":{"files":3}}});
        assert_eq!(display_tool_result("list", &listed), "list src: main.rs");
        let read = serde_json::json!({"is_error":false,"capture":"CompleteInline","value":{"content":"hello\n","base_digest":"private-detail","has_more":false}});
        assert_eq!(display_tool_result("read", &read), "read:\nhello\n");
        assert!(!display_tool_result("read", &read).contains("base_digest"));
        let failure = serde_json::json!({"is_error":true,"value":{"error":"stale revision"}});
        assert_eq!(
            display_tool_result("edit", &failure),
            "edit failed: stale revision"
        );
    }

    fn key(code: KeyCode, modifiers: Modifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn paste_is_multiline_data_until_enter() {
        let mut ui = Frontend::default();
        ui.insert("one\r\ntwo");
        assert_eq!(ui.draft, "one\ntwo");
        assert!(matches!(
            ui.key(key(KeyCode::Enter, Modifiers::SHIFT), false),
            Action::None
        ));
        assert_eq!(ui.draft, "one\ntwo\n");
        assert!(
            matches!(ui.key(key(KeyCode::Enter, Modifiers::NONE), false), Action::Submit(text) if text == "one\ntwo")
        );
    }

    #[test]
    fn cursor_and_backspace_use_graphemes() {
        let mut ui = Frontend::default();
        ui.insert("a👩‍💻b");
        ui.key(key(KeyCode::Left, Modifiers::NONE), false);
        ui.key(key(KeyCode::Backspace, Modifiers::NONE), false);
        assert_eq!(ui.draft, "ab");
        assert_eq!(ui.cursor, 1);
    }

    #[test]
    fn control_sequences_cannot_enter_terminal_output() {
        let cleaned = clean_display("hi\u{1b}[31m\u{7}ok", 100);
        assert_eq!(cleaned, "hi�[31m�ok");
    }

    #[test]
    fn provisional_progress_is_attempt_scoped_and_clears() {
        let turn = TurnId::new(1).unwrap();
        let attempt = AttemptId::new(2).unwrap();
        let epoch = ion_core::SessionId::new().as_uuid();
        let mut ui = Frontend::default();
        ui.observe_progress(
            turn,
            SessionProgress {
                attachment_epoch: epoch,
                turn,
                attempt,
                update: ProgressUpdate::ModelText {
                    text: "live answer".into(),
                    omitted_prefix: false,
                },
            },
        );
        assert_eq!(ui.progress.as_ref().unwrap().text, "live answer");
        ui.observe_progress(
            turn,
            SessionProgress {
                attachment_epoch: epoch,
                turn,
                attempt: AttemptId::new(3).unwrap(),
                update: ProgressUpdate::End,
            },
        );
        assert!(ui.progress.is_some());
        ui.observe_progress(
            turn,
            SessionProgress {
                attachment_epoch: epoch,
                turn,
                attempt,
                update: ProgressUpdate::End,
            },
        );
        assert!(ui.progress.is_none());
        ui.observe_progress(
            turn,
            SessionProgress {
                attachment_epoch: epoch,
                turn,
                attempt: AttemptId::new(4).unwrap(),
                update: ProgressUpdate::ToolOutput {
                    stream: ToolOutputStream::Stderr,
                    text: "compiling".into(),
                    omitted_prefix: false,
                },
            },
        );
        assert_eq!(
            ui.tool_progress
                .as_ref()
                .unwrap()
                .stderr
                .as_ref()
                .unwrap()
                .text,
            "compiling"
        );
        ui.observe_progress(
            turn,
            SessionProgress {
                attachment_epoch: epoch,
                turn,
                attempt: AttemptId::new(4).unwrap(),
                update: ProgressUpdate::End,
            },
        );
        assert!(ui.tool_progress.is_none());
    }

    #[test]
    fn wrap_tracks_wide_cursor_and_newline() {
        let (rows, cursor) = wrap("› 界a\nb", 5, Some("› 界a".len()));
        assert_eq!(rows, vec!["› 界a", "b"]);
        assert_eq!(cursor, Some((0, 4)));
    }

    #[test]
    fn parked_drive_reason_survives_structural_snapshot_status() {
        let id = TurnId::new(7).expect("turn id");
        let message = "Turn 7: Parked(MissingCredentials); /resume after checking status";
        let mut ui = Frontend {
            exit_status: Some((id, message.into())),
            ..Frontend::default()
        };
        ui.update_turn_status(
            id,
            false,
            &TurnPhase::Parked(ion_core::ParkReason::MissingCredentials),
        );
        assert_eq!(ui.status, message);
        ui.exit_status = None;
        ui.update_turn_status(id, false, &TurnPhase::Ready);
        assert_eq!(ui.status, "Turn 7: Ready");
    }
}
