//! Pure compact rendering for the typed coding transcript.
use crate::display_text::fit_line;
use ion_core::{
    ActivityGroup, ActivityResult, ActivityState, ToolActivityKind, TranscriptActivity,
    TranscriptItem, TranscriptMessage, TranscriptPart, TranscriptProjection, UserShellActivity,
};
use ratatui::{
    style::{Color, Modifier, Style},
    text::Line,
};

pub(super) fn kind_label(kind: ToolActivityKind) -> &'static str {
    match kind {
        ToolActivityKind::Read => "read",
        ToolActivityKind::List => "list",
        ToolActivityKind::Search => "search",
        ToolActivityKind::Edit => "edit",
        ToolActivityKind::Write => "write",
        ToolActivityKind::Command => "command",
        ToolActivityKind::Ask => "ask",
        ToolActivityKind::Subagent => "subagent",
        ToolActivityKind::External => "external",
    }
}

pub(super) fn state_label(state: ActivityState) -> &'static str {
    match state {
        ActivityState::Queued => "queued",
        ActivityState::Running => "running",
        ActivityState::Completed => "completed",
        ActivityState::Failed => "failed",
        ActivityState::Cancelled => "cancelled",
        ActivityState::TimedOut => "timed out",
        ActivityState::Rejected => "rejected",
        ActivityState::Unknown => "unknown",
    }
}

const MAX_COALESCED_SUBJECTS: usize = 3;

pub fn rows(projection: &TranscriptProjection, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut rows = Vec::new();
    for item in &projection.items {
        if !rows.is_empty() && rows.last().is_some_and(|row: &Line<'_>| row.width() > 0) {
            rows.push(Line::default());
        }
        match item {
            TranscriptItem::User(message) => render_message(&mut rows, message, true, width),
            TranscriptItem::Assistant(message) => render_message(&mut rows, message, false, width),
            TranscriptItem::ActivityGroup(group) => render_group(&mut rows, group, width),
            TranscriptItem::UserShell(shell) => render_shell(&mut rows, shell, width),
        }
    }
    while rows.last().is_some_and(|row| row.width() == 0) {
        rows.pop();
    }
    rows
}

/// Select a semantic focus before rendering its children. Never take a suffix
/// of the flattened conversation: that can detach branches from their root.
pub(super) fn live_rows(
    projection: &TranscriptProjection,
    width: usize,
    budget: usize,
) -> Vec<Line<'static>> {
    if budget == 0 {
        return Vec::new();
    }
    let rendered = rows(projection, width);
    if rendered.len() <= budget {
        return rendered;
    }
    let activities = || {
        projection
            .items
            .iter()
            .enumerate()
            .flat_map(|(item_index, item)| {
                let activities = match item {
                    TranscriptItem::ActivityGroup(group) => group.activities.as_slice(),
                    _ => &[],
                };
                activities
                    .iter()
                    .enumerate()
                    .map(move |(activity_index, activity)| (item_index, activity_index, activity))
            })
    };
    let mut selected = Vec::new();
    let mut pinned = None;
    if activities().next().is_some() {
        push_wrapped(
            &mut selected,
            &format!(
                "{} · Ctrl-O",
                group_header(activities().map(|(_, _, activity)| activity))
            ),
            width.max(1),
        );
        for row in &mut selected {
            row.style = heading();
        }
        selected.truncate(budget.saturating_sub(2).clamp(1, 3));
        if budget > selected.len()
            && let Some((item_index, activity_index, exception)) =
                activities().rfind(|(_, _, activity)| nested_exception(activity).is_some())
        {
            pinned = Some((item_index, activity_index));
            let exception = nested_exception(exception).expect("exception selected above");
            // Preserve severity even when a long child subject must be fitted.
            let subject = exception
                .activity
                .subject
                .as_deref()
                .map(clean_inline)
                .unwrap_or_else(|| exception.name.clone());
            let diagnostic = match model_result_notice(exception) {
                Some(notice) => format!("! {notice} · {subject}"),
                None => format!("! {} · {subject}", action_label(exception)),
            };
            selected.push(Line::styled(
                fit_line(&diagnostic, width),
                activity_style(
                    exception.state,
                    model_result_notice(exception).is_some(),
                    false,
                ),
            ));
        }
    } else {
        selected.push(Line::styled(
            fit_line("… earlier conversation · Ctrl-O", width),
            subdued(),
        ));
    }
    let remaining = budget.saturating_sub(selected.len());
    let focus = projection.items.iter().enumerate().rfind(|(_, item)| {
        matches!(item, TranscriptItem::ActivityGroup(group) if group.activities.iter().any(|a| a.state == ActivityState::Running))
    }).or_else(|| projection.items.iter().enumerate().next_back());
    if remaining > 0
        && let Some((item_index, focus)) = focus
    {
        match focus {
            TranscriptItem::ActivityGroup(group) => {
                let pinned_index = pinned
                    .filter(|(index, _)| *index == item_index)
                    .map(|(_, index)| index);
                render_current_group(&mut selected, group, width, remaining, pinned_index)
            }
            TranscriptItem::User(message) | TranscriptItem::Assistant(message) => {
                let user = matches!(focus, TranscriptItem::User(_));
                let mut preview = Vec::new();
                render_message(&mut preview, message, user, width);
                let omitted = preview.len().saturating_sub(remaining);
                if omitted > 0 {
                    preview.drain(..omitted);
                    if let Some(first) = preview.first_mut() {
                        *first = Line::styled(
                            fit_line(
                                &format!(
                                    "{}… {}",
                                    if user { "› " } else { "" },
                                    first.to_string().trim_start()
                                ),
                                width,
                            ),
                            first.style,
                        );
                    }
                }
                selected.extend(preview);
            }
            TranscriptItem::UserShell(shell) => {
                let mut preview = Vec::new();
                render_shell(&mut preview, shell, width);
                preview.truncate(remaining);
                selected.extend(preview);
            }
        }
    }
    selected
}

/// The live tree keeps a root and selects whole actions in execution priority,
/// then restores their source order. A queued tail cannot evict running work.
fn render_current_group(
    rows: &mut Vec<Line<'static>>,
    group: &ActivityGroup,
    width: usize,
    budget: usize,
    pinned_index: Option<usize>,
) {
    let display = compact_activities(&group.activities);
    let capacity = budget.saturating_sub(1);
    let mut indices = (0..display.len())
        .filter(|&index| Some(display[index].source.start) != pinned_index)
        .collect::<Vec<_>>();
    if indices.is_empty() {
        return;
    }
    rows.push(Line::styled(
        fit_line("• Current activity", width),
        heading(),
    ));
    let reserve_omission = usize::from(indices.len() > capacity && capacity > 1);
    indices.sort_by_key(|&index| (display[index].priority(), std::cmp::Reverse(index)));
    indices.truncate(capacity.saturating_sub(reserve_omission));
    indices.sort_unstable();
    let omitted = display
        .iter()
        .enumerate()
        .filter(|(index, _)| {
            !indices.contains(index) && Some(display[*index].source.start) != pinned_index
        })
        .map(|(_, activity)| activity.source.len())
        .sum::<usize>();
    for (position, &index) in indices.iter().enumerate() {
        let last = position + 1 == indices.len() && reserve_omission == 0;
        rows.push(Line::styled(
            fit_line(
                &format!(
                    "{}{}",
                    if last { "└ " } else { "├ " },
                    display[index].summary
                ),
                width,
            ),
            display[index].style(),
        ));
    }
    if reserve_omission > 0 {
        rows.push(Line::styled(
            fit_line(&format!("└ {omitted} more · Ctrl-O"), width),
            subdued(),
        ));
    }
}

pub(super) fn render_message(
    rows: &mut Vec<Line<'static>>,
    message: &TranscriptMessage,
    user: bool,
    width: usize,
) {
    let start = rows.len();
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
    if user {
        for row in &mut rows[start..] {
            row.style = Style::default().fg(Color::Blue);
        }
    }
}

fn render_shell(rows: &mut Vec<Line<'static>>, shell: &UserShellActivity, width: usize) {
    let prefix = if shell.exclude_from_context {
        "› !!"
    } else {
        "› !"
    };
    let start = rows.len();
    push_prefixed(rows, prefix, "  ", &shell.command, width);
    for row in &mut rows[start..] {
        row.style = Style::default().fg(Color::Blue);
    }
    let ion_core::UserShellOutcome::Observed { output, is_error } = &shell.outcome else {
        if shell.exclude_from_context {
            push_wrapped(rows, "  not shared with model", width);
        }
        let start = rows.len();
        push_wrapped(rows, ion_core::UserShellOutcome::unknown_notice(), width);
        for row in &mut rows[start..] {
            row.style = heading().fg(Color::Yellow);
        }
        return;
    };
    for stream in ["stdout", "stderr"] {
        if let Some(text) = output[stream].as_str().filter(|text| !text.is_empty()) {
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
        if output[format!("{stream}_truncated")].as_bool() == Some(true) {
            let capture = output[format!("{stream}_full_path")].as_str();
            let note = capture.map_or_else(
                || format!("  {stream} truncated/incomplete · Ctrl-O"),
                |path| format!("  {stream} truncated/incomplete · capture: {path} · Ctrl-O"),
            );
            push_wrapped(rows, &note, width);
        }
    }
    let mut state = if let Some(signal) = output["signal"].as_i64() {
        format!("signal {signal}")
    } else if let Some(code) = output["exit_code"].as_i64() {
        format!("exit {code}")
    } else if output["wait_error"].as_str().is_some() {
        "exit unknown; inspect details".into()
    } else if *is_error {
        "failed; inspect details".into()
    } else {
        "completed".into()
    };
    if output["cancelled"].as_bool() == Some(true) {
        state.push_str(" · cancelled");
    }
    if output["timed_out"].as_bool() == Some(true) {
        state.push_str(" · timed out");
    }
    if shell.exclude_from_context {
        state.push_str(" · not shared with model");
    }
    let start = rows.len();
    push_wrapped(rows, &format!("  {state}"), width);
    let style = if *is_error {
        heading().fg(Color::Red)
    } else {
        subdued()
    };
    for row in &mut rows[start..] {
        row.style = style;
    }
}

#[derive(Debug)]
struct DisplayActivity {
    summary: String,
    detail: Option<String>,
    source: std::ops::Range<usize>,
    observation: bool,
    state: ActivityState,
    model_notice: Option<&'static str>,
}

impl DisplayActivity {
    fn style(&self) -> Style {
        activity_style(self.state, self.model_notice.is_some(), self.observation)
    }

    fn priority(&self) -> u8 {
        match self.state {
            ActivityState::Running => 0,
            _ if self.model_notice.is_some() => 1,
            ActivityState::Failed
            | ActivityState::Cancelled
            | ActivityState::TimedOut
            | ActivityState::Rejected
            | ActivityState::Unknown => 1,
            ActivityState::Queued => 2,
            ActivityState::Completed if !self.observation => 3,
            ActivityState::Completed => 4,
        }
    }
}

fn render_group(rows: &mut Vec<Line<'static>>, group: &ActivityGroup, width: usize) {
    if group.activities.is_empty() {
        return;
    }
    let display = compact_activities(&group.activities);

    if group.activities.len() == 1 {
        let item = &display[0];
        push_activity(rows, "• ", "  ", item, width);
        if group.activities[0].children.is_empty()
            && let Some(detail) = &item.detail
        {
            push_detail(rows, "  └ ", "    ", detail, width);
        }
        render_children(rows, &group.activities[0].children, "  ", width);
        return;
    }

    rows.push(Line::styled(
        fit_line(&group_header(group.activities.iter()), width),
        heading(),
    ));
    let total_children = display.len();
    for (index, item) in display.iter().enumerate() {
        let last = index + 1 == total_children;
        let branch = if last { "└ " } else { "├ " };
        push_activity(rows, branch, if last { "  " } else { "│ " }, item, width);
        if group.activities[item.source.start].children.is_empty()
            && let Some(detail) = &item.detail
        {
            let prefix = if last { "  └ " } else { "│ └ " };
            push_detail(
                rows,
                prefix,
                if last { "    " } else { "│   " },
                detail,
                width,
            );
        }
        render_children(
            rows,
            &group.activities[item.source.start].children,
            if last { "  " } else { "│ " },
            width,
        );
    }
}

fn render_children(
    rows: &mut Vec<Line<'static>>,
    children: &[TranscriptActivity],
    prefix: &str,
    width: usize,
) {
    let display = compact_activities(children);
    for (index, item) in display.iter().enumerate() {
        let last = index + 1 == display.len();
        let continuation = format!("{prefix}{}", if last { "  " } else { "│ " });
        push_activity(
            rows,
            &format!("{prefix}{}", if last { "└ " } else { "├ " }),
            &continuation,
            item,
            width,
        );
        if let Some(detail) = &item.detail {
            push_detail(
                rows,
                &format!("{continuation}└ "),
                &format!("{continuation}  "),
                detail,
                width,
            );
        }
    }
}

fn group_header<'a>(activities: impl Iterator<Item = &'a TranscriptActivity>) -> String {
    let mut counts = [0usize; 9];
    let mut queued = 0usize;
    let mut running = 0usize;
    let mut count = 0usize;
    let mut failed = 0usize;
    let mut cancelled = 0usize;
    let mut timed_out = 0usize;
    let mut rejected = 0usize;
    let mut unknown = 0usize;
    let mut withheld = 0usize;

    for activity in activities {
        count += 1;
        withheld += usize::from(model_result_notice(activity).is_some());
        counts[kind_index(activity.activity.kind)] += 1;
        match activity.state {
            ActivityState::Failed => failed += 1,
            ActivityState::Cancelled => cancelled += 1,
            ActivityState::TimedOut => timed_out += 1,
            ActivityState::Rejected => rejected += 1,
            ActivityState::Unknown => unknown += 1,
            ActivityState::Queued => queued += 1,
            ActivityState::Running => running += 1,
            ActivityState::Completed => {}
        }
    }

    let mut parts = vec![format!(
        "{count} action{}",
        if count == 1 { "" } else { "s" }
    )];
    for (count, label) in [
        (failed, "failed"),
        (running, "running"),
        (queued, "queued"),
        (cancelled, "cancelled"),
        (timed_out, "timed out"),
        (rejected, "skipped"),
        (unknown, "unknown"),
        (withheld, "not shared"),
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
    format!("• {}", parts.join(" · "))
}

fn compact_activities(activities: &[TranscriptActivity]) -> Vec<DisplayActivity> {
    let mut display = Vec::new();
    let mut index = 0;
    while index < activities.len() {
        let current = &activities[index];
        if is_observation(current.activity.kind)
            && current.state == ActivityState::Completed
            && model_result_notice(current).is_none()
        {
            let kind = current.activity.kind;
            let mut end = index + 1;
            while end < activities.len()
                && activities[end].activity.kind == kind
                && activities[end].state == ActivityState::Completed
                && model_result_notice(&activities[end]).is_none()
            {
                end += 1;
            }
            if end - index > 1 {
                display.push(coalesced_observation(&activities[index..end], index..end));
                index = end;
                continue;
            }
        }
        display.push(display_activity(current, index));
        index += 1;
    }
    display
}

fn coalesced_observation(
    activities: &[TranscriptActivity],
    source: std::ops::Range<usize>,
) -> DisplayActivity {
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
        source,
        observation: true,
        state: ActivityState::Completed,
        model_notice: None,
    }
}

fn model_result_notice(activity: &TranscriptActivity) -> Option<&'static str> {
    activity
        .result
        .as_ref()
        .and_then(|result| result.projection.notice())
}

fn nested_exception(activity: &TranscriptActivity) -> Option<&TranscriptActivity> {
    let exceptional = |activity: &TranscriptActivity| {
        matches!(
            activity.state,
            ActivityState::Failed
                | ActivityState::Cancelled
                | ActivityState::TimedOut
                | ActivityState::Rejected
                | ActivityState::Unknown
        )
    };
    if model_result_notice(activity).is_some() {
        return Some(activity);
    }
    activity
        .children
        .iter()
        .rfind(|child| exceptional(child))
        .or_else(|| exceptional(activity).then_some(activity))
}

fn display_activity(activity: &TranscriptActivity, index: usize) -> DisplayActivity {
    let kind = activity.activity.kind;
    let mut summary = action_label(activity);
    if !activity.children.is_empty() {
        let running = activity
            .children
            .iter()
            .filter(|child| child.state == ActivityState::Running)
            .count();
        let exceptions = activity
            .children
            .iter()
            .filter(|child| {
                !matches!(
                    child.state,
                    ActivityState::Queued | ActivityState::Running | ActivityState::Completed
                )
            })
            .count();
        summary.push_str(&format!(
            " · {} child calls · {running} running · {exceptions} exceptions",
            activity.children.len()
        ));
    }
    let model_notice = model_result_notice(activity);
    if model_notice.is_some() {
        summary.push_str(" · result not shared with model");
    }
    if let Some(subject) = activity.activity.subject.as_deref() {
        let subject = clean_inline(subject);
        if !subject.is_empty() {
            summary.push(' ');
            summary.push_str(&subject);
        }
    }
    if activity.state == ActivityState::Queued {
        summary.push_str(&format!(" · {}", kind_label(kind)));
    }
    append_result_summary(&mut summary, activity);
    let child_detail = activity
        .children
        .iter()
        .rfind(|child| {
            !matches!(
                child.state,
                ActivityState::Completed | ActivityState::Queued | ActivityState::Running
            )
        })
        .or_else(|| {
            activity
                .children
                .iter()
                .find(|child| child.state == ActivityState::Running)
        })
        .map(|child| {
            format!(
                "{} {}",
                action_label(child),
                child.activity.subject.as_deref().unwrap_or(&child.name)
            )
        });
    let detail = child_detail.or_else(|| {
        activity.result.as_ref().and_then(|result| {
            if kind == ToolActivityKind::Command {
                command_detail(result)
            } else {
                None
            }
        })
    });
    let observation = is_observation(kind) && activity.children.is_empty();
    DisplayActivity {
        summary,
        detail,
        source: index..index + 1,
        observation,
        state: activity.state,
        model_notice,
    }
}

fn action_label(activity: &TranscriptActivity) -> String {
    let kind = activity.activity.kind;
    match activity.state {
        ActivityState::Queued => "Queued".into(),
        ActivityState::Running => running_verb(kind).into(),
        ActivityState::Completed => completed_verb(kind).into(),
        ActivityState::Cancelled => "Cancelled".into(),
        ActivityState::TimedOut => "Timed out".into(),
        ActivityState::Rejected => "Skipped".into(),
        ActivityState::Unknown => "Interrupted".into(),
        ActivityState::Failed => {
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
    if activity.state != ActivityState::Completed {
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

fn running_verb(kind: ToolActivityKind) -> &'static str {
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

fn heading() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}
fn subdued() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}
fn activity_style(state: ActivityState, notice: bool, observation: bool) -> Style {
    if notice {
        return heading().fg(Color::Yellow);
    }
    match state {
        ActivityState::Failed | ActivityState::Rejected => heading().fg(Color::Red),
        ActivityState::Unknown | ActivityState::Cancelled | ActivityState::TimedOut => {
            heading().fg(Color::Yellow)
        }
        ActivityState::Running => heading().fg(Color::Cyan),
        ActivityState::Queued => subdued(),
        ActivityState::Completed if !observation => heading(),
        ActivityState::Completed => Style::default(),
    }
}
fn push_prefixed(
    rows: &mut Vec<Line<'static>>,
    prefix: &str,
    continuation: &str,
    text: &str,
    width: usize,
) {
    let mut plain = Vec::new();
    crate::display_text::push_prefixed(&mut plain, prefix, continuation, text, width);
    rows.extend(plain.into_iter().map(Line::raw));
}
fn push_wrapped(rows: &mut Vec<Line<'static>>, text: &str, width: usize) {
    push_prefixed(rows, "", "", text, width);
}
fn push_activity(
    rows: &mut Vec<Line<'static>>,
    prefix: &str,
    continuation: &str,
    item: &DisplayActivity,
    width: usize,
) {
    let start = rows.len();
    push_prefixed(rows, prefix, continuation, &item.summary, width);
    for row in &mut rows[start..] {
        row.style = item.style();
    }
}
fn push_detail(
    rows: &mut Vec<Line<'static>>,
    prefix: &str,
    continuation: &str,
    text: &str,
    width: usize,
) {
    let start = rows.len();
    push_prefixed(rows, prefix, continuation, text, width);
    for row in &mut rows[start..] {
        row.style = subdued();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn plain_rows(projection: &TranscriptProjection, width: usize) -> Vec<String> {
        super::rows(projection, width)
            .iter()
            .map(ToString::to_string)
            .collect()
    }
    fn plain_live_rows(
        projection: &TranscriptProjection,
        width: usize,
        budget: usize,
    ) -> Vec<String> {
        super::live_rows(projection, width, budget)
            .iter()
            .map(ToString::to_string)
            .collect()
    }
    use ion_core::{ToolActivity, TranscriptActivity};

    #[test]
    fn activity_styles_follow_typed_state_and_wrapping_keeps_the_tree() {
        let subject = "deep/project/with/a/long/path/界界/important.rs";
        let projection = TranscriptProjection {
            items: vec![TranscriptItem::ActivityGroup(ActivityGroup {
                turn: 1,
                open: true,
                activities: vec![
                    activity(
                        "read",
                        ToolActivityKind::Read,
                        subject,
                        ActivityState::Running,
                        None,
                    ),
                    activity(
                        "edit",
                        ToolActivityKind::Edit,
                        "failed.rs",
                        ActivityState::Failed,
                        None,
                    ),
                ],
            })],
        };
        let rows = super::rows(&projection, 24);
        let read = rows
            .iter()
            .position(|row| row.to_string().starts_with("├ Reading"))
            .unwrap();
        let failed = rows
            .iter()
            .position(|row| row.to_string().starts_with("└ Edit failed"))
            .unwrap();
        assert!(failed > read + 1);
        assert_eq!(rows[read].style.fg, Some(Color::Cyan));
        assert_eq!(rows[failed].style.fg, Some(Color::Red));
        let wrapped = rows[read..failed]
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        assert!(wrapped[1..].iter().all(|row| row.starts_with("│ ")));
        let retained = wrapped
            .iter()
            .map(|row| row.chars().skip(2).collect::<String>())
            .collect::<String>();
        assert!(retained.contains(subject));
        assert!(rows.iter().all(|row| row.width() <= 24));
        let live = super::live_rows(&projection, 24, 6);
        assert!(live.iter().any(|row| row.style.fg == Some(Color::Red)));
        assert!(live.iter().any(|row| row.style.fg == Some(Color::Cyan)));
    }

    #[test]
    fn nested_wrapping_keeps_sibling_rails_for_actions_and_details() {
        let mut parent = activity(
            "parent",
            ToolActivityKind::External,
            "composition",
            ActivityState::Running,
            None,
        );
        parent.children = vec![
            activity(
                "first",
                ToolActivityKind::Command,
                "a long first child command that wraps across several rows",
                ActivityState::Completed,
                Some(
                    serde_json::json!({"stdout":"a long first child output that wraps across several rows"}),
                ),
            ),
            activity(
                "second",
                ToolActivityKind::Edit,
                "second.rs",
                ActivityState::Completed,
                None,
            ),
        ];
        let projection = TranscriptProjection {
            items: vec![TranscriptItem::ActivityGroup(ActivityGroup {
                turn: 1,
                open: true,
                activities: vec![parent],
            })],
        };
        let rows = plain_rows(&projection, 24);
        let first = rows
            .iter()
            .position(|row| row.starts_with("  ├ Ran"))
            .unwrap();
        let last = rows
            .iter()
            .position(|row| row.starts_with("  └ Edited"))
            .unwrap();
        assert!(last > first + 2);
        assert!(
            rows[first + 1..last]
                .iter()
                .all(|row| row.starts_with("  │ ")),
            "{rows:?}"
        );
        assert!(
            rows[first + 1..last]
                .iter()
                .any(|row| row.starts_with("  │ └ "))
        );
        assert!(
            rows.iter()
                .all(|row| unicode_width::UnicodeWidthStr::width(row.as_str()) <= 24)
        );
    }

    #[test]
    fn shell_preview_shows_observed_output_outcome_and_context_choice() {
        let projection = TranscriptProjection {
            items: vec![TranscriptItem::UserShell(ion_core::UserShellActivity {
                command: "run-check".into(),
                outcome: ion_core::UserShellOutcome::Observed {
                    output: serde_json::json!({
                        "stdout": "first\nsecond\nthird\nfourth\nOBSERVED_OUTPUT\n",
                        "stderr": "\u{1b}[2JOBSERVED_FAILURE\n",
                        "exit_code": 7,
                        "stdout_truncated": true,
                        "stdout_full_path": "/tmp/capture.txt"
                    }),
                    is_error: true,
                },
                exclude_from_context: true,
            })],
        };
        let rendered = plain_rows(&projection, 120).join("\n");
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
        state: ActivityState,
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
            children: Vec::new(),
            state,
            result: result.map(|value| ActivityResult {
                projection: ion_core::ToolResultProjection::Observed,
                value,
                image_mime_types: Vec::new(),
                is_error: state != ActivityState::Completed,
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
                        ActivityState::Completed,
                        None,
                    ),
                    activity(
                        "r2",
                        ToolActivityKind::Read,
                        "src/b.rs",
                        ActivityState::Completed,
                        None,
                    ),
                    activity(
                        "r3",
                        ToolActivityKind::Read,
                        "src/c.rs",
                        ActivityState::Completed,
                        None,
                    ),
                    activity(
                        "e1",
                        ToolActivityKind::Edit,
                        "src/parser.rs",
                        ActivityState::Completed,
                        Some(serde_json::json!({"replacements":2})),
                    ),
                    activity(
                        "x1",
                        ToolActivityKind::Command,
                        "cargo test",
                        ActivityState::Completed,
                        Some(
                            serde_json::json!({"stdout":"running\ntest result: ok. 148 passed","stderr":""}),
                        ),
                    ),
                ],
            })],
        };
        let rendered = plain_rows(&projection, 100).join("\n");
        for count in ["• 5 actions", "3 read", "1 edit", "1 command"] {
            assert!(rendered.contains(count));
        }
        assert!(rendered.contains("├ Read src/a.rs, src/b.rs, src/c.rs"));
        assert!(rendered.contains("├ Edited src/parser.rs · 2 replacements"));
        assert!(rendered.contains("└ Ran cargo test"));
        assert!(rendered.contains("  └ test result: ok. 148 passed"));
        assert_eq!(rendered.matches("Read src/").count(), 1);
    }

    #[test]
    fn withheld_result_notice_survives_long_subject_without_overflow() {
        for projection in [
            ion_core::ToolResultProjection::RequestLimitExceeded,
            ion_core::ToolResultProjection::ImagesUnsupported,
        ] {
            let mut read = activity(
                "x",
                ToolActivityKind::Read,
                &"long/path/".repeat(30),
                ActivityState::Completed,
                Some(serde_json::json!({"path":"observed"})),
            );
            read.result.as_mut().unwrap().projection = projection;
            let history = TranscriptProjection {
                items: vec![TranscriptItem::ActivityGroup(ActivityGroup {
                    turn: 1,
                    open: false,
                    activities: vec![read],
                })],
            };
            for rendered in [plain_rows(&history, 80), plain_live_rows(&history, 80, 16)] {
                let rendered = rendered.join("\n");
                assert!(rendered.contains("not shared with model"), "{rendered}");
                assert!(rendered.contains("Read"), "{rendered}");
                assert!(!rendered.contains("Read failed"), "{rendered}");
            }
        }
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
                    ActivityState::Failed,
                    Some(serde_json::json!({"exit_code":1,"stdout":"","stderr":"compile failed"})),
                )],
            })],
        };
        let rendered = plain_rows(&projection, 80).join("\n");
        assert!(rendered.contains("• Exited 1 cargo test"));
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
            plain_rows(&projection, 80),
            vec!["› fix the parser", "", "I found the issue."]
        );
    }

    #[test]
    fn live_budget_keeps_earlier_exception_and_mutations_across_narrative() {
        let mut activities = vec![activity(
            "failed",
            ToolActivityKind::Command,
            "FAILURE_MARKER",
            ActivityState::Failed,
            Some(serde_json::json!({"exit_code": 7})),
        )];
        activities.extend((0..14).map(|index| {
            activity(
                &format!("write-{index}"),
                ToolActivityKind::Write,
                "changed.txt",
                ActivityState::Completed,
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
                        ActivityState::Queued,
                        None,
                    )],
                }),
            ],
        };
        for width in [24, 100] {
            let rendered = plain_live_rows(&projection, width, 7);
            assert!(rendered.len() <= 7);
            let text = rendered.join("\n");
            for fact in [
                "16 actions",
                "1 failed",
                "1 queued",
                "14 write",
                // Severity leads the pinned row; narrow subjects may be fitted.
                "Exited 7",
                "FAILURE_MA",
                "CURRENT_READ",
            ] {
                assert!(text.contains(fact), "missing {fact}: {text}");
            }
        }
        let tiny = plain_live_rows(&projection, 100, 2);
        assert_eq!(tiny.len(), 2);
        assert!(tiny.join("\n").contains("FAILURE_MARKER"));
        assert_eq!(
            plain_live_rows(&projection, 100, 100),
            plain_rows(&projection, 100)
        );
        assert!(plain_live_rows(&projection, 100, 0).is_empty());
    }

    #[test]
    fn nested_failure_remains_visible_under_a_running_parent() {
        let mut parent = activity(
            "code",
            ToolActivityKind::External,
            "JavaScript",
            ActivityState::Running,
            None,
        );
        parent.children = vec![activity(
            "read",
            ToolActivityKind::Read,
            "CHILD_FAILURE_LONG_PATH",
            ActivityState::Failed,
            Some(serde_json::json!({"error":"missing"})),
        )];
        let projection = TranscriptProjection {
            items: vec![TranscriptItem::ActivityGroup(ActivityGroup {
                turn: 1,
                open: true,
                activities: vec![parent],
            })],
        };
        let full = plain_rows(&projection, 80).join("\n");
        assert!(
            full.contains("1 child calls") && full.contains("1 exceptions"),
            "{full}"
        );
        assert!(
            full.contains("└ Read failed CHILD_FAILURE_LONG_PATH"),
            "{full}"
        );
        let tiny = plain_live_rows(&projection, 24, 2).join("\n");
        assert!(
            tiny.contains("Read failed") && tiny.contains("CHILD"),
            "{tiny}"
        );
    }

    #[test]
    fn exception_pin_uses_source_location_when_call_ids_are_reused() {
        let projection = TranscriptProjection {
            items: vec![
                TranscriptItem::ActivityGroup(ActivityGroup {
                    turn: 1,
                    open: true,
                    activities: vec![
                        activity(
                            "reused",
                            ToolActivityKind::Read,
                            "FIRST_READ",
                            ActivityState::Completed,
                            None,
                        ),
                        activity(
                            "read-2",
                            ToolActivityKind::Read,
                            "SECOND_READ",
                            ActivityState::Completed,
                            None,
                        ),
                        activity(
                            "reused",
                            ToolActivityKind::Command,
                            "REUSED_FAILURE",
                            ActivityState::Failed,
                            None,
                        ),
                        activity(
                            "running",
                            ToolActivityKind::Command,
                            "ACTIVE_COMMAND",
                            ActivityState::Running,
                            None,
                        ),
                        activity(
                            "queued",
                            ToolActivityKind::Write,
                            "LATER_WRITE",
                            ActivityState::Queued,
                            None,
                        ),
                    ],
                }),
                TranscriptItem::Assistant(TranscriptMessage {
                    turn: Some(1),
                    steering: false,
                    parts: vec![TranscriptPart::Text("later narrative\n".repeat(20))],
                }),
            ],
        };
        let rendered = plain_live_rows(&projection, 100, 7).join("\n");
        assert_eq!(rendered.matches("REUSED_FAILURE").count(), 1, "{rendered}");
        assert!(
            rendered.contains("Read FIRST_READ, SECOND_READ"),
            "{rendered}"
        );
        assert!(rendered.contains("Running ACTIVE_COMMAND"), "{rendered}");
    }

    #[test]
    fn running_call_keeps_its_tree_when_later_calls_are_queued() {
        let projection = TranscriptProjection {
            items: vec![TranscriptItem::ActivityGroup(ActivityGroup {
                turn: 1,
                open: true,
                activities: vec![
                    activity(
                        "running",
                        ToolActivityKind::Command,
                        "ACTIVE_COMMAND",
                        ActivityState::Running,
                        None,
                    ),
                    activity(
                        "queued",
                        ToolActivityKind::Read,
                        "LATER_READ",
                        ActivityState::Queued,
                        None,
                    ),
                    activity(
                        "queued-2",
                        ToolActivityKind::Write,
                        "LATER_WRITE",
                        ActivityState::Queued,
                        None,
                    ),
                ],
            })],
        };
        let rendered = plain_live_rows(&projection, 40, 3).join("\n");
        for fact in [
            "1 running",
            "2 queued",
            "Current activity",
            "Running ACTIVE_COMMAND",
        ] {
            assert!(rendered.contains(fact), "{rendered}");
        }
        assert!(!rendered.contains("Reading LATER_READ"), "{rendered}");
        assert!(
            rendered
                .lines()
                .any(|line| line.starts_with("└ ") && line.contains("ACTIVE_COMMAND"))
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
                    ActivityState::Completed,
                    None,
                )
            })
            .collect::<Vec<_>>();
        activities.push(activity(
            "edit",
            ToolActivityKind::Edit,
            "important.rs",
            ActivityState::Completed,
            Some(serde_json::json!({"replacements":1})),
        ));
        activities.push(activity(
            "failed",
            ToolActivityKind::Command,
            "cargo test",
            ActivityState::Failed,
            Some(serde_json::json!({"exit_code":1,"stderr":"failed"})),
        ));
        let projection = TranscriptProjection {
            items: vec![TranscriptItem::ActivityGroup(ActivityGroup {
                turn: 1,
                open: false,
                activities,
            })],
        };
        let rendered = plain_rows(&projection, 100).join("\n");
        assert!(rendered.contains("Edited important.rs"));
        assert!(rendered.contains("Exited 1 cargo test"));
        assert!(rendered.contains("Read file-0.rs, file-1.rs, file-2.rs · +17 more"));
    }
}
