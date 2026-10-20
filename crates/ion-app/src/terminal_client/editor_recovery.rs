//! Terminal-local recovery for unsent draft text.
//!
//! Undo/redo stores owned text deltas with their before/after byte cursors;
//! the kill ring stores killed text only. Both are presentation state: they
//! never admit, submit or duplicate attached images, and submission handoff
//! discards them rather than reviving already-sent input.
use std::{collections::VecDeque, ops::Range};

use ion_terminal::{KeyCode, KeyEvent, Modifiers};

use super::{Frontend, MAX_DRAFT, editing::Kill};

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
        debug_assert!(entry.removed.is_empty());
        debug_assert_eq!(entry.start + entry.inserted.len(), start);
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

/// Undo for either documented Ctrl spelling, redo only with Alt.
fn undo_action(key: KeyEvent) -> Option<bool> {
    let control = key.modifiers.contains(Modifiers::CONTROL);
    let alt = key.modifiers.contains(Modifiers::ALT);
    match key.code {
        // Legacy terminals send one 0x1f byte for Ctrl-minus/Ctrl-underscore.
        KeyCode::Char('\x1f') => Some(alt),
        KeyCode::Char('-' | '_') if control => Some(alt),
        _ => None,
    }
}

fn yank_key(key: KeyEvent) -> bool {
    key.code == KeyCode::Char('y')
        && key.modifiers.contains(Modifiers::CONTROL)
        && !key.modifiers.contains(Modifiers::ALT)
}

fn yank_pop_key(key: KeyEvent) -> bool {
    key.code == KeyCode::Char('y')
        && key.modifiers.contains(Modifiers::CONTROL)
        && key.modifiers.contains(Modifiers::ALT)
}

fn typed_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char(_))
        && !key.modifiers.contains(Modifiers::CONTROL)
        && !key.modifiers.contains(Modifiers::ALT)
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

impl Frontend {
    /// Keyboard/tool focus changed: later typing and kills start new groups.
    pub(super) fn break_edit_focus(&mut self) {
        self.recovery.interrupt();
    }

    /// A different draft or attachment baseline owns the next edits.
    pub(super) fn reset_edit_baseline(&mut self) {
        self.recovery.reset();
    }

    /// Take the current literal text out of recovery before Host handoff.
    pub(super) fn begin_draft_handoff(&mut self) {
        self.recovery.reset();
    }

    /// Consume the key-level grouping rules before a normal composer action.
    pub(super) fn prepare_edit_key(&mut self, key: KeyEvent) {
        if !yank_pop_key(key) {
            self.recovery.clear_yank();
        }
        if !typed_key(key) && super::editing::kill(key).is_none() {
            self.recovery.break_group();
        }
    }

    pub(super) fn undo_key(&mut self, key: KeyEvent) -> bool {
        let Some(redo) = undo_action(key) else {
            return false;
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

    pub(super) fn yank_keys(&mut self, key: KeyEvent) -> bool {
        if yank_pop_key(key) {
            self.yank_pop();
            true
        } else if yank_key(key) {
            self.yank();
            true
        } else {
            false
        }
    }

    fn insert_clean(&mut self, text: &str, group: Group, typed: bool) {
        self.completion.clear();
        let clean = sanitize(text);
        if clean.is_empty() {
            return;
        }
        if self.draft.len().saturating_add(clean.len()) > MAX_DRAFT {
            self.status = format!("Prompt is limited to {MAX_DRAFT} bytes");
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
        self.completion.clear();
        if self.pickers.insert(text) {
            return;
        }
        self.insert_clean(text, Group::Other, false);
    }

    /// Replace one already-owned composer range, such as completion or a path.
    pub(super) fn replace_text(&mut self, range: Range<usize>, text: &str, cursor: usize) -> bool {
        let Some(removed) = self.draft.get(range.clone()).map(str::to_owned) else {
            debug_assert!(false, "editor replacement range is not a draft boundary");
            return false;
        };
        let next_len = self.draft.len() - removed.len() + text.len();
        if next_len > MAX_DRAFT {
            self.status = format!("Prompt is limited to {MAX_DRAFT} bytes");
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
        self.pickers.close();
        let end = self.draft.len();
        self.replace_text(0..end, &text, text.len());
    }

    pub(super) fn delete_text(&mut self, range: Range<usize>, kill: Option<Kill>) {
        if range.start >= range.end {
            self.recovery.interrupt();
            return;
        }
        let Some(removed) = self.draft.get(range.clone()).map(str::to_owned) else {
            debug_assert!(false, "editor deletion range is not a draft boundary");
            return;
        };
        let cursor_before = self.cursor;
        let was_kill = self.recovery.group == Group::Kill;
        self.browsing = None;
        self.completion.clear();
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

    fn apply_history(&mut self, entry: &Delta, undo: bool) -> bool {
        let (expected, replacement, cursor_after) = if undo {
            (&entry.inserted, &entry.removed, entry.cursor_before)
        } else {
            (&entry.removed, &entry.inserted, entry.cursor_after)
        };
        let end = entry.start + expected.len();
        if self.draft.get(entry.start..end) != Some(expected.as_str()) {
            debug_assert!(false, "editor history delta no longer owns the draft");
            return false;
        }
        self.browsing = None;
        self.completion.clear();
        self.draft.replace_range(entry.start..end, replacement);
        self.cursor = cursor_after;
        true
    }

    fn undo(&mut self) {
        let Some(entry) = self.recovery.undo.pop_back() else {
            return;
        };
        if !self.apply_history(&entry, true) {
            self.recovery.undo.push_back(entry);
            return;
        }
        self.recovery.redo.push_back(entry);
        self.recovery.interrupt();
    }

    fn redo(&mut self) {
        let Some(entry) = self.recovery.redo.pop_back() else {
            return;
        };
        if !self.apply_history(&entry, false) {
            self.recovery.redo.push_back(entry);
            return;
        }
        self.recovery.undo.push_back(entry);
        self.recovery.interrupt();
    }

    fn yank(&mut self) {
        self.recovery.interrupt();
        let Some(text) = self.recovery.kills.newest().map(str::to_owned) else {
            return;
        };
        if self.draft.len().saturating_add(text.len()) > MAX_DRAFT {
            self.status = format!("Prompt is limited to {MAX_DRAFT} bytes");
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
        if self.draft.get(yank.range.clone()) != Some(current.as_str()) {
            debug_assert!(false, "yank-pop no longer owns its inserted span");
            self.recovery.clear_yank();
            return;
        }
        let Some(next) = self.recovery.kills.second_newest().map(str::to_owned) else {
            return;
        };
        let next_len = self.draft.len() - yank.range.len() + next.len();
        if next_len > MAX_DRAFT {
            self.status = format!("Prompt is limited to {MAX_DRAFT} bytes");
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
    use crate::terminal_client::{EditorDraft, editing::Edit};

    fn key(code: KeyCode, modifiers: Modifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn composer_key(ui: &mut Frontend, key: KeyEvent) -> bool {
        ui.prepare_edit_key(key);
        if ui.undo_key(key) {
            return true;
        }
        if ui.yank_keys(key) {
            return true;
        }
        if let Some(edit) = super::super::editing::key(&ui.draft, ui.cursor, key) {
            match edit {
                Edit::Move(cursor) => ui.cursor = cursor,
                Edit::Delete { range, kill } => ui.delete_text(range, kill),
            }
            return true;
        }
        false
    }

    #[test]
    fn paste_completion_and_editor_replacement_are_atomic_undo_edits() {
        let mut ui = Frontend::default();
        ui.insert("alpha\nβeta");
        ui.prepare_edit_key(key(KeyCode::Char('\x1f'), Modifiers::NONE));
        assert!(ui.undo_key(key(KeyCode::Char('\x1f'), Modifiers::NONE)));
        assert_eq!(ui.draft, "");

        ui.insert("/he");
        ui.prepare_edit_key(key(KeyCode::Tab, Modifiers::NONE));
        assert!(matches!(
            ui.key(key(KeyCode::Tab, Modifiers::NONE), None),
            super::super::Action::None
        ));
        assert_eq!(ui.draft, "/help ");
        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('_'), Modifiers::CONTROL | Modifiers::SHIFT)
        ));
        assert_eq!(ui.draft, "/he");

        ui.replace_draft_atomic("edited 🦀".into());
        assert_eq!(
            (ui.draft.as_str(), ui.cursor),
            ("edited 🦀", "edited 🦀".len())
        );
        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('-'), Modifiers::CONTROL)
        ));
        assert_eq!(ui.draft, "/he");
    }

    #[test]
    fn actual_new_edit_invalidates_redo_and_groups_at_word_boundaries() {
        let mut ui = Frontend::default();
        for ch in "one two".chars() {
            ui.prepare_edit_key(key(KeyCode::Char(ch), Modifiers::NONE));
            ui.insert_typed(ch.encode_utf8(&mut [0; 4]));
        }
        assert_eq!(ui.draft, "one two");
        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('_'), Modifiers::CONTROL)
        ));
        assert_eq!(ui.draft, "one");
        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('_'), Modifiers::CONTROL)
        ));
        assert_eq!(ui.draft, "");
        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('_'), Modifiers::ALT | Modifiers::CONTROL)
        ));
        assert_eq!(ui.draft, "one");
        ui.prepare_edit_key(key(KeyCode::Char('x'), Modifiers::NONE));
        ui.insert_typed("x");
        assert_eq!(ui.draft, "onex");
        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('_'), Modifiers::ALT | Modifiers::CONTROL)
        ));
        assert_eq!(ui.draft, "onex", "a fresh edit must discard the redo path");
    }

    #[test]
    fn unicode_kills_accumulate_and_yank_pop_rotates_only_after_success() {
        let mut ui = Frontend::default();
        ui.insert("alpha βeta");
        ui.cursor = ui.draft.len();
        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('w'), Modifiers::CONTROL)
        ));
        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('u'), Modifiers::CONTROL)
        ));
        assert_eq!(ui.draft, "");
        assert_eq!(ui.recovery.kills.newest(), Some("alpha βeta"));

        ui.insert("Ω");
        ui.cursor = 0;
        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('k'), Modifiers::CONTROL)
        ));
        assert_eq!(ui.draft, "");
        assert_eq!(ui.recovery.kills.newest(), Some("Ω"));

        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('y'), Modifiers::CONTROL)
        ));
        assert_eq!(ui.draft, "Ω");
        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('y'), Modifiers::ALT | Modifiers::CONTROL)
        ));
        assert_eq!(ui.draft, "alpha βeta");
        assert_eq!(ui.recovery.kills.newest(), Some("alpha βeta"));
    }

    #[test]
    fn rejected_yank_and_pop_preserve_text_history_and_ring_rotation() {
        let mut ui = Frontend::default();
        ui.insert(&"κ".repeat(100));
        ui.cursor = 0;
        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('k'), Modifiers::CONTROL)
        ));
        ui.insert("x");
        ui.cursor = ui.draft.len();
        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('u'), Modifiers::CONTROL)
        ));

        ui.replace_draft_atomic("a".repeat(MAX_DRAFT));
        let before_draft = ui.draft.clone();
        let before_history = (
            ui.recovery.undo.len(),
            ui.recovery.redo.len(),
            ui.recovery.retained,
        );
        let before_ring = ui.recovery.kills.entries.clone();
        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('y'), Modifiers::CONTROL)
        ));
        assert_eq!(ui.draft, before_draft, "over-limit yank changed the draft");
        assert_eq!(
            (
                ui.recovery.undo.len(),
                ui.recovery.redo.len(),
                ui.recovery.retained
            ),
            before_history
        );
        assert_eq!(ui.recovery.kills.entries, before_ring);

        ui.replace_draft_atomic("b".repeat(MAX_DRAFT - 1));
        ui.cursor = ui.draft.len();
        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('y'), Modifiers::CONTROL)
        ));
        assert_eq!(ui.draft.len(), MAX_DRAFT);
        let before_draft = ui.draft.clone();
        let before_history = (
            ui.recovery.undo.len(),
            ui.recovery.redo.len(),
            ui.recovery.retained,
        );
        let before_ring = ui.recovery.kills.entries.clone();
        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('y'), Modifiers::ALT | Modifiers::CONTROL)
        ));
        assert_eq!(
            ui.draft, before_draft,
            "over-limit yank-pop changed the draft"
        );
        assert_eq!(
            (
                ui.recovery.undo.len(),
                ui.recovery.redo.len(),
                ui.recovery.retained
            ),
            before_history
        );
        assert_eq!(
            ui.recovery.kills.entries, before_ring,
            "rejected yank-pop rotated the ring"
        );
    }

    #[test]
    fn attachment_and_returned_baselines_discard_history_but_keep_ring_and_images() {
        let mut ui = Frontend::default();
        ui.insert("typed");
        ui.cursor = ui.draft.len();
        composer_key(&mut ui, key(KeyCode::Char('w'), Modifiers::CONTROL));
        assert!(ui.recovery.kills.newest() == Some("typed"));
        ui.attach_image(super::super::ImageSource::Path("/tmp/not-read.png".into()));
        assert!(ui.recovery.undo.is_empty());
        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('_'), Modifiers::CONTROL)
        ));
        assert_eq!(ui.draft, "");

        ui.insert("returned");
        ui.restore_editor(EditorDraft::at_end("host returned".into()));
        assert!(ui.recovery.undo.is_empty());
        assert_eq!(ui.draft, "host returned");
        assert_eq!(ui.images.len(), 1);
        ui.cursor = ui.draft.len();
        composer_key(&mut ui, key(KeyCode::Char('y'), Modifiers::CONTROL));
        assert_eq!(ui.draft, "host returnedtyped");
    }

    #[test]
    fn history_and_kill_ring_evict_oldest_entries_at_their_bounds() {
        let mut ui = Frontend::default();
        for _ in 0..=MAX_HISTORY_ENTRIES {
            ui.insert_typed(" ");
        }
        assert_eq!(ui.recovery.undo.len(), MAX_HISTORY_ENTRIES);
        for _ in 0..MAX_HISTORY_ENTRIES {
            composer_key(&mut ui, key(KeyCode::Char('_'), Modifiers::CONTROL));
        }
        assert_eq!(ui.draft, " ", "eviction must preserve retained edits");

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
            ui.key(key(KeyCode::Char('c'), Modifiers::CONTROL), None),
            super::super::Action::None
        ));
        assert_eq!(ui.draft, "");
        assert!(ui.recovery.kills.entries.is_empty());
        assert!(composer_key(
            &mut ui,
            key(KeyCode::Char('_'), Modifiers::CONTROL)
        ));
        assert_eq!(ui.draft, "unsent 🦀");
    }

    #[test]
    fn simple_backspace_is_undoable_but_not_a_kill() {
        let mut ui = Frontend::default();
        ui.insert("ab");
        composer_key(&mut ui, key(KeyCode::Backspace, Modifiers::NONE));
        assert_eq!(ui.draft, "a");
        assert!(ui.recovery.kills.entries.is_empty());
        composer_key(&mut ui, key(KeyCode::Char('_'), Modifiers::CONTROL));
        assert_eq!(ui.draft, "ab");
    }
}
