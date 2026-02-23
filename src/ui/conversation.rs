//! ConversationView — virtual-scrolled list of conversation messages.
//!
//! Holds all rendered message entries. Only lines that overlap the visible
//! viewport are written to the buffer — O(visible_height) per frame, not
//! O(total_content). Heights are cached per entry per terminal width;
//! invalidated on resize.

use tui::{
    geometry::Rect,
    widgets::{canvas::Canvas, Element, IntoElement},
};

use crate::tui::highlight::markdown::render_markdown_with_width;
use crate::tui::terminal::{Color as IonColor, StyledLine, TextStyle};
use crate::ui::style::text_style_to_tui;

// ── Entry types ───────────────────────────────────────────────────────────────

/// Role of a conversation entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryRole {
    User,
    Assistant,
    ToolCall { name: String },
    ToolResult { name: String, is_error: bool },
}

/// A single conversation entry.
pub struct ConversationEntry {
    pub role: EntryRole,
    /// Raw markdown content.
    pub content: String,
    /// Cached rendered lines: (width, lines). Invalidated when width changes.
    rendered: Option<(u16, Vec<StyledLine>)>,
}

impl ConversationEntry {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: EntryRole::User,
            content: content.into(),
            rendered: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: EntryRole::Assistant,
            content: content.into(),
            rendered: None,
        }
    }

    pub fn tool_call(name: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: EntryRole::ToolCall { name: name.into() },
            content: content.into(),
            rendered: None,
        }
    }

    pub fn tool_result(
        name: impl Into<String>,
        content: impl Into<String>,
        is_error: bool,
    ) -> Self {
        Self {
            role: EntryRole::ToolResult {
                name: name.into(),
                is_error,
            },
            content: content.into(),
            rendered: None,
        }
    }

    /// Render (or return cached) lines for the given width.
    fn lines_at_width(&mut self, width: u16) -> &[StyledLine] {
        if self.rendered.as_ref().map_or(true, |(w, _)| *w != width) {
            let lines = self.render_to_lines(width);
            self.rendered = Some((width, lines));
        }
        &self.rendered.as_ref().unwrap().1
    }

    fn render_to_lines(&self, width: u16) -> Vec<StyledLine> {
        let w = width as usize;
        match &self.role {
            EntryRole::User => {
                // Prepend "› " prefix in a dim style, then the content.
                let prefix = "› ";
                let content_width = w.saturating_sub(prefix.len());
                let mut result = Vec::new();
                let content_lines = render_markdown_with_width(&self.content, content_width.max(1));
                for (i, line) in content_lines.iter().enumerate() {
                    let mut spans = Vec::new();
                    if i == 0 {
                        spans.push(crate::tui::terminal::StyledSpan::dim(prefix));
                    } else {
                        // Indent continuation lines
                        spans.push(crate::tui::terminal::StyledSpan::raw(
                            " ".repeat(prefix.len()),
                        ));
                    }
                    spans.extend(line.spans.clone());
                    result.push(StyledLine::new(spans));
                }
                if result.is_empty() {
                    result.push(StyledLine::empty());
                }
                result
            }
            EntryRole::Assistant => render_markdown_with_width(&self.content, w),
            EntryRole::ToolCall { name } => {
                // Compact header: "  [tool: name]" then content indented
                render_tool_block(&format!("tool: {name}"), &self.content, w, false)
            }
            EntryRole::ToolResult { name, is_error } => {
                render_tool_block(&format!("result: {name}"), &self.content, w, *is_error)
            }
        }
    }
}

/// Render a tool call or result as a compact indented block.
fn render_tool_block(header: &str, content: &str, width: usize, is_error: bool) -> Vec<StyledLine> {
    let header_style = if is_error {
        TextStyle {
            foreground_color: Some(IonColor::DarkRed),
            bold: true,
            ..Default::default()
        }
    } else {
        TextStyle {
            foreground_color: Some(IonColor::DarkGrey),
            ..Default::default()
        }
    };

    let mut result = vec![StyledLine::new(vec![
        crate::tui::terminal::StyledSpan::new(format!("[{header}]"), header_style),
    ])];

    // Indent content by 2 spaces
    let indent = "  ";
    let content_width = width.saturating_sub(indent.len()).max(1);
    let content_lines = render_markdown_with_width(content, content_width);
    for line in content_lines {
        let mut spans = vec![crate::tui::terminal::StyledSpan::raw(indent)];
        spans.extend(line.spans);
        result.push(StyledLine::new(spans));
    }
    result
}

// ── ConversationView ──────────────────────────────────────────────────────────

/// Widget state for the conversation history — store this in your app model.
///
/// Entries are appended as messages arrive. Heights are cached per width and
/// invalidated on resize. Virtual scrolling ensures only visible lines hit
/// the buffer.
pub struct ConversationView {
    entries: Vec<ConversationEntry>,
    /// Total rendered lines across all entries at the last known width.
    total_lines: usize,
    /// Scroll offset in rendered lines.
    scroll_offset: usize,
    /// Pin to bottom while streaming.
    pub auto_scroll: bool,
}

impl ConversationView {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            total_lines: 0,
            scroll_offset: 0,
            auto_scroll: true,
        }
    }

    /// Append a new entry to the conversation.
    pub fn push(&mut self, entry: ConversationEntry) {
        self.entries.push(entry);
        // Force total_lines recount on next view()
        self.total_lines = usize::MAX;
    }

    /// Append a user message.
    pub fn push_user(&mut self, content: impl Into<String>) {
        self.push(ConversationEntry::user(content));
    }

    /// Append an assistant message.
    pub fn push_assistant(&mut self, content: impl Into<String>) {
        self.push(ConversationEntry::assistant(content));
    }

    /// Update the last entry's content (for streaming into the last message).
    /// If the last entry is not an assistant entry, does nothing.
    pub fn append_to_last(&mut self, token: &str) {
        if let Some(entry) = self.entries.last_mut() {
            if entry.role == EntryRole::Assistant {
                entry.content.push_str(token);
                entry.rendered = None; // invalidate cache
                self.total_lines = usize::MAX;
            }
        }
    }

    /// Replace the last entry's content entirely.
    pub fn set_last_content(&mut self, content: &str) {
        if let Some(entry) = self.entries.last_mut() {
            entry.content.clear();
            entry.content.push_str(content);
            entry.rendered = None;
            self.total_lines = usize::MAX;
        }
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Scroll up by `n` lines. Disables auto-scroll.
    pub fn scroll_up(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
        self.auto_scroll = false;
    }

    /// Scroll down by `n` lines.
    pub fn scroll_down(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_add(n);
    }

    /// Re-enable auto-scroll.
    pub fn resume_auto_scroll(&mut self) {
        self.auto_scroll = true;
    }

    /// Compute rendered lines for all entries at the given width, updating
    /// the height cache. Called from `view()` which takes `&mut self`.
    ///
    /// Returns the total line count.
    fn ensure_rendered(&mut self, width: u16) -> usize {
        let total: usize = self
            .entries
            .iter_mut()
            .map(|e| e.lines_at_width(width).len())
            .sum();
        self.total_lines = total;
        total
    }

    /// Build a renderable element.
    ///
    /// Unlike `StreamingText::view()`, this takes `&mut self` to update the
    /// height cache before building the immutable Canvas closure.
    pub fn view(&mut self, width: u16) -> Element {
        // Pre-compute all rendered lines at this width, then capture a flat
        // clone into the Canvas closure. This avoids interior mutability in
        // the closure while keeping render time O(visible_height).
        self.ensure_rendered(width);

        // Collect a flat list of (StyledLine, separator_before) for efficient
        // viewport slicing.
        let mut flat: Vec<StyledLine> = Vec::with_capacity(self.total_lines);
        for (i, entry) in self.entries.iter().enumerate() {
            // Blank separator between entries (skip before first)
            if i > 0 {
                flat.push(StyledLine::empty());
            }
            if let Some((_, ref lines)) = entry.rendered {
                flat.extend(lines.iter().cloned());
            }
        }

        let total = flat.len();
        let scroll_offset = self.scroll_offset;
        let auto_scroll = self.auto_scroll;

        Canvas::new(move |area: Rect, buf: &mut tui::buffer::Buffer| {
            if area.is_empty() || flat.is_empty() {
                return;
            }
            let visible = area.height as usize;
            let offset = if auto_scroll {
                total.saturating_sub(visible)
            } else {
                scroll_offset.min(total.saturating_sub(visible))
            };

            for (row_offset, line) in flat.iter().skip(offset).take(visible).enumerate() {
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

impl Default for ConversationView {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_render() {
        let mut cv = ConversationView::new();
        cv.push_user("Hello");
        cv.push_assistant("Hi there! How can I help?");

        let element = cv.view(40);
        let lines = tui::widgets::testing::render_to_lines(element, 40, 20);
        assert!(!lines.is_empty());
        // User line has the › prefix
        assert!(lines[0].contains('›'));
    }

    #[test]
    fn append_to_last_updates_content() {
        let mut cv = ConversationView::new();
        cv.push_assistant("");
        cv.append_to_last("Hello ");
        cv.append_to_last("world");
        assert_eq!(cv.entries[0].content, "Hello world");
        assert!(cv.entries[0].rendered.is_none()); // cache invalidated
    }

    #[test]
    fn width_cache_invalidation() {
        let mut cv = ConversationView::new();
        cv.push_assistant("Some text");

        cv.ensure_rendered(40);
        assert!(cv.entries[0].rendered.as_ref().unwrap().0 == 40);

        // Simulate resize: lines_at_width with different width should re-render
        cv.entries[0].lines_at_width(60);
        assert!(cv.entries[0].rendered.as_ref().unwrap().0 == 60);
    }

    #[test]
    fn empty_conversation_renders_cleanly() {
        let mut cv = ConversationView::new();
        let element = cv.view(40);
        let lines = tui::widgets::testing::render_to_lines(element, 40, 10);
        // All blank
        for line in &lines {
            assert!(line.trim().is_empty());
        }
    }
}
