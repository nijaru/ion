//! Shared RNK text-line rendering helpers.

use rnk::components::{Box as RnkBox, Text};
use rnk::core::{FlexDirection, TextWrap};

/// Render a single text line using RNK.
///
/// The caller controls wrapping policy. The returned string contains ANSI
/// escapes and no trailing newline.
pub(crate) fn render_text_line(text: Text, max_cells: usize, wrap: TextWrap) -> String {
    if max_cells == 0 {
        return String::new();
    }

    let width = max_cells.min(u16::MAX as usize) as u16;
    let element = RnkBox::new()
        .flex_direction(FlexDirection::Row)
        .width(width)
        .child(text.wrap(wrap).into_element())
        .into_element();
    let rendered = rnk::render_to_string(&element, width);
    rendered.lines().next().unwrap_or_default().to_string()
}

/// Render a single line with truncation semantics.
pub(crate) fn render_truncated_text_line(text: Text, max_cells: usize) -> String {
    render_text_line(text, max_cells, TextWrap::Truncate)
}

/// Render a single line without wrapping.
///
/// RNK does not expose a dedicated "NoWrap" mode; using truncate with an
/// explicit width preserves single-line behavior.
pub(crate) fn render_no_wrap_text_line(text: Text, max_cells: usize) -> String {
    render_text_line(text, max_cells, TextWrap::Truncate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_text_line_does_not_pad_to_container_width() {
        let rendered = render_no_wrap_text_line(Text::new("abc"), 20);
        assert_eq!(rendered, "abc");
    }
}
