//! Row — a horizontal flex container.

use crate::{
    buffer::Buffer,
    geometry::Rect,
    layout::{Align, Direction, Justify, LayoutStyle},
    widgets::{Element, IntoElement, Renderable, WidgetId},
};

/// A horizontal flex container that lays its children side by side.
pub struct Row {
    children: Vec<Element>,
    gap: u16,
    align_items: Align,
    justify_content: Justify,
}

impl Row {
    pub fn new(children: Vec<Element>) -> Self {
        Self {
            children,
            gap: 0,
            align_items: Align::Stretch,
            justify_content: Justify::Start,
        }
    }

    /// Column gap between children, in cells.
    pub fn gap(mut self, gap: u16) -> Self {
        self.gap = gap;
        self
    }

    /// Cross-axis (vertical) alignment.
    pub fn align(mut self, a: Align) -> Self {
        self.align_items = a;
        self
    }

    /// Main-axis (horizontal) distribution.
    pub fn justify(mut self, j: Justify) -> Self {
        self.justify_content = j;
        self
    }
}

impl IntoElement for Row {
    fn into_element(self) -> Element {
        Element {
            id: WidgetId::new(),
            inner: Box::new(ContainerRenderer),
            layout_style: LayoutStyle {
                direction: Direction::Row,
                gap: (self.gap, 0),
                align_items: Some(self.align_items),
                justify_content: Some(self.justify_content),
                ..LayoutStyle::default()
            },
            children: self.children,
        }
    }
}

// ── Internal ──────────────────────────────────────────────────────────────────

pub(crate) struct ContainerRenderer;

impl Renderable for ContainerRenderer {
    fn render(&self, _area: Rect, _buf: &mut Buffer) {
        // Container has no visual of its own; children are rendered separately.
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::{testing::render_to_lines, text::Text};

    #[test]
    fn row_two_equal_children() {
        let children = vec![
            Text::new("AB").into_element(),
            Text::new("CD").into_element(),
        ];
        let lines = render_to_lines(Row::new(children).into_element(), 4, 1);
        assert_eq!(&lines[0], "ABCD");
    }

    #[test]
    fn row_gap() {
        let children = vec![Text::new("A").into_element(), Text::new("B").into_element()];
        let lines = render_to_lines(Row::new(children).gap(2).into_element(), 6, 1);
        // Each child gets (6-2)/2=2 cells; "A" in 2-cell box + 2-cell gap + "B" in 2-cell box.
        assert_eq!(lines[0].trim_end(), "A   B");
    }
}
