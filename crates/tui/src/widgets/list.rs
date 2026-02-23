//! List widget — virtual-scrolled list with variable-height items.
//!
//! Items are [`Element`] values stored in the renderer. Only items that
//! overlap the viewport are rendered; items before `state.offset` are skipped.
//! Each item is laid out into a scratch buffer and the result is merged into
//! the frame buffer at the appropriate row.
//!
//! # Item height resolution
//!
//! Height is read from `item.layout_style.size.height`:
//! - `Dimension::Cells(n)` → exact `n` rows.
//! - `Dimension::Auto` / `Dimension::Percent(_)` → defaults to 1.
//!
//! For items with dynamic height, call [`ListState::ensure_visible`] with the
//! pre-computed heights slice instead of relying on the automatic default.

use crate::{
    buffer::Buffer,
    geometry::{Rect, Size},
    layout::{compute_layout, Dimension, LayoutStyle},
    widgets::{Element, IntoElement, Renderable, WidgetId},
};

// ── ListState ─────────────────────────────────────────────────────────────────

/// External state for a [`List`] widget.
#[derive(Debug, Clone, Default)]
pub struct ListState {
    /// Index of the first visible item (topmost row).
    pub offset: usize,
    /// Currently highlighted item (keyboard navigation).
    pub selected: Option<usize>,
}

impl ListState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Select item `i` unconditionally.
    pub fn select(&mut self, i: usize) {
        self.selected = Some(i);
    }

    /// Move selection down, clamping at `item_count - 1`.
    pub fn select_next(&mut self, item_count: usize) {
        if item_count == 0 {
            return;
        }
        self.selected = Some(match self.selected {
            None => 0,
            Some(i) => (i + 1).min(item_count - 1),
        });
    }

    /// Move selection up, clamping at 0.
    pub fn select_prev(&mut self) {
        self.selected = Some(match self.selected {
            None => 0,
            Some(0) => 0,
            Some(i) => i - 1,
        });
    }

    /// Scroll to show the last item and select it.
    pub fn scroll_to_bottom(&mut self, item_count: usize) {
        if item_count > 0 {
            self.offset = item_count.saturating_sub(1);
            self.selected = Some(item_count - 1);
        }
    }

    /// Adjust `offset` so the selected item is fully within the viewport.
    ///
    /// `item_heights[i]` is the height of item `i` in terminal rows.  When
    /// the selected item is taller than the viewport, it is shown at the top.
    pub fn ensure_visible(&mut self, viewport_height: u16, item_heights: &[u16]) {
        let Some(selected) = self.selected else {
            return;
        };
        if item_heights.is_empty() {
            return;
        }
        let selected = selected.min(item_heights.len() - 1);

        // Scroll up if selected is above the current view.
        if selected < self.offset {
            self.offset = selected;
            return;
        }

        // Check whether selected is already visible.
        let mut y = 0u16;
        let mut needs_scroll = true;
        for (i, &h) in item_heights.iter().enumerate().skip(self.offset) {
            if i == selected {
                if y.saturating_add(h) <= viewport_height {
                    needs_scroll = false;
                }
                break;
            }
            y = y.saturating_add(h);
            if y >= viewport_height {
                break; // overflowed without reaching selected
            }
        }

        if !needs_scroll {
            return;
        }

        // Find the highest offset such that selected fits in the viewport.
        // Pack items backward from selected until we run out of space.
        let mut remaining = viewport_height;
        let mut new_offset = selected; // default: selected at top

        for j in (0..=selected).rev() {
            if j >= item_heights.len() {
                break;
            }
            let h = item_heights[j];
            if h > remaining {
                if j == selected {
                    // Selected itself is taller than the viewport — pin to top.
                    new_offset = selected;
                } else {
                    new_offset = j + 1;
                }
                break;
            }
            remaining -= h;
            new_offset = j;
        }

        self.offset = new_offset;
    }
}

// ── List widget ───────────────────────────────────────────────────────────────

/// A virtual-scrolled list of arbitrary [`Element`] items.
pub struct List {
    items: Vec<Element>,
    state: ListState,
    scroll_bar: bool,
}

impl List {
    pub fn new(items: Vec<Element>) -> Self {
        Self {
            items,
            state: ListState::default(),
            scroll_bar: false,
        }
    }

    /// Attach external list state.
    pub fn state(mut self, state: &ListState) -> Self {
        self.state = state.clone();
        self
    }

    /// Show a 1-column scrollbar on the right edge.
    pub fn scroll_bar(mut self, show: bool) -> Self {
        self.scroll_bar = show;
        self
    }
}

impl IntoElement for List {
    fn into_element(self) -> Element {
        let renderer = ListRenderable {
            items: self.items,
            state: self.state,
            show_bar: self.scroll_bar,
        };
        Element {
            id: WidgetId::new(),
            inner: Box::new(renderer),
            layout_style: LayoutStyle::default(),
            // Items are rendered by the renderer, not managed by Taffy.
            children: vec![],
        }
    }
}

// ── Internal renderer ─────────────────────────────────────────────────────────

struct ListRenderable {
    items: Vec<Element>,
    state: ListState,
    show_bar: bool,
}

impl ListRenderable {
    /// Resolve item height without an expensive layout pass.
    fn item_height(item: &Element) -> u16 {
        match item.layout_style.size.height {
            Dimension::Cells(n) => n.max(1),
            _ => 1, // auto/percent — callers should use explicit heights
        }
    }

    fn draw_scrollbar(
        &self,
        buf: &mut Buffer,
        bar_x: u16,
        area_y: u16,
        viewport_h: u16,
        total_items: usize,
    ) {
        crate::widgets::scroll::draw_vscrollbar(
            buf,
            bar_x,
            area_y,
            viewport_h,
            total_items as u16,
            viewport_h, // treat viewport as `viewport_len` for item-count scrollbar
            self.state.offset as u16,
        );
    }
}

impl Renderable for ListRenderable {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() || self.items.is_empty() {
            return;
        }

        let bar_w: u16 = if self.show_bar { 1 } else { 0 };
        let viewport_w = area.width.saturating_sub(bar_w);
        let viewport_h = area.height;

        if viewport_w == 0 {
            return;
        }

        let mut dest_y = area.y;
        let max_y = area.y + viewport_h;
        let total_items = self.items.len();

        for (_, item) in self.items.iter().enumerate().skip(self.state.offset) {
            if dest_y >= max_y {
                break;
            }

            let item_h = Self::item_height(item);
            let visible_h = item_h.min(max_y - dest_y);

            // Render item into a scratch buffer.
            let scratch_area = Rect::new(0, 0, viewport_w, item_h);
            let mut scratch = Buffer::new(scratch_area);
            let child_size = Size {
                width: viewport_w,
                height: item_h,
            };
            let layout = compute_layout(item, child_size);
            item.render(&layout, &mut scratch);

            // Copy visible rows into the frame buffer.
            for row in 0..visible_h {
                for col in 0..viewport_w {
                    let cell = scratch.get(col, row).clone();
                    *buf.get_mut(area.x + col, dest_y + row) = cell;
                }
            }

            dest_y += item_h;
        }

        if self.show_bar && total_items > 0 {
            self.draw_scrollbar(buf, area.x + viewport_w, area.y, viewport_h, total_items);
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── ListState helpers ────────────────────────────────────────────────────

    fn state_with(offset: usize, selected: Option<usize>) -> ListState {
        ListState { offset, selected }
    }

    // select_next / select_prev

    #[test]
    fn select_next_from_none_picks_first() {
        let mut s = ListState::new();
        s.select_next(5);
        assert_eq!(s.selected, Some(0));
    }

    #[test]
    fn select_next_clamps_at_last() {
        let mut s = state_with(0, Some(4));
        s.select_next(5);
        assert_eq!(s.selected, Some(4));
    }

    #[test]
    fn select_prev_from_none_picks_first() {
        let mut s = ListState::new();
        s.select_prev();
        assert_eq!(s.selected, Some(0));
    }

    #[test]
    fn select_prev_clamps_at_zero() {
        let mut s = state_with(0, Some(0));
        s.select_prev();
        assert_eq!(s.selected, Some(0));
    }

    #[test]
    fn select_next_on_empty_list_is_noop() {
        let mut s = ListState::new();
        s.select_next(0);
        assert_eq!(s.selected, None);
    }

    // scroll_to_bottom

    #[test]
    fn scroll_to_bottom_selects_last() {
        let mut s = ListState::new();
        s.scroll_to_bottom(5);
        assert_eq!(s.selected, Some(4));
        assert_eq!(s.offset, 4);
    }

    #[test]
    fn scroll_to_bottom_on_empty_is_noop() {
        let mut s = ListState::new();
        s.scroll_to_bottom(0);
        assert_eq!(s.selected, None);
        assert_eq!(s.offset, 0);
    }

    // ensure_visible

    #[test]
    fn ensure_visible_noop_when_already_visible() {
        // offset=0, items 0-2 each height 2, viewport=6 → all fit
        let mut s = state_with(0, Some(2));
        s.ensure_visible(6, &[2, 2, 2]);
        assert_eq!(s.offset, 0);
    }

    #[test]
    fn ensure_visible_scrolls_up_when_selected_above_offset() {
        let mut s = state_with(3, Some(1));
        s.ensure_visible(5, &[1; 10]);
        assert_eq!(s.offset, 1);
    }

    #[test]
    fn ensure_visible_scrolls_down_packs_backward() {
        // viewport=5, items each height=2, selected=3 (rows 6-7 from offset=0)
        let mut s = state_with(0, Some(3));
        s.ensure_visible(5, &[2, 2, 2, 2, 2]);
        // Backward pack from 3: item3(h=2)+item2(h=2)=4 ≤ 5, item1(h=2)→4+2=6 > 5 → new_offset=2
        assert_eq!(s.offset, 2);
    }

    #[test]
    fn ensure_visible_selected_taller_than_viewport_pins_to_top() {
        let mut s = state_with(0, Some(1));
        // Item 1 has height 10, viewport is only 5
        s.ensure_visible(5, &[1, 10, 1]);
        assert_eq!(s.offset, 1);
    }

    #[test]
    fn ensure_visible_no_change_when_no_selection() {
        let mut s = state_with(2, None);
        s.ensure_visible(5, &[1; 10]);
        assert_eq!(s.offset, 2);
    }

    #[test]
    fn ensure_visible_empty_heights_is_noop() {
        let mut s = state_with(0, Some(0));
        s.ensure_visible(5, &[]);
        assert_eq!(s.offset, 0);
    }

    #[test]
    fn ensure_visible_selected_just_at_boundary_is_noop() {
        // viewport=4, item heights [2,2], selected=1, offset=0
        // item0 rows 0-1, item1 rows 2-3 — exactly fits
        let mut s = state_with(0, Some(1));
        s.ensure_visible(4, &[2, 2]);
        assert_eq!(s.offset, 0);
    }

    #[test]
    fn ensure_visible_selected_overflows_by_one_row() {
        // viewport=3, item heights [2,2], selected=1, offset=0
        // item0 rows 0-1, item1 rows 2-3 — item1 overflows by 1
        let mut s = state_with(0, Some(1));
        s.ensure_visible(3, &[2, 2]);
        // Pack: item1(h=2)+item0(h=2)=4 > 3, so item0 doesn't fit → new_offset=1
        assert_eq!(s.offset, 1);
    }

    // ── List widget rendering ────────────────────────────────────────────────

    fn text_item(s: &str, height: u16) -> Element {
        use crate::layout::Dimension;
        use crate::style::Style;
        use crate::widgets::canvas::Canvas;
        let content = s.to_string();
        Canvas::new(move |area, buf| {
            buf.set_string(area.x, area.y, &content, Style::default());
        })
        .into_element()
        .height(Dimension::Cells(height))
    }

    #[test]
    fn list_renders_items_in_order() {
        let items = vec![
            text_item("AAA", 1),
            text_item("BBB", 1),
            text_item("CCC", 1),
        ];
        let lines = crate::widgets::testing::render_to_lines(List::new(items).into_element(), 6, 3);
        assert_eq!(&lines[0][..3], "AAA");
        assert_eq!(&lines[1][..3], "BBB");
        assert_eq!(&lines[2][..3], "CCC");
    }

    #[test]
    fn list_respects_offset() {
        let items = vec![
            text_item("ONE", 1),
            text_item("TWO", 1),
            text_item("THREE", 1),
        ];
        let mut state = ListState::new();
        state.offset = 1;
        let lines = crate::widgets::testing::render_to_lines(
            List::new(items).state(&state).into_element(),
            6,
            2,
        );
        assert_eq!(&lines[0][..3], "TWO");
        assert_eq!(&lines[1][..5], "THREE");
    }

    #[test]
    fn list_clips_items_that_overflow_viewport() {
        // 3 items of height 2 each, viewport height 5 — third item partially visible.
        // Each Canvas writes a single character at row 0; row 1 of each item is blank.
        let items = vec![text_item("A", 2), text_item("B", 2), text_item("C", 2)];
        let lines = crate::widgets::testing::render_to_lines(List::new(items).into_element(), 4, 5);
        assert_eq!(&lines[0][..1], "A"); // item A, row 0
                                         // lines[1] is item A row 1 — blank (Canvas only writes at area.y)
        assert_eq!(&lines[2][..1], "B"); // item B rows 2-3
                                         // lines[3] is item B row 1 — blank
        assert_eq!(&lines[4][..1], "C"); // item C: only first row visible
    }

    #[test]
    fn list_empty_renders_nothing() {
        let lines =
            crate::widgets::testing::render_to_lines(List::new(vec![]).into_element(), 10, 3);
        assert!(lines.iter().all(|l| l.chars().all(|c| c == ' ')));
    }

    #[test]
    fn list_offset_past_end_renders_blank() {
        let items = vec![text_item("X", 1)];
        let mut state = ListState::new();
        state.offset = 5; // past the only item
        let lines = crate::widgets::testing::render_to_lines(
            List::new(items).state(&state).into_element(),
            4,
            2,
        );
        assert!(lines.iter().all(|l| l.chars().all(|c| c == ' ')));
    }
}
