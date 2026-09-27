//! Terminal view over the same coding loop used by headless and library hosts.
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use ion_ai::{Content, Message, ModelRef, Role};
use ion_core::{CodingAgent, CodingAgentEvent, CodingSession};
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
            CodingAgentEvent::ToolStarted { name, arguments } => self
                .events
                .push(format!("→ {name} {}", brief(&arguments.to_string(), 2048))),
            CodingAgentEvent::ToolFinished { name, output } => self.events.push(format!(
                "← {name} {}: {}",
                if output.is_error { "error" } else { "done" },
                brief(&output.value.to_string(), 2048)
            )),
            CodingAgentEvent::InterruptedCalls(n) => self.events.push(format!(
                "{n} previous tool call(s) had unknown effects; inspect before retrying"
            )),
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
    model: ModelRef,
    instructions: String,
) -> Result<()> {
    install_panic_hook();
    let mut terminal = TerminalSession::enter().context("interactive chat requires a terminal")?;
    terminal.enter_alt_screen()?;
    let (width, height) = terminal.size()?;
    let mut screen = Screen::new(width, 0, height);
    let mut input = terminal.input();
    let view = session.view()?;
    let mut ui = Frontend {
        history: view.messages,
        status: "Enter to send · Shift-Enter newline · Ctrl-C clear/quit".into(),
        ..Frontend::default()
    };
    if view.unfinished_turn.is_some() {
        ui.status =
            "Previous turn interrupted; tool effects may be unknown. Inspect before retrying."
                .into();
    }
    loop {
        draw(&mut terminal, &mut screen, &ui, None, &model, false)?;
        let Some(event) = input.next().await else {
            break;
        };
        match event? {
            InputEvent::Key(key) => match ui.key(key) {
                Action::None => {}
                Action::Quit => break,
                Action::Submit(prompt) => {
                    ui.status = "Working · Ctrl-C cancels the current turn".into();
                    ui.scroll = 0;
                    run_turn(
                        &mut terminal,
                        &mut screen,
                        &mut input,
                        &mut ui,
                        &session,
                        &agent,
                        model.clone(),
                        &instructions,
                        prompt,
                    )
                    .await?;
                }
            },
            InputEvent::Paste(text) => ui.insert(&text),
            InputEvent::Resize(size) => screen.resize(size.columns, size.rows),
            InputEvent::Mouse(mouse) => match mouse.kind() {
                MouseKind::ScrollUp => ui.scroll = ui.scroll.saturating_add(3),
                MouseKind::ScrollDown => ui.scroll = ui.scroll.saturating_sub(3),
                _ => {}
            },
            InputEvent::Focus(_) => {}
        }
    }
    terminal.restore()?;
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
    model: ModelRef,
    instructions: &str,
    prompt: String,
) -> Result<()> {
    let progress = Arc::new(Mutex::new(Progress::default()));
    let observer = progress.clone();
    let stop = CancellationToken::new();
    let turn = agent.submit(
        session,
        model.clone(),
        prompt,
        instructions.to_owned(),
        stop.clone(),
        move |event| {
            observer
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .observe(event);
        },
    );
    tokio::pin!(turn);
    let mut tick = interval(Duration::from_millis(50));
    let mut input_ended = false;
    let result = loop {
        tokio::select! {
            result = &mut turn => break result,
            event = input.next(), if !input_ended => match event {
                Some(Ok(InputEvent::Key(KeyEvent { code: KeyCode::Char('c'), modifiers }))) if modifiers.contains(Modifiers::CONTROL) => { stop.cancel(); ui.status = "Cancelling…".into(); },
                Some(Ok(InputEvent::Resize(size))) => screen.resize(size.columns, size.rows),
                Some(Ok(InputEvent::Mouse(mouse))) => match mouse.kind() {
                    MouseKind::ScrollUp => ui.scroll = ui.scroll.saturating_add(3),
                    MouseKind::ScrollDown => ui.scroll = ui.scroll.saturating_sub(3),
                    _ => {},
                },
                Some(Ok(_)) => {},
                Some(Err(error)) => { stop.cancel(); input_ended = true; ui.status = format!("Input failed: {error}. Cancelling…"); },
                None => { stop.cancel(); input_ended = true; },
            },
            _ = tick.tick() => {
                let preview = progress.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                draw(terminal, screen, ui, Some(&preview), &model, true)?;
            }
        }
    };
    ui.history = session.view()?.messages;
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
    Quit,
}

impl Frontend {
    fn key(&mut self, key: KeyEvent) -> Action {
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
            } => {
                let prompt = self.draft.trim().to_owned();
                self.draft.clear();
                self.cursor = 0;
                if prompt.is_empty() {
                    Action::None
                } else if prompt == "/exit" || prompt == "/quit" {
                    Action::Quit
                } else {
                    Action::Submit(prompt)
                }
            }
            KeyEvent {
                code: KeyCode::Backspace,
                ..
            } => {
                let start = previous_grapheme(&self.draft, self.cursor);
                self.draft.replace_range(start..self.cursor, "");
                self.cursor = start;
                Action::None
            }
            KeyEvent {
                code: KeyCode::Delete,
                ..
            } => {
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
    fn insert(&mut self, text: &str) {
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
        self.draft.insert_str(self.cursor, &clean);
        self.cursor += clean.len();
    }
}

fn draw(
    terminal: &mut TerminalSession,
    screen: &mut Screen,
    ui: &Frontend,
    progress: Option<&Progress>,
    model: &ModelRef,
    busy: bool,
) -> Result<()> {
    let (width, height) = terminal.size()?;
    screen.resize(width, height);
    let width = width.max(1) as usize;
    let height = height.max(1) as usize;
    let mut composer = wrap_input(&ui.draft, ui.cursor, width);
    if busy {
        composer.lines = vec!["… working (Ctrl-C cancels)".into()];
    }
    let chrome_height = if height >= 3 { 2 } else { 0 };
    let composer_height = composer.lines.len().min(4).min(height - chrome_height);
    let composer_start = if busy {
        0
    } else {
        composer
            .cursor_row
            .saturating_sub(composer_height - 1)
            .min(composer.lines.len().saturating_sub(composer_height))
    };
    let history_height = height - composer_height - chrome_height;
    let mut history = history_rows(&ui.history, width);
    if let Some(progress) = progress {
        if !progress.text.is_empty() {
            push_wrapped(&mut history, &format!("ion> {}", progress.text), width);
        }
        for event in &progress.events {
            push_wrapped(&mut history, event, width);
        }
    }
    if history.len() > MAX_ROWS {
        history.drain(..history.len() - MAX_ROWS);
    }
    let end = history.len().saturating_sub(ui.scroll.min(history.len()));
    let start = end.saturating_sub(history_height);
    let mut rows = vec![Line::raw(""); height];
    let padding = history_height.saturating_sub(end - start);
    for (i, row) in history[start..end].iter().enumerate() {
        rows[padding + i] = Line::raw(row.clone());
    }
    if chrome_height > 0 {
        rows[history_height] = Line::raw("─".repeat(width));
        rows[history_height + 1] = Line::raw(brief(
            &format!("{} / {} · {}", model.provider, model.model, ui.status),
            width,
        ));
    }
    let composer_row = height - composer_height;
    for i in 0..composer_height {
        rows[composer_row + i] = Line::raw(composer.lines[composer_start + i].clone());
    }
    let cursor = if busy {
        None
    } else {
        let row = composer_row + composer.cursor_row.saturating_sub(composer_start);
        (row < height).then_some((row, composer.cursor_col.min(width.saturating_sub(1)) as u16))
    };
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
}
