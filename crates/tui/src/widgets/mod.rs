use std::sync::atomic::{AtomicU64, Ordering};

use crate::{
    buffer::Buffer,
    geometry::Rect,
    layout::{Dimension, Edges, Layout, LayoutStyle},
};

pub mod block;
pub mod canvas;
pub mod col;
pub mod input;
pub mod list;
pub mod row;
pub mod scroll;
pub mod testing;
pub mod text;

/// A unique identifier for a widget node. Used by the layout pass to map
/// widgets to screen rects. Generated per-frame; do not persist across frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WidgetId(u64);

impl WidgetId {
    pub fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

impl Default for WidgetId {
    fn default() -> Self {
        Self::new()
    }
}

/// Trait implemented by widget internals.
///
/// `render` writes into the provided frame buffer region. Widgets must not
/// write outside `area`.
///
/// The `area` coordinates are absolute within the frame buffer, not terminal-
/// global screen coordinates. In practice this means:
///
/// - `area` is already positioned relative to the root render buffer
/// - `Buffer` writes use the same coordinate space as `area`
/// - terminal inline/fullscreen offsets are applied later by `Terminal`
pub(crate) trait Renderable: Send + Sync {
    fn render(&self, area: Rect, buf: &mut Buffer);
}

/// A node in the render tree.
///
/// Produced by widget builder methods via [`IntoElement::into_element`].
/// Layout is computed by Taffy before rendering; each element gets a [`Rect`]
/// determined by its [`LayoutStyle`] and its position in the tree.
pub struct Element {
    pub(crate) id: WidgetId,
    pub(crate) inner: Box<dyn Renderable>,
    pub(crate) layout_style: LayoutStyle,
    pub(crate) children: Vec<Element>,
}

impl Element {
    /// Render this element and all children into the buffer.
    /// Each element's area is looked up from the layout pass result.
    pub(crate) fn render(&self, layout: &Layout, buf: &mut Buffer) {
        let area = layout.get(self.id);
        self.inner.render(area, buf);
        for child in &self.children {
            child.render(layout, buf);
        }
    }

    // ── Layout builder methods ────────────────────────────────────────────────

    pub fn width(mut self, w: Dimension) -> Self {
        self.layout_style.size.width = w;
        self
    }

    pub fn height(mut self, h: Dimension) -> Self {
        self.layout_style.size.height = h;
        self
    }

    pub fn min_width(mut self, w: Dimension) -> Self {
        self.layout_style.min_size.width = w;
        self
    }

    pub fn min_height(mut self, h: Dimension) -> Self {
        self.layout_style.min_size.height = h;
        self
    }

    pub fn max_width(mut self, w: Dimension) -> Self {
        self.layout_style.max_size.width = w;
        self
    }

    pub fn max_height(mut self, h: Dimension) -> Self {
        self.layout_style.max_size.height = h;
        self
    }

    pub fn flex_grow(mut self, v: f32) -> Self {
        self.layout_style.flex_grow = v;
        self
    }

    pub fn flex_shrink(mut self, v: f32) -> Self {
        self.layout_style.flex_shrink = v;
        self
    }

    pub fn padding(mut self, edges: Edges) -> Self {
        self.layout_style.padding = edges;
        self
    }

    pub fn margin(mut self, edges: Edges) -> Self {
        self.layout_style.margin = edges;
        self
    }
}

/// Conversion helper — widgets implement this to produce an `Element`.
pub trait IntoElement {
    fn into_element(self) -> Element;
}
