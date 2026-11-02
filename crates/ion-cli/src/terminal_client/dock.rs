//! One measured interaction dock for both conversation viewports.
use ion_ai::ModelRef;
use ratatui::text::Line;

use super::interaction::InputTarget;
use super::paint::Prepared;
use super::picker::PickerCandidate;
use super::question::QuestionTarget;
use super::{ActiveOperation, Frontend, orientation, visible_status};
use crate::presentation_style::Role;
use crate::terminal_completion::CompletionFrame;
use crate::terminal_input::input_window;

pub(super) struct DockPaint {
    geometry: (u16, u16),
    fullscreen: bool,
    candidate: DockCandidate,
}

enum DockCandidate {
    Composer {
        completion: CompletionFrame,
        width: usize,
    },
    Picker(Option<Prepared<PickerCandidate>>),
    Question {
        candidate: Option<Prepared<QuestionTarget>>,
        editor_width: Option<usize>,
    },
    HiddenQuestion,
}

impl DockPaint {
    fn new(ui: &Frontend, width: usize, height: usize, candidate: DockCandidate) -> Self {
        Self {
            geometry: (width as u16, height as u16),
            fullscreen: ui.mode == super::TuiMode::Fullscreen,
            candidate,
        }
    }

    pub(super) fn observe(self, ui: &mut Frontend, receipt: ion_terminal::PaintReceipt) {
        self.observe_geometry(ui, receipt.geometry(), receipt.is_fullscreen());
    }

    fn observe_geometry(self, ui: &mut Frontend, geometry: (u16, u16), fullscreen: bool) {
        if self.geometry == geometry && self.fullscreen == fullscreen {
            self.candidate.install(ui);
        } else {
            ui.invalidate_interaction_paint();
        }
    }

    #[cfg(test)]
    pub(super) fn observe_simulated(
        self,
        ui: &mut Frontend,
        geometry: (u16, u16),
        fullscreen: bool,
    ) {
        self.observe_geometry(ui, geometry, fullscreen);
    }
}

impl DockCandidate {
    fn install(self, ui: &mut Frontend) {
        match self {
            Self::Composer { completion, width } if ui.input_target() == InputTarget::Composer => {
                ui.editor.completion.observe_paint(completion);
                ui.editor.publish_editor_width(width);
            }
            Self::Picker(candidate) if ui.input_target() == InputTarget::Picker => {
                ui.pickers.observe_paint(candidate)
            }
            Self::Question {
                candidate,
                editor_width,
            } if ui.input_target() == InputTarget::Question => {
                if let Some(question) = &mut ui.question {
                    question.observe_paint(candidate, editor_width);
                }
            }
            _ => ui.invalidate_interaction_paint(),
        }
    }
}

pub(super) struct Dock {
    pub body_rows: usize,
    pub rows: Vec<Line<'static>>,
    pub cursor: Option<(usize, u16)>,
    pub paint: DockPaint,
}

impl Dock {
    pub(super) fn prepare(
        ui: &mut Frontend,
        operation: Option<ActiveOperation>,
        model: Option<&ModelRef>,
        width: usize,
        height: usize,
    ) -> Self {
        ui.invalidate_interaction_paint();
        let width = width.max(1);
        let height = height.max(1);
        match ui.input_target() {
            InputTarget::Picker => return Self::selector(ui, operation, width, height),
            InputTarget::Question => {
                let question = ui.question.as_mut().expect("derived question custody");
                if question.is_collapsed() {
                    return Self {
                        body_rows: height.saturating_sub(2),
                        rows: vec![Line::styled(
                            crate::display_text::fit_line(
                                &super::QuestionModal::collapsed_hint(&ui.keys),
                                width,
                            ),
                            ui.themes.theme().style(Role::Secondary),
                        )],
                        cursor: None,
                        paint: DockPaint::new(ui, width, height, DockCandidate::HiddenQuestion),
                    };
                }
                // No caret requires a physical parking row. Layout cannot let
                // that row clip an otherwise authorized question choice.
                let budget = height.saturating_sub(1).max(1);
                let frame =
                    question.render(width as u16, budget as u16, &ui.keys, ui.themes.theme());
                return Self {
                    body_rows: height
                        .saturating_sub(frame.rows.len() + usize::from(frame.cursor.is_none())),
                    rows: frame.rows,
                    cursor: frame.cursor,
                    paint: DockPaint::new(
                        ui,
                        width,
                        height,
                        DockCandidate::Question {
                            candidate: (height > 1).then_some(frame.candidate).flatten(),
                            editor_width: frame.editor_width,
                        },
                    ),
                };
            }
            InputTarget::Composer => {}
            InputTarget::Details => unreachable!("inspector owns its fullscreen viewport"),
        }
        let budget = height;
        let mut status = Vec::new();
        if let Some(notice) = ui
            .conversation
            .live()
            .and_then(|live| live.notices().last())
        {
            status.push(format!("Notice · {notice}"));
        }
        if let Some(value) = visible_status(ui, operation) {
            status.push(value);
        }
        let status_budget = budget.saturating_sub(1);
        if status.len() > status_budget {
            status.drain(..status.len() - status_budget);
        }
        let has_body = operation.is_some()
            || !ui.conversation.mutable_items().is_empty()
            || ui.view.is_reading()
            || ui.mode == super::TuiMode::Fullscreen;
        let body_minimum = usize::from(has_body && budget > status.len() + 1);
        let discovery_room = budget.saturating_sub(1 + status.len() + body_minimum);
        let completion =
            ui.editor
                .completion
                .rows(width, discovery_room.min((budget / 3).max(1)), &ui.keys);
        let editor_room = budget
            .saturating_sub(status.len() + completion.rows.len() + body_minimum)
            .max(1);
        let editor_budget = if has_body {
            editor_room.div_ceil(2)
        } else {
            editor_room
        };
        let composer = input_window(&ui.editor.draft, ui.editor.cursor, width, editor_budget);
        let footer = orientation::rows(
            ui,
            model,
            width,
            budget.saturating_sub(
                status.len() + completion.rows.len() + composer.lines.len() + body_minimum,
            ),
        );
        let body_rows = budget.saturating_sub(
            status.len() + completion.rows.len() + composer.lines.len() + footer.len(),
        );
        let mut rows = Vec::new();
        for text in status {
            rows.push(Line::styled(
                crate::display_text::fit_line(&text, width),
                ui.themes.theme().style(Role::Secondary),
            ));
        }
        for text in &completion.rows {
            rows.push(Line::styled(
                text.clone(),
                ui.themes.theme().style(Role::Text),
            ));
        }
        let composer_top = rows.len();
        for text in composer.lines {
            rows.push(Line::styled(text, ui.themes.theme().style(Role::UserInput)));
        }
        rows.extend(footer);
        let cursor = Some((
            composer_top + composer.cursor_row,
            composer.cursor_col.min(width - 1) as u16,
        ));
        Self {
            body_rows,
            rows,
            cursor,
            paint: DockPaint::new(
                ui,
                width,
                height,
                DockCandidate::Composer { completion, width },
            ),
        }
    }

    fn selector(
        ui: &mut Frontend,
        operation: Option<ActiveOperation>,
        width: usize,
        height: usize,
    ) -> Self {
        let picker = ui.pickers.active().expect("derived picker custody");
        let query = input_window(
            &picker.query,
            picker.query.len(),
            width,
            height.saturating_sub(1).clamp(1, 3),
        );
        let reserve = usize::from(height > query.lines.len());
        let controls = ui.pickers.controls(&ui.keys);
        let status = visible_status(ui, operation);
        let metadata_budget = height.saturating_sub(query.lines.len() + reserve);
        let metadata: Vec<_> = controls
            .into_iter()
            .chain(status)
            .take(metadata_budget)
            .collect();
        let frame = ui.pickers.rows(
            width,
            height.saturating_sub(query.lines.len() + metadata.len()),
            ui.themes.theme(),
        );
        let mut rows = frame.rows;
        for text in metadata {
            rows.push(Line::styled(
                crate::display_text::fit_line(&text, width),
                ui.themes.theme().style(Role::Secondary),
            ));
        }
        let query_top = rows.len();
        rows.extend(
            query
                .lines
                .into_iter()
                .map(|text| Line::styled(text, ui.themes.theme().style(Role::UserInput))),
        );
        let cursor = Some((
            query_top + query.cursor_row,
            query.cursor_col.min(width - 1) as u16,
        ));
        Self {
            body_rows: height.saturating_sub(rows.len()),
            rows,
            cursor,
            paint: DockPaint::new(ui, width, height, DockCandidate::Picker(frame.candidate)),
        }
    }
}
