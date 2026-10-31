//! Terminal-local recovery for unsent draft text.
//!
//! Undo/redo stores owned text deltas with their before/after byte cursors;
//! the kill ring stores killed text only. Both are presentation state and never
//! admit, submit or duplicate attached images. Submission handoff discards
//! undo/redo, while the kill ring deliberately survives draft changes so killed
//! text can be yanked into another draft.
use std::{collections::VecDeque, ops::Range};

use crate::keybindings::{self, COMPOSER, KeyAction};
use ion_terminal::KeyEvent;
#[cfg(test)]
use ion_terminal::{KeyCode, Modifiers};

use super::{
    MAX_DRAFT,
    editing::Kill,
    editor::{Editor, Limit},
};

const MAX_HISTORY_ENTRIES: usize = 100;
const MAX_HISTORY_BYTES: usize = 1024 * 1024;
const MAX_KILL_ENTRIES: usize = 32;
const MAX_KILL_BYTES: usize = 256 * 1024;

#[derive(Default)]
pub(super) struct EditorRecovery {
    undo: VecDeque<Delta>,
    redo: VecDeque<Delta>,
    retained: usize,
    group: Group,
    yank: Option<Yank>,
    kills: KillRing,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Group {
    #[default]
    Other,
    TypedWord,
    Kill,
}

struct Delta {
    start: usize,
    removed: String,
    inserted: String,
    cursor_before: usize,
    cursor_after: usize,
}

impl Delta {
    fn retained(&self) -> usize {
        self.removed.len() + self.inserted.len()
    }
}

#[derive(Clone)]
struct Yank {
    range: Range<usize>,
    cursor: usize,
}

#[derive(Default)]
struct KillRing {
    entries: VecDeque<String>,
    retained: usize,
}

impl KillRing {
    fn push(&mut self, text: &str, direction: Kill, accumulate: bool) {
        if text.is_empty() {
            return;
        }
        // Consecutive kills without an insertion remove at most the current
        // MAX_DRAFT-sized draft, so accumulation cannot exceed that bound.
        let merged = accumulate
            && self
                .entries
                .back()
                .is_some_and(|entry| entry.len() + text.len() <= MAX_DRAFT);
        if merged {
            let entry = self.entries.back_mut().expect("merged kill has an owner");
            if direction == Kill::Backward {
                entry.insert_str(0, text);
            } else {
                entry.push_str(text);
            }
            self.retained += text.len();
        } else {
            self.entries.push_back(text.to_owned());
            self.retained += text.len();
        }
        while self.entries.len() > MAX_KILL_ENTRIES || self.retained > MAX_KILL_BYTES {
            let Some(entry) = self.entries.pop_front() else {
                break;
            };
            self.retained -= entry.len();
        }
    }

    fn newest(&self) -> Option<&str> {
        self.entries.back().map(String::as_str)
    }

    fn second_newest(&self) -> Option<&str> {
        self.entries
            .len()
            .checked_sub(2)
            .and_then(|index| self.entries.get(index))
            .map(String::as_str)
    }

    fn rotate(&mut self) {
        if let Some(entry) = self.entries.pop_back() {
            self.entries.push_front(entry);
        }
    }
}

impl EditorRecovery {
    fn reset(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.retained = 0;
        self.interrupt();
    }

    fn interrupt(&mut self) {
        self.group = Group::Other;
        self.yank = None;
    }

    fn break_group(&mut self) {
        self.group = Group::Other;
    }

    fn clear_yank(&mut self) {
        self.yank = None;
    }

    fn record(&mut self, delta: Delta, group: Group) {
        for dropped in self.redo.drain(..) {
            self.retained -= dropped.retained();
        }
        self.retained += delta.retained();
        self.undo.push_back(delta);
        self.group = group;
        self.yank = None;
        while self.undo.len() + self.redo.len() > MAX_HISTORY_ENTRIES
            || self.retained > MAX_HISTORY_BYTES
        {
            let Some(dropped) = self.undo.pop_front() else {
                break;
            };
            self.retained -= dropped.retained();
        }
    }

    fn extend_typed(&mut self, start: usize, text: &str, cursor_after: usize) {
        let entry = self
            .undo
            .back_mut()
            .expect("a typed continuation has an undo entry");
        assert!(
            entry.removed.is_empty(),
            "a typed continuation must extend an insertion"
        );
        assert_eq!(
            entry.start + entry.inserted.len(),
            start,
            "a typed continuation must append at its recorded draft end"
        );
        entry.inserted.push_str(text);
        entry.cursor_after = cursor_after;
        self.retained += text.len();
        self.group = Group::TypedWord;
        self.yank = None;
        while self.undo.len() + self.redo.len() > MAX_HISTORY_ENTRIES
            || self.retained > MAX_HISTORY_BYTES
        {
            let Some(dropped) = self.undo.pop_front() else {
                break;
            };
            self.retained -= dropped.retained();
        }
    }
}

fn sanitize(text: &str) -> String {
    text.chars()
        .filter_map(|ch| match ch {
            '\n' | '\t' => Some(ch),
            ch if ch.is_control() => None,
            ch => Some(ch),
        })
        .collect()
}

impl Editor {
    /// Keyboard/tool focus changed: later typing and kills start new groups.
    pub(super) fn break_edit_focus(&mut self) {
        self.error = None;
        self.recovery.interrupt();
        self.preferred_column = None;
    }

    /// A different draft or attachment baseline owns the next edits.
    pub(super) fn reset_edit_baseline(&mut self) {
        self.error = None;
        self.preferred_column = None;
        self.recovery.reset();
    }

    /// Consume the key-level grouping rules before a normal composer action.
    pub(super) fn prepare_edit_key(
        &mut self,
        key: KeyEvent,
        keys: &crate::keybindings::KeyBindings,
    ) {
        self.error = None;
        let action = keys.action(COMPOSER, key);
        if !matches!(action, Some(KeyAction::CursorUp | KeyAction::CursorDown)) {
            self.preferred_column = None;
        }
        if action != Some(KeyAction::YankPop) {
            self.recovery.clear_yank();
        }
        let typed = action.is_none() && keybindings::printable(key).is_some();
        if !typed && action.and_then(super::editing::kill).is_none() {
            self.recovery.break_group();
        }
    }

    pub(super) fn undo_key(
        &mut self,
        key: KeyEvent,
        keys: &crate::keybindings::KeyBindings,
    ) -> bool {
        let redo = match keys.action(COMPOSER, key) {
            Some(KeyAction::Undo) => false,
            Some(KeyAction::Redo) => true,
            _ => return false,
        };
        self.browsing = None;
        self.completion.clear();
        if redo {
            self.redo();
        } else {
            self.undo();
        }
        true
    }

    pub(super) fn yank_keys(
        &mut self,
        key: KeyEvent,
        keys: &crate::keybindings::KeyBindings,
    ) -> bool {
        if keys.matches(KeyAction::YankPop, key) {
            self.yank_pop();
            true
        } else if keys.matches(KeyAction::Yank, key) {
            self.yank();
            true
        } else {
            false
        }
    }

    fn insert_clean(&mut self, text: &str, group: Group, typed: bool) {
        self.error = None;
        self.preferred_column = None;
        self.completion.edited(self.cursor);
        let clean = sanitize(text);
        if clean.is_empty() {
            self.recovery.interrupt();
            return;
        }
        if self.draft.len().saturating_add(clean.len()) > MAX_DRAFT {
            self.error = Some(Limit::Prompt);
            return;
        }
        let start = self.cursor;
        let cursor_before = self.cursor;
        let coalesce = typed
            && group == Group::TypedWord
            && self.recovery.group == Group::TypedWord
            && !clean.chars().any(char::is_whitespace);
        self.browsing = None;
        self.draft.insert_str(start, &clean);
        self.cursor += clean.len();
        if coalesce {
            self.recovery.extend_typed(start, &clean, self.cursor);
        } else {
            self.recovery.record(
                Delta {
                    start,
                    removed: String::new(),
                    inserted: clean,
                    cursor_before,
                    cursor_after: self.cursor,
                },
                group,
            );
        }
    }

    /// One typed character keeps Pi's whitespace-coalescing boundaries.
    pub(super) fn insert_typed(&mut self, text: &str) {
        self.insert_clean(text, Group::TypedWord, true);
    }

    /// Paste, newline, completion opener and other programmatic insertion are
    /// one atomic edit even when they contain several characters.
    pub(super) fn insert(&mut self, text: &str) {
        self.insert_clean(text, Group::Other, false);
    }

    /// Replace one already-owned composer range, such as completion or a path.
    pub(super) fn replace_text(&mut self, range: Range<usize>, text: &str, cursor: usize) -> bool {
        self.error = None;
        self.preferred_column = None;
        // Completion and editor callers derive this range from the current
        // draft. An invalid internal range is a writer bug, not malformed input.
        let removed = self.draft[range.clone()].to_owned();
        let next_len = self.draft.len() - removed.len() + text.len();
        if next_len > MAX_DRAFT {
            self.error = Some(Limit::Prompt);
            return false;
        }
        let cursor_before = self.cursor;
        let changed = removed != text;
        self.browsing = None;
        self.completion.clear();
        if changed {
            self.draft.replace_range(range.clone(), text);
            self.recovery.record(
                Delta {
                    start: range.start,
                    removed,
                    inserted: text.to_owned(),
                    cursor_before,
                    cursor_after: cursor,
                },
                Group::Other,
            );
            self.cursor = cursor;
        } else {
            self.cursor = cursor;
            self.recovery.interrupt();
        }
        true
    }

    /// The whole draft from the external editor is one undoable replacement.
    pub(super) fn replace_draft_atomic(&mut self, text: String) {
        let end = self.draft.len();
        self.replace_text(0..end, &text, text.len());
    }

    pub(super) fn delete_text(&mut self, range: Range<usize>, kill: Option<Kill>) {
        self.preferred_column = None;
        // Empty ranges are expected at line and draft boundaries; an inverted
        // range is an internal writer bug and fails at the direct index below.
        if range.start == range.end {
            self.recovery.interrupt();
            return;
        }
        // `editing::apply` derives deletion ranges from the current draft. An
        // invalid internal range would corrupt cursor and recovery state.
        let removed = self.draft[range.clone()].to_owned();
        let cursor_before = self.cursor;
        let was_kill = self.recovery.group == Group::Kill;
        self.browsing = None;
        self.completion.edited(range.start);
        self.draft.replace_range(range.clone(), "");
        self.cursor = range.start;
        if let Some(direction) = kill {
            self.recovery.kills.push(&removed, direction, was_kill);
        }
        self.recovery.record(
            Delta {
                start: range.start,
                removed,
                inserted: String::new(),
                cursor_before,
                cursor_after: self.cursor,
            },
            if kill.is_some() {
                Group::Kill
            } else {
                Group::Other
            },
        );
    }

    pub(super) fn clear_draft_text(&mut self) {
        let end = self.draft.len();
        self.replace_text(0..end, "", 0);
    }

    fn apply_history(&mut self, entry: &Delta, undo: bool) {
        self.preferred_column = None;
        let (expected, replacement, cursor_after) = if undo {
            (&entry.inserted, &entry.removed, entry.cursor_before)
        } else {
            (&entry.removed, &entry.inserted, entry.cursor_after)
        };
        let end = entry.start + expected.len();
        // History entries are recorded against the exact draft they edit. Any
        // baseline replacement resets history, so a mismatch is a writer bug.
        assert!(
            end <= self.draft.len(),
            "an editor history delta must stay within the current draft"
        );
        assert!(
            &self.draft[entry.start..end] == expected.as_str(),
            "an editor history delta must still own its recorded draft text"
        );
        self.browsing = None;
        self.completion.clear();
        self.draft.replace_range(entry.start..end, replacement);
        self.cursor = cursor_after;
    }

    fn undo(&mut self) {
        let Some(entry) = self.recovery.undo.pop_back() else {
            return;
        };
        self.apply_history(&entry, true);
        self.recovery.redo.push_back(entry);
        self.recovery.interrupt();
    }

    fn redo(&mut self) {
        let Some(entry) = self.recovery.redo.pop_back() else {
            return;
        };
        self.apply_history(&entry, false);
        self.recovery.undo.push_back(entry);
        self.recovery.interrupt();
    }

    fn yank(&mut self) {
        self.error = None;
        self.preferred_column = None;
        self.recovery.interrupt();
        let Some(text) = self.recovery.kills.newest().map(str::to_owned) else {
            return;
        };
        if self.draft.len().saturating_add(text.len()) > MAX_DRAFT {
            self.error = Some(Limit::Prompt);
            return;
        }
        let start = self.cursor;
        let cursor_before = self.cursor;
        self.browsing = None;
        self.completion.clear();
        self.draft.insert_str(start, &text);
        self.cursor = start + text.len();
        self.recovery.record(
            Delta {
                start,
                removed: String::new(),
                inserted: text,
                cursor_before,
                cursor_after: self.cursor,
            },
            Group::Other,
        );
        self.recovery.yank = Some(Yank {
            range: start..self.cursor,
            cursor: self.cursor,
        });
    }

    fn yank_pop(&mut self) {
        self.error = None;
        self.preferred_column = None;
        let Some(yank) = self.recovery.yank.clone() else {
            return;
        };
        if self.cursor != yank.cursor {
            self.recovery.clear_yank();
            return;
        }
        let Some(current) = self.recovery.kills.newest().map(str::to_owned) else {
            self.recovery.clear_yank();
            return;
        };
        assert!(
            self.draft.get(yank.range.clone()) == Some(current.as_str()),
            "a yank-pop span must still hold the kill it inserted"
        );
        let Some(next) = self.recovery.kills.second_newest().map(str::to_owned) else {
            return;
        };
        let next_len = self.draft.len() - yank.range.len() + next.len();
        if next_len > MAX_DRAFT {
            self.error = Some(Limit::Prompt);
            return;
        }
        let cursor_before = self.cursor;
        self.browsing = None;
        self.completion.clear();
        self.recovery.kills.rotate();
        self.draft.replace_range(yank.range.clone(), &next);
        self.cursor = yank.range.start + next.len();
        if current != next {
            self.recovery.record(
                Delta {
                    start: yank.range.start,
                    removed: current,
                    inserted: next,
                    cursor_before,
                    cursor_after: self.cursor,
                },
                Group::Other,
            );
        } else {
            self.recovery.interrupt();
        }
        self.recovery.yank = Some(Yank {
            range: yank.range.start..self.cursor,
            cursor: self.cursor,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_client::{Action, EditorDraft, Frontend};

    fn key(code: KeyCode, modifiers: Modifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn composer_key(ui: &mut Frontend, key: KeyEvent) -> bool {
        matches!(ui.key(key, None, None), Action::None)
    }

    #[test]
    fn submission_hands_off_the_literal_cursor_without_undo_or_attachment_loss() {
        let literal = "literal 🦀\nsecond";
        let cursor = "literal 🦀".len();
        for modifiers in [Modifiers::NONE, Modifiers::ALT] {
            let mut ui = Frontend::default();
            ui.insert(literal);
            ui.editor.cursor = cursor;
            ui.attach_image(ion_host::image_input::ImageSource::Path("kept.png".into()));
            let source = match ui.key(key(KeyCode::Enter, modifiers), None, None) {
                Action::Submit(source) if modifiers == Modifiers::NONE => source,
                Action::Queue(source) if modifiers == Modifiers::ALT => source,
                _ => panic!("expected a typed editor handoff"),
            };
            assert_eq!((source.literal.as_str(), source.cursor), (literal, cursor));
            assert!(ui.editor.draft.is_empty());
            assert_eq!(ui.editor.cursor, 0);
            assert_eq!(ui.editor.images.len(), 1);
            assert!(ui.editor.recovery.undo.is_empty());
            ui.restore_editor(source);
            assert!(composer_key(
                &mut ui,
                key(KeyCode::Char('\x1f'), Modifiers::NONE)
            ));
            assert_eq!(
                (ui.editor.draft.as_str(), ui.editor.cursor),
                (literal, cursor)
            );
            assert_eq!(ui.editor.images.len(), 1);
        }
    }

    #[test]
    fn paste_completion_and_editor_replacement_are_atomic_undo_edits() {
        let mut ui = Frontend::default();
        ui.insert("alpha\nβeta");
        ui.editor
            .prepare_edit_key(key(KeyCode::Char('\x1f'), Modifiers::NONE), &ui.keys);
        assert!(
            ui.editor
                .undo_key(key(KeyCode::Char('\x1f'), Modifiers::NONE), &ui.keys)
        );
        assert_eq!(ui.editor.draft, "");

        ui.insert("/he");
        ui.refresh_completion(None, None, false);
        ui.editor.completion.painted_rows(80, 6, &ui.keys);
        ui.editor
            .prepare_edit_key(key(KeyCode::Tab, Modifiers::NONE), &ui.keys);
        assert!(matches!(
            ui.key(key(KeyCode::Tab, Modifiers::NONE), None, None),
            super::super::Action::None
        ));
        assert_eq!(ui.editor.draft, "/help ");
        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('_'), Modifiers::CONTROL | Modifiers::SHIFT)
        ));
        assert_eq!(ui.editor.draft, "/he");

        ui.replace_draft_atomic("edited 🦀".into());
        assert_eq!(
            (ui.editor.draft.as_str(), ui.editor.cursor),
            ("edited 🦀", "edited 🦀".len())
        );
        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('-'), Modifiers::CONTROL)
        ));
        assert_eq!(ui.editor.draft, "/he");
    }

    #[test]
    fn actual_new_edit_invalidates_redo_and_groups_at_word_boundaries() {
        let mut ui = Frontend::default();
        for ch in "one two".chars() {
            ui.editor
                .prepare_edit_key(key(KeyCode::Char(ch), Modifiers::NONE), &ui.keys);
            ui.editor.insert_typed(ch.encode_utf8(&mut [0; 4]));
        }
        assert_eq!(ui.editor.draft, "one two");
        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('_'), Modifiers::CONTROL)
        ));
        assert_eq!(ui.editor.draft, "one");
        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('_'), Modifiers::CONTROL)
        ));
        assert_eq!(ui.editor.draft, "");
        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('_'), Modifiers::ALT | Modifiers::CONTROL)
        ));
        assert_eq!(ui.editor.draft, "one");
        ui.editor
            .prepare_edit_key(key(KeyCode::Char('x'), Modifiers::NONE), &ui.keys);
        ui.editor.insert_typed("x");
        assert_eq!(ui.editor.draft, "onex");
        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('_'), Modifiers::ALT | Modifiers::CONTROL)
        ));
        assert_eq!(
            ui.editor.draft, "onex",
            "a fresh edit must discard the redo path"
        );
    }

    #[test]
    fn empty_enter_preserves_redo_for_both_enter_forms() {
        for modifiers in [Modifiers::NONE, Modifiers::ALT] {
            let mut ui = Frontend::default();
            ui.insert("hello");
            assert!(composer_key(
                &mut ui,
                key(KeyCode::Char('_'), Modifiers::CONTROL)
            ));
            assert_eq!((ui.editor.draft.as_str(), ui.editor.cursor), ("", 0));
            let history = (
                ui.editor.recovery.undo.len(),
                ui.editor.recovery.redo.len(),
                ui.editor.recovery.retained,
            );
            assert!(matches!(
                ui.key(key(KeyCode::Enter, modifiers), None, None),
                Action::None
            ));
            assert_eq!((ui.editor.draft.as_str(), ui.editor.cursor), ("", 0));
            assert_eq!(
                (
                    ui.editor.recovery.undo.len(),
                    ui.editor.recovery.redo.len(),
                    ui.editor.recovery.retained
                ),
                history,
                "an empty Enter is not a submission handoff"
            );
            assert!(composer_key(
                &mut ui,
                key(KeyCode::Char('_'), Modifiers::CONTROL | Modifiers::ALT)
            ));
            assert_eq!(ui.editor.draft, "hello");
        }
    }

    #[test]
    fn empty_shell_rejection_preserves_literal_cursor_and_undo() {
        for (literal, expected) in [
            ("!", "Type a shell command after !"),
            ("!!", "Type a shell command after !!"),
            ("!   ", "Type a shell command after !"),
            ("!!   ", "Type a shell command after !!"),
        ] {
            let mut ui = Frontend::default();
            ui.insert(literal);
            ui.editor.cursor = literal.len() / 2;
            let history = (
                ui.editor.recovery.undo.len(),
                ui.editor.recovery.redo.len(),
                ui.editor.recovery.retained,
            );
            assert!(matches!(
                ui.key(key(KeyCode::Enter, Modifiers::NONE), None, None),
                Action::None
            ));
            assert_eq!(
                (ui.editor.draft.as_str(), ui.editor.cursor),
                (literal, literal.len() / 2)
            );
            assert_eq!(ui.status, expected);
            assert_eq!(
                (
                    ui.editor.recovery.undo.len(),
                    ui.editor.recovery.redo.len(),
                    ui.editor.recovery.retained
                ),
                history
            );
            assert!(composer_key(
                &mut ui,
                key(KeyCode::Char('_'), Modifiers::CONTROL)
            ));
            assert_eq!(ui.editor.draft, "");
        }
    }

    #[test]
    fn empty_insert_breaks_yank_affinity_without_touching_history() {
        let mut ui = Frontend::default();
        for text in ["first", "second"] {
            ui.insert(text);
            ui.editor.cursor = ui.editor.draft.len();
            assert!(composer_key(
                &mut ui,
                key(KeyCode::Char('w'), Modifiers::CONTROL)
            ));
        }
        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('y'), Modifiers::CONTROL)
        ));
        let history = (
            ui.editor.recovery.undo.len(),
            ui.editor.recovery.redo.len(),
            ui.editor.recovery.retained,
        );
        ui.insert("\u{7}");
        assert_eq!(ui.editor.draft, "second");
        assert_eq!(
            (
                ui.editor.recovery.undo.len(),
                ui.editor.recovery.redo.len(),
                ui.editor.recovery.retained
            ),
            history
        );
        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('y'), Modifiers::ALT)
        ));
        assert_eq!(ui.editor.draft, "second");
    }

    #[test]
    fn unicode_kills_accumulate_and_yank_pop_rotates_only_after_success() {
        let mut ui = Frontend::default();
        ui.insert("alpha βeta");
        ui.editor.cursor = ui.editor.draft.len();
        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('w'), Modifiers::CONTROL)
        ));
        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('u'), Modifiers::CONTROL)
        ));
        assert_eq!(ui.editor.draft, "");
        assert_eq!(ui.editor.recovery.kills.newest(), Some("alpha βeta"));

        ui.insert("Ω");
        ui.editor.cursor = 0;
        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('k'), Modifiers::CONTROL)
        ));
        assert_eq!(ui.editor.draft, "");
        assert_eq!(ui.editor.recovery.kills.newest(), Some("Ω"));

        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('y'), Modifiers::CONTROL)
        ));
        assert_eq!(ui.editor.draft, "Ω");
        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('y'), Modifiers::ALT)
        ));
        assert_eq!(ui.editor.draft, "alpha βeta");
        assert_eq!(ui.editor.recovery.kills.newest(), Some("alpha βeta"));
    }

    #[test]
    fn frontend_key_binds_yank_pop_to_alt_y_without_the_old_chord() {
        let mut ui = Frontend::default();
        for text in ["first", "second"] {
            ui.insert(text);
            ui.editor.cursor = ui.editor.draft.len();
            assert!(composer_key(
                &mut ui,
                key(KeyCode::Char('w'), Modifiers::CONTROL)
            ));
        }
        assert!(matches!(
            ui.key(key(KeyCode::Char('y'), Modifiers::CONTROL), None, None),
            Action::None
        ));
        assert_eq!(ui.editor.draft, "second");
        assert!(matches!(
            ui.key(key(KeyCode::Char('y'), Modifiers::ALT), None, None),
            Action::None
        ));
        assert_eq!(ui.editor.draft, "first", "Alt-Y must pop the previous kill");
        assert!(matches!(
            ui.key(
                key(KeyCode::Char('y'), Modifiers::CONTROL | Modifiers::ALT),
                None,
                None
            ),
            Action::None
        ));
        assert_eq!(
            ui.editor.draft, "first",
            "the former Ctrl-Alt-Y chord is not an alias"
        );
    }

    #[test]
    fn rejected_yank_and_pop_preserve_text_history_and_ring_rotation() {
        let mut ui = Frontend::default();
        ui.insert(&"κ".repeat(100));
        ui.editor.cursor = 0;
        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('k'), Modifiers::CONTROL)
        ));
        ui.insert("x");
        ui.editor.cursor = ui.editor.draft.len();
        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('u'), Modifiers::CONTROL)
        ));

        ui.replace_draft_atomic("a".repeat(MAX_DRAFT));
        let before_draft = ui.editor.draft.clone();
        let before_history = (
            ui.editor.recovery.undo.len(),
            ui.editor.recovery.redo.len(),
            ui.editor.recovery.retained,
        );
        let before_ring = ui.editor.recovery.kills.entries.clone();
        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('y'), Modifiers::CONTROL)
        ));
        assert_eq!(
            ui.editor.draft, before_draft,
            "over-limit yank changed the draft"
        );
        assert_eq!(
            (
                ui.editor.recovery.undo.len(),
                ui.editor.recovery.redo.len(),
                ui.editor.recovery.retained
            ),
            before_history
        );
        assert_eq!(ui.editor.recovery.kills.entries, before_ring);

        ui.replace_draft_atomic("b".repeat(MAX_DRAFT - 1));
        ui.editor.cursor = ui.editor.draft.len();
        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('y'), Modifiers::CONTROL)
        ));
        assert_eq!(ui.editor.draft.len(), MAX_DRAFT);
        let before_draft = ui.editor.draft.clone();
        let before_history = (
            ui.editor.recovery.undo.len(),
            ui.editor.recovery.redo.len(),
            ui.editor.recovery.retained,
        );
        let before_ring = ui.editor.recovery.kills.entries.clone();
        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('y'), Modifiers::ALT)
        ));
        assert_eq!(
            ui.editor.draft, before_draft,
            "over-limit yank-pop changed the draft"
        );
        assert_eq!(
            (
                ui.editor.recovery.undo.len(),
                ui.editor.recovery.redo.len(),
                ui.editor.recovery.retained
            ),
            before_history
        );
        assert_eq!(
            ui.editor.recovery.kills.entries, before_ring,
            "rejected yank-pop rotated the ring"
        );
    }

    #[test]
    fn attachment_and_returned_baselines_discard_history_but_keep_ring_and_images() {
        let mut ui = Frontend::default();
        ui.insert("typed");
        ui.editor.cursor = ui.editor.draft.len();
        composer_key(&mut ui, key(KeyCode::Char('w'), Modifiers::CONTROL));
        assert!(ui.editor.recovery.kills.newest() == Some("typed"));
        ui.attach_image(super::super::ImageSource::Path("/tmp/not-read.png".into()));
        assert!(ui.editor.recovery.undo.is_empty());
        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('_'), Modifiers::CONTROL)
        ));
        assert_eq!(ui.editor.draft, "");

        ui.insert("returned");
        ui.restore_editor(EditorDraft::at_end("host returned".into()));
        assert!(ui.editor.recovery.undo.is_empty());
        assert_eq!(ui.editor.draft, "host returned");
        assert_eq!(ui.editor.images.len(), 1);
        ui.editor.cursor = ui.editor.draft.len();
        composer_key(&mut ui, key(KeyCode::Char('y'), Modifiers::CONTROL));
        assert_eq!(ui.editor.draft, "host returnedtyped");
    }

    #[test]
    fn history_and_kill_ring_evict_oldest_entries_at_their_bounds() {
        let mut ui = Frontend::default();
        for _ in 0..=MAX_HISTORY_ENTRIES {
            ui.editor.insert_typed(" ");
        }
        assert_eq!(ui.editor.recovery.undo.len(), MAX_HISTORY_ENTRIES);
        for _ in 0..MAX_HISTORY_ENTRIES {
            composer_key(&mut ui, key(KeyCode::Char('_'), Modifiers::CONTROL));
        }
        assert_eq!(
            ui.editor.draft, " ",
            "eviction must preserve retained edits"
        );

        let mut ui = Frontend::default();
        for index in 0..10 {
            let text = format!("{index:02}{}", "x".repeat(MAX_DRAFT - 2));
            ui.replace_draft_atomic(text);
        }
        assert!(ui.editor.recovery.undo.len() < MAX_HISTORY_ENTRIES);
        let retained = ui
            .editor
            .recovery
            .undo
            .iter()
            .chain(&ui.editor.recovery.redo)
            .map(|delta| delta.removed.len() + delta.inserted.len())
            .sum::<usize>();
        assert!(retained <= MAX_HISTORY_BYTES);
        assert_eq!(retained, ui.editor.recovery.retained);
        let final_text = ui.editor.draft.clone();
        while !ui.editor.recovery.undo.is_empty() {
            assert!(composer_key(
                &mut ui,
                key(KeyCode::Char('_'), Modifiers::CONTROL)
            ));
        }
        while !ui.editor.recovery.redo.is_empty() {
            assert!(composer_key(
                &mut ui,
                key(KeyCode::Char('_'), Modifiers::CONTROL | Modifiers::ALT)
            ));
        }
        assert_eq!(
            (ui.editor.draft.as_str(), ui.editor.cursor),
            (final_text.as_str(), final_text.len()),
            "byte-budget eviction must leave a valid undo/redo chain"
        );

        let mut ring = KillRing::default();
        for index in 0..=MAX_KILL_ENTRIES {
            ring.push(&index.to_string(), Kill::Forward, false);
        }
        assert_eq!(ring.entries.len(), MAX_KILL_ENTRIES);
        assert_eq!(ring.entries.front().map(String::as_str), Some("1"));
        let text = "k".repeat(MAX_DRAFT);
        for _ in 0..5 {
            ring.push(&text, Kill::Forward, false);
        }
        assert_eq!(ring.entries.len(), MAX_KILL_BYTES / MAX_DRAFT);
        assert!(ring.retained <= MAX_KILL_BYTES);
    }

    #[test]
    fn ctrl_c_clear_is_undoable_and_not_a_kill() {
        let mut ui = Frontend::default();
        ui.insert("unsent 🦀");
        assert!(matches!(
            ui.key(key(KeyCode::Char('c'), Modifiers::CONTROL), None, None),
            super::super::Action::None
        ));
        assert_eq!(ui.editor.draft, "");
        assert!(ui.editor.recovery.kills.entries.is_empty());
        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('_'), Modifiers::CONTROL)
        ));
        assert_eq!(ui.editor.draft, "unsent 🦀");
    }

    #[test]
    fn simple_backspace_is_undoable_but_not_a_kill() {
        let mut ui = Frontend::default();
        ui.insert("ab");
        composer_key(&mut ui, key(KeyCode::Backspace, Modifiers::NONE));
        assert_eq!(ui.editor.draft, "a");
        assert!(ui.editor.recovery.kills.entries.is_empty());
        composer_key(&mut ui, key(KeyCode::Char('_'), Modifiers::CONTROL));
        assert_eq!(ui.editor.draft, "ab");
    }
}
