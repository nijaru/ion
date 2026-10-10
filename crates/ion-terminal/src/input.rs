use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Read};
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

mod error;
mod paste;
mod query;
pub use error::InputError;
use paste::{Frame as InputFrame, PasteFramer};
use query::TerminalReplyFilter;

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
    Rejected(InputError),
    Mouse(MouseEvent),
    Resize(Size),
}

#[derive(Debug)]
struct ReaderReturn {
    file: File,
    unsent: Option<io::Result<Vec<u8>>>,
}

/// The single terminal input reader for a live frontend.
#[derive(Debug)]
pub(crate) struct InputStream {
    reader: Option<JoinHandle<ReaderReturn>>,
    file: Option<File>,
    unsent: Option<io::Result<Vec<u8>>>,
    stop: Arc<AtomicBool>,
    chunks: mpsc::Receiver<io::Result<Vec<u8>>>,
    resize: Signal,
    parser: InputParser,
    utf8: Vec<u8>,
    paste: PasteFramer,
    replies: TerminalReplyFilter,
    pending: VecDeque<InputEvent>,
    escape_deadline: Option<tokio::time::Instant>,
    eof: bool,
    escape_grace: Duration,
}

impl InputStream {
    pub(crate) fn new() -> io::Result<Self> {
        let resize = signal(SignalKind::window_change())?;
        // A separate open file description keeps O_NONBLOCK off stdout and
        // the caller's stdin.
        let stdin = File::open("/dev/tty")?;
        let original_flags = fcntl_getfl(stdin.as_fd())?;
        fcntl_setfl(stdin.as_fd(), original_flags | OFlags::NONBLOCK)?;
        let (sender, chunks) = mpsc::channel(32);
        let stop = Arc::new(AtomicBool::new(false));
        let reader_stop = Arc::clone(&stop);
        let reader = thread::Builder::new()
            .name("ion-terminal-input".into())
            .spawn(move || {
                let unsent = read_chunks(&stdin, sender, &reader_stop);
                ReaderReturn {
                    file: stdin,
                    unsent,
                }
            })?;
        let remote =
            std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some();
        Ok(Self {
            reader: Some(reader),
            file: None,
            unsent: None,
            stop,
            chunks,
            resize,
            parser: InputParser::new(),
            utf8: Vec::new(),
            paste: PasteFramer::default(),
            replies: TerminalReplyFilter::default(),
            pending: VecDeque::new(),
            escape_deadline: None,
            eof: false,
            escape_grace: Duration::from_millis(if remote { 100 } else { 10 }),
        })
    }

    /// Join the reader before releasing or quarantining input custody.
    pub(crate) fn suspend(&mut self) -> io::Result<()> {
        self.stop.store(true, Ordering::Release);
        if let Some(reader) = self.reader.take() {
            let returned = reader
                .join()
                .map_err(|_| io::Error::other("terminal reader panicked"))?;
            self.file = Some(returned.file);
            self.unsent = returned.unsent;
        }
        Ok(())
    }

    pub(crate) fn resume(&mut self) -> io::Result<()> {
        let Some(file) = self.file.take() else {
            return Ok(());
        };
        while let Ok(chunk) = self.chunks.try_recv() {
            self.parse(&chunk?, true);
        }
        if let Some(chunk) = self.unsent.take() {
            self.parse(&chunk?, true);
        }
        let (sender, chunks) = mpsc::channel(32);
        self.stop.store(false, Ordering::Release);
        let stop = Arc::clone(&self.stop);
        self.reader = Some(
            thread::Builder::new()
                .name("ion-terminal-input".into())
                .spawn(move || {
                    let unsent = read_chunks(&file, sender, &stop);
                    ReaderReturn { file, unsent }
                })?,
        );
        self.chunks = chunks;
        Ok(())
    }

    pub(crate) fn discard_for_credentials(&mut self) -> io::Result<()> {
        debug_assert!(self.reader.is_none());
        self.pending.clear();
        self.parser = InputParser::new();
        self.utf8.clear();
        self.paste.quarantine();
        self.replies = TerminalReplyFilter::default();
        self.escape_deadline = None;
        let mut error = None;
        while let Ok(chunk) = self.chunks.try_recv() {
            match chunk {
                Ok(bytes) => self.discard_secret_chunk(&bytes),
                Err(failure) => {
                    error.get_or_insert(failure);
                }
            }
        }
        if let Some(chunk) = self.unsent.take() {
            match chunk {
                Ok(bytes) => self.discard_secret_chunk(&bytes),
                Err(failure) => {
                    error.get_or_insert(failure);
                }
            }
        }
        error.map_or(Ok(()), Err)
    }

    /// Quarantine both decoded/read-ahead input and bytes still in the kernel.
    /// Keep the terminal raw throughout; secret entry must never enable echo.
    pub(crate) fn quarantine(&mut self) -> io::Result<()> {
        self.suspend()?;
        self.discard_for_credentials()?;
        // Drain a bounded snapshot rather than flushing blindly: a queued
        // closing marker must reach the framer, never erase its discard boundary.
        let mut remaining = rustix::io::ioctl_fionread(
            self.file
                .as_ref()
                .ok_or_else(|| io::Error::other("terminal input custody is unavailable"))?,
        )?;
        let mut buffer = [0u8; 8192];
        while remaining > 0 {
            let count = usize::try_from(remaining.min(buffer.len() as u64))
                .map_err(|_| io::Error::other("terminal input size overflow"))?;
            let mut file = self
                .file
                .as_ref()
                .ok_or_else(|| io::Error::other("terminal input custody is unavailable"))?;
            match file.read(&mut buffer[..count]) {
                Ok(0) => break,
                Ok(count) => {
                    remaining = remaining.saturating_sub(count as u64);
                    self.discard_secret_chunk(&buffer[..count]);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error),
            }
        }
        self.resume()
    }

    pub(crate) fn secret_paste_is_open(&self) -> bool {
        self.paste.is_open()
    }

    /// Settle a rejected streamed paste before a caller negotiates replies or
    /// resumes its composer. No payload or suffix becomes a decoded event.
    pub(crate) async fn settle_secret_paste(&mut self) -> io::Result<()> {
        self.paste.quarantine();
        while self.paste.is_open() {
            let chunk = self.chunks.recv().await.ok_or_else(|| {
                io::Error::new(io::ErrorKind::UnexpectedEof, "secret paste input ended")
            })??;
            self.discard_secret_chunk(&chunk);
        }
        Ok(())
    }

    fn discard_secret_chunk(&mut self, bytes: &[u8]) {
        let _discarded = self.paste.feed(bytes);
        self.paste.quarantine();
    }

    pub(crate) async fn keyboard_support(&mut self) -> io::Result<Option<bool>> {
        self.replies.keyboard = None;
        self.await_reply(|replies| replies.keyboard.take(), false)
            .await
    }

    pub(crate) async fn cursor_position(&mut self) -> io::Result<(u16, u16)> {
        self.replies.cursor = None;
        let result = self
            .await_reply(|replies| replies.cursor.take(), true)
            .await;
        result?.ok_or_else(|| {
            io::Error::new(io::ErrorKind::TimedOut, "terminal cursor query timed out")
        })
    }

    async fn await_reply<T>(
        &mut self,
        reply: impl Fn(&mut TerminalReplyFilter) -> Option<T>,
        cursor_query: bool,
    ) -> io::Result<Option<T>> {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                self.parse(&[], false);
                return Ok(None);
            }
            if let Some(value) = reply(&mut self.replies) {
                return Ok(Some(value));
            }
            if self.escape_deadline.is_some_and(|escape| now >= escape) {
                self.parse_for_query(&[], false, cursor_query);
                continue;
            }
            let escape_deadline = self.escape_deadline;
            let chunk = tokio::select! {
                chunk = self.chunks.recv() => chunk,
                () = tokio::time::sleep_until(deadline) => continue,
                () = wait_escape(escape_deadline) => continue,
            };
            match chunk {
                Some(Ok(bytes)) => {
                    if tokio::time::Instant::now() >= deadline {
                        // Receive has transferred custody; preserve late type-ahead
                        // through ordinary intake, never as an accepted query reply.
                        self.parse(&bytes, true);
                        self.parse(&[], false);
                        return Ok(None);
                    }
                    self.parse_for_query(&bytes, true, cursor_query);
                }
                Some(Err(error)) => return Err(error),
                None => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "terminal input ended during query",
                    ));
                }
            }
        }
    }

    /// Read the next decoded event, preserving stream termination and I/O
    /// errors for the owning runtime to handle explicitly.
    pub(crate) async fn next(&mut self) -> Option<io::Result<InputEvent>> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Some(Ok(event));
            }
            if self.eof {
                return None;
            }
            let escape_deadline = self.escape_deadline;
            tokio::select! {
                result = self.chunks.recv() => match result {
                    None => {
                        self.eof = true;
                        self.parse(&[], false);
                        if !self.utf8.is_empty() {
                            self.utf8.clear();
                            self.pending.push_back(InputEvent::Rejected(InputError::InvalidUtf8));
                        }
                        if let Some(error) = self.paste.finish() {
                            self.pending.push_back(InputEvent::Rejected(error));
                        }
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
                () = wait_escape(escape_deadline) => self.parse(&[], false),
            }
        }
    }

    fn parse(&mut self, bytes: &[u8], maybe_more: bool) {
        self.parse_for_query(bytes, maybe_more, false);
    }

    fn parse_for_query(&mut self, bytes: &[u8], maybe_more: bool, cursor_query: bool) {
        for frame in self.paste.feed(bytes) {
            match frame {
                InputFrame::Bytes(bytes) => self.parse_keys(&bytes, maybe_more, cursor_query),
                InputFrame::Paste(text) => {
                    self.parse_keys(&[], false, cursor_query);
                    self.pending.push_back(InputEvent::Paste(text));
                }
                InputFrame::Rejected(error) => {
                    self.parse_keys(&[], false, cursor_query);
                    self.pending.push_back(InputEvent::Rejected(error));
                }
            }
        }
        if !maybe_more {
            let prefix = self.paste.flush_prefix();
            self.parse_keys(&prefix, false, cursor_query);
        }
        self.escape_deadline = maybe_more.then(|| tokio::time::Instant::now() + self.escape_grace);
    }

    fn parse_keys(&mut self, bytes: &[u8], maybe_more: bool, cursor_query: bool) {
        for frame in self.replies.feed(bytes, !maybe_more, cursor_query) {
            match frame {
                query::KeyFrame::Bytes(bytes) => self.parse_key_bytes(&bytes, maybe_more),
                query::KeyFrame::Reply => self.parse_key_bytes(&[], false),
            }
        }
        if !maybe_more {
            self.parse_key_bytes(&[], false);
        }
    }

    fn parse_key_bytes(&mut self, bytes: &[u8], maybe_more: bool) {
        let mut joined = std::mem::take(&mut self.utf8);
        let mut remaining = if joined.is_empty() {
            bytes
        } else {
            joined.extend_from_slice(bytes);
            joined.as_slice()
        };
        while !remaining.is_empty() {
            match std::str::from_utf8(remaining) {
                Ok(_) => {
                    self.decode_key_bytes(remaining, maybe_more);
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    self.decode_key_bytes(&remaining[..valid], maybe_more);
                    if let Some(invalid) = error.error_len() {
                        self.decode_key_bytes(&[], false);
                        self.pending
                            .push_back(InputEvent::Rejected(InputError::InvalidUtf8));
                        remaining = &remaining[valid + invalid..];
                    } else {
                        self.utf8.extend_from_slice(&remaining[valid..]);
                        break;
                    }
                }
            }
        }
        if !maybe_more {
            self.decode_key_bytes(&[], false);
        }
    }

    fn decode_key_bytes(&mut self, bytes: &[u8], maybe_more: bool) {
        self.parser.parse(
            bytes,
            |event| {
                if let Some(decoded) = Self::decode(event) {
                    self.pending.push_back(decoded);
                }
            },
            maybe_more,
        );
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
            // Paste openers never reach this decoder: the bounded framer owns
            // them, and removed replies cannot bridge key sequence boundaries.
            term_input::InputEvent::Paste(_) => None,
            term_input::InputEvent::Mouse(mouse) => Some(InputEvent::Mouse(MouseEvent(mouse))),
            term_input::InputEvent::Resized { cols, rows } => Some(InputEvent::Resize(Size {
                columns: cols.try_into().unwrap_or(u16::MAX),
                rows: rows.try_into().unwrap_or(u16::MAX),
            })),
            term_input::InputEvent::PixelMouse(_) | term_input::InputEvent::Wake => None,
        }
    }
}

async fn wait_escape(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

impl Drop for InputStream {
    fn drop(&mut self) {
        let _ = self.suspend();
    }
}

fn read_chunks(
    file: &File,
    sender: mpsc::Sender<io::Result<Vec<u8>>>,
    stop: &AtomicBool,
) -> Option<io::Result<Vec<u8>>> {
    let mut buffer = [0u8; 8192];
    while !stop.load(Ordering::Acquire) {
        match rustix::io::read(file, &mut buffer) {
            Ok(0) => break,
            Ok(size) => {
                if let Err(unsent) = send_chunk(&sender, Ok(buffer[..size].to_vec()), stop) {
                    return Some(unsent);
                }
            }
            Err(error) if error == rustix::io::Errno::AGAIN => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => {
                return send_chunk(&sender, Err(error.into()), stop).err();
            }
        }
    }
    None
}

fn send_chunk(
    sender: &mpsc::Sender<io::Result<Vec<u8>>>,
    mut item: io::Result<Vec<u8>>,
    stop: &AtomicBool,
) -> Result<(), io::Result<Vec<u8>>> {
    loop {
        match sender.try_send(item) {
            Ok(()) => return Ok(()),
            Err(mpsc::error::TrySendError::Closed(unsent)) => return Err(unsent),
            Err(mpsc::error::TrySendError::Full(unsent)) => item = unsent,
        }
        if stop.load(Ordering::Acquire) {
            return Err(item);
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

    fn stream(chunks: mpsc::Receiver<io::Result<Vec<u8>>>) -> InputStream {
        InputStream {
            reader: None,
            file: None,
            unsent: None,
            stop: Arc::new(AtomicBool::new(false)),
            chunks,
            resize: signal(SignalKind::window_change()).unwrap(),
            parser: InputParser::new(),
            utf8: Vec::new(),
            paste: PasteFramer::default(),
            replies: TerminalReplyFilter::default(),
            pending: VecDeque::new(),
            escape_deadline: None,
            eof: false,
            escape_grace: Duration::from_millis(100),
        }
    }

    #[tokio::test]
    async fn queries_preserve_typeahead_and_cancel_without_filtering_keys() {
        let (sender, chunks) = mpsc::channel(4);
        let mut input = stream(chunks);
        sender.send(Ok(b"early\x1b[?1u".to_vec())).await.unwrap();
        assert_eq!(input.keyboard_support().await.unwrap(), Some(true));
        sender.send(Ok(b"later\x1b[2;3R".to_vec())).await.unwrap();
        assert_eq!(input.cursor_position().await.unwrap(), (2, 1));
        assert_eq!(input.pending.len(), "earlylater".len());
        input.pending.clear();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), input.cursor_position())
                .await
                .is_err()
        );
        sender.send(Ok(b"\x1b[1;2R".to_vec())).await.unwrap();
        assert_eq!(
            input.next().await.unwrap().unwrap(),
            InputEvent::Key(KeyEvent::new(KeyCode::F(3), Modifiers::SHIFT))
        );
        assert_eq!(input.keyboard_support().await.unwrap(), None);
    }

    #[tokio::test]
    async fn late_ready_reply_cannot_extend_the_absolute_query_deadline() {
        use std::future::Future;
        use std::task::{Context, Poll, Waker};

        let (sender, chunks) = mpsc::channel(4);
        let mut input = stream(chunks);
        let mut query = Box::pin(input.keyboard_support());
        let mut context = Context::from_waker(Waker::noop());
        assert!(query.as_mut().poll(&mut context).is_pending());
        // Deliberately stop polling, as a synchronous frontend operation can.
        std::thread::sleep(Duration::from_millis(550));
        sender.try_send(Ok(b"late\x1b[?1u".to_vec())).unwrap();
        assert!(matches!(
            query.as_mut().poll(&mut context),
            Poll::Ready(Ok(None))
        ));
        drop(query);
        assert_eq!(
            input.next().await.unwrap().unwrap(),
            InputEvent::Key(KeyEvent::new(KeyCode::Char('l'), Modifiers::NONE))
        );
    }

    #[tokio::test]
    async fn delayed_paste_marker_and_unterminated_input_never_become_keys() {
        let (sender, chunks) = mpsc::channel(4);
        let mut input = stream(chunks);
        sender.send(Ok(b"\x1b[200".to_vec())).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(150), input.next())
                .await
                .is_err()
        );
        sender.send(Ok(b"~body\r\x1b[201~".to_vec())).await.unwrap();
        assert_eq!(
            input.next().await.unwrap().unwrap(),
            InputEvent::Paste("body\r".into())
        );
        sender
            .send(Ok(b"\x1b[200~unfinished".to_vec()))
            .await
            .unwrap();
        drop(sender);
        assert_eq!(
            input.next().await.unwrap().unwrap(),
            InputEvent::Rejected(InputError::IncompletePaste)
        );
        assert!(input.next().await.is_none());
    }

    #[tokio::test]
    async fn paste_is_bounded_validated_and_literal_before_reply_parsing() {
        let (_sender, chunks) = mpsc::channel(4);
        let mut input = stream(chunks);
        for bytes in [
            b"\x1b[20".as_slice(),
            b"0~a\x1b[?1u",
            b"\xe9\x81",
            b"\x93\x1b[201",
            b"~z",
        ] {
            input.parse(bytes, true);
        }
        assert_eq!(
            input.pending.pop_front(),
            Some(InputEvent::Paste("a\x1b[?1u道".into()))
        );
        assert_eq!(
            input.pending.pop_front(),
            Some(InputEvent::Key(KeyEvent::new(
                KeyCode::Char('z'),
                Modifiers::NONE
            )))
        );
        // A removed reply must not join two key fragments into a new paste
        // opener downstream of the bounded framer.
        input.parse(b"\x1b[\x1b[?1u200~\xff\x1b[201~", false);
        assert!(
            input
                .pending
                .iter()
                .all(|event| !matches!(event, InputEvent::Paste(_)))
        );
        assert!(
            input
                .pending
                .contains(&InputEvent::Rejected(InputError::InvalidUtf8))
        );
        input.parse(b"\x1b[20\x1b[?1u0~\xff\x1b[201~", false);
        assert!(
            input
                .pending
                .iter()
                .all(|event| !matches!(event, InputEvent::Paste(_)))
        );
        input.pending.clear();
        input.parse(b"\x1b[200~\xff\x1b[201~", false);
        assert_eq!(
            input.pending.pop_front(),
            Some(InputEvent::Rejected(InputError::InvalidPasteUtf8))
        );
        input.parse(b"\x1b[200~", true);
        input.parse(&vec![b'x'; paste::MAX_PASTE_BYTES + 1], true);
        input.parse(b"more discarded\x1b[201~k", false);
        assert_eq!(
            input.pending.pop_front(),
            Some(InputEvent::Rejected(InputError::PasteTooLarge))
        );
        assert_eq!(
            input.pending.pop_front(),
            Some(InputEvent::Key(KeyEvent::new(
                KeyCode::Char('k'),
                Modifiers::NONE
            )))
        );
        assert!(input.pending.is_empty());
        input.parse(b"\xf0\x9f", true);
        input.parse(&[], false);
        input.parse(b"\xa6\x80", false);
        assert_eq!(
            input.pending.pop_front(),
            Some(InputEvent::Key(KeyEvent::new(
                KeyCode::Char('🦀'),
                Modifiers::NONE
            )))
        );
    }

    #[tokio::test]
    async fn credential_handoff_quarantines_all_pre_read_input() {
        let (sender, chunks) = mpsc::channel(4);
        let mut input = stream(chunks);
        input.parse(b"/login provider\rDISPOSABLE_KEY\r", true);
        sender.send(Ok(b"MORE_PRE_READ".to_vec())).await.unwrap();
        input.unsent = Some(Ok(b"UNSENT".to_vec()));
        input.discard_for_credentials().unwrap();
        assert!(input.pending.is_empty());
        assert!(input.chunks.try_recv().is_err());
        assert!(input.unsent.is_none());
        input.parse(b"\x1b[200~unfinished", true);
        input.discard_for_credentials().unwrap();
        input.parse(b"DELAYED_SECRET\r", false);
        assert!(input.pending.is_empty());
        input.parse(b"\x1b[201~a", false);
        assert_eq!(
            input.pending.pop_front(),
            Some(InputEvent::Key(KeyEvent::new(
                KeyCode::Char('a'),
                Modifiers::NONE
            )))
        );
    }

    #[tokio::test]
    async fn suspension_returns_the_already_read_backpressured_chunk() {
        let (sender, mut chunks) = mpsc::channel(1);
        sender.try_send(Ok(b"queued".to_vec())).unwrap();
        let unsent = send_chunk(&sender, Ok(b"retained".to_vec()), &AtomicBool::new(true))
            .unwrap_err()
            .unwrap();
        assert_eq!(chunks.try_recv().unwrap().unwrap(), b"queued");
        assert_eq!(unsent, b"retained");
    }

    #[tokio::test]
    async fn escape_grace_survives_cancelled_reads_without_breaking_split_keys() {
        let (sender, chunks) = mpsc::channel(4);
        let mut stream = stream(chunks);
        sender.send(Ok(b"\x1b".to_vec())).await.unwrap();
        let mut refresh = tokio::time::interval(Duration::from_millis(30));
        let event = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                tokio::select! {
                    event = stream.next() => break event.unwrap().unwrap(),
                    _ = refresh.tick() => {}
                }
            }
        })
        .await
        .expect("redraw cancellation must not renew Escape grace");
        assert_eq!(
            event,
            InputEvent::Key(KeyEvent::new(KeyCode::Esc, Modifiers::NONE))
        );

        sender.send(Ok(b"\x1b".to_vec())).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), stream.next())
                .await
                .is_err()
        );
        sender.send(Ok(b"[13;3u".to_vec())).await.unwrap();
        let event = stream.next().await.unwrap().unwrap();
        assert_eq!(
            event,
            InputEvent::Key(KeyEvent::new(KeyCode::Enter, Modifiers::ALT))
        );
    }

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
        for sequence in [b"\x1b[<64;3;4M".as_slice(), b"\x1b[?1u"] {
            let mut events = Vec::new();
            for frame in replies.feed(sequence, true, false) {
                match frame {
                    query::KeyFrame::Bytes(bytes) => {
                        events.extend(parser.parse_as_vec(&bytes, false))
                    }
                    query::KeyFrame::Reply => events.extend(parser.parse_as_vec(&[], false)),
                }
            }
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
        assert!(replies.feed(b"\x1b[?1;", false, false).is_empty());
        assert!(
            matches!(replies.feed(b"2cA", false, false).as_slice(), [query::KeyFrame::Reply, query::KeyFrame::Bytes(bytes)] if bytes == b"A")
        );
        replies.feed(b"\x1b[?1;2c\x1b[?1u\x1b[?1;2c", true, false);
        assert_eq!(replies.keyboard, Some(true));
    }
}
