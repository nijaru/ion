//! StreamingText — widget for incremental LLM token streaming.
//!
//! Renders markdown content into a `tui::Canvas`, bridging ion's styled
//! line model to the `tui::Buffer` API. Designed for high-throughput token
//! streaming: `push_token` is O(1) append; layout happens at render time.

use tui::{
    geometry::Rect,
    widgets::{Element, IntoElement, canvas::Canvas},
};

use crate::ui::style::text_style_to_tui;

/// Widget state for streaming text — store this in your app model.
///
/// ```no_run
/// # use ion::ui::StreamingText;
/// let mut st = StreamingText::new();
/// st.push_token("Hello, **world**!");
/// // later, in App::view:
/// // let element = st.view();
/// ```
pub struct StreamingText {
    /// Raw accumulated content — the source of truth.
    content: String,
    /// Scroll offset in rendered lines (ignored when auto_scroll is true).
    scroll_offset: usize,
    /// Pin the view to the bottom while content is streaming in.
    pub auto_scroll: bool,
}

impl StreamingText {
    pub fn new() -> Self {
        Self {
            content: String::new(),
            scroll_offset: 0,
            auto_scroll: true,
        }
    }

    /// Append a token to the content. O(token.len()) — no re-render.
    pub fn push_token(&mut self, token: &str) {
        self.content.push_str(token);
    }

    /// Replace all content and reset scroll position.
    pub fn set_content(&mut self, content: &str) {
        self.content.clear();
        self.content.push_str(content);
        self.scroll_offset = 0;
        self.auto_scroll = true;
    }

    /// Clear all content.
    pub fn clear(&mut self) {
        self.content.clear();
        self.scroll_offset = 0;
        self.auto_scroll = true;
    }

    /// Scroll up by `n` rendered rows. Disables auto-scroll.
    pub fn scroll_up(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
        self.auto_scroll = false;
    }

    /// Scroll down by `n` rendered rows.
    pub fn scroll_down(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_add(n);
    }

    /// Re-enable auto-scroll (e.g. after user scrolls back to bottom).
    pub fn resume_auto_scroll(&mut self) {
        self.auto_scroll = true;
    }

    /// Raw content length in bytes.
    pub fn len(&self) -> usize {
        self.content.len()
    }

    pub fn is_empty(&self) -> bool {
        self.content.is_empty()
    }

    /// Build a renderable element capturing the current state.
    ///
    /// The returned `Element` renders the markdown content into the allocated
    /// `Rect`. Called from `App::view(&self)` — no mutation needed.
    pub fn view(&self) -> Element {
        let content = self.content.clone();
        let scroll_offset = self.scroll_offset;
        let auto_scroll = self.auto_scroll;

        Canvas::new(move |area: Rect, buf: &mut tui::buffer::Buffer| {
            if area.is_empty() || content.is_empty() {
                return;
            }

            let lines = crate::tui::highlight::markdown::render_markdown_with_width(
                &content,
                area.width as usize,
            );
            let total = lines.len();
            let visible = area.height as usize;

            let offset = if auto_scroll {
                total.saturating_sub(visible)
            } else {
                scroll_offset.min(total.saturating_sub(visible))
            };

            for (row_offset, line) in lines.iter().skip(offset).take(visible).enumerate() {
                let row = area.y + row_offset as u16;
                let mut col = area.x;
                let max_col = area.x + area.width;

                for span in &line.spans {
                    if col >= max_col {
                        break;
                    }
                    let style = text_style_to_tui(&span.style);
                    col = buf.set_string(col, row, &span.content, style);
                }
            }
        })
        .into_element()
    }
}

impl Default for StreamingText {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_token_accumulates() {
        let mut st = StreamingText::new();
        st.push_token("Hello, ");
        st.push_token("world");
        assert_eq!(st.content, "Hello, world");
    }

    #[test]
    fn set_content_replaces() {
        let mut st = StreamingText::new();
        st.push_token("old content");
        st.set_content("new content");
        assert_eq!(st.content, "new content");
        assert_eq!(st.scroll_offset, 0);
    }

    #[test]
    fn view_renders_without_panic() {
        let mut st = StreamingText::new();
        st.push_token("# Hello\n\nSome text.");

        let element = st.view();
        let lines = tui::widgets::testing::render_to_lines(element, 40, 10);
        assert!(!lines.is_empty());
    }
}
