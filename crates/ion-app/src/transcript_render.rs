//! Pure compact rendering for the typed coding transcript.
use crate::{
    display_text::{Component, Field, MappedLine, Origin, Wrapping, fit_line, wrap_mapped},
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

/// Disposable source-to-row map. Activity ranges refer to the frozen group
/// slice being laid out; they are not action identities or a client journal.
pub(super) struct ActivityRow {
    pub source: std::ops::Range<usize>,
    pub row: usize,
    pub priority: u8,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ContentRoot {
    Block,
    Part(usize),
    Activity(ion_core::TranscriptActivityIdentity),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TextPoint {
    Raw(usize),
    Component { component: Component, byte: usize },
    Node(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ReadingPoint {
    pub root: ContentRoot,
    pub point: TextPoint,
}

#[derive(Default)]
pub(super) struct BlockLayout {
    pub rows: Vec<Line<'static>>,
    pub provenance: Vec<Vec<(ContentRoot, Origin)>>,
    pub activities: Vec<ActivityRow>,
}

impl BlockLayout {
    pub fn literal(text: &str, width: usize) -> Self {
        let mut block = Self::default();
        block.extend(
            ContentRoot::Block,
            wrap_mapped(
                "",
                "",
                MappedLine::raw(Line::from(Span::raw(text.to_owned())), 0..text.len()),
                width,
                Wrapping::Literal,
            ),
        );
        block
    }

    fn extend(&mut self, root: ContentRoot, rows: Vec<MappedLine>) {
        for row in rows {
            assert!(!row.map.is_empty(), "rendered rows have source provenance");
            let mut origins: Vec<(ContentRoot, Origin)> = Vec::new();
            for mapping in row.map {
                match (origins.last_mut(), mapping.origin) {
                    (Some((_, Origin::Raw(previous))), Origin::Raw(next))
                        if previous.end == next.start =>
                    {
                        previous.end = next.end
                    }
                    (
                        Some((_, Origin::Component { component, bytes })),
                        Origin::Component {
                            component: next_component,
                            bytes: next,
                        },
                    ) if *component == next_component && bytes.end == next.start => {
                        bytes.end = next.end
                    }
                    (Some((_, previous)), next) if *previous == next => {}
                    (_, origin) => origins.push((root, origin)),
                }
            }
            self.provenance.push(origins);
            self.rows.push(row.line);
        }
    }

    pub fn reading_point(&self, row: usize) -> ReadingPoint {
        let provenance = &self.provenance[row];
        let (root, origin) = provenance
            .iter()
            .find(|(_, origin)| !matches!(origin, Origin::Node(_)))
            .or_else(|| provenance.first())
            .expect("every measured row has provenance");
        ReadingPoint {
            root: *root,
            point: match origin {
                Origin::Raw(range) => TextPoint::Raw(range.start),
                Origin::Component { component, bytes } => TextPoint::Component {
                    component: *component,
                    byte: bytes.start,
                },
                Origin::Node(node) => TextPoint::Node(*node),
            },
        }
    }

    pub fn locate(&self, point: ReadingPoint) -> Option<usize> {
        // Projection can hide a part or fold multiple activities. Prefer the
        // actual byte's row; otherwise use the closest visible syntax boundary
        // in that root. The retained point is not rewritten by this projection.
        self.provenance
            .iter()
            .enumerate()
            .flat_map(|(row, origins)| {
                origins.iter().filter_map(move |(root, origin)| {
                    if *root != point.root {
                        return None;
                    }
                    let (byte, range) = match (point.point, origin) {
                        (TextPoint::Raw(byte), Origin::Raw(range)) => (byte, range.clone()),
                        (
                            TextPoint::Component { component, byte },
                            Origin::Component {
                                component: actual,
                                bytes,
                            },
                        ) if component == *actual => (byte, bytes.clone()),
                        (TextPoint::Node(byte), Origin::Node(node)) => (byte, *node..*node),
                        _ => return None,
                    };
                    let distance =
                        if range.contains(&byte) || (range.is_empty() && range.start == byte) {
                            0
                        } else {
                            byte.abs_diff(range.start)
                                .min(byte.abs_diff(range.end.saturating_sub(1)))
                                + 1
                        };
                    Some((distance, row))
                })
            })
            .min()
            .map(|(_, row)| row)
            // A disclosure may hide an entire field while its occurrence stays
            // visible. Project to that root's boundary, never another field's
            // byte offset. ConversationView retains the original hidden point.
            .or_else(|| {
                self.provenance
                    .iter()
                    .position(|origins| origins.iter().any(|(root, _)| *root == point.root))
            })
    }

    fn extend_activities(&mut self, activities: &[TranscriptActivity], rows: Vec<MappedLine>) {
        let first = activities
            .first()
            .expect("display activity has source occurrences");
        let start = self.rows.len();
        self.extend(ContentRoot::Activity(first.identity), rows);
        if activities.len() == 1 {
            return;
        }
        for origins in &mut self.provenance[start..] {
            let source = origins.clone();
            for activity in &activities[1..] {
                origins.extend(
                    source.iter().map(|(_, origin)| {
                        (ContentRoot::Activity(activity.identity), origin.clone())
                    }),
                );
            }
        }
    }
}
pub(super) fn layout(
    item: &TranscriptItem,
    width: usize,
    detail: &RenderOptions<'_>,
) -> BlockLayout {
    let mut block = BlockLayout::default();
    if hidden_thinking(item, detail.thinking) {
        return block;
    }
    match item {
        TranscriptItem::Readiness { turn, notices, .. } => {
            block.extend(
                ContentRoot::Block,
                render_readiness(*turn, notices, width, detail.theme),
            );
        }
        TranscriptItem::User(message) => {
            render_message_layout(&mut block, message, true, width, detail)
        }
        TranscriptItem::Assistant(message) => {
            render_message_layout(&mut block, message, false, width, detail)
        }
        TranscriptItem::ActivityGroup(group) => render_group(&mut block, group, width, detail),
        TranscriptItem::UserShell(shell) => {
            block.extend(ContentRoot::Block, render_shell(shell, width, detail))
        }
    }
    assert_eq!(
        block.rows.len(),
        block.provenance.len(),
        "layout and provenance are one measurement"
    );
    while block.rows.last().is_some_and(|row| row.width() == 0) {
        block.rows.pop();
        block.provenance.pop();
    }
    block
}
#[cfg(test)]
pub fn rows(
    items: &[TranscriptItem],
    width: usize,
    detail: &RenderOptions<'_>,
) -> Vec<Line<'static>> {
    let mut rows = Vec::new();
    for item in items {
        let block = layout(item, width.max(1), detail);
        if block.rows.is_empty() {
            continue;
        }
        if rows.last().is_some_and(|row: &Line<'_>| row.width() > 0) {
            rows.push(Line::default());
        }
        rows.extend(block.rows);
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
    turn: u64,
    notices: &[ion_core::CompletionNotice],
    width: usize,
    theme: &Theme,
) -> Vec<MappedLine> {
    wrap_mapped(
        "",
        "",
        MappedLine::component(
            Line::from(Span::raw(readiness_text(turn, notices)))
                .style(theme.style(Role::Secondary)),
            Component::Readiness,
            0,
        ),
        width,
        Wrapping::Literal,
    )
}

fn hidden_thinking(item: &TranscriptItem, thinking: ThinkingVisibility) -> bool {
    thinking == ThinkingVisibility::Hidden
        && matches!(item, TranscriptItem::Assistant(message) if message.parts.iter().all(|part| matches!(part, TranscriptPart::Thinking(_))))
}

/// Attention is a semantic affordance over the same blocks, not a second
/// operation-shaped transcript. Only a clipped source needs it.
pub(super) struct Attention {
    pub source: ion_core::TranscriptSource,
    pub activity: usize,
    pub nested: bool,
    pub row: Line<'static>,
}
pub(super) fn context_summary(
    items: &[TranscriptItem],
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let activities = items
        .iter()
        .filter_map(|item| match item {
            TranscriptItem::ActivityGroup(group) => Some(group.activities.as_slice()),
            _ => None,
        })
        .flatten();
    if activities.clone().next().is_none() {
        return vec![];
    }
    let mut rows = Vec::new();
    crate::display_text::push_styled(
        &mut rows,
        "",
        "",
        Line::styled(group_header(activities), theme.style(Role::Heading)),
        width,
    );
    rows
}
pub(super) fn attention(
    items: &[TranscriptItem],
    width: usize,
    detail: &RenderOptions<'_>,
) -> Option<Attention> {
    items.iter().rev().find_map(|item| {
        let TranscriptItem::ActivityGroup(group) = item else {
            return None;
        };
        group
            .activities
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, parent)| {
                let activity = nested_exception(parent)?;
                let subject = activity
                    .activity
                    .subject
                    .as_deref()
                    .map(clean_inline)
                    .unwrap_or_else(|| activity.name.clone());
                let text = model_result_notice(activity).map_or_else(
                    || format!("! {} · {subject}", action_label(activity)),
                    |notice| format!("! {notice} · {subject}"),
                );
                Some(Attention {
                    source: group.source,
                    activity: index,
                    nested: !std::ptr::eq(activity, parent),
                    row: Line::styled(
                        fit_line(&text, width),
                        activity_style(
                            activity.state,
                            model_result_notice(activity).is_some(),
                            false,
                            detail.theme,
                        ),
                    ),
                })
            })
    })
}

#[cfg(test)]
fn window_rows(
    items: &[TranscriptItem],
    width: usize,
    budget: usize,
    detail: &RenderOptions<'_>,
) -> Vec<Line<'static>> {
    crate::terminal_client::measured_window_fixture(items, width, budget, detail)
}

#[cfg(test)]
fn render_message(
    rows: &mut Vec<Line<'static>>,
    message: &TranscriptMessage,
    user: bool,
    width: usize,
    detail: &RenderOptions<'_>,
) {
    let mut block = BlockLayout::default();
    render_message_layout(&mut block, message, user, width, detail);
    rows.extend(block.rows);
}

fn render_message_layout(
    block: &mut BlockLayout,
    message: &TranscriptMessage,
    user: bool,
    width: usize,
    detail: &RenderOptions<'_>,
) {
    let rail = user && width > 2;
    let content_width = if rail { width - 2 } else { width };
    let start = block.rows.len();
    for (index, part) in message.parts.iter().enumerate() {
        let root = ContentRoot::Part(index);
        let prefix = if user && message.steering {
            if index == 0 { "↳ " } else { "  " }
        } else {
            ""
        };
        match part {
            TranscriptPart::Thinking(text)
                if user || detail.thinking == ThinkingVisibility::Visible =>
            {
                block.extend(
                    root,
                    vec![MappedLine::generated(
                        Line::styled(
                            fit_line("Thinking", content_width),
                            detail.theme.style(Role::Secondary),
                        ),
                        0,
                    )],
                );
                block.extend(
                    root,
                    wrap_mapped(
                        "  ",
                        "  ",
                        MappedLine::raw(
                            Line::from(Span::styled(
                                text.clone(),
                                if user {
                                    Style::default()
                                } else {
                                    detail.theme.style(Role::Secondary)
                                },
                            )),
                            0..text.len(),
                        ),
                        content_width,
                        if user {
                            Wrapping::Literal
                        } else {
                            Wrapping::Words
                        },
                    ),
                );
            }
            TranscriptPart::Thinking(_) => {}
            TranscriptPart::Text(text) if user => block.extend(
                root,
                wrap_mapped(
                    prefix,
                    if prefix.is_empty() { "" } else { "  " },
                    MappedLine::raw(Line::from(Span::raw(text.clone())), 0..text.len()),
                    content_width,
                    Wrapping::Words,
                ),
            ),
            TranscriptPart::Text(text) => block.extend(
                root,
                crate::markdown::render_mapped(text, content_width, detail.theme),
            ),
            TranscriptPart::Image { mime_type } => block.extend(
                root,
                wrap_mapped(
                    prefix,
                    if prefix.is_empty() { "" } else { "  " },
                    MappedLine::generated(
                        Line::from(Span::raw(format!("[image: {mime_type}]"))),
                        0,
                    ),
                    content_width,
                    Wrapping::Literal,
                ),
            ),
        }
    }
    if user {
        for row in &mut block.rows[start..] {
            row.style = detail
                .theme
                .style(Role::Text)
                .add_modifier(ratatui::style::Modifier::BOLD);
            if rail {
                row.spans
                    .insert(0, Span::styled("┃ ", detail.theme.style(Role::UserInput)));
            }
        }
    }
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
    shell: &UserShellActivity,
    width: usize,
    detail: &RenderOptions<'_>,
) -> Vec<MappedLine> {
    let prefix = if shell.exclude_from_context {
        "› !!"
    } else {
        "› !"
    };
    let mut rows = wrap_mapped(
        prefix,
        "  ",
        MappedLine::component(
            Line::from(Span::raw(shell.command.clone())).style(detail.theme.style(Role::UserInput)),
            Component::Field(Field::Command),
            0,
        ),
        width,
        Wrapping::Literal,
    );
    let Some(ion_core::UserShellOutcome::Observed { output, is_error }) = &shell.outcome else {
        if shell.exclude_from_context {
            rows.extend(wrap_mapped(
                "",
                "",
                MappedLine::generated(Line::raw("  not shared with model"), 0),
                width,
                Wrapping::Literal,
            ));
        }
        rows.extend(wrap_mapped(
            "",
            "",
            MappedLine::generated(
                Line::styled(
                    ion_core::UserShellOutcome::unknown_notice(),
                    detail.theme.style(Role::Warning),
                ),
                0,
            ),
            width,
            Wrapping::Literal,
        ));
        return rows;
    };
    crate::tool_output::render_command(&mut rows, output, "  ", width, detail, 4, !is_error);
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
    let style = if *is_error {
        detail.theme.style(Role::Error)
    } else {
        detail.theme.style(Role::Secondary)
    };
    rows.extend(wrap_mapped(
        "",
        "",
        MappedLine::generated(Line::styled(format!("  {state}"), style), 0),
        width,
        Wrapping::Literal,
    ));
    rows
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
    fn priority(&self) -> u8 {
        match self.state {
            ActivityState::Running => 0,
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
    fn style(&self, theme: &Theme) -> Style {
        activity_style(
            self.state,
            self.model_notice.is_some(),
            self.observation,
            theme,
        )
    }
}

fn render_group(
    block: &mut BlockLayout,
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
        block.activities.push(ActivityRow {
            source: item.source.clone(),
            row: block.rows.len(),
            priority: item.priority(),
        });
        let mut rows = Vec::new();
        push_activity(&mut rows, "• ", "  ", item, width, theme);
        if group.activities[0].children.is_empty()
            && group.activities[0].activity.kind != ToolActivityKind::Command
            && let Some(detail) = &item.detail
        {
            push_detail(&mut rows, "  └ ", "    ", detail, width, theme);
        }
        crate::tool_output::render_activity(&mut rows, &group.activities[0], "  ", width, detail);
        crate::edit_diff::render(&mut rows, &group.activities[0], "  ", width, detail);
        block.extend_activities(&group.activities[..1], rows);
        render_children(block, &group.activities[0].children, "  ", width, detail);
        return;
    }

    block.extend(
        ContentRoot::Block,
        vec![MappedLine::generated(
            Line::styled(
                fit_line(&group_header(group.activities.iter()), width),
                theme.style(Role::Heading),
            ),
            0,
        )],
    );
    let total_children = display.len();
    for (index, item) in display.iter().enumerate() {
        block.activities.push(ActivityRow {
            source: item.source.clone(),
            row: block.rows.len(),
            priority: item.priority(),
        });
        let mut rows = Vec::new();
        let last = index + 1 == total_children;
        let branch = if last { "└ " } else { "├ " };
        push_activity(
            &mut rows,
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
                &mut rows,
                prefix,
                if last { "    " } else { "│   " },
                detail,
                width,
                theme,
            );
        }
        if item.source.len() == 1 {
            crate::tool_output::render_activity(
                &mut rows,
                &group.activities[item.source.start],
                if last { "  " } else { "│ " },
                width,
                detail,
            );
        } else {
            crate::tool_output::render_folded_reads(
                &mut rows,
                &group.activities[item.source.clone()],
                if last { "  " } else { "│ " },
                width,
                detail,
            );
        }
        crate::edit_diff::render(
            &mut rows,
            &group.activities[item.source.start],
            if last { "  " } else { "│ " },
            width,
            detail,
        );
        block.extend_activities(&group.activities[item.source.clone()], rows);
        render_children(
            block,
            &group.activities[item.source.start].children,
            if last { "  " } else { "│ " },
            width,
            detail,
        );
    }
}

fn render_children(
    block: &mut BlockLayout,
    children: &[TranscriptActivity],
    prefix: &str,
    width: usize,
    detail: &RenderOptions<'_>,
) {
    let theme = detail.theme;
    let display = display_activities(children, detail.detail);
    for (index, item) in display.iter().enumerate() {
        let mut rows = Vec::new();
        let last = index + 1 == display.len();
        let continuation = format!("{prefix}{}", if last { "  " } else { "│ " });
        push_activity(
            &mut rows,
            &format!("{prefix}{}", if last { "└ " } else { "├ " }),
            &continuation,
            item,
            width,
            theme,
        );
        crate::edit_diff::render(
            &mut rows,
            &children[item.source.start],
            &continuation,
            width,
            detail,
        );
        if item.source.len() == 1 {
            crate::tool_output::render_activity(
                &mut rows,
                &children[item.source.start],
                &continuation,
                width,
                detail,
            );
        } else {
            crate::tool_output::render_folded_reads(
                &mut rows,
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
                &mut rows,
                &format!("{continuation}└ "),
                &format!("{continuation}  "),
                detail,
                width,
                theme,
            );
        }
        block.extend_activities(&children[item.source.clone()], rows);
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
    rows: &mut Vec<MappedLine>,
    prefix: &str,
    continuation: &str,
    item: &DisplayActivity,
    width: usize,
    theme: &Theme,
) {
    let line = Line::from(Span::raw(item.summary.clone())).style(item.style(theme));
    let source = if item.source.len() == 1 {
        MappedLine::component(line, Component::Field(Field::Summary), 0)
    } else {
        // A fold is not the concatenation of its members' summary components.
        MappedLine::generated(line, 0)
    };
    rows.extend(wrap_mapped(
        prefix,
        continuation,
        source,
        width,
        Wrapping::Literal,
    ));
}
fn push_detail(
    rows: &mut Vec<MappedLine>,
    prefix: &str,
    continuation: &str,
    text: &str,
    width: usize,
    theme: &Theme,
) {
    rows.extend(wrap_mapped(
        prefix,
        continuation,
        MappedLine::component(
            Line::from(Span::raw(text.to_owned())).style(theme.style(Role::Secondary)),
            Component::Field(Field::Detail),
            0,
        ),
        width,
        Wrapping::Literal,
    ));
}

#[cfg(test)]
mod tests;
