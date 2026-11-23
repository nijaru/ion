//! One measured interaction dock for both conversation viewports.
use ion_ai::ModelRef;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::interaction::InputTarget;
use super::paint::Prepared;
use super::picker::PickerPaint;
use super::question::QuestionTarget;
use super::{ActiveOperation, Frontend, orientation, status_feedback, visible_status};

use crate::presentation_style::Role;
use crate::terminal_completion::CompletionFrame;
use crate::terminal_input::input_window;

fn activity_border(ui: &Frontend, operation: ActiveOperation, width: usize) -> Line<'static> {
    const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    let spinner = FRAMES[ui.spinner_frame % FRAMES.len()];
    let theme = ui.themes.theme();
    let border = theme.style(Role::Secondary);
    let retry = ui
        .retry_progress
        .as_ref()
        .and_then(|retry| retry.label(tokio::time::Instant::now()));
    let label = retry.as_deref().unwrap_or_else(|| operation.label());
    let mut variants = Vec::new();
    if ui.gestures.prompt().is_none() && ui.clipboard_cancel_target().is_none() {
        let stop = ui.keys.hint(crate::keybindings::KeyAction::Interrupt);
        variants.push(format!("{label} ({stop} to interrupt)"));
        variants.push(format!("{label} ({stop})"));
    }
    variants.push(label.to_owned());
    let spinner = Span::styled(
        spinner.to_string(),
        theme.style(if retry.is_some() {
            Role::Warning
        } else {
            Role::Running
        }),
    );
    for text in variants {
        // Leave at least one rule cell at the right edge.
        let room = width.saturating_sub(7);
        if room == 0 {
            break;
        }
        let (text, clipped) = crate::display_text::fit_line_with_status(&text, room);
        if !clipped {
            let tail = "─".repeat(width - 6 - text.width());
            return Line::from(vec![
                Span::styled("── ", border),
                spinner,
                Span::styled(format!(" {text} "), border),
                Span::styled(tail, border),
            ]);
        }
    }
    let prefix = (width - 1).min(2);
    Line::from(vec![
        Span::styled("─".repeat(prefix), border),
        spinner,
        Span::styled("─".repeat(width - prefix - 1), border),
    ])
}

/// A transient rendering of Host custody, never a second mutable input queue.
fn queue_rows(
    queued: &[ion_app::QueuedInput],
    keys: &crate::keybindings::KeyBindings,
    width: usize,
    budget: usize,
) -> Vec<String> {
    if queued.is_empty() || budget == 0 {
        return Vec::new();
    }
    let waiting = queued.iter().filter(|input| input.withdrawable).count();
    let resolving = queued.len() - waiting;
    let steering = queued
        .iter()
        .filter(|input| input.withdrawable)
        .filter(|input| matches!(input.delivery, ion_app::Delivery::Steer(_)))
        .count();
    let mut summary = if waiting > 0 {
        format!(
            "{}: withdraw queue · {steering} steering · {} follow-up(s)",
            keys.hint(crate::keybindings::KeyAction::Dequeue),
            waiting - steering,
        )
    } else {
        String::new()
    };
    if resolving > 0 {
        let prefix = if summary.is_empty() { "" } else { " · " };
        summary.push_str(&format!("{prefix}{resolving} resolving acceptance"));
    }
    let mut rows = vec![crate::display_text::fit_line(&summary, width)];
    let slots = budget.saturating_sub(1);
    let shown = if queued.len() > slots {
        slots.saturating_sub(1)
    } else {
        queued.len()
    };
    for input in queued.iter().take(shown) {
        let kind = match input.delivery {
            ion_app::Delivery::Steer(_) => "Steering",
            ion_app::Delivery::FollowUp => "Follow-up",
            ion_app::Delivery::Start => unreachable!("Host queue excludes initial prompts"),
        };
        let attachments = if input.attachments == 0 {
            String::new()
        } else {
            format!(" · {} attachment(s)", input.attachments)
        };
        rows.push(crate::display_text::fit_line(
            &format!(
                "{kind}{}: {}{}{attachments}",
                if input.withdrawable {
                    ""
                } else {
                    " (resolving)"
                },
                input.preview,
                if input.truncated { "…" } else { "" }
            ),
            width,
        ));
    }
    if shown < queued.len() && slots > 0 {
        rows.push(crate::display_text::fit_line(
            &format!("… {} more queued", queued.len() - shown),
            width,
        ));
    }
    rows
}

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
    Picker(PickerPaint),
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
        queued: &[ion_app::QueuedInput],
        width: usize,
        height: usize,
    ) -> Self {
        ui.invalidate_interaction_paint();
        let width = width.max(1);
        let height = height.max(1);
        match ui.input_target() {
            InputTarget::Picker => return Self::selector(ui, operation, width, height),
            InputTarget::Question => {
                let prompt = ui.gestures.prompt().map(str::to_owned);
                let question = ui.question.as_mut().expect("derived question custody");
                if question.is_collapsed() {
                    return Self {
                        body_rows: height.saturating_sub(2),
                        rows: vec![Line::styled(
                            crate::display_text::fit_line(
                                prompt
                                    .as_deref()
                                    .unwrap_or(&super::QuestionModal::collapsed_hint(&ui.keys)),
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
                let header = usize::from(prompt.is_some() && budget > 1);
                let mut frame = question.render(
                    width as u16,
                    (budget - header) as u16,
                    &ui.keys,
                    ui.themes.theme(),
                );
                if let Some(prompt) = prompt {
                    let row = Line::styled(
                        crate::display_text::fit_line(&prompt, width),
                        ui.themes.theme().style(Role::Secondary),
                    );
                    if header > 0 {
                        frame.rows.insert(0, row);
                        frame.cursor = frame.cursor.map(|(row, col)| (row + 1, col));
                    } else {
                        frame.rows[0] = row;
                        frame.cursor = None;
                        frame.candidate = None;
                        frame.editor_width = None;
                    }
                }
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
            crate::display_text::push_wrapped(&mut status, &format!("Notice · {notice}"), width);
        }
        let earlier_rows = status.len();
        let activity = operation.filter(|op| !op.is_cancelled());
        let status_value = if activity.is_some() {
            status_feedback(ui)
        } else {
            visible_status(ui, operation)
        };
        if let Some(value) = status_value {
            crate::display_text::push_wrapped(&mut status, &value, width);
        }
        status.extend(queue_rows(queued, &ui.keys, width, (budget / 3).max(1)));
        // Drop an older transcript notice before taking the border away from
        // active work. Current feedback and queued inputs still get first claim.
        let status_budget = budget.saturating_sub(1);
        let indicator_space =
            usize::from(activity.is_some() && status.len() - earlier_rows < status_budget);
        let status_budget = status_budget - indicator_space;
        if status.len() > status_budget {
            // Older notices yield first. Keep the current feedback's prefix
            // (including stop controls) and one caret row; mark omitted source.
            let omitted_earlier = earlier_rows.min(status.len() - status_budget);
            status.drain(..omitted_earlier);
            if status.len() > status_budget {
                status.truncate(status_budget);
                if let Some(last) = status.last_mut() {
                    *last = crate::display_text::fit_line(&format!("{last}…"), width);
                }
            } else if omitted_earlier < earlier_rows {
                let first = status.first_mut().expect("partially retained older notice");
                *first = crate::display_text::fit_line(&format!("…{first}"), width);
            }
        }
        let has_body = operation.is_some()
            || !ui.conversation.mutable_items().is_empty()
            || ui.view.is_reading()
            || ui.mode == super::TuiMode::Fullscreen;
        let body_minimum = usize::from(has_body && budget > status.len() + 1 + indicator_space);
        let discovery_room =
            budget.saturating_sub(1 + status.len() + body_minimum + indicator_space);
        let completion =
            ui.editor
                .completion
                .rows(width, discovery_room.min((budget / 3).max(1)), &ui.keys);
        let editor_room = budget
            .saturating_sub(status.len() + completion.rows.len() + body_minimum + indicator_space)
            .max(1);
        let editor_budget = if has_body {
            editor_room.div_ceil(2)
        } else {
            editor_room
        };
        let composer = input_window(&ui.editor.draft, ui.editor.cursor, width, editor_budget);
        let spare_rows = budget - status.len() - completion.rows.len() - composer.lines.len();
        // Active progress is more useful than passive footer fields, but never
        // takes the caret, feedback, completion, or last live transcript row.
        let indicator = activity.filter(|_| spare_rows > body_minimum);
        let footer = orientation::rows(
            ui,
            model,
            width,
            spare_rows - body_minimum - usize::from(indicator.is_some()),
        );
        let spare_rows = spare_rows - footer.len();
        let rule = (width >= 3 && spare_rows >= 8)
            .then(|| Line::styled("─".repeat(width), ui.themes.theme().style(Role::Secondary)));
        let top = if let Some(operation) = indicator {
            ui.spinner_visible = true;
            Some(activity_border(ui, operation, width))
        } else {
            rule.clone()
        };
        let body_rows = spare_rows - usize::from(top.is_some()) - usize::from(rule.is_some());
        let preview = operation
            .filter(|op| matches!(op.kind, super::activity::OperationKind::Shell))
            .map_or_else(Vec::new, |_| {
                ui.shell_preview.rows(width, body_rows.min(20))
            });
        let body_rows = body_rows - preview.len();
        let mut rows: Vec<_> = preview
            .into_iter()
            .map(|text| Line::styled(text, ui.themes.theme().style(Role::Text)))
            .collect();
        for text in status {
            rows.push(Line::styled(text, ui.themes.theme().style(Role::Secondary)));
        }
        for (index, text) in completion.rows.iter().enumerate() {
            let role = if completion.selected_row == Some(index) {
                Role::Selected
            } else if index == 0 {
                Role::Secondary
            } else {
                Role::Text
            };
            rows.push(Line::styled(text.clone(), ui.themes.theme().style(role)));
        }
        rows.extend(top);
        let composer_top = rows.len();
        for text in composer.lines {
            rows.push(Line::styled(text, ui.themes.theme().style(Role::UserInput)));
        }
        rows.extend(rule);
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
            ui.pickers.query_cursor(),
            width,
            height.saturating_sub(1).clamp(1, 3),
        );
        let confirming = ui.gestures.prompt().is_some();
        let reserve = usize::from(!confirming && height > query.lines.len());
        let controls = ui.pickers.controls(&ui.keys, width);
        let status = visible_status(ui, operation);
        let metadata_budget = height.saturating_sub(query.lines.len() + reserve);
        let metadata: Vec<_> = if confirming {
            status
                .into_iter()
                .chain(controls)
                .take(metadata_budget)
                .collect()
        } else {
            controls
                .into_iter()
                .chain(status)
                .take(metadata_budget)
                .collect()
        };
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
            paint: DockPaint::new(ui, width, height, DockCandidate::Picker(frame.paint)),
        }
    }
}
