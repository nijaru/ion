//! Presentation of a recorded edit or write observation; patch syntax never determines effect state.
use crate::presentation_style::Role;
use ion_core::{ActivityState, ToolActivityKind, TranscriptActivity};
use ratatui::text::{Line, Span};
use serde_json::{Map, Value};
use unicode_width::UnicodeWidthStr;

pub(crate) struct RecordedDiff<'a> {
    pub fields: &'a Map<String, Value>,
    pub text: &'a str,
    pub truncated: bool,
}

pub(crate) fn recorded(activity: &TranscriptActivity) -> Option<RecordedDiff<'_>> {
    if !matches!(
        activity.activity.kind,
        ToolActivityKind::Edit | ToolActivityKind::Write
    ) || activity.state != ActivityState::Completed
    {
        return None;
    }
    let result = activity.result.as_ref().filter(|result| !result.is_error)?;
    let fields = result.value.as_object()?;
    let diff = fields.get("diff")?;
    Some(RecordedDiff {
        fields,
        text: diff.get("text")?.as_str()?,
        truncated: diff.get("truncated")?.as_bool()?,
    })
}

pub(crate) fn render(
    rows: &mut Vec<Line<'static>>,
    activity: &TranscriptActivity,
    prefix: &str,
    width: usize,
    detail: &crate::tool_output::RenderOptions<'_>,
) {
    let Some(diff) = recorded(activity) else {
        return;
    };
    const PREVIEW_LINES: usize = 8;
    let expanded = detail.detail == crate::tool_output::OutputDetail::Expanded;
    let mut source = diff.text.lines().skip(if expanded { 0 } else { 2 });
    let lines = source
        .by_ref()
        .take(if expanded { usize::MAX } else { PREVIEW_LINES })
        .collect::<Vec<_>>();
    let more = source.next().is_some();
    let footer = if diff.truncated {
        Some(format!("… diff capture truncated · {}", detail.details_key))
    } else if more {
        Some(format!("… more diff · {}", detail.details_key))
    } else if lines.is_empty() {
        Some("No text changes".into())
    } else {
        None
    };
    for (index, text) in lines.iter().enumerate() {
        let last = index + 1 == lines.len() && footer.is_none();
        // These are syntax styles only. The Completed state above comes from Core facts.
        let style = match text.as_bytes().first() {
            Some(b'+') => detail.theme.style(Role::Added),
            Some(b'-') => detail.theme.style(Role::Removed),
            Some(b'@') => detail.theme.style(Role::Hunk),
            _ => detail.theme.style(Role::Secondary),
        };
        // Bound physical preview rows; long source lines remain in inspection.
        let visible = if expanded {
            (*text).to_owned()
        } else {
            crate::display_text::fit_line(text, width.saturating_sub(prefix.width() + 2).max(1))
        };
        crate::display_text::push_styled(
            rows,
            &format!("{prefix}{}", if last { "└ " } else { "│ " }),
            &format!("{prefix}{}", if last { "  " } else { "│ " }),
            Line::from(Span::styled(visible, style)),
            width,
        );
    }
    if let Some(footer) = footer {
        crate::display_text::push_styled(
            rows,
            &format!("{prefix}└ "),
            &format!("{prefix}  "),
            Line::styled(
                crate::display_text::fit_line(
                    &footer,
                    width.saturating_sub(prefix.width() + 2).max(1),
                ),
                detail.theme.style(Role::Secondary),
            ),
            width,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ion_core::{
        ActivityGroup, ActivityResult, ToolActivity, ToolResultProjection, TranscriptItem,
        TranscriptProjection,
    };
    use serde_json::json;

    #[test]
    fn write_activity_uses_the_same_recorded_diff_presentation() {
        let mut activity = TranscriptActivity {
            call_id: "write-1".into(),
            name: "write".into(),
            activity: ToolActivity {
                kind: ToolActivityKind::Write,
                subject: Some("file".into()),
            },
            arguments: json!({"path":"file"}),
            state: ActivityState::Completed,
            result: Some(ActivityResult {
                projection: Some(ToolResultProjection::Observed),
                value: json!({"path":"file", "diff":{"text":"--- file\n+++ file\n@@ -1 +1 @@\n-old\n+new\n","truncated":false}}),
                image_mime_types: vec![],
                is_error: false,
            }),
            children: vec![],
        };
        let mut rows = Vec::new();
        render(
            &mut rows,
            &activity,
            "  ",
            80,
            &crate::tool_output::RenderOptions::new(
                crate::tool_output::OutputDetail::Compact,
                &crate::keybindings::KeyBindings::default(),
                &crate::presentation_style::Theme::default(),
                crate::transcript_render::ThinkingVisibility::Hidden,
            ),
        );
        assert!(rows.iter().any(|row| row.to_string().contains("+new")));
        activity.activity.kind = ToolActivityKind::Read;
        assert!(recorded(&activity).is_none());
    }

    #[test]
    fn preview_bounds_physical_rows_and_inspection_retains_the_recorded_patch() {
        let text = format!(
            "--- old\n+++ new\n@@ -1 +1 @@\n-{}\n+x\u{1b}[2J\n",
            "\u{200b}".repeat(500)
        );
        let mut activity = TranscriptActivity {
            call_id: "edit-1".into(),
            name: "edit".into(),
            activity: ToolActivity {
                kind: ToolActivityKind::Edit,
                subject: Some("file".into()),
            },
            arguments: json!({"path":"file"}),
            state: ActivityState::Completed,
            result: Some(ActivityResult {
                projection: Some(ToolResultProjection::Observed),
                value: json!({"path":"file", "diff":{"text":text,"truncated":true}}),
                image_mime_types: vec![],
                is_error: false,
            }),
            children: vec![],
        };
        for width in 1..=40 {
            let mut rows = vec![];
            render(
                &mut rows,
                &activity,
                "│ ",
                width,
                &crate::tool_output::RenderOptions::new(
                    crate::tool_output::OutputDetail::Compact,
                    &crate::keybindings::KeyBindings::default(),
                    &crate::presentation_style::Theme::default(),
                    crate::transcript_render::ThinkingVisibility::Hidden,
                ),
            );
            assert!(rows.len() <= 9, "width {width}: {} rows", rows.len());
            assert!(
                rows.iter()
                    .all(|row| row.width() <= width && !row.to_string().contains('\u{1b}'))
            );
            assert!(rows.iter().any(|row| {
                row.spans.iter().any(|span| {
                    span.style.fg
                        == crate::presentation_style::Theme::default()
                            .style(Role::Removed)
                            .fg
                })
            }));
            assert!(rows.iter().any(|row| {
                row.spans.iter().any(|span| {
                    span.style.fg
                        == crate::presentation_style::Theme::default()
                            .style(Role::Added)
                            .fg
                })
            }));
        }
        let mut expanded = Vec::new();
        render(
            &mut expanded,
            &activity,
            "  ",
            80,
            &crate::tool_output::RenderOptions::new(
                crate::tool_output::OutputDetail::Expanded,
                &crate::keybindings::KeyBindings::default(),
                &crate::presentation_style::Theme::default(),
                crate::transcript_render::ThinkingVisibility::Hidden,
            ),
        );
        let expanded = expanded
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(expanded.contains("--- old") && expanded.contains("+++ new"));
        assert!(expanded.contains("diff capture truncated") && !expanded.contains('\u{1b}'));
        let history = TranscriptProjection {
            items: vec![TranscriptItem::ActivityGroup(ActivityGroup {
                turn: 1,
                open: false,
                activities: vec![activity.clone()],
            })],
        };
        let mut detail = crate::transcript_detail::DetailView::new(std::num::NonZeroUsize::new(1));
        detail.prepare(&history, None, 80);
        let inspection = detail.rows().join("\n");
        assert!(inspection.contains("Recorded edit diff · capture truncated"));
        assert!(inspection.contains("@@ -1 +1 @@") && inspection.contains("+x�[2J"));
        assert!(
            !inspection.contains("\\n"),
            "patch was displayed as escaped JSON"
        );
        assert_eq!(
            activity.result.as_ref().unwrap().value["diff"]["text"],
            text
        );
        activity.state = ActivityState::Unknown;
        assert!(recorded(&activity).is_none());
    }
}
