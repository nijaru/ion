//! Pure compact rendering for the typed coding transcript.
use ion_core::{
    ActivityGroup, ActivityOutcome, ActivityResult, ToolActivityKind, TranscriptActivity,
    TranscriptItem, TranscriptMessage, TranscriptPart, TranscriptProjection,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const MAX_COMPACT_GROUP_ROWS: usize = 12;
const MAX_COALESCED_SUBJECTS: usize = 3;

pub fn rows(projection: &TranscriptProjection, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();
    for item in &projection.items {
        if !rows.is_empty() && rows.last().is_some_and(|row: &String| !row.is_empty()) {
            rows.push(String::new());
        }
        match item {
            TranscriptItem::User(message) => render_message(&mut rows, message, true, width),
            TranscriptItem::Assistant(message) => render_message(&mut rows, message, false, width),
            TranscriptItem::ActivityGroup(group) => render_group(&mut rows, group, width),
            TranscriptItem::UserShell(shell) => {
                let marker = if shell.is_error { "!" } else { "›" };
                push_prefixed(
                    &mut rows,
                    &format!("{marker} !"),
                    "  ",
                    &shell.command,
                    width,
                );
            }
        }
    }
    while rows.last().is_some_and(String::is_empty) {
        rows.pop();
    }
    rows
}

fn render_message(rows: &mut Vec<String>, message: &TranscriptMessage, user: bool, width: usize) {
    let mut first = true;
    for part in &message.parts {
        match part {
            TranscriptPart::Text(text) => {
                if user {
                    let prefix = if first {
                        if message.steering { "↳ " } else { "› " }
                    } else {
                        "  "
                    };
                    push_prefixed(rows, prefix, "  ", text, width);
                } else {
                    push_wrapped(rows, text, width);
                }
            }
            TranscriptPart::Image { mime_type } => {
                let image = format!("[image: {mime_type}]");
                if user {
                    let prefix = if first {
                        if message.steering { "↳ " } else { "› " }
                    } else {
                        "  "
                    };
                    push_prefixed(rows, prefix, "  ", &image, width);
                } else {
                    push_wrapped(rows, &image, width);
                }
            }
        }
        first = false;
    }
}

#[derive(Debug)]
struct DisplayActivity {
    summary: String,
    detail: Option<String>,
    source_count: usize,
    observation: bool,
    high_salience: bool,
}

fn render_group(rows: &mut Vec<String>, group: &ActivityGroup, width: usize) {
    if group.activities.is_empty() {
        return;
    }
    let display = compact_activities(&group.activities);
    let (display, omitted) = bound_activities(display);

    if group.activities.len() == 1 && omitted == 0 {
        let item = &display[0];
        rows.push(fit_line(&format!("● {}", item.summary), width));
        if let Some(detail) = &item.detail {
            rows.push(fit_line(&format!("  └ {detail}"), width));
        }
        return;
    }

    rows.push(fit_line(&group_header(group), width));
    let total_children = display.len() + usize::from(omitted > 0);
    for (index, item) in display.iter().enumerate() {
        let last = index + 1 == total_children;
        let branch = if last { "└ " } else { "├ " };
        rows.push(fit_line(&format!("{branch}{}", item.summary), width));
        if let Some(detail) = &item.detail {
            let prefix = if last { "  └ " } else { "│ └ " };
            rows.push(fit_line(&format!("{prefix}{detail}"), width));
        }
    }
    if omitted > 0 {
        rows.push(fit_line(&format!("└ {omitted} more · Ctrl-O"), width));
    }
}

fn group_header(group: &ActivityGroup) -> String {
    let mut counts = [0usize; 9];
    let mut failed = 0usize;
    let mut cancelled = 0usize;
    let mut timed_out = 0usize;
    let mut rejected = 0usize;
    let mut unknown = 0usize;

    for activity in &group.activities {
        counts[kind_index(activity.activity.kind)] += 1;
        match activity.outcome {
            ActivityOutcome::Failed => failed += 1,
            ActivityOutcome::Cancelled => cancelled += 1,
            ActivityOutcome::TimedOut => timed_out += 1,
            ActivityOutcome::Rejected => rejected += 1,
            ActivityOutcome::Unknown => unknown += 1,
            ActivityOutcome::Pending | ActivityOutcome::Completed => {}
        }
    }

    let count = group.activities.len();
    let mut parts = vec![format!(
        "{count} action{}",
        if count == 1 { "" } else { "s" }
    )];
    const LABELS: [&str; 9] = [
        "read", "list", "search", "edit", "write", "command", "ask", "subagent", "external",
    ];
    for (count, label) in counts.into_iter().zip(LABELS) {
        if count > 0 {
            parts.push(format!("{count} {label}"));
        }
    }
    for (count, label) in [
        (failed, "failed"),
        (cancelled, "cancelled"),
        (timed_out, "timed out"),
        (rejected, "skipped"),
        (unknown, "unknown"),
    ] {
        if count > 0 {
            parts.push(format!("{count} {label}"));
        }
    }
    format!("● {}", parts.join(" · "))
}

fn compact_activities(activities: &[TranscriptActivity]) -> Vec<DisplayActivity> {
    let mut display = Vec::new();
    let mut index = 0;
    while index < activities.len() {
        let current = &activities[index];
        if is_observation(current.activity.kind) && current.outcome == ActivityOutcome::Completed {
            let kind = current.activity.kind;
            let mut end = index + 1;
            while end < activities.len()
                && activities[end].activity.kind == kind
                && activities[end].outcome == ActivityOutcome::Completed
            {
                end += 1;
            }
            if end - index > 1 {
                display.push(coalesced_observation(&activities[index..end]));
                index = end;
                continue;
            }
        }
        display.push(display_activity(current));
        index += 1;
    }
    display
}

fn coalesced_observation(activities: &[TranscriptActivity]) -> DisplayActivity {
    let kind = activities[0].activity.kind;
    let subjects = activities
        .iter()
        .filter_map(|activity| activity.activity.subject.as_deref())
        .take(MAX_COALESCED_SUBJECTS)
        .map(clean_inline)
        .collect::<Vec<_>>();
    let mut summary = completed_verb(kind).to_owned();
    if !subjects.is_empty() {
        summary.push(' ');
        summary.push_str(&subjects.join(", "));
    } else {
        summary.push(' ');
        summary.push_str(&format!("{} items", activities.len()));
    }
    if activities.len() > subjects.len() && !subjects.is_empty() {
        summary.push_str(&format!(" · +{} more", activities.len() - subjects.len()));
    }
    DisplayActivity {
        summary,
        detail: None,
        source_count: activities.len(),
        observation: true,
        high_salience: false,
    }
}

fn display_activity(activity: &TranscriptActivity) -> DisplayActivity {
    let kind = activity.activity.kind;
    let mut summary = action_label(activity);
    if let Some(subject) = activity.activity.subject.as_deref() {
        let subject = clean_inline(subject);
        if !subject.is_empty() {
            summary.push(' ');
            summary.push_str(&subject);
        }
    }
    append_result_summary(&mut summary, activity);
    let detail = activity.result.as_ref().and_then(|result| {
        if kind == ToolActivityKind::Command {
            command_detail(result)
        } else {
            None
        }
    });
    let observation = is_observation(kind);
    let high_salience = !observation || activity.outcome != ActivityOutcome::Completed;
    DisplayActivity {
        summary,
        detail,
        source_count: 1,
        observation,
        high_salience,
    }
}

fn action_label(activity: &TranscriptActivity) -> String {
    let kind = activity.activity.kind;
    match activity.outcome {
        ActivityOutcome::Pending => pending_verb(kind).into(),
        ActivityOutcome::Completed => completed_verb(kind).into(),
        ActivityOutcome::Cancelled => "Cancelled".into(),
        ActivityOutcome::TimedOut => "Timed out".into(),
        ActivityOutcome::Rejected => "Skipped".into(),
        ActivityOutcome::Unknown => "Interrupted".into(),
        ActivityOutcome::Failed => {
            if kind == ToolActivityKind::Command
                && let Some(code) = activity
                    .result
                    .as_ref()
                    .and_then(|result| result.value.get("exit_code"))
                    .and_then(serde_json::Value::as_i64)
            {
                return format!("Exited {code}");
            }
            failed_label(kind).into()
        }
    }
}

fn append_result_summary(summary: &mut String, activity: &TranscriptActivity) {
    if activity.outcome != ActivityOutcome::Completed {
        return;
    }
    let Some(result) = &activity.result else {
        return;
    };
    match activity.activity.kind {
        ToolActivityKind::Edit => {
            if let Some(count) = result
                .value
                .get("replacements")
                .and_then(serde_json::Value::as_u64)
            {
                summary.push_str(&format!(
                    " · {count} replacement{}",
                    if count == 1 { "" } else { "s" }
                ));
            }
        }
        ToolActivityKind::Write => {
            if result
                .value
                .get("created")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
                && summary.starts_with("Wrote")
            {
                summary.replace_range(..5, "Created");
            }
            if let Some(bytes) = result
                .value
                .get("bytes")
                .and_then(serde_json::Value::as_u64)
            {
                summary.push_str(&format!(" · {bytes} B"));
            }
        }
        _ => {}
    }
}

fn command_detail(result: &ActivityResult) -> Option<String> {
    let stderr = result
        .value
        .get("stderr")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let stdout = result
        .value
        .get("stdout")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let preferred = if result.is_error && !stderr.trim().is_empty() {
        stderr
    } else {
        stdout
    };
    preferred
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map(clean_inline)
        .filter(|line| !line.is_empty())
}

fn bound_activities(display: Vec<DisplayActivity>) -> (Vec<DisplayActivity>, usize) {
    if display.len() <= MAX_COMPACT_GROUP_ROWS {
        return (display, 0);
    }
    let high_count = display.iter().filter(|item| item.high_salience).count();
    let observation_budget = MAX_COMPACT_GROUP_ROWS.saturating_sub(high_count);
    let mut kept = Vec::new();
    let mut observations_kept = 0usize;
    let mut omitted = 0usize;
    for item in display {
        if item.high_salience || observations_kept < observation_budget {
            if item.observation && !item.high_salience {
                observations_kept += 1;
            }
            kept.push(item);
        } else {
            omitted += item.source_count;
        }
    }
    (kept, omitted)
}

fn is_observation(kind: ToolActivityKind) -> bool {
    matches!(
        kind,
        ToolActivityKind::Read | ToolActivityKind::List | ToolActivityKind::Search
    )
}

fn kind_index(kind: ToolActivityKind) -> usize {
    match kind {
        ToolActivityKind::Read => 0,
        ToolActivityKind::List => 1,
        ToolActivityKind::Search => 2,
        ToolActivityKind::Edit => 3,
        ToolActivityKind::Write => 4,
        ToolActivityKind::Command => 5,
        ToolActivityKind::Ask => 6,
        ToolActivityKind::Subagent => 7,
        ToolActivityKind::External => 8,
    }
}

fn pending_verb(kind: ToolActivityKind) -> &'static str {
    match kind {
        ToolActivityKind::Read => "Reading",
        ToolActivityKind::List => "Listing",
        ToolActivityKind::Search => "Searching",
        ToolActivityKind::Edit => "Editing",
        ToolActivityKind::Write => "Writing",
        ToolActivityKind::Command => "Running",
        ToolActivityKind::Ask => "Asking",
        ToolActivityKind::Subagent => "Delegating",
        ToolActivityKind::External => "Calling",
    }
}

fn completed_verb(kind: ToolActivityKind) -> &'static str {
    match kind {
        ToolActivityKind::Read => "Read",
        ToolActivityKind::List => "Listed",
        ToolActivityKind::Search => "Searched",
        ToolActivityKind::Edit => "Edited",
        ToolActivityKind::Write => "Wrote",
        ToolActivityKind::Command => "Ran",
        ToolActivityKind::Ask => "Asked",
        ToolActivityKind::Subagent => "Delegated",
        ToolActivityKind::External => "Called",
    }
}

fn failed_label(kind: ToolActivityKind) -> &'static str {
    match kind {
        ToolActivityKind::Read => "Read failed",
        ToolActivityKind::List => "List failed",
        ToolActivityKind::Search => "Search failed",
        ToolActivityKind::Edit => "Edit failed",
        ToolActivityKind::Write => "Write failed",
        ToolActivityKind::Command => "Command failed",
        ToolActivityKind::Ask => "Ask failed",
        ToolActivityKind::Subagent => "Subagent failed",
        ToolActivityKind::External => "Call failed",
    }
}

fn clean_inline(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character == '\n' || character == '\r' || character == '\t' {
                ' '
            } else if character.is_control() {
                '�'
            } else {
                character
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn push_prefixed(
    rows: &mut Vec<String>,
    prefix: &str,
    continuation: &str,
    text: &str,
    width: usize,
) {
    let mut first = true;
    for logical in text.split('\n') {
        let line_prefix = if first { prefix } else { continuation };
        wrap_one(rows, line_prefix, continuation, logical, width);
        first = false;
    }
}

fn push_wrapped(rows: &mut Vec<String>, text: &str, width: usize) {
    for logical in text.split('\n') {
        wrap_one(rows, "", "", logical, width);
    }
}

fn wrap_one(
    rows: &mut Vec<String>,
    first_prefix: &str,
    continuation: &str,
    text: &str,
    width: usize,
) {
    let mut line = first_prefix.to_owned();
    let mut col = UnicodeWidthStr::width(first_prefix);
    let mut prefix = first_prefix;
    for grapheme in text.graphemes(true) {
        let display = if grapheme == "\t" {
            "    "
        } else if grapheme.chars().any(char::is_control) {
            "�"
        } else {
            grapheme
        };
        let size = UnicodeWidthStr::width(display).max(1);
        if col + size > width && col > UnicodeWidthStr::width(prefix) {
            rows.push(std::mem::take(&mut line));
            line.push_str(continuation);
            prefix = continuation;
            col = UnicodeWidthStr::width(continuation);
        }
        line.push_str(display);
        col += size;
    }
    rows.push(line);
}

fn fit_line(text: &str, width: usize) -> String {
    let width = width.max(1);
    if UnicodeWidthStr::width(text) <= width {
        return text.to_owned();
    }
    if width == 1 {
        return "…".into();
    }
    let target = width - 1;
    let mut out = String::new();
    let mut used = 0usize;
    for grapheme in text.graphemes(true) {
        let size = UnicodeWidthStr::width(grapheme).max(1);
        if used + size > target {
            break;
        }
        out.push_str(grapheme);
        used += size;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ion_core::{ToolActivity, TranscriptActivity};

    fn activity(
        id: &str,
        kind: ToolActivityKind,
        subject: &str,
        outcome: ActivityOutcome,
        result: Option<serde_json::Value>,
    ) -> TranscriptActivity {
        TranscriptActivity {
            call_id: id.into(),
            name: id.into(),
            activity: ToolActivity {
                kind,
                subject: Some(subject.into()),
            },
            arguments: serde_json::Value::Null,
            outcome,
            result: result.map(|value| ActivityResult {
                value,
                image_mime_types: Vec::new(),
                is_error: outcome != ActivityOutcome::Completed,
            }),
        }
    }

    #[test]
    fn grouped_tools_are_hierarchical_and_mutations_are_visually_distinct() {
        let projection = TranscriptProjection {
            items: vec![TranscriptItem::ActivityGroup(ActivityGroup {
                turn: 1,
                open: false,
                activities: vec![
                    activity(
                        "r1",
                        ToolActivityKind::Read,
                        "src/a.rs",
                        ActivityOutcome::Completed,
                        None,
                    ),
                    activity(
                        "r2",
                        ToolActivityKind::Read,
                        "src/b.rs",
                        ActivityOutcome::Completed,
                        None,
                    ),
                    activity(
                        "r3",
                        ToolActivityKind::Read,
                        "src/c.rs",
                        ActivityOutcome::Completed,
                        None,
                    ),
                    activity(
                        "e1",
                        ToolActivityKind::Edit,
                        "src/parser.rs",
                        ActivityOutcome::Completed,
                        Some(serde_json::json!({"replacements":2})),
                    ),
                    activity(
                        "x1",
                        ToolActivityKind::Command,
                        "cargo test",
                        ActivityOutcome::Completed,
                        Some(
                            serde_json::json!({"stdout":"running\ntest result: ok. 148 passed","stderr":""}),
                        ),
                    ),
                ],
            })],
        };
        let rendered = rows(&projection, 100).join("\n");
        assert!(rendered.contains("● 5 actions · 3 read · 1 edit · 1 command"));
        assert!(rendered.contains("├ Read src/a.rs, src/b.rs, src/c.rs"));
        assert!(rendered.contains("├ Edited src/parser.rs · 2 replacements"));
        assert!(rendered.contains("└ Ran cargo test"));
        assert!(rendered.contains("  └ test result: ok. 148 passed"));
        assert_eq!(rendered.matches("Read src/").count(), 1);
    }

    #[test]
    fn failed_command_surfaces_exit_and_stderr() {
        let projection = TranscriptProjection {
            items: vec![TranscriptItem::ActivityGroup(ActivityGroup {
                turn: 1,
                open: false,
                activities: vec![activity(
                    "x",
                    ToolActivityKind::Command,
                    "cargo test",
                    ActivityOutcome::Failed,
                    Some(serde_json::json!({"exit_code":1,"stdout":"","stderr":"compile failed"})),
                )],
            })],
        };
        let rendered = rows(&projection, 80).join("\n");
        assert!(rendered.contains("● Exited 1 cargo test"));
        assert!(rendered.contains("└ compile failed"));
    }

    #[test]
    fn user_and_assistant_are_not_rendered_as_peer_tool_protocol_rows() {
        let projection = TranscriptProjection {
            items: vec![
                TranscriptItem::User(TranscriptMessage {
                    turn: Some(1),
                    steering: false,
                    parts: vec![TranscriptPart::Text("fix the parser".into())],
                }),
                TranscriptItem::Assistant(TranscriptMessage {
                    turn: Some(1),
                    steering: false,
                    parts: vec![TranscriptPart::Text("I found the issue.".into())],
                }),
            ],
        };
        assert_eq!(
            rows(&projection, 80),
            vec!["› fix the parser", "", "I found the issue."]
        );
    }

    #[test]
    fn compact_group_never_hides_mutations_or_failures() {
        let mut activities = (0..20)
            .map(|index| {
                activity(
                    &format!("r{index}"),
                    ToolActivityKind::Read,
                    &format!("file-{index}.rs"),
                    ActivityOutcome::Completed,
                    None,
                )
            })
            .collect::<Vec<_>>();
        activities.push(activity(
            "edit",
            ToolActivityKind::Edit,
            "important.rs",
            ActivityOutcome::Completed,
            Some(serde_json::json!({"replacements":1})),
        ));
        activities.push(activity(
            "failed",
            ToolActivityKind::Command,
            "cargo test",
            ActivityOutcome::Failed,
            Some(serde_json::json!({"exit_code":1,"stderr":"failed"})),
        ));
        let projection = TranscriptProjection {
            items: vec![TranscriptItem::ActivityGroup(ActivityGroup {
                turn: 1,
                open: false,
                activities,
            })],
        };
        let rendered = rows(&projection, 100).join("\n");
        assert!(rendered.contains("Edited important.rs"));
        assert!(rendered.contains("Exited 1 cargo test"));
        assert!(rendered.contains("Read file-0.rs, file-1.rs, file-2.rs · +17 more"));
    }
}
