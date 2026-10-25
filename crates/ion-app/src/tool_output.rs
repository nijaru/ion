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

pub(crate) struct OutputOptions {
    pub detail: OutputDetail,
    pub details_key: String,
}

impl OutputOptions {
    pub(crate) fn new(detail: OutputDetail, keys: &crate::keybindings::KeyBindings) -> Self {
        Self {
            detail,
            details_key: keys.hint(crate::keybindings::KeyAction::Detail),
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
    detail: &OutputOptions,
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
                if result.value["start_line"]
                    .as_u64()
                    .is_some_and(|line| line > 1)
                {
                    note(
                        rows,
                        prefix,
                        "… earlier file lines are outside this recorded range",
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
    match (value["start_line"].as_u64(), value["next_line"].as_u64()) {
        (Some(start), Some(next)) if start > 0 && next > start => {
            format!("content · lines {start}–{}", next - 1)
        }
        (Some(start), Some(next)) if start > 0 && next == start => {
            format!("content · EOF at line {start}")
        }
        _ => "content".into(),
    }
}

pub(crate) fn render_folded_reads(
    rows: &mut Vec<Line<'static>>,
    activities: &[TranscriptActivity],
    prefix: &str,
    width: usize,
    detail: &OutputOptions,
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
            "… {} read outputs folded · /settings expanded · {}",
            activities.len(),
            detail.details_key
        ),
        width,
    );
    for activity in activities {
        let Some(result) = &activity.result else {
            continue;
        };
        let preceding = result.value["start_line"]
            .as_u64()
            .is_some_and(|start| start > 1);
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
                        "earlier lines not recorded"
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
    detail: &OutputOptions,
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
        if output[format!("{stream}_complete")].as_bool() == Some(false) {
            note(
                rows,
                prefix,
                &format!("… {stream} capture incomplete · omitted byte count unknown"),
                width,
            );
        } else if output[format!("{stream}_truncated")].as_bool() == Some(true) {
            let notice = match output[format!("{stream}_omitted_bytes")].as_u64() {
                Some(bytes) => format!(
                    "… {stream} truncated · {bytes} raw bytes omitted · {}",
                    detail.details_key
                ),
                None => format!("… {stream} truncated · {}", detail.details_key),
            };
            note(rows, prefix, &notice, width);
        }
        if output[format!("{stream}_lossy")].as_bool() == Some(true) {
            note(
                rows,
                prefix,
                &format!(
                    "… {stream} decoding loss · UTF-8 replacement used (possibly at tail boundary)"
                ),
                width,
            );
        }
        if let Some(path) = output[format!("{stream}_full_path")].as_str() {
            note(
                rows,
                prefix,
                &format!("… {stream} temporary raw capture: {path}"),
                width,
            );
        }
        if let Some(error) = output[format!("{stream}_full_error")].as_str() {
            note(
                rows,
                prefix,
                &format!("… {stream} capture error: {error}"),
                width,
            );
        }
    }
}

fn render_text(
    rows: &mut Vec<Line<'static>>,
    text: &str,
    prefix: &str,
    label: &str,
    width: usize,
    detail: &OutputOptions,
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
    let expanded = detail.detail == OutputDetail::Expanded;
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
                "… {omitted} {} {label} lines · /settings expanded · {}",
                if tail { "earlier" } else { "more" },
                detail.details_key
            )
        } else {
            format!(
                "… long line shortened · /settings expanded · {}",
                detail.details_key
            )
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn command_discloses_decoding_loss_artifacts_and_incomplete_capture_in_both_views() {
        for detail in [OutputDetail::Compact, OutputDetail::Expanded] {
            let mut rows = Vec::new();
            render_command(
                &mut rows,
                &json!({
                    "stdout": "\u{fffd}", "stdout_lossy": true,
                    "stdout_complete": true, "stdout_truncated": false,
                    "stdout_full_path": "/tmp/raw",
                    "stderr": "partial", "stderr_lossy": false,
                    "stderr_complete": false, "stderr_truncated": true,
                    "stderr_full_error": "injected failure"
                }),
                "",
                120,
                &OutputOptions::new(detail, &crate::keybindings::KeyBindings::default()),
                3,
            );
            let text = rows
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(text.contains("stdout decoding loss"), "{text}");
            assert!(text.contains("temporary raw capture: /tmp/raw"), "{text}");
            assert!(text.contains("stderr capture incomplete"), "{text}");
            assert!(text.contains("omitted byte count unknown"), "{text}");
            assert!(
                text.contains("stderr capture error: injected failure"),
                "{text}"
            );
            assert!(!text.contains("stdout truncated"), "{text}");
            assert!(!text.contains("stderr decoding loss"), "{text}");

            rows.clear();
            render_command(
                &mut rows,
                &json!({"stdout": "\u{fffd}", "stdout_lossy": false, "stdout_complete": true}),
                "",
                120,
                &OutputOptions::new(detail, &crate::keybindings::KeyBindings::default()),
                3,
            );
            assert!(
                !rows
                    .iter()
                    .any(|row| row.to_string().contains("decoding loss"))
            );
        }
    }

    #[test]
    fn read_labels_do_not_reinterpret_byte_or_unstructured_results_as_lines() {
        assert_eq!(
            read_label(&json!({"start_line":2,"next_line":4})),
            "content · lines 2–3"
        );
        assert_eq!(
            read_label(&json!({"start_line":4,"next_line":4})),
            "content · EOF at line 4"
        );
        for value in [
            json!({"offset":64,"next_offset":100}),
            json!({"content":"image"}),
            json!({"start_line":0,"next_line":2}),
        ] {
            assert_eq!(read_label(&value), "content");
        }
    }
}
