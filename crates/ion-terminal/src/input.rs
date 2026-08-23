use std::io;

use crossterm::event::{self, Event, EventStream};
use futures_util::StreamExt;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FocusEvent {
    pub gained: bool,
}

/// Mouse input is typed at the boundary even though inline Ion does not yet
/// claim mouse ownership. The raw event stays private to this crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MouseEvent(event::MouseEvent);

/// Responses from terminal queries, reserved for negotiated input/output
/// features that are not represented by Crossterm's high-level events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalResponse {
    Unknown(Vec<u8>),
}

/// All terminal-originated input consumed by a frontend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEvent {
    Key(KeyEvent),
    Paste(String),
    Mouse(MouseEvent),
    Focus(FocusEvent),
    Resize(Size),
    TerminalResponse(TerminalResponse),
    Closed,
}

/// The single terminal input reader for a live frontend.
#[derive(Debug)]
pub struct InputStream {
    stream: EventStream,
}

impl InputStream {
    pub(crate) fn new() -> Self {
        Self {
            stream: EventStream::new(),
        }
    }

    /// Read the next decoded event, preserving stream termination and I/O
    /// errors for the owning runtime to handle explicitly.
    pub async fn next(&mut self) -> Option<io::Result<InputEvent>> {
        self.stream
            .next()
            .await
            .map(|result| result.map(Self::decode))
    }

    fn decode(event: Event) -> InputEvent {
        match event {
            Event::Key(key) => InputEvent::Key(KeyEvent {
                code: decode_code(key.code),
                modifiers: decode_modifiers(key.modifiers),
            }),
            Event::Paste(text) => InputEvent::Paste(text),
            Event::Mouse(mouse) => InputEvent::Mouse(MouseEvent(mouse)),
            Event::FocusGained => InputEvent::Focus(FocusEvent { gained: true }),
            Event::FocusLost => InputEvent::Focus(FocusEvent { gained: false }),
            Event::Resize(columns, rows) => InputEvent::Resize(Size { columns, rows }),
        }
    }
}

fn decode_code(code: event::KeyCode) -> KeyCode {
    match code {
        event::KeyCode::Backspace => KeyCode::Backspace,
        event::KeyCode::Enter => KeyCode::Enter,
        event::KeyCode::Left => KeyCode::Left,
        event::KeyCode::Right => KeyCode::Right,
        event::KeyCode::Up => KeyCode::Up,
        event::KeyCode::Down => KeyCode::Down,
        event::KeyCode::Home => KeyCode::Home,
        event::KeyCode::End => KeyCode::End,
        event::KeyCode::Tab => KeyCode::Tab,
        event::KeyCode::BackTab => KeyCode::BackTab,
        event::KeyCode::Delete => KeyCode::Delete,
        event::KeyCode::Insert => KeyCode::Insert,
        event::KeyCode::F(number) => KeyCode::F(number),
        event::KeyCode::Char(ch) => KeyCode::Char(ch),
        event::KeyCode::Esc => KeyCode::Esc,
        _ => KeyCode::Other,
    }
}

fn decode_modifiers(modifiers: event::KeyModifiers) -> Modifiers {
    let mut decoded = Modifiers::NONE;
    if modifiers.contains(event::KeyModifiers::SHIFT) {
        decoded |= Modifiers::SHIFT;
    }
    if modifiers.contains(event::KeyModifiers::CONTROL) {
        decoded |= Modifiers::CONTROL;
    }
    if modifiers.contains(event::KeyModifiers::ALT) {
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
        let decoded = InputStream::decode(Event::Key(event::KeyEvent::new(
            event::KeyCode::Enter,
            event::KeyModifiers::SHIFT,
        )));
        assert_eq!(
            decoded,
            InputEvent::Key(KeyEvent::new(KeyCode::Enter, Modifiers::SHIFT))
        );
    }

    #[test]
    fn decoding_keeps_paste_as_one_semantic_event() {
        let decoded = InputStream::decode(Event::Paste("one\ntwo".to_owned()));
        assert_eq!(decoded, InputEvent::Paste("one\ntwo".to_owned()));
    }
}
