//! Disposable inspection layout keyed by Core entries and source fields.
//! Source bytes and named writer components use the shared provenance kernel;
//! flattened JSON dumps and fitted rows never define reading positions.
use super::{Entry, EntryKey};
use crate::display_text::{Component, MappedLine, Mapping, Origin};
use crate::transcript_render::{
    BlockLayout, ReadingPoint, kind_label, source_message_layout, source_thinking_layout,
    state_label,
};
use ion_core::{TranscriptActivity, TranscriptItem};
use ratatui::text::{Line, Span};
use serde_json::Value;
use std::ops::Range;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum PathPart {
    Key(String),
    Index(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum JsonField {
    Arguments,
    Result,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Field {
    Gap,
    PageHeading,
    Heading,
    Message,
    Command,
    CallId,
    Subject,
    ArgumentsLabel,
    ResultLabel,
    ResultNotice,
    Json {
        field: JsonField,
        path: Vec<PathPart>,
        closing: bool,
    },
    DiffLabel,
    Diff,
    Artifact(usize),
    Image(usize),
    ChildLabel,
    Readiness,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Anchor {
    pub entry: EntryKey,
    pub field: Field,
    pub point: Option<ReadingPoint>,
    pub previous: Option<EntryKey>,
}

struct Section {
    entry: EntryKey,
    field: Field,
    previous: Option<EntryKey>,
    rows: Range<usize>,
    layout: BlockLayout,
}

#[derive(Default)]
pub(super) struct Document {
    sections: Vec<Section>,
    pub len: usize,
    previous: Option<EntryKey>,
}

impl Document {
    fn push(&mut self, entry: EntryKey, field: Field, layout: BlockLayout) {
        if layout.rows.is_empty() {
            return;
        }
        let end = self.len + layout.rows.len();
        self.sections.push(Section {
            entry,
            field,
            previous: self.previous,
            rows: self.len..end,
            layout,
        });
        self.len = end;
    }

    pub fn generated(&mut self, entry: EntryKey, field: Field, text: &str, width: usize) {
        self.push(
            entry,
            field,
            BlockLayout::mapped(
                MappedLine::component(
                    Line::from(Span::raw(text.to_owned())),
                    Component::Generated(0),
                    0,
                ),
                width,
            ),
        );
    }

    fn raw(&mut self, entry: EntryKey, field: Field, prefix: &str, text: &str, width: usize) {
        let mut source = MappedLine::component(
            Line::from(Span::raw(prefix.to_owned())),
            Component::Generated(0),
            0,
        );
        let start = prefix.len();
        source.line.spans.push(text.to_owned().into());
        source.map.push(Mapping {
            display: start..start + text.len(),
            origin: Origin::Raw(0..text.len()),
            linear: true,
        });
        self.push(entry, field, BlockLayout::mapped(source, width));
    }

    pub fn entry(
        &mut self,
        key: EntryKey,
        previous: Option<EntryKey>,
        entry: &Entry<'_>,
        number: usize,
        width: usize,
    ) {
        self.previous = previous;
        self.generated(key, Field::Gap, "", width);
        match entry {
            Entry::Tool(activity) => self.activity(activity, &number.to_string(), width),
            Entry::Thinking(note) => {
                self.generated(key, Field::Heading, "Assistant", width);
                self.push(
                    key,
                    Field::Message,
                    source_thinking_layout(&note.text, width),
                );
            }
            Entry::Item(item) => match item {
                TranscriptItem::User(message) | TranscriptItem::Assistant(message) => {
                    let user = matches!(item, TranscriptItem::User(_));
                    let title = if user {
                        if message.steering { "Steering" } else { "User" }
                    } else if message.source.is_none() {
                        "Assistant · provisional"
                    } else {
                        "Assistant"
                    };
                    self.generated(key, Field::Heading, title, width);
                    self.push(
                        key,
                        Field::Message,
                        source_message_layout(message, user, width),
                    );
                }
                TranscriptItem::Readiness { turn, notices, .. } => {
                    self.generated(
                        key,
                        Field::Readiness,
                        &crate::transcript_render::readiness_text(*turn, notices),
                        width,
                    );
                }
                TranscriptItem::UserShell(shell) => {
                    self.generated(
                        key,
                        Field::Heading,
                        if shell.exclude_from_context {
                            "User shell · not shared with model"
                        } else {
                            "User shell · shared with model"
                        },
                        width,
                    );
                    self.raw(key, Field::Command, "", &shell.command, width);
                    match &shell.outcome {
                        Some(ion_core::UserShellOutcome::Observed { output }) => {
                            self.generated(
                                key,
                                Field::ResultLabel,
                                if output.is_error {
                                    "Result marked as error"
                                } else {
                                    "Result marked as successful"
                                },
                                width,
                            );
                            self.json(key, JsonField::Result, &output.value, width);
                            self.artifacts(key, &output.artifacts, width);
                            for (index, image) in output.images.iter().enumerate() {
                                self.generated(
                                    key,
                                    Field::Image(index),
                                    &format!("[image: {}]", image.mime_type().as_str()),
                                    width,
                                );
                            }
                        }
                        Some(ion_core::UserShellOutcome::Unknown) | None => {
                            self.generated(
                                key,
                                Field::ResultNotice,
                                ion_core::UserShellOutcome::unknown_notice(),
                                width,
                            );
                        }
                    }
                }
                TranscriptItem::ActivityGroup(_) => {
                    unreachable!("groups are flattened into tool entries")
                }
            },
        }
    }

    pub fn activity(&mut self, activity: &TranscriptActivity, number: &str, width: usize) {
        let key = EntryKey::Tool(activity.identity);
        self.generated(
            key,
            Field::Heading,
            &format!(
                "Tool {number}: {} · {} · {}",
                activity.name,
                kind_label(activity.activity.kind),
                state_label(activity.state)
            ),
            width,
        );
        self.raw(key, Field::CallId, "Call ID: ", &activity.call_id, width);
        if let Some(subject) = &activity.activity.subject {
            self.raw(key, Field::Subject, "Subject: ", subject, width);
        }
        self.generated(key, Field::ArgumentsLabel, "Arguments", width);
        self.json(key, JsonField::Arguments, &activity.arguments, width);
        self.generated(key, Field::ResultLabel, "Result", width);
        if let Some(result) = &activity.result {
            if let Some(notice) = result
                .projection
                .and_then(ion_core::ToolResultProjection::notice)
            {
                self.generated(key, Field::ResultNotice, notice, width);
            }
            if let Some(diff) = crate::edit_diff::recorded(activity) {
                for (field, value) in diff.fields {
                    if field != "diff" {
                        self.json_value(
                            key,
                            JsonField::Result,
                            value,
                            &mut vec![PathPart::Key(field.clone())],
                            &format!("{field}: "),
                            false,
                            width,
                        );
                    }
                }
                self.generated(key, Field::DiffLabel, &diff.heading(), width);
                if diff.text.is_empty() {
                    self.generated(key, Field::Diff, "No text changes", width);
                } else {
                    self.raw(key, Field::Diff, "", diff.text, width);
                }
            } else {
                self.json(key, JsonField::Result, &result.value, width);
            }
            self.artifacts(key, &result.artifacts, width);
            for (index, mime) in result.image_mime_types.iter().enumerate() {
                self.generated(key, Field::Image(index), &format!("[image: {mime}]"), width);
            }
        } else {
            self.generated(key, Field::ResultNotice, "[no committed result]", width);
        }
        for (index, child) in activity.children.iter().enumerate() {
            self.generated(
                EntryKey::Tool(child.identity),
                Field::ChildLabel,
                "Child activity · not included in model context",
                width,
            );
            self.activity(child, &format!("{number}.{}", index + 1), width);
        }
    }

    fn artifacts(&mut self, key: EntryKey, artifacts: &[ion_core::ArtifactRef], width: usize) {
        for (index, metadata) in crate::tool_output::artifact_metadata(artifacts).enumerate() {
            self.generated(key, Field::Artifact(index), &metadata, width);
        }
    }

    fn json(&mut self, key: EntryKey, field: JsonField, value: &Value, width: usize) {
        self.json_value(key, field, value, &mut Vec::new(), "", false, width);
    }

    // A leaf's key is a typed path in the original Value. Adding a sibling or
    // changing a tool heading cannot change its source coordinate. Escapes map
    // back to the scalar's actual bytes; punctuation has no byte claim.
    #[allow(clippy::too_many_arguments)]
    fn json_value(
        &mut self,
        key: EntryKey,
        field: JsonField,
        value: &Value,
        path: &mut Vec<PathPart>,
        label: &str,
        comma: bool,
        width: usize,
    ) {
        let indent = "  ".repeat(path.len());
        let prefix = format!("{indent}{label}");
        let suffix = if comma { "," } else { "" };
        let coordinate = |closing| Field::Json {
            field,
            path: path.clone(),
            closing,
        };
        let multiline_text = |value: &Value| {
            field == JsonField::Result && value.as_str().is_some_and(|text| text.contains('\n'))
        };
        match value {
            Value::Object(values) if !values.is_empty() => {
                self.generated(key, coordinate(false), &format!("{prefix}{{"), width);
                // Keep status/capture metadata before literal output, so the
                // inspector's tail shows diagnostics rather than protocol flags.
                let fields = values
                    .iter()
                    .filter(|(_, value)| !multiline_text(value))
                    .chain(values.iter().filter(|(_, value)| multiline_text(value)));
                for (index, (name, value)) in fields.enumerate() {
                    path.push(PathPart::Key(name.clone()));
                    self.json_value(
                        key,
                        field,
                        value,
                        path,
                        &format!("{}: ", serde_json::to_string(name).expect("JSON key")),
                        index + 1 < values.len(),
                        width,
                    );
                    path.pop();
                }
                self.generated(
                    key,
                    Field::Json {
                        field,
                        path: path.clone(),
                        closing: true,
                    },
                    &format!("{indent}}}{suffix}"),
                    width,
                );
            }
            Value::Array(values) if !values.is_empty() => {
                self.generated(key, coordinate(false), &format!("{prefix}["), width);
                for (index, value) in values.iter().enumerate() {
                    path.push(PathPart::Index(index));
                    self.json_value(key, field, value, path, "", index + 1 < values.len(), width);
                    path.pop();
                }
                self.generated(
                    key,
                    Field::Json {
                        field,
                        path: path.clone(),
                        closing: true,
                    },
                    &format!("{indent}]{suffix}"),
                    width,
                );
            }
            Value::String(text) if multiline_text(value) => {
                // This is a readable value view; evidence copying still uses
                // the original JSON. Padding is layout, not scalar source bytes.
                let mut layout = BlockLayout::mapped(
                    MappedLine::component(
                        Line::from(Span::raw(format!("{prefix}(text)"))),
                        Component::Generated(0),
                        0,
                    ),
                    width,
                );
                layout.append_literal(&format!("{indent}  "), text, width);
                self.push(key, coordinate(false), layout);
            }
            Value::String(text) => {
                self.push(
                    key,
                    coordinate(false),
                    BlockLayout::mapped(json_string(&prefix, text, suffix), width),
                );
            }
            _ => self.generated(
                key,
                coordinate(false),
                &format!("{prefix}{value}{suffix}"),
                width,
            ),
        }
    }

    pub fn anchor(&self, row: usize) -> Anchor {
        let section = &self.sections[self
            .sections
            .partition_point(|section| section.rows.end <= row)];
        Anchor {
            entry: section.entry,
            field: section.field.clone(),
            point: Some(section.layout.reading_point(row - section.rows.start)),
            previous: section.previous,
        }
    }

    pub fn locate(&self, anchor: &Anchor) -> usize {
        if let Some(section) = self
            .sections
            .iter()
            .find(|section| section.entry == anchor.entry && section.field == anchor.field)
        {
            return section.rows.start
                + anchor
                    .point
                    .and_then(|point| section.layout.locate(point))
                    .unwrap_or(0);
        }
        // A field or entry can disappear (for example, an abandoned response).
        // Project to its surviving boundary without changing the retained point.
        self.sections
            .iter()
            .find(|section| section.entry == anchor.entry)
            .or_else(|| {
                self.sections
                    .iter()
                    .find(|section| Some(section.entry) == anchor.previous)
            })
            .map_or(0, |section| section.rows.start)
    }

    pub fn window(&self, start: usize, height: usize) -> Vec<String> {
        let end = start.saturating_add(height).min(self.len);
        self.sections
            .iter()
            .skip_while(|section| section.rows.end <= start)
            .take_while(|section| section.rows.start < end)
            .flat_map(|section| {
                let first = start.saturating_sub(section.rows.start);
                let last = end.min(section.rows.end) - section.rows.start;
                section.layout.rows[first..last]
                    .iter()
                    .map(ToString::to_string)
            })
            .collect()
    }
}

fn json_string(prefix: &str, text: &str, suffix: &str) -> MappedLine {
    let quoted = serde_json::to_string(text).expect("JSON string");
    let display = format!("{prefix}{quoted}{suffix}");
    let mut offset = prefix.len() + 1;
    let mut map = vec![Mapping {
        display: 0..offset,
        origin: Origin::Component {
            component: Component::Generated(0),
            bytes: 0..offset,
        },
        linear: true,
    }];
    let (mut raw_start, mut display_start) = (0, offset);
    for (byte, character) in text.char_indices() {
        let escaped = match character {
            '"' | '\\' | '\x08' | '\x0c' | '\n' | '\r' | '\t' => Some(2),
            '\0'..='\x1f' => Some(6),
            _ => None,
        };
        if let Some(len) = escaped {
            if raw_start < byte {
                map.push(Mapping {
                    display: display_start..offset,
                    origin: Origin::Raw(raw_start..byte),
                    linear: true,
                });
            }
            map.push(Mapping {
                display: offset..offset + len,
                origin: Origin::Raw(byte..byte + character.len_utf8()),
                linear: false,
            });
            offset += len;
            raw_start = byte + character.len_utf8();
            display_start = offset;
        } else {
            offset += character.len_utf8();
        }
    }
    assert_eq!(
        offset,
        prefix.len() + quoted.len() - 1,
        "JSON escapes cover the scalar"
    );
    if raw_start < text.len() {
        map.push(Mapping {
            display: display_start..offset,
            origin: Origin::Raw(raw_start..text.len()),
            linear: true,
        });
    }
    map.push(Mapping {
        display: offset..display.len(),
        origin: Origin::Component {
            component: Component::Generated(1),
            bytes: 0..display.len() - offset,
        },
        linear: true,
    });
    MappedLine {
        line: Line::from(Span::raw(display)),
        map,
    }
}
