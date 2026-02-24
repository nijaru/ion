//! Block widget — a container with a configurable border and optional title.
//!
//! Border inset is implemented via Taffy padding so the child element is
//! automatically positioned in the inner area.

use crate::{
    buffer::Buffer,
    geometry::Rect,
    layout::{Direction, Edges, LayoutStyle},
    style::Style,
    widgets::{Element, IntoElement, Renderable, WidgetId, text::Span},
};

// ── Public types ──────────────────────────────────────────────────────────────

/// The visual style of a block's border.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BorderType {
    #[default]
    Rounded,
    Plain,
    Double,
    Thick,
    /// No border — block is invisible but still occupies space.
    None,
}

/// Where to render the block title.
#[derive(Debug, Clone, Copy, Default)]
pub enum TitlePosition {
    #[default]
    TopLeft,
    TopCenter,
    TopRight,
}

/// A container widget that draws a border around its child element.
pub struct Block {
    title: Option<Vec<Span>>,
    title_position: TitlePosition,
    border_type: BorderType,
    border_style: Style,
    style: Style,
    child: Option<Element>,
}

impl Default for Block {
    fn default() -> Self {
        Self::new()
    }
}

impl Block {
    pub fn new() -> Self {
        Self {
            title: None,
            title_position: TitlePosition::default(),
            border_type: BorderType::default(),
            border_style: Style::default(),
            style: Style::default(),
            child: None,
        }
    }

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(vec![Span::new(title)]);
        self
    }

    pub fn title_spans(mut self, spans: Vec<Span>) -> Self {
        self.title = Some(spans);
        self
    }

    pub fn title_position(mut self, pos: TitlePosition) -> Self {
        self.title_position = pos;
        self
    }

    pub fn border(mut self, border_type: BorderType) -> Self {
        self.border_type = border_type;
        self
    }

    pub fn border_style(mut self, style: Style) -> Self {
        self.border_style = style;
        self
    }

    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    pub fn child(mut self, child: Element) -> Self {
        self.child = Some(child);
        self
    }
}

impl IntoElement for Block {
    fn into_element(self) -> Element {
        let pad = if self.border_type != BorderType::None {
            1u16
        } else {
            0u16
        };
        let renderer = BlockRenderer {
            title: self.title,
            title_position: self.title_position,
            border_type: self.border_type,
            border_style: self.border_style,
            bg_style: self.style,
        };
        let children = self.child.map(|c| vec![c]).unwrap_or_default();
        Element {
            id: WidgetId::new(),
            inner: Box::new(renderer),
            layout_style: LayoutStyle {
                direction: Direction::Column,
                padding: Edges::all(pad),
                ..LayoutStyle::default()
            },
            children,
        }
    }
}

// ── Border character sets ─────────────────────────────────────────────────────

struct BorderChars {
    top_left: &'static str,
    top_right: &'static str,
    bottom_left: &'static str,
    bottom_right: &'static str,
    horizontal: &'static str,
    vertical: &'static str,
}

impl BorderType {
    fn chars(self) -> BorderChars {
        match self {
            BorderType::Plain => BorderChars {
                top_left: "┌",
                top_right: "┐",
                bottom_left: "└",
                bottom_right: "┘",
                horizontal: "─",
                vertical: "│",
            },
            BorderType::Rounded => BorderChars {
                top_left: "╭",
                top_right: "╮",
                bottom_left: "╰",
                bottom_right: "╯",
                horizontal: "─",
                vertical: "│",
            },
            BorderType::Double => BorderChars {
                top_left: "╔",
                top_right: "╗",
                bottom_left: "╚",
                bottom_right: "╝",
                horizontal: "═",
                vertical: "║",
            },
            BorderType::Thick => BorderChars {
                top_left: "┏",
                top_right: "┓",
                bottom_left: "┗",
                bottom_right: "┛",
                horizontal: "━",
                vertical: "┃",
            },
            BorderType::None => BorderChars {
                top_left: "",
                top_right: "",
                bottom_left: "",
                bottom_right: "",
                horizontal: "",
                vertical: "",
            },
        }
    }
}

// ── Internal renderer ─────────────────────────────────────────────────────────

struct BlockRenderer {
    title: Option<Vec<Span>>,
    title_position: TitlePosition,
    border_type: BorderType,
    border_style: Style,
    bg_style: Style,
}

impl Renderable for BlockRenderer {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }

        // Fill background.
        if self.bg_style != Style::default() {
            let fill_cell = crate::buffer::Cell {
                symbol: " ".to_string(),
                style: self.bg_style,
                width: 1,
                skip: false,
            };
            buf.fill_region(area, &fill_cell);
        }

        if self.border_type == BorderType::None {
            return;
        }

        let bc = self.border_type.chars();
        let s = self.border_style;
        let w = area.width;
        let h = area.height;
        let x = area.x;
        let y = area.y;

        if w < 2 || h < 2 {
            return;
        }

        // Top border.
        buf.set_string(x, y, bc.top_left, s);
        for col in 1..w - 1 {
            buf.set_string(x + col, y, bc.horizontal, s);
        }
        buf.set_string(x + w - 1, y, bc.top_right, s);

        // Left and right borders.
        for row in 1..h - 1 {
            buf.set_string(x, y + row, bc.vertical, s);
            buf.set_string(x + w - 1, y + row, bc.vertical, s);
        }

        // Bottom border.
        buf.set_string(x, y + h - 1, bc.bottom_left, s);
        for col in 1..w - 1 {
            buf.set_string(x + col, y + h - 1, bc.horizontal, s);
        }
        buf.set_string(x + w - 1, y + h - 1, bc.bottom_right, s);

        // Title.
        if let Some(spans) = &self.title {
            let title: String = spans.iter().map(|s| s.content.as_str()).collect();
            if title.is_empty() || w < 4 {
                return;
            }
            // Available space: width minus two corners minus two spaces for padding.
            let max_title_width = w.saturating_sub(4) as usize;
            let display: String = title.chars().take(max_title_width).collect();
            let display = format!(" {display} ");

            let title_x = match self.title_position {
                TitlePosition::TopLeft => x + 1,
                TitlePosition::TopCenter => {
                    let tw = display.chars().count() as u16;
                    x + (w / 2).saturating_sub(tw / 2)
                }
                TitlePosition::TopRight => {
                    let tw = display.chars().count() as u16;
                    x + w.saturating_sub(tw + 1)
                }
            };
            // Render title spans over the top border line.
            let mut cx = title_x;
            buf.set_string(cx, y, " ", s);
            cx += 1;
            for span in spans {
                let max_w = (x + w - 1).saturating_sub(cx);
                if max_w == 0 {
                    break;
                }
                let ts = self.border_style.patch(span.style);
                cx = buf.set_string_truncated(cx, y, &span.content, max_w, ts);
            }
            buf.set_string(cx.min(x + w - 2), y, " ", s);
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::testing::render_to_lines;

    #[test]
    fn block_plain_border() {
        let lines = render_to_lines(Block::new().border(BorderType::Plain).into_element(), 10, 3);
        assert_eq!(&lines[0], "┌────────┐");
        assert_eq!(&lines[1], "│        │");
        assert_eq!(&lines[2], "└────────┘");
    }

    #[test]
    fn block_rounded_border() {
        let lines = render_to_lines(
            Block::new().border(BorderType::Rounded).into_element(),
            6,
            3,
        );
        assert_eq!(&lines[0], "╭────╮");
        assert_eq!(&lines[1], "│    │");
        assert_eq!(&lines[2], "╰────╯");
    }

    #[test]
    fn block_double_border() {
        let lines = render_to_lines(Block::new().border(BorderType::Double).into_element(), 6, 3);
        assert_eq!(&lines[0], "╔════╗");
        assert_eq!(&lines[1], "║    ║");
        assert_eq!(&lines[2], "╚════╝");
    }

    #[test]
    fn block_thick_border() {
        let lines = render_to_lines(Block::new().border(BorderType::Thick).into_element(), 6, 3);
        assert_eq!(&lines[0], "┏━━━━┓");
        assert_eq!(&lines[1], "┃    ┃");
        assert_eq!(&lines[2], "┗━━━━┛");
    }

    #[test]
    fn block_no_border() {
        let lines = render_to_lines(Block::new().border(BorderType::None).into_element(), 4, 2);
        assert_eq!(&lines[0], "    ");
        assert_eq!(&lines[1], "    ");
    }
}
