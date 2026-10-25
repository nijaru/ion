//! Typed question modal: presentation, bounded editor and lease-bound answers.
//! Question custody itself remains in ion-host.
use crate::keybindings::{self, CHOICE, KeyAction, KeyBindings, TEXT};
use ion_host::{
    MAX_ANSWER_BYTES, Question, QuestionAnswer, QuestionRequestId, SYNTHETIC_NEXT_LABEL,
    SYNTHETIC_TEXT_LABEL,
};
use ion_terminal::KeyEvent;
#[cfg(test)]
use ion_terminal::{KeyCode, Modifiers};
use ratatui::text::Line;

use super::composer::input_window;
use super::editing::{self, Edit};
use crate::display_text::{fit_line, push_prefixed, push_wrapped};

const MAX_EDITOR_ROWS: usize = 4;

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

pub(super) struct QuestionFrame {
    pub(super) rows: Vec<Line<'static>>,
    pub(super) cursor: Option<(usize, u16)>,
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
                self.status.clear();
                QuestionAction::None
            }
            Some(KeyAction::SelectDown) => {
                let last = self.current_options().len() + usize::from(self.multi_select());
                self.selected = (self.selected + 1).min(last);
                self.status.clear();
                QuestionAction::None
            }
            Some(KeyAction::Confirm) => self.confirm(),
            Some(KeyAction::Toggle) if self.multi_select() => {
                if self.selected < self.current_options().len() {
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
        self.editor = Some(TextEditor {
            text: String::new(),
            cursor: 0,
        });
        self.status = "Type a custom answer".into();
    }

    fn editor_key(&mut self, key: KeyEvent, action: Option<KeyAction>) -> QuestionAction {
        match action {
            Some(KeyAction::Cancel) => return QuestionAction::Cancel(self.request),
            Some(KeyAction::Submit) => return self.confirm_custom(),
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

    fn current_options(&self) -> &[ion_host::QuestionOption] {
        &self.questions[self.index].options
    }

    pub(super) fn render(&mut self, width: u16, height: u16, keys: &KeyBindings) -> QuestionFrame {
        let width = width.max(1) as usize;
        let height = height.max(1) as usize;
        self.too_small = width < 8;
        if self.too_small {
            let mut notice = Vec::new();
            push_wrapped(&mut notice, "Resize terminal to answer", width);
            let mut rows = vec![Line::raw(""); height];
            for (row, line) in rows.iter_mut().zip(notice) {
                *row = Line::raw(line);
            }
            return QuestionFrame { rows, cursor: None };
        }
        let mut content = Vec::new();
        let mut focus = 0usize;
        let question = &self.questions[self.index];
        push_wrapped(
            &mut content,
            &format!(
                "Question {}/{} · {}",
                self.index + 1,
                self.questions.len(),
                question.question
            ),
            width,
        );
        content.push(String::new());
        for (index, option) in question.options.iter().enumerate() {
            if index == self.selected {
                focus = content.len();
            }
            let marker = if index == self.selected { "› " } else { "  " };
            let checkbox = if question.multi_select {
                if self.checked.contains(&index) {
                    "[x] "
                } else {
                    "[ ] "
                }
            } else {
                ""
            };
            push_wrapped(
                &mut content,
                &format!("{marker}{checkbox}{}", option.label),
                width,
            );
            if let Some(description) = &option.description {
                push_prefixed(&mut content, "    ", "    ", description, width);
            }
        }
        let synthetic = question.options.len();
        if self.selected == synthetic {
            focus = content.len();
        }
        let marker = if self.selected == synthetic {
            "› "
        } else {
            "  "
        };
        push_wrapped(
            &mut content,
            &format!("{marker}{SYNTHETIC_TEXT_LABEL}"),
            width,
        );

        if question.multi_select {
            let marker = if self.selected == synthetic + 1 {
                focus = content.len();
                "› "
            } else {
                "  "
            };
            push_wrapped(
                &mut content,
                &format!("{marker}{SYNTHETIC_NEXT_LABEL}"),
                width,
            );
        }

        let mut cursor = None;
        if let Some(editor) = &self.editor {
            content.push(String::new());
            let base = content.len();
            let prefix = if width >= 3 { "  " } else { "" };
            let window = input_window(
                &editor.text,
                editor.cursor,
                width.saturating_sub(prefix.len()).max(1),
                MAX_EDITOR_ROWS,
            );
            focus = base + window.cursor_row;
            for line in window.lines {
                content.push(format!("{prefix}{line}"));
            }
            cursor = Some((base + window.cursor_row, prefix.len() + window.cursor_col));
        }

        let editing = self.editor.is_some();
        let controls = if editing {
            format!(
                "{} answer · {} newline",
                keys.hint(KeyAction::Submit),
                keys.hint(KeyAction::NewLine)
            )
        } else if question.multi_select {
            format!(
                "{} / {} move · {} / {} toggle · Next confirms",
                keys.hint(KeyAction::SelectUp),
                keys.hint(KeyAction::SelectDown),
                keys.hint(KeyAction::Toggle),
                keys.hint(KeyAction::Confirm)
            )
        } else {
            format!(
                "{} / {} choose · {} select",
                keys.hint(KeyAction::SelectUp),
                keys.hint(KeyAction::SelectDown),
                keys.hint(KeyAction::Confirm)
            )
        };
        let controls = format!(
            "{controls} · {} cancel · {} stops Turn",
            keys.hint(KeyAction::Cancel),
            keys.hint(KeyAction::Interrupt)
        );
        let mut footer = vec![fit_line(&controls, width)];
        if !self.status.is_empty() {
            footer.push(fit_line(&self.status, width));
        }
        let footer_budget = height.saturating_sub(1).min(footer.len());
        footer.truncate(footer_budget);
        let viewport = height.saturating_sub(footer.len()).max(1);
        let start = if content.len() <= viewport {
            0
        } else {
            focus
                .saturating_add(1)
                .saturating_sub(viewport)
                .min(content.len().saturating_sub(viewport))
        };
        let mut rows = vec![Line::raw(""); height];
        for (index, line) in content.iter().skip(start).take(viewport).enumerate() {
            rows[index] = Line::raw(line.clone());
        }
        for (index, line) in footer.iter().enumerate() {
            rows[height - footer.len() + index] = Line::raw(line.clone());
        }
        let cursor = cursor.and_then(|(row, column)| {
            (row >= start && row < start + viewport)
                .then_some((row - start, column.min(width.saturating_sub(1)) as u16))
        });
        QuestionFrame { rows, cursor }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ion_host::{Question, QuestionOption};

    fn modal() -> QuestionModal {
        QuestionModal::new(
            QuestionRequestId(uuid::Uuid::now_v7()),
            vec![
                Question {
                    question: "Choose a route".into(),
                    multi_select: false,
                    options: vec![
                        QuestionOption {
                            label: "Alpha".into(),
                            description: Some("First choice".into()),
                        },
                        QuestionOption {
                            label: "Beta".into(),
                            description: None,
                        },
                    ],
                },
                Question {
                    question: "Custom?".into(),
                    multi_select: false,
                    options: vec![
                        QuestionOption {
                            label: "Yes".into(),
                            description: None,
                        },
                        QuestionOption {
                            label: "No".into(),
                            description: None,
                        },
                    ],
                },
            ],
        )
    }

    fn enter() -> KeyEvent {
        KeyEvent::new(KeyCode::Enter, Modifiers::NONE)
    }

    #[test]
    fn selection_advances_across_batch_and_text_row() {
        let mut modal = modal();
        modal.key(
            KeyEvent::new(KeyCode::Down, Modifiers::NONE),
            &KeyBindings::default(),
        );
        assert!(matches!(
            modal.key(enter(), &KeyBindings::default()),
            QuestionAction::None
        ));
        assert_eq!(modal.answers, vec![QuestionAnswer::Option { index: 1 }]);
        modal.key(
            KeyEvent::new(KeyCode::Down, Modifiers::NONE),
            &KeyBindings::default(),
        );
        modal.key(
            KeyEvent::new(KeyCode::Down, Modifiers::NONE),
            &KeyBindings::default(),
        );
        assert_eq!(modal.selected, 2);
        assert!(matches!(
            modal.key(enter(), &KeyBindings::default()),
            QuestionAction::None
        ));
        assert!(modal.editor.is_some());
        modal.paste("café 🦀");
        assert!(
            matches!(modal.key(enter(), &KeyBindings::default()), QuestionAction::Answer(_, ref answers) if answers.last() == Some(&QuestionAnswer::Text { text: "café 🦀".into() }))
        );
    }

    #[test]
    fn multi_select_toggles_without_advancing_until_explicit_next() {
        let mut modal = modal();
        modal.questions[0].multi_select = true;
        let down = KeyEvent::new(KeyCode::Down, Modifiers::NONE);
        let space = KeyEvent::new(KeyCode::Char(' '), Modifiers::NONE);
        modal.key(down, &KeyBindings::default());
        modal.key(space, &KeyBindings::default()); // Beta first, output must still follow request order.
        modal.key(
            KeyEvent::new(KeyCode::Up, Modifiers::NONE),
            &KeyBindings::default(),
        );
        modal.key(enter(), &KeyBindings::default());
        modal.key(enter(), &KeyBindings::default()); // Undo Alpha, then select it again.
        modal.key(space, &KeyBindings::default());
        assert!(modal.answers.is_empty());
        assert_eq!(modal.index, 0);
        let rows = modal.render(80, 16, &KeyBindings::default()).rows;
        assert!(rows.iter().any(|row| row.to_string().contains("[x] Alpha")));
        assert!(rows.iter().any(|row| row.to_string().contains("[x] Beta")));
        for _ in 0..3 {
            modal.key(down, &KeyBindings::default());
        }
        assert_eq!(modal.selected, 3);
        modal.key(space, &KeyBindings::default()); // Next is not a checkable option.
        let rows = modal.render(24, 2, &KeyBindings::default()).rows;
        assert!(rows.iter().any(|row| row.to_string().contains("› Next")));
        assert!(matches!(
            modal.key(enter(), &KeyBindings::default()),
            QuestionAction::None
        ));
        assert_eq!(
            modal.answers,
            vec![QuestionAnswer::Options {
                indices: vec![0, 1]
            }]
        );
        assert_eq!(modal.index, 1);
        assert!(modal.checked.is_empty());
        assert!(
            matches!(modal.key(enter(), &KeyBindings::default()), QuestionAction::Answer(_, ref answers)
            if answers == &vec![QuestionAnswer::Options { indices: vec![0,1] }, QuestionAnswer::Option { index: 0 }])
        );
    }

    #[test]
    fn multi_select_empty_is_explicit_and_custom_text_replaces_checkboxes() {
        for custom in [false, true] {
            let mut modal = modal();
            modal.questions.truncate(1);
            modal.questions[0].multi_select = true;
            if custom {
                modal.key(enter(), &KeyBindings::default());
            }
            for _ in 0..2 {
                modal.key(
                    KeyEvent::new(KeyCode::Down, Modifiers::NONE),
                    &KeyBindings::default(),
                );
            }
            let expected = if custom {
                modal.key(enter(), &KeyBindings::default());
                modal.paste("  different 🦀  ");
                QuestionAnswer::Text {
                    text: "  different 🦀  ".into(),
                }
            } else {
                modal.key(
                    KeyEvent::new(KeyCode::Down, Modifiers::NONE),
                    &KeyBindings::default(),
                );
                QuestionAnswer::Options { indices: vec![] }
            };
            assert!(
                matches!(modal.key(enter(), &KeyBindings::default()), QuestionAction::Answer(_, ref answers) if answers == &vec![expected])
            );
            assert!(modal.checked.is_empty());
        }
    }

    #[test]
    fn custom_editor_is_bounded_and_preserves_exact_unicode() {
        let mut modal = modal();
        modal.key(
            KeyEvent::new(KeyCode::Down, Modifiers::NONE),
            &KeyBindings::default(),
        );
        modal.key(
            KeyEvent::new(KeyCode::Down, Modifiers::NONE),
            &KeyBindings::default(),
        );
        modal.key(enter(), &KeyBindings::default());
        modal.paste("  ");
        assert!(matches!(
            modal.key(enter(), &KeyBindings::default()),
            QuestionAction::None
        ));
        assert!(modal.status.contains("must not be empty"));
        modal.paste("  spaced ✓  ");
        let before = modal.editor.as_ref().unwrap().text.clone();
        modal.paste(&"x".repeat(MAX_ANSWER_BYTES));
        assert_eq!(modal.editor.as_ref().unwrap().text, before);
        assert!(modal.status.contains("limited"));
        assert!(matches!(
            modal.key(enter(), &KeyBindings::default()),
            QuestionAction::None
        ));
        assert_eq!(
            modal.answers[0],
            QuestionAnswer::Text {
                text: "    spaced ✓  ".into()
            }
        );
    }

    #[test]
    fn tiny_width_disables_submission_and_never_claims_invisible_selection() {
        for (width, multi_select) in [(1, false), (2, false), (1, true), (2, true)] {
            let mut modal = modal();
            modal.questions[0].multi_select = multi_select;
            let frame = modal.render(width, 1, &KeyBindings::default());
            assert_eq!(frame.rows.len(), 1);
            assert!(frame.cursor.is_none());
            assert!(!frame.rows[0].to_string().is_empty());
            assert!(matches!(
                modal.key(enter(), &KeyBindings::default()),
                QuestionAction::None
            ));
            assert!(modal.answers.is_empty());
            modal.paste("invisible");
            assert!(modal.answers.is_empty());
        }
        let mut modal = modal();
        modal.render(20, 1, &KeyBindings::default());
        assert!(!modal.too_small);
        assert!(matches!(
            modal.key(enter(), &KeyBindings::default()),
            QuestionAction::None
        ));
        assert_eq!(modal.answers, vec![QuestionAnswer::Option { index: 0 }]);
    }

    #[test]
    fn one_row_editor_keeps_cursor_and_text_visible_at_narrow_geometry() {
        let mut modal = modal();
        modal.render(20, 1, &KeyBindings::default());
        modal.key(
            KeyEvent::new(KeyCode::Down, Modifiers::NONE),
            &KeyBindings::default(),
        );
        modal.key(
            KeyEvent::new(KeyCode::Down, Modifiers::NONE),
            &KeyBindings::default(),
        );
        modal.key(enter(), &KeyBindings::default());
        modal.paste("x");
        let frame = modal.render(8, 1, &KeyBindings::default());
        assert!(frame.cursor.is_some());
        let (row, _) = frame.cursor.unwrap();
        assert!(frame.rows[row].to_string().contains('x'));
        assert!(!modal.too_small);
    }

    #[test]
    fn short_viewport_keeps_selected_row_and_editor_cursor_visible() {
        let mut modal = modal();
        let frame = modal.render(30, 4, &KeyBindings::default());
        assert!(
            frame
                .rows
                .iter()
                .any(|row| row.to_string().contains("Alpha"))
        );
        modal.key(
            KeyEvent::new(KeyCode::Down, Modifiers::NONE),
            &KeyBindings::default(),
        );
        let frame = modal.render(30, 3, &KeyBindings::default());
        assert!(
            frame
                .rows
                .iter()
                .any(|row| row.to_string().contains("› Beta")),
            "{:?}",
            frame
                .rows
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        );
        modal.key(
            KeyEvent::new(KeyCode::Down, Modifiers::NONE),
            &KeyBindings::default(),
        );
        modal.key(enter(), &KeyBindings::default());
        modal.paste("cursor");
        let frame = modal.render(30, 3, &KeyBindings::default());
        assert!(frame.cursor.is_some());
        assert!(frame.cursor.unwrap().0 < frame.rows.len());
    }
}
