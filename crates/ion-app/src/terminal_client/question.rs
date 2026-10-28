//! Typed question modal: presentation, bounded drafts and lease-bound answers.
//! Question custody itself remains in ion-host.
use crate::keybindings::{self, CHOICE, KeyAction, KeyBindings, TEXT};
use ion_host::{MAX_ANSWER_BYTES, Question, QuestionAnswer, QuestionRequestId};
use ion_terminal::KeyEvent;
#[cfg(test)]
use ion_terminal::{KeyCode, Modifiers};
mod preview;
mod render;
mod review;

use super::editing::{self, Edit};

pub(super) struct QuestionModal {
    request: QuestionRequestId,
    questions: Vec<Question>,
    drafts: Vec<QuestionDraft>,
    // Always a valid question index, including while the review page is open.
    index: usize,
    page: Page,
    editing: bool,
    collapsed: bool,
    status: String,
    too_small: bool,
    selection_visible: bool,
    preview_offset: usize,
    preview_page: usize,
    review_choice: usize,
    review_offset: usize,
    review_page: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Question,
    Review,
}

#[derive(Default)]
struct QuestionDraft {
    /// Focused row: options, free text, then Next for multi-select.
    selected: usize,
    checked: Vec<usize>,
    text: TextEditor,
    // None is unanswered/edited, not the explicitly confirmed Options { [] }.
    confirmed: Option<QuestionAnswer>,
}

#[derive(Default)]
struct TextEditor {
    text: String,
    cursor: usize,
}

pub(super) enum QuestionAction {
    None,
    Scroll { up: bool },
    Answer(QuestionRequestId, Vec<QuestionAnswer>),
    Cancel(QuestionRequestId),
}

impl QuestionModal {
    pub(super) fn new(request: QuestionRequestId, questions: Vec<Question>) -> Self {
        Self {
            request,
            drafts: questions.iter().map(|_| QuestionDraft::default()).collect(),
            questions,
            index: 0,
            page: Page::Question,
            editing: false,
            collapsed: false,
            status: String::new(),
            too_small: false,
            selection_visible: false,
            preview_offset: 0,
            preview_page: 1,
            review_choice: 0,
            review_offset: 0,
            review_page: 1,
        }
    }

    fn draft(&self) -> &QuestionDraft {
        &self.drafts[self.index]
    }

    fn draft_mut(&mut self) -> &mut QuestionDraft {
        &mut self.drafts[self.index]
    }

    pub(super) fn request(&self) -> QuestionRequestId {
        self.request
    }

    pub(super) fn set_status(&mut self, status: String) {
        self.status = status;
    }

    pub(super) fn is_collapsed(&self) -> bool {
        self.collapsed
    }

    pub(super) fn collapsed_hint(keys: &KeyBindings) -> String {
        format!(
            "Question hidden · {} reopen · {} cancel",
            keys.hint(KeyAction::QuestionCollapse),
            keys.hint(KeyAction::Cancel),
        )
    }

    pub(super) fn key(&mut self, key: KeyEvent, keys: &KeyBindings) -> QuestionAction {
        let context = if self.editing && !self.collapsed {
            TEXT
        } else {
            CHOICE
        };
        let action = keys.action(context, key);
        if action == Some(KeyAction::QuestionCollapse) {
            self.collapsed = !self.collapsed;
            self.invalidate_visibility();
            return QuestionAction::None;
        }
        if self.collapsed {
            return match action {
                Some(KeyAction::Cancel) => QuestionAction::Cancel(self.request),
                Some(KeyAction::PageUp | KeyAction::PageDown) => QuestionAction::Scroll {
                    up: action == Some(KeyAction::PageUp),
                },
                _ => QuestionAction::None,
            };
        }
        if self.too_small && action != Some(KeyAction::Cancel) {
            return QuestionAction::None;
        }
        if self.editing {
            return self.editor_key(key, action);
        }
        if action == Some(KeyAction::Cancel) {
            return QuestionAction::Cancel(self.request);
        }
        if matches!(
            action,
            Some(KeyAction::QuestionNext | KeyAction::QuestionPrevious)
        ) {
            self.navigate(action == Some(KeyAction::QuestionNext));
            return QuestionAction::None;
        }
        if self.page == Page::Review {
            return self.review_key(action);
        }
        match action {
            Some(KeyAction::SelectUp) => {
                self.draft_mut().selected = self.draft().selected.saturating_sub(1);
                self.invalidate_selection();
                QuestionAction::None
            }
            Some(KeyAction::SelectDown) => {
                let last = self.current_options().len() + usize::from(self.multi_select());
                self.draft_mut().selected = (self.draft().selected + 1).min(last);
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
                self.invalidate_visibility();
                QuestionAction::None
            }
            Some(KeyAction::Toggle) if self.multi_select() => {
                if self.selection_visible && self.draft().selected < self.current_options().len() {
                    self.toggle();
                }
                QuestionAction::None
            }
            _ => QuestionAction::None,
        }
    }

    pub(super) fn paste(&mut self, text: &str) -> QuestionAction {
        if self.collapsed || self.too_small || self.page == Page::Review {
            return QuestionAction::None;
        }
        if !self.editing {
            if !self.selection_visible {
                return QuestionAction::None;
            }
            if self.draft().selected == self.current_options().len() {
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
        if self.draft().selected == options {
            self.open_editor();
            return QuestionAction::None;
        }
        let answer = if self.multi_select() {
            if self.draft().selected < options {
                self.toggle();
                return QuestionAction::None;
            }
            QuestionAnswer::Options {
                indices: (0..options)
                    .filter(|index| self.draft().checked.contains(index))
                    .collect(),
            }
        } else {
            QuestionAnswer::Option {
                index: self.draft().selected,
            }
        };
        self.draft_mut().confirmed = Some(answer);
        self.advance()
    }

    fn multi_select(&self) -> bool {
        self.questions[self.index].multi_select
    }

    fn toggle(&mut self) {
        let draft = self.draft_mut();
        if let Some(position) = draft
            .checked
            .iter()
            .position(|index| *index == draft.selected)
        {
            draft.checked.remove(position);
        } else {
            draft.checked.push(draft.selected);
        }
        draft.confirmed = None;
        self.invalidate_visibility();
        self.status.clear();
    }

    fn open_editor(&mut self) {
        self.invalidate_visibility();
        self.editing = true;
        self.status = "Type a custom answer".into();
    }

    fn editor_key(&mut self, key: KeyEvent, action: Option<KeyAction>) -> QuestionAction {
        match action {
            Some(KeyAction::Cancel) => return QuestionAction::Cancel(self.request),
            Some(KeyAction::Submit) if self.selection_visible => return self.confirm_custom(),
            Some(KeyAction::NewLine) => self.insert("\n"),
            Some(KeyAction::Tab) => self.insert("\t"),
            Some(KeyAction::SelectUp | KeyAction::SelectDown) => {
                let editor = &mut self.draft_mut().text;
                if let Some(cursor) = editing::vertical_cursor(
                    &editor.text,
                    editor.cursor,
                    action == Some(KeyAction::SelectDown),
                ) {
                    editor.cursor = cursor;
                }
            }
            Some(action) => {
                let draft = self.draft_mut();
                let editor = &mut draft.text;
                if let Some(edit) = editing::apply(&editor.text, editor.cursor, action) {
                    match edit {
                        Edit::Move(cursor) => editor.cursor = cursor,
                        Edit::Delete { range, .. } => {
                            editor.cursor = range.start;
                            editor.text.replace_range(range, "");
                            draft.confirmed = None;
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
        let text = &self.draft().text.text;
        if text.trim().is_empty() {
            self.status = "A custom answer must not be empty".into();
            return QuestionAction::None;
        }
        let text = text.clone();
        let draft = self.draft_mut();
        draft.confirmed = Some(QuestionAnswer::Text { text });
        draft.checked.clear();
        self.editing = false;
        self.advance()
    }

    fn advance(&mut self) -> QuestionAction {
        self.invalidate_selection();
        if self.questions.len() == 1 {
            return self.submit();
        }
        if self.index + 1 < self.questions.len() {
            self.index += 1;
        } else {
            self.page = Page::Review;
            self.review_choice = 0;
            self.review_offset = 0;
        }
        QuestionAction::None
    }

    fn insert(&mut self, text: &str) {
        if !self.editing {
            return;
        }
        let clean = text
            .chars()
            .filter_map(|ch| match ch {
                '\n' | '\t' => Some(ch),
                '\r' => None,
                ch if ch.is_control() => None,
                ch => Some(ch),
            })
            .collect::<String>();
        if self.draft().text.text.len().saturating_add(clean.len()) > MAX_ANSWER_BYTES {
            self.status = format!("A custom answer is limited to {MAX_ANSWER_BYTES} UTF-8 bytes");
            return;
        }
        let draft = self.draft_mut();
        draft.text.text.insert_str(draft.text.cursor, &clean);
        draft.text.cursor += clean.len();
        if !clean.is_empty() {
            draft.confirmed = None;
        }
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
            && !self.editing
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
mod review_tests;
#[cfg(test)]
mod tests;
