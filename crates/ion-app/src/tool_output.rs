//! Literal observed output disclosure, independent of effect state and model delivery.
use crate::presentation_style::{Role, style};
use ion_core::{ActivityState, ToolActivityKind, TranscriptActivity};
use ratatui::text::Line;
use serde_json::Value;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum OutputDetail {
    #[default]
    Compact,
    Expanded,
}

impl OutputDetail {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Expanded => "expanded",
        }
    }
}

enum Preview {
    Head(usize),
    Tail(usize),
}

pub(crate) fn render_activity(
    rows: &mut Vec<Line<'static>>,
    activity: &TranscriptActivity,
    prefix: &str,
    width: usize,
    detail: OutputDetail,
) {
    if !matches!(
        activity.state,
        ActivityState::Completed
            | ActivityState::Failed
            | ActivityState::Cancelled
            | ActivityState::TimedOut
    ) {
        return;
    }
    let Some(result) = &activity.result else {
        return;
    };
    match activity.activity.kind {
        ToolActivityKind::Command => render_command(rows, &result.value, prefix, width, detail, 3),
        ToolActivityKind::Read => {
            if let Some(text) = result.value["content"].as_str() {
                note(rows, prefix, &read_label(&result.value), width);
                render_text(
                    rows,
                    text,
                    prefix,
                    "content",
                    width,
                    detail,
                    Preview::Head(3),
                );
                if result.value["offset"]
                    .as_u64()
                    .is_some_and(|offset| offset > 0)
                {
                    note(
                        rows,
                        prefix,
                        "… earlier file bytes are outside this recorded range",
                        width,
                    );
                }
                if result.value["has_more"].as_bool() == Some(true) {
                    note(
                        rows,
                        prefix,
                        "… file continues beyond this recorded range",
                        width,
                    );
                }
            }
        }
        _ => return,
    }
    if let Some(error) = result.value["error"].as_str() {
        note(rows, prefix, "error", width);
        render_text(
            rows,
            error,
            prefix,
            "error",
            width,
            detail,
            Preview::Head(3),
        );
    }
}

fn read_label(value: &Value) -> String {
    match (
        value["offset"].as_u64(),
        value["next_offset"].as_u64(),
        value["file_bytes"].as_u64(),
    ) {
        (Some(start), Some(end), Some(total)) => {
            format!("content · bytes {start}..{end} of {total}")
        }
        _ => "content".into(),
    }
}

pub(crate) fn render_folded_reads(
    rows: &mut Vec<Line<'static>>,
    activities: &[TranscriptActivity],
    prefix: &str,
    width: usize,
) {
    if activities
        .first()
        .is_none_or(|activity| activity.activity.kind != ToolActivityKind::Read)
    {
        return;
    }
    note(
        rows,
        prefix,
        &format!(
            "… {} read outputs folded · /settings expanded · Ctrl-O",
            activities.len()
        ),
        width,
    );
    for activity in activities {
        let Some(result) = &activity.result else {
            continue;
        };
        let preceding = result.value["offset"]
            .as_u64()
            .is_some_and(|start| start > 0);
        let following = result.value["has_more"].as_bool() == Some(true);
        if preceding || following {
            let subject = activity
                .activity
                .subject
                .as_deref()
                .unwrap_or(&activity.name);
            note(
                rows,
                prefix,
                &format!(
                    "{subject}: {} · {}{}",
                    read_label(&result.value),
                    if preceding {
                        "earlier bytes not recorded"
                    } else {
                        ""
                    },
                    if following {
                        if preceding {
                            "; file continues"
                        } else {
                            "file continues"
                        }
                    } else {
                        ""
                    }
                ),
                width,
            );
        }
    }
}

pub(crate) fn render_command(
    rows: &mut Vec<Line<'static>>,
    output: &Value,
    prefix: &str,
    width: usize,
    detail: OutputDetail,
    compact_lines: usize,
) {
    for stream in ["stdout", "stderr"] {
        if let Some(text) = output[stream].as_str().filter(|text| !text.is_empty()) {
            note(rows, prefix, stream, width);
            render_text(
                rows,
                text,
                prefix,
                stream,
                width,
                detail,
                Preview::Tail(compact_lines),
            );
        }
        if output[format!("{stream}_truncated")].as_bool() == Some(true) {
            let notice = output[format!("{stream}_full_path")].as_str().map_or_else(
                || format!("… {stream} truncated/incomplete · Ctrl-O"),
                |path| format!("… {stream} truncated/incomplete · capture: {path} · Ctrl-O"),
            );
            note(rows, prefix, &notice, width);
        }
    }
}

fn render_text(
    rows: &mut Vec<Line<'static>>,
    text: &str,
    prefix: &str,
    label: &str,
    width: usize,
    detail: OutputDetail,
    preview: Preview,
) {
    let (compact_lines, tail) = match preview {
        Preview::Head(limit) => (limit, false),
        Preview::Tail(limit) => (limit, true),
    };
    let count = text.lines().count();
    if count == 0 {
        return;
    }
    let expanded = detail == OutputDetail::Expanded;
    let skip = if !expanded && tail {
        count.saturating_sub(compact_lines)
    } else {
        0
    };
    let limit = if expanded { usize::MAX } else { compact_lines };
    let continuation = format!("{prefix}  ");
    let mut clipped = false;
    for text in text.lines().skip(skip).take(limit) {
        let visible = if expanded {
            text.to_owned()
        } else {
            let (fitted, shortened) = crate::display_text::fit_line_with_status(
                text,
                width
                    .saturating_sub(unicode_width::UnicodeWidthStr::width(continuation.as_str()))
                    .max(1),
            );
            clipped |= shortened;
            fitted
        };
        crate::display_text::push_styled(
            rows,
            &continuation,
            &continuation,
            Line::raw(visible),
            width,
        );
    }
    if !expanded && (count > compact_lines || clipped) {
        let omitted = count.saturating_sub(compact_lines);
        let notice = if omitted > 0 {
            format!(
                "… {omitted} {} {label} lines · /settings expanded · Ctrl-O",
                if tail { "earlier" } else { "more" }
            )
        } else {
            "… long line shortened · /settings expanded · Ctrl-O".into()
        };
        note(rows, prefix, &notice, width);
    }
}

fn note(rows: &mut Vec<Line<'static>>, prefix: &str, text: &str, width: usize) {
    crate::display_text::push_styled(
        rows,
        &format!("{prefix}└ "),
        &format!("{prefix}  "),
        Line::styled(text.to_owned(), style(Role::Secondary)),
        width,
    );
}
