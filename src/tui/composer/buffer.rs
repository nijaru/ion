use ropey::Rope;

/// Core text buffer for the Composer widget.
///
/// Uses a Rope (via `ropey`) for efficient text manipulation and
/// stores large paste blobs separately to keep the primary buffer light.
#[derive(Debug, Clone, Default)]
pub struct ComposerBuffer {
    rope: Rope,
    /// Large pastes (>5 lines or >500 chars) are stored here.
    /// The buffer contains a placeholder like `[Pasted text #1]`.
    pub blobs: Vec<String>,
}

impl ComposerBuffer {
    /// Create a new empty buffer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a character at the given character index, clamped to buffer boundaries.
    pub fn insert_char(&mut self, char_idx: usize, ch: char) {
        let idx = char_idx.min(self.rope.len_chars());
        self.rope.insert_char(idx, ch);
    }

    /// Insert a string at the given character index, clamped to buffer boundaries.
    pub fn insert_str(&mut self, char_idx: usize, text: &str) {
        let idx = char_idx.min(self.rope.len_chars());
        self.rope.insert(idx, text);
    }

    /// Delete a character at the given character index.
    pub fn remove_char(&mut self, char_idx: usize) {
        if char_idx < self.rope.len_chars() {
            self.rope.remove(char_idx..=char_idx);
        }
    }

    /// Remove a range of characters.
    pub fn remove_range(&mut self, range: std::ops::Range<usize>) {
        let start = range.start.min(self.rope.len_chars());
        let end = range.end.min(self.rope.len_chars());
        if start < end {
            self.rope.remove(start..end);
        }
    }

    /// Get the total number of characters in the buffer.
    #[must_use]
    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    /// Check if the buffer is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rope.len_chars() == 0
    }

    /// Get the number of lines in the buffer.
    #[must_use]
    pub fn len_lines(&self) -> usize {
        self.rope.len_lines()
    }

    /// Clear the buffer and blobs.
    pub fn clear(&mut self) {
        self.rope = Rope::new();
        self.blobs.clear();
    }

    /// Returns the full content of the buffer as a String.
    #[must_use]
    pub fn get_content(&self) -> String {
        self.rope.to_string()
    }

    /// Set the buffer content from a string.
    pub fn set_content(&mut self, text: &str) {
        self.rope = Rope::from_str(text);
        self.blobs.clear();
    }

    /// Get a reference to the underlying Rope.
    #[must_use]
    pub fn rope(&self) -> &Rope {
        &self.rope
    }

    /// Get the character index at the start of a line.
    #[must_use]
    pub fn line_to_char(&self, line_idx: usize) -> usize {
        self.rope
            .line_to_char(line_idx.min(self.rope.len_lines().saturating_sub(1)))
    }

    /// Get the line index containing a character position.
    #[must_use]
    pub fn char_to_line(&self, char_idx: usize) -> usize {
        self.rope.char_to_line(char_idx.min(self.rope.len_chars()))
    }

    /// Store a large paste and return its placeholder index.
    pub fn push_blob(&mut self, content: String) -> usize {
        self.blobs.push(content);
        self.blobs.len()
    }

    /// Resolve all placeholders in the given text using the stored blobs.
    #[must_use]
    pub fn resolve_content(&self) -> String {
        let mut final_content = self.get_content();

        for (i, blob) in self.blobs.iter().enumerate() {
            // Use unique delimiter (\x1f = Unit Separator) that can't be accidentally typed
            // but still allows the visible text to be human-readable
            let placeholder = Self::internal_placeholder(i + 1);
            final_content = final_content.replace(&placeholder, blob);
        }

        final_content
    }

    /// Get the internal placeholder for a blob index (1-indexed).
    /// Uses Unit Separator (\x1f) as invisible delimiter to prevent collision
    /// with user-typed text. The visible portion is "[Pasted text #N]".
    #[must_use]
    pub fn internal_placeholder(blob_idx: usize) -> String {
        format!("\x1f[Pasted text #{blob_idx}]\x1f")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_editing() {
        let mut buf = ComposerBuffer::new();
        buf.insert_str(0, "Hello");
        buf.insert_char(5, '!');
        assert_eq!(buf.get_content(), "Hello!");

        buf.remove_char(5);
        assert_eq!(buf.get_content(), "Hello");

        buf.insert_str(0, "Ion: ");
        assert_eq!(buf.get_content(), "Ion: Hello");
    }

    #[test]
    fn test_blobs() {
        let mut buf = ComposerBuffer::new();
        let blob_idx = buf.push_blob("Large content".to_string());
        // Insert using internal placeholder (what the system stores)
        let placeholder = ComposerBuffer::internal_placeholder(blob_idx);
        buf.insert_str(0, &format!("Context: {}", placeholder));

        assert_eq!(buf.resolve_content(), "Context: Large content");
    }

    #[test]
    fn test_blob_placeholder_collision_protection() {
        let mut buf = ComposerBuffer::new();
        buf.push_blob("Actual blob content".to_string());
        // User types what looks like a placeholder - should NOT be replaced
        // because we use invisible delimiters (\x1f) in the internal format
        buf.insert_str(0, "User typed: [Pasted text #1] literally");

        // The user-typed text should remain unchanged
        assert_eq!(
            buf.resolve_content(),
            "User typed: [Pasted text #1] literally"
        );
    }

    #[test]
    fn test_bounds_safety() {
        let mut buf = ComposerBuffer::new();
        buf.insert_char(100, 'a'); // Should not panic, clamps to 0
        assert_eq!(buf.get_content(), "a");
        buf.insert_str(100, "bc"); // Should not panic, clamps to 1
        assert_eq!(buf.get_content(), "abc");
    }

    #[test]
    fn test_multiline() {
        let mut buf = ComposerBuffer::new();
        buf.insert_str(0, "line1\nline2\nline3");
        assert_eq!(buf.len_lines(), 3);
        assert_eq!(buf.line_to_char(1), 6); // "line1\n" = 6 chars
        assert_eq!(buf.char_to_line(7), 1); // char 7 is in line 1
    }
}
