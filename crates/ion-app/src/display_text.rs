//! Control-safe terminal text. Composer byte/cursor mapping remains separate.
use ratatui::{
    style::Style,
    text::{Line, Span},
};
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
    wrap_line(|row| rows.push(row), prefix, continuation, line, width);
}

fn visible(grapheme: &str) -> &str {
    if grapheme == "\t" {
        "    "
    } else if grapheme.chars().any(char::is_control) {
        "�"
    } else {
        grapheme
    }
}

fn append(row: &mut Line<'static>, text: &str, style: Style) {
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
) {
    let width = width.max(1);
    let first_prefix = if UnicodeWidthStr::width(first_prefix) < width {
        first_prefix
    } else {
        ""
    };
    let continuation = if UnicodeWidthStr::width(continuation) < width {
        continuation
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
    let mut row = Line::default().style(source.style);
    append(&mut row, first_prefix, Style::default());
    let mut prefix = first_prefix;
    let mut col = UnicodeWidthStr::width(prefix);
    let mut style_index = 0;
    for (offset, grapheme) in text.grapheme_indices(true) {
        while styles
            .get(style_index)
            .is_some_and(|(end, _)| *end <= offset)
        {
            style_index += 1;
        }
        let style = styles
            .get(style_index)
            .map_or(Style::default(), |(_, style)| *style);
        if matches!(grapheme, "\n" | "\r\n") {
            emit(row);
            row = Line::default().style(source.style);
            append(&mut row, continuation, Style::default());
            prefix = continuation;
            col = UnicodeWidthStr::width(prefix);
            continue;
        }
        let display = visible(grapheme);
        let display = if UnicodeWidthStr::width(display) > available {
            "�"
        } else {
            display
        };
        let size = UnicodeWidthStr::width(display).max(1);
        if col + size > width && col > UnicodeWidthStr::width(prefix) {
            emit(row);
            row = Line::default().style(source.style);
            append(&mut row, continuation, Style::default());
            prefix = continuation;
            col = UnicodeWidthStr::width(prefix);
        }
        append(&mut row, display, style);
        col += size;
    }
    emit(row);
}

pub(super) fn fit_line(text: &str, width: usize) -> String {
    let width = width.max(1);
    let text = text
        .graphemes(true)
        .map(|grapheme| match grapheme {
            "\n" | "\r" | "\r\n" => " ",
            _ => visible(grapheme),
        })
        .collect::<String>();
    if text
        .graphemes(true)
        .try_fold(0usize, |used, grapheme| {
            used.checked_add(UnicodeWidthStr::width(grapheme).max(1))
                .filter(|&used| used <= width)
        })
        .is_some()
    {
        return text;
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

    #[test]
    fn chrome_and_transcript_use_control_safe_display_columns() {
        for (text, width, expected) in [
            ("abcdef", 4, "abc…"),
            ("界界", 4, "界界"),
            ("e\u{0301}x", 2, "e\u{0301}x"),
            ("a\u{1b}b", 3, "a�b"),
            ("\u{200b}\u{200b}\u{200b}", 2, "\u{200b}…"),
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
