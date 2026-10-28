//! Application keys over decoded terminal events; never secret or raw-program input.
use std::path::Path;

use anyhow::{Context, Result, bail};
use ion_terminal::{KeyCode, KeyEvent, Modifiers};
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};

mod chord;
use chord::Chord;

pub(crate) const COMPOSER: u8 = 1;
pub(crate) const COMPLETION: u8 = 2;
pub(crate) const PICKER: u8 = 4;
pub(crate) const CHOICE: u8 = 8;
pub(crate) const TEXT: u8 = 16;
pub(crate) const DETAILS: u8 = 32;
const GLOBAL: u8 = COMPOSER | COMPLETION | PICKER | CHOICE | TEXT | DETAILS;
const EDIT: u8 = COMPOSER | TEXT;
const SELECT: u8 = COMPLETION | PICKER | CHOICE | TEXT;

macro_rules! actions {
    ($($action:ident, $id:literal, $contexts:expr, [$($key:literal),*], $description:literal;)*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub(crate) enum KeyAction { $($action),* }
        const DEFINITIONS: &[Definition] = &[$(Definition {
            action: KeyAction::$action, id: $id, contexts: $contexts,
            defaults: &[$($key),*], description: $description,
        }),*];
    };
}

actions! {
    Interrupt, "app.interrupt", GLOBAL, ["ctrl+c"], "Stop busy work; otherwise clear/exit composer or close picker";
    Suspend, "app.suspend", GLOBAL, ["ctrl+z"], "Suspend the terminal job";
    Exit, "app.exit", EDIT, ["ctrl+d"], "Delete forward; exit an empty composer";
    Model, "app.model.select", COMPOSER, ["ctrl+l"], "Choose Session model";
    External, "app.editor.external", EDIT, ["ctrl+g"], "Open external editor";
    Paste, "app.clipboard.pasteImage", COMPOSER, ["ctrl+v"], "Paste clipboard text or image";
    Copy, "app.message.copy", COMPOSER | DETAILS, ["ctrl+x"], "Copy answer or selected tool evidence";
    Detail, "app.tools.expand", COMPOSER | DETAILS, ["ctrl+o"], "Open/close transcript details";
    FollowUp, "app.message.followUp", COMPOSER, ["alt+enter"], "Queue a follow-up";
    Dequeue, "app.message.dequeue", COMPOSER, ["alt+up"], "Recover queued or returned input";
    Scope, "app.session.toggleScope", PICKER, ["tab"], "Toggle launch Session folder scope";
    NamedSessions, "app.session.toggleNamedFilter", PICKER, ["ctrl+n"], "Toggle named Sessions only";
    Submit, "tui.input.submit", EDIT, ["enter"], "Submit composer or custom answer";
    NewLine, "tui.input.newLine", EDIT, ["shift+enter", "ctrl+enter", "ctrl+j"], "Insert newline";
    Tab, "tui.input.tab", EDIT | COMPLETION, ["tab"], "Complete a path/command; otherwise insert tab";
    Confirm, "tui.select.confirm", COMPLETION | PICKER | CHOICE, ["enter"], "Accept the visible selected item";
    Cancel, "tui.select.cancel", COMPLETION | PICKER | CHOICE | TEXT | DETAILS, ["escape"], "Dismiss completion, picker, question or details";
    SelectUp, "tui.select.up", SELECT, ["up"], "Move up in completion or modal input";
    SelectDown, "tui.select.down", SELECT, ["down"], "Move down in completion or modal input";
    Toggle, "tui.select.toggle", PICKER | CHOICE, ["space"], "Toggle a setting or multi-select option";
    QuestionCollapse, "tui.question.toggleCollapsed", CHOICE | TEXT, ["ctrl+]"], "Hide/reopen the pending question without releasing input";
    QuestionNext, "tui.question.next", CHOICE, ["tab", "right"], "Next question or batch review";
    QuestionPrevious, "tui.question.previous", CHOICE, ["shift+tab", "left"], "Previous question or batch review";
    CursorUp, "tui.editor.cursorUp", COMPOSER, ["up"], "Move up; browse history at first line";
    CursorDown, "tui.editor.cursorDown", COMPOSER, ["down"], "Move down; browse history at last line";
    Left, "tui.editor.cursorLeft", EDIT, ["left", "ctrl+b"], "Move left by grapheme";
    Right, "tui.editor.cursorRight", EDIT, ["right", "ctrl+f"], "Move right by grapheme";
    WordLeft, "tui.editor.cursorWordLeft", EDIT, ["ctrl+left", "alt+left", "alt+b"], "Move left by word";
    WordRight, "tui.editor.cursorWordRight", EDIT, ["ctrl+right", "alt+right", "alt+f"], "Move right by word";
    LineStart, "tui.editor.cursorLineStart", EDIT, ["home", "ctrl+a"], "Move to line start";
    LineEnd, "tui.editor.cursorLineEnd", EDIT, ["end", "ctrl+e"], "Move to line end";
    Backspace, "tui.editor.deleteCharBackward", EDIT | PICKER, ["backspace"], "Delete previous grapheme (or picker query character)";
    Delete, "tui.editor.deleteCharForward", EDIT, ["delete"], "Delete next grapheme";
    KillWordBackward, "tui.editor.deleteWordBackward", EDIT, ["ctrl+w", "alt+backspace"], "Kill previous word";
    KillWordForward, "tui.editor.deleteWordForward", EDIT, ["alt+d", "alt+delete"], "Kill next word";
    KillLineBackward, "tui.editor.deleteToLineStart", EDIT, ["ctrl+u"], "Kill to line start";
    KillLineForward, "tui.editor.deleteToLineEnd", EDIT, ["ctrl+k"], "Kill to line end";
    Undo, "tui.editor.undo", COMPOSER, ["ctrl+-"], "Undo composer edit";
    Redo, "tui.editor.redo", COMPOSER, ["ctrl+alt+-"], "Redo composer edit";
    Yank, "tui.editor.yank", COMPOSER, ["ctrl+y"], "Yank last kill";
    YankPop, "tui.editor.yankPop", COMPOSER, ["alt+y"], "Cycle directly preceding yank";
    PageUp, "tui.editor.pageUp", COMPOSER | DETAILS | CHOICE, ["pageup"], "Scroll transcript/details or question preview/review up";
    PageDown, "tui.editor.pageDown", COMPOSER | DETAILS | CHOICE, ["pagedown"], "Scroll transcript/details or question preview/review down";
    DetailUp, "app.details.up", DETAILS, ["up"], "Scroll details up one row";
    DetailDown, "app.details.down", DETAILS, ["down"], "Scroll details down one row";
    Earlier, "app.details.previousPage", DETAILS, ["left"], "Show earlier detail page";
    Later, "app.details.nextPage", DETAILS, ["right"], "Show later detail page";
}

struct Definition {
    action: KeyAction,
    id: &'static str,
    contexts: u8,
    defaults: &'static [&'static str],
    description: &'static str,
}

#[derive(Debug, Clone)]
pub(crate) struct KeyBindings {
    keys: Vec<Vec<Chord>>,
}

impl Default for KeyBindings {
    fn default() -> Self {
        Self {
            keys: DEFINITIONS
                .iter()
                .map(|definition| {
                    definition
                        .defaults
                        .iter()
                        .map(|key| Chord::parse(key).expect("valid built-in binding"))
                        .collect()
                })
                .collect(),
        }
    }
}

impl KeyBindings {
    pub(crate) fn load(root: &Path) -> Result<Self> {
        let bytes = match ion_host::file_io::read_bounded(&root.join("keybindings.json"), 64 * 1024)
        {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error).context("read keybindings.json"),
        };
        Self::parse(&bytes)
    }

    pub(crate) fn parse(bytes: &[u8]) -> Result<Self> {
        let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
        let overrides: Overrides = serde_json::from_slice(bytes).map_err(|error| {
            // JSON type errors can contain private values. Report location only.
            anyhow::anyhow!("invalid keybindings.json at line {}, column {}: expected known action IDs and a key string or array of key strings", error.line(), error.column())
        })?;
        let mut result = Self::default();
        for (index, values) in overrides.0 {
            let mut keys = Vec::new();
            for chord in values {
                if !keys.contains(&chord) {
                    keys.push(chord);
                }
            }
            result.keys[index] = keys;
        }
        result.validate()?;
        Ok(result)
    }

    fn validate(&self) -> Result<()> {
        for (index, definition) in DEFINITIONS.iter().enumerate() {
            for (other, second) in DEFINITIONS.iter().enumerate().skip(index + 1) {
                if definition.contexts & second.contexts != 0
                    && self.keys[index]
                        .iter()
                        .any(|key| self.keys[other].contains(key))
                {
                    bail!(
                        "conflicting keybindings for {} and {} in the same input context; remove the shared key from one action",
                        definition.id,
                        second.id
                    );
                }
            }
        }
        Ok(())
    }

    pub(crate) fn action(&self, context: u8, event: KeyEvent) -> Option<KeyAction> {
        let event = Chord::from_event(event);
        DEFINITIONS
            .iter()
            .zip(&self.keys)
            .find_map(|(definition, keys)| {
                (definition.contexts & context != 0 && keys.contains(&event))
                    .then_some(definition.action)
            })
    }

    pub(crate) fn matches(&self, action: KeyAction, event: KeyEvent) -> bool {
        self.keys[action as usize].contains(&Chord::from_event(event))
    }

    pub(crate) fn hint(&self, action: KeyAction) -> String {
        self.keys[action as usize]
            .first()
            .map_or_else(|| "unbound".into(), ToString::to_string)
    }

    pub(crate) fn label(&self, action: KeyAction) -> String {
        if self.keys[action as usize].is_empty() {
            "unbound".into()
        } else {
            self.keys[action as usize]
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" / ")
        }
    }

    pub(crate) fn help(&self) -> String {
        let mut output = String::from(
            "Keys · user keybindings.json · /reload applies changes\nMissing actions keep defaults; a string/array replaces them; [] disables.\nKeys may repeat across focused contexts, not within one context.\nSecret entry and attached raw programs retain their physical controls.\n",
        );
        for definition in DEFINITIONS {
            output.push_str(&format!(
                "\n{} · {}\n  {}",
                definition.id,
                self.label(definition.action),
                definition.description
            ));
        }
        output
    }
}

struct ConfiguredKeys(Vec<Chord>);

impl<'de> Deserialize<'de> for ConfiguredKeys {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct KeysVisitor;
        impl<'de> Visitor<'de> for KeysVisitor {
            type Value = ConfiguredKeys;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a key string or array of key strings")
            }
            fn visit_str<E: de::Error>(self, value: &str) -> std::result::Result<Self::Value, E> {
                Chord::parse(value)
                    .map(|key| ConfiguredKeys(vec![key]))
                    .map_err(de::Error::custom)
            }
            fn visit_seq<S: SeqAccess<'de>>(
                self,
                mut sequence: S,
            ) -> std::result::Result<Self::Value, S::Error> {
                let mut keys = Vec::new();
                while let Some(key) = sequence.next_element()? {
                    keys.push(key);
                }
                Ok(ConfiguredKeys(keys))
            }
        }
        deserializer.deserialize_any(KeysVisitor)
    }
}

struct Overrides(Vec<(usize, Vec<Chord>)>);
impl<'de> Deserialize<'de> for Overrides {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct BindingsVisitor;
        impl<'de> Visitor<'de> for BindingsVisitor {
            type Value = Overrides;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("keybinding action object")
            }
            fn visit_map<M: MapAccess<'de>>(
                self,
                mut map: M,
            ) -> std::result::Result<Self::Value, M::Error> {
                let mut entries = Vec::new();
                while let Some(id) = map.next_key::<String>()? {
                    let index = DEFINITIONS
                        .iter()
                        .position(|definition| definition.id == id)
                        .ok_or_else(|| de::Error::custom("unknown action"))?;
                    if entries.iter().any(|(found, _)| *found == index) {
                        return Err(de::Error::custom("duplicate action"));
                    }
                    let ConfiguredKeys(values) = map.next_value()?;
                    entries.push((index, values));
                }
                Ok(Overrides(entries))
            }
        }
        deserializer.deserialize_map(BindingsVisitor)
    }
}

pub(crate) fn printable(key: KeyEvent) -> Option<char> {
    match key.code {
        KeyCode::Char(ch)
            if !ch.is_control()
                && !key.modifiers.contains(Modifiers::CONTROL)
                && !key.modifiers.contains(Modifiers::ALT) =>
        {
            Some(ch)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replacements_disabling_and_focused_aliases_are_unambiguous() {
        KeyBindings::default().validate().unwrap();
        let keys = KeyBindings::parse(br#"{"app.model.select":["alt+m","alt+M"],"tui.editor.undo":[],"tui.select.confirm":"ctrl+g"}"#).unwrap();
        assert_eq!(keys.label(KeyAction::Model), "Alt-M");
        assert_eq!(keys.label(KeyAction::Undo), "unbound");
        let event = KeyEvent::new(KeyCode::Char('g'), Modifiers::CONTROL);
        assert_eq!(keys.action(PICKER, event), Some(KeyAction::Confirm));
        assert_eq!(keys.action(COMPOSER, event), Some(KeyAction::External));
        assert!(!keys.matches(
            KeyAction::Model,
            KeyEvent::new(KeyCode::Char('l'), Modifiers::CONTROL)
        ));
        assert!(
            KeyBindings::parse(br#"{"app.model.select":"ctrl+g"}"#)
                .unwrap_err()
                .to_string()
                .contains("conflicting")
        );
    }
    #[test]
    fn file_loading_is_bounded_and_missing_is_the_only_default_fallback() {
        let root = std::env::temp_dir().join(format!("ion-key-config-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(
            KeyBindings::load(&root).unwrap().label(KeyAction::Model),
            "Ctrl-L"
        );
        let path = root.join("keybindings.json");
        std::fs::write(&path, b"\xef\xbb\xbf{\"app.model.select\":\"alt+m\"}").unwrap();
        assert_eq!(
            KeyBindings::load(&root).unwrap().label(KeyAction::Model),
            "Alt-M"
        );
        std::fs::write(&path, "{\n  \"app.model.select\": true\n}").unwrap();
        let error = KeyBindings::load(&root).err().unwrap().to_string();
        assert!(error.contains("line 2"), "{error}");
        std::fs::write(&path, vec![b' '; 64 * 1024 + 1]).unwrap();
        let error = KeyBindings::load(&root).err().unwrap();
        assert!(format!("{error:#}").contains("65536-byte read bound"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_configuration_does_not_expose_private_values() {
        for bytes in [
            br#"[]"#.as_slice(),
            br#"{"private-secret": "foo"}"#,
            br#"{"app.model.select":42}"#,
            br#"{"app.model.select":"alt+m","app.model.select":[]}"#,
            br#"{"app.model.select":"private-secret"}"#,
        ] {
            let error = KeyBindings::parse(bytes).err().unwrap().to_string();
            assert!(!error.contains("private-secret"));
        }
    }
}
