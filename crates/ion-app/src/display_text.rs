//! Control-safe terminal text. Composer byte/cursor mapping remains separate.
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(super) fn push_prefixed(
    rows: &mut Vec<String>,
    prefix: &str,
    continuation: &str,
    text: &str,
    width: usize,
) {
    let mut first = true;
    let text = text.replace("\r\n", "\n");
    for logical in text.split('\n') {
        let line_prefix = if first { prefix } else { continuation };
        wrap_one(rows, line_prefix, continuation, logical, width);
        first = false;
    }
}

pub(super) fn push_wrapped(rows: &mut Vec<String>, text: &str, width: usize) {
    let text = text.replace("\r\n", "\n");
    let width = width.max(1);
    for logical in text.split('\n') {
        wrap_one(rows, "", "", logical, width);
    }
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

fn wrap_one(
    rows: &mut Vec<String>,
    first_prefix: &str,
    continuation: &str,
    text: &str,
    width: usize,
) {
    let width = width.max(1);
    // At least one content column; a prefix cannot make a tiny row overflow.
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
    let mut line = first_prefix.to_owned();
    let mut col = UnicodeWidthStr::width(first_prefix);
    let mut prefix = first_prefix;
    for grapheme in text.graphemes(true) {
        let display = visible(grapheme);
        let display = if UnicodeWidthStr::width(display)
            > width - UnicodeWidthStr::width(first_prefix).max(UnicodeWidthStr::width(continuation))
        {
            "�"
        } else {
            display
        };
        let size = UnicodeWidthStr::width(display).max(1);
        if col + size > width && col > UnicodeWidthStr::width(prefix) {
            rows.push(std::mem::take(&mut line));
            line.push_str(continuation);
            prefix = continuation;
            col = UnicodeWidthStr::width(continuation);
        }
        line.push_str(display);
        col += size;
    }
    rows.push(line);
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
    if UnicodeWidthStr::width(text.as_str()) <= width {
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
