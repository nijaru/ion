//! Pure compact rendering for the typed coding transcript.
use ion_core::{
    ActivityGroup, ActivityOutcome, ActivityResult, ToolActivityKind, TranscriptActivity,
    TranscriptItem, TranscriptMessage, TranscriptPart, TranscriptProjection, UserShellActivity,
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
            TranscriptItem::UserShell(shell) => render_shell(&mut rows, shell, width),
        }
    }
    while rows.last().is_some_and(String::is_empty) {
        rows.pop();
    }
    rows
}

/// Keep current-Turn counts and an exception visible when its narrative/tool
/// rows exceed the inline viewport. This is a projection, never a history cut.
pub(super) fn live_rows(
    projection: &TranscriptProjection,
    width: usize,
    budget: usize,
) -> Vec<String> {
    if budget == 0 {
        return Vec::new();
    }
    let mut rendered = rows(projection, width);
    if rendered.len() <= budget {
        return rendered;
    }
    let activities = || {
        projection.items.iter().flat_map(|item| match item {
            TranscriptItem::ActivityGroup(group) => group.activities.as_slice(),
            _ => &[],
        })
    };
    let mut pinned = Vec::new();
    if activities().next().is_some() {
        push_wrapped(
            &mut pinned,
            &format!("{} · Ctrl-O", group_header(activities())),
            width.max(1),
        );
        pinned.truncate(budget.saturating_sub(2).clamp(1, 3));
        if budget > pinned.len() + 1
            && let Some(exception) = activities().rfind(|activity| {
                !matches!(
                    activity.outcome,
                    ActivityOutcome::Pending | ActivityOutcome::Completed
                )
            })
        {
            pinned.push(fit_line(&display_activity(exception).summary, width.max(1)));
        }
    } else {
        pinned.push(fit_line("… earlier conversation · Ctrl-O", width.max(1)));
    }
    let tail = budget.saturating_sub(pinned.len());
    rendered.drain(..rendered.len().saturating_sub(tail));
    pinned.extend(rendered);
    pinned
}

pub(super) fn render_message(
    rows: &mut Vec<String>,
    message: &TranscriptMessage,
    user: bool,
    width: usize,
) {
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

fn render_shell(rows: &mut Vec<String>, shell: &UserShellActivity, width: usize) {
    let prefix = if shell.exclude_from_context {
        "› !!"
    } else {
        "› !"
    };
    push_prefixed(rows, prefix, "  ", &shell.command, width);
    for stream in ["stdout", "stderr"] {
        if let Some(text) = shell.output[stream]
            .as_str()
            .filter(|text| !text.is_empty())
        {
            let count = text.lines().count();
            if count > 4 {
                push_wrapped(
                    rows,
                    &format!("  … {} earlier {stream} lines · Ctrl-O", count - 4),
                    width,
                );
            }
            for line in text.lines().skip(count.saturating_sub(4)) {
                push_prefixed(rows, &format!("  {stream}: "), "    ", line, width);
            }
        }
        if shell.output[format!("{stream}_truncated")].as_bool() == Some(true) {
            let capture = shell.output[format!("{stream}_full_path")].as_str();
            let note = capture.map_or_else(
                || format!("  {stream} truncated/incomplete · Ctrl-O"),
                |path| format!("  {stream} truncated/incomplete · capture: {path} · Ctrl-O"),
            );
            push_wrapped(rows, &note, width);
        }
    }
    let mut outcome = if let Some(signal) = shell.output["signal"].as_i64() {
        format!("signal {signal}")
    } else if let Some(code) = shell.output["exit_code"].as_i64() {
        format!("exit {code}")
    } else if shell.output["wait_error"].as_str().is_some() {
        "exit unknown; inspect details".into()
    } else if shell.is_error {
        "failed; inspect details".into()
    } else {
        "completed".into()
    };
    if shell.output["cancelled"].as_bool() == Some(true) {
        outcome.push_str(" · cancelled");
    }
    if shell.output["timed_out"].as_bool() == Some(true) {
        outcome.push_str(" · timed out");
    }
    if shell.exclude_from_context {
        outcome.push_str(" · not shared with model");
    }
    push_wrapped(rows, &format!("  {outcome}"), width);
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

    rows.push(fit_line(&group_header(group.activities.iter()), width));
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

fn group_header<'a>(activities: impl Iterator<Item = &'a TranscriptActivity>) -> String {
    let mut counts = [0usize; 9];
    let mut pending = 0usize;
    let mut count = 0usize;
    let mut failed = 0usize;
    let mut cancelled = 0usize;
    let mut timed_out = 0usize;
    let mut rejected = 0usize;
    let mut unknown = 0usize;

    for activity in activities {
        count += 1;
        counts[kind_index(activity.activity.kind)] += 1;
        match activity.outcome {
            ActivityOutcome::Failed => failed += 1,
            ActivityOutcome::Cancelled => cancelled += 1,
            ActivityOutcome::TimedOut => timed_out += 1,
            ActivityOutcome::Rejected => rejected += 1,
            ActivityOutcome::Unknown => unknown += 1,
            ActivityOutcome::Pending => pending += 1,
            ActivityOutcome::Completed => {}
        }
    }

    let mut parts = vec![format!(
        "{count} action{}",
        if count == 1 { "" } else { "s" }
    )];
    for (count, label) in [
        (failed, "failed"),
        (pending, "pending"),
        (cancelled, "cancelled"),
        (timed_out, "timed out"),
        (rejected, "skipped"),
        (unknown, "unknown"),
    ] {
        if count > 0 {
            parts.push(format!("{count} {label}"));
        }
    }
    const KINDS: [(usize, &str); 9] = [
        (3, "edit"),
        (4, "write"),
        (5, "command"),
        (6, "ask"),
        (7, "subagent"),
        (8, "external"),
        (0, "read"),
        (1, "list"),
        (2, "search"),
    ];
    for (index, label) in KINDS {
        if counts[index] > 0 {
            parts.push(format!("{} {label}", counts[index]));
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

pub(super) fn push_wrapped(rows: &mut Vec<String>, text: &str, width: usize) {
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

    #[test]
    fn shell_preview_shows_observed_output_outcome_and_context_choice() {
        let projection = TranscriptProjection {
            items: vec![TranscriptItem::UserShell(ion_core::UserShellActivity {
                command: "run-check".into(),
                output: serde_json::json!({
                    "stdout": "first\nsecond\nthird\nfourth\nOBSERVED_OUTPUT\n",
                    "stderr": "\u{1b}[2JOBSERVED_FAILURE\n",
                    "exit_code": 7,
                    "stdout_truncated": true,
                    "stdout_full_path": "/tmp/capture.txt"
                }),
                is_error: true,
                exclude_from_context: true,
            })],
        };
        let rendered = rows(&projection, 120).join("\n");
        assert!(rendered.contains("OBSERVED_OUTPUT"), "{rendered}");
        assert!(rendered.contains("OBSERVED_FAILURE"));
        assert!(rendered.contains("exit 7"));
        assert!(rendered.contains("not shared with model"));
        assert!(rendered.contains("stdout truncated"));
        assert!(rendered.contains("/tmp/capture.txt"));
        assert!(rendered.contains("earlier stdout lines"));
        assert!(!rendered.contains("\u{1b}"));
    }

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
        for count in ["● 5 actions", "3 read", "1 edit", "1 command"] {
            assert!(rendered.contains(count));
        }
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
    fn live_budget_keeps_earlier_exception_and_mutations_across_narrative() {
        let mut activities = vec![activity(
            "failed",
            ToolActivityKind::Command,
            "FAILURE_MARKER",
            ActivityOutcome::Failed,
            Some(serde_json::json!({"exit_code": 7})),
        )];
        activities.extend((0..14).map(|index| {
            activity(
                &format!("write-{index}"),
                ToolActivityKind::Write,
                "changed.txt",
                ActivityOutcome::Completed,
                None,
            )
        }));
        let projection = TranscriptProjection {
            items: vec![
                TranscriptItem::ActivityGroup(ActivityGroup {
                    turn: 1,
                    open: false,
                    activities,
                }),
                TranscriptItem::Assistant(TranscriptMessage {
                    turn: Some(1),
                    steering: false,
                    parts: vec![TranscriptPart::Text(
                        (0..20)
                            .map(|index| format!("NARRATIVE_{index}\n"))
                            .collect(),
                    )],
                }),
                TranscriptItem::ActivityGroup(ActivityGroup {
                    turn: 1,
                    open: true,
                    activities: vec![activity(
                        "pending",
                        ToolActivityKind::Read,
                        "CURRENT_READ",
                        ActivityOutcome::Pending,
                        None,
                    )],
                }),
            ],
        };
        for width in [24, 100] {
            let rendered = live_rows(&projection, width, 7);
            assert!(rendered.len() <= 7);
            let text = rendered.join("\n");
            for fact in [
                "16 actions",
                "1 failed",
                "1 pending",
                "14 write",
                "FAILURE_MARKER",
                "CURRENT_READ",
            ] {
                assert!(text.contains(fact), "missing {fact}: {text}");
            }
        }
        assert_eq!(live_rows(&projection, 100, 100), rows(&projection, 100));
        assert!(live_rows(&projection, 100, 0).is_empty());
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
