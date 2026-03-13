//! Scroll widget — a viewport that clips and offsets a child element.
//!
//! The child is rendered into a scratch buffer whose height equals
//! `state.content_length`. The visible slice (rows `offset..offset+viewport_h`)
//! is then merged into the frame buffer. An optional scrollbar is drawn in the
//! rightmost column.

use crate::{
    buffer::Buffer,
    geometry::{Rect, Size},
    layout::{LayoutStyle, compute_layout},
    style::Style,
    widgets::{Element, IntoElement, Renderable, WidgetId},
};

// ── ScrollDirection ───────────────────────────────────────────────────────────

/// Which axis the Scroll viewport clips.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ScrollDirection {
    #[default]
    Vertical,
    Horizontal,
    Both,
}

// ── ScrollState ───────────────────────────────────────────────────────────────

/// External state for a [`Scroll`] viewport.
///
/// `content_length` and `viewport_length` must be set by the caller to enable
/// bounds-checked scrolling. The offset is clamped on every mutation so it
/// never exceeds `content_length - viewport_length`.
#[derive(Debug, Clone, Default)]
pub struct ScrollState {
    /// Current scroll offset in rows (vertical) or columns (horizontal).
    pub offset: u16,
    /// Total size of the content — rows for vertical, columns for horizontal.
    pub content_length: u16,
    /// Size of the visible viewport — updated by the caller after each render.
    pub viewport_length: u16,
}

impl ScrollState {
    pub fn new() -> Self {
        Self::default()
    }

    fn max_offset(&self) -> u16 {
        self.content_length.saturating_sub(self.viewport_length)
    }

    pub fn scroll_up(&mut self, delta: u16) {
        self.offset = self.offset.saturating_sub(delta);
    }

    pub fn scroll_down(&mut self, delta: u16) {
        self.offset = self.offset.saturating_add(delta).min(self.max_offset());
    }

    pub fn scroll_to_top(&mut self) {
        self.offset = 0;
    }

    pub fn scroll_to_bottom(&mut self) {
        self.offset = self.max_offset();
    }

    /// Scroll down by one full viewport length.
    pub fn scroll_by_page(&mut self, viewport: u16) {
        self.scroll_down(viewport);
    }

    /// True when the viewport is scrolled to the very end.
    pub fn at_bottom(&self) -> bool {
        self.offset >= self.max_offset()
    }
}

// ── Scroll widget ─────────────────────────────────────────────────────────────

/// A viewport that scrolls over an arbitrary child [`Element`].
///
/// The child is rendered independently of the main Taffy layout pass into a
/// scratch buffer, and the visible slice is copied into the frame buffer.
pub struct Scroll {
    child: Element,
    state: ScrollState,
    direction: ScrollDirection,
    bar: bool,
}

impl Scroll {
    pub fn new(child: Element) -> Self {
        Self {
            child,
            state: ScrollState::default(),
            direction: ScrollDirection::default(),
            bar: false,
        }
    }

    /// Attach external scroll state.
    pub fn state(mut self, state: &ScrollState) -> Self {
        self.state = state.clone();
        self
    }

    pub fn direction(mut self, direction: ScrollDirection) -> Self {
        self.direction = direction;
        self
    }

    /// Show a scrollbar indicator (1 column / row on the trailing edge).
    pub fn bar(mut self, show: bool) -> Self {
        self.bar = show;
        self
    }
}

impl IntoElement for Scroll {
    fn into_element(self) -> Element {
        let renderer = ScrollRenderable {
            child: self.child,
            state: self.state,
            direction: self.direction,
            show_bar: self.bar,
        };
        Element {
            id: WidgetId::new(),
            inner: Box::new(renderer),
            layout_style: LayoutStyle::default(),
            // Child is managed by the renderer, not by Taffy.
            children: vec![],
        }
    }
}

// ── Internal renderer ─────────────────────────────────────────────────────────

struct ScrollRenderable {
    child: Element,
    state: ScrollState,
    direction: ScrollDirection,
    show_bar: bool,
}

impl Renderable for ScrollRenderable {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }

        let bar_size: u16 = if self.show_bar { 1 } else { 0 };

        let (viewport_w, viewport_h) = match self.direction {
            ScrollDirection::Vertical => (area.width.saturating_sub(bar_size), area.height),
            ScrollDirection::Horizontal => (area.width, area.height.saturating_sub(bar_size)),
            ScrollDirection::Both => (
                area.width.saturating_sub(bar_size),
                area.height.saturating_sub(bar_size),
            ),
        };

        if viewport_w == 0 || viewport_h == 0 {
            return;
        }

        // Content size: use content_length if set, otherwise fall back to
        // the viewport size (effectively no scrolling).
        let content_h = if self.state.content_length > 0 {
            self.state.content_length
        } else {
            viewport_h
        };
        let content_w = viewport_w;

        // Render child into a scratch buffer sized to the full content.
        let scratch_area = Rect::new(0, 0, content_w, content_h);
        let mut scratch = Buffer::new(scratch_area);
        let child_size = Size {
            width: content_w,
            height: content_h,
        };
        let layout = compute_layout(&self.child, child_size);
        self.child.render(&layout, &mut scratch);

        // Copy the visible rows/cols from the scratch buffer to the frame.
        let offset = self.state.offset;
        let copy_rows = viewport_h.min(content_h.saturating_sub(offset));

        for row in 0..copy_rows {
            for col in 0..viewport_w {
                let src_row = offset + row;
                if src_row >= content_h {
                    break;
                }
                let cell = scratch.get(col, src_row).clone();
                *buf.get_mut(area.x + col, area.y + row) = cell;
            }
        }

        // Scrollbar.
        if self.show_bar && self.state.content_length > self.state.viewport_length {
            draw_vscrollbar(
                buf,
                area.x + viewport_w,
                area.y,
                viewport_h,
                self.state.content_length,
                self.state.viewport_length,
                self.state.offset,
            );
        }
    }
}

// ── Scrollbar helper ──────────────────────────────────────────────────────────

/// Draw a vertical scrollbar at column `x`, rows `y..y+height`.
pub(crate) fn draw_vscrollbar(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    height: u16,
    content_len: u16,
    viewport_len: u16,
    offset: u16,
) {
    if height == 0 || content_len == 0 {
        return;
    }

    let track_h = height as f32;
    let content_f = content_len as f32;

    let thumb_h = ((viewport_len as f32 / content_f) * track_h)
        .max(1.0)
        .min(track_h)
        .round() as u16;

    let max_offset = content_len.saturating_sub(viewport_len).max(1) as f32;
    let thumb_top =
        ((offset as f32 / max_offset) * (track_h - thumb_h as f32).max(0.0)).round() as u16;

    for row in 0..height {
        let in_thumb = row >= thumb_top && row < thumb_top + thumb_h;
        let ch = if in_thumb { "█" } else { "░" };
        buf.set_symbol(x, y + row, ch, Style::default());
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── ScrollState ──────────────────────────────────────────────────────────

    #[test]
    fn scroll_up_clamps_at_zero() {
        let mut s = ScrollState {
            offset: 2,
            content_length: 20,
            viewport_length: 5,
        };
        s.scroll_up(10);
        assert_eq!(s.offset, 0);
    }

    #[test]
    fn scroll_down_clamps_at_max_offset() {
        let mut s = ScrollState {
            offset: 0,
            content_length: 20,
            viewport_length: 5,
        };
        s.scroll_down(100);
        assert_eq!(s.offset, 15); // 20 - 5
    }

    #[test]
    fn scroll_to_top_resets_offset() {
        let mut s = ScrollState {
            offset: 10,
            content_length: 20,
            viewport_length: 5,
        };
        s.scroll_to_top();
        assert_eq!(s.offset, 0);
    }

    #[test]
    fn scroll_to_bottom_sets_max_offset() {
        let mut s = ScrollState {
            offset: 0,
            content_length: 20,
            viewport_length: 5,
        };
        s.scroll_to_bottom();
        assert_eq!(s.offset, 15);
    }

    #[test]
    fn at_bottom_when_at_max_offset() {
        let mut s = ScrollState {
            offset: 15,
            content_length: 20,
            viewport_length: 5,
        };
        assert!(s.at_bottom());
        s.offset = 10;
        assert!(!s.at_bottom());
    }

    #[test]
    fn at_bottom_when_content_fits_in_viewport() {
        let s = ScrollState {
            offset: 0,
            content_length: 3,
            viewport_length: 5,
        };
        assert!(s.at_bottom());
    }

    #[test]
    fn scroll_by_page_advances_one_viewport() {
        let mut s = ScrollState {
            offset: 0,
            content_length: 30,
            viewport_length: 10,
        };
        s.scroll_by_page(10);
        assert_eq!(s.offset, 10);
    }

    // ── Scroll widget rendering ──────────────────────────────────────────────

    #[test]
    fn scroll_clips_child_to_viewport() {
        // Build a tall child (10 rows) with known content.
        use crate::layout::Dimension;
        use crate::widgets::canvas::Canvas;

        let child = Canvas::new(|area, buf| {
            for row in 0..area.height {
                buf.set_string(
                    area.x,
                    area.y + row,
                    &format!("row {row:02}"),
                    Style::default(),
                );
            }
        })
        .into_element()
        .height(Dimension::Cells(10));

        let state = ScrollState {
            offset: 2,
            content_length: 10,
            viewport_length: 3,
        };

        let scroll = Scroll::new(child).state(&state).into_element();
        let lines = crate::widgets::testing::render_to_lines(scroll, 8, 3);
        assert_eq!(lines[0], "row 02  ");
        assert_eq!(lines[1], "row 03  ");
        assert_eq!(lines[2], "row 04  ");
    }

    #[test]
    fn scroll_at_top_shows_first_rows() {
        use crate::layout::Dimension;
        use crate::widgets::canvas::Canvas;

        let child = Canvas::new(|area, buf| {
            for row in 0..area.height {
                buf.set_string(area.x, area.y + row, &format!("L{row}"), Style::default());
            }
        })
        .into_element()
        .height(Dimension::Cells(8));

        let state = ScrollState {
            offset: 0,
            content_length: 8,
            viewport_length: 3,
        };

        let lines = crate::widgets::testing::render_to_lines(
            Scroll::new(child).state(&state).into_element(),
            4,
            3,
        );
        assert_eq!(lines[0], "L0  ");
        assert_eq!(lines[1], "L1  ");
        assert_eq!(lines[2], "L2  ");
    }
}
