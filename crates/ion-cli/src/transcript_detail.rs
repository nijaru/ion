//! Paged inspection of canonical saved facts and the current live transcript.
mod document;

use crate::keybindings::KeyAction;
use crate::transcript_render::{ContentRoot, kind_label, state_label};
use document::{Anchor, Document, Field};
#[cfg(test)]
use ion_core::TranscriptProjection;
use ion_core::{
    AgentEvent, LiveTranscript, TranscriptActivity, TranscriptActivityIdentity, TranscriptItem,
    TranscriptSource,
};
use std::num::NonZeroUsize;

const PAGE_ENTRIES: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EntryKey {
    Fact(TranscriptSource),
    Tool(TranscriptActivityIdentity),
    // There is one removable response overlay. Its identity is retired at
    // restart/gap boundaries, before a new response can occupy that overlay.
    Provisional,
    Heading,
    Notice(usize),
}

#[derive(Clone, Copy)]
struct Boundary {
    entry: EntryKey,
    previous: Option<EntryKey>,
}

#[derive(Default)]
pub struct DetailView {
    tool: Option<(NonZeroUsize, TranscriptActivityIdentity)>,
    through: Option<Boundary>,
    anchor: Option<Anchor>,
    viewport: usize,
    page: Option<Page>,
}

struct Page {
    revision: u64,
    width: usize,
    end: usize,
    total: usize,
    before: Option<Boundary>,
    later: Option<Boundary>,
    last: Option<Boundary>,
    document: Document,
    evidence: Option<String>,
}

enum Entry<'a> {
    Item(&'a TranscriptItem),
    Tool(&'a TranscriptActivity),
    Thinking(&'a ion_core::GroupThinking),
}

impl Entry<'_> {
    fn key(&self) -> EntryKey {
        match self {
            Self::Item(item) => item.source().map_or(EntryKey::Provisional, EntryKey::Fact),
            Self::Tool(activity) => EntryKey::Tool(activity.identity),
            Self::Thinking(note) => EntryKey::Fact(note.source),
        }
    }

    fn contains(&self, key: EntryKey) -> bool {
        fn contains(activity: &TranscriptActivity, identity: TranscriptActivityIdentity) -> bool {
            activity.identity == identity
                || activity
                    .children
                    .iter()
                    .any(|child| contains(child, identity))
        }
        self.key() == key
            || matches!((self, key), (Self::Tool(activity), EntryKey::Tool(identity)) if contains(activity, identity))
    }
}

impl DetailView {
    pub fn new(tool: Option<(NonZeroUsize, TranscriptActivityIdentity)>) -> Self {
        Self {
            tool,
            ..Self::default()
        }
    }

    /// Canonical refresh invalidates measurement, not the reader's source point.
    pub fn invalidate(&mut self) {
        self.page = None;
    }

    pub(crate) fn commit_response(&mut self, live: &LiveTranscript, event: &AgentEvent) {
        if let Some(boundary) = &mut self.through
            && boundary.entry == EntryKey::Provisional
        {
            let source = live.provisional_item_index().and_then(|index| {
                let TranscriptItem::Assistant(message) = &live.projection().items[index] else {
                    unreachable!("the provisional response is an assistant message")
                };
                live.committed_part_source(event, message.parts.len().checked_sub(1)?)
            });
            if let Some(source) = source {
                boundary.entry = EntryKey::Fact(source);
            } else {
                self.through = boundary.previous.map(|entry| Boundary {
                    entry,
                    previous: None,
                });
            }
        }
        if let Some(anchor) = &mut self.anchor
            && anchor.entry == EntryKey::Provisional
        {
            let part = match anchor.point.map(|point| point.root) {
                Some(ContentRoot::Part(part)) => part,
                _ => 0,
            };
            if let Some(source) = live.committed_part_source(event, part) {
                anchor.entry = EntryKey::Fact(source);
                if let Some(point) = &mut anchor.point
                    && matches!(point.root, ContentRoot::Part(_))
                {
                    point.root = ContentRoot::Part(0);
                }
            } else {
                self.discard_provisional();
            }
        }
    }

    pub(crate) fn discard_provisional(&mut self) {
        if let Some(boundary) = self.through
            && boundary.entry == EntryKey::Provisional
        {
            self.through = boundary.previous.map(|entry| Boundary {
                entry,
                previous: None,
            });
        }
        if let Some(anchor) = &mut self.anchor
            && anchor.entry == EntryKey::Provisional
        {
            anchor.entry = anchor.previous.unwrap_or(EntryKey::Heading);
            anchor.field = Field::Heading;
            anchor.point = None;
            anchor.previous = None;
        }
    }

    pub(crate) fn navigate(&mut self, action: KeyAction) {
        let Some(page) = &self.page else { return };
        match action {
            KeyAction::DetailUp | KeyAction::PageUp => {
                self.scroll_by(if action == KeyAction::DetailUp {
                    -1
                } else {
                    -12
                });
            }
            KeyAction::DetailDown | KeyAction::PageDown => {
                self.scroll_by(if action == KeyAction::DetailDown {
                    1
                } else {
                    12
                });
            }
            KeyAction::Earlier if self.tool.is_none() && page.before.is_some() => {
                self.through = page.before;
                self.anchor = None;
                self.invalidate();
            }
            KeyAction::Later if self.tool.is_none() => {
                self.through = page.later;
                self.anchor = None;
                self.invalidate();
            }
            _ => {}
        }
    }

    fn top(&self, page: &Page, height: usize) -> usize {
        let limit = page.document.len.saturating_sub(height.max(1));
        self.anchor
            .as_ref()
            .map_or(limit, |anchor| page.document.locate(anchor).min(limit))
    }

    /// Positive movement reads newer rows; only deliberate navigation replaces
    /// the source point. Measurement/clamping never accumulates reflow drift.
    pub(crate) fn scroll_by(&mut self, movement: isize) {
        let Some(page) = &self.page else { return };
        let top = self.top(page, self.viewport);
        let limit = page.document.len.saturating_sub(self.viewport.max(1));
        let next = top.saturating_add_signed(movement).min(limit);
        if movement > 0 && next == limit && (self.tool.is_some() || page.end == page.total) {
            self.anchor = None;
            self.through = None;
        } else if next != top {
            self.anchor = Some(page.document.anchor(next));
            if movement < 0 && self.tool.is_none() {
                self.through = page.last;
            }
        }
    }

    /// Bounded extraction and navigation share the measured source layout.
    pub(crate) fn window(&mut self, height: usize) -> Vec<String> {
        self.viewport = height;
        let page = self.page.as_ref().expect("prepare precedes display");
        page.document.window(self.top(page, height), height)
    }

    pub fn prepare<'a>(
        &mut self,
        items: impl Iterator<Item = &'a TranscriptItem>,
        notices: &[String],
        revision: u64,
        width: usize,
    ) {
        let width = width.max(1);
        if self
            .page
            .as_ref()
            .is_some_and(|page| page.revision == revision && page.width == width)
        {
            return;
        }
        // References only: Conversation remains the sole semantic transcript.
        let entries = entries(items).collect::<Vec<_>>();
        let total = entries.len();
        if (self
            .anchor
            .as_ref()
            .is_some_and(|anchor| anchor.entry == EntryKey::Provisional)
            || self
                .through
                .is_some_and(|boundary| boundary.entry == EntryKey::Provisional))
            && !entries
                .iter()
                .any(|entry| entry.key() == EntryKey::Provisional)
        {
            self.discard_provisional();
        }
        let mut end = self
            .through
            .and_then(|boundary| {
                entries
                    .iter()
                    .position(|entry| entry.key() == boundary.entry)
                    .map(|index| index + 1)
            })
            .unwrap_or(total);
        let mut start = end.saturating_sub(PAGE_ENTRIES);
        if let Some(anchor) = &self.anchor
            && let Some(index) = entries
                .iter()
                .position(|entry| entry.contains(anchor.entry))
            && !(start..end).contains(&index)
        {
            // Appending to an earlier open group can move the selected entry
            // out of a numeric page. Keep its identity inside a bounded page.
            end = (index + PAGE_ENTRIES).min(total);
            start = end.saturating_sub(PAGE_ENTRIES);
        }
        let boundary = |index: usize| Boundary {
            entry: entries[index].key(),
            previous: index.checked_sub(1).map(|index| entries[index].key()),
        };
        let before = start.checked_sub(1).map(boundary);
        let later_end = end.saturating_add(PAGE_ENTRIES).min(total);
        let later = (later_end < total).then(|| boundary(later_end - 1));
        let last = end.checked_sub(1).map(boundary);
        let mut document = Document::default();
        let mut evidence = None;
        if let Some((number, identity)) = &mut self.tool {
            let selected = entries
                .iter()
                .filter_map(|entry| match entry {
                    Entry::Tool(activity) => Some(*activity),
                    Entry::Item(..) | Entry::Thinking(..) => None,
                })
                .enumerate()
                .find(|(_, activity)| activity.identity == *identity);
            if let Some((index, activity)) = selected {
                *number = NonZeroUsize::new(index + 1).expect("one-based tool number");
                let number = number.to_string();
                document.activity(activity, &number, width);
                evidence = Some(format_activity(&number, activity));
            } else {
                document.generated(
                    EntryKey::Tool(*identity),
                    Field::Heading,
                    "Tool result not found",
                    width,
                );
            }
        } else {
            document.generated(
                entries.get(start).map_or(EntryKey::Heading, Entry::key),
                Field::PageHeading,
                &format!(
                    "Conversation · entries {}–{end} of {total}",
                    if total == 0 { 0 } else { start + 1 }
                ),
                width,
            );
            let mut number = 0;
            for (index, entry) in entries.iter().enumerate().take(end) {
                if matches!(entry, Entry::Tool(_)) {
                    number += 1;
                }
                if index >= start {
                    document.entry(
                        entry.key(),
                        index.checked_sub(1).map(|previous| entries[previous].key()),
                        entry,
                        number,
                        width,
                    );
                }
            }
            if end == total {
                for (index, notice) in notices.iter().enumerate() {
                    document.generated(
                        EntryKey::Notice(index),
                        Field::Heading,
                        &format!("Notice: {notice}"),
                        width,
                    );
                }
            }
        }
        self.page = Some(Page {
            revision,
            width,
            end,
            total,
            before,
            later,
            last,
            document,
            evidence,
        });
    }

    #[cfg(test)]
    pub(crate) fn rows(&self) -> Vec<String> {
        let document = &self
            .page
            .as_ref()
            .expect("prepare precedes display")
            .document;
        document.window(0, document.len)
    }

    pub fn copy_evidence(&self) -> Option<(NonZeroUsize, &str)> {
        let number = self.tool?.0;
        Some((number, self.page.as_ref()?.evidence.as_deref()?))
    }

    pub(crate) fn controls(&self, keys: &crate::keybindings::KeyBindings) -> String {
        let title = self.tool.map_or_else(
            || {
                format!(
                    "Conversation · {} older / {} newer",
                    keys.hint(KeyAction::Earlier),
                    keys.hint(KeyAction::Later)
                )
            },
            |(number, _)| {
                format!(
                    "Tool {number} · {} copies evidence",
                    keys.hint(KeyAction::Copy)
                )
            },
        );
        format!(
            "{title} · {} / {} scroll · {} or {} closes",
            keys.hint(KeyAction::DetailUp),
            keys.hint(KeyAction::DetailDown),
            keys.hint(KeyAction::Cancel),
            keys.hint(KeyAction::ExpandTools)
        )
    }
}

fn entries<'a>(items: impl Iterator<Item = &'a TranscriptItem>) -> impl Iterator<Item = Entry<'a>> {
    items.flat_map(|item| {
        let mut activity = 0;
        let mut thinking = 0;
        let mut single = Some(item);
        std::iter::from_fn(move || {
            let TranscriptItem::ActivityGroup(group) = item else {
                return single.take().map(Entry::Item);
            };
            if let Some(note) = group.thinking.get(thinking)
                && note.before == activity
            {
                thinking += 1;
                return Some(Entry::Thinking(note));
            }
            let next = group.activities.get(activity)?;
            activity += 1;
            Some(Entry::Tool(next))
        })
    })
}

pub fn tools<'a>(
    items: impl IntoIterator<Item = &'a TranscriptItem>,
) -> impl Iterator<Item = &'a TranscriptActivity> {
    entries(items.into_iter()).filter_map(|entry| match entry {
        Entry::Tool(activity) => Some(activity),
        Entry::Item(..) | Entry::Thinking(..) => None,
    })
}

fn format_activity(number: &str, activity: &TranscriptActivity) -> String {
    let mut text = String::new();
    append_activity(&mut text, number, activity);
    text
}

fn append_activity(text: &mut String, number: &str, activity: &TranscriptActivity) {
    push_field(
        text,
        &format!(
            "Tool {number}: {} · {} · {}",
            activity.name,
            kind_label(activity.activity.kind),
            state_label(activity.state)
        ),
    );
    push_field(text, &format!("Call ID: {}", activity.call_id));
    if let Some(subject) = &activity.activity.subject {
        push_field(text, &format!("Subject: {subject}"));
    }
    push_field(text, "Arguments");
    push_field(text, &json_text(&activity.arguments));
    push_field(text, "Result");
    if let Some(result) = &activity.result {
        if let Some(notice) = result
            .projection
            .and_then(ion_core::ToolResultProjection::notice)
        {
            push_field(text, notice);
        }
        if let Some(diff) = crate::edit_diff::recorded(activity) {
            for (key, value) in diff.fields {
                if key != "diff" {
                    push_field(text, &format!("{key}: {value}"));
                }
            }
            push_field(text, &diff.heading());
            if diff.text.is_empty() {
                push_field(text, "No text changes");
            } else {
                push_field(text, diff.text);
            }
        } else {
            push_field(text, &json_text(&result.value));
        }
        for metadata in crate::tool_output::artifact_metadata(&result.artifacts) {
            push_field(text, &metadata);
        }
        for mime in &result.image_mime_types {
            push_field(text, &format!("[image: {mime}]"));
        }
    } else {
        push_field(text, "[no committed result]");
    }
    for (child, activity) in activity.children.iter().enumerate() {
        push_field(text, "Child activity · not included in model context");
        append_activity(text, &format!("{number}.{}", child + 1), activity);
    }
}

fn json_text(value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(value).expect("JSON values are serializable")
}

fn push_field(text: &mut String, field: &str) {
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(field);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ion_ai::{Content, Message, ResponseTermination, ToolCall};
    use ion_core::{AgentEvent, ToolOutput};
    use ion_core::{
        StoredToolActivity, ToolActivity, ToolActivityKind, TranscriptMessage, TranscriptPart,
        UserShellActivity,
    };

    fn activity(ordinal: usize) -> TranscriptActivity {
        TranscriptActivity {
            identity: TranscriptActivityIdentity::Tool(ion_core::ToolOccurrence {
                assistant_entry: 2,
                ordinal,
            }),
            call_id: format!("call-{ordinal}"),
            name: "exec".into(),
            activity: ToolActivity {
                kind: ToolActivityKind::Command,
                subject: Some("check".into()),
            },
            arguments: serde_json::json!({}),
            state: ion_core::ActivityState::Completed,
            result: Some(ion_core::ActivityResult {
                projection: Some(ion_core::ToolResultProjection::Observed),
                value: serde_json::json!({"stdout": "done"}),
                image_mime_types: vec![],
                artifacts: vec![],
                is_error: false,
            }),
            children: vec![],
        }
    }

    fn tool_history(activities: Vec<TranscriptActivity>) -> TranscriptProjection {
        TranscriptProjection {
            items: vec![TranscriptItem::ActivityGroup(ion_core::ActivityGroup {
                thinking: Vec::new(),
                source: TranscriptSource { entry: 2, part: 0 },
                turn: 1,
                activities,
                open: true,
            })],
        }
    }

    #[test]
    fn inspection_keeps_group_thinking_in_source_order_without_renumbering_tools() {
        let mut history = tool_history(vec![activity(0), activity(1)]);
        let TranscriptItem::ActivityGroup(group) = &mut history.items[0] else {
            unreachable!()
        };
        group.thinking = vec![
            ion_core::GroupThinking {
                source: TranscriptSource { entry: 4, part: 0 },
                before: 1,
                text: "**BETWEEN**\r\nkept literally".into(),
            },
            ion_core::GroupThinking {
                source: TranscriptSource { entry: 6, part: 0 },
                before: 2,
                text: "TRAILING".into(),
            },
        ];
        let mut view = DetailView::default();
        view.prepare(history.items.iter(), &[], 0, 80);
        let text = view.rows().join("\n");
        let positions = [
            "Tool 1:",
            "**BETWEEN**",
            "kept literally",
            "Tool 2:",
            "TRAILING",
        ]
        .map(|needle| text.find(needle).unwrap());
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "{text}");
        assert_eq!(tools(history.items.iter()).count(), 2);
        let note = entries(history.items.iter()).nth(1).unwrap();
        assert_eq!(
            note.key(),
            EntryKey::Fact(TranscriptSource { entry: 4, part: 0 })
        );
        read_marker(&mut view, "**BETWEEN**");
        let retained = view.anchor.clone();
        view.prepare(history.items.iter(), &[], 0, 23);
        assert!(view.window(8)[0].contains("**BETWEEN**"));
        assert_eq!(view.anchor, retained);
    }

    fn read_marker(view: &mut DetailView, marker: &str) {
        for _ in 0..10_000 {
            if view.window(8)[0].contains(marker) {
                return;
            }
            view.scroll_by(-1);
        }
        panic!("marker not reachable: {marker}");
    }

    #[test]
    fn nested_json_child_fields_survive_sibling_growth_and_tool_renumbering() {
        let mut parent = activity(0);
        let mut child = activity(1);
        child.identity = TranscriptActivityIdentity::Child {
            parent: ion_core::ToolOccurrence {
                assistant_entry: 2,
                ordinal: 0,
            },
            child: 0,
        };
        let payload = format!(
            "{}TARGET_MARKER{}",
            "😀\t\n\"\\\u{1b}".repeat(80),
            "suffix".repeat(100)
        );
        child.arguments = serde_json::json!({"nested": [{"0": {"payload": payload}}]});
        parent.children.push(child);
        let identity = parent.identity;
        let mut history = tool_history(vec![parent]);
        let mut view = DetailView::new(Some((NonZeroUsize::new(1).unwrap(), identity)));
        view.prepare(history.items.iter(), &[], 0, 50);
        read_marker(&mut view, "TARGET_MARKER");
        let visible = view.window(8)[0].clone();
        let point = view.anchor.clone();
        let TranscriptItem::ActivityGroup(group) = &mut history.items[0] else {
            panic!()
        };
        let parent = &mut group.activities[0];
        parent.result.as_mut().unwrap().value =
            serde_json::json!({"stdout": "preceding output".repeat(100)});
        parent.children[0].arguments["nested"][0]["0"]["a"] =
            serde_json::json!("new sibling".repeat(100));
        let mut later = activity(3);
        later.identity = TranscriptActivityIdentity::Child {
            parent: ion_core::ToolOccurrence {
                assistant_entry: 2,
                ordinal: 0,
            },
            child: 1,
        };
        parent.children.push(later);
        group.activities.insert(0, activity(20));
        view.invalidate();
        view.prepare(history.items.iter(), &[], 1, 50);
        assert_eq!(view.window(8)[0], visible);
        assert_eq!(view.copy_evidence().unwrap().0.get(), 2);
        assert!(view.copy_evidence().unwrap().1.contains("TARGET_MARKER"));
        for width in [3, 80, 1, 17, 50, 120, 50] {
            view.prepare(history.items.iter(), &[], 1, width);
            assert!(
                view.window(12)
                    .iter()
                    .all(|row| unicode_width::UnicodeWidthStr::width(row.as_str()) <= width)
            );
            assert_eq!(
                view.anchor, point,
                "remeasurement rewrote the raw scalar point"
            );
        }
        assert_eq!(view.window(8)[0], visible);
        // A removed selection must not silently adopt the new tool at its ordinal.
        let TranscriptItem::ActivityGroup(group) = &mut history.items[0] else {
            panic!()
        };
        group.activities.remove(1);
        view.prepare(history.items.iter(), &[], 2, 50);
        assert!(view.rows().join("").contains("Tool result not found"));
        assert!(view.copy_evidence().is_none());
    }

    #[test]
    fn single_row_scroll_crosses_wrapped_generated_fields_and_json_escapes() {
        let mut tool = activity(0);
        tool.arguments = serde_json::json!({"a long key": "\u{1f}\n\t\\\""});
        let identity = tool.identity;
        let history = tool_history(vec![tool]);
        for width in [1, 2, 3, 6] {
            let mut view = DetailView::new(Some((NonZeroUsize::new(1).unwrap(), identity)));
            view.prepare(history.items.iter(), &[], 0, width);
            view.window(2);
            view.scroll_by(isize::MIN);
            let rows = view.rows();
            for first in 0..rows.len() - 1 {
                assert_eq!(
                    view.window(2),
                    rows[first..first + 2],
                    "unreachable row {first} at width {width}"
                );
                view.scroll_by(1);
            }
        }
    }

    #[test]
    fn reading_page_heading_pins_the_semantic_history_boundary() {
        let mut history = TranscriptProjection {
            items: (1..=40)
                .map(|entry| {
                    TranscriptItem::Assistant(TranscriptMessage {
                        source: Some(TranscriptSource { entry, part: 0 }),
                        turn: Some(entry),
                        steering: false,
                        parts: vec![TranscriptPart::Text(format!("ANSWER_{entry}"))],
                    })
                })
                .collect(),
        };
        let mut view = DetailView::default();
        view.prepare(history.items.iter(), &[], 0, 80);
        view.window(8);
        view.scroll_by(isize::MIN);
        history
            .items
            .push(TranscriptItem::Assistant(TranscriptMessage {
                source: Some(TranscriptSource { entry: 41, part: 0 }),
                turn: Some(41),
                steering: false,
                parts: vec![TranscriptPart::Text("new arrival".into())],
            }));
        view.prepare(history.items.iter(), &[], 1, 80);
        assert!(view.window(8)[0].contains("entries 9–40 of 41"));
        assert!(!view.rows().join("\n").contains("new arrival"));
    }

    #[test]
    fn literal_tab_expansion_rows_have_distinct_source_positions() {
        let layout = crate::transcript_render::BlockLayout::literal("\tX", 1);
        for row in 0..layout.rows.len() {
            let point = layout.reading_point(row);
            assert_eq!(layout.locate(point), Some(row));
            for width in [2, 8, 1] {
                let reflow = crate::transcript_render::BlockLayout::literal("\tX", width);
                assert!(reflow.locate(point).is_some());
            }
            assert_eq!(
                crate::transcript_render::BlockLayout::literal("\tX", 1).locate(point),
                Some(row)
            );
        }
        assert_eq!(
            layout.reading_point(1).point,
            crate::transcript_render::TextPoint::Expansion {
                source: crate::display_text::SourceKind::Raw,
                byte: 0,
                offset: 1,
            }
        );
    }

    #[test]
    fn multiline_results_are_readable_and_keep_exact_evidence_and_source_positions() {
        let mut tool = activity(0);
        let identity = tool.identity;
        let stderr = "Traceback (most recent call last):\n  File \"test_calculator.py\", line 7\n    path = r\"C:\\temp\\n\"\n\u{1b}]52;c;Zm9v\u{7}\nAssertionError: 😀 != 5\n";
        let value =
            serde_json::json!({"exit_code":1,"stderr":stderr,"stdout":"","cancelled":false});
        tool.state = ion_core::ActivityState::Failed;
        let result = tool.result.as_mut().unwrap();
        result.is_error = true;
        result.value = value.clone();
        let mut history = tool_history(vec![tool]);
        let mut view = DetailView::new(Some((NonZeroUsize::new(1).unwrap(), identity)));
        view.prepare(history.items.iter(), &[], 0, 80);
        let rows = view.rows();
        for expected in [
            "Traceback (most recent call last):",
            "File \"test_calculator.py\", line 7",
            r#"path = r"C:\temp\n""#,
            "AssertionError: 😀 != 5",
        ] {
            assert!(
                rows.iter().any(|row| row.trim() == expected),
                "missing readable result line {expected:?}: {rows:?}"
            );
        }
        assert!(rows.join("\n").contains("\"exit_code\": 1"));
        assert!(rows.join("\n").contains("\"cancelled\": false"));
        assert!(rows.iter().all(|row| !row.contains(['\u{1b}', '\u{7}'])));
        assert!(
            rows.iter()
                .position(|row| row.contains("\"stdout\""))
                .unwrap()
                < rows
                    .iter()
                    .position(|row| row.contains("Traceback"))
                    .unwrap()
        );
        assert!(view.copy_evidence().unwrap().1.contains(&json_text(&value)));
        let marker = rows
            .iter()
            .position(|row| row.contains("AssertionError"))
            .unwrap();
        view.window(2);
        view.scroll_by(isize::MIN);
        view.scroll_by(isize::try_from(marker).unwrap());
        let visible = view.window(2)[0].clone();
        assert!(visible.contains("AssertionError"));
        let anchor = view.anchor.clone();
        assert_eq!(
            anchor.as_ref().unwrap().point.unwrap().point,
            crate::transcript_render::TextPoint::Raw(stderr.find("AssertionError").unwrap())
        );
        let TranscriptItem::ActivityGroup(group) = &mut history.items[0] else {
            panic!()
        };
        group.activities[0].result.as_mut().unwrap().value["earlier"] =
            serde_json::json!("preceding metadata\n".repeat(100));
        view.invalidate();
        for width in [12, 1, 120, 80] {
            view.prepare(history.items.iter(), &[], 1, width);
            assert_eq!(view.anchor, anchor);
            assert!(
                view.window(2)
                    .iter()
                    .all(|row| { unicode_width::UnicodeWidthStr::width(row.as_str()) <= width })
            );
        }
        assert_eq!(view.window(2)[0], visible);
    }

    #[test]
    fn inspection_json_matches_pretty_values_without_emitting_controls() {
        let controls: String = ('\0'..='\u{1f}').collect();
        let mut tool = activity(0);
        tool.arguments = serde_json::json!({
            "quote\"key": [null, true, false, 1.25, {}, [], {"0": format!("{controls}😀\"\\/")}],
        });
        let expected = json_text(&tool.arguments);
        let identity = tool.identity;
        let history = tool_history(vec![tool]);
        let mut view = DetailView::new(Some((NonZeroUsize::new(1).unwrap(), identity)));
        view.prepare(history.items.iter(), &[], 0, 1000);
        let rows = view.rows();
        let start = rows.iter().position(|row| row == "Arguments").unwrap() + 1;
        let end = rows.iter().position(|row| row == "Result").unwrap();
        assert_eq!(rows[start..end].join("\n"), expected);
        assert!(
            rows.iter()
                .all(|row| !row.contains('\u{1b}') && !row.contains('\t'))
        );
    }

    #[test]
    fn second_live_text_block_retains_its_reading_position_after_commit() {
        let mut live = LiveTranscript::default();
        live.observe(&AgentEvent::TurnAccepted {
            entry: 1,
            turn: 1,
            input: Message::user_input("request".into(), []),
        });
        let second = (0..40)
            .map(|line| format!("SECOND_{line:03}\n\n"))
            .collect::<String>();
        live.observe(&AgentEvent::TextDelta {
            block: 0,
            text: "FIRST".into(),
        });
        live.observe(&AgentEvent::TextDelta {
            block: 1,
            text: second.clone(),
        });
        let mut view = DetailView::default();
        view.prepare(live.projection().items.iter(), &[], live.revision(), 80);
        read_marker(&mut view, "SECOND_010");
        assert_eq!(
            view.anchor.as_ref().unwrap().point.unwrap().root,
            ContentRoot::Part(1)
        );
        let event = AgentEvent::AssistantCommitted {
            entry: 2,
            turn: 1,
            content: vec![
                Content::Text(" ".into()),
                Content::Text("FIRST".into()),
                Content::Text(second),
            ],
            tool_activities: vec![],
            termination: ResponseTermination::Completed,
        };
        view.commit_response(&live, &event);
        live.observe(&event);
        view.prepare(live.projection().items.iter(), &[], live.revision(), 20);
        assert!(view.window(8)[0].contains("SECOND_010"));
        let anchor = view.anchor.as_ref().unwrap();
        assert_eq!(
            anchor.entry,
            EntryKey::Fact(TranscriptSource { entry: 2, part: 2 })
        );
        assert_eq!(anchor.point.unwrap().root, ContentRoot::Part(0));
    }

    #[test]
    fn tail_follows_growth_and_explicit_return_releases_reading_position() {
        let mut live = LiveTranscript::default();
        live.observe(&AgentEvent::TextDelta {
            block: 0,
            text: "old\n".repeat(100),
        });
        let mut view = DetailView::default();
        view.prepare(live.projection().items.iter(), &[], live.revision(), 80);
        view.window(8);
        view.scroll_by(-20);
        assert!(view.anchor.is_some());
        view.scroll_by(isize::MAX);
        assert!(view.anchor.is_none());
        live.observe(&AgentEvent::TextDelta {
            block: 0,
            text: "LATEST_MARKER".into(),
        });
        view.invalidate();
        view.prepare(live.projection().items.iter(), &[], live.revision(), 12);
        assert!(view.window(8).join("").contains("LATEST_MARKER"));
        view.prepare(live.projection().items.iter(), &[], live.revision(), 80);
        assert!(view.window(12).join("").contains("LATEST_MARKER"));
    }

    #[test]
    fn keyed_history_pages_keep_a_tool_when_its_group_gains_entries() {
        let mut history = tool_history(vec![activity(0)]);
        history.items.extend((0..40).map(|index| {
            TranscriptItem::Assistant(TranscriptMessage {
                source: Some(TranscriptSource {
                    entry: index + 3,
                    part: 0,
                }),
                turn: Some(1),
                steering: false,
                parts: vec![TranscriptPart::Text(format!("ANSWER_{index}"))],
            })
        }));
        let mut view = DetailView::default();
        view.prepare(history.items.iter(), &[], 0, 80);
        view.window(8);
        view.navigate(KeyAction::Earlier);
        view.prepare(history.items.iter(), &[], 0, 80);
        read_marker(&mut view, "Tool 1:");
        let point = view.anchor.clone();
        let mut heading = DetailView::default();
        heading.prepare(history.items.iter(), &[], 0, 80);
        heading.window(8);
        heading.navigate(KeyAction::Earlier);
        heading.prepare(history.items.iter(), &[], 0, 80);
        heading.scroll_by(isize::MIN);
        let heading_point = heading.anchor.clone();
        let TranscriptItem::ActivityGroup(group) = &mut history.items[0] else {
            panic!()
        };
        group.activities.extend((1..70).map(activity));
        view.invalidate();
        view.prepare(history.items.iter(), &[], 1, 80);
        assert!(view.window(8)[0].contains("Tool 1:"));
        assert_eq!(view.anchor, point);
        assert!(view.rows()[0].contains("entries 1–32 of 110"));
        heading.prepare(history.items.iter(), &[], 1, 80);
        assert!(heading.window(8)[0].contains("entries 1–32 of 110"));
        assert_eq!(heading.anchor, heading_point);
        view.scroll_by(isize::MAX);
        assert!(
            view.anchor.is_some(),
            "an older page's end is not the live tail"
        );
        view.navigate(KeyAction::Later);
        view.prepare(history.items.iter(), &[], 1, 80);
        assert!(view.rows()[0].contains("entries 33–64 of 110"));
    }

    fn numbered_history() -> TranscriptProjection {
        TranscriptProjection {
            items: vec![TranscriptItem::Assistant(TranscriptMessage {
                source: Some(ion_core::TranscriptSource { entry: 1, part: 0 }),
                turn: Some(1),
                steering: false,
                parts: vec![TranscriptPart::Text(
                    (0..80)
                        .map(|index| format!("ROW_{index:03} {}\n", "z".repeat(45)))
                        .collect(),
                )],
            })],
        }
    }

    #[test]
    fn inspector_keeps_source_position_through_width_and_height_changes() {
        let history = numbered_history();
        let mut view = DetailView::default();
        view.prepare(history.items.iter(), &[], 0, 80);
        view.window(8);
        view.scroll_by(-24);
        let marker = view.window(8)[0][..7].to_owned();
        view.prepare(history.items.iter(), &[], 0, 30);
        assert!(view.window(8)[0].starts_with(&marker));
        assert!(view.window(12)[0].starts_with(&marker));
        view.prepare(history.items.iter(), &[], 0, 80);
        assert!(view.window(8)[0].starts_with(&marker));
    }

    #[test]
    fn inspector_keeps_source_position_when_live_refresh_invalidates_layout() {
        let history = numbered_history();
        let mut live = LiveTranscript::default();
        live.observe(&AgentEvent::TextDelta {
            block: 0,
            text: "live\n".repeat(20),
        });
        let mut view = DetailView::default();
        view.prepare(
            history.items.iter().chain(live.projection().items.iter()),
            &[],
            0,
            80,
        );
        view.window(8);
        view.scroll_by(-48);
        let first = view.window(8)[0].clone();
        live.observe(&AgentEvent::TextDelta {
            block: 0,
            text: "more live\n".repeat(20),
        });
        view.invalidate();
        view.prepare(
            history.items.iter().chain(live.projection().items.iter()),
            &[],
            1,
            80,
        );
        assert_eq!(view.window(8)[0], first);
    }

    #[test]
    fn inspector_bounds_follow_reflow_and_replaced_output() {
        let mut history = TranscriptProjection {
            items: vec![TranscriptItem::Assistant(TranscriptMessage {
                source: Some(ion_core::TranscriptSource { entry: 1, part: 0 }),
                turn: Some(1),
                steering: false,
                parts: vec![TranscriptPart::Text("x".repeat(400))],
            })],
        };
        let mut view = DetailView::default();
        view.prepare(history.items.iter(), &[], 0, 10);
        view.window(8);
        view.scroll_by(isize::MIN);
        let first_window = view.rows()[..8].to_vec();
        assert_eq!(view.window(8), first_window);

        // Widening and a shorter replacement must not leave stale scroll debt.
        view.prepare(history.items.iter(), &[], 0, 200);
        assert_eq!(view.window(8).len(), view.rows().len());
        if let TranscriptItem::Assistant(message) = &mut history.items[0] {
            message.parts = vec![TranscriptPart::Text("short".into())];
        }
        view.invalidate();
        view.prepare(history.items.iter(), &[], 1, 200);
        view.scroll_by(isize::MIN);
        assert!(view.window(8).iter().any(|row| row == "short"));
        assert!(view.window(0).is_empty());
        view.scroll_by(isize::MAX);
        assert_eq!(view.window(8).len(), view.rows().len());
    }

    #[test]
    fn conversation_tracks_active_facts_and_preserves_full_results() {
        let history = TranscriptProjection {
            items: vec![TranscriptItem::UserShell(UserShellActivity {
                source: ion_core::TranscriptSource { entry: 1, part: 0 },
                command: "previous-check".into(),
                outcome: Some(ion_core::UserShellOutcome::Observed {
                    output: ToolOutput::success(serde_json::json!({"stdout": "SAVED_SHELL"})),
                }),
                exclude_from_context: true,
            })],
        };
        let mut live = LiveTranscript::default();
        live.observe(&AgentEvent::TurnAccepted {
            entry: 3,
            turn: 2,
            input: Message::user_input("CURRENT_REQUEST".into(), []),
        });
        let activity = ToolActivity {
            kind: ToolActivityKind::Command,
            subject: Some("run-check".into()),
        };
        live.observe(&AgentEvent::AssistantCommitted {
            entry: 4,
            turn: 2,
            content: vec![Content::ToolCall(ToolCall {
                id: "call".into(),
                name: "exec".into(),
                arguments: serde_json::json!({"command": "run-check"}),
                raw_arguments: None,
            })],
            tool_activities: vec![StoredToolActivity {
                call_id: "call".into(),
                activity: activity.clone(),
            }],
            termination: ResponseTermination::Completed,
        });
        let mut view = DetailView::default();
        view.prepare(
            history.items.iter().chain(live.projection().items.iter()),
            live.notices(),
            live.revision(),
            120,
        );
        let initial = view.rows().join("\n");
        assert!(initial.contains("CURRENT_REQUEST"));
        assert!(initial.contains("SAVED_SHELL"));
        assert!(initial.contains("not shared with model"));
        assert!(initial.contains("queued"));
        assert!(initial.contains("no committed result"));
        live.observe(&AgentEvent::ToolFinished {
            call_id: "call".into(),
            name: "exec".into(),
            activity,
            outcome: ion_core::ToolOutcome::Observed {
                projection: ion_core::ToolResultProjection::Observed,
                output: ToolOutput {
                    value: serde_json::json!({"stdout": format!("{}\nEND_MARKER", "x".repeat(4_000))}),
                    images: Vec::new(),
                    artifacts: vec![crate::tool_output::tests::artifact("stdout", true)],
                    is_error: false,
                },
            },
        });
        live.observe(&AgentEvent::TextDelta {
            block: 0,
            text: "PROVISIONAL_TEXT".into(),
        });
        view.prepare(
            history.items.iter().chain(live.projection().items.iter()),
            live.notices(),
            live.revision(),
            120,
        );
        let running = view.rows().join("\n");
        assert!(running.contains("completed"));
        assert!(running.contains("END_MARKER"));
        assert!(running.contains("Assistant · provisional"));
        assert!(running.contains("PROVISIONAL_TEXT"));
        live.observe(&AgentEvent::ResponseRestarted);
        view.prepare(
            history.items.iter().chain(live.projection().items.iter()),
            live.notices(),
            live.revision(),
            120,
        );
        let restarted = view.rows().join("\n");
        assert!(!restarted.contains("PROVISIONAL_TEXT"));
        assert!(restarted.contains("END_MARKER"));
        assert_eq!(restarted.matches("Tool 1: exec").count(), 1);
        let mut selected = DetailView::new(Some((
            NonZeroUsize::new(1).unwrap(),
            tools(live.projection().items.iter())
                .next()
                .unwrap()
                .identity,
        )));
        selected.prepare(
            history.items.iter().chain(live.projection().items.iter()),
            live.notices(),
            live.revision(),
            120,
        );
        let output = selected.rows().join("\n");
        assert!(output.contains("Arguments"));
        assert!(output.contains("run-check"));
        assert!(output.contains("Result"));
        assert!(output.contains("END_MARKER"));
        assert!(output.contains("Session artifact 0 · stdout · 1234 bytes · complete"));
        let (_, evidence) = selected.copy_evidence().unwrap();
        assert!(evidence.contains("Session artifact 0 · stdout · 1234 bytes · complete"));
        assert!(evidence.contains(&"a".repeat(64)));
    }

    #[test]
    fn conversation_pages_reach_earlier_history_and_rewrap() {
        let history = TranscriptProjection {
            items: (0..40)
                .map(|index| {
                    TranscriptItem::Assistant(TranscriptMessage {
                        source: Some(ion_core::TranscriptSource {
                            entry: index + 1,
                            part: 0,
                        }),
                        turn: Some(index),
                        steering: false,
                        parts: vec![TranscriptPart::Text(format!("ANSWER_{index}"))],
                    })
                })
                .collect(),
        };
        let mut view = DetailView::default();
        view.prepare(history.items.iter(), &[], 0, 80);
        let tail = view.rows().join("\n");
        assert!(tail.contains("ANSWER_39"));
        assert!(!tail.contains("ANSWER_0\n"));
        view.navigate(KeyAction::Earlier);
        view.prepare(history.items.iter(), &[], 0, 80);
        let earlier = view.rows().join("\n");
        assert!(earlier.contains("ANSWER_0\n"));
        assert!(!earlier.contains("ANSWER_39"));
        view.navigate(KeyAction::Later);
        view.prepare(history.items.iter(), &[], 0, 6);
        assert!(
            view.rows()
                .iter()
                .all(|row| unicode_width::UnicodeWidthStr::width(row.as_str()) <= 6)
        );
        assert!(view.rows().join("").contains("ANSWER_39"));
    }
}
