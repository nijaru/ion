use std::sync::atomic::{AtomicU64, Ordering};

use crate::{buffer::Buffer, geometry::Rect};

pub mod canvas;

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
/// `render` writes into the provided buffer region. Widgets must not write
/// outside `area`. The area uses buffer-local coordinates.
pub(crate) trait Renderable: Send + Sync {
    fn render(&self, area: Rect, buf: &mut Buffer);
}

/// A node in the render tree.
///
/// Produced by widget builder methods via [`IntoElement::into_element`].
/// Phase 2: no layout children; the root element receives the full terminal
/// area. Phase 3 will add `layout_style` and `children` for Taffy layout.
pub struct Element {
    #[allow(dead_code)]
    pub(crate) id: WidgetId,
    pub(crate) inner: Box<dyn Renderable>,
}

impl Element {
    pub(crate) fn render(&self, area: Rect, buf: &mut Buffer) {
        self.inner.render(area, buf);
    }
}

/// Conversion helper — widgets implement this to produce an `Element`.
pub trait IntoElement {
    fn into_element(self) -> Element;
}
