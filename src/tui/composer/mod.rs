pub mod buffer;

pub use buffer::ComposerBuffer;

use unicode_segmentation::UnicodeSegmentation;

/// Build a list of visual lines as (start_char_idx, end_char_idx) pairs using word-wrap.
/// This matches Ratatui's Paragraph::wrap(Wrap { trim: false }) behavior.
/// end_char_idx is exclusive.
pub fn build_visual_lines(content: &str, width: usize) -> Vec<(usize, usize)> {
    use unicode_width::UnicodeWidthChar;

    let mut lines = Vec::new();
    if width == 0 {
        lines.push((0, content.chars().count()));
        return lines;
    }

    let chars: Vec<char> = content.chars().collect();
    let mut line_start = 0;
    let mut col = 0;
    let mut last_space_idx = None::<usize>; // char index AFTER the space

    for (i, &c) in chars.iter().enumerate() {
        if c == '\n' {
            lines.push((line_start, i + 1)); // Include newline in range
            line_start = i + 1;
            col = 0;
            last_space_idx = None;
        } else {
            let char_width = UnicodeWidthChar::width(c).unwrap_or(0);

            if col + char_width > width {
                // Need to wrap
                if let Some(space_idx) = last_space_idx {
                    // Wrap at the last space
                    lines.push((line_start, space_idx));
                    line_start = space_idx;
                    // Recalculate col from space_idx to i
                    col = 0;
                    for ch in chars.iter().take(i).skip(space_idx) {
                        col += UnicodeWidthChar::width(*ch).unwrap_or(0);
                    }
                    last_space_idx = None;
                } else {
                    // No space - wrap at character boundary
                    lines.push((line_start, i));
                    line_start = i;
                    col = 0;
                }
            }

            if c == ' ' {
                last_space_idx = Some(i + 1);
            }

            col += char_width;
        }
    }

    // Final line
    lines.push((line_start, chars.len()));
    lines
}

/// Find which visual line contains the given char index and the column within that line.
fn find_visual_line_and_col(lines: &[(usize, usize)], char_idx: usize) -> (usize, usize) {
    for (i, (start, end)) in lines.iter().enumerate() {
        if char_idx >= *start && char_idx < *end {
            return (i, char_idx - start);
        }
        // Handle cursor at end of line (at the boundary)
        if char_idx == *end && i == lines.len() - 1 {
            return (i, char_idx - start);
        }
    }
    // Cursor at very end
    let last = lines.len().saturating_sub(1);
    (last, char_idx.saturating_sub(lines[last].0))
}

/// Ephemeral UI state for the Composer widget.
#[derive(Debug, Clone, Default)]
pub struct ComposerState {
    /// Absolute character index in the buffer.
    cursor_char_idx: usize,
    /// Vertical scroll offset (in visual lines) for internal scrolling.
    scroll_offset: usize,
    /// Stashed draft (Ctrl+S style) - includes both text and blobs.
    stash: Option<(String, Vec<String>)>,
    /// Calculated cursor position (x, y) relative to the widget area.
    pub cursor_pos: (u16, u16),
    /// Last known render width (for scroll calculations).
    last_width: usize,
}

impl ComposerState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cursor_char_idx(&self) -> usize {
        self.cursor_char_idx
    }

    pub fn set_cursor(&mut self, idx: usize, max_len: usize) {
        self.cursor_char_idx = idx.min(max_len);
    }

    /// Move cursor to the start of the buffer.
    pub fn move_to_start(&mut self) {
        self.cursor_char_idx = 0;
    }

    /// Move cursor to the end of the buffer.
    pub fn move_to_end(&mut self, buffer: &ComposerBuffer) {
        self.cursor_char_idx = buffer.len_chars();
    }

    /// Move cursor to the start of the current line.
    pub fn move_to_line_start(&mut self, buffer: &ComposerBuffer) {
        if self.cursor_char_idx == 0 {
            return;
        }
        let line_idx = buffer.char_to_line(self.cursor_char_idx);
        self.cursor_char_idx = buffer.line_to_char(line_idx);
    }

    /// Move cursor to the end of the current line.
    pub fn move_to_line_end(&mut self, buffer: &ComposerBuffer) {
        let len = buffer.len_chars();
        if self.cursor_char_idx >= len {
            return;
        }
        let line_idx = buffer.char_to_line(self.cursor_char_idx);
        let next_line_start = if line_idx + 1 < buffer.len_lines() {
            buffer.line_to_char(line_idx + 1)
        } else {
            len
        };
        // Position before newline if there is one
        let content = buffer.get_content();
        if next_line_start > 0 && content.chars().nth(next_line_start - 1) == Some('\n') {
            self.cursor_char_idx = next_line_start - 1;
        } else {
            self.cursor_char_idx = next_line_start;
        }
    }

    /// Move cursor to the start of the current VISUAL line (wrapped line).
    /// Uses last_width from previous render; falls back to logical line start if width unknown.
    pub fn move_to_visual_line_start(&mut self, buffer: &ComposerBuffer) {
        if self.last_width == 0 || self.cursor_char_idx == 0 {
            self.move_to_line_start(buffer);
            return;
        }

        let content = buffer.get_content();
        let lines = build_visual_lines(&content, self.last_width);
        let (cur_line, _) = find_visual_line_and_col(&lines, self.cursor_char_idx);

        if cur_line < lines.len() {
            self.cursor_char_idx = lines[cur_line].0;
        }
    }

    /// Move cursor to the end of the current VISUAL line (wrapped line).
    /// Uses last_width from previous render; falls back to logical line end if width unknown.
    pub fn move_to_visual_line_end(&mut self, buffer: &ComposerBuffer) {
        if self.last_width == 0 {
            self.move_to_line_end(buffer);
            return;
        }

        let content = buffer.get_content();
        let len = buffer.len_chars();
        if self.cursor_char_idx >= len {
            return;
        }

        let lines = build_visual_lines(&content, self.last_width);
        let (cur_line, _) = find_visual_line_and_col(&lines, self.cursor_char_idx);

        if cur_line < lines.len() {
            let line_end = lines[cur_line].1;
            // If line ends with newline, position before it
            if line_end > 0 && content.chars().nth(line_end - 1) == Some('\n') {
                self.cursor_char_idx = line_end - 1;
            } else {
                // For wrapped lines (not ending in newline), go to end
                self.cursor_char_idx = line_end;
            }
        }
    }

    /// Move cursor one grapheme cluster to the left.
    pub fn move_left(&mut self, buffer: &ComposerBuffer) {
        if self.cursor_char_idx == 0 {
            return;
        }

        let rope = buffer.rope();
        let window_size = 10;
        let start = self.cursor_char_idx.saturating_sub(window_size);
        let prefix = rope.slice(start..self.cursor_char_idx);
        let prefix_str = prefix.to_string();

        if let Some((offset, _)) = prefix_str.grapheme_indices(true).next_back() {
            let chars_to_move = prefix_str[offset..].chars().count();
            self.cursor_char_idx -= chars_to_move;
        } else {
            self.cursor_char_idx = self.cursor_char_idx.saturating_sub(1);
        }
    }

    /// Move cursor one grapheme cluster to the right.
    pub fn move_right(&mut self, buffer: &ComposerBuffer) {
        let len = buffer.len_chars();
        if self.cursor_char_idx >= len {
            return;
        }

        let rope = buffer.rope();
        let window_size = 10;
        let end = (self.cursor_char_idx + window_size).min(len);
        let suffix = rope.slice(self.cursor_char_idx..end);
        let suffix_str = suffix.to_string();

        if let Some((_, grapheme)) = suffix_str.grapheme_indices(true).next() {
            let chars_to_move = grapheme.chars().count();
            self.cursor_char_idx = (self.cursor_char_idx + chars_to_move).min(len);
        } else {
            self.cursor_char_idx = (self.cursor_char_idx + 1).min(len);
        }
    }

    /// Move cursor up one visual line (including wrapped lines).
    /// Uses last_width from previous render; falls back to logical line if width unknown.
    pub fn move_up(&mut self, buffer: &ComposerBuffer) -> bool {
        if self.last_width == 0 {
            return self.move_up_logical(buffer);
        }
        self.move_up_visual(buffer, self.last_width)
    }

    /// Move cursor down one visual line (including wrapped lines).
    /// Uses last_width from previous render; falls back to logical line if width unknown.
    pub fn move_down(&mut self, buffer: &ComposerBuffer) -> bool {
        if self.last_width == 0 {
            return self.move_down_logical(buffer);
        }
        self.move_down_visual(buffer, self.last_width)
    }

    /// Move cursor up one visual line at the given width.
    fn move_up_visual(&mut self, buffer: &ComposerBuffer, width: usize) -> bool {
        let content = buffer.get_content();
        if content.is_empty() {
            return false;
        }

        // Build visual line map: Vec<(start_char_idx, end_char_idx)>
        let lines = build_visual_lines(&content, width);

        // Find which visual line the cursor is on
        let (cur_line, col_in_line) = find_visual_line_and_col(&lines, self.cursor_char_idx);

        if cur_line == 0 {
            return false; // Already on first visual line
        }

        // Move to same column on previous visual line
        let prev_line = &lines[cur_line - 1];
        let prev_line_len = prev_line.1 - prev_line.0;
        let target_col = col_in_line.min(prev_line_len.saturating_sub(1));
        self.cursor_char_idx = prev_line.0 + target_col;
        true
    }

    /// Move cursor down one visual line at the given width.
    fn move_down_visual(&mut self, buffer: &ComposerBuffer, width: usize) -> bool {
        let content = buffer.get_content();
        if content.is_empty() {
            return false;
        }

        // Build visual line map
        let lines = build_visual_lines(&content, width);

        // Find which visual line the cursor is on
        let (cur_line, col_in_line) = find_visual_line_and_col(&lines, self.cursor_char_idx);

        if cur_line >= lines.len() - 1 {
            return false; // Already on last visual line
        }

        // Move to same column on next visual line
        let next_line = &lines[cur_line + 1];
        let next_line_len = next_line.1 - next_line.0;
        let target_col = col_in_line.min(next_line_len.saturating_sub(1));
        self.cursor_char_idx = next_line.0 + target_col;
        true
    }

    /// Move cursor up one logical line (newline-separated).
    fn move_up_logical(&mut self, buffer: &ComposerBuffer) -> bool {
        let line_idx = buffer.char_to_line(self.cursor_char_idx);
        if line_idx == 0 {
            return false;
        }

        let line_start = buffer.line_to_char(line_idx);
        let col = self.cursor_char_idx - line_start;

        let prev_line_start = buffer.line_to_char(line_idx - 1);
        let prev_line_len = line_start - prev_line_start - 1;

        self.cursor_char_idx = prev_line_start + col.min(prev_line_len);
        true
    }

    /// Move cursor down one logical line (newline-separated).
    fn move_down_logical(&mut self, buffer: &ComposerBuffer) -> bool {
        let line_idx = buffer.char_to_line(self.cursor_char_idx);
        if line_idx >= buffer.len_lines().saturating_sub(1) {
            return false;
        }

        let line_start = buffer.line_to_char(line_idx);
        let col = self.cursor_char_idx - line_start;

        let next_line_start = buffer.line_to_char(line_idx + 1);
        let next_next_start = if line_idx + 2 < buffer.len_lines() {
            buffer.line_to_char(line_idx + 2)
        } else {
            buffer.len_chars()
        };
        let next_line_len = next_next_start - next_line_start;
        let next_line_len = if next_line_len > 0
            && buffer.get_content().chars().nth(next_next_start - 1) == Some('\n')
        {
            next_line_len - 1
        } else {
            next_line_len
        };

        self.cursor_char_idx = next_line_start + col.min(next_line_len);
        true
    }

    /// Move cursor one word to the left.
    pub fn move_word_left(&mut self, buffer: &ComposerBuffer) {
        if self.cursor_char_idx == 0 {
            return;
        }

        let rope = buffer.rope();
        let prefix = rope.slice(..self.cursor_char_idx);
        let prefix_str = prefix.to_string();

        // Skip whitespace, then find word boundary
        let trimmed = prefix_str.trim_end();
        if trimmed.is_empty() {
            self.cursor_char_idx = 0;
            return;
        }

        if let Some((offset, _)) = trimmed.unicode_word_indices().next_back() {
            self.cursor_char_idx = offset;
        } else {
            self.cursor_char_idx = 0;
        }
    }

    /// Move cursor one word to the right.
    pub fn move_word_right(&mut self, buffer: &ComposerBuffer) {
        let len = buffer.len_chars();
        if self.cursor_char_idx >= len {
            return;
        }

        let rope = buffer.rope();
        let suffix = rope.slice(self.cursor_char_idx..);
        let suffix_str = suffix.to_string();

        // Find next word boundary
        let mut iter = suffix_str.unicode_word_indices();
        if let Some((offset, word)) = iter.next() {
            if offset == 0 {
                // Cursor is at start of word, jump to end
                self.cursor_char_idx = (self.cursor_char_idx + word.chars().count()).min(len);
            } else {
                // Cursor is before word, jump to start
                self.cursor_char_idx = (self.cursor_char_idx + offset).min(len);
            }
        } else {
            self.cursor_char_idx = len;
        }
    }

    /// Delete the grapheme before the cursor (backspace).
    pub fn delete_char_before(&mut self, buffer: &mut ComposerBuffer) {
        if self.cursor_char_idx == 0 {
            return;
        }

        let old_idx = self.cursor_char_idx;
        self.move_left(buffer);
        buffer.remove_range(self.cursor_char_idx..old_idx);
    }

    /// Delete the grapheme after the cursor (delete key).
    pub fn delete_char_after(&mut self, buffer: &mut ComposerBuffer) {
        if self.cursor_char_idx >= buffer.len_chars() {
            return;
        }

        let old_idx = self.cursor_char_idx;
        // Temporarily move right to find grapheme boundary
        let len = buffer.len_chars();
        let rope = buffer.rope();
        let window_size = 10;
        let end = (self.cursor_char_idx + window_size).min(len);
        let suffix = rope.slice(self.cursor_char_idx..end);
        let suffix_str = suffix.to_string();

        let chars_to_delete = if let Some((_, grapheme)) = suffix_str.grapheme_indices(true).next()
        {
            grapheme.chars().count()
        } else {
            1
        };

        buffer.remove_range(old_idx..old_idx + chars_to_delete);
    }

    /// Delete the word before the cursor (Ctrl+W / Opt+Backspace).
    pub fn delete_word(&mut self, buffer: &mut ComposerBuffer) {
        if self.cursor_char_idx == 0 {
            return;
        }

        let old_idx = self.cursor_char_idx;
        self.move_word_left(buffer);
        buffer.remove_range(self.cursor_char_idx..old_idx);
    }

    /// Delete everything to the left of the cursor on the current line (Ctrl+U).
    pub fn delete_line_left(&mut self, buffer: &mut ComposerBuffer) {
        if self.cursor_char_idx == 0 {
            return;
        }

        let line_idx = buffer.char_to_line(self.cursor_char_idx);
        let line_start = buffer.line_to_char(line_idx);

        if self.cursor_char_idx > line_start {
            buffer.remove_range(line_start..self.cursor_char_idx);
            self.cursor_char_idx = line_start;
        } else if line_idx > 0 {
            // At start of line, delete the newline to join with previous
            buffer.remove_range(self.cursor_char_idx.saturating_sub(1)..self.cursor_char_idx);
            self.cursor_char_idx = self.cursor_char_idx.saturating_sub(1);
        }
    }

    /// Delete from cursor to end of line (Ctrl+K).
    pub fn delete_line_right(&mut self, buffer: &mut ComposerBuffer) {
        let len = buffer.len_chars();
        if self.cursor_char_idx >= len {
            return;
        }

        let line_idx = buffer.char_to_line(self.cursor_char_idx);
        let next_line_start = if line_idx + 1 < buffer.len_lines() {
            buffer.line_to_char(line_idx + 1)
        } else {
            len
        };

        // Don't delete the newline, just content up to it
        let content = buffer.get_content();
        let end = if next_line_start > 0 && content.chars().nth(next_line_start - 1) == Some('\n') {
            next_line_start - 1
        } else {
            next_line_start
        };

        if end > self.cursor_char_idx {
            buffer.remove_range(self.cursor_char_idx..end);
        } else if self.cursor_char_idx < len {
            // At end of line, delete the newline
            buffer.remove_char(self.cursor_char_idx);
        }
    }

    /// Insert a character at the cursor position.
    pub fn insert_char(&mut self, buffer: &mut ComposerBuffer, ch: char) {
        buffer.insert_char(self.cursor_char_idx, ch);
        self.cursor_char_idx += 1;
    }

    /// Insert a string at the cursor position.
    pub fn insert_str(&mut self, buffer: &mut ComposerBuffer, text: &str) {
        buffer.insert_str(self.cursor_char_idx, text);
        self.cursor_char_idx += text.chars().count();
    }

    /// Insert a newline at the cursor position.
    pub fn insert_newline(&mut self, buffer: &mut ComposerBuffer) {
        self.insert_char(buffer, '\n');
    }

    /// Clear the buffer and reset cursor.
    pub fn clear(&mut self, buffer: &mut ComposerBuffer) {
        buffer.clear();
        self.cursor_char_idx = 0;
        self.scroll_offset = 0;
    }

    /// Stash the current buffer content.
    pub fn stash_buffer(&mut self, buffer: &mut ComposerBuffer) {
        self.stash = Some((buffer.get_content(), buffer.blobs.clone()));
        buffer.clear();
        self.cursor_char_idx = 0;
    }

    /// Restore the stashed buffer content.
    pub fn restore_stash(&mut self, buffer: &mut ComposerBuffer) {
        if let Some((content, blobs)) = self.stash.take() {
            buffer.set_content(&content);
            buffer.blobs = blobs;
            self.cursor_char_idx = buffer.len_chars();
        }
    }

    pub fn has_stash(&self) -> bool {
        self.stash.is_some()
    }

    /// Invalidate cached width (call on terminal resize).
    /// Forces cursor position recalculation on next render.
    pub fn invalidate_width(&mut self) {
        self.last_width = 0;
    }

    /// Get the current scroll offset.
    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    /// Adjust scroll to keep cursor visible within the given height.
    /// Also clamps scroll_offset when content has shrunk.
    pub fn scroll_to_cursor(&mut self, visible_height: usize, total_lines: usize) {
        // Guard against zero height (very small terminal)
        if visible_height == 0 {
            self.scroll_offset = 0;
            return;
        }

        // Clamp scroll_offset so we don't show empty space below content
        // If content fits in viewport, no scrolling needed
        // Otherwise max_scroll positions last line at bottom of viewport
        let max_scroll = total_lines.saturating_sub(visible_height);
        if self.scroll_offset > max_scroll {
            self.scroll_offset = max_scroll;
        }

        let cursor_line = self.cursor_pos.1 as usize;

        // Cursor above viewport
        if cursor_line < self.scroll_offset {
            self.scroll_offset = cursor_line;
        }
        // Cursor below viewport
        else if cursor_line >= self.scroll_offset + visible_height {
            self.scroll_offset = cursor_line + 1 - visible_height;
        }
    }

    /// Calculate cursor visual position using word-wrap logic that matches Ratatui's Paragraph.
    /// Returns (cursor_x, cursor_y) relative to text area origin.
    pub fn calculate_cursor_pos(&mut self, buffer: &ComposerBuffer, width: usize) -> (u16, u16) {
        use unicode_width::UnicodeWidthChar;

        self.last_width = width;
        let content = buffer.get_content();
        // Clamp cursor to buffer bounds (safety for external buffer changes)
        self.cursor_char_idx = self.cursor_char_idx.min(buffer.len_chars());
        let cursor_idx = self.cursor_char_idx;

        if width == 0 || content.is_empty() {
            self.cursor_pos = (0, 0);
            return self.cursor_pos;
        }

        // Build visual line map using word-wrap
        let lines = build_visual_lines(&content, width);
        let (line_idx, col_in_line) = find_visual_line_and_col(&lines, cursor_idx);

        // Convert column (char offset) to x position (display width)
        let line_start = lines[line_idx].0;
        let x: usize = content
            .chars()
            .skip(line_start)
            .take(col_in_line)
            .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
            .sum();

        // Handle cursor at exact end of full line
        let y = if cursor_idx == buffer.len_chars() && x >= width {
            line_idx + 1
        } else {
            line_idx
        };
        let x = if cursor_idx == buffer.len_chars() && x >= width {
            0
        } else {
            x
        };

        self.cursor_pos = (x as u16, y as u16);
        self.cursor_pos
    }

    /// Get the number of visual lines the content occupies at the given width.
    /// Uses word-wrap to match Ratatui's Paragraph behavior.
    pub fn visual_line_count(&self, buffer: &ComposerBuffer, width: usize) -> usize {
        if buffer.is_empty() {
            return 1;
        }

        let content = buffer.get_content();
        let lines = build_visual_lines(&content, width);

        // Check if cursor at end would be on a new line
        let last_line = lines.last().unwrap();
        let last_line_width: usize = content
            .chars()
            .skip(last_line.0)
            .take(last_line.1 - last_line.0)
            .filter(|&c| c != '\n')
            .map(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(0))
            .sum();

        if width > 0 && last_line_width >= width {
            lines.len() + 1
        } else {
            lines.len()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cursor_movement() {
        let mut buf = ComposerBuffer::new();
        let mut state = ComposerState::new();

        buf.insert_str(0, "hello world");
        state.set_cursor(0, buf.len_chars());

        // Move right
        state.move_right(&buf);
        assert_eq!(state.cursor_char_idx(), 1);

        // Move to end
        state.move_to_end(&buf);
        assert_eq!(state.cursor_char_idx(), 11);

        // Move left
        state.move_left(&buf);
        assert_eq!(state.cursor_char_idx(), 10);

        // Move word left
        state.move_word_left(&buf);
        assert_eq!(state.cursor_char_idx(), 6); // "hello "
    }

    #[test]
    fn test_line_navigation() {
        let mut buf = ComposerBuffer::new();
        let mut state = ComposerState::new();

        buf.insert_str(0, "line1\nline2\nline3");
        state.set_cursor(8, buf.len_chars()); // Middle of "line2"

        // Move up
        assert!(state.move_up(&buf));
        assert_eq!(state.cursor_char_idx(), 2); // Same column in "line1"

        // Move down twice
        assert!(state.move_down(&buf));
        assert!(state.move_down(&buf));
        assert_eq!(buf.char_to_line(state.cursor_char_idx()), 2); // On line3
    }

    #[test]
    fn test_delete_operations() {
        let mut buf = ComposerBuffer::new();
        let mut state = ComposerState::new();

        buf.insert_str(0, "hello world");
        state.set_cursor(11, buf.len_chars());

        // Delete word (Ctrl+W)
        state.delete_word(&mut buf);
        assert_eq!(buf.get_content(), "hello ");

        // Delete to line start (Ctrl+U)
        state.delete_line_left(&mut buf);
        assert_eq!(buf.get_content(), "");
    }

    #[test]
    fn test_cursor_position_calculation() {
        let mut buf = ComposerBuffer::new();
        let mut state = ComposerState::new();

        // Test 1: Simple single line
        buf.insert_str(0, "hello");
        state.set_cursor(5, buf.len_chars());
        let pos = state.calculate_cursor_pos(&buf, 20);
        assert_eq!(pos, (5, 0), "cursor at end of 'hello' should be at (5, 0)");

        // Test 2: With explicit newline
        buf.clear();
        buf.insert_str(0, "abc\ndef");
        state.set_cursor(5, buf.len_chars()); // at 'e' in "def"
        let pos = state.calculate_cursor_pos(&buf, 20);
        assert_eq!(
            pos,
            (1, 1),
            "cursor at 'e' in second line should be at (1, 1)"
        );

        // Test 3: Wrapped line (width 10, content wraps)
        buf.clear();
        buf.insert_str(0, "0123456789ab"); // 12 chars
        state.set_cursor(12, buf.len_chars()); // at end
        let pos = state.calculate_cursor_pos(&buf, 10);
        // 0-9 on line 0 (10 chars), "ab" on line 1
        // cursor after "ab" should be at column 2, line 1
        assert_eq!(
            pos,
            (2, 1),
            "cursor after '0123456789ab' with width 10 should be at (2, 1)"
        );

        // Test 4: Cursor in middle of wrapped content
        buf.clear();
        buf.insert_str(0, "0123456789ab");
        state.set_cursor(11, buf.len_chars()); // at 'b'
        let pos = state.calculate_cursor_pos(&buf, 10);
        assert_eq!(pos, (1, 1), "cursor at 'b' should be at (1, 1)");

        // Test 5: Cursor at wrap point
        buf.clear();
        buf.insert_str(0, "0123456789ab");
        state.set_cursor(10, buf.len_chars()); // at 'a'
        let pos = state.calculate_cursor_pos(&buf, 10);
        assert_eq!(
            pos,
            (0, 1),
            "cursor at 'a' (first char of wrapped line) should be at (0, 1)"
        );

        // Test 6: Cursor at exact width boundary (no overflow yet)
        buf.clear();
        buf.insert_str(0, "0123456789"); // exactly 10 chars
        state.set_cursor(10, buf.len_chars()); // at end
        let pos = state.calculate_cursor_pos(&buf, 10);
        // Cursor after last char on a full line wraps to next line
        assert_eq!(
            pos,
            (0, 1),
            "cursor after exactly 10 chars with width 10 should wrap to (0, 1)"
        );

        // Test 7: Cursor just before width boundary
        buf.clear();
        buf.insert_str(0, "0123456789");
        state.set_cursor(9, buf.len_chars()); // at '9'
        let pos = state.calculate_cursor_pos(&buf, 10);
        assert_eq!(pos, (9, 0), "cursor at '9' should be at (9, 0)");

        // Test 8: Multiple wrapped lines
        buf.clear();
        buf.insert_str(0, "0123456789abcdefghij"); // 20 chars
        state.set_cursor(20, buf.len_chars()); // at end
        let pos = state.calculate_cursor_pos(&buf, 10);
        // Line 0: 0123456789 (10 chars)
        // Line 1: abcdefghij (10 chars)
        // Cursor after 'j' should wrap to line 2
        assert_eq!(
            pos,
            (0, 2),
            "cursor after 20 chars with width 10 should be at (0, 2)"
        );

        // Test 9: Cursor in middle of second wrapped line
        buf.clear();
        buf.insert_str(0, "0123456789abcdef");
        state.set_cursor(15, buf.len_chars()); // at 'f'
        let pos = state.calculate_cursor_pos(&buf, 10);
        assert_eq!(pos, (5, 1), "cursor at 'f' should be at (5, 1)");

        // Test 10: Multiline with explicit newlines - cursor on second line
        buf.clear();
        buf.insert_str(0, "abc\ndefghi");
        state.set_cursor(6, buf.len_chars()); // at 'f' (abc\nde|fghi)
        let pos = state.calculate_cursor_pos(&buf, 20);
        // 'abc' + newline = 4 chars, then 'de' = 2 more, so char 6 is 'f'
        // Visual: line 0 = "abc", line 1 = "defghi"
        // Cursor at 'f' should be at column 2 on line 1
        assert_eq!(
            pos,
            (2, 1),
            "cursor at 'f' after newline should be at (2, 1)"
        );

        // Test 11: Multiline - cursor right after newline
        buf.clear();
        buf.insert_str(0, "abc\ndef");
        state.set_cursor(4, buf.len_chars()); // at 'd' right after newline
        let pos = state.calculate_cursor_pos(&buf, 20);
        assert_eq!(
            pos,
            (0, 1),
            "cursor at 'd' right after newline should be at (0, 1)"
        );

        // Test 12: Multiple newlines
        buf.clear();
        buf.insert_str(0, "a\nb\nc");
        state.set_cursor(4, buf.len_chars()); // at 'c'
        let pos = state.calculate_cursor_pos(&buf, 20);
        assert_eq!(
            pos,
            (0, 2),
            "cursor at 'c' on third line should be at (0, 2)"
        );
    }

    #[test]
    fn test_visual_line_navigation_wrapped() {
        let mut buf = ComposerBuffer::new();
        let mut state = ComposerState::new();

        // "0123456789abcdef" at width 10 = two visual lines:
        // Line 0: "0123456789" (chars 0-9)
        // Line 1: "abcdef" (chars 10-15)
        buf.insert_str(0, "0123456789abcdef");

        // Initialize last_width by calculating cursor pos
        state.set_cursor(5, buf.len_chars()); // at '5' on line 0
        state.calculate_cursor_pos(&buf, 10);

        // Move down should go to line 1, column 5 -> 'f' (char 15)
        assert!(state.move_down(&buf));
        assert_eq!(
            state.cursor_char_idx(),
            15,
            "down from col 5 on line 0 should go to col 5 on line 1 (char 15)"
        );

        // Move up should go back to line 0, column 5 -> '5' (char 5)
        assert!(state.move_up(&buf));
        assert_eq!(
            state.cursor_char_idx(),
            5,
            "up from col 5 on line 1 should go back to col 5 on line 0 (char 5)"
        );
    }

    #[test]
    fn test_visual_line_navigation_shorter_line() {
        let mut buf = ComposerBuffer::new();
        let mut state = ComposerState::new();

        // "0123456789abc" at width 10:
        // Line 0: "0123456789" (chars 0-9)
        // Line 1: "abc" (chars 10-12)
        buf.insert_str(0, "0123456789abc");

        // Start at column 8 on line 0
        state.set_cursor(8, buf.len_chars());
        state.calculate_cursor_pos(&buf, 10);

        // Move down - line 1 only has 3 chars, so cursor should go to end (col 2)
        assert!(state.move_down(&buf));
        assert_eq!(
            state.cursor_char_idx(),
            12,
            "down from col 8 should clamp to end of shorter line 1 (char 12)"
        );

        // Move up should preserve the clamped column
        assert!(state.move_up(&buf));
        assert_eq!(
            state.cursor_char_idx(),
            2,
            "up from col 2 should go to col 2 on line 0 (char 2)"
        );
    }

    #[test]
    fn test_visual_line_navigation_with_newlines() {
        let mut buf = ComposerBuffer::new();
        let mut state = ComposerState::new();

        // "abc\n0123456789def" at width 10:
        // Line 0: "abc\n" (chars 0-3)
        // Line 1: "0123456789" (chars 4-13)
        // Line 2: "def" (chars 14-16)
        buf.insert_str(0, "abc\n0123456789def");

        // Start at 'b' (char 1) on line 0
        state.set_cursor(1, buf.len_chars());
        state.calculate_cursor_pos(&buf, 10);

        // Move down to line 1, col 1 -> '1' (char 5)
        assert!(state.move_down(&buf));
        assert_eq!(
            state.cursor_char_idx(),
            5,
            "down from 'b' should go to '1' (char 5)"
        );

        // Move down again to line 2, col 1 -> 'e' (char 15)
        assert!(state.move_down(&buf));
        assert_eq!(
            state.cursor_char_idx(),
            15,
            "down from '1' should go to 'e' (char 15)"
        );

        // Can't move down further
        assert!(
            !state.move_down(&buf),
            "should not be able to move down from last line"
        );
    }

    #[test]
    fn test_visual_line_navigation_boundaries() {
        let mut buf = ComposerBuffer::new();
        let mut state = ComposerState::new();

        // Single visual line - can't move up or down
        buf.insert_str(0, "hello");
        state.set_cursor(2, buf.len_chars());
        state.calculate_cursor_pos(&buf, 20);

        assert!(
            !state.move_up(&buf),
            "should not move up from first/only line"
        );
        assert!(
            !state.move_down(&buf),
            "should not move down from last/only line"
        );
    }

    #[test]
    fn test_visual_line_navigation_empty() {
        let buf = ComposerBuffer::new();
        let mut state = ComposerState::new();

        state.calculate_cursor_pos(&buf, 10);

        assert!(!state.move_up(&buf), "should not move up in empty buffer");
        assert!(
            !state.move_down(&buf),
            "should not move down in empty buffer"
        );
    }

    #[test]
    fn test_visual_line_count_full_line() {
        let mut buf = ComposerBuffer::new();
        let mut state = ComposerState::new();

        // Exactly filling width - cursor at end should be on "next" line
        buf.insert_str(0, "0123456789"); // 10 chars
        state.set_cursor(10, buf.len_chars()); // at end

        let cursor_pos = state.calculate_cursor_pos(&buf, 10);
        let line_count = state.visual_line_count(&buf, 10);

        // Cursor wraps to next line
        assert_eq!(
            cursor_pos,
            (0, 1),
            "cursor at end of full line wraps to (0, 1)"
        );
        // Line count must include the cursor's line
        assert_eq!(
            line_count, 2,
            "visual_line_count must account for cursor-at-end"
        );
        // Cursor line must be < total lines (required for scroll logic)
        assert!(
            (cursor_pos.1 as usize) < line_count,
            "cursor line {} must be < total lines {}",
            cursor_pos.1,
            line_count
        );
    }

    #[test]
    fn test_visual_line_count_not_full() {
        let mut buf = ComposerBuffer::new();
        let state = ComposerState::new();

        // Not filling width - no extra line needed
        buf.insert_str(0, "012345678"); // 9 chars at width 10
        let line_count = state.visual_line_count(&buf, 10);
        assert_eq!(line_count, 1, "partial line should be 1");

        // With trailing newline
        buf.clear();
        buf.insert_str(0, "abc\n");
        let line_count = state.visual_line_count(&buf, 10);
        assert_eq!(line_count, 2, "line with newline should be 2");
    }

    /// Helper to compute where each character would be rendered (matching render_char_wrapped logic)
    fn compute_char_positions(content: &str, width: usize) -> Vec<(usize, usize)> {
        let mut positions = Vec::new();
        let mut x = 0usize;
        let mut y = 0usize;

        for c in content.chars() {
            if c == '\n' {
                positions.push((x, y)); // newline is "at" end of line
                x = 0;
                y += 1;
            } else {
                let char_width = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
                if x + char_width > width {
                    x = 0;
                    y += 1;
                }
                positions.push((x, y));
                x += char_width;
            }
        }
        positions
    }

    #[test]
    fn test_render_cursor_alignment() {
        let mut buf = ComposerBuffer::new();
        let mut state = ComposerState::new();
        let width = 10;

        // Test wrapped content
        buf.insert_str(0, "0123456789abcdef"); // 16 chars, wraps at 10
        let char_positions = compute_char_positions(&buf.get_content(), width);

        // Verify cursor position for each character index
        for (i, &(char_x, char_y)) in char_positions.iter().enumerate() {
            state.set_cursor(i, buf.len_chars());
            let (cursor_x, cursor_y) = state.calculate_cursor_pos(&buf, width);

            assert_eq!(
                (cursor_x as usize, cursor_y as usize),
                (char_x, char_y),
                "Cursor at char {} should be at ({}, {}), but got ({}, {})",
                i,
                char_x,
                char_y,
                cursor_x,
                cursor_y
            );
        }

        // Also verify cursor AFTER last char
        let last_char = char_positions.last().unwrap();
        state.set_cursor(buf.len_chars(), buf.len_chars());
        let (cursor_x, cursor_y) = state.calculate_cursor_pos(&buf, width);

        // After 'f' at (5, 1), cursor should be at (6, 1)
        assert_eq!(
            (cursor_x as usize, cursor_y as usize),
            (last_char.0 + 1, last_char.1),
            "Cursor after last char should be one position after it"
        );
    }
}
