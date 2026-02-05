use super::buffer::ComposerBuffer;
use super::visual_lines::{build_visual_lines, find_visual_line_and_col};
use unicode_segmentation::UnicodeSegmentation;

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
    /// Preferred column for vertical navigation (preserved across short lines).
    /// Set on up/down movement, cleared on horizontal movement or explicit cursor set.
    preferred_col: Option<usize>,
}

impl ComposerState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn cursor_char_idx(&self) -> usize {
        self.cursor_char_idx
    }

    pub fn set_cursor(&mut self, idx: usize, max_len: usize) {
        self.cursor_char_idx = idx.min(max_len);
        self.preferred_col = None;
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
    /// Uses `last_width` from previous render; falls back to logical line start if width unknown.
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
    /// Uses `last_width` from previous render; falls back to logical line end if width unknown.
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

        self.preferred_col = None;

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

        self.preferred_col = None;

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
    /// Uses `last_width` from previous render; falls back to logical line if width unknown.
    pub fn move_up(&mut self, buffer: &ComposerBuffer) -> bool {
        if self.last_width == 0 {
            return self.move_up_logical(buffer);
        }
        self.move_up_visual(buffer, self.last_width)
    }

    /// Move cursor down one visual line (including wrapped lines).
    /// Uses `last_width` from previous render; falls back to logical line if width unknown.
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

        // Use preferred column if set, otherwise use current column and remember it
        let target_col = self.preferred_col.unwrap_or(col_in_line);
        if self.preferred_col.is_none() {
            self.preferred_col = Some(col_in_line);
        }

        // Move to target column on previous visual line (clamped to line length)
        let prev_line = &lines[cur_line - 1];
        let prev_line_len = prev_line.1 - prev_line.0;
        let actual_col = target_col.min(prev_line_len);
        self.cursor_char_idx = prev_line.0 + actual_col;
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

        // Use preferred column if set, otherwise use current column and remember it
        let target_col = self.preferred_col.unwrap_or(col_in_line);
        if self.preferred_col.is_none() {
            self.preferred_col = Some(col_in_line);
        }

        // Move to target column on next visual line (clamped to line length)
        let next_line = &lines[cur_line + 1];
        let next_line_len = next_line.1 - next_line.0;
        let actual_col = target_col.min(next_line_len);
        self.cursor_char_idx = next_line.0 + actual_col;
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
        let prev_line_len = line_start.saturating_sub(prev_line_start).saturating_sub(1);

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

    #[must_use]
    pub fn has_stash(&self) -> bool {
        self.stash.is_some()
    }

    /// Invalidate cached width (call on terminal resize).
    /// Forces cursor position recalculation on next render.
    pub fn invalidate_width(&mut self) {
        self.last_width = 0;
    }

    /// Get the current scroll offset.
    #[must_use]
    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    /// Adjust scroll to keep cursor visible within the given height.
    /// Also clamps `scroll_offset` when content has shrunk.
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
    /// Returns (`cursor_x`, `cursor_y`) relative to text area origin.
    pub fn calculate_cursor_pos(&mut self, buffer: &ComposerBuffer, width: usize) -> (u16, u16) {
        let content = buffer.get_content();
        let lines = build_visual_lines(&content, width);
        self.calculate_cursor_pos_with(&content, &lines, buffer.len_chars(), width)
    }

    /// Calculate cursor position from precomputed content and visual lines.
    pub fn calculate_cursor_pos_with(
        &mut self,
        content: &str,
        lines: &[(usize, usize)],
        len_chars: usize,
        width: usize,
    ) -> (u16, u16) {
        use unicode_width::UnicodeWidthChar;

        self.last_width = width;
        self.cursor_char_idx = self.cursor_char_idx.min(len_chars);
        let cursor_idx = self.cursor_char_idx;

        if width == 0 || content.is_empty() {
            self.cursor_pos = (0, 0);
            return self.cursor_pos;
        }

        let (line_idx, col_in_line) = find_visual_line_and_col(lines, cursor_idx);

        let line_start = lines.get(line_idx).map_or(0, |l| l.0);
        let x: usize = content
            .chars()
            .skip(line_start)
            .take(col_in_line)
            .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
            .sum();

        let y = if cursor_idx == len_chars && x >= width {
            line_idx + 1
        } else {
            line_idx
        };
        let x = if cursor_idx == len_chars && x >= width {
            0
        } else {
            x
        };

        #[allow(clippy::cast_possible_truncation)] // Terminal cursor fits in u16
        {
            self.cursor_pos = (x as u16, y as u16);
        }
        self.cursor_pos
    }

    /// Get the number of visual lines the content occupies at the given width.
    /// Uses word-wrap to match Ratatui's Paragraph behavior.
    #[must_use]
    pub fn visual_line_count(&self, buffer: &ComposerBuffer, width: usize) -> usize {
        let content = buffer.get_content();
        let lines = build_visual_lines(&content, width);
        Self::visual_line_count_with(&content, &lines, width)
    }

    /// Get visual line count from precomputed content and visual lines.
    #[must_use]
    pub fn visual_line_count_with(content: &str, lines: &[(usize, usize)], width: usize) -> usize {
        if content.is_empty() {
            return 1;
        }

        let last_line = lines.last().unwrap_or(&(0, 0));
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
