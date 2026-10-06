//! Paged inspection of canonical saved facts and the current live transcript.
use ion_core::{LiveTranscript, TranscriptActivity, TranscriptItem, TranscriptProjection};
use ion_terminal::KeyCode;
use std::num::NonZeroUsize;

use crate::display_text::push_wrapped;
use crate::transcript_render::{kind_label, render_message, state_label};

const PAGE_ENTRIES: usize = 32;

#[derive(Default)]
pub struct DetailView {
    tool: Option<NonZeroUsize>,
    page_end: Option<usize>,
    pub scroll: usize,
    page: Option<Page>,
}

struct Page {
    revision: Option<u64>,
    width: usize,
    start: usize,
    end: usize,
    total: usize,
    rows: Vec<String>,
}

enum Entry<'a> {
    Item(&'a TranscriptItem, bool),
    Tool(&'a TranscriptActivity),
}

impl DetailView {
    pub fn new(tool: Option<NonZeroUsize>) -> Self {
        Self {
            tool,
            ..Self::default()
        }
    }

    /// The saved projection is immutable between host refreshes; invalidate on replacement.
    pub fn invalidate(&mut self) {
        self.page = None;
    }

    pub fn key(&mut self, key: KeyCode) {
        let Some(page) = &self.page else { return };
        match key {
            KeyCode::Up | KeyCode::PageUp => {
                self.page_end = Some(page.end);
                let step = if key == KeyCode::Up { 1 } else { 12 };
                self.scroll = self
                    .scroll
                    .saturating_add(step)
                    .min(page.rows.len().saturating_sub(1));
            }
            KeyCode::Down | KeyCode::PageDown => {
                let step = if key == KeyCode::Down { 1 } else { 12 };
                self.scroll = self.scroll.saturating_sub(step);
            }
            KeyCode::Left if self.tool.is_none() && page.start > 0 => {
                self.page_end = Some(page.start);
                self.scroll = 0;
                self.invalidate();
            }
            KeyCode::Right if self.tool.is_none() => {
                let end = page.end.saturating_add(PAGE_ENTRIES).min(page.total);
                self.page_end = (end < page.total).then_some(end);
                self.scroll = 0;
                self.invalidate();
            }
            _ => {}
        }
    }

    pub fn prepare(
        &mut self,
        history: &TranscriptProjection,
        live: Option<&LiveTranscript>,
        width: usize,
    ) {
        let width = width.max(1);
        let revision = live.map(LiveTranscript::revision);
        if self
            .page
            .as_ref()
            .is_none_or(|page| page.revision != revision || page.width != width)
        {
            let count = |projection: &TranscriptProjection| {
                projection
                    .items
                    .iter()
                    .map(|item| match item {
                        TranscriptItem::ActivityGroup(group) => group.activities.len(),
                        _ => 1,
                    })
                    .sum::<usize>()
            };
            let total = count(history) + live.map_or(0, |live| count(live.projection()));
            let end = self.page_end.unwrap_or(total).min(total);
            let start = end.saturating_sub(PAGE_ENTRIES);
            let mut rows = Vec::new();
            if let Some(number) = self.tool {
                if let Some(activity) = entries(history, live)
                    .filter_map(|entry| match entry {
                        Entry::Tool(activity) => Some(activity),
                        Entry::Item(..) => None,
                    })
                    .nth(number.get() - 1)
                {
                    render_tool(&mut rows, number.get(), activity, width);
                } else {
                    push_wrapped(&mut rows, "Tool result not found", width);
                }
            } else {
                push_wrapped(
                    &mut rows,
                    &format!(
                        "Conversation · entries {}–{end} of {total}",
                        if total == 0 { 0 } else { start + 1 }
                    ),
                    width,
                );
                let mut tool_number = 0;
                for (index, entry) in entries(history, live).enumerate().take(end) {
                    if matches!(entry, Entry::Tool(_)) {
                        tool_number += 1;
                    }
                    if index < start {
                        continue;
                    }
                    rows.push(String::new());
                    match entry {
                        Entry::Tool(activity) => {
                            render_tool(&mut rows, tool_number, activity, width)
                        }
                        Entry::Item(item, provisional) => match item {
                            TranscriptItem::User(message) => {
                                push_wrapped(
                                    &mut rows,
                                    if message.steering { "Steering" } else { "User" },
                                    width,
                                );
                                render_message(&mut rows, message, true, width);
                            }
                            TranscriptItem::Assistant(message) => {
                                push_wrapped(
                                    &mut rows,
                                    if provisional {
                                        "Assistant · provisional"
                                    } else {
                                        "Assistant"
                                    },
                                    width,
                                );
                                render_message(&mut rows, message, false, width);
                            }
                            TranscriptItem::UserShell(shell) => {
                                push_wrapped(
                                    &mut rows,
                                    if shell.exclude_from_context {
                                        "User shell · not shared with model"
                                    } else {
                                        "User shell · shared with model"
                                    },
                                    width,
                                );
                                push_wrapped(
                                    &mut rows,
                                    if shell.is_error {
                                        "Result marked as error"
                                    } else {
                                        "Result marked as successful"
                                    },
                                    width,
                                );
                                push_wrapped(&mut rows, &shell.command, width);
                                render_json(&mut rows, &shell.output, width);
                            }
                            TranscriptItem::ActivityGroup(_) => {
                                unreachable!("groups are flattened into tool entries")
                            }
                        },
                    }
                }
            }
            if self.tool.is_none()
                && end == total
                && let Some(live) = live
            {
                for notice in live.notices() {
                    push_wrapped(&mut rows, &format!("Notice: {notice}"), width);
                }
            }
            if let Some(page) = &self.page
                && self.scroll > 0
                && page.width == width
                && page.start == start
                && page.end == end
            {
                // Keep the inspected prefix stationary as the live tail grows.
                self.scroll = rows
                    .len()
                    .saturating_sub(page.rows.len().saturating_sub(self.scroll));
            }
            self.page = Some(Page {
                revision,
                width,
                start,
                end,
                total,
                rows,
            });
        }
    }

    pub fn rows(&self) -> &[String] {
        &self.page.as_ref().expect("prepare precedes display").rows
    }

    pub fn controls(&self) -> String {
        self.tool.map_or_else(
            || "Conversation · ← older / → newer · ↑/↓ scroll · Esc or Ctrl-O closes".into(),
            |number| format!("Tool {number} · ↑/↓ scroll · Esc or Ctrl-O closes"),
        )
    }
}

fn entries<'a>(
    history: &'a TranscriptProjection,
    live: Option<&'a LiveTranscript>,
) -> impl Iterator<Item = Entry<'a>> {
    let provisional = live
        .and_then(LiveTranscript::provisional_item_index)
        .map(|index| history.items.len() + index);
    history
        .items
        .iter()
        .chain(live.into_iter().flat_map(|live| &live.projection().items))
        .enumerate()
        .flat_map(move |(index, item)| {
            let activities = match item {
                TranscriptItem::ActivityGroup(group) => group.activities.as_slice(),
                _ => &[],
            };
            (!matches!(item, TranscriptItem::ActivityGroup(_)))
                .then_some(Entry::Item(item, provisional == Some(index)))
                .into_iter()
                .chain(activities.iter().map(Entry::Tool))
        })
}

pub fn tools(history: &TranscriptProjection) -> impl Iterator<Item = &TranscriptActivity> {
    entries(history, None).filter_map(|entry| match entry {
        Entry::Tool(activity) => Some(activity),
        Entry::Item(..) => None,
    })
}

fn render_tool(rows: &mut Vec<String>, number: usize, activity: &TranscriptActivity, width: usize) {
    render_activity(rows, &number.to_string(), activity, width);
}

fn render_activity(
    rows: &mut Vec<String>,
    number: &str,
    activity: &TranscriptActivity,
    width: usize,
) {
    push_wrapped(
        rows,
        &format!(
            "Tool {number}: {} · {} · {}",
            activity.name,
            kind_label(activity.activity.kind),
            state_label(activity.state)
        ),
        width,
    );
    push_wrapped(rows, &format!("Call ID: {}", activity.call_id), width);
    if let Some(subject) = &activity.activity.subject {
        push_wrapped(rows, &format!("Subject: {subject}"), width);
    }
    push_wrapped(rows, "Arguments", width);
    render_json(rows, &activity.arguments, width);
    push_wrapped(rows, "Result", width);
    if let Some(result) = &activity.result {
        if let Some(notice) = result.projection.notice() {
            push_wrapped(rows, notice, width);
        }
        render_json(rows, &result.value, width);
        for mime in &result.image_mime_types {
            push_wrapped(rows, &format!("[image: {mime}]"), width);
        }
    } else {
        push_wrapped(rows, "[no committed result]", width);
    }
    for (child, activity) in activity.children.iter().enumerate() {
        push_wrapped(
            rows,
            "Child activity · not included in model context",
            width,
        );
        render_activity(rows, &format!("{number}.{}", child + 1), activity, width);
    }
}

fn render_json(rows: &mut Vec<String>, value: &serde_json::Value, width: usize) {
    push_wrapped(
        rows,
        &serde_json::to_string_pretty(value).expect("JSON values are serializable"),
        width,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ion_ai::{Content, Message, ResponseTermination, ToolCall};
    use ion_core::{CodingAgentEvent as AgentEvent, CodingToolOutput as ToolOutput};
    use ion_core::{
        StoredToolActivity, ToolActivity, ToolActivityKind, TranscriptMessage, TranscriptPart,
        UserShellActivity,
    };

    #[test]
    fn conversation_tracks_active_facts_and_preserves_full_results() {
        let history = TranscriptProjection {
            items: vec![TranscriptItem::UserShell(UserShellActivity {
                command: "previous-check".into(),
                output: serde_json::json!({"stdout": "SAVED_SHELL"}),
                is_error: false,
                exclude_from_context: true,
            })],
        };
        let mut live =
            LiveTranscript::with_user_input(&Message::user_input("CURRENT_REQUEST".into(), []));
        live.observe(AgentEvent::TurnAccepted { turn: 2 });
        let activity = ToolActivity {
            kind: ToolActivityKind::Command,
            subject: Some("run-check".into()),
        };
        live.observe(AgentEvent::AssistantCommitted {
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
        view.prepare(&history, Some(&live), 120);
        let initial = view.rows().join("\n");
        assert!(initial.contains("CURRENT_REQUEST"));
        assert!(initial.contains("SAVED_SHELL"));
        assert!(initial.contains("not shared with model"));
        assert!(initial.contains("queued"));
        assert!(initial.contains("no committed result"));
        live.observe(AgentEvent::ToolFinished {
            projection: ion_core::ToolResultProjection::Observed,
            call_id: "call".into(),
            name: "exec".into(),
            activity,
            output: ToolOutput {
                value: serde_json::json!({"stdout": format!("{}\nEND_MARKER", "x".repeat(4_000))}),
                images: Vec::new(),
                is_error: false,
            },
        });
        live.observe(AgentEvent::TextDelta("PROVISIONAL_TEXT".into()));
        view.prepare(&history, Some(&live), 120);
        let running = view.rows().join("\n");
        assert!(running.contains("completed"));
        assert!(running.contains("END_MARKER"));
        assert!(running.contains("Assistant · provisional"));
        assert!(running.contains("PROVISIONAL_TEXT"));
        live.observe(AgentEvent::ResponseRestarted);
        view.prepare(&history, Some(&live), 120);
        let restarted = view.rows().join("\n");
        assert!(!restarted.contains("PROVISIONAL_TEXT"));
        assert!(restarted.contains("END_MARKER"));
        assert_eq!(restarted.matches("Tool 1: exec").count(), 1);
        let mut selected = DetailView::new(NonZeroUsize::new(1));
        selected.prepare(&history, Some(&live), 120);
        let output = selected.rows().join("\n");
        assert!(output.contains("Arguments"));
        assert!(output.contains("run-check"));
        assert!(output.contains("Result"));
        assert!(output.contains("END_MARKER"));
    }

    #[test]
    fn conversation_pages_reach_earlier_history_and_rewrap() {
        let history = TranscriptProjection {
            items: (0..40)
                .map(|index| {
                    TranscriptItem::Assistant(TranscriptMessage {
                        turn: Some(index),
                        steering: false,
                        parts: vec![TranscriptPart::Text(format!("ANSWER_{index}"))],
                    })
                })
                .collect(),
        };
        let mut view = DetailView::default();
        view.prepare(&history, None, 80);
        let tail = view.rows().join("\n");
        assert!(tail.contains("ANSWER_39"));
        assert!(!tail.contains("ANSWER_0\n"));
        view.key(KeyCode::Left);
        view.prepare(&history, None, 80);
        let earlier = view.rows().join("\n");
        assert!(earlier.contains("ANSWER_0\n"));
        assert!(!earlier.contains("ANSWER_39"));
        view.key(KeyCode::Right);
        view.prepare(&history, None, 6);
        assert!(
            view.rows()
                .iter()
                .all(|row| unicode_width::UnicodeWidthStr::width(row.as_str()) <= 6)
        );
        assert!(view.rows().join("").contains("ANSWER_39"));
    }
}
