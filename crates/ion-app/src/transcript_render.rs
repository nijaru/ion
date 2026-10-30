//! Pure compact rendering for the typed coding transcript.
use crate::{
    display_text::fit_line,
    presentation_style::{Role, Theme},
    tool_output::{OutputDetail, RenderOptions},
};
use ion_core::{
    ActivityGroup, ActivityState, ToolActivityKind, TranscriptActivity, TranscriptItem,
    TranscriptMessage, TranscriptPart, UserShellActivity,
};
use ratatui::{
    style::Style,
    text::{Line, Span},
};

#[cfg(test)]
use ion_core::TranscriptProjection;
#[cfg(test)]
use ratatui::style::Modifier;

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

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) enum ThinkingVisibility {
    #[default]
    Hidden,
    Visible,
}

impl ThinkingVisibility {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Hidden => "Hidden",
            Self::Visible => "Shown",
        }
    }
}

const MAX_COALESCED_SUBJECTS: usize = 3;

pub fn rows(
    items: &[TranscriptItem],
    width: usize,
    detail: &RenderOptions<'_>,
) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut rows = Vec::new();
    for item in items {
        if hidden_thinking(item, detail.thinking) {
            continue;
        }
        if !rows.is_empty() && rows.last().is_some_and(|row: &Line<'_>| row.width() > 0) {
            rows.push(Line::default());
        }
        match item {
            TranscriptItem::Readiness { turn, notices } => {
                render_readiness(&mut rows, *turn, notices, width, detail.theme);
            }
            TranscriptItem::User(message) => {
                render_message(&mut rows, message, true, width, detail)
            }
            TranscriptItem::Assistant(message) => {
                render_message(&mut rows, message, false, width, detail)
            }
            TranscriptItem::ActivityGroup(group) => render_group(&mut rows, group, width, detail),
            TranscriptItem::UserShell(shell) => render_shell(&mut rows, shell, width, detail),
        }
    }
    while rows.last().is_some_and(|row| row.width() == 0) {
        rows.pop();
    }
    rows
}

pub(super) fn readiness_text(turn: u64, notices: &[ion_core::CompletionNotice]) -> String {
    let mut text = format!(
        "Managed-work readiness · Turn {turn} · not a user instruction; reports not acknowledged"
    );
    for notice in notices {
        use std::fmt::Write;
        write!(
            text,
            "\nSource {} · run {} · child {}",
            notice.source_session, notice.run, notice.child_session
        )
        .expect("String write");
    }
    text
}
fn render_readiness(
    rows: &mut Vec<Line<'static>>,
    turn: u64,
    notices: &[ion_core::CompletionNotice],
    width: usize,
    theme: &Theme,
) {
    let start = rows.len();
    push_wrapped(rows, &readiness_text(turn, notices), width);
    for row in &mut rows[start..] {
        row.style = theme.style(Role::Secondary);
    }
}

fn hidden_thinking(item: &TranscriptItem, thinking: ThinkingVisibility) -> bool {
    thinking == ThinkingVisibility::Hidden
        && matches!(item, TranscriptItem::Assistant(message) if message.parts.iter().all(|part| matches!(part, TranscriptPart::Thinking(_))))
}

/// Select a semantic focus before rendering its children. Never take a suffix
/// of the flattened conversation: that can detach branches from their root.
pub(super) fn live_rows(
    items: &[TranscriptItem],
    width: usize,
    budget: usize,
    detail: &RenderOptions<'_>,
) -> Vec<Line<'static>> {
    if budget == 0 {
        return Vec::new();
    }
    // Expanded capture is for publication/fullscreen. Inline progress stays a
    // compact semantic preview instead of allocating a whole capture each tick.
    let rendered = rows(items, width, detail);
    if rendered.len() <= budget {
        return rendered;
    }
    let activities = || {
        items.iter().enumerate().flat_map(|(item_index, item)| {
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
                "{} · {}",
                group_header(activities().map(|(_, _, activity)| activity)),
                detail.details_key
            ),
            width.max(1),
        );
        for row in &mut selected {
            row.style = detail.theme.style(Role::Heading);
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
                    detail.theme,
                ),
            ));
        }
    } else {
        selected.push(Line::styled(
            fit_line(
                &format!("… earlier conversation · {}", detail.details_key),
                width,
            ),
            detail.theme.style(Role::Secondary),
        ));
    }
    let remaining = budget.saturating_sub(selected.len());
    let focus = items.iter().enumerate().rfind(|(_, item)| {
        matches!(item, TranscriptItem::ActivityGroup(group) if group.activities.iter().any(|a| a.state == ActivityState::Running))
    }).or_else(|| items.iter().enumerate().rfind(|(_, item)| !hidden_thinking(item, detail.thinking)));
    if remaining > 0
        && let Some((item_index, focus)) = focus
    {
        match focus {
            TranscriptItem::Readiness { turn, notices } => {
                let mut preview = Vec::new();
                render_readiness(&mut preview, *turn, notices, width, detail.theme);
                selected.extend(preview.into_iter().take(remaining));
            }
            TranscriptItem::ActivityGroup(group) => {
                let pinned_index = pinned
                    .filter(|(index, _)| *index == item_index)
                    .map(|(_, index)| index);
                render_current_group(&mut selected, group, width, remaining, pinned_index, detail)
            }
            TranscriptItem::User(message) | TranscriptItem::Assistant(message) => {
                let user = matches!(focus, TranscriptItem::User(_));
                let mut preview = Vec::new();
                render_message(&mut preview, message, user, width, detail);
                let omitted = preview.len().saturating_sub(remaining);
                if omitted > 0 {
                    preview.drain(..omitted);
                    if let Some(first) = preview.first_mut() {
                        let prefix = if user && message.steering {
                            "↳ … "
                        } else if message
                            .parts
                            .iter()
                            .all(|part| matches!(part, TranscriptPart::Thinking(_)))
                        {
                            "Thinking · … "
                        } else {
                            "… "
                        };
                        first.spans.insert(0, Span::raw(prefix));
                        let clipped = first.width() > width;
                        if width <= 1 {
                            first.spans = vec![Span::raw("…")];
                        } else {
                            let mut wrapped = Vec::new();
                            crate::display_text::push_styled(
                                &mut wrapped,
                                "",
                                "",
                                std::mem::take(first),
                                if clipped { width - 1 } else { width },
                            );
                            // Only this leading row is clipped; later retained rows stay intact.
                            *first = wrapped.into_iter().next().expect("wrapped omission row");
                            if clipped {
                                first.spans.push(Span::raw("…"));
                            }
                        }
                    }
                }
                selected.extend(preview);
            }
            TranscriptItem::UserShell(shell) => {
                let mut preview = Vec::new();
                render_shell(&mut preview, shell, width, detail);
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
    detail: &RenderOptions<'_>,
) {
    let display = display_activities(&group.activities, OutputDetail::Compact);
    let capacity = budget.saturating_sub(1);
    let mut indices = (0..display.len())
        .filter(|&index| Some(display[index].source.start) != pinned_index)
        .collect::<Vec<_>>();
    if indices.is_empty() {
        return;
    }
    rows.push(Line::styled(
        fit_line("• Current activity", width),
        detail.theme.style(Role::Heading),
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
            display[index].style(detail.theme),
        ));
    }
    if reserve_omission > 0 {
        rows.push(Line::styled(
            fit_line(&format!("└ {omitted} more · {}", detail.details_key), width),
            detail.theme.style(Role::Secondary),
        ));
    }
}

pub(super) fn render_message(
    rows: &mut Vec<Line<'static>>,
    message: &TranscriptMessage,
    user: bool,
    width: usize,
    detail: &RenderOptions<'_>,
) {
    if user {
        let start = rows.len();
        let rail = width > 2;
        // The rail replaces the prompt chevron, but not steering's provenance arrow.
        render_source_message(
            rows,
            message,
            message.steering,
            if rail { width - 2 } else { width },
        );
        for row in &mut rows[start..] {
            row.style = detail
                .theme
                .style(Role::Text)
                .add_modifier(ratatui::style::Modifier::BOLD);
            if rail {
                row.spans
                    .insert(0, Span::styled("┃ ", detail.theme.style(Role::UserInput)));
            }
        }
        return;
    }
    for part in &message.parts {
        match part {
            TranscriptPart::Thinking(text) => {
                if detail.thinking == ThinkingVisibility::Visible {
                    render_thinking(rows, text, width, detail.theme);
                }
            }
            TranscriptPart::Text(text) => crate::markdown::render(rows, text, width, detail.theme),
            TranscriptPart::Image { mime_type } => {
                push_wrapped(rows, &format!("[image: {mime_type}]"), width)
            }
        }
    }
}

fn render_thinking(rows: &mut Vec<Line<'static>>, text: &str, width: usize, theme: &Theme) {
    rows.push(Line::styled(
        fit_line("Thinking", width),
        theme.style(Role::Secondary),
    ));
    crate::display_text::push_styled(
        rows,
        "  ",
        "  ",
        Line::from(Span::styled(text, theme.style(Role::Secondary))),
        width,
    );
}

/// Inspection keeps the original human-visible source, not the formatted view.
pub(super) fn render_source_message(
    rows: &mut Vec<Line<'static>>,
    message: &TranscriptMessage,
    user: bool,
    width: usize,
) {
    let mut first = true;
    for part in &message.parts {
        match part {
            TranscriptPart::Thinking(text) => {
                push_wrapped(rows, "Thinking", width);
                push_prefixed(rows, "  ", "  ", text, width);
            }
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

fn render_shell(
    rows: &mut Vec<Line<'static>>,
    shell: &UserShellActivity,
    width: usize,
    detail: &RenderOptions<'_>,
) {
    let prefix = if shell.exclude_from_context {
        "› !!"
    } else {
        "› !"
    };
    let start = rows.len();
    push_prefixed(rows, prefix, "  ", &shell.command, width);
    for row in &mut rows[start..] {
        row.style = detail.theme.style(Role::UserInput);
    }
    let ion_core::UserShellOutcome::Observed { output, is_error } = &shell.outcome else {
        if shell.exclude_from_context {
            push_wrapped(rows, "  not shared with model", width);
        }
        let start = rows.len();
        push_wrapped(rows, ion_core::UserShellOutcome::unknown_notice(), width);
        for row in &mut rows[start..] {
            row.style = detail.theme.style(Role::Warning);
        }
        return;
    };
    crate::tool_output::render_command(rows, output, "  ", width, detail, 4);
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
        detail.theme.style(Role::Error)
    } else {
        detail.theme.style(Role::Secondary)
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
    fn style(&self, theme: &Theme) -> Style {
        activity_style(
            self.state,
            self.model_notice.is_some(),
            self.observation,
            theme,
        )
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

fn render_group(
    rows: &mut Vec<Line<'static>>,
    group: &ActivityGroup,
    width: usize,
    detail: &RenderOptions<'_>,
) {
    if group.activities.is_empty() {
        return;
    }
    let theme = detail.theme;
    let display = display_activities(&group.activities, detail.detail);

    if group.activities.len() == 1 {
        let item = &display[0];
        push_activity(rows, "• ", "  ", item, width, theme);
        if group.activities[0].children.is_empty()
            && group.activities[0].activity.kind != ToolActivityKind::Command
            && let Some(detail) = &item.detail
        {
            push_detail(rows, "  └ ", "    ", detail, width, theme);
        }
        crate::tool_output::render_activity(rows, &group.activities[0], "  ", width, detail);
        crate::edit_diff::render(rows, &group.activities[0], "  ", width, detail);
        render_children(rows, &group.activities[0].children, "  ", width, detail);
        return;
    }

    rows.push(Line::styled(
        fit_line(&group_header(group.activities.iter()), width),
        theme.style(Role::Heading),
    ));
    let total_children = display.len();
    for (index, item) in display.iter().enumerate() {
        let last = index + 1 == total_children;
        let branch = if last { "└ " } else { "├ " };
        push_activity(
            rows,
            branch,
            if last { "  " } else { "│ " },
            item,
            width,
            theme,
        );
        if group.activities[item.source.start].children.is_empty()
            && group.activities[item.source.start].activity.kind != ToolActivityKind::Command
            && let Some(detail) = &item.detail
        {
            let prefix = if last { "  └ " } else { "│ └ " };
            push_detail(
                rows,
                prefix,
                if last { "    " } else { "│   " },
                detail,
                width,
                theme,
            );
        }
        if item.source.len() == 1 {
            crate::tool_output::render_activity(
                rows,
                &group.activities[item.source.start],
                if last { "  " } else { "│ " },
                width,
                detail,
            );
        } else {
            crate::tool_output::render_folded_reads(
                rows,
                &group.activities[item.source.clone()],
                if last { "  " } else { "│ " },
                width,
                detail,
            );
        }
        crate::edit_diff::render(
            rows,
            &group.activities[item.source.start],
            if last { "  " } else { "│ " },
            width,
            detail,
        );
        render_children(
            rows,
            &group.activities[item.source.start].children,
            if last { "  " } else { "│ " },
            width,
            detail,
        );
    }
}

fn render_children(
    rows: &mut Vec<Line<'static>>,
    children: &[TranscriptActivity],
    prefix: &str,
    width: usize,
    detail: &RenderOptions<'_>,
) {
    let theme = detail.theme;
    let display = display_activities(children, detail.detail);
    for (index, item) in display.iter().enumerate() {
        let last = index + 1 == display.len();
        let continuation = format!("{prefix}{}", if last { "  " } else { "│ " });
        push_activity(
            rows,
            &format!("{prefix}{}", if last { "└ " } else { "├ " }),
            &continuation,
            item,
            width,
            theme,
        );
        crate::edit_diff::render(
            rows,
            &children[item.source.start],
            &continuation,
            width,
            detail,
        );
        if item.source.len() == 1 {
            crate::tool_output::render_activity(
                rows,
                &children[item.source.start],
                &continuation,
                width,
                detail,
            );
        } else {
            crate::tool_output::render_folded_reads(
                rows,
                &children[item.source.clone()],
                &continuation,
                width,
                detail,
            );
        }
        if children[item.source.start].activity.kind != ToolActivityKind::Command
            && let Some(detail) = &item.detail
        {
            push_detail(
                rows,
                &format!("{continuation}└ "),
                &format!("{continuation}  "),
                detail,
                width,
                theme,
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

fn display_activities(
    activities: &[TranscriptActivity],
    detail: OutputDetail,
) -> Vec<DisplayActivity> {
    if detail == OutputDetail::Expanded {
        return activities
            .iter()
            .enumerate()
            .map(|(index, activity)| display_activity(activity, index))
            .collect();
    }
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
    activity.result.as_ref().and_then(|result| {
        result
            .projection
            .and_then(ion_core::ToolResultProjection::notice)
    })
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
    let detail = activity
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

fn activity_style(state: ActivityState, notice: bool, observation: bool, theme: &Theme) -> Style {
    let role = if notice {
        Role::Warning
    } else {
        match state {
            ActivityState::Failed | ActivityState::Rejected => Role::Error,
            ActivityState::Unknown | ActivityState::Cancelled | ActivityState::TimedOut => {
                Role::Warning
            }
            ActivityState::Running => Role::Running,
            ActivityState::Completed if !observation => Role::Heading,
            ActivityState::Queued | ActivityState::Completed => Role::Secondary,
        }
    };
    theme.style(role)
}
fn push_prefixed(
    rows: &mut Vec<Line<'static>>,
    prefix: &str,
    continuation: &str,
    text: &str,
    width: usize,
) {
    crate::display_text::push_styled(
        rows,
        prefix,
        continuation,
        Line::from(Span::raw(text)),
        width,
    );
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
    theme: &Theme,
) {
    let start = rows.len();
    push_prefixed(rows, prefix, continuation, &item.summary, width);
    for row in &mut rows[start..] {
        row.style = item.style(theme);
    }
}
fn push_detail(
    rows: &mut Vec<Line<'static>>,
    prefix: &str,
    continuation: &str,
    text: &str,
    width: usize,
    theme: &Theme,
) {
    let start = rows.len();
    push_prefixed(rows, prefix, continuation, text, width);
    for row in &mut rows[start..] {
        row.style = theme.style(Role::Secondary);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    fn distinct_theme() -> Theme {
        let mut theme = Theme::default();
        for (role, color) in [
            (Role::Heading, Color::Rgb(1, 2, 3)),
            (Role::Secondary, Color::Rgb(4, 5, 6)),
            (Role::Error, Color::Rgb(7, 8, 9)),
            (Role::Running, Color::Rgb(10, 11, 12)),
            (Role::Added, Color::Rgb(13, 14, 15)),
            (Role::Removed, Color::Rgb(16, 17, 18)),
            (Role::Hunk, Color::Rgb(19, 20, 21)),
            (Role::Text, Color::Rgb(22, 23, 24)),
            (Role::Code, Color::Rgb(25, 26, 27)),
            (Role::Link, Color::Rgb(28, 29, 30)),
            (Role::SyntaxKeyword, Color::Rgb(31, 32, 33)),
            (Role::SyntaxString, Color::Rgb(34, 35, 36)),
        ] {
            theme.set_color(role, color);
        }
        theme
    }

    fn has_color(rows: &[Line<'_>], text: &str, color: Color) -> bool {
        rows.iter().any(|row| {
            row.spans.iter().any(|span| {
                span.content.contains(text) && row.style.patch(span.style).fg == Some(color)
            })
        })
    }

    #[test]
    fn custom_theme_reaches_nested_markdown_without_theming_source_inspection() {
        let theme = distinct_theme();
        let keys = crate::keybindings::KeyBindings::default();
        let options = RenderOptions::new(
            OutputDetail::Compact,
            &keys,
            &theme,
            ThinkingVisibility::Visible,
        );
        let message = TranscriptMessage {
            turn: Some(1),
            steering: false,
            parts: vec![
                TranscriptPart::Text("body\n\n> - | Field | Value |\n>   | --- | --- |\n>   | table | `inline` [link](https://example.org) |\n>\n>   ```rust\n>   let n = \"value\";\n>   ```\n\nafter".into()),
                TranscriptPart::Thinking("thought".into()),
            ],
        };
        let items = [TranscriptItem::Assistant(message.clone())];
        // Exercise aligned and stacked table cells as well as a nested code block.
        for width in [24, 120] {
            let rendered = rows(&items, width, &options);
            assert!(rendered.iter().all(|row| row.width() <= width));
            for (text, color) in [
                ("body", Color::Rgb(22, 23, 24)),
                ("table", Color::Rgb(22, 23, 24)),
                ("inline", Color::Rgb(25, 26, 27)),
                ("link", Color::Rgb(28, 29, 30)),
                ("code · rust", Color::Rgb(4, 5, 6)),
                ("let", Color::Rgb(31, 32, 33)),
                ("value", Color::Rgb(34, 35, 36)),
                ("after", Color::Rgb(22, 23, 24)),
                ("thought", Color::Rgb(4, 5, 6)),
            ] {
                assert!(
                    has_color(&rendered, text, color),
                    "missing themed {text} at width {width}: {rendered:?}"
                );
            }
        }
        for user in [false, true] {
            let mut source = Vec::new();
            render_source_message(&mut source, &message, user, 120);
            assert!(
                source
                    .iter()
                    .any(|row| row.to_string().contains("`inline`"))
            );
            assert!(source.iter().all(|row| row.style == Style::default()
                && row.spans.iter().all(|span| span.style == Style::default())));
        }
    }

    #[test]
    fn custom_theme_styles_activity_and_diff_semantics_but_not_observations() {
        use serde_json::json;
        let theme = distinct_theme();
        let keys = crate::keybindings::KeyBindings::default();
        let mut parent = activity(
            "parent",
            ToolActivityKind::External,
            "composition",
            ActivityState::Running,
            None,
        );
        parent.children = vec![
            activity(
                "command",
                ToolActivityKind::Command,
                "test",
                ActivityState::Failed,
                Some(
                    json!({"exit_code":7,"stdout":"OBSERVED_NATIVE","stderr":"OBSERVED_ERROR","error":"OBSERVED_TOOL_ERROR"}),
                ),
            ),
            activity(
                "read",
                ToolActivityKind::Read,
                "file",
                ActivityState::Completed,
                Some(json!({"content":"OBSERVED_READ"})),
            ),
        ];
        let items = [TranscriptItem::ActivityGroup(ActivityGroup {
            turn: 1,
            open: true,
            activities: vec![
                parent,
                activity(
                    "r1",
                    ToolActivityKind::Read,
                    "one",
                    ActivityState::Completed,
                    Some(json!({"content":"OBSERVED_ONE","start_line":2,"next_line":3})),
                ),
                activity(
                    "r2",
                    ToolActivityKind::Read,
                    "two",
                    ActivityState::Completed,
                    Some(json!({"content":"OBSERVED_TWO","has_more":true})),
                ),
                activity(
                    "edit",
                    ToolActivityKind::Edit,
                    "file",
                    ActivityState::Completed,
                    Some(
                        json!({"diff":{"text":"--- old\n+++ new\n@@ hunk\n-old\n+new","truncated":true}}),
                    ),
                ),
            ],
        })];
        for detail in [OutputDetail::Compact, OutputDetail::Expanded] {
            let options = RenderOptions::new(detail, &keys, &theme, ThinkingVisibility::Hidden);
            let rendered = rows(&items, 160, &options);
            for (text, color) in [
                ("• 4 actions", Color::Rgb(1, 2, 3)),
                ("composition", Color::Rgb(10, 11, 12)),
                ("Exited 7", Color::Rgb(7, 8, 9)),
            ] {
                assert!(
                    has_color(&rendered, text, color),
                    "missing themed {text}: {rendered:?}"
                );
            }
            for (text, color) in [
                ("stdout", Color::Rgb(4, 5, 6)),
                ("diff capture truncated", Color::Rgb(4, 5, 6)),
                ("@@ hunk", Color::Rgb(19, 20, 21)),
                ("-old", Color::Rgb(16, 17, 18)),
                ("+new", Color::Rgb(13, 14, 15)),
            ] {
                assert!(has_color(&rendered, text, color), "missing themed {text}");
            }
            for text in [
                "OBSERVED_NATIVE",
                "OBSERVED_ERROR",
                "OBSERVED_TOOL_ERROR",
                "OBSERVED_READ",
            ] {
                let row = rendered
                    .iter()
                    .find(|row| row.to_string().contains(text))
                    .unwrap();
                assert_eq!(row.style, Style::default(), "{text}");
                assert!(
                    row.spans.iter().all(|span| span.style == Style::default()),
                    "{text}"
                );
            }
            if detail == OutputDetail::Compact {
                for text in [
                    "read outputs folded",
                    "earlier lines not recorded",
                    "file continues",
                ] {
                    assert!(
                        has_color(&rendered, text, Color::Rgb(4, 5, 6)),
                        "missing themed {text}"
                    );
                }
            }
            let live = live_rows(&items, 160, 4, &options);
            assert!(
                live.iter()
                    .any(|row| row.style.fg == Some(Color::Rgb(1, 2, 3)))
            );
            assert!(live.iter().any(|row| row.to_string().contains("Exited 7")
                && row.style.fg == Some(Color::Rgb(7, 8, 9))));
        }
    }

    #[test]
    fn disclosure_hints_follow_configured_or_disabled_detail_action() {
        use serde_json::json;
        let activities = vec![
            activity(
                "r1",
                ToolActivityKind::Read,
                "one",
                ActivityState::Completed,
                Some(json!({"content":"one"})),
            ),
            activity(
                "r2",
                ToolActivityKind::Read,
                "two",
                ActivityState::Completed,
                Some(json!({"content":"two"})),
            ),
            activity(
                "c",
                ToolActivityKind::Command,
                "test",
                ActivityState::Completed,
                Some(json!({"stdout":"1\n2\n3\n4\n5\n6", "stdout_truncated":true})),
            ),
            activity(
                "e",
                ToolActivityKind::Edit,
                "file",
                ActivityState::Completed,
                Some(
                    json!({"diff":{"text":"--- old\n+++ new\n@@ hunk\n-1\n+2\n-3\n+4\n-5\n+6\n-7\n+8\n-9\n+10", "truncated":false}}),
                ),
            ),
        ];
        let items = [TranscriptItem::ActivityGroup(ActivityGroup {
            turn: 1,
            open: false,
            activities,
        })];
        for (config, expected) in [
            (r#"{"app.tools.expand":"f8"}"#, "F8"),
            (r#"{"app.tools.expand":[]}"#, "unbound"),
        ] {
            let keys = crate::keybindings::KeyBindings::parse(config.as_bytes()).unwrap();
            let theme = Theme::default();
            let options = RenderOptions::new(
                OutputDetail::Compact,
                &keys,
                &theme,
                ThinkingVisibility::Hidden,
            );
            let published = rows(&items, 160, &options);
            for label in [
                "read outputs folded",
                "earlier stdout",
                "stdout truncated",
                "more diff",
            ] {
                let notice = published
                    .iter()
                    .find(|row| row.to_string().contains(label))
                    .unwrap();
                assert!(notice.to_string().contains(expected), "{notice}");
            }
            let live = live_rows(&items, 160, 4, &options);
            assert!(live.iter().any(|row| row.to_string().contains(expected)));
            assert!(
                published
                    .iter()
                    .chain(&live)
                    .all(|row| !row.to_string().contains("Ctrl-O"))
            );
        }
    }

    #[test]
    fn thinking_visibility_preserves_literal_source_and_wrapping() {
        let projection = TranscriptProjection {
            items: vec![
                TranscriptItem::Assistant(TranscriptMessage {
                    turn: Some(1),
                    steering: false,
                    parts: vec![TranscriptPart::Text("answer".into())],
                }),
                TranscriptItem::Assistant(TranscriptMessage {
                    turn: Some(1),
                    steering: false,
                    parts: vec![TranscriptPart::Thinking(
                        "**human thought**\nnext\u{1b}[2J".into(),
                    )],
                }),
            ],
        };
        for width in [1, 8, 24, 80] {
            let hidden = rows(
                &projection.items,
                width,
                &RenderOptions::new(
                    OutputDetail::Expanded,
                    &crate::keybindings::KeyBindings::default(),
                    &Theme::default(),
                    ThinkingVisibility::Hidden,
                ),
            );
            assert!(
                !hidden
                    .iter()
                    .any(|line| line.to_string().contains("thought"))
            );
            assert_eq!(
                live_rows(
                    &projection.items,
                    width,
                    3,
                    &RenderOptions::new(
                        OutputDetail::Compact,
                        &crate::keybindings::KeyBindings::default(),
                        &Theme::default(),
                        ThinkingVisibility::Hidden,
                    )
                ),
                live_rows(
                    &projection.items[..1],
                    width,
                    3,
                    &RenderOptions::new(
                        OutputDetail::Compact,
                        &crate::keybindings::KeyBindings::default(),
                        &Theme::default(),
                        ThinkingVisibility::Hidden,
                    )
                )
            );
            let visible = rows(
                &projection.items,
                width,
                &RenderOptions::new(
                    OutputDetail::Compact,
                    &crate::keybindings::KeyBindings::default(),
                    &Theme::default(),
                    ThinkingVisibility::Visible,
                ),
            );
            assert!(
                visible
                    .iter()
                    .all(|line| line.width() <= width && !line.to_string().contains('\u{1b}'))
            );
        }
        let shown = rows(
            &projection.items,
            80,
            &RenderOptions::new(
                OutputDetail::Compact,
                &crate::keybindings::KeyBindings::default(),
                &Theme::default(),
                ThinkingVisibility::Visible,
            ),
        );
        assert!(shown.iter().any(|line| {
            line.to_string() == "  **human thought**"
                && line
                    .spans
                    .iter()
                    .filter(|span| !span.content.trim().is_empty())
                    .all(|span| !span.style.add_modifier.contains(Modifier::DIM))
        }));
        assert!(
            shown
                .iter()
                .any(|line| line.to_string().contains("next�[2J"))
        );
        let long = TranscriptProjection {
            items: vec![TranscriptItem::Assistant(TranscriptMessage {
                turn: Some(1),
                steering: false,
                parts: vec![TranscriptPart::Thinking("long thought\n".repeat(20))],
            })],
        };
        let preview = live_rows(
            &long.items,
            80,
            3,
            &RenderOptions::new(
                OutputDetail::Compact,
                &crate::keybindings::KeyBindings::default(),
                &Theme::default(),
                ThinkingVisibility::Visible,
            ),
        );
        assert_eq!(preview.len(), 3);
        assert!(
            preview
                .iter()
                .any(|line| line.to_string().starts_with("Thinking · … "))
        );
        let TranscriptItem::Assistant(message) = &projection.items[1] else {
            panic!()
        };
        let mut source = Vec::new();
        render_source_message(&mut source, message, false, 80);
        assert!(
            source
                .iter()
                .any(|line| line.to_string().contains("**human thought**"))
        );
    }

    fn plain_rows(projection: &TranscriptProjection, width: usize) -> Vec<String> {
        super::rows(
            &projection.items,
            width,
            &RenderOptions::new(
                OutputDetail::Compact,
                &crate::keybindings::KeyBindings::default(),
                &Theme::default(),
                ThinkingVisibility::Hidden,
            ),
        )
        .iter()
        .map(ToString::to_string)
        .collect()
    }
    fn plain_live_rows(
        projection: &TranscriptProjection,
        width: usize,
        budget: usize,
    ) -> Vec<String> {
        super::live_rows(
            &projection.items,
            width,
            budget,
            &RenderOptions::new(
                OutputDetail::Compact,
                &crate::keybindings::KeyBindings::default(),
                &Theme::default(),
                ThinkingVisibility::Hidden,
            ),
        )
        .iter()
        .map(ToString::to_string)
        .collect()
    }
    use ion_core::{ActivityResult, ToolActivity, TranscriptActivity};

    #[test]
    fn output_modes_disclose_ranges_streams_and_uncoalesced_observations() {
        let mut read = activity(
            "r1",
            ToolActivityKind::Read,
            "data.txt",
            ActivityState::Completed,
            Some(
                serde_json::json!({"content":"**literal**\nsecond\nthird\nFOURTH_READ", "start_line":64, "next_line":68, "file_bytes":256, "has_more":true}),
            ),
        );
        read.result.as_mut().unwrap().projection =
            Some(ion_core::ToolResultProjection::RequestLimitExceeded);
        let second = activity(
            "r2",
            ToolActivityKind::Read,
            "other.txt",
            ActivityState::Completed,
            Some(
                serde_json::json!({"content":"SECOND_READ", "start_line":4, "next_line":5, "file_bytes":15, "has_more":false}),
            ),
        );
        let command = activity(
            "c",
            ToolActivityKind::Command,
            "test",
            ActivityState::Failed,
            Some(
                serde_json::json!({"exit_code":7, "stdout":"FIRST_STDOUT\ntwo\nthree\nLAST_STDOUT", "stderr":"diagnostic\u{1b}[2J", "stdout_truncated":true, "stdout_full_path":"/capture/stdout"}),
            ),
        );
        let single = TranscriptProjection {
            items: vec![TranscriptItem::ActivityGroup(ActivityGroup {
                turn: 1,
                open: false,
                activities: vec![read.clone(), command.clone()],
            })],
        };
        let compact_rows = rows(
            &single.items,
            120,
            &RenderOptions::new(
                OutputDetail::Compact,
                &crate::keybindings::KeyBindings::default(),
                &Theme::default(),
                ThinkingVisibility::Hidden,
            ),
        );
        // Ranges, omission notices and capture paths are essential evidence,
        // not decoration whose legibility may depend on terminal faint support.
        assert!(compact_rows.iter().all(|row| {
            !row.style.add_modifier.contains(Modifier::DIM)
                && row
                    .spans
                    .iter()
                    .all(|span| !span.style.add_modifier.contains(Modifier::DIM))
        }));
        let compact = compact_rows
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            compact.contains("**literal**") && !compact.contains("FOURTH_READ"),
            "{compact}"
        );
        assert!(compact.contains("file continues beyond this recorded range"));
        assert!(compact.contains("lines 64–67") && compact.contains("earlier file lines"));
        assert!(compact.contains("LAST_STDOUT") && !compact.contains("FIRST_STDOUT"));
        assert!(
            compact.contains("stderr")
                && compact.contains("diagnostic�[2J")
                && !compact.contains('\u{1b}')
        );
        assert!(
            compact.contains("1 earlier stdout lines") && compact.contains("1 more content lines")
        );
        assert!(compact.contains("not shared with model") && compact.contains("Exited 7"));
        assert!(compact.contains("/capture/stdout") && compact.contains("stdout truncated"));
        let grouped = TranscriptProjection {
            items: vec![TranscriptItem::ActivityGroup(ActivityGroup {
                turn: 1,
                open: false,
                activities: vec![
                    read,
                    second,
                    activity(
                        "r3",
                        ToolActivityKind::Read,
                        "third.txt",
                        ActivityState::Completed,
                        Some(serde_json::json!({"content":"THIRD_READ"})),
                    ),
                    command,
                ],
            })],
        };
        let folded = plain_rows(&grouped, 120).join("\n");
        assert!(!folded.contains("SECOND_READ"));
        assert!(folded.contains("2 read outputs folded"));
        assert!(
            folded.contains("other.txt: content · lines 4–4")
                && folded.contains("earlier lines not recorded")
        );
        for width in [1, 8, 24, 120] {
            let expanded = rows(
                &grouped.items,
                width,
                &RenderOptions::new(
                    OutputDetail::Expanded,
                    &crate::keybindings::KeyBindings::default(),
                    &Theme::default(),
                    ThinkingVisibility::Hidden,
                ),
            );
            assert!(expanded.iter().all(|row| row.width() <= width));
            assert!(
                expanded
                    .iter()
                    .all(|row| !row.to_string().contains('\u{1b}'))
            );
        }
        let expanded = rows(
            &grouped.items,
            120,
            &RenderOptions::new(
                OutputDetail::Expanded,
                &crate::keybindings::KeyBindings::default(),
                &Theme::default(),
                ThinkingVisibility::Hidden,
            ),
        )
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
        for text in [
            "FOURTH_READ",
            "SECOND_READ",
            "FIRST_STDOUT",
            "**literal**",
            "not shared with model",
            "stdout truncated",
        ] {
            assert!(expanded.contains(text), "{expanded}");
        }
        assert!(!expanded.contains("earlier stdout lines"));
    }

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
        let rows = super::rows(
            &projection.items,
            24,
            &RenderOptions::new(
                OutputDetail::Compact,
                &crate::keybindings::KeyBindings::default(),
                &Theme::default(),
                ThinkingVisibility::Hidden,
            ),
        );
        let read = rows
            .iter()
            .position(|row| row.to_string().starts_with("├ Reading"))
            .unwrap();
        let failed = rows
            .iter()
            .position(|row| row.to_string().starts_with("└ Edit failed"))
            .unwrap();
        assert!(failed > read + 1);
        assert_eq!(
            rows[read].style.fg,
            Theme::default().style(Role::Running).fg
        );
        assert_eq!(
            rows[failed].style.fg,
            Theme::default().style(Role::Error).fg
        );
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
        let live = super::live_rows(
            &projection.items,
            24,
            6,
            &RenderOptions::new(
                OutputDetail::Compact,
                &crate::keybindings::KeyBindings::default(),
                &Theme::default(),
                ThinkingVisibility::Hidden,
            ),
        );
        assert!(
            live.iter()
                .any(|row| row.style.fg == Theme::default().style(Role::Error).fg)
        );
        assert!(
            live.iter()
                .any(|row| row.style.fg == Theme::default().style(Role::Running).fg)
        );
    }

    #[test]
    fn clipped_markdown_retains_inline_style_and_code_indentation() {
        for (source, width) in [
            ("**abcdefghijklmnoabcdefghijklmnopqrstuv**", 8),
            ("```\n  first\n  second\n  third\n  fourth\n```", 12),
        ] {
            let projection = TranscriptProjection {
                items: vec![TranscriptItem::Assistant(TranscriptMessage {
                    turn: Some(1),
                    steering: false,
                    parts: vec![TranscriptPart::Text(source.into())],
                })],
            };
            for narrow in 1..=width {
                assert!(
                    super::live_rows(
                        &projection.items,
                        narrow,
                        3,
                        &RenderOptions::new(
                            OutputDetail::Compact,
                            &crate::keybindings::KeyBindings::default(),
                            &Theme::default(),
                            ThinkingVisibility::Hidden,
                        )
                    )
                    .iter()
                    .all(|row| row.width() <= narrow)
                );
            }
            let rows = super::live_rows(
                &projection.items,
                width,
                3,
                &RenderOptions::new(
                    OutputDetail::Compact,
                    &crate::keybindings::KeyBindings::default(),
                    &Theme::default(),
                    ThinkingVisibility::Hidden,
                ),
            );
            let first = &rows[1];
            if source.starts_with("**") {
                assert!(
                    first
                        .spans
                        .iter()
                        .any(|span| span.style.add_modifier.contains(Modifier::BOLD)),
                    "{first:?}"
                );
            } else {
                assert!(first.to_string().starts_with("…     "), "{first:?}");
                assert!(
                    first
                        .spans
                        .iter()
                        .any(|span| span.style.fg == Theme::default().style(Role::Code).fg),
                    "{first:?}"
                );
            }
        }
    }

    #[test]
    fn markdown_is_assistant_presentation_not_user_or_inspection_source() {
        let source = "# Heading\n\n**bold** [site](https://example.org)\n\n| Key | Value |\n| --- | --- |\n| state | verified |";
        let message = TranscriptMessage {
            turn: Some(1),
            steering: false,
            parts: vec![TranscriptPart::Text(source.into())],
        };
        let mut formatted = Vec::new();
        render_message(
            &mut formatted,
            &message,
            false,
            80,
            &RenderOptions::new(
                OutputDetail::Compact,
                &crate::keybindings::KeyBindings::default(),
                &Theme::default(),
                ThinkingVisibility::Hidden,
            ),
        );
        let formatted = formatted
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(formatted.contains("Heading\n\nbold site (https://example.org)"));
        assert!(!formatted.contains("**"));
        assert!(formatted.contains("state │ verified"));
        assert!(!formatted.contains("| --- |"));
        let mut raw = Vec::new();
        render_source_message(&mut raw, &message, false, 80);
        assert_eq!(
            raw.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
            source
        );
        let mut user = Vec::new();
        render_message(
            &mut user,
            &message,
            true,
            80,
            &RenderOptions::new(
                OutputDetail::Compact,
                &crate::keybindings::KeyBindings::default(),
                &Theme::default(),
                ThinkingVisibility::Hidden,
            ),
        );
        let user = user
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(user.starts_with("┃ # Heading"));
        assert!(user.contains("**bold** [site](https://example.org)"));
        assert!(matches!(&message.parts[0], TranscriptPart::Text(text) if text == source));
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
                projection: Some(ion_core::ToolResultProjection::Observed),
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
        assert!(rendered.contains("  └ stdout"));
        assert!(rendered.contains("test result: ok. 148 passed"));
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
            read.result.as_mut().unwrap().projection = Some(projection);
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
        assert!(rendered.contains("└ stderr"));
        assert!(rendered.contains("compile failed"));
    }

    #[test]
    fn submitted_prompts_keep_literal_text_and_connected_rails_without_padding() {
        let theme = distinct_theme();
        let keys = crate::keybindings::KeyBindings::default();
        let options = RenderOptions::new(
            OutputDetail::Compact,
            &keys,
            &theme,
            ThinkingVisibility::Hidden,
        );
        let mut message = TranscriptMessage {
            turn: Some(1),
            steering: false,
            parts: vec![
                TranscriptPart::Text("**界e\u{0301}**\n\nnext\u{1b}".into()),
                TranscriptPart::Image {
                    mime_type: "image/png".into(),
                },
            ],
        };
        for steering in [false, true] {
            message.steering = steering;
            let mut rendered = Vec::new();
            render_message(&mut rendered, &message, true, 80, &options);
            let first = if steering {
                "┃ ↳ **界e\u{0301}**"
            } else {
                "┃ **界e\u{0301}**"
            };
            assert_eq!(rendered[0].to_string(), first);
            assert!(rendered.iter().all(|row| row.to_string().starts_with("┃ ")));
            assert!(rendered.iter().any(|row| row.to_string().trim_end() == "┃"));
            assert!(rendered.iter().any(|row| row.to_string().contains("next�")));
            assert!(
                rendered
                    .last()
                    .unwrap()
                    .to_string()
                    .ends_with("[image: image/png]")
            );
            for row in &rendered {
                assert_eq!(row.style.fg, theme.style(Role::Text).fg);
                assert!(row.style.add_modifier.contains(Modifier::BOLD));
                assert_eq!(row.spans[0].style.fg, theme.style(Role::UserInput).fg);
                assert_eq!(row.style.bg, None);
            }
            for width in 1..=16 {
                let mut narrow = Vec::new();
                render_message(&mut narrow, &message, true, width, &options);
                assert!(
                    narrow.iter().all(|row| row.width() <= width),
                    "{width}: {narrow:?}"
                );
                if width > 2 {
                    assert!(narrow.iter().all(|row| row.to_string().starts_with("┃ ")));
                }
            }
            let mut source = Vec::new();
            render_source_message(&mut source, &message, true, 80);
            assert!(
                source[0]
                    .to_string()
                    .starts_with(if steering { "↳ **" } else { "› **" })
            );
            assert!(
                source
                    .iter()
                    .all(|row| row.style == Style::default() && !row.to_string().contains('┃'))
            );
        }
    }

    #[test]
    fn clipped_prompt_preview_keeps_its_rail_and_steering_provenance() {
        for steering in [false, true] {
            let projection = TranscriptProjection {
                items: vec![TranscriptItem::User(TranscriptMessage {
                    turn: Some(1),
                    steering,
                    parts: vec![TranscriptPart::Text("first\nsecond\nthird\nfourth".into())],
                })],
            };
            let preview = plain_live_rows(&projection, 20, 3);
            assert_eq!(preview.len(), 3);
            assert!(preview[1].starts_with(if steering { "↳ … ┃" } else { "… ┃" }));
            assert!(preview[2].ends_with("fourth"));
            assert!(!preview.iter().any(|row| row.contains('›')));
        }
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
            vec!["┃ fix the parser", "", "I found the issue."]
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
