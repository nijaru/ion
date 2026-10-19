//! Terminal attachment view: one live human input lease over a PTY program.
//!
//! The view borrows the existing screen projection: no local echo, no second
//! emulator and no transcript/OSC mutation. Detaching restores the composer and
//! leaves the program running. Ctrl-] is the only local binding; every other
//! key is encoded for the program, including Ctrl-C.
use std::time::Duration;

use anyhow::{Context, Result, bail};
use ion_host::{SessionControl, TerminalAttachment};
use ion_terminal::{InputEvent, KeyCode, KeyEvent, Modifiers, Screen, TerminalSession};
use ratatui::text::Line;
use uuid::Uuid;

use super::EditorDraft;
use crate::display_text::fit_line;

const DETACH_MODIFIER: Modifiers = Modifiers::CONTROL;
const DETACH_CHAR: char = ']';
const MAX_REMOTE_ROWS: u16 = 200;
const MAX_REMOTE_COLUMNS: u16 = 400;
const MAX_REMOTE_CELLS: u32 = 20_000;
const PAGE_ROWS: u16 = 64;
const HINT: &str = "Attached · Ctrl-] detach · the program keeps running";
const MAX_PASTE_CHUNK: usize = 16 * 1024;

/// Run `/attach [ID]`, returning a status line when the composer is restored.
pub(super) async fn run(
    runtime: &SessionControl<EditorDraft>,
    terminal: &mut TerminalSession,
    screen: &mut Screen,
    args: &str,
) -> Result<Option<String>> {
    let Some(owner) = runtime.owner() else {
        bail!("terminal attachment requires the coding application owner");
    };
    let session = runtime.session().id();
    let cwd = runtime
        .session()
        .cwd()
        .context("terminal attachment requires a captured working directory")?
        .to_path_buf();
    let processes = owner.processes();
    let id = if args.is_empty() {
        let terminals = processes.list_terminals(session, &cwd).await;
        let running = terminals
            .iter()
            .filter(|terminal| terminal.running)
            .collect::<Vec<_>>();
        match running.as_slice() {
            [] => bail!("no running PTY programs in this Session; start one with backend pty"),
            [only] => only.id,
            many => bail!(
                "several PTY programs are running; attach with /attach <id>: {}",
                many.iter()
                    .map(|terminal| format!(
                        "{}{}",
                        terminal.id,
                        if terminal.attached { " (attached)" } else { "" }
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    } else {
        Uuid::parse_str(args).context("attach expects a process handle UUID")?
    };
    let attachment = processes
        .attach_terminal(session, &cwd, id)
        .await
        .map_err(anyhow::Error::msg)?;
    view(terminal, screen, attachment).await?;
    Ok(Some("Detached; the program keeps running".into()))
}

/// Draw and drive the program until detach, exit or a terminal-level failure.
async fn view(
    terminal: &mut TerminalSession,
    screen: &mut Screen,
    attachment: TerminalAttachment,
) -> Result<()> {
    let already_fullscreen = terminal.is_alt_screen()?;
    if !already_fullscreen {
        terminal
            .enter_alt_screen()
            .context("enter the attachment surface")?;
    }
    let outcome = drive(terminal, screen, &attachment).await;
    // Detach does not stop the program; a failed restore still releases the
    // lease through the guard below so the model regains input custody.
    let restore = async {
        if !already_fullscreen && terminal.is_alt_screen()? {
            terminal.leave_alt_screen()?;
        }
        screen.invalidate();
        Ok::<(), anyhow::Error>(())
    }
    .await;
    attachment.release().await;
    outcome.and(restore)
}

async fn drive(
    terminal: &mut TerminalSession,
    screen: &mut Screen,
    attachment: &TerminalAttachment,
) -> Result<()> {
    let mut notice: Option<String> = None;
    let mut remote = (0u16, 0u16);
    loop {
        if !attachment.is_live() {
            return Ok(());
        }
        let (width, height) = terminal.size()?;
        let width = width.max(1);
        let height = height.max(1);
        screen.resize_fullscreen(width, height);
        // One physical row is reserved for the detach hint; the program gets
        // the rest of the viewport, capped by the driver's real dimensions.
        let content_height = height.saturating_sub(1).max(1);
        let dimensions = remote_dimensions(content_height, width);
        if dimensions != remote {
            match attachment.resize(dimensions.0, dimensions.1).await {
                Ok(()) => remote = dimensions,
                Err(error) => notice = Some(format!("Resize failed: {error}")),
            }
        }
        let mut rows = vec![Line::raw(""); height as usize];
        let mut cursor = None;
        let mut start = 0u16;
        while start < content_height {
            let page_rows = PAGE_ROWS.min(content_height - start);
            match attachment.page(start, page_rows) {
                Ok(page) => {
                    let lines = page.text.split('\n').collect::<Vec<_>>();
                    for (index, line) in lines.iter().take(page_rows as usize).enumerate() {
                        rows[start as usize + index] = Line::raw(fit_line(line, width as usize));
                    }
                    let meta = &page.screen;
                    if start == 0
                        && meta.cursor_visible
                        && meta.cursor_row < content_height
                        && meta.cursor_column < width
                    {
                        cursor = Some((
                            meta.cursor_row as usize,
                            meta.cursor_column.min(width.saturating_sub(1)),
                        ));
                    }
                }
                Err(error) => {
                    // Never present a stale screen as current state.
                    notice = Some(format!("Screen projection unavailable: {error}"));
                    break;
                }
            }
            start += page_rows;
        }
        let hint = match &notice {
            Some(notice) => format!("{notice} · Ctrl-] detach"),
            None => HINT.to_owned(),
        };
        rows[(height - 1) as usize] = Line::raw(fit_line(&hint, width as usize));
        screen.draw_fullscreen(&mut terminal.output()?, &rows, cursor)?;

        let event = tokio::select! {
            event = terminal.next_input() => event,
            () = tokio::time::sleep(Duration::from_millis(50)) => continue,
        };
        let event = event.context("terminal input ended unexpectedly")??;
        match event {
            InputEvent::Key(key)
                if key.code == KeyCode::Char(DETACH_CHAR) && key.modifiers == DETACH_MODIFIER =>
            {
                return Ok(());
            }
            InputEvent::Key(key) => {
                let application_cursor = attachment
                    .page(0, 1)
                    .map(|page| page.screen.application_cursor)
                    .unwrap_or(false);
                let bytes = encode_key(key, application_cursor);
                let Some(bytes) = bytes else {
                    notice = Some("Key cannot be represented on this terminal".into());
                    continue;
                };
                match attachment.write(bytes).await {
                    write if write.error.is_some() || write.cancelled => {
                        notice = Some(format!(
                            "Input incomplete after {} bytes: {}",
                            write.bytes_written,
                            write.error.as_deref().unwrap_or("cancelled")
                        ));
                    }
                    _ => notice = None,
                }
            }
            InputEvent::Paste(text) => {
                let meta = attachment
                    .page(0, 1)
                    .map(|page| page.screen.bracketed_paste)
                    .unwrap_or(false);
                let mut failure = None;
                for chunk in text.as_bytes().chunks(MAX_PASTE_CHUNK) {
                    let mut bytes = Vec::with_capacity(chunk.len() + 12);
                    if meta {
                        bytes.extend_from_slice(b"\x1b[200~");
                    }
                    bytes.extend_from_slice(chunk);
                    if meta {
                        bytes.extend_from_slice(b"\x1b[201~");
                    }
                    let write = attachment.write(bytes).await;
                    if write.error.is_some() || write.cancelled {
                        failure = Some(format!(
                            "Paste incomplete after {} bytes: {}",
                            write.bytes_written,
                            write.error.as_deref().unwrap_or("cancelled")
                        ));
                        break;
                    }
                }
                notice = failure;
            }
            InputEvent::Resize(_) => {}
            InputEvent::Rejected(error) => notice = Some(error.to_string()),
            InputEvent::Mouse(_) => {}
        }
    }
}

/// Fit the remote grid into the host viewport and the driver's cell budget.
fn remote_dimensions(content_height: u16, width: u16) -> (u16, u16) {
    let rows = content_height.clamp(1, MAX_REMOTE_ROWS);
    let columns = width.clamp(1, MAX_REMOTE_COLUMNS);
    let columns = columns.min((MAX_REMOTE_CELLS / u32::from(rows)).max(1) as u16);
    (rows, columns)
}

/// Encode one key for the remote terminal. `None` means Ion cannot represent
/// the key faithfully; it is reported rather than silently dropped.
fn encode_key(key: KeyEvent, application_cursor: bool) -> Option<Vec<u8>> {
    use KeyCode::*;
    let modifiers = key.modifiers;
    let control = modifiers.contains(Modifiers::CONTROL);
    let alt = modifiers.contains(Modifiers::ALT);
    let mut bytes = Vec::new();
    if alt && !control {
        bytes.push(0x1b);
    }
    match key.code {
        Char(ch) => {
            if control {
                let byte = control_byte(ch)?;
                if alt {
                    bytes.insert(0, 0x1b);
                }
                bytes.push(byte);
            } else {
                let mut buffer = [0u8; 4];
                bytes.extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes());
            }
        }
        Enter => bytes.push(b'\r'),
        Tab | BackTab => {
            if key.code == BackTab || modifiers.contains(Modifiers::SHIFT) {
                bytes.extend_from_slice(b"\x1b[Z");
            } else {
                bytes.push(b'\t');
            }
        }
        Backspace => bytes.push(0x7f),
        Esc => bytes.push(0x1b),
        Up => cursor_key(&mut bytes, b'A', application_cursor),
        Down => cursor_key(&mut bytes, b'B', application_cursor),
        Right => cursor_key(&mut bytes, b'C', application_cursor),
        Left => cursor_key(&mut bytes, b'D', application_cursor),
        Home => {
            if application_cursor {
                bytes.extend_from_slice(b"\x1bOH");
            } else {
                bytes.extend_from_slice(b"\x1b[H");
            }
        }
        End => {
            if application_cursor {
                bytes.extend_from_slice(b"\x1bOF");
            } else {
                bytes.extend_from_slice(b"\x1b[F");
            }
        }
        PageUp => bytes.extend_from_slice(b"\x1b[5~"),
        PageDown => bytes.extend_from_slice(b"\x1b[6~"),
        Insert => bytes.extend_from_slice(b"\x1b[2~"),
        Delete => bytes.extend_from_slice(b"\x1b[3~"),
        F(number) => match number {
            1..=4 => {
                bytes.extend_from_slice(b"\x1bO");
                bytes.push(b'P' + (number - 1));
            }
            5 => bytes.extend_from_slice(b"\x1b[15~"),
            6 => bytes.extend_from_slice(b"\x1b[17~"),
            7 => bytes.extend_from_slice(b"\x1b[18~"),
            8 => bytes.extend_from_slice(b"\x1b[19~"),
            9 => bytes.extend_from_slice(b"\x1b[20~"),
            10 => bytes.extend_from_slice(b"\x1b[21~"),
            11 => bytes.extend_from_slice(b"\x1b[23~"),
            12 => bytes.extend_from_slice(b"\x1b[24~"),
            _ => return None,
        },
        Other => return None,
    }
    Some(bytes)
}

fn cursor_key(bytes: &mut Vec<u8>, final_byte: u8, application: bool) {
    if application {
        bytes.extend_from_slice(b"\x1bO");
    } else {
        bytes.extend_from_slice(b"\x1b[");
    }
    bytes.push(final_byte);
}

/// Control bytes for printable characters, following the xterm convention.
fn control_byte(ch: char) -> Option<u8> {
    let byte = match ch {
        ' ' | '@' => 0x00,
        'a'..='z' => ch as u8 - b'a' + 1,
        'A'..='Z' => ch as u8 - b'A' + 1,
        '[' => 0x1b,
        '\\' => 0x1c,
        ']' => 0x1d,
        '^' => 0x1e,
        '_' => 0x1f,
        '?' => 0x7f,
        _ => return None,
    };
    Some(byte)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: Modifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn encode(code: KeyCode, modifiers: Modifiers, application_cursor: bool) -> Vec<u8> {
        encode_key(key(code, modifiers), application_cursor).expect("representable key")
    }

    #[test]
    fn ordinary_keys_and_control_bytes_are_encoded_for_the_program() {
        assert_eq!(encode(KeyCode::Char('a'), Modifiers::NONE, false), b"a");
        assert_eq!(encode(KeyCode::Enter, Modifiers::NONE, false), b"\r");
        assert_eq!(encode(KeyCode::Tab, Modifiers::NONE, false), b"\t");
        assert_eq!(encode(KeyCode::BackTab, Modifiers::SHIFT, false), b"\x1b[Z");
        assert_eq!(
            encode(KeyCode::Backspace, Modifiers::NONE, false),
            vec![0x7f]
        );
        assert_eq!(encode(KeyCode::Esc, Modifiers::NONE, false), vec![0x1b]);
        assert_eq!(
            encode(KeyCode::Char('c'), Modifiers::CONTROL, false),
            vec![0x03]
        );
        assert_eq!(
            encode(KeyCode::Char('['), Modifiers::CONTROL, false),
            vec![0x1b]
        );
        assert_eq!(encode(KeyCode::Char('x'), Modifiers::ALT, false), b"\x1bx");
        assert_eq!(
            encode(KeyCode::Char('é'), Modifiers::NONE, false),
            "é".as_bytes()
        );
    }

    #[test]
    fn cursor_keys_follow_the_program_application_mode() {
        assert_eq!(encode(KeyCode::Up, Modifiers::NONE, false), b"\x1b[A");
        assert_eq!(encode(KeyCode::Up, Modifiers::NONE, true), b"\x1bOA");
        assert_eq!(encode(KeyCode::Home, Modifiers::NONE, false), b"\x1b[H");
        assert_eq!(encode(KeyCode::Home, Modifiers::NONE, true), b"\x1bOH");
        assert_eq!(encode(KeyCode::Delete, Modifiers::NONE, false), b"\x1b[3~");
        assert_eq!(encode(KeyCode::F(1), Modifiers::NONE, false), b"\x1bOP");
        assert_eq!(encode(KeyCode::F(5), Modifiers::NONE, false), b"\x1b[15~");
    }

    #[test]
    fn unrepresentable_keys_and_oversize_grids_are_explicit() {
        assert!(encode_key(key(KeyCode::F(13), Modifiers::NONE), false).is_none());
        assert!(encode_key(key(KeyCode::Other, Modifiers::NONE), false).is_none());
        assert!(encode_key(key(KeyCode::Char('1'), Modifiers::CONTROL), false).is_none());
        assert_eq!(remote_dimensions(200, 400), (200, 100));
        assert_eq!(remote_dimensions(80, 300), (80, 250));
        assert_eq!(remote_dimensions(0, 0), (1, 1));
    }
}
