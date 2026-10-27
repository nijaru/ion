//! Typed question modal: presentation, bounded editor and lease-bound answers.
//! Question custody itself remains in ion-host.
use crate::keybindings::{self, CHOICE, KeyAction, KeyBindings, TEXT};
use ion_host::{MAX_ANSWER_BYTES, Question, QuestionAnswer, QuestionRequestId};
use ion_terminal::KeyEvent;
#[cfg(test)]
use ion_terminal::{KeyCode, Modifiers};
mod preview;
mod render;

use super::editing::{self, Edit};

pub(super) struct QuestionModal {
    request: QuestionRequestId,
    questions: Vec<Question>,
    index: usize,
    /// Focused row: options, free text, then Next for multi-select.
    selected: usize,
    checked: Vec<usize>,
    editor: Option<TextEditor>,
    answers: Vec<QuestionAnswer>,
    status: String,
    too_small: bool,
    selection_visible: bool,
    preview_offset: usize,
    preview_page: usize,
}

struct TextEditor {
    text: String,
    cursor: usize,
}

pub(super) enum QuestionAction {
    None,
    Answer(QuestionRequestId, Vec<QuestionAnswer>),
    Cancel(QuestionRequestId),
}

impl QuestionModal {
    pub(super) fn new(request: QuestionRequestId, questions: Vec<Question>) -> Self {
        Self {
            request,
            questions,
            index: 0,
            selected: 0,
            checked: Vec::new(),
            editor: None,
            answers: Vec::new(),
            status: String::new(),
            too_small: false,
            selection_visible: false,
            preview_offset: 0,
            preview_page: 1,
        }
    }

    pub(super) fn request(&self) -> QuestionRequestId {
        self.request
    }

    pub(super) fn set_status(&mut self, status: String) {
        self.status = status;
    }

    pub(super) fn key(&mut self, key: KeyEvent, keys: &KeyBindings) -> QuestionAction {
        let context = if self.editor.is_some() { TEXT } else { CHOICE };
        let action = keys.action(context, key);
        if self.too_small && action != Some(KeyAction::Cancel) {
            return QuestionAction::None;
        }
        if self.editor.is_some() {
            return self.editor_key(key, action);
        }
        match action {
            Some(KeyAction::Cancel) => QuestionAction::Cancel(self.request),
            Some(KeyAction::SelectUp) => {
                self.selected = self.selected.saturating_sub(1);
                self.invalidate_selection();
                QuestionAction::None
            }
            Some(KeyAction::SelectDown) => {
                let last = self.current_options().len() + usize::from(self.multi_select());
                self.selected = (self.selected + 1).min(last);
                self.invalidate_selection();
                QuestionAction::None
            }
            Some(KeyAction::Confirm) if self.selection_visible => self.confirm(),
            Some(KeyAction::PageUp | KeyAction::PageDown) if self.has_preview() => {
                self.preview_offset = if action == Some(KeyAction::PageUp) {
                    self.preview_offset.saturating_sub(self.preview_page)
                } else {
                    self.preview_offset.saturating_add(self.preview_page)
                };
                QuestionAction::None
            }
            Some(KeyAction::Toggle) if self.multi_select() => {
                if self.selection_visible && self.selected < self.current_options().len() {
                    self.toggle();
                }
                QuestionAction::None
            }
            _ => QuestionAction::None,
        }
    }

    pub(super) fn paste(&mut self, text: &str) -> QuestionAction {
        if self.too_small {
            return QuestionAction::None;
        }
        if self.editor.is_none() {
            if !self.selection_visible {
                return QuestionAction::None;
            }
            if self.selected == self.current_options().len() {
                self.open_editor();
            } else {
                self.status = "Select Type something. to enter a custom answer".into();
                return QuestionAction::None;
            }
        }
        self.insert(text);
        QuestionAction::None
    }

    fn confirm(&mut self) -> QuestionAction {
        let options = self.current_options().len();
        if self.selected == options {
            self.open_editor();
            return QuestionAction::None;
        }
        if self.multi_select() {
            if self.selected < options {
                self.toggle();
                return QuestionAction::None;
            }
            self.answers.push(QuestionAnswer::Options {
                indices: (0..options)
                    .filter(|index| self.checked.contains(index))
                    .collect(),
            });
        } else {
            self.answers.push(QuestionAnswer::Option {
                index: self.selected,
            });
        }
        self.advance()
    }

    fn multi_select(&self) -> bool {
        self.questions[self.index].multi_select
    }

    fn toggle(&mut self) {
        if let Some(position) = self
            .checked
            .iter()
            .position(|index| *index == self.selected)
        {
            self.checked.remove(position);
        } else {
            self.checked.push(self.selected);
        }
        self.status.clear();
    }

    fn open_editor(&mut self) {
        self.selection_visible = false;
        self.editor = Some(TextEditor {
            text: String::new(),
            cursor: 0,
        });
        self.status = "Type a custom answer".into();
    }

    fn editor_key(&mut self, key: KeyEvent, action: Option<KeyAction>) -> QuestionAction {
        match action {
            Some(KeyAction::Cancel) => return QuestionAction::Cancel(self.request),
            Some(KeyAction::Submit) if self.selection_visible => return self.confirm_custom(),
            Some(KeyAction::NewLine) => self.insert("\n"),
            Some(KeyAction::Tab) => self.insert("\t"),
            Some(KeyAction::SelectUp | KeyAction::SelectDown) => {
                let editor = self.editor.as_mut().expect("checked editor");
                if let Some(cursor) = editing::vertical_cursor(
                    &editor.text,
                    editor.cursor,
                    action == Some(KeyAction::SelectDown),
                ) {
                    editor.cursor = cursor;
                }
            }
            Some(action) => {
                let editor = self.editor.as_mut().expect("checked editor");
                if let Some(edit) = editing::apply(&editor.text, editor.cursor, action) {
                    match edit {
                        Edit::Move(cursor) => editor.cursor = cursor,
                        Edit::Delete { range, .. } => {
                            editor.cursor = range.start;
                            editor.text.replace_range(range, "");
                        }
                    }
                }
            }
            None => {
                if let Some(ch) = keybindings::printable(key) {
                    let mut bytes = [0; 4];
                    self.insert(ch.encode_utf8(&mut bytes));
                }
            }
        }
        QuestionAction::None
    }

    fn confirm_custom(&mut self) -> QuestionAction {
        let editor = self.editor.as_ref().expect("checked editor");
        if editor.text.trim().is_empty() {
            self.status = "A custom answer must not be empty".into();
            return QuestionAction::None;
        }
        if editor.text.len() > MAX_ANSWER_BYTES {
            self.status = format!("A custom answer is limited to {MAX_ANSWER_BYTES} UTF-8 bytes");
            return QuestionAction::None;
        }
        let text = editor.text.clone();
        self.editor = None;
        self.answers.push(QuestionAnswer::Text { text });
        self.advance()
    }

    fn advance(&mut self) -> QuestionAction {
        self.checked.clear();
        self.invalidate_selection();
        if self.index + 1 < self.questions.len() {
            self.index += 1;
            self.selected = 0;
            self.status.clear();
            QuestionAction::None
        } else {
            QuestionAction::Answer(self.request, self.answers.clone())
        }
    }

    fn insert(&mut self, text: &str) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        let clean = text
            .chars()
            .filter_map(|ch| match ch {
                '\n' | '\t' => Some(ch),
                '\r' => None,
                ch if ch.is_control() => None,
                ch => Some(ch),
            })
            .collect::<String>();
        if editor.text.len().saturating_add(clean.len()) > MAX_ANSWER_BYTES {
            self.status = format!("A custom answer is limited to {MAX_ANSWER_BYTES} UTF-8 bytes");
            return;
        }
        editor.text.insert_str(editor.cursor, &clean);
        editor.cursor += clean.len();
        self.status.clear();
    }

    pub(super) fn invalidate_visibility(&mut self) {
        self.selection_visible = false;
    }

    fn invalidate_selection(&mut self) {
        self.invalidate_visibility();
        self.preview_offset = 0;
        self.status.clear();
    }

    fn has_preview(&self) -> bool {
        !self.multi_select()
            && self.editor.is_none()
            && self
                .current_options()
                .iter()
                .any(|option| option.preview.is_some())
    }

    fn current_options(&self) -> &[ion_host::QuestionOption] {
        &self.questions[self.index].options
    }
}

#[cfg(test)]
mod preview_tests;
#[cfg(test)]
mod tests;
