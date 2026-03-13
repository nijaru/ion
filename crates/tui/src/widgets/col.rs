//! Col — a vertical flex container.

use crate::{
    layout::{Align, Direction, Justify, LayoutStyle},
    widgets::{Element, IntoElement, WidgetId, row::ContainerRenderer},
};

/// A vertical flex container that stacks its children top to bottom.
pub struct Col {
    children: Vec<Element>,
    gap: u16,
    align_items: Align,
    justify_content: Justify,
}

impl Col {
    pub fn new(children: Vec<Element>) -> Self {
        Self {
            children,
            gap: 0,
            align_items: Align::Stretch,
            justify_content: Justify::Start,
        }
    }

    /// Row gap between children, in cells.
    pub fn gap(mut self, gap: u16) -> Self {
        self.gap = gap;
        self
    }

    /// Cross-axis (horizontal) alignment.
    pub fn align(mut self, a: Align) -> Self {
        self.align_items = a;
        self
    }

    /// Main-axis (vertical) distribution.
    pub fn justify(mut self, j: Justify) -> Self {
        self.justify_content = j;
        self
    }
}

impl IntoElement for Col {
    fn into_element(self) -> Element {
        Element {
            id: WidgetId::new(),
            inner: Box::new(ContainerRenderer),
            layout_style: LayoutStyle {
                direction: Direction::Column,
                gap: (0, self.gap),
                align_items: Some(self.align_items),
                justify_content: Some(self.justify_content),
                ..LayoutStyle::default()
            },
            children: self.children,
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::{testing::render_to_lines, text::Text};

    #[test]
    fn col_two_equal_children() {
        let children = vec![
            Text::new("AB").into_element(),
            Text::new("CD").into_element(),
        ];
        let lines = render_to_lines(Col::new(children).into_element(), 4, 2);
        assert_eq!(lines[0].trim_end(), "AB");
        assert_eq!(lines[1].trim_end(), "CD");
    }

    #[test]
    fn col_gap() {
        // Two equal children in 4-tall col, gap=1.
        // Available = 4-1 = 3; Taffy distributes 2 to A and 1 to B.
        // A: rows 0-1, gap: row 2, B: row 3.
        let children = vec![Text::new("A").into_element(), Text::new("B").into_element()];
        let lines = render_to_lines(Col::new(children).gap(1).into_element(), 4, 4);
        assert_eq!(lines[0].trim_end(), "A");
        // rows 1-2 are blank (A's unused second row + gap)
        assert_eq!(lines[1].trim_end(), "");
        assert_eq!(lines[2].trim_end(), "");
        assert_eq!(lines[3].trim_end(), "B");
    }
}
