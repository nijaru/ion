//! Multi-question navigation and final review over the same immutable request.
use ion_host::QuestionAnswer;
use ratatui::text::Line;

use super::{Page, QuestionAction, QuestionModal, render::QuestionFrame};
use crate::{
    display_text::{fit_line, push_wrapped},
    keybindings::{KeyAction, KeyBindings},
    presentation_style::{Role, Theme},
};

impl QuestionModal {
    pub(super) fn navigate(&mut self, forward: bool) {
        if self.questions.len() == 1 {
            return;
        }
        let count = self.questions.len();
        let current = if self.page == Page::Review {
            count
        } else {
            self.index
        };
        let next = (current + if forward { 1 } else { count }) % (count + 1);
        self.page = if next == count {
            Page::Review
        } else {
            Page::Question
        };
        if next < count {
            self.index = next;
        }
        self.review_choice = 0;
        self.review_offset = 0;
        self.invalidate_selection();
    }

    pub(super) fn submit(&mut self) -> QuestionAction {
        let answers: Option<Vec<_>> = self
            .drafts
            .iter()
            .map(|draft| draft.confirmed.clone())
            .collect();
        self.invalidate_visibility();
        match answers {
            Some(answers) => QuestionAction::Answer(self.request, answers),
            None => {
                self.status = "Confirm every question before submitting".into();
                QuestionAction::None
            }
        }
    }

    pub(super) fn review_key(&mut self, action: Option<KeyAction>) -> QuestionAction {
        match action {
            Some(KeyAction::SelectUp | KeyAction::SelectDown) => {
                self.review_choice = 1 - self.review_choice;
                self.invalidate_visibility();
            }
            Some(KeyAction::PageUp | KeyAction::PageDown) => {
                self.review_offset = if action == Some(KeyAction::PageUp) {
                    self.review_offset.saturating_sub(self.review_page)
                } else {
                    self.review_offset.saturating_add(self.review_page)
                };
                self.invalidate_visibility();
            }
            Some(KeyAction::Confirm) if self.can_confirm() => {
                return if self.review_choice == 0 {
                    self.submit()
                } else {
                    self.invalidate_visibility();
                    QuestionAction::Cancel(self.request)
                };
            }
            _ => {}
        }
        QuestionAction::None
    }

    pub(super) fn tabs(&self, width: usize) -> String {
        let mut tabs = self
            .questions
            .iter()
            .zip(&self.drafts)
            .enumerate()
            .map(|(index, (question, draft))| {
                let name = question
                    .header
                    .clone()
                    .unwrap_or_else(|| format!("Q{}", index + 1));
                let mark = if draft.confirmed.is_some() {
                    "✓"
                } else {
                    "·"
                };
                if self.page == Page::Question && self.index == index {
                    format!("[{name}{mark}]")
                } else {
                    format!("{name}{mark}")
                }
            })
            .collect::<Vec<_>>();
        tabs.push(
            if self.page == Page::Review {
                "[Review]"
            } else {
                "Review"
            }
            .into(),
        );
        fit_line(&tabs.join("  "), width)
    }

    pub(super) fn render_review(
        &mut self,
        width: usize,
        height: usize,
        keys: &KeyBindings,
        theme: &Theme,
    ) -> QuestionFrame {
        // Tabs, status, content, both action labels and both hints must fit.
        // Review never borrows authority from a prior question's paint.
        if width < 16 || height < 7 {
            return QuestionFrame {
                rows: (0..height)
                    .map(|index| {
                        Line::styled(
                            if index == 0 {
                                fit_line("Resize terminal to review answers", width)
                            } else {
                                String::new()
                            },
                            theme.style(Role::Warning),
                        )
                    })
                    .collect(),
                cursor: None,
                candidate: None,
                editor_width: None,
            };
        }
        let mut content = Vec::new();
        for (index, (question, draft)) in self.questions.iter().zip(&self.drafts).enumerate() {
            push_wrapped(
                &mut content,
                &format!("Q{} · {}", index + 1, question.question),
                width,
            );
            match &draft.confirmed {
                Some(QuestionAnswer::Option { index }) => {
                    push_wrapped(&mut content, &question.options[*index].label, width)
                }
                Some(QuestionAnswer::Options { indices }) => {
                    if indices.is_empty() {
                        push_wrapped(&mut content, "None (explicitly confirmed)", width);
                    }
                    for index in indices {
                        push_wrapped(
                            &mut content,
                            &format!("[x] {}", question.options[*index].label),
                            width,
                        );
                    }
                }
                Some(QuestionAnswer::Text { text }) => {
                    push_wrapped(&mut content, &format!("Custom: {text}"), width)
                }
                None => push_wrapped(
                    &mut content,
                    "Not confirmed · return to this question",
                    width,
                ),
            }
            content.push(String::new());
        }
        let height = height.min((content.len() + 6).max(7));
        let budget = height - 6;
        self.review_page = budget;
        self.review_offset = self.review_offset.min(content.len().saturating_sub(budget));
        let mut rows = vec![Line::raw(""); height];
        let confirmed = self
            .drafts
            .iter()
            .filter(|draft| draft.confirmed.is_some())
            .count();
        rows[0] = Line::styled(self.tabs(width), theme.style(Role::Heading));
        rows[1] = Line::styled(
            fit_line(
                &format!(
                    "Review · {confirmed}/{} confirmed · {}",
                    self.questions.len(),
                    self.status
                ),
                width,
            ),
            theme.style(Role::Heading),
        );
        for (row, text) in rows[2..2 + budget]
            .iter_mut()
            .zip(content.into_iter().skip(self.review_offset))
        {
            *row = Line::styled(text, theme.style(Role::Text));
        }
        for (choice, label) in ["Submit answers", "Cancel"].into_iter().enumerate() {
            let selected = self.review_choice == choice;
            rows[height - 4 + choice] = Line::styled(
                format!("{}{label}", if selected { "› " } else { "  " }),
                theme.style(if selected { Role::Selected } else { Role::Text }),
            );
        }
        rows[height - 2] = Line::styled(
            fit_line(
                &format!(
                    "{} hide · {} / {} tabs · {} / {} choose · {} select",
                    keys.hint(KeyAction::QuestionCollapse),
                    keys.hint(KeyAction::QuestionNext),
                    keys.hint(KeyAction::QuestionPrevious),
                    keys.hint(KeyAction::SelectUp),
                    keys.hint(KeyAction::SelectDown),
                    keys.hint(KeyAction::Confirm),
                ),
                width,
            ),
            theme.style(Role::Secondary),
        );
        rows[height - 1] = Line::styled(
            fit_line(
                &format!(
                    "{} / {} scroll · {} cancel · {} stops Turn",
                    keys.hint(KeyAction::PageUp),
                    keys.hint(KeyAction::PageDown),
                    keys.hint(KeyAction::Cancel),
                    keys.hint(KeyAction::Interrupt),
                ),
                width,
            ),
            theme.style(Role::Secondary),
        );
        QuestionFrame {
            rows,
            cursor: None,
            candidate: Some(self.visibility.prepare(self.target())),
            editor_width: None,
        }
    }
}
