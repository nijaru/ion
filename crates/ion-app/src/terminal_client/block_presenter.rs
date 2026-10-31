//! Disposable block layouts and source-relative reading over one conversation.
#[cfg(test)]
mod tests;
use std::collections::{HashMap, HashSet};

use ion_core::{TranscriptItem, TranscriptPart, TranscriptSource};
use ratatui::text::Line;

use super::conversation::{Conversation, Scope, Segment};
use crate::{
    presentation_style::Theme,
    tool_output::{OutputDetail, RenderOptions},
    transcript_render::ThinkingVisibility,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Key {
    Fact(TranscriptSource),
    Local(usize),
    Provisional(Option<TranscriptSource>),
}

#[derive(Clone, Copy)]
struct Anchor {
    key: Key,
    position: Option<crate::transcript_render::ReadingPoint>,
}

impl Anchor {
    fn row(&self, layout: &Layout, gap: bool) -> usize {
        // Hidden content projects to the block boundary until disclosed again.
        // Reprojection must not round the retained point to a fitted row start.
        self.position
            .and_then(|position| layout.content.locate(position))
            .map_or(0, |row| row + usize::from(gap))
    }
}

/// Reading retains a source point, not a row in a previous measurement. Layout
/// projection may move that point's row without rewriting the retained point.
#[derive(Default)]
pub(super) struct ConversationView {
    reading: bool,
    anchor: Option<Anchor>,
    last_top: Option<Anchor>,
    movement: isize,
    scope: Option<Scope>,
}

impl ConversationView {
    pub(super) fn scroll(&mut self, delta: isize) {
        if delta < 0 && !self.reading {
            self.reading = true;
            self.anchor = self.last_top;
        }
        if self.reading {
            self.movement = self.movement.saturating_add(delta);
        }
    }
    pub(super) fn follow_end(&mut self) {
        self.reading = false;
        self.anchor = None;
        self.last_top = None;
        self.movement = 0;
    }
    pub(super) fn is_reading(&self) -> bool {
        self.reading
    }
    pub(super) fn commit_response(
        &mut self,
        live: &ion_core::LiveTranscript,
        event: &ion_core::AgentEvent,
    ) {
        let Some(index) = live.provisional_item_index() else {
            return;
        };
        let after = live.projection().items[..index]
            .iter()
            .rev()
            .find_map(TranscriptItem::source);
        for anchor in [&mut self.anchor, &mut self.last_top].into_iter().flatten() {
            if anchor.key != Key::Provisional(after) {
                continue;
            }
            let part = match anchor.position {
                None => 0,
                Some(crate::transcript_render::ReadingPoint {
                    root: crate::transcript_render::ContentRoot::Part(part),
                    ..
                }) => part,
                _ => unreachable!("provisional assistant reading points belong to content parts"),
            };
            if let Some(source) = live.committed_part_source(event, part) {
                anchor.key = Key::Fact(source);
                if let Some(position) = &mut anchor.position {
                    position.root = crate::transcript_render::ContentRoot::Part(0);
                }
            }
        }
    }
    pub(super) fn discard_provisional(&mut self) {
        for anchor in [&mut self.anchor, &mut self.last_top] {
            if let Some(point) = anchor
                && let Key::Provisional(after) = point.key
            {
                *anchor = after.map(|source| Anchor {
                    key: Key::Fact(source),
                    position: None,
                });
            }
        }
    }
}

#[derive(PartialEq, Eq)]
struct Format {
    theme: Theme,
    detail: OutputDetail,
    thinking: ThinkingVisibility,
    details_key: String,
}

struct Layout {
    revision: Option<u64>,
    content: crate::transcript_render::BlockLayout,
}

struct Block<'a> {
    key: Key,
    segment: Segment<'a>,
    gap: bool,
}

#[derive(Clone, Copy)]
struct Point {
    block: usize,
    row: usize,
}

#[derive(Default)]
pub(super) struct BlockPresenter {
    scope: Option<Scope>,
    format: Option<Format>,
    layouts: HashMap<(Key, usize), Layout>,
}

impl BlockPresenter {
    fn configure(
        &mut self,
        conversation: &Conversation,
        width: usize,
        options: &RenderOptions<'_>,
    ) {
        let selection_changed = self.scope.as_ref() != conversation.scope();
        let format = Format {
            theme: options.theme.clone(),
            detail: options.detail,
            thinking: options.thinking,
            details_key: options.details_key.clone(),
        };
        if selection_changed || self.format.as_ref() != Some(&format) {
            self.layouts.clear();
            self.scope = conversation.scope().cloned();
            self.format = Some(format);
        }
        self.layouts
            .retain(|(_, measured_width), _| measured_width.abs_diff(width) <= 1);
    }

    fn blocks<'a>(
        conversation: &'a Conversation,
        pending: bool,
        options: &RenderOptions<'_>,
    ) -> Vec<Block<'a>> {
        let mut previous = None;
        let mut visible_before = pending && conversation.has_published_prefix();
        conversation.segments(pending).into_iter().map(|segment| {
            let (key, visible) = match &segment {
                Segment::Item(item) => {
                    let key = match item.source() {
                        Some(source) => { previous = Some(source); Key::Fact(source) }
                        None => Key::Provisional(previous),
                    };
                    let visible = !matches!(item, TranscriptItem::Assistant(message) if message.parts.iter().all(|part| match part {
                        TranscriptPart::Thinking(_) => options.thinking == ThinkingVisibility::Hidden,
                        TranscriptPart::Text(text) => text.trim().is_empty(),
                        TranscriptPart::Image { .. } => false,
                    }));
                    (key, visible)
                }
                Segment::Notice { ordinal, text } => (Key::Local(*ordinal), !text.is_empty()),
            };
            let gap = visible_before;
            visible_before |= visible;
            Block { key, segment, gap }
        }).collect()
    }

    fn layout(
        &mut self,
        block: &Block<'_>,
        revision: u64,
        width: usize,
        options: &RenderOptions<'_>,
    ) -> &Layout {
        let version = match &block.segment {
            Segment::Item(item) if !item.is_sealed() => Some(revision),
            _ => None,
        };
        if self
            .layouts
            .get(&(block.key, width))
            .is_none_or(|layout| layout.revision != version)
        {
            let content = match &block.segment {
                Segment::Item(item) => crate::transcript_render::layout(item, width, options),
                Segment::Notice { text, .. } => {
                    crate::transcript_render::BlockLayout::literal(text, width)
                }
            };
            self.layouts.insert(
                (block.key, width),
                Layout {
                    revision: version,
                    content,
                },
            );
        }
        &self.layouts[&(block.key, width)]
    }

    fn height(
        &mut self,
        block: &Block<'_>,
        revision: u64,
        width: usize,
        options: &RenderOptions<'_>,
    ) -> usize {
        let rows = self
            .layout(block, revision, width, options)
            .content
            .rows
            .len();
        rows + usize::from(rows > 0 && block.gap)
    }

    pub(super) fn publication(
        &mut self,
        conversation: &Conversation,
        width: usize,
        options: &RenderOptions<'_>,
    ) -> Vec<Line<'static>> {
        self.configure(conversation, width, options);
        let mut rows = Vec::new();
        for block in Self::blocks(conversation, true, options) {
            let body = &self
                .layout(&block, conversation.revision(), width, options)
                .content
                .rows;
            if body.is_empty() {
                continue;
            }
            if block.gap {
                rows.push(Line::default());
            }
            rows.extend(body.iter().cloned());
        }
        rows
    }

    fn shift(
        &mut self,
        blocks: &[Block<'_>],
        mut point: Point,
        mut delta: isize,
        revision: u64,
        width: usize,
        options: &RenderOptions<'_>,
    ) -> Point {
        while delta < 0 {
            if point.row == 0 {
                if point.block == 0 {
                    break;
                }
                point.block -= 1;
                point.row = self.height(&blocks[point.block], revision, width, options);
            }
            let take = point.row.min(delta.unsigned_abs());
            point.row -= take;
            delta += take as isize;
        }
        while delta > 0 && point.block < blocks.len() {
            let remaining = self.height(&blocks[point.block], revision, width, options) - point.row;
            let take = remaining.min(delta as usize);
            point.row += take;
            delta -= take as isize;
            if take == remaining {
                point.block += 1;
                point.row = 0;
            }
        }
        point
    }

    fn focused(
        &mut self,
        blocks: &[Block<'_>],
        items: &[TranscriptItem],
        budget: usize,
        width: usize,
        revision: u64,
        options: &RenderOptions<'_>,
    ) -> Vec<Line<'static>> {
        if budget == 0 {
            return vec![];
        }
        let mut rows = crate::transcript_render::context_summary(items, width, options.theme);
        if rows.is_empty() {
            rows.push(Line::styled(
                crate::display_text::fit_line(
                    &format!("… earlier conversation · {}", options.details_key),
                    width,
                ),
                options
                    .theme
                    .style(crate::presentation_style::Role::Secondary),
            ));
        }
        rows.truncate(budget.saturating_sub(2).clamp(1, 3));
        let attention = crate::transcript_render::attention(items, width, options);
        if let Some(attention) = &attention
            && rows.len() < budget
        {
            rows.push(attention.row.clone());
        }
        let remaining = budget.saturating_sub(rows.len());
        let focus = blocks.iter().rfind(|block| matches!(&block.segment, Segment::Item(TranscriptItem::ActivityGroup(group)) if group.activities.iter().any(|activity| activity.state == ion_core::ActivityState::Running)))
            .or_else(|| blocks.iter().rfind(|block| !self.layout(block, revision, width, options).content.rows.is_empty()));
        let Some(block) = focus else { return rows };
        let layout = self.layout(block, revision, width, options);
        if let Segment::Item(TranscriptItem::ActivityGroup(group)) = block.segment {
            let mut selected: Vec<_> = layout
                .content
                .activities
                .iter()
                .filter(|item| {
                    attention.as_ref().is_none_or(|pin| {
                        pin.source != group.source
                            || pin.nested
                            || !item.source.contains(&pin.activity)
                    })
                })
                .collect();
            if remaining == 0 || selected.is_empty() {
                return rows;
            }
            let capacity = remaining - 1;
            let omitted_row = usize::from(selected.len() > capacity && capacity > 1);
            selected.sort_by_key(|item| (item.priority, std::cmp::Reverse(item.row)));
            selected.truncate(capacity.saturating_sub(omitted_row));
            selected.sort_by_key(|item| item.row);
            rows.push(Line::styled(
                crate::display_text::fit_line("• Current activity", width),
                options
                    .theme
                    .style(crate::presentation_style::Role::Heading),
            ));
            for (position, item) in selected.iter().enumerate() {
                let last = position + 1 == selected.len() && omitted_row == 0;
                let mut row = layout.content.rows[item.row].clone();
                let branch = if last { "└ " } else { "├ " };
                if let Some(first) = row.spans.first_mut() {
                    let rest = ["├ ", "└ ", "• "]
                        .iter()
                        .find_map(|prefix| first.content.strip_prefix(prefix));
                    if let Some(rest) = rest {
                        first.content = format!("{branch}{rest}").into();
                    } else {
                        row.spans.insert(0, ratatui::text::Span::raw(branch));
                    }
                }
                rows.push(row);
            }
            if omitted_row > 0 {
                let shown: usize = selected.iter().map(|item| item.source.len()).sum();
                let pinned = usize::from(
                    attention
                        .as_ref()
                        .is_some_and(|pin| pin.source == group.source && !pin.nested),
                );
                rows.push(Line::styled(
                    crate::display_text::fit_line(
                        &format!(
                            "└ {} more · {}",
                            group.activities.len() - shown - pinned,
                            options.details_key
                        ),
                        width,
                    ),
                    options
                        .theme
                        .style(crate::presentation_style::Role::Secondary),
                ));
            }
        } else if remaining > 0 {
            let omitted = layout.content.rows.len().saturating_sub(remaining);
            let mut body = layout.content.rows[omitted..].to_vec();
            if omitted > 0
                && let Some(first) = body.first_mut()
            {
                let prefix = match block.segment {
                    Segment::Item(TranscriptItem::User(message)) if message.steering => "↳ … ",
                    Segment::Item(TranscriptItem::Assistant(message))
                        if message
                            .parts
                            .iter()
                            .all(|part| matches!(part, TranscriptPart::Thinking(_))) =>
                    {
                        "Thinking · … "
                    }
                    _ => "… ",
                };
                first.spans.insert(0, ratatui::text::Span::raw(prefix));
                if first.width() > width {
                    let mut wrapped = vec![];
                    crate::display_text::push_styled(
                        &mut wrapped,
                        "",
                        "",
                        std::mem::take(first),
                        width.saturating_sub(1).max(1),
                    );
                    *first = wrapped
                        .into_iter()
                        .next()
                        .expect("a retained row has a display line");
                    if width > 1 {
                        first.spans.push(ratatui::text::Span::raw("…"));
                    }
                }
            }
            rows.extend(body);
        }
        rows
    }

    pub(super) fn window(
        &mut self,
        conversation: &Conversation,
        view: &mut ConversationView,
        (width, budget): (usize, usize),
        inline: bool,
        active: bool,
        options: &RenderOptions<'_>,
    ) -> Vec<Line<'static>> {
        self.configure(conversation, width, options);
        if view.scope.as_ref() != conversation.scope() {
            view.follow_end();
            view.scope = conversation.scope().cloned();
        }
        let mut blocks = Self::blocks(conversation, false, options);
        let keys: HashSet<_> = blocks.iter().map(|block| block.key).collect();
        self.layouts.retain(|(key, _), _| keys.contains(key));
        if inline && !view.reading {
            let mutable: HashSet<_> = conversation
                .mutable_items()
                .iter()
                .map(TranscriptItem::source)
                .collect();
            blocks.retain(|block| matches!(&block.segment, Segment::Item(item) if mutable.contains(&item.source())));
        }
        let revision = conversation.revision();
        let tail = Point {
            block: blocks.len(),
            row: 0,
        };
        let back = isize::try_from(budget).expect("terminal row budget fits isize");
        let mut start = if view.reading {
            view.anchor
                .as_ref()
                .and_then(|anchor| {
                    blocks
                        .iter()
                        .position(|block| block.key == anchor.key)
                        .map(|block| {
                            let layout = self.layout(&blocks[block], revision, width, options);
                            let row = anchor.row(layout, blocks[block].gap);
                            Point { block, row }
                        })
                })
                .unwrap_or_else(|| self.shift(&blocks, tail, -back, revision, width, options))
        } else {
            self.shift(&blocks, tail, -back, revision, width, options)
        };
        let movement = std::mem::take(&mut view.movement);
        start = self.shift(&blocks, start, movement, revision, width, options);
        if view.reading && movement > 0 && start.block == blocks.len() {
            view.follow_end();
            return self.window(conversation, view, (width, budget), inline, active, options);
        }
        let mut rows = Vec::new();
        let mut first = None;
        let mut point = start;
        while point.block < blocks.len() && rows.len() < budget {
            let block = &blocks[point.block];
            let layout = self.layout(block, revision, width, options);
            let body = &layout.content.rows;
            let gap = usize::from(!body.is_empty() && block.gap);
            if point.row >= body.len() + gap {
                point.block += 1;
                point.row = 0;
                continue;
            }
            first.get_or_insert(Anchor {
                key: block.key,
                position: if gap > 0 && point.row == 0 {
                    None
                } else {
                    Some(layout.content.reading_point(point.row - gap))
                },
            });
            if gap > 0 && point.row == 0 {
                rows.push(Line::default());
            } else {
                rows.push(body[point.row - gap].clone());
            }
            point.row += 1;
        }
        view.last_top = first;
        if view.reading && (movement != 0 || view.anchor.is_none()) {
            view.anchor = first;
        }
        if active && !view.reading && (start.block > 0 || start.row > 0) {
            let items = conversation.mutable_items();
            let focused = self.focused(&blocks, items, budget, width, revision, options);
            return focused;
        }
        rows
    }
}
