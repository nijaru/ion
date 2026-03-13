//! Canvas — an escape hatch for custom rendering.
//!
//! The closure receives the assigned frame-buffer area and the root render
//! buffer directly. The area coordinates are absolute within that frame
//! buffer; they are not terminal-global coordinates.
//!
//! No layout children. Ion uses this for streaming text, code blocks, diff
//! views, and custom footer rendering.

use crate::{
    buffer::Buffer,
    geometry::Rect,
    layout::LayoutStyle,
    widgets::{Element, IntoElement, Renderable, WidgetId},
};

type CanvasRenderFn = dyn Fn(Rect, &mut Buffer) + Send + Sync;

pub struct Canvas {
    render_fn: Box<CanvasRenderFn>,
}

impl Canvas {
    pub fn new(f: impl Fn(Rect, &mut Buffer) + Send + Sync + 'static) -> Self {
        Self {
            render_fn: Box::new(f),
        }
    }
}

impl Renderable for Canvas {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        (self.render_fn)(area, buf);
    }
}

impl IntoElement for Canvas {
    fn into_element(self) -> Element {
        Element {
            id: WidgetId::new(),
            inner: Box::new(self),
            layout_style: LayoutStyle::default(),
            children: vec![],
        }
    }
}
