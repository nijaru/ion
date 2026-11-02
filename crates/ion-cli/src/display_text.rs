//! Shared control-safe grapheme layout for literal text, prose and editor maps.
use ratatui::{
    style::Style,
    text::{Line, Span},
};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(super) fn push_prefixed(
    rows: &mut Vec<String>,
    prefix: &str,
    continuation: &str,
    text: &str,
    width: usize,
) {
    wrap_line(
        |mut row| {
            // Plain input and prefixes share one default-style run; move its text.
            rows.push(
                row.spans
                    .pop()
                    .map_or_else(String::new, |span| span.content.into_owned()),
            );
        },
        prefix,
        continuation,
        Line::from(Span::raw(text)),
        width,
        Wrapping::Literal,
    );
}

pub(super) fn push_wrapped(rows: &mut Vec<String>, text: &str, width: usize) {
    push_prefixed(rows, "", "", text, width);
}

pub(super) fn push_styled(
    rows: &mut Vec<Line<'static>>,
    prefix: &str,
    continuation: &str,
    line: Line<'_>,
    width: usize,
) {
    wrap_line(
        |row| rows.push(row),
        prefix,
        continuation,
        line,
        width,
        Wrapping::Literal,
    );
}

/// Parser components are named by their original syntax boundary and role;
/// fields are named inside an existing Core message/activity/shell root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ParsedComponent {
    Text,
    Html,
    Code,
    LinkDestination,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Field {
    Command,
    Stdout,
    Stderr,
    Content,
    Error,
    Diff,
    Summary,
    Detail,
}

impl Field {
    pub fn label(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
            Self::Content => "content",
            Self::Error => "error",
            Self::Diff => "diff",
            Self::Summary => "summary",
            Self::Detail => "detail",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Component {
    Parsed { node: usize, kind: ParsedComponent },
    Field(Field),
    Readiness,
}

/// Untouched text bytes, component-local bytes before fitting, or a generated
/// boundary with no byte claim. Never coordinates in a fitted row or JSON dump.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Origin {
    Raw(Range<usize>),
    Component {
        component: Component,
        bytes: Range<usize>,
    },
    Node(usize),
}

impl Origin {
    fn slice(&self, relative: Range<usize>) -> Self {
        match self {
            Self::Raw(raw) => Self::Raw(raw.start + relative.start..raw.start + relative.end),
            Self::Component { component, bytes } => Self::Component {
                component: *component,
                bytes: bytes.start + relative.start..bytes.start + relative.end,
            },
            Self::Node(node) => Self::Node(*node),
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct Mapping {
    pub display: Range<usize>,
    pub origin: Origin,
    /// False for generated boundaries or a display replacement spanning a unit.
    pub linear: bool,
}

impl Mapping {
    fn project(&self, source: &Range<usize>, display: Range<usize>) -> Option<Self> {
        let start = self.display.start.max(source.start);
        let end = self.display.end.min(source.end);
        if start >= end {
            return None;
        }
        let origin = if self.linear {
            self.origin
                .slice(start - self.display.start..end - self.display.start)
        } else {
            self.origin.clone()
        };
        Some(Self {
            linear: self.linear && end - start == display.len(),
            display,
            origin,
        })
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct MappedLine {
    pub line: Line<'static>,
    pub map: Vec<Mapping>,
}

impl MappedLine {
    pub fn raw(line: Line<'static>, source: Range<usize>) -> Self {
        let len = line.spans.iter().map(|span| span.content.len()).sum();
        assert_eq!(source.len(), len, "verbatim source extent");
        Self {
            line,
            map: vec![Mapping {
                display: 0..len,
                origin: Origin::Raw(source),
                linear: true,
            }],
        }
    }

    pub fn generated(line: Line<'static>, node: usize) -> Self {
        let len = line.spans.iter().map(|span| span.content.len()).sum();
        Self {
            line,
            map: vec![Mapping {
                display: 0..len,
                origin: Origin::Node(node),
                linear: false,
            }],
        }
    }

    pub fn component(line: Line<'static>, component: Component, start: usize) -> Self {
        let len: usize = line.spans.iter().map(|span| span.content.len()).sum();
        Self {
            line,
            map: vec![Mapping {
                display: 0..len,
                origin: Origin::Component {
                    component,
                    bytes: start..start + len,
                },
                linear: true,
            }],
        }
    }

    pub fn append(&mut self, other: &Self) {
        let offset: usize = self.line.spans.iter().map(|span| span.content.len()).sum();
        self.line
            .spans
            .extend(other.line.spans.iter().cloned().map(|mut span| {
                span.style = other.line.style.patch(span.style);
                span
            }));
        self.map
            .extend(other.map.iter().cloned().map(|mut mapping| {
                mapping.display.start += offset;
                mapping.display.end += offset;
                mapping
            }));
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Wrapping {
    Literal,
    /// Prefer word boundaries without trimming or rewriting source whitespace.
    Words,
}

pub(super) struct DisplayUnit<'a> {
    pub source: Range<usize>,
    pub text: &'a str,
    pub cells: usize,
    whitespace: bool,
}

pub(super) struct TextRow<'a> {
    pub units: Vec<DisplayUnit<'a>>,
    pub end: usize,
    pub soft_wrap: bool,
}

/// The sole wrapping kernel. Tabs expand to four spaces, even across narrow rows;
/// oversized glyphs and standalone invisible/control units use a safe placeholder.
/// Byte ranges always refer to the untouched source.
pub(super) fn layout(text: &str, width: usize, policy: Wrapping) -> Vec<TextRow<'_>> {
    let width = width.max(1);
    let mut units = Vec::new();
    for (byte, grapheme) in text.grapheme_indices(true) {
        let hard_break = matches!(grapheme, "\n" | "\r\n");
        let display = if hard_break { "" } else { visible(grapheme) };
        let display = if UnicodeWidthStr::width(display) > width && grapheme != "\t" {
            "�"
        } else {
            display
        };
        for _ in 0..if grapheme == "\t" { 4 } else { 1 } {
            let display = if grapheme == "\t" { " " } else { display };
            units.push(DisplayUnit {
                source: byte..byte + grapheme.len(),
                text: display,
                cells: UnicodeWidthStr::width(display),
                whitespace: grapheme.chars().all(char::is_whitespace),
            });
        }
    }
    let mut rows = Vec::new();
    let mut row = TextRow {
        units: Vec::new(),
        end: 0,
        soft_wrap: false,
    };
    let mut col = 0;
    let mut word_start = true;
    let mut units = std::collections::VecDeque::from(units);
    while let Some(unit) = units.pop_front() {
        if unit.text.is_empty() {
            row.end = unit.source.start;
            rows.push(row);
            row = TextRow {
                units: Vec::new(),
                end: unit.source.end,
                soft_wrap: false,
            };
            col = 0;
            word_start = true;
            continue;
        }
        let word_cells = if policy == Wrapping::Words && word_start && !unit.whitespace {
            unit.cells
                + units
                    .iter()
                    .take_while(|next| !next.whitespace)
                    .map(|next| next.cells)
                    .take(width)
                    .sum::<usize>()
        } else {
            0
        };
        if col > 0
            && (col + unit.cells > width || (word_cells <= width && col + word_cells > width))
        {
            row.end = unit.source.start;
            row.soft_wrap = true;
            rows.push(row);
            row = TextRow {
                units: Vec::new(),
                end: unit.source.start,
                soft_wrap: false,
            };
            col = 0;
        }
        col += unit.cells;
        word_start = unit.whitespace;
        row.end = unit.source.end;
        row.units.push(unit);
    }
    rows.push(row);
    rows
}

fn visible(grapheme: &str) -> &str {
    if grapheme == "\t" {
        "    "
    } else if grapheme.chars().any(char::is_control) || UnicodeWidthStr::width(grapheme) == 0 {
        // Wrapping budgets every standalone grapheme at least one column.
        // Make otherwise invisible units occupy that column on screen too.
        "�"
    } else {
        grapheme
    }
}

fn append(row: &mut Line<'static>, text: &str, style: Style) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = row.spans.last_mut().filter(|span| span.style == style) {
        last.content.to_mut().push_str(text);
    } else {
        row.spans.push(Span::styled(text.to_owned(), style));
    }
}

/// Both plain and styled consumers share the same column/control policy.
/// Segment the whole text, not individual spans: markup can split a grapheme.
fn wrap_line(
    mut emit: impl FnMut(Line<'static>),
    first_prefix: &str,
    continuation: &str,
    source: Line<'_>,
    width: usize,
    policy: Wrapping,
) {
    wrap_rows(
        |row| emit(row.line),
        first_prefix,
        continuation,
        source,
        width,
        policy,
        None,
    );
}

/// Carry the writer's source runs through the same grapheme decisions as every
/// other text consumer. No fitted-text lookup participates in provenance.
pub(super) fn wrap_mapped(
    first_prefix: &str,
    continuation: &str,
    source: MappedLine,
    width: usize,
    policy: Wrapping,
) -> Vec<MappedLine> {
    let mut rows = Vec::new();
    wrap_rows(
        |row| rows.push(row),
        first_prefix,
        continuation,
        source.line,
        width,
        policy,
        Some(&source.map),
    );
    rows
}

fn wrap_rows(
    mut emit: impl FnMut(MappedLine),
    first_prefix: &str,
    continuation: &str,
    source: Line<'_>,
    width: usize,
    policy: Wrapping,
    source_map: Option<&[Mapping]>,
) {
    let mappings = source_map.unwrap_or_default();
    let width = width.max(1);
    let first_prefix = diagnostic(first_prefix).replace('\n', "�");
    let continuation = diagnostic(continuation).replace('\n', "�");
    let first_prefix = if UnicodeWidthStr::width(first_prefix.as_str()) < width {
        first_prefix.as_str()
    } else {
        ""
    };
    let continuation = if UnicodeWidthStr::width(continuation.as_str()) < width {
        continuation.as_str()
    } else {
        ""
    };
    let available =
        width - UnicodeWidthStr::width(first_prefix).max(UnicodeWidthStr::width(continuation));
    let mut text = String::new();
    let mut styles = Vec::new();
    for span in source.spans {
        text.push_str(&span.content);
        styles.push((text.len(), span.style));
    }
    let mut style_index = 0;
    let mut map_index = 0;
    assert!(
        mappings
            .iter()
            .all(|mapping| mapping.display.start <= mapping.display.end
                && mapping.display.end <= text.len()),
        "source runs fit the writer's text"
    );
    for (index, content) in layout(&text, available, policy).into_iter().enumerate() {
        let mut row = Line::default().style(source.style);
        let mut map = Vec::new();
        append(
            &mut row,
            if index == 0 {
                first_prefix
            } else {
                continuation
            },
            Style::default(),
        );
        let mut display_offset: usize = row.spans.iter().map(|span| span.content.len()).sum();
        for unit in content.units {
            while styles
                .get(style_index)
                .is_some_and(|(end, _)| *end <= unit.source.start)
            {
                style_index += 1;
            }
            let style = styles
                .get(style_index)
                .map_or(Style::default(), |(_, style)| *style);
            while mappings
                .get(map_index)
                .is_some_and(|mapping| mapping.display.end <= unit.source.start)
            {
                map_index += 1;
            }
            for mapping in mappings[map_index..]
                .iter()
                .take_while(|mapping| mapping.display.start < unit.source.end)
            {
                if let Some(mapping) = mapping.project(
                    &unit.source,
                    display_offset..display_offset + unit.text.len(),
                ) {
                    map.push(mapping);
                }
            }
            append(&mut row, unit.text, style);
            display_offset += unit.text.len();
        }
        if map.is_empty() {
            // Empty logical lines still have an actual source boundary.
            let point = content.end;
            if let Some(mapping) = mappings
                .iter()
                .find(|mapping| mapping.display.start <= point && point <= mapping.display.end)
            {
                let origin = if mapping.linear {
                    let byte = point - mapping.display.start;
                    mapping.origin.slice(byte..byte)
                } else {
                    mapping.origin.clone()
                };
                map.push(Mapping {
                    display: 0..0,
                    origin,
                    linear: false,
                });
            }
        }
        assert!(
            source_map.is_none() || !map.is_empty(),
            "every mapped text row has writer provenance"
        );
        emit(MappedLine { line: row, map });
    }
}

/// Human diagnostics keep their logical lines without allowing quoted metadata
/// to issue terminal controls. Use the same grapheme policy as transcript text.
pub(super) fn diagnostic(text: &str) -> String {
    text.graphemes(true)
        .map(|grapheme| match grapheme {
            "\n" | "\r\n" => "\n",
            _ => visible(grapheme),
        })
        .collect()
}

pub(super) fn fit_line(text: &str, width: usize) -> String {
    fit_line_with_status(text, width).0
}

/// The flag describes omitted source, not control replacement or a literal ellipsis.
pub(super) fn fit_line_with_status(text: &str, width: usize) -> (String, bool) {
    let (units, omitted) = fit_units(text, width);
    let mut text = units.iter().map(|unit| unit.text).collect::<String>();
    if omitted.is_some() {
        text.push('…');
    }
    (text, omitted.is_some())
}

fn fit_units(text: &str, width: usize) -> (Vec<DisplayUnit<'_>>, Option<usize>) {
    let width = width.max(1);
    let mut cells = 0;
    let mut units = Vec::new();
    'source: for (byte, grapheme) in text.grapheme_indices(true) {
        let value = match grapheme {
            "\n" | "\r" | "\r\n" => " ",
            _ => visible(grapheme),
        };
        for _ in 0..if grapheme == "\t" { 4 } else { 1 } {
            let value = if grapheme == "\t" { " " } else { value };
            units.push(DisplayUnit {
                source: byte..byte + grapheme.len(),
                text: value,
                cells: UnicodeWidthStr::width(value).max(1),
                whitespace: false,
            });
            cells += UnicodeWidthStr::width(value).max(1);
            if cells > width {
                break 'source;
            }
        }
    }
    if cells <= width {
        return (units, None);
    }
    let mut used = 0;
    let keep = units
        .iter()
        .take_while(|unit| {
            used += unit.cells;
            used < width
        })
        .count();
    let omitted = units[keep].source.start;
    units.truncate(keep);
    (units, Some(omitted))
}

/// Clip at the same writer decisions as chrome, retaining field/parser offsets
/// through replacement characters and the generated omission marker.
pub(super) fn fit_mapped(source: MappedLine, width: usize) -> (MappedLine, bool) {
    let text = source
        .line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    let (units, omitted) = fit_units(&text, width);
    let mut row = MappedLine {
        line: Line::default().style(source.line.style),
        map: Vec::new(),
    };
    let mut offset = 0;
    let mut span_end = 0;
    let mut styles = source
        .line
        .spans
        .iter()
        .map(|span| {
            span_end += span.content.len();
            (span_end, span.style)
        })
        .peekable();
    let mut style = Style::default();
    for unit in units {
        while styles
            .peek()
            .is_some_and(|(end, _)| *end <= unit.source.start)
        {
            styles.next();
        }
        style = styles.peek().map_or(Style::default(), |(_, style)| *style);
        row.map.extend(
            source.map.iter().filter_map(|mapping| {
                mapping.project(&unit.source, offset..offset + unit.text.len())
            }),
        );
        append(&mut row.line, unit.text, style);
        offset += unit.text.len();
    }
    if let Some(byte) = omitted {
        let mapping = source
            .map
            .iter()
            .find(|mapping| mapping.display.contains(&byte))
            .expect("omitted byte has writer provenance");
        let origin = if mapping.linear {
            mapping
                .origin
                .slice(byte - mapping.display.start..byte - mapping.display.start)
        } else {
            mapping.origin.clone()
        };
        row.map.push(Mapping {
            display: offset..offset + '…'.len_utf8(),
            origin,
            linear: false,
        });
        append(
            &mut row.line,
            "…",
            styles.peek().map_or(style, |(_, style)| *style),
        );
    }
    if text.is_empty() {
        row.map = source.map;
    }
    assert!(!row.map.is_empty(), "fitted text has writer provenance");
    (row, omitted.is_some())
}

/// Logical line slices retain offsets in the untouched field, including skipped
/// preview lines and CRLF terminators. Matches `str::lines`' presentation policy.
pub(super) fn source_lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut byte = 0;
    text.split_inclusive('\n').map(move |segment| {
        let start = byte;
        byte += segment.len();
        let line = segment.strip_suffix('\n').unwrap_or(segment);
        (start, line.strip_suffix('\r').unwrap_or(line))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prose_prefers_words_while_literal_keeps_technical_row_boundaries() {
        let mut prose = Vec::new();
        let mut literal = Vec::new();
        wrap_line(
            |row| prose.push(row),
            "",
            "",
            Line::from(Span::raw("alpha beta\n\n123456789")),
            8,
            Wrapping::Words,
        );
        push_styled(
            &mut literal,
            "",
            "",
            Line::from(Span::raw("alpha beta\n\n123456789")),
            8,
        );
        assert_eq!(
            prose.iter().map(ToString::to_string).collect::<Vec<_>>(),
            ["alpha ", "beta", "", "12345678", "9"]
        );
        assert_eq!(
            literal.iter().map(ToString::to_string).collect::<Vec<_>>(),
            ["alpha be", "ta", "", "12345678", "9"]
        );
    }

    #[test]
    fn both_policies_preserve_whitespace_and_cross_span_graphemes_safely() {
        use ratatui::style::Modifier;
        let bold = Style::default().add_modifier(Modifier::BOLD);
        for policy in [Wrapping::Literal, Wrapping::Words] {
            for width in 1..=12 {
                let mut rows = Vec::new();
                wrap_line(
                    |row| rows.push(row),
                    "",
                    "",
                    Line::from(vec![
                        Span::styled("e", bold),
                        Span::raw("\u{301} 界\t\u{1b}abcdef  "),
                    ]),
                    width,
                    policy,
                );
                assert!(rows.iter().all(|row| row.width() <= width));
                let joined = rows.iter().map(ToString::to_string).collect::<String>();
                assert_eq!(
                    joined,
                    if width == 1 {
                        "e\u{301} �    �abcdef  "
                    } else {
                        "e\u{301} 界    �abcdef  "
                    }
                );
                assert!(!joined.chars().any(char::is_control));
                assert_eq!(rows[0].spans[0].content, "e\u{301}");
                assert_eq!(rows[0].spans[0].style, bold);
            }
        }
    }

    #[test]
    fn clipped_fields_keep_original_offsets_through_safe_replacements() {
        let component = Component::Field(Field::Stdout);
        for (text, width, expected, omitted) in [
            ("a\u{1b}b", 3, "a�b", false),
            ("界x", 2, "…", true),
            ("ab\tcd", 5, "ab  …", true),
        ] {
            let (row, clipped) = fit_mapped(
                MappedLine::component(Line::from(Span::raw(text.to_owned())), component, 100),
                width,
            );
            assert_eq!(row.line.to_string(), expected);
            assert_eq!(clipped, omitted);
            assert!(row.line.width() <= width);
            assert!(row.map.iter().all(|mapping| matches!(&mapping.origin, Origin::Component { component: actual, bytes } if *actual == component && bytes.start >= 100 && bytes.end <= 100 + text.len())));
            if text.contains('\u{1b}') {
                assert!(row.map.iter().any(|mapping| mapping.origin
                    == Origin::Component {
                        component,
                        bytes: 101..102
                    }
                    && &row.line.to_string()[mapping.display.clone()] == "�"));
            }
        }
        assert_eq!(
            source_lines("a\r\nb\n").collect::<Vec<_>>(),
            [(0, "a"), (3, "b")]
        );
    }

    #[test]
    fn diagnostics_preserve_lines_without_executing_quoted_controls() {
        assert_eq!(
            diagnostic("\u{1b}]52;c;QUJD\u{7}\r\t🦀\r\nmore"),
            "�]52;c;QUJD��    🦀\nmore"
        );
    }

    #[test]
    fn chrome_and_transcript_use_control_safe_display_columns() {
        for (text, width, expected) in [
            ("abcdef", 4, "abc…"),
            ("界界", 4, "界界"),
            ("e\u{0301}x", 2, "e\u{0301}x"),
            ("a\u{1b}b", 3, "a�b"),
            ("\u{200b}\u{200b}\u{200b}", 2, "�…"),
        ] {
            let fitted = fit_line(text, width);
            assert_eq!(fitted, expected);
            assert!(UnicodeWidthStr::width(fitted.as_str()) <= width);
        }
        let mut wrapped = Vec::new();
        push_wrapped(&mut wrapped, "a\r\nb\r", 20);
        assert_eq!(wrapped, ["a", "b�"]);
        wrapped.clear();
        push_prefixed(&mut wrapped, "› ", "  ", "界\tx", 1);
        assert!(
            wrapped
                .iter()
                .all(|row| UnicodeWidthStr::width(row.as_str()) <= 1)
        );
    }
}
