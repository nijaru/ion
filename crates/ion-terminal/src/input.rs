use std::collections::VecDeque;
use std::fs::File;
use std::io;
use std::os::fd::AsFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crossterm::terminal;
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use termwiz::input::{self as term_input, InputParser};
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::sync::mpsc;

/// Terminal dimensions in columns and rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub columns: u16,
    pub rows: u16,
}

/// Application-owned key codes. Crossterm remains an implementation detail of
/// the terminal substrate rather than a UI-state contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    Backspace,
    Enter,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Tab,
    BackTab,
    Delete,
    Insert,
    F(u8),
    Char(char),
    Esc,
    Other,
}

/// Modifier flags in the application-owned input vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Modifiers(u8);

impl Modifiers {
    pub const NONE: Self = Self(0);
    pub const SHIFT: Self = Self(1 << 0);
    pub const CONTROL: Self = Self(1 << 1);
    pub const ALT: Self = Self(1 << 2);

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl std::ops::BitOr for Modifiers {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for Modifiers {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// A decoded key press. Key release/repeat distinctions are deliberately
/// normalized until the UI has an owner that needs them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub modifiers: Modifiers,
}

impl KeyEvent {
    #[must_use]
    pub const fn new(code: KeyCode, modifiers: Modifiers) -> Self {
        Self { code, modifiers }
    }
}

/// Mouse input, typed at the boundary; the raw event stays private to
/// this crate. Fullscreen frontends decode scroll steps and clicks from
/// this vocabulary only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MouseEvent(term_input::MouseEvent);

/// The application-owned mouse vocabulary a frontend needs: wheel
/// steps and button presses with their cell position. Coordinates are
/// 0-based cell (column, row) in the terminal viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    ScrollUp,
    ScrollDown,
    ScrollLeft,
    ScrollRight,
    Press(u8),
    Release(u8),
    Move,
}

impl MouseEvent {
    /// What happened, in frontend terms.
    #[must_use]
    pub fn kind(&self) -> MouseKind {
        use term_input::MouseButtons as Button;
        let buttons = &self.0.mouse_buttons;
        if buttons.contains(Button::VERT_WHEEL) {
            return if buttons.contains(Button::WHEEL_POSITIVE) {
                MouseKind::ScrollUp
            } else {
                MouseKind::ScrollDown
            };
        }
        if buttons.contains(Button::HORZ_WHEEL) {
            return if buttons.contains(Button::WHEEL_POSITIVE) {
                MouseKind::ScrollRight
            } else {
                MouseKind::ScrollLeft
            };
        }
        if buttons.contains(Button::LEFT) {
            MouseKind::Press(0)
        } else if buttons.contains(Button::RIGHT) {
            MouseKind::Press(1)
        } else if buttons.contains(Button::MIDDLE) {
            MouseKind::Press(2)
        } else {
            MouseKind::Move
        }
    }

    /// 0-based cell column of the event.
    #[must_use]
    pub fn column(&self) -> u16 {
        self.0.x.saturating_sub(1)
    }

    /// 0-based cell row of the event.
    #[must_use]
    pub fn row(&self) -> u16 {
        self.0.y.saturating_sub(1)
    }
}

/// All terminal-originated input consumed by a frontend. Stream end is
/// `None`, not an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEvent {
    Key(KeyEvent),
    Paste(String),
    Mouse(MouseEvent),
    Resize(Size),
}

/// The single terminal input reader for a live frontend.
#[derive(Debug)]
pub struct InputStream {
    reader: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    chunks: mpsc::Receiver<io::Result<Vec<u8>>>,
    resize: Signal,
    parser: InputParser,
    replies: TerminalReplyFilter,
    pending: VecDeque<InputEvent>,
    waiting: bool,
    eof: bool,
    escape_grace: Duration,
}

impl InputStream {
    pub(crate) fn new() -> io::Result<Self> {
        let resize = signal(SignalKind::window_change())?;
        // A separate open file description keeps O_NONBLOCK off stdout and
        // the synchronous stdin used by masked credential prompts.
        let stdin = File::open("/dev/tty")?;
        let original_flags = fcntl_getfl(stdin.as_fd())?;
        fcntl_setfl(stdin.as_fd(), original_flags | OFlags::NONBLOCK)?;
        let (sender, chunks) = mpsc::channel(32);
        let stop = Arc::new(AtomicBool::new(false));
        let reader_stop = Arc::clone(&stop);
        let reader = thread::Builder::new()
            .name("ion-terminal-input".into())
            .spawn(move || read_chunks(stdin, sender, &reader_stop))?;
        let remote =
            std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some();
        Ok(Self {
            reader: Some(reader),
            stop,
            chunks,
            resize,
            parser: InputParser::new(),
            replies: TerminalReplyFilter::default(),
            pending: VecDeque::new(),
            waiting: false,
            eof: false,
            escape_grace: Duration::from_millis(if remote { 100 } else { 10 }),
        })
    }

    /// Release stdin before a synchronous credential prompt takes it.
    pub fn suspend(&mut self) -> io::Result<()> {
        self.stop.store(true, Ordering::Release);
        if let Some(reader) = self.reader.take() {
            reader
                .join()
                .map_err(|_| io::Error::other("terminal reader panicked"))?;
        }
        Ok(())
    }

    /// Read the next decoded event, preserving stream termination and I/O
    /// errors for the owning runtime to handle explicitly.
    pub async fn next(&mut self) -> Option<io::Result<InputEvent>> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Some(Ok(event));
            }
            if self.eof {
                return None;
            }
            tokio::select! {
                result = self.chunks.recv() => match result {
                    None => {
                        self.eof = true;
                        self.parse(&[], false);
                    }
                    Some(Ok(bytes)) => self.parse(&bytes, true),
                    Some(Err(error)) => return Some(Err(error)),
                },
                resize = self.resize.recv() => {
                    if resize.is_none() {
                        return Some(Err(io::Error::other("terminal resize signal closed")));
                    }
                    match terminal::size() {
                        Ok((columns, rows)) => return Some(Ok(InputEvent::Resize(Size { columns, rows }))),
                        Err(error) => return Some(Err(error)),
                    }
                },
                () = tokio::time::sleep(self.escape_grace), if self.waiting => self.parse(&[], false),
            }
        }
    }

    fn parse(&mut self, bytes: &[u8], maybe_more: bool) {
        let bytes = self.replies.feed(bytes, !maybe_more);
        self.parser.parse(
            &bytes,
            |event| {
                if let Some(decoded) = Self::decode(event) {
                    self.pending.push_back(decoded);
                }
            },
            maybe_more,
        );
        self.waiting = maybe_more;
    }

    fn decode(event: term_input::InputEvent) -> Option<InputEvent> {
        match event {
            term_input::InputEvent::Key(key) => {
                let modifiers = decode_modifiers(key.modifiers);
                let code = if key.key == term_input::KeyCode::Tab
                    && modifiers.contains(Modifiers::SHIFT)
                {
                    KeyCode::BackTab
                } else {
                    decode_code(key.key)
                };
                Some(InputEvent::Key(KeyEvent { code, modifiers }))
            }
            term_input::InputEvent::Paste(text) => Some(InputEvent::Paste(text)),
            term_input::InputEvent::Mouse(mouse) => Some(InputEvent::Mouse(MouseEvent(mouse))),
            term_input::InputEvent::Resized { cols, rows } => Some(InputEvent::Resize(Size {
                columns: cols.try_into().unwrap_or(u16::MAX),
                rows: rows.try_into().unwrap_or(u16::MAX),
            })),
            term_input::InputEvent::PixelMouse(_) | term_input::InputEvent::Wake => None,
        }
    }
}

/// Crossterm's startup query can leave a late private CSI reply in the tty.
/// Termwiz decodes keys but treats these replies as Alt+[ followed by text.
#[derive(Debug, Default)]
struct TerminalReplyFilter {
    state: ReplyState,
}

#[derive(Debug, Default)]
enum ReplyState {
    #[default]
    Ground,
    Esc,
    Csi,
    Private(Vec<u8>),
}

impl TerminalReplyFilter {
    fn feed(&mut self, bytes: &[u8], flush: bool) -> Vec<u8> {
        let mut output = Vec::with_capacity(bytes.len());
        for &byte in bytes {
            let state = std::mem::take(&mut self.state);
            self.state = match state {
                ReplyState::Ground if byte == b'\x1b' => ReplyState::Esc,
                ReplyState::Ground => {
                    output.push(byte);
                    ReplyState::Ground
                }
                ReplyState::Esc if byte == b'[' => ReplyState::Csi,
                ReplyState::Esc => {
                    output.push(b'\x1b');
                    if byte == b'\x1b' {
                        ReplyState::Esc
                    } else {
                        output.push(byte);
                        ReplyState::Ground
                    }
                }
                ReplyState::Csi if byte == b'?' => ReplyState::Private(Vec::new()),
                ReplyState::Csi => {
                    output.extend_from_slice(b"\x1b[");
                    if byte == b'\x1b' {
                        ReplyState::Esc
                    } else {
                        output.push(byte);
                        ReplyState::Ground
                    }
                }
                ReplyState::Private(_) if byte == b'\x1b' => ReplyState::Esc,
                ReplyState::Private(mut body) => {
                    body.push(byte);
                    if (0x40..=0x7e).contains(&byte) || body.len() >= 128 {
                        if !matches!(body.last(), Some(b'u' | b'c'))
                            || !body[..body.len() - 1]
                                .iter()
                                .all(|value| value.is_ascii_digit() || *value == b';')
                        {
                            output.extend_from_slice(b"\x1b[?");
                            output.extend_from_slice(&body);
                        }
                        ReplyState::Ground
                    } else {
                        ReplyState::Private(body)
                    }
                }
            };
        }
        if flush {
            match std::mem::take(&mut self.state) {
                ReplyState::Esc => output.push(b'\x1b'),
                ReplyState::Csi => output.extend_from_slice(b"\x1b["),
                ReplyState::Private(body) => {
                    output.extend_from_slice(b"\x1b[?");
                    output.extend_from_slice(&body);
                }
                ReplyState::Ground => {}
            }
        }
        output
    }
}

impl Drop for InputStream {
    fn drop(&mut self) {
        let _ = self.suspend();
    }
}

fn read_chunks(file: File, sender: mpsc::Sender<io::Result<Vec<u8>>>, stop: &AtomicBool) {
    let mut buffer = [0u8; 8192];
    while !stop.load(Ordering::Acquire) {
        match rustix::io::read(&file, &mut buffer) {
            Ok(0) => break,
            Ok(size) => {
                if !send_chunk(&sender, Ok(buffer[..size].to_vec()), stop) {
                    return;
                }
            }
            Err(error) if error == rustix::io::Errno::AGAIN => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => {
                let _ = send_chunk(&sender, Err(error.into()), stop);
                return;
            }
        }
    }
}

fn send_chunk(
    sender: &mpsc::Sender<io::Result<Vec<u8>>>,
    mut item: io::Result<Vec<u8>>,
    stop: &AtomicBool,
) -> bool {
    loop {
        match sender.try_send(item) {
            Ok(()) => return true,
            Err(mpsc::error::TrySendError::Closed(_)) => return false,
            Err(mpsc::error::TrySendError::Full(unsent)) => item = unsent,
        }
        if stop.load(Ordering::Acquire) {
            return false;
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn decode_code(code: term_input::KeyCode) -> KeyCode {
    match code {
        term_input::KeyCode::Backspace => KeyCode::Backspace,
        term_input::KeyCode::Enter => KeyCode::Enter,
        term_input::KeyCode::LeftArrow => KeyCode::Left,
        term_input::KeyCode::RightArrow => KeyCode::Right,
        term_input::KeyCode::UpArrow => KeyCode::Up,
        term_input::KeyCode::DownArrow => KeyCode::Down,
        term_input::KeyCode::Home => KeyCode::Home,
        term_input::KeyCode::End => KeyCode::End,
        term_input::KeyCode::PageUp => KeyCode::PageUp,
        term_input::KeyCode::PageDown => KeyCode::PageDown,
        term_input::KeyCode::Tab => KeyCode::Tab,
        term_input::KeyCode::Delete => KeyCode::Delete,
        term_input::KeyCode::Insert => KeyCode::Insert,
        term_input::KeyCode::Function(number) => KeyCode::F(number),
        term_input::KeyCode::Char('\r' | '\n') => KeyCode::Enter,
        term_input::KeyCode::Char('\t') => KeyCode::Tab,
        term_input::KeyCode::Char(ch) => KeyCode::Char(ch),
        term_input::KeyCode::Escape => KeyCode::Esc,
        _ => KeyCode::Other,
    }
}

fn decode_modifiers(modifiers: term_input::Modifiers) -> Modifiers {
    let mut decoded = Modifiers::NONE;
    if modifiers.contains(term_input::Modifiers::SHIFT) {
        decoded |= Modifiers::SHIFT;
    }
    if modifiers.contains(term_input::Modifiers::CTRL) {
        decoded |= Modifiers::CONTROL;
    }
    if modifiers.contains(term_input::Modifiers::ALT) {
        decoded |= Modifiers::ALT;
    }
    decoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn application_key_event_has_no_crossterm_state() {
        let key = KeyEvent::new(KeyCode::Char('a'), Modifiers::CONTROL | Modifiers::SHIFT);
        assert_eq!(key.code, KeyCode::Char('a'));
        assert!(!key.modifiers.is_empty());
    }

    #[test]
    fn decoding_preserves_shift_enter_as_a_typed_event() {
        let decoded = InputStream::decode(term_input::InputEvent::Key(term_input::KeyEvent {
            key: term_input::KeyCode::Enter,
            modifiers: term_input::Modifiers::SHIFT,
        }));
        assert_eq!(
            decoded,
            Some(InputEvent::Key(KeyEvent::new(
                KeyCode::Enter,
                Modifiers::SHIFT
            )))
        );
    }

    #[test]
    fn decoding_keeps_paste_as_one_semantic_event() {
        let decoded = InputStream::decode(term_input::InputEvent::Paste("one\ntwo".to_owned()));
        assert_eq!(decoded, Some(InputEvent::Paste("one\ntwo".to_owned())));
    }

    #[test]
    fn split_kitty_alt_enter_is_one_key() {
        let mut parser = InputParser::new();
        let first = parser.parse_as_vec(b"\x1b", true);
        assert!(first.is_empty());
        let rest = parser.parse_as_vec(b"[13;3u", true);
        assert_eq!(rest.len(), 1);
        assert_eq!(
            InputStream::decode(rest[0].clone()),
            Some(InputEvent::Key(KeyEvent::new(
                KeyCode::Enter,
                Modifiers::ALT
            )))
        );
    }

    #[test]
    fn enabled_mouse_and_private_replies_do_not_become_draft_text() {
        let mut parser = InputParser::new();
        let mut replies = TerminalReplyFilter::default();
        for sequence in [
            b"\x1b[<64;3;4M".as_slice(),
            b"\x1b[200~pasted\x1b[201~",
            b"\x1b[?1u",
        ] {
            let bytes = replies.feed(sequence, true);
            let events = parser.parse_as_vec(&bytes, false);
            assert!(
                !events.iter().any(|event| matches!(
                    event,
                    term_input::InputEvent::Key(term_input::KeyEvent {
                        key: term_input::KeyCode::Char(_),
                        ..
                    })
                )),
                "sequence {sequence:?} leaked as text: {events:?}"
            );
        }
    }

    #[test]
    fn split_private_reply_is_consumed_without_losing_following_key() {
        let mut replies = TerminalReplyFilter::default();
        assert!(replies.feed(b"\x1b[?1;", false).is_empty());
        assert_eq!(replies.feed(b"2cA", false), b"A");
    }
}
