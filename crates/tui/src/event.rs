/// All events the app can receive.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Paste(String),
    Resize(u16, u16),
    FocusGained,
    FocusLost,
    /// Fired at `App::tick_rate()` if set. Used for animations / spinners.
    Tick,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
    pub kind: KeyEventKind,
}

impl KeyEvent {
    pub fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self {
            code,
            modifiers,
            kind: KeyEventKind::Press,
        }
    }

    pub fn plain(code: KeyCode) -> Self {
        Self::new(code, KeyModifiers::NONE)
    }

    pub fn ctrl(c: char) -> Self {
        Self::new(KeyCode::Char(c), KeyModifiers::CTRL)
    }

    pub fn alt(c: char) -> Self {
        Self::new(KeyCode::Char(c), KeyModifiers::ALT)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    Char(char),
    F(u8),
    Backspace,
    Delete,
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
    Insert,
    Esc,
    Null,
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct KeyModifiers: u8 {
        const SHIFT = 0b00000001;
        const CTRL  = 0b00000010;
        const ALT   = 0b00000100;
        const SUPER = 0b00001000;
        const NONE  = 0b00000000;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyEventKind {
    Press,
    Repeat,
    Release,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MouseEvent {
    pub kind: MouseEventKind,
    pub column: u16,
    pub row: u16,
    pub modifiers: KeyModifiers,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MouseEventKind {
    Down(MouseButton),
    Up(MouseButton),
    Drag(MouseButton),
    Moved,
    ScrollDown,
    ScrollUp,
    ScrollLeft,
    ScrollRight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// Translate a crossterm event to our event type. Returns `None` for unknown
/// or unsupported crossterm event variants (media keys, etc.).
pub(crate) fn translate_event(ev: crossterm::event::Event) -> Option<Event> {
    use crossterm::event as ct;
    match ev {
        ct::Event::Key(k) => translate_key(k).map(Event::Key),
        ct::Event::Mouse(m) => Some(Event::Mouse(translate_mouse(m))),
        ct::Event::Paste(s) => Some(Event::Paste(s)),
        ct::Event::Resize(w, h) => Some(Event::Resize(w, h)),
        ct::Event::FocusGained => Some(Event::FocusGained),
        ct::Event::FocusLost => Some(Event::FocusLost),
    }
}

fn translate_key(k: crossterm::event::KeyEvent) -> Option<KeyEvent> {
    let code = translate_keycode(k.code)?;
    let modifiers = translate_modifiers(k.modifiers);
    let kind = match k.kind {
        crossterm::event::KeyEventKind::Press => KeyEventKind::Press,
        crossterm::event::KeyEventKind::Repeat => KeyEventKind::Repeat,
        crossterm::event::KeyEventKind::Release => KeyEventKind::Release,
    };
    Some(KeyEvent {
        code,
        modifiers,
        kind,
    })
}

fn translate_keycode(code: crossterm::event::KeyCode) -> Option<KeyCode> {
    use crossterm::event::KeyCode as Ct;
    match code {
        Ct::Char(c) => Some(KeyCode::Char(c)),
        Ct::F(n) => Some(KeyCode::F(n)),
        Ct::Backspace => Some(KeyCode::Backspace),
        Ct::Delete => Some(KeyCode::Delete),
        Ct::Enter => Some(KeyCode::Enter),
        Ct::Left => Some(KeyCode::Left),
        Ct::Right => Some(KeyCode::Right),
        Ct::Up => Some(KeyCode::Up),
        Ct::Down => Some(KeyCode::Down),
        Ct::Home => Some(KeyCode::Home),
        Ct::End => Some(KeyCode::End),
        Ct::PageUp => Some(KeyCode::PageUp),
        Ct::PageDown => Some(KeyCode::PageDown),
        Ct::Tab => Some(KeyCode::Tab),
        Ct::BackTab => Some(KeyCode::BackTab),
        Ct::Insert => Some(KeyCode::Insert),
        Ct::Esc => Some(KeyCode::Esc),
        Ct::Null => Some(KeyCode::Null),
        // Unknown keys (media, modifier-only, etc.) — ignore.
        _ => None,
    }
}

fn translate_modifiers(mods: crossterm::event::KeyModifiers) -> KeyModifiers {
    use crossterm::event::KeyModifiers as Ct;
    let mut m = KeyModifiers::NONE;
    if mods.contains(Ct::SHIFT) {
        m |= KeyModifiers::SHIFT;
    }
    if mods.contains(Ct::CONTROL) {
        m |= KeyModifiers::CTRL;
    }
    if mods.contains(Ct::ALT) {
        m |= KeyModifiers::ALT;
    }
    if mods.contains(Ct::SUPER) {
        m |= KeyModifiers::SUPER;
    }
    m
}

fn translate_mouse(m: crossterm::event::MouseEvent) -> MouseEvent {
    use crossterm::event::MouseEventKind as CtKind;
    let kind = match m.kind {
        CtKind::Down(b) => MouseEventKind::Down(translate_button(b)),
        CtKind::Up(b) => MouseEventKind::Up(translate_button(b)),
        CtKind::Drag(b) => MouseEventKind::Drag(translate_button(b)),
        CtKind::Moved => MouseEventKind::Moved,
        CtKind::ScrollDown => MouseEventKind::ScrollDown,
        CtKind::ScrollUp => MouseEventKind::ScrollUp,
        CtKind::ScrollLeft => MouseEventKind::ScrollLeft,
        CtKind::ScrollRight => MouseEventKind::ScrollRight,
    };
    MouseEvent {
        kind,
        column: m.column,
        row: m.row,
        modifiers: translate_modifiers(m.modifiers),
    }
}

fn translate_button(b: crossterm::event::MouseButton) -> MouseButton {
    match b {
        crossterm::event::MouseButton::Left => MouseButton::Left,
        crossterm::event::MouseButton::Right => MouseButton::Right,
        crossterm::event::MouseButton::Middle => MouseButton::Middle,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_event_ctrl() {
        let k = KeyEvent::ctrl('c');
        assert_eq!(k.code, KeyCode::Char('c'));
        assert!(k.modifiers.contains(KeyModifiers::CTRL));
        assert_eq!(k.kind, KeyEventKind::Press);
    }

    #[test]
    fn key_event_plain() {
        let k = KeyEvent::plain(KeyCode::Enter);
        assert_eq!(k.code, KeyCode::Enter);
        assert!(k.modifiers.is_empty());
    }

    #[test]
    fn modifier_flags_combine() {
        let m = KeyModifiers::CTRL | KeyModifiers::SHIFT;
        assert!(m.contains(KeyModifiers::CTRL));
        assert!(m.contains(KeyModifiers::SHIFT));
        assert!(!m.contains(KeyModifiers::ALT));
    }
}
