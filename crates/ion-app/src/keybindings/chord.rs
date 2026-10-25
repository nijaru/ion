use anyhow::{Result, bail};
use ion_terminal::{KeyCode, KeyEvent, Modifiers};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Chord(KeyEvent);

impl Chord {
    pub(super) fn parse(value: &str) -> Result<Self> {
        let lower = value.to_ascii_lowercase();
        let mut remaining = lower.as_str();
        let mut modifiers = Modifiers::NONE;
        while remaining != "+" {
            let Some((prefix, rest)) = remaining.split_once('+') else {
                break;
            };
            let modifier = match prefix {
                "ctrl" => Modifiers::CONTROL,
                "alt" => Modifiers::ALT,
                "shift" => Modifiers::SHIFT,
                _ => bail!("expected Ctrl, Alt or Shift modifier"),
            };
            if modifiers.contains(modifier) {
                bail!("repeated key modifier");
            }
            modifiers |= modifier;
            remaining = rest;
        }
        let code = match remaining {
            "escape" | "esc" => KeyCode::Esc,
            "enter" | "return" => KeyCode::Enter,
            "tab" => KeyCode::Tab,
            "backtab" => {
                modifiers |= Modifiers::SHIFT;
                KeyCode::Tab
            }
            "space" => KeyCode::Char(' '),
            "backspace" => KeyCode::Backspace,
            "delete" => KeyCode::Delete,
            "insert" => KeyCode::Insert,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "pageup" => KeyCode::PageUp,
            "pagedown" => KeyCode::PageDown,
            key if key.starts_with('f') && key[1..].parse::<u8>().is_ok_and(|n| n > 0) => {
                KeyCode::F(key[1..].parse().expect("validated function key"))
            }
            key => {
                let mut chars = key.chars();
                let Some(ch) = chars.next() else {
                    bail!("missing key");
                };
                if chars.next().is_some() || ch.is_control() || ch.is_whitespace() {
                    bail!("expected one character or a named terminal key");
                }
                KeyCode::Char(ch)
            }
        };
        Ok(Self::normalize(KeyEvent::new(code, modifiers), false))
    }

    pub(super) fn from_event(event: KeyEvent) -> Self {
        Self::normalize(event, true)
    }

    fn normalize(mut event: KeyEvent, infer_shift: bool) -> Self {
        if event.code == KeyCode::BackTab {
            event.code = KeyCode::Tab;
            event.modifiers |= Modifiers::SHIFT;
        }
        if let KeyCode::Char(ch) = event.code {
            if ch == '\x1f' {
                event.code = KeyCode::Char('-');
                event.modifiers |= Modifiers::CONTROL;
            } else if ch.is_ascii_uppercase() {
                event.code = KeyCode::Char(ch.to_ascii_lowercase());
                if infer_shift && !event.modifiers.contains(Modifiers::CONTROL) {
                    event.modifiers |= Modifiers::SHIFT;
                }
            }
            // Legacy Ctrl-minus/underscore is one byte, including the shifted
            // spellings delivered by CSI-u. Keep one configurable undo chord.
            if matches!(event.code, KeyCode::Char('-' | '_'))
                && event.modifiers.contains(Modifiers::CONTROL)
            {
                event.code = KeyCode::Char('-');
                event.modifiers = event.modifiers.without(Modifiers::SHIFT);
            } else {
                const UNSHIFTED: &str = "`1234567890-=[]\\;',./";
                const SHIFTED: &str = "~!@#$%^&*()_+{}|:\"<>?";
                if let KeyCode::Char(ch) = event.code
                    && event.modifiers.contains(Modifiers::SHIFT)
                {
                    if let Some(index) = UNSHIFTED.chars().position(|base| base == ch) {
                        event.code = KeyCode::Char(
                            SHIFTED
                                .chars()
                                .nth(index)
                                .expect("paired ASCII shift symbols"),
                        );
                        event.modifiers = event.modifiers.without(Modifiers::SHIFT);
                    } else if SHIFTED.contains(ch) {
                        event.modifiers = event.modifiers.without(Modifiers::SHIFT);
                    }
                }
            }
        }
        Self(event)
    }
}

impl<'de> serde::Deserialize<'de> for Chord {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let text = <String as serde::Deserialize>::deserialize(deserializer)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (modifier, label) in [
            (Modifiers::CONTROL, "Ctrl-"),
            (Modifiers::ALT, "Alt-"),
            (Modifiers::SHIFT, "Shift-"),
        ] {
            if self.0.modifiers.contains(modifier) {
                formatter.write_str(label)?;
            }
        }
        let name = match self.0.code {
            KeyCode::Char(' ') => "Space",
            KeyCode::Char(ch) => return write!(formatter, "{}", ch.to_ascii_uppercase()),
            KeyCode::F(number) => return write!(formatter, "F{number}"),
            KeyCode::Esc => "Esc",
            KeyCode::Enter => "Enter",
            KeyCode::Tab => "Tab",
            KeyCode::Backspace => "Backspace",
            KeyCode::Delete => "Delete",
            KeyCode::Insert => "Insert",
            KeyCode::Home => "Home",
            KeyCode::End => "End",
            KeyCode::Up => "Up",
            KeyCode::Down => "Down",
            KeyCode::Left => "Left",
            KeyCode::Right => "Right",
            KeyCode::PageUp => "PageUp",
            KeyCode::PageDown => "PageDown",
            KeyCode::BackTab | KeyCode::Other => unreachable!("not a configured chord"),
        };
        formatter.write_str(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aliases_retain_legacy_undo_and_extended_shift_semantics() {
        for (text, event) in [
            (
                "ctrl+-",
                KeyEvent::new(KeyCode::Char('\x1f'), Modifiers::NONE),
            ),
            (
                "ctrl+alt+-",
                KeyEvent::new(
                    KeyCode::Char('_'),
                    Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT,
                ),
            ),
            (
                "shift+tab",
                KeyEvent::new(KeyCode::BackTab, Modifiers::SHIFT),
            ),
            ("!", KeyEvent::new(KeyCode::Char('1'), Modifiers::SHIFT)),
            (
                "shift+a",
                KeyEvent::new(KeyCode::Char('A'), Modifiers::NONE),
            ),
        ] {
            assert_eq!(Chord::parse(text).unwrap(), Chord::from_event(event));
        }
        assert_ne!(
            Chord::parse("a").unwrap(),
            Chord::from_event(KeyEvent::new(KeyCode::Char('A'), Modifiers::NONE))
        );
        for bad in ["super+x", "ctrl+ctrl+x", "ctrl+", "unknown", "\n"] {
            assert!(Chord::parse(bad).is_err());
        }
        assert_eq!(
            Chord::parse("ctrl++").unwrap(),
            Chord::parse("ctrl+shift+=").unwrap()
        );
    }
}
