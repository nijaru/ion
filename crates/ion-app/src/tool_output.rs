//! Literal observed output disclosure, independent of effect state and model delivery.
use crate::{
    display_text::{Component, Field, MappedLine, Wrapping, fit_mapped, source_lines, wrap_mapped},
    presentation_style::{Role, Theme},
    transcript_render::ThinkingVisibility,
};
use ion_core::{ActivityState, ToolActivityKind, TranscriptActivity};
use ratatui::text::{Line, Span};
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

pub(crate) struct RenderOptions<'a> {
    pub theme: &'a Theme,
    pub detail: OutputDetail,
    pub thinking: ThinkingVisibility,
    pub details_key: String,
}

impl<'a> RenderOptions<'a> {
    pub(crate) fn new(
        detail: OutputDetail,
        keys: &crate::keybindings::KeyBindings,
        theme: &'a Theme,
        thinking: ThinkingVisibility,
    ) -> Self {
        Self {
            theme,
            detail,
            thinking,
            details_key: keys.hint(crate::keybindings::KeyAction::Detail),
        }
    }
}

enum Preview {
    Head(usize),
    Tail(usize),
}

pub(crate) fn render_activity(
    rows: &mut Vec<MappedLine>,
    activity: &TranscriptActivity,
    prefix: &str,
    width: usize,
    detail: &RenderOptions<'_>,
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
        ToolActivityKind::Command => render_command(
            rows,
            &result.value,
            prefix,
            width,
            detail,
            3,
            activity.state == ActivityState::Completed && !result.is_error,
        ),
        ToolActivityKind::Read => {
            if let Some(text) = result.value["content"].as_str() {
                note(
                    rows,
                    prefix,
                    &read_label(&result.value),
                    width,
                    detail.theme,
                );
                render_text(
                    rows,
                    text,
                    prefix,
                    Field::Content,
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
                        detail.theme,
                    );
                }
                if result.value["has_more"].as_bool() == Some(true) {
                    note(
                        rows,
                        prefix,
                        "… file continues beyond this recorded range",
                        width,
                        detail.theme,
                    );
                }
            }
        }
        _ => {}
    }
    if let Some(error) = result.value["error"].as_str() {
        note(rows, prefix, "error", width, detail.theme);
        render_text(
            rows,
            error,
            prefix,
            Field::Error,
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
    rows: &mut Vec<MappedLine>,
    activities: &[TranscriptActivity],
    prefix: &str,
    width: usize,
    detail: &RenderOptions<'_>,
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
        detail.theme,
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
                detail.theme,
            );
        }
    }
}

pub(crate) fn render_command(
    rows: &mut Vec<MappedLine>,
    output: &Value,
    prefix: &str,
    width: usize,
    detail: &RenderOptions<'_>,
    compact_lines: usize,
    successful: bool,
) {
    for field in [Field::Stdout, Field::Stderr] {
        let stream = field.label();
        if let Some(text) = output[stream].as_str().filter(|text| !text.is_empty()) {
            if successful && stream == "stdout" && detail.detail == OutputDetail::Compact {
                note(
                    rows,
                    prefix,
                    &format!(
                        "stdout · {} {} folded · {}",
                        text.lines().count(),
                        if text.lines().count() == 1 {
                            "line"
                        } else {
                            "lines"
                        },
                        detail.details_key
                    ),
                    width,
                    detail.theme,
                );
            } else {
                note(rows, prefix, stream, width, detail.theme);
                render_text(
                    rows,
                    text,
                    prefix,
                    field,
                    width,
                    detail,
                    Preview::Tail(compact_lines),
                );
            }
        }
        if output[format!("{stream}_complete")].as_bool() == Some(false) {
            note(
                rows,
                prefix,
                &format!("… {stream} capture incomplete · omitted byte count unknown"),
                width,
                detail.theme,
            );
        } else if output[format!("{stream}_truncated")].as_bool() == Some(true) {
            let notice = match output[format!("{stream}_omitted_bytes")].as_u64() {
                Some(bytes) => format!(
                    "… {stream} truncated · {bytes} raw bytes omitted · {}",
                    detail.details_key
                ),
                None => format!("… {stream} truncated · {}", detail.details_key),
            };
            note(rows, prefix, &notice, width, detail.theme);
        }
        if output[format!("{stream}_lossy")].as_bool() == Some(true) {
            note(
                rows,
                prefix,
                &format!(
                    "… {stream} decoding loss · UTF-8 replacement used (possibly at tail boundary)"
                ),
                width,
                detail.theme,
            );
        }
        if let Some(path) = output[format!("{stream}_full_path")].as_str() {
            note(
                rows,
                prefix,
                &format!("… {stream} temporary raw capture: {path}"),
                width,
                detail.theme,
            );
        }
        if let Some(error) = output[format!("{stream}_full_error")].as_str() {
            note(
                rows,
                prefix,
                &format!("… {stream} capture error: {error}"),
                width,
                detail.theme,
            );
        }
    }
}

fn render_text(
    rows: &mut Vec<MappedLine>,
    text: &str,
    prefix: &str,
    field: Field,
    width: usize,
    detail: &RenderOptions<'_>,
    preview: Preview,
) {
    let label = field.label();
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
    for (byte, text) in source_lines(text).skip(skip).take(limit) {
        let source = MappedLine::component(
            Line::from(Span::raw(text.to_owned())),
            Component::Field(field),
            byte,
        );
        let visible = if expanded {
            source
        } else {
            let (fitted, shortened) = fit_mapped(
                source,
                width
                    .saturating_sub(unicode_width::UnicodeWidthStr::width(continuation.as_str()))
                    .max(1),
            );
            clipped |= shortened;
            fitted
        };
        rows.extend(wrap_mapped(
            &continuation,
            &continuation,
            visible,
            width,
            Wrapping::Literal,
        ));
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
        note(rows, prefix, &notice, width, detail.theme);
    }
}

fn note(rows: &mut Vec<MappedLine>, prefix: &str, text: &str, width: usize, theme: &Theme) {
    rows.extend(wrap_mapped(
        &format!("{prefix}└ "),
        &format!("{prefix}  "),
        MappedLine::generated(
            Line::styled(text.to_owned(), theme.style(Role::Secondary)),
            0,
        ),
        width,
        Wrapping::Literal,
    ));
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
                &RenderOptions::new(
                    detail,
                    &crate::keybindings::KeyBindings::default(),
                    &Theme::default(),
                    ThinkingVisibility::Hidden,
                ),
                3,
                true,
            );
            let text = rows
                .iter()
                .map(|row| row.line.to_string())
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
                &RenderOptions::new(
                    detail,
                    &crate::keybindings::KeyBindings::default(),
                    &Theme::default(),
                    ThinkingVisibility::Hidden,
                ),
                3,
                true,
            );
            assert!(
                !rows
                    .iter()
                    .any(|row| row.line.to_string().contains("decoding loss"))
            );
        }
    }

    #[test]
    fn only_successful_compact_commands_fold_stdout_and_never_stderr() {
        let mut activity = TranscriptActivity {
            identity: ion_core::TranscriptActivityIdentity::Tool(ion_core::ToolOccurrence {
                assistant_entry: 2,
                ordinal: 0,
            }),
            call_id: "command".into(),
            name: "exec".into(),
            activity: ion_core::ToolActivity {
                kind: ToolActivityKind::Command,
                subject: Some("probe".into()),
            },
            arguments: json!({"command": "probe"}),
            state: ActivityState::Completed,
            result: Some(ion_core::ActivityResult {
                projection: Some(ion_core::ToolResultProjection::Observed),
                value: json!({"stdout": "OBSERVED_STDOUT\n", "stderr": "OBSERVED_STDERR\n"}),
                image_mime_types: vec![],
                is_error: false,
            }),
            children: vec![],
        };
        let keys = crate::keybindings::KeyBindings::default();
        let theme = Theme::default();
        for (state, mode, folded) in [
            (ActivityState::Completed, OutputDetail::Compact, true),
            (ActivityState::Completed, OutputDetail::Expanded, false),
            (ActivityState::Failed, OutputDetail::Compact, false),
            (ActivityState::TimedOut, OutputDetail::Compact, false),
        ] {
            activity.state = state;
            activity.result.as_mut().unwrap().is_error = state != ActivityState::Completed;
            let mut rows = vec![];
            render_activity(
                &mut rows,
                &activity,
                "",
                100,
                &RenderOptions::new(mode, &keys, &theme, ThinkingVisibility::Hidden),
            );
            let text = rows
                .iter()
                .map(|row| row.line.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(text.contains("OBSERVED_STDOUT"), !folded, "{text}");
            assert_eq!(text.contains("stdout · 1 line folded"), folded, "{text}");
            assert!(text.contains("OBSERVED_STDERR"), "{text}");
        }
    }

    #[test]
    fn failed_mutations_show_the_generic_error_and_its_full_cause() {
        let cause = "write failed\ncaused by: permission denied (os error 13)";
        let keys = crate::keybindings::KeyBindings::default();
        let theme = Theme::default();
        for kind in [ToolActivityKind::Write, ToolActivityKind::Edit] {
            let activity = TranscriptActivity {
                identity: ion_core::TranscriptActivityIdentity::Tool(ion_core::ToolOccurrence {
                    assistant_entry: 1,
                    ordinal: 0,
                }),
                call_id: "same".into(),
                name: "mutation".into(),
                activity: ion_core::ToolActivity {
                    kind,
                    subject: Some("file".into()),
                },
                arguments: Value::Null,
                state: ActivityState::Failed,
                result: Some(ion_core::ActivityResult {
                    projection: Some(ion_core::ToolResultProjection::Observed),
                    value: json!({"error":cause}),
                    image_mime_types: vec![],
                    is_error: true,
                }),
                children: vec![],
            };
            for mode in [OutputDetail::Compact, OutputDetail::Expanded] {
                let mut rows = Vec::new();
                render_activity(
                    &mut rows,
                    &activity,
                    "",
                    100,
                    &RenderOptions::new(mode, &keys, &theme, ThinkingVisibility::Hidden),
                );
                let rendered = rows
                    .iter()
                    .map(|row| row.line.to_string())
                    .collect::<Vec<_>>()
                    .join("\n");
                assert!(rendered.contains("write failed"), "{kind:?}: {rendered}");
                assert!(
                    rendered.contains("caused by: permission denied (os error 13)"),
                    "{kind:?}: {rendered}"
                );
                assert!(rows.iter().flat_map(|row| &row.map).any(|mapping| matches!(
                    mapping.origin,
                    crate::display_text::Origin::Component {
                        component: Component::Field(Field::Error),
                        ..
                    }
                )));
            }
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
