//! Semantic views of recorded discovery and external-tool observations.
use super::*;

const PREVIEW_ITEMS: usize = 3;

#[derive(Clone, Copy)]
enum Collection {
    Entries,
    Files,
    Matches,
}

impl Collection {
    fn label(self) -> &'static str {
        match self {
            Self::Entries => "entries",
            Self::Files => "files",
            Self::Matches => "matches",
        }
    }
}

pub(super) fn render(
    rows: &mut Vec<MappedLine>,
    activity: &TranscriptActivity,
    prefix: &str,
    width: usize,
    detail: &RenderOptions<'_>,
) {
    let Some(result) = &activity.result else {
        return;
    };
    let value = &result.value;
    let collection = match activity.activity.kind {
        ToolActivityKind::List => value["entries"]
            .as_array()
            .map(|items| (Collection::Entries, items)),
        ToolActivityKind::Search => value["files"]
            .as_array()
            .map(|items| (Collection::Files, items))
            .or_else(|| {
                value["matches"]
                    .as_array()
                    .map(|items| (Collection::Matches, items))
            }),
        _ => None,
    };
    if let Some((collection, items)) = collection {
        let label = collection.label();
        note(
            rows,
            prefix,
            &format!("{label} · {} recorded", items.len()),
            width,
            detail.theme,
        );
        let limit = if detail.detail == OutputDetail::Compact {
            PREVIEW_ITEMS
        } else {
            usize::MAX
        };
        let mut shortened = false;
        for (index, item) in items.iter().enumerate().take(limit) {
            if let Some(line) = collection_item(collection, item, index) {
                shortened |= push_line(rows, line, prefix, width, detail, true);
            } else {
                // A presentation kind is not a payload schema for custom tools.
                note(
                    rows,
                    prefix,
                    "Unrecognized recorded item · /tool all for the full value",
                    width,
                    detail.theme,
                );
            }
        }
        disclosure(
            rows,
            items.len().saturating_sub(limit),
            shortened,
            label,
            prefix,
            width,
            detail,
        );
    }
    if let Some(value) = value
        .get("structured_content")
        .filter(|value| !value.is_null())
    {
        note(rows, prefix, "structured content", width, detail.theme);
        render_value(rows, value, Field::Structured, prefix, width, detail, true);
    }
}

fn collection_item(collection: Collection, value: &Value, index: usize) -> Option<MappedLine> {
    let literal = |text: &str, field| {
        MappedLine::component(
            Line::from(Span::raw(text.to_owned())),
            Component::Item { index, field },
            0,
        )
    };
    let decoration = |text: String| MappedLine::generated(Line::from(Span::raw(text)), index);
    match collection {
        Collection::Files => Some(literal(value.as_str()?, Field::Path)),
        Collection::Entries => {
            let mut line = literal(value["name"].as_str()?, Field::Path);
            if let Some(kind) = value["type"].as_str() {
                line.append(&decoration(format!(" · {kind}")));
            }
            Some(line)
        }
        Collection::Matches => {
            let mut line = literal(value["path"].as_str()?, Field::Path);
            line.append(&decoration(format!(":{}: ", value["line"].as_u64()?)));
            line.append(&literal(value["text"].as_str()?, Field::Content));
            Some(line)
        }
    }
}

fn push_line(
    rows: &mut Vec<MappedLine>,
    mut line: MappedLine,
    prefix: &str,
    width: usize,
    detail: &RenderOptions<'_>,
    preview: bool,
) -> bool {
    let padding = format!("{prefix}  ");
    let mut shortened = false;
    if preview && detail.detail == OutputDetail::Compact {
        (line, shortened) = fit_mapped(
            line,
            width
                .saturating_sub(unicode_width::UnicodeWidthStr::width(padding.as_str()))
                .max(1),
        );
    }
    rows.extend(wrap_mapped(
        &padding,
        &padding,
        line,
        width,
        Wrapping::Literal,
    ));
    shortened
}

fn disclosure(
    rows: &mut Vec<MappedLine>,
    omitted: usize,
    shortened: bool,
    label: &str,
    prefix: &str,
    width: usize,
    detail: &RenderOptions<'_>,
) {
    let message = if omitted > 0 {
        Some(format!(
            "… {omitted} more recorded {label} · {}",
            detail.details_key
        ))
    } else if shortened {
        Some(format!(
            "… long recorded value shortened · {}",
            detail.details_key
        ))
    } else {
        None
    };
    if let Some(message) = message {
        note(rows, prefix, &message, width, detail.theme);
    }
}

/// Value nodes are numbered in original-tree traversal order even when their
/// rows are hidden. Scalar bytes and key bytes remain separate; formatting has
/// only generated boundaries. No coordinate depends on serialized JSON.
fn render_value(
    rows: &mut Vec<MappedLine>,
    value: &Value,
    field: Field,
    prefix: &str,
    width: usize,
    detail: &RenderOptions<'_>,
    preview: bool,
) {
    let mut tree = ValueView {
        rows,
        field,
        prefix,
        width,
        detail,
        next_node: 0,
        shortened: false,
        limit: if preview && detail.detail == OutputDetail::Compact {
            PREVIEW_ITEMS
        } else {
            usize::MAX
        },
    };
    tree.visit(value, None, None, 0);
    let omitted = tree.next_node.saturating_sub(tree.limit);
    let shortened = tree.shortened;
    disclosure(
        rows,
        omitted,
        shortened,
        "value nodes",
        prefix,
        width,
        detail,
    );
}

struct ValueView<'a, 'theme> {
    rows: &'a mut Vec<MappedLine>,
    field: Field,
    prefix: &'a str,
    width: usize,
    detail: &'a RenderOptions<'theme>,
    next_node: usize,
    limit: usize,
    shortened: bool,
}

impl ValueView<'_, '_> {
    fn visit(&mut self, value: &Value, key: Option<&str>, index: Option<usize>, depth: usize) {
        let node = self.next_node;
        self.next_node += 1;
        if node < self.limit {
            let generated = |text: String| MappedLine::generated(Line::from(Span::raw(text)), node);
            let scalar = |text: &str, key| {
                MappedLine::component(
                    Line::from(Span::raw(text.to_owned())),
                    Component::Value {
                        field: self.field,
                        node,
                        key,
                    },
                    0,
                )
            };
            let mut line = generated("  ".repeat(depth));
            if let Some(key) = key {
                line.append(&scalar(key, true));
                line.append(&generated(": ".into()));
            } else if let Some(index) = index {
                line.append(&generated(format!("[{index}]: ")));
            }
            let boundary = |text: String| {
                MappedLine::boundary(
                    Line::from(Span::raw(text)),
                    Component::Value {
                        field: self.field,
                        node,
                        key: false,
                    },
                )
            };
            line.append(&match value {
                Value::String(text) if text.is_empty() => boundary("\"\"".into()),
                Value::String(text) => scalar(text, false),
                Value::Object(fields) if !fields.is_empty() => {
                    boundary(format!("{{{} fields}}", fields.len()))
                }
                Value::Array(items) if !items.is_empty() => {
                    boundary(format!("[{} items]", items.len()))
                }
                _ => boundary(value.to_string()),
            });
            self.shortened |= push_line(
                self.rows,
                line,
                self.prefix,
                self.width,
                self.detail,
                self.limit != usize::MAX,
            );
        }
        match value {
            Value::Object(fields) => {
                for (key, child) in fields {
                    self.visit(child, Some(key), None, depth + 1);
                }
            }
            Value::Array(items) => {
                for (index, child) in items.iter().enumerate() {
                    self.visit(child, None, Some(index), depth + 1);
                }
            }
            _ => {}
        }
    }
}

/// Capture/conversion limitations are observations, not presentation folding.
/// They remain visible even when a body or a group of observations is folded.
pub(super) fn notices(
    rows: &mut Vec<MappedLine>,
    value: &Value,
    prefix: &str,
    width: usize,
    detail: &RenderOptions<'_>,
) {
    for (key, field) in [
        ("omissions", Field::Omission),
        ("conversion_notices", Field::Notice),
    ] {
        if let Some(items) = value[key].as_array() {
            for (index, text) in items
                .iter()
                .enumerate()
                .filter_map(|(index, value)| value.as_str().map(|text| (index, text)))
            {
                push_line(
                    rows,
                    MappedLine::component(
                        Line::from(Span::raw(text.to_owned())),
                        Component::Item { index, field },
                        0,
                    ),
                    prefix,
                    width,
                    detail,
                    false,
                );
            }
        }
    }
    if value["matches"].as_array().is_some_and(|items| {
        items
            .iter()
            .any(|item| item["truncated"].as_bool() == Some(true))
    }) {
        note(
            rows,
            prefix,
            "… recorded match text is truncated; expansion cannot recover omitted text",
            width,
            detail.theme,
        );
    }
    if value["exhaustive"].as_bool() == Some(false) {
        note(
            rows,
            prefix,
            "… incomplete observation; an empty result is not proof of absence",
            width,
            detail.theme,
        );
    }
    if value["truncated"].as_bool() == Some(true) {
        note(
            rows,
            prefix,
            "… recorded output truncated; expansion cannot recover omitted data",
            width,
            detail.theme,
        );
    }
    if value["structured_content_omitted"].as_bool() == Some(true) {
        note(
            rows,
            prefix,
            "… structured content omitted from the recorded inline output",
            width,
            detail.theme,
        );
    }
    if let Some(diagnostics) = value
        .get("diagnostics")
        .filter(|value| value.as_array().is_some_and(|items| !items.is_empty()))
    {
        note(rows, prefix, "diagnostics", width, detail.theme);
        render_value(
            rows,
            diagnostics,
            Field::Diagnostics,
            prefix,
            width,
            detail,
            false,
        );
    }
    for key in ["artifact_error", "full_output_error", "raw_result_error"] {
        if let Some(error) = value[key].as_str() {
            note(
                rows,
                prefix,
                &format!("… {key}: {error}"),
                width,
                detail.theme,
            );
        }
    }
    if let Some(notice) = value
        .pointer("/output_schema_validation/notice")
        .and_then(Value::as_str)
    {
        note(rows, prefix, notice, width, detail.theme);
    }
}
