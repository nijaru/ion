//! Testing utilities for widgets.
//!
//! These helpers make it easy to snapshot-test widget output without running
//! a real terminal.

use crate::{
    buffer::Buffer,
    geometry::{Rect, Size},
    layout::compute_layout,
    widgets::Element,
};

/// Render an element tree into a buffer of the given size and return the
/// result as a `Vec` of plain strings (one per row, padded to `width`).
///
/// This is the primary assertion primitive for widget tests. The returned
/// strings use spaces for empty cells, making it trivial to compare output
/// with string literals.
pub fn render_to_lines(element: Element, width: u16, height: u16) -> Vec<String> {
    let size = Size { width, height };
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::new(area);
    let layout = compute_layout(&element, size);
    element.render(&layout, &mut buf);
    buf.to_lines()
}

/// Assert that a widget renders to the expected lines.
///
/// # Example
///
/// ```ignore
/// assert_render!(
///     Block::new().border(BorderType::Plain).into_element(),
///     10, 3,
///     vec!["┌────────┐", "│        │", "└────────┘"]
/// );
/// ```
#[macro_export]
macro_rules! assert_render {
    ($element:expr, $width:expr, $height:expr, $expected:expr) => {{
        let lines = $crate::widgets::testing::render_to_lines($element, $width, $height);
        assert_eq!(lines, $expected, "render mismatch");
    }};
}
