//! Question geometry and styled presentation; no answers or terminal I/O.
use std::ops::Range;

use ion_host::{SYNTHETIC_NEXT_LABEL, SYNTHETIC_TEXT_LABEL};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::{QuestionModal, preview};
use crate::{
    display_text::{diagnostic, fit_line, push_prefixed, push_wrapped},
    keybindings::{KeyAction, KeyBindings},
    presentation_style::{Role, Theme},
    terminal_client::composer::input_window,
};

const MAX_EDITOR_ROWS: usize = 4;
const PREVIEW_MIN_WIDTH: usize = 100;
const COLUMN_GAP: usize = 2;

pub(in crate::terminal_client) struct QuestionFrame {
    pub(in crate::terminal_client) rows: Vec<Line<'static>>,
    pub(in crate::terminal_client) cursor: Option<(usize, u16)>,
}

struct Choices {
    rows: Vec<Line<'static>>,
    selected: Range<usize>,
    cursor: Option<(usize, usize)>,
}

impl QuestionModal {
    pub(in crate::terminal_client) fn render(
        &mut self,
        width: u16,
        height: u16,
        keys: &KeyBindings,
        theme: &Theme,
    ) -> QuestionFrame {
        let width = usize::from(width.max(1));
        let height = usize::from(height.max(1));
        self.too_small = width < 8;
        self.selection_visible = false;
        if self.collapsed {
            let mut rows = vec![Line::raw(""); height];
            rows[0] = Line::styled(
                fit_line(&Self::collapsed_hint(keys), width),
                theme.style(Role::Secondary),
            );
            return QuestionFrame { rows, cursor: None };
        }
        if self.too_small {
            let mut notice = Vec::new();
            push_wrapped(&mut notice, "Resize terminal to answer", width);
            let mut rows = vec![Line::raw(""); height];
            for (row, line) in rows.iter_mut().zip(notice) {
                *row = Line::styled(line, theme.style(Role::Warning));
            }
            return QuestionFrame { rows, cursor: None };
        }

        if self.page == super::Page::Review {
            return self.render_review(width, height, keys, theme);
        }
        let mut footer = vec![fit_line(&self.controls(keys), width)];
        if self.questions.len() > 1 && !self.editing {
            footer.push(fit_line(
                &format!(
                    "{} / {} question/review tabs",
                    keys.hint(KeyAction::QuestionNext),
                    keys.hint(KeyAction::QuestionPrevious)
                ),
                width,
            ));
        }
        if self.has_preview() {
            footer.push(fit_line(
                &format!(
                    "{} / {} scroll preview",
                    keys.hint(KeyAction::PageUp),
                    keys.hint(KeyAction::PageDown)
                ),
                width,
            ));
        }
        if !self.status.is_empty() {
            footer.push(fit_line(&self.status, width));
        }
        footer.truncate(height.saturating_sub(1));
        let viewport = height - footer.len();
        let rich = self.has_preview();
        let side_by_side = rich && width >= PREVIEW_MIN_WIDTH && viewport >= 3;
        let choice_width = if side_by_side {
            let longest = self
                .current_options()
                .iter()
                .map(|option| UnicodeWidthStr::width(diagnostic(&option.label).as_str()) + 2)
                .max()
                .unwrap_or(0);
            longest.max(30).min(width / 2)
        } else {
            width
        };
        let choices = self.choice_rows(choice_width, theme);
        let selected_option = self.current_options().get(self.draft().selected);
        let requires_preview = selected_option.is_some_and(|option| option.preview.is_some());
        let mut box_rows = Vec::new();
        if rich {
            let box_width = if side_by_side {
                width - choice_width - COLUMN_GAP
            } else {
                width
            };
            let box_budget = if side_by_side {
                viewport
            } else {
                // Keep the whole selected label if possible, rather than
                // letting a tall preview squeeze it into an unseen choice.
                viewport
                    .saturating_sub(choices.selected.len().max(1) + 1)
                    .min((viewport / 2).max(3))
            };
            if box_budget >= 3 {
                let rendered = preview::render(
                    selected_option.and_then(|option| option.preview.as_deref()),
                    selected_option.map_or(SYNTHETIC_TEXT_LABEL, |option| option.label.as_str()),
                    box_width,
                    box_budget,
                    self.preview_offset,
                    theme,
                );
                self.preview_offset = rendered.offset;
                self.preview_page = rendered.page;
                box_rows = rendered.rows;
            }
        }
        let choice_budget = if side_by_side || box_rows.is_empty() {
            viewport
        } else {
            viewport - box_rows.len() - 1
        };
        let focus = choices
            .cursor
            .map_or(choices.selected.end.saturating_sub(1), |(row, _)| row);
        let start = focus
            .saturating_add(1)
            .saturating_sub(choice_budget)
            .min(choices.rows.len().saturating_sub(choice_budget));
        // A wrapped label is one choice. Painting its first line is not
        // confirmation authority for a hidden remainder.
        self.selection_visible =
            choices.selected.start >= start && choices.selected.end <= start + choice_budget;
        let mut cursor = choices.cursor.and_then(|(row, col)| {
            (row >= start && row < start + choice_budget)
                .then_some((row - start, col.min(width - 1) as u16))
        });
        if self.editing {
            self.selection_visible = cursor.is_some();
        }
        let notice = if rich && box_rows.is_empty() && requires_preview {
            self.selection_visible = false;
            Some("Resize terminal to show the preview")
        } else if !self.selection_visible {
            Some("Resize terminal to show the selected choice")
        } else {
            None
        };
        if let (Some(notice), Some(last)) = (notice, footer.last_mut()) {
            *last = fit_line(notice, width);
        }
        let mut rows = vec![Line::raw(""); height];
        for (index, line) in choices
            .rows
            .into_iter()
            .skip(start)
            .take(choice_budget)
            .enumerate()
        {
            rows[index] = line;
        }
        if side_by_side {
            for (index, right) in box_rows.into_iter().enumerate() {
                let left = std::mem::take(&mut rows[index]);
                let pad = choice_width.saturating_sub(left.width()) + COLUMN_GAP;
                let mut joined = Line::default();
                append(&mut joined, left);
                joined.spans.push(Span::raw(" ".repeat(pad)));
                append(&mut joined, right);
                rows[index] = joined;
            }
            cursor = None;
        } else if !box_rows.is_empty() {
            for (index, line) in box_rows.into_iter().enumerate() {
                rows[choice_budget + 1 + index] = line;
            }
        }
        if footer.is_empty()
            && let Some(notice) = notice
        {
            rows[0] = Line::styled(fit_line(notice, width), theme.style(Role::Warning));
        }
        for (index, line) in footer.into_iter().enumerate() {
            rows[viewport + index] = Line::styled(line, theme.style(Role::Secondary));
        }
        QuestionFrame { rows, cursor }
    }

    fn choice_rows(&self, width: usize, theme: &Theme) -> Choices {
        let question = &self.questions[self.index];
        let mut content = Vec::new();
        if self.questions.len() > 1 {
            content.push(self.tabs(width));
        }
        let header = question
            .header
            .as_ref()
            .map_or(String::new(), |text| format!(" · {text}"));
        push_wrapped(
            &mut content,
            &format!(
                "Question {}/{}{header} · {}",
                self.index + 1,
                self.questions.len(),
                question.question,
            ),
            width,
        );
        let heading_rows = content.len();
        content.push(String::new());
        let mut selected = 0..0;
        for (index, option) in question.options.iter().enumerate() {
            let start = content.len();
            let marker = if index == self.draft().selected {
                "› "
            } else {
                "  "
            };
            let checkbox = if question.multi_select {
                if self.draft().checked.contains(&index) {
                    "[x] "
                } else {
                    "[ ] "
                }
            } else {
                ""
            };
            let confirmed = if matches!(self.draft().confirmed, Some(ion_host::QuestionAnswer::Option { index: chosen }) if chosen == index)
            {
                " ✓"
            } else {
                ""
            };
            push_wrapped(
                &mut content,
                &format!("{marker}{checkbox}{}{confirmed}", option.label),
                width,
            );
            if index == self.draft().selected {
                selected = start..content.len();
            }
            if let Some(description) = &option.description {
                push_prefixed(&mut content, "    ", "    ", description, width);
            }
        }
        let synthetic = question.options.len();
        let mut start = content.len();
        let marker = if self.draft().selected == synthetic {
            "› "
        } else {
            "  "
        };
        push_wrapped(
            &mut content,
            &format!("{marker}{SYNTHETIC_TEXT_LABEL}"),
            width,
        );
        if self.draft().selected == synthetic {
            selected = start..content.len();
        }
        if question.multi_select {
            start = content.len();
            let marker = if self.draft().selected == synthetic + 1 {
                "› "
            } else {
                "  "
            };
            push_wrapped(
                &mut content,
                &format!("{marker}{SYNTHETIC_NEXT_LABEL}"),
                width,
            );
            if self.draft().selected == synthetic + 1 {
                selected = start..content.len();
            }
        }
        let cursor = self.editing.then(|| {
            let editor = &self.draft().text;
            content.push(String::new());
            let base = content.len();
            let window = input_window(&editor.text, editor.cursor, width - 2, MAX_EDITOR_ROWS);
            for line in window.lines {
                content.push(format!("  {line}"));
            }
            (base + window.cursor_row, 2 + window.cursor_col)
        });
        let rows = content
            .into_iter()
            .enumerate()
            .map(|(index, text)| {
                let role = if selected.contains(&index) && !self.editing {
                    Role::Selected
                } else if index < heading_rows {
                    Role::Heading
                } else {
                    Role::Text
                };
                Line::styled(text, theme.style(role))
            })
            .collect();
        Choices {
            rows,
            selected,
            cursor,
        }
    }

    fn controls(&self, keys: &KeyBindings) -> String {
        let controls = if self.editing {
            format!(
                "{} answer · {} editor · {} newline",
                keys.hint(KeyAction::Submit),
                keys.hint(KeyAction::External),
                keys.hint(KeyAction::NewLine)
            )
        } else if self.multi_select() {
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
        format!(
            "{} hide · {controls} · {} cancel · {} stops Turn",
            keys.hint(KeyAction::QuestionCollapse),
            keys.hint(KeyAction::Cancel),
            keys.hint(KeyAction::Interrupt)
        )
    }
}

fn append(target: &mut Line<'static>, source: Line<'static>) {
    target
        .spans
        .extend(source.spans.into_iter().map(|mut span| {
            span.style = source.style.patch(span.style);
            span
        }));
}
