//! Simple single-line text input for filter/search fields.
//!
//! Minimal implementation to replace rat-text::TextInput for pickers.

use ratatui::prelude::*;
use ratatui::widgets::{Block, Paragraph, Widget};

/// Simple single-line input state.
#[derive(Debug, Clone, Default)]
pub struct FilterInputState {
    /// The text content.
    content: String,
    /// Cursor position (character index).
    cursor: usize,
    /// Screen cursor position (set during render).
    screen_cursor: Option<(u16, u16)>,
}

impl FilterInputState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get the current text content.
    pub fn text(&self) -> &str {
        &self.content
    }

    /// Clear the input.
    pub fn clear(&mut self) {
        self.content.clear();
        self.cursor = 0;
    }

    /// Set the text content.
    pub fn set_text(&mut self, text: &str) {
        self.content = text.to_string();
        self.cursor = self.content.chars().count();
    }

    /// Insert a character at the cursor.
    pub fn insert_char(&mut self, ch: char) {
        let byte_idx = self.char_to_byte(self.cursor);
        self.content.insert(byte_idx, ch);
        self.cursor += 1;
    }

    /// Delete the character before the cursor.
    pub fn delete_char_before(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            let byte_idx = self.char_to_byte(self.cursor);
            let next_byte = self.char_to_byte(self.cursor + 1);
            self.content.drain(byte_idx..next_byte);
        }
    }

    /// Delete the character after the cursor.
    pub fn delete_char_after(&mut self) {
        let len = self.content.chars().count();
        if self.cursor < len {
            let byte_idx = self.char_to_byte(self.cursor);
            let next_byte = self.char_to_byte(self.cursor + 1);
            self.content.drain(byte_idx..next_byte);
        }
    }

    /// Move cursor left.
    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Move cursor right.
    pub fn move_right(&mut self) {
        let len = self.content.chars().count();
        self.cursor = (self.cursor + 1).min(len);
    }

    /// Move cursor to start.
    pub fn move_to_start(&mut self) {
        self.cursor = 0;
    }

    /// Move cursor to end.
    pub fn move_to_end(&mut self) {
        self.cursor = self.content.chars().count();
    }

    /// Get the screen cursor position (set during render).
    pub fn screen_cursor(&self) -> Option<(u16, u16)> {
        self.screen_cursor
    }

    /// Convert character index to byte index.
    fn char_to_byte(&self, char_idx: usize) -> usize {
        self.content
            .char_indices()
            .nth(char_idx)
            .map(|(i, _)| i)
            .unwrap_or(self.content.len())
    }
}

/// Simple single-line input widget.
#[allow(dead_code)]
pub struct FilterInput<'a> {
    block: Option<Block<'a>>,
    style: Style,
    placeholder: Option<&'a str>,
}

impl<'a> FilterInput<'a> {
    pub fn new() -> Self {
        Self {
            block: None,
            style: Style::default(),
            placeholder: None,
        }
    }

    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }

    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    pub fn placeholder(mut self, placeholder: &'a str) -> Self {
        self.placeholder = Some(placeholder);
        self
    }
}

impl Default for FilterInput<'_> {
    fn default() -> Self {
        Self::new()
    }
}

impl StatefulWidget for FilterInput<'_> {
    type State = FilterInputState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let inner = if let Some(block) = self.block {
            let inner = block.inner(area);
            block.render(area, buf);
            inner
        } else {
            area
        };

        if inner.width == 0 || inner.height == 0 {
            state.screen_cursor = None;
            return;
        }

        // Render content or placeholder
        let display_text = if state.content.is_empty() {
            self.placeholder.unwrap_or("").to_string()
        } else {
            state.content.clone()
        };

        let style = if state.content.is_empty() && self.placeholder.is_some() {
            Style::default().fg(Color::DarkGray).italic()
        } else {
            self.style
        };

        Paragraph::new(display_text).style(style).render(inner, buf);

        // Calculate cursor screen position
        let cursor_x = state
            .content
            .chars()
            .take(state.cursor)
            .map(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(0) as u16)
            .sum::<u16>();

        state.screen_cursor = Some((inner.x + cursor_x, inner.y));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_input() {
        let mut state = FilterInputState::new();

        state.insert_char('h');
        state.insert_char('i');
        assert_eq!(state.text(), "hi");

        state.delete_char_before();
        assert_eq!(state.text(), "h");

        state.clear();
        assert_eq!(state.text(), "");
    }

    #[test]
    fn test_cursor_movement() {
        let mut state = FilterInputState::new();
        state.set_text("hello");

        state.move_to_start();
        assert_eq!(state.cursor, 0);

        state.move_right();
        assert_eq!(state.cursor, 1);

        state.move_to_end();
        assert_eq!(state.cursor, 5);
    }
}
