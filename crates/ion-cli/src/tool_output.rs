//! Literal observed output disclosure, independent of effect state and model delivery.
use crate::{
    display_text::{Component, Field, MappedLine, Wrapping, fit_mapped, source_lines, wrap_mapped},
    presentation_style::{Role, Theme},
    transcript_render::ThinkingVisibility,
};
use ion_core::{ActivityState, ArtifactRef, ToolActivityKind, TranscriptActivity};
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

#[derive(Clone)]
pub(crate) struct RenderOptions<'a> {
    pub cwd: &'a std::path::Path,
    pub theme: &'a Theme,
    pub detail: OutputDetail,
    pub thinking: ThinkingVisibility,
    expand_hint: Option<String>,
    inspect_command: &'static str,
}

impl<'a> RenderOptions<'a> {
    pub(crate) fn new(
        detail: OutputDetail,
        keys: &crate::keybindings::KeyBindings,
        theme: &'a Theme,
        thinking: ThinkingVisibility,
        cwd: &'a std::path::Path,
    ) -> Self {
        Self {
            cwd,
            theme,
            detail,
            thinking,
            expand_hint: keys.bound_hint(crate::keybindings::KeyAction::ExpandTools),
            inspect_command: "/tools",
        }
    }

    pub(crate) fn with_inspector_hint(mut self, immutable_scrollback: bool) -> Self {
        if immutable_scrollback {
            self.expand_hint = None;
        }
        self
    }

    pub(crate) fn for_user_shell(&self) -> Self {
        Self {
            inspect_command: "/tool all",
            ..self.clone()
        }
    }

    pub(crate) fn details_hint(&self) -> &str {
        self.expand_hint.as_deref().unwrap_or(self.inspect_command)
    }
}

mod observations;
pub(crate) use observations::compact_count as compact_discovery_count;

pub(crate) enum RecordedExit {
    Code(i64),
    Signal(i64),
    Unavailable,
}

impl RecordedExit {
    pub(crate) fn from_value(output: &Value) -> Option<Self> {
        if let Some(signal) = output["signal"].as_i64() {
            Some(Self::Signal(signal))
        } else if let Some(code) = output["exit_code"].as_i64() {
            Some(Self::Code(code))
        } else {
            output["wait_error"].as_str().map(|_| Self::Unavailable)
        }
    }
}

enum Preview {
    Head(usize),
    Tail(usize),
    Bookends(usize),
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
            | ActivityState::Rejected
    ) {
        return;
    }
    let Some(result) = &activity.result else {
        return;
    };
    if activity.activity.kind == ToolActivityKind::Command {
        render_command(
            rows,
            &result.value,
            prefix,
            width,
            detail,
            3,
            activity.state == ActivityState::Completed && !result.is_error,
        );
    }
    if !(detail.detail == OutputDetail::Compact && successful_read(activity))
        && let Some(text) = result.value["content"].as_str()
    {
        note(
            rows,
            prefix,
            &if activity.activity.kind == ToolActivityKind::Read {
                read_label(&result.value)
            } else {
                "content".into()
            },
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
        if activity.activity.kind == ToolActivityKind::Read
            && result.value["start_line"]
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
        if activity.activity.kind == ToolActivityKind::Read
            && result.value["has_more"].as_bool() == Some(true)
        {
            note(
                rows,
                prefix,
                "… file continues beyond this recorded range",
                width,
                detail.theme,
            );
        }
    }
    observations::render(rows, activity, prefix, width, detail);
    observations::notices(rows, &result.value, prefix, width, detail);
    render_artifacts(rows, &result.artifacts, prefix, width, detail);
    if let Some(error) = result.value["error"].as_str().or_else(|| {
        result
            .value
            .pointer("/error/message")
            .and_then(Value::as_str)
    }) {
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

fn successful_read(activity: &TranscriptActivity) -> bool {
    activity.activity.kind == ToolActivityKind::Read
        && activity.state == ActivityState::Completed
        && activity
            .result
            .as_ref()
            .is_some_and(|result| !result.is_error && result.value["content"].is_string())
}

fn read_range(value: &Value) -> Option<String> {
    if let (Some(start), Some(next)) = (value["start_line"].as_u64(), value["next_line"].as_u64()) {
        if start > 0 && next > start {
            return Some(format!("lines {start}–{}", next - 1));
        }
        if start > 0 && next == start {
            return Some(format!("EOF at line {start}"));
        }
    }
    if let (Some(start), Some(next)) = (value["start_byte"].as_u64(), value["next_byte"].as_u64()) {
        if next > start {
            return Some(format!("bytes {start}–{}", next - 1));
        }
        if next == start {
            return Some(format!("EOF at byte {start}"));
        }
    }
    None
}

fn read_label(value: &Value) -> String {
    read_range(value).map_or_else(|| "content".into(), |range| format!("content · {range}"))
}

/// Whole-file reads need no range annotation in compact view. Pagination,
/// byte offsets and EOF still matter even when the payload is hidden.
pub(crate) fn compact_read_range(activity: &TranscriptActivity) -> Option<String> {
    if !successful_read(activity) {
        return None;
    }
    let value = &activity.result.as_ref()?.value;
    let start_line = value["start_line"].as_u64();
    let earlier = start_line.is_some_and(|start| start > 1)
        || value["start_byte"].as_u64().is_some_and(|start| start > 0);
    let following = value["has_more"].as_bool() == Some(true);
    let eof = start_line.is_some() && start_line == value["next_line"].as_u64();
    if !earlier && !following && !eof && value["start_byte"].is_null() {
        return None;
    }
    let mut summary = read_range(value).unwrap_or_else(|| "partial read".into());
    if earlier {
        summary.push_str(" · earlier ");
        summary.push_str(if value["start_byte"].is_u64() {
            "bytes"
        } else {
            "lines"
        });
        summary.push_str(" not recorded");
    }
    if following {
        summary.push_str(" · file continues");
    }
    Some(summary)
}

pub(crate) fn render_folded_summary(
    rows: &mut Vec<MappedLine>,
    activities: &[TranscriptActivity],
    prefix: &str,
    width: usize,
    detail: &RenderOptions<'_>,
) {
    let Some(first) = activities.first() else {
        return;
    };
    note(
        rows,
        prefix,
        &format!(
            "… {} {} outputs folded · {}",
            activities.len(),
            crate::transcript_render::kind_label(first.activity.kind),
            detail.details_hint()
        ),
        width,
        detail.theme,
    );
}

pub(crate) fn render_folded_observation(
    rows: &mut Vec<MappedLine>,
    activity: &TranscriptActivity,
    prefix: &str,
    width: usize,
    detail: &RenderOptions<'_>,
) {
    let Some(result) = &activity.result else {
        return;
    };
    render_artifacts(rows, &result.artifacts, prefix, width, detail);
    let mut notices = Vec::new();
    observations::notices(&mut notices, &result.value, prefix, width, detail);
    let range = compact_read_range(activity);
    if !notices.is_empty() || range.is_some() {
        let subject = crate::transcript_render::activity_subject(activity, detail.cwd)
            .unwrap_or_else(|| activity.name.clone());
        let label = range.map_or_else(|| subject.clone(), |range| format!("{subject}: {range}"));
        note(rows, prefix, &label, width, detail.theme);
        rows.extend(notices);
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
    let failures_first = !successful && detail.detail == OutputDetail::Compact;
    let fields = if failures_first {
        [Field::Stderr, Field::Stdout]
    } else {
        [Field::Stdout, Field::Stderr]
    };
    for field in fields {
        let stream = field.label();
        let incomplete = output[format!("{stream}_complete")].as_bool() == Some(false);
        let truncated = output[format!("{stream}_truncated")].as_bool() == Some(true);
        if let Some(text) = output[stream].as_str().filter(|text| !text.is_empty()) {
            let line_count = text.lines().count();
            let fold_success = match field {
                // Like Pi's collapsed output, keep the ending of a noisy
                // command visible even when the full capture spills.
                Field::Stdout => !truncated || incomplete,
                _ => line_count > compact_lines,
            };
            if successful && detail.detail == OutputDetail::Compact && fold_success {
                let unit = if line_count == 1 { "line" } else { "lines" };
                note(
                    rows,
                    prefix,
                    &format!(
                        "{stream} · {line_count} {unit} folded · {}",
                        detail.details_hint()
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
                    if failures_first && field == Field::Stderr {
                        Preview::Bookends(compact_lines)
                    } else {
                        Preview::Tail(compact_lines)
                    },
                );
            }
        }
        if incomplete {
            note(
                rows,
                prefix,
                &format!("… {stream} capture incomplete · omitted byte count unknown"),
                width,
                detail.theme,
            );
        } else if truncated {
            let notice = match output[format!("{stream}_omitted_bytes")].as_u64() {
                Some(bytes) => format!(
                    "… {stream} truncated · {bytes} raw bytes omitted · {}",
                    detail.details_hint()
                ),
                None => format!("… {stream} truncated · {}", detail.details_hint()),
            };
            note(rows, prefix, &notice, width, detail.theme);
        }
        if let Some(path) = output[format!("{stream}_file")].as_str() {
            let label = if incomplete {
                "Partial output"
            } else {
                "Full output"
            };
            note(
                rows,
                prefix,
                &format!("{label} ({stream}):"),
                width,
                detail.theme,
            );
            note(rows, prefix, path, width, detail.theme);
        }
        if let Some(error) = output[format!("{stream}_file_error")].as_str() {
            note(
                rows,
                prefix,
                &format!("… {stream} output file unavailable: {error}"),
                width,
                detail.theme,
            );
        }
        if output[format!("{stream}_lossy")].as_bool() == Some(true) {
            note(
                rows,
                prefix,
                &format!(
                    "… {stream} decoding loss · UTF-8 replacement used (possibly at a preview boundary)"
                ),
                width,
                detail.theme,
            );
        }
        if let Some(error) = output[format!("{stream}_artifact_error")].as_str() {
            note(
                rows,
                prefix,
                &format!("… {stream} capture error: {error}"),
                width,
                detail.theme,
            );
        }
    }
    if let Some(error) = output["wait_error"].as_str() {
        note(rows, prefix, "exit status unavailable", width, detail.theme);
        render_text(
            rows,
            error,
            prefix,
            Field::WaitError,
            width,
            detail,
            Preview::Head(3),
        );
    }
}

pub(crate) fn artifact_metadata(artifacts: &[ArtifactRef]) -> impl Iterator<Item = String> + '_ {
    artifacts.iter().enumerate().map(|(index, artifact)| {
        format!(
            "Session artifact {index} · {} · {} bytes · {} · id {} · sha256 {}",
            artifact.label,
            artifact.bytes,
            if artifact.complete {
                "complete"
            } else {
                "incomplete"
            },
            artifact.id,
            artifact.sha256,
        )
    })
}

pub(crate) fn render_artifacts(
    rows: &mut Vec<MappedLine>,
    artifacts: &[ArtifactRef],
    prefix: &str,
    width: usize,
    detail: &RenderOptions<'_>,
) {
    match detail.detail {
        OutputDetail::Compact => {
            for (index, artifact) in artifacts.iter().enumerate() {
                let incomplete = if artifact.complete {
                    ""
                } else {
                    " · incomplete"
                };
                let badge = format!(
                    "Artifact {index} · {} · {} bytes{incomplete}",
                    artifact.label, artifact.bytes,
                );
                note(rows, prefix, &badge, width, detail.theme);
            }
        }
        OutputDetail::Expanded => {
            for metadata in artifact_metadata(artifacts) {
                note(rows, prefix, &metadata, width, detail.theme);
            }
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
    let count = text.lines().count();
    let expanded = detail.detail == OutputDetail::Expanded;
    let (head, tail, omission) = match preview {
        Preview::Head(limit) => (limit, 0, "more"),
        Preview::Tail(limit) => (0, limit, "earlier"),
        Preview::Bookends(limit) => (1.min(limit), limit.saturating_sub(1), "middle"),
    };
    let omitted = if expanded {
        0
    } else {
        count.saturating_sub(head + tail)
    };
    let omission_note = || {
        format!(
            "… {omitted} {omission} {label} lines · {}",
            detail.details_hint()
        )
    };
    let continuation = format!("{prefix}  ");
    let mut clipped = false;
    for (index, (byte, text)) in source_lines(text).enumerate() {
        if omitted > 0 && (head..head + omitted).contains(&index) {
            if index == head && head != 0 && tail != 0 {
                note(rows, prefix, &omission_note(), width, detail.theme);
            }
            continue;
        }
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
    if omitted > 0 && (head == 0 || tail == 0) {
        note(rows, prefix, &omission_note(), width, detail.theme);
    } else if clipped && omitted == 0 {
        let notice = format!("… long line shortened · {}", detail.details_hint());
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
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    pub(crate) fn artifact(label: &str, complete: bool) -> ArtifactRef {
        ArtifactRef {
            id: uuid::Uuid::from_u128(42),
            label: label.into(),
            bytes: 1234,
            sha256: "a".repeat(64),
            complete,
        }
    }

    #[test]
    fn command_discloses_decoding_loss_artifacts_and_incomplete_capture_in_both_views() {
        for detail in [OutputDetail::Compact, OutputDetail::Expanded] {
            let mut rows = Vec::new();
            render_command(
                &mut rows,
                &json!({
                    "stdout": "\u{fffd}", "stdout_lossy": true,
                    "stdout_complete": true, "stdout_truncated": false,
                    "stderr": "partial", "stderr_lossy": false,
                    "stderr_complete": false, "stderr_truncated": true,
                    "stderr_artifact_error": "injected failure"
                }),
                "",
                120,
                &RenderOptions::new(
                    detail,
                    &crate::keybindings::KeyBindings::default(),
                    &Theme::default(),
                    ThinkingVisibility::Hidden,
                    std::path::Path::new("/workspace"),
                ),
                3,
                true,
            );
            render_artifacts(
                &mut rows,
                &[artifact("stdout", true), artifact("stderr", false)],
                "",
                200,
                &RenderOptions::new(
                    detail,
                    &crate::keybindings::KeyBindings::default(),
                    &Theme::default(),
                    ThinkingVisibility::Hidden,
                    std::path::Path::new("/workspace"),
                ),
            );
            let text = rows
                .iter()
                .map(|row| row.line.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(text.contains("stdout decoding loss"), "{text}");
            if detail == OutputDetail::Compact {
                assert!(text.contains("Artifact 0 · stdout · 1234 bytes"), "{text}");
                assert!(
                    text.contains("Artifact 1 · stderr · 1234 bytes · incomplete"),
                    "{text}"
                );
                assert!(
                    !text.contains(&uuid::Uuid::from_u128(42).to_string()),
                    "{text}"
                );
                assert!(!text.contains(&"a".repeat(64)), "{text}");
            } else {
                assert!(
                    text.contains("Session artifact 0 · stdout · 1234 bytes · complete"),
                    "{text}"
                );
                assert!(
                    text.contains("Session artifact 1 · stderr · 1234 bytes · incomplete"),
                    "{text}"
                );
                assert!(
                    text.contains(&uuid::Uuid::from_u128(42).to_string()),
                    "{text}"
                );
                assert!(text.contains(&"a".repeat(64)), "{text}");
            }
            assert!(!text.contains("temporary"), "{text}");
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
                    std::path::Path::new("/workspace"),
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
    fn retained_artifact_display_uses_typed_metadata_for_commands_reads_and_external_tools() {
        let keys = crate::keybindings::KeyBindings::default();
        let theme = Theme::default();
        for kind in [
            ToolActivityKind::Command,
            ToolActivityKind::Read,
            ToolActivityKind::External,
        ] {
            let mut activity = TranscriptActivity {
                identity: ion_core::TranscriptActivityIdentity::Tool(ion_core::ToolOccurrence {
                    assistant_entry: 2,
                    ordinal: 0,
                }),
                call_id: "capture".into(),
                name: "capture".into(),
                activity: ion_core::ToolActivity {
                    kind,
                    subject: None,
                },
                arguments: Value::Null,
                state: ActivityState::Completed,
                result: Some(ion_core::ActivityResult {
                    projection: Some(ion_core::ToolResultProjection::Observed),
                    value: json!({"raw_result_artifact": 0, "artifacts": [artifact("OPAQUE_JSON", true)]}),
                    image_mime_types: vec![],
                    artifacts: vec![],
                    is_error: false,
                }),
                children: vec![],
            };
            for mode in [OutputDetail::Compact, OutputDetail::Expanded] {
                let detail = RenderOptions::new(
                    mode,
                    &keys,
                    &theme,
                    ThinkingVisibility::Hidden,
                    std::path::Path::new("/workspace"),
                );
                let mut rows = vec![];
                render_activity(&mut rows, &activity, "", 200, &detail);
                assert!(
                    !rows.iter().any(|row| row
                        .line
                        .to_string()
                        .to_lowercase()
                        .contains("artifact 0"))
                );
                activity.result.as_mut().unwrap().artifacts =
                    vec![artifact("raw_mcp_result\u{1b}", false)];
                render_activity(&mut rows, &activity, "", 200, &detail);
                let text = rows
                    .iter()
                    .map(|row| row.line.to_string())
                    .collect::<Vec<_>>()
                    .join("\n");
                let expected = match detail.detail {
                    OutputDetail::Compact => {
                        "Artifact 0 · raw_mcp_result� · 1234 bytes · incomplete"
                    }
                    OutputDetail::Expanded => {
                        "Session artifact 0 · raw_mcp_result� · 1234 bytes · incomplete"
                    }
                };
                assert!(text.contains(expected), "{text}");
                assert!(
                    !text.contains('\u{1b}') && !text.contains("OPAQUE_JSON"),
                    "{text}"
                );
                activity.result.as_mut().unwrap().artifacts.clear();
            }
        }
    }

    #[test]
    fn successful_compact_commands_fold_stdout_and_long_stderr() {
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
                artifacts: Vec::new(),
                projection: Some(ion_core::ToolResultProjection::Observed),
                value: json!({"stdout": "OBSERVED_STDOUT\n", "stderr": "progress 1\nprogress 2\nprogress 3\nprogress 4\nOBSERVED_STDERR\n"}),
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
                &RenderOptions::new(
                    mode,
                    &keys,
                    &theme,
                    ThinkingVisibility::Hidden,
                    std::path::Path::new("/workspace"),
                ),
            );
            let text = rows
                .iter()
                .map(|row| row.line.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(text.contains("OBSERVED_STDOUT"), !folded, "{text}");
            assert_eq!(text.contains("stdout · 1 line folded"), folded, "{text}");
            assert_eq!(text.contains("stderr · 5 lines folded"), folded, "{text}");
            assert_eq!(text.contains("OBSERVED_STDERR"), !folded, "{text}");
        }
        activity.state = ActivityState::Completed;
        let result = activity.result.as_mut().unwrap();
        result.is_error = false;
        result.value["stderr"] = json!("SHORT_WARNING\n");
        let mut rows = vec![];
        render_activity(
            &mut rows,
            &activity,
            "",
            100,
            &RenderOptions::new(
                OutputDetail::Compact,
                &keys,
                &theme,
                ThinkingVisibility::Hidden,
                std::path::Path::new("/workspace"),
            ),
        );
        assert!(
            rows.iter()
                .any(|row| row.line.to_string().contains("SHORT_WARNING"))
        );
    }

    #[test]
    fn successful_truncated_stdout_keeps_a_bounded_tail_in_compact_output() {
        let keys = crate::keybindings::KeyBindings::default();
        let theme = Theme::default();
        let detail = RenderOptions::new(
            OutputDetail::Compact,
            &keys,
            &theme,
            ThinkingVisibility::Hidden,
            std::path::Path::new("/workspace"),
        );
        for complete in [true, false] {
            let output = json!({
                "stdout": "BEGIN\nprogress 1\nprogress 2\nprogress 3\nPASSED\n",
                "stdout_truncated": true,
                "stdout_complete": complete,
                "stdout_omitted_bytes": 5000,
                "stdout_file": "/tmp/capture",
            });
            let mut rows = vec![];
            render_command(&mut rows, &output, "", 100, &detail, 3, true);
            let text = rows
                .iter()
                .map(|row| row.line.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(text.contains("PASSED"), complete, "{text}");
            assert!(
                !text.contains("BEGIN") && !text.contains("progress 1"),
                "{text}"
            );
            let disclosure = if complete {
                "Full output"
            } else {
                "Partial output"
            };
            assert!(
                text.contains(disclosure) && text.contains("/tmp/capture"),
                "{text}"
            );
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
                    artifacts: Vec::new(),
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
                    &RenderOptions::new(
                        mode,
                        &keys,
                        &theme,
                        ThinkingVisibility::Hidden,
                        std::path::Path::new("/workspace"),
                    ),
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
    fn rejected_calls_disclose_the_durable_reason_in_both_output_modes() {
        let outcome = ion_core::ToolOutcome::NotDispatched {
            reason: "Operator refused this call — café 🦀".into(),
        };
        let output = outcome.inspection_output();
        let keys = crate::keybindings::KeyBindings::default();
        let theme = Theme::default();
        for kind in [
            ToolActivityKind::Read,
            ToolActivityKind::Write,
            ToolActivityKind::Command,
            ToolActivityKind::External,
        ] {
            let activity = TranscriptActivity {
                identity: ion_core::TranscriptActivityIdentity::Tool(ion_core::ToolOccurrence {
                    assistant_entry: 1,
                    ordinal: 0,
                }),
                call_id: "refused".into(),
                name: "refused".into(),
                activity: ion_core::ToolActivity {
                    kind,
                    subject: None,
                },
                arguments: json!({}),
                state: ActivityState::Rejected,
                result: Some(ion_core::ActivityResult {
                    artifacts: Vec::new(),
                    projection: outcome.projection(),
                    value: output.value.clone(),
                    image_mime_types: Vec::new(),
                    is_error: output.is_error,
                }),
                children: Vec::new(),
            };
            for mode in [OutputDetail::Compact, OutputDetail::Expanded] {
                let mut rows = Vec::new();
                render_activity(
                    &mut rows,
                    &activity,
                    "",
                    120,
                    &RenderOptions::new(
                        mode,
                        &keys,
                        &theme,
                        ThinkingVisibility::Hidden,
                        std::path::Path::new("/workspace"),
                    ),
                );
                let text = rows
                    .iter()
                    .map(|row| row.line.to_string())
                    .collect::<Vec<_>>()
                    .join("\n");
                assert!(
                    text.contains(
                        "Tool call was not dispatched: Operator refused this call — café 🦀"
                    ),
                    "{kind:?}: {text}"
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
    fn wait_failures_keep_their_own_source_and_expansion() {
        let keys = crate::keybindings::KeyBindings::default();
        let theme = Theme::default();
        for mode in [OutputDetail::Compact, OutputDetail::Expanded] {
            for width in [20, 100] {
                let mut rows = Vec::new();
                render_command(
                    &mut rows,
                    &json!({"wait_error": "wait 🦀\r\n\u{1b}[2Jmore\nlast preview\nOMITTED_WAIT_DETAIL"}),
                    "",
                    width,
                    &RenderOptions::new(
                        mode,
                        &keys,
                        &theme,
                        ThinkingVisibility::Hidden,
                        std::path::Path::new("/workspace"),
                    ),
                    3,
                    false,
                );
                assert!(rows.iter().all(|row| row.line.width() <= width));
                let text = rows
                    .iter()
                    .map(|row| row.line.to_string())
                    .collect::<Vec<_>>()
                    .join("\n");
                assert!(!text.contains(['\r', '\u{1b}']));
                assert!(rows.iter().flat_map(|row| &row.map).any(|mapping| matches!(
                    mapping.origin,
                    crate::display_text::Origin::Component {
                        component: Component::Field(Field::WaitError),
                        ..
                    }
                )));
                if width == 100 {
                    assert!(text.contains("exit status unavailable"));
                    assert_eq!(
                        text.contains("OMITTED_WAIT_DETAIL"),
                        mode == OutputDetail::Expanded
                    );
                    assert_eq!(
                        text.contains("1 more wait error lines"),
                        mode == OutputDetail::Compact
                    );
                }
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
        assert_eq!(
            read_label(&json!({"start_byte":0,"next_byte":5})),
            "content · bytes 0–4"
        );
        assert_eq!(
            read_label(&json!({"start_byte":12,"next_byte":12})),
            "content · EOF at byte 12"
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
