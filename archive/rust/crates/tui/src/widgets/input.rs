//! Input widget — multiline text input with terminal keybindings.
//!
//! State is owned externally in the app model (`InputState`). The `Input`
//! widget is a thin renderer. Key handling lives on `InputState::handle_key`.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::{
    buffer::Buffer,
    event::{KeyCode, KeyEvent, KeyModifiers},
    geometry::Rect,
    layout::LayoutStyle,
    style::Style,
    widgets::{Element, IntoElement, Renderable, WidgetId},
};

// ── InputAction ───────────────────────────────────────────────────────────────

/// The outcome of `InputState::handle_key`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputAction {
    /// Content changed.
    Changed,
    /// Cursor moved, content unchanged.
    Navigated,
    /// User pressed Enter — caller should submit and clear.
    Submit,
    /// User pressed Shift+Enter or Alt+Enter — insert a newline.
    Newline,
    /// Key not handled by the input widget.
    Unhandled,
}

// ── InputState ────────────────────────────────────────────────────────────────

/// Owned input state — store this in your app model.
#[derive(Debug, Clone, Default)]
pub struct InputState {
    /// Lines of content. Always at least one (possibly empty) line.
    lines: Vec<String>,
    /// Cursor position: (line_index, grapheme_index_within_line).
    cursor: (usize, usize),
    /// Kill ring — stores text killed with Ctrl+K / Ctrl+U.
    kill_buffer: String,
    /// Input history (oldest first).
    history: Vec<String>,
    /// Current position in history (`None` = editing live buffer).
    history_pos: Option<usize>,
    /// Draft saved when entering history navigation.
    history_draft: Option<String>,
}

impl InputState {
    pub fn new() -> Self {
        Self {
            lines: vec![String::new()],
            ..Default::default()
        }
    }

    // ── Accessors ─────────────────────────────────────────────────────────────

    /// All lines joined with newlines.
    pub fn value(&self) -> String {
        self.lines.join("\n")
    }

    /// Set content from a string (newlines split into lines).
    pub fn set_value(&mut self, s: &str) {
        self.lines = s.split('\n').map(String::from).collect();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        // Move cursor to end.
        let last = self.lines.len() - 1;
        let col = grapheme_len(&self.lines[last]);
        self.cursor = (last, col);
        self.history_pos = None;
        self.history_draft = None;
    }

    /// Clear all content and reset cursor.
    pub fn clear(&mut self) {
        self.lines = vec![String::new()];
        self.cursor = (0, 0);
        self.history_pos = None;
        self.history_draft = None;
    }

    /// True if content is empty.
    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    /// Number of lines.
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Cursor position (line, grapheme column).
    pub fn cursor(&self) -> (usize, usize) {
        self.cursor
    }

    /// Append an entry to the history ring.
    pub fn push_history(&mut self, entry: String) {
        if entry.is_empty() {
            return;
        }
        // De-duplicate adjacent identical entries.
        if self.history.last().map(|s| s.as_str()) == Some(entry.as_str()) {
            return;
        }
        self.history.push(entry);
    }

    // ── Key handling ──────────────────────────────────────────────────────────

    /// Process a key event. Returns the action that occurred.
    pub fn handle_key(&mut self, key: &KeyEvent) -> InputAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CTRL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        match key.code {
            // ── Submit / newline ───────────────────────────────────────────────
            KeyCode::Enter if shift || alt => {
                self.insert_newline();
                InputAction::Newline
            }
            KeyCode::Enter => InputAction::Submit,

            // ── Character insert ───────────────────────────────────────────────
            KeyCode::Char(c) if ctrl => match c {
                'a' | 'A' => {
                    self.move_line_start();
                    InputAction::Navigated
                }
                'e' | 'E' => {
                    self.move_line_end();
                    InputAction::Navigated
                }
                'u' | 'U' => {
                    self.kill_to_line_start();
                    InputAction::Changed
                }
                'k' | 'K' => {
                    self.kill_to_line_end();
                    InputAction::Changed
                }
                'y' | 'Y' => {
                    self.yank();
                    InputAction::Changed
                }
                'w' | 'W' | 'h' | 'H' => {
                    self.delete_word_back();
                    InputAction::Changed
                }
                _ => InputAction::Unhandled,
            },

            KeyCode::Char(c) if alt => match c {
                'b' | 'B' => {
                    self.move_word_back();
                    InputAction::Navigated
                }
                'f' | 'F' => {
                    self.move_word_forward();
                    InputAction::Navigated
                }
                'd' | 'D' => {
                    self.delete_word_forward();
                    InputAction::Changed
                }
                _ => InputAction::Unhandled,
            },

            KeyCode::Char(c) => {
                self.insert_char(c);
                // Exit history navigation on any insert.
                self.history_pos = None;
                self.history_draft = None;
                InputAction::Changed
            }

            // ── Deletion ───────────────────────────────────────────────────────
            KeyCode::Backspace => {
                self.delete_back();
                InputAction::Changed
            }
            KeyCode::Delete => {
                self.delete_forward();
                InputAction::Changed
            }

            // ── Cursor movement ────────────────────────────────────────────────
            KeyCode::Left if ctrl => {
                self.move_word_back();
                InputAction::Navigated
            }
            KeyCode::Right if ctrl => {
                self.move_word_forward();
                InputAction::Navigated
            }
            KeyCode::Left => {
                self.move_left();
                InputAction::Navigated
            }
            KeyCode::Right => {
                self.move_right();
                InputAction::Navigated
            }
            KeyCode::Up => {
                if self.cursor.0 > 0 {
                    self.move_up();
                    InputAction::Navigated
                } else {
                    self.history_prev();
                    InputAction::Changed
                }
            }
            KeyCode::Down => {
                let last_line = self.lines.len() - 1;
                if self.cursor.0 < last_line {
                    self.move_down();
                    InputAction::Navigated
                } else {
                    self.history_next();
                    InputAction::Changed
                }
            }
            KeyCode::Home => {
                self.move_line_start();
                InputAction::Navigated
            }
            KeyCode::End => {
                self.move_line_end();
                InputAction::Navigated
            }

            _ => InputAction::Unhandled,
        }
    }

    // ── Insert helpers ────────────────────────────────────────────────────────

    fn insert_char(&mut self, c: char) {
        let (row, col) = self.cursor;
        let byte_pos = grapheme_to_byte(&self.lines[row], col);
        self.lines[row].insert(byte_pos, c);
        self.cursor.1 += 1;
    }

    fn insert_newline(&mut self) {
        let (row, col) = self.cursor;
        let byte_pos = grapheme_to_byte(&self.lines[row], col);
        let tail = self.lines[row].split_off(byte_pos);
        self.lines.insert(row + 1, tail);
        self.cursor = (row + 1, 0);
    }

    /// Insert a multi-character string at the cursor position.
    /// Handles embedded newlines by splitting into multiple lines.
    pub fn insert_text(&mut self, text: &str) {
        for c in text.chars() {
            match c {
                '\n' => self.insert_newline(),
                '\r' => {} // strip carriage returns
                c => self.insert_char(c),
            }
        }
        self.history_pos = None;
        self.history_draft = None;
    }

    // ── Deletion helpers ──────────────────────────────────────────────────────

    fn delete_back(&mut self) {
        let (row, col) = self.cursor;
        if col == 0 {
            if row == 0 {
                return; // Nothing to delete.
            }
            // Merge with previous line.
            let line = self.lines.remove(row);
            let prev_col = grapheme_len(&self.lines[row - 1]);
            self.lines[row - 1].push_str(&line);
            self.cursor = (row - 1, prev_col);
        } else {
            let byte_pos = grapheme_to_byte(&self.lines[row], col);
            let prev_byte = prev_grapheme_boundary(&self.lines[row], byte_pos);
            self.lines[row].drain(prev_byte..byte_pos);
            self.cursor.1 -= 1;
        }
    }

    fn delete_forward(&mut self) {
        let (row, col) = self.cursor;
        let line_len = grapheme_len(&self.lines[row]);
        if col >= line_len {
            if row + 1 < self.lines.len() {
                // Merge with next line.
                let next = self.lines.remove(row + 1);
                self.lines[row].push_str(&next);
            }
        } else {
            let byte_pos = grapheme_to_byte(&self.lines[row], col);
            let next_byte = next_grapheme_boundary(&self.lines[row], byte_pos);
            self.lines[row].drain(byte_pos..next_byte);
        }
    }

    fn kill_to_line_end(&mut self) {
        let (row, col) = self.cursor;
        let line_len = grapheme_len(&self.lines[row]);
        if col >= line_len {
            // At end of line — kill the newline (merge next line).
            if row + 1 < self.lines.len() {
                let next = self.lines.remove(row + 1);
                self.kill_buffer = "\n".to_string();
                self.lines[row].push_str(&next);
            }
        } else {
            let byte_pos = grapheme_to_byte(&self.lines[row], col);
            let killed = self.lines[row].split_off(byte_pos);
            self.kill_buffer = killed;
        }
    }

    fn kill_to_line_start(&mut self) {
        let (row, col) = self.cursor;
        if col == 0 {
            return;
        }
        let byte_pos = grapheme_to_byte(&self.lines[row], col);
        self.kill_buffer = self.lines[row][..byte_pos].to_string();
        self.lines[row].drain(..byte_pos);
        self.cursor.1 = 0;
    }

    fn yank(&mut self) {
        if self.kill_buffer.is_empty() {
            return;
        }
        let yanked = self.kill_buffer.clone();
        for c in yanked.chars() {
            if c == '\n' {
                self.insert_newline();
            } else {
                self.insert_char(c);
            }
        }
    }

    // ── Word operations ───────────────────────────────────────────────────────

    fn move_word_back(&mut self) {
        let (row, col) = self.cursor;
        if col == 0 {
            if row > 0 {
                self.cursor = (row - 1, grapheme_len(&self.lines[row - 1]));
            }
            return;
        }
        let new_col = prev_word_boundary(&self.lines[row], col);
        self.cursor.1 = new_col;
    }

    fn move_word_forward(&mut self) {
        let (row, col) = self.cursor;
        let line_len = grapheme_len(&self.lines[row]);
        if col >= line_len {
            if row + 1 < self.lines.len() {
                self.cursor = (row + 1, 0);
            }
            return;
        }
        let new_col = next_word_boundary(&self.lines[row], col);
        self.cursor.1 = new_col;
    }

    fn delete_word_back(&mut self) {
        let (row, col) = self.cursor;
        if col == 0 {
            if row > 0 {
                // Merge with previous line.
                let line = self.lines.remove(row);
                let prev_col = grapheme_len(&self.lines[row - 1]);
                self.lines[row - 1].push_str(&line);
                self.cursor = (row - 1, prev_col);
            }
            return;
        }
        let target = prev_word_boundary(&self.lines[row], col);
        let start_byte = grapheme_to_byte(&self.lines[row], target);
        let end_byte = grapheme_to_byte(&self.lines[row], col);
        self.lines[row].drain(start_byte..end_byte);
        self.cursor.1 = target;
    }

    fn delete_word_forward(&mut self) {
        let (row, col) = self.cursor;
        let line_len = grapheme_len(&self.lines[row]);
        if col >= line_len {
            if row + 1 < self.lines.len() {
                let next = self.lines.remove(row + 1);
                self.lines[row].push_str(&next);
            }
            return;
        }
        let target = next_word_boundary(&self.lines[row], col);
        let start_byte = grapheme_to_byte(&self.lines[row], col);
        let end_byte = grapheme_to_byte(&self.lines[row], target);
        self.lines[row].drain(start_byte..end_byte);
    }

    // ── Cursor movement ───────────────────────────────────────────────────────

    fn move_left(&mut self) {
        let (row, col) = self.cursor;
        if col > 0 {
            self.cursor.1 -= 1;
        } else if row > 0 {
            self.cursor = (row - 1, grapheme_len(&self.lines[row - 1]));
        }
    }

    fn move_right(&mut self) {
        let (row, col) = self.cursor;
        let line_len = grapheme_len(&self.lines[row]);
        if col < line_len {
            self.cursor.1 += 1;
        } else if row + 1 < self.lines.len() {
            self.cursor = (row + 1, 0);
        }
    }

    fn move_up(&mut self) {
        let (row, col) = self.cursor;
        if row > 0 {
            let new_col = col.min(grapheme_len(&self.lines[row - 1]));
            self.cursor = (row - 1, new_col);
        }
    }

    fn move_down(&mut self) {
        let (row, col) = self.cursor;
        if row + 1 < self.lines.len() {
            let new_col = col.min(grapheme_len(&self.lines[row + 1]));
            self.cursor = (row + 1, new_col);
        }
    }

    fn move_line_start(&mut self) {
        self.cursor.1 = 0;
    }

    fn move_line_end(&mut self) {
        let row = self.cursor.0;
        self.cursor.1 = grapheme_len(&self.lines[row]);
    }

    // ── History navigation ────────────────────────────────────────────────────

    fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let new_pos = match self.history_pos {
            None => {
                // Save current draft before entering history.
                self.history_draft = Some(self.value());
                self.history.len() - 1
            }
            Some(0) => return, // Already at oldest.
            Some(p) => p - 1,
        };
        let value = self.history[new_pos].clone();
        let draft = self.history_draft.take(); // preserve across set_value
        self.set_value(&value);
        self.history_pos = Some(new_pos);
        self.history_draft = draft;
    }

    fn history_next(&mut self) {
        let Some(pos) = self.history_pos else { return };
        if pos + 1 < self.history.len() {
            let new_pos = pos + 1;
            let value = self.history[new_pos].clone();
            let draft = self.history_draft.take();
            self.set_value(&value);
            self.history_pos = Some(new_pos);
            self.history_draft = draft;
        } else {
            // Return to draft.
            let draft = self.history_draft.take().unwrap_or_default();
            self.set_value(&draft);
            self.history_pos = None;
        }
    }
}

// ── Input widget ──────────────────────────────────────────────────────────────

/// Renders an `InputState` into a buffer region.
///
/// The widget itself is stateless — state is owned by the caller via
/// `InputState`. To render a cursor, use `cursor_position()` to get the
/// terminal cell and move the real terminal cursor after rendering.
pub struct Input<'a> {
    state: &'a InputState,
    placeholder: Option<&'a str>,
    style: Style,
    placeholder_style: Style,
    layout_style: LayoutStyle,
}

impl<'a> Input<'a> {
    pub fn new(state: &'a InputState) -> Self {
        Self {
            state,
            placeholder: None,
            style: Style::default(),
            placeholder_style: Style::default(),
            layout_style: LayoutStyle::default(),
        }
    }

    pub fn placeholder(mut self, text: &'a str) -> Self {
        self.placeholder = Some(text);
        self
    }

    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    pub fn placeholder_style(mut self, style: Style) -> Self {
        self.placeholder_style = style;
        self
    }

    pub fn width(mut self, w: crate::layout::Dimension) -> Self {
        self.layout_style.size.width = w;
        self
    }

    pub fn height(mut self, h: crate::layout::Dimension) -> Self {
        self.layout_style.size.height = h;
        self
    }

    /// Return the terminal-relative cursor position (col, row) within `area`,
    /// so the caller can position the real terminal cursor after rendering.
    ///
    /// Returns `None` if the cursor is outside the visible area.
    pub fn cursor_position(&self, area: Rect) -> Option<(u16, u16)> {
        let (line_idx, glyph_col) = self.state.cursor;
        if line_idx >= area.height as usize {
            return None;
        }
        let line = &self.state.lines[line_idx];
        let display_col: u16 = graphemes_display_width(line, glyph_col) as u16;
        let term_col = area.x.saturating_add(display_col);
        let term_row = area.y.saturating_add(line_idx as u16);
        if term_col < area.x + area.width && term_row < area.y + area.height {
            Some((term_col, term_row))
        } else {
            None
        }
    }
}

/// Snapshot-capable render — delegates to the inner `Renderable`.
struct InputRenderable {
    lines: Vec<String>,
    placeholder: Option<String>,
    style: Style,
    placeholder_style: Style,
}

impl Renderable for InputRenderable {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }

        let show_placeholder =
            self.placeholder.is_some() && self.lines.iter().all(|l| l.is_empty());

        if show_placeholder {
            let ph = self.placeholder.as_deref().unwrap_or("");
            let visible: String = ph.chars().take(area.width as usize).collect();
            for (i, g) in visible.graphemes(true).enumerate() {
                buf.set_symbol(area.x + i as u16, area.y, g, self.placeholder_style);
            }
            return;
        }

        for (row_offset, line) in self.lines.iter().enumerate() {
            let row = area.y + row_offset as u16;
            if row >= area.y + area.height {
                break;
            }
            let mut col = area.x;
            let max_col = area.x + area.width;
            for g in line.graphemes(true) {
                let w = g.width();
                if col + w as u16 > max_col {
                    break;
                }
                buf.set_symbol(col, row, g, self.style);
                col += w as u16;
            }
        }
    }
}

impl<'a> IntoElement for Input<'a> {
    fn into_element(self) -> Element {
        let renderable = InputRenderable {
            lines: self.state.lines.clone(),
            placeholder: self.placeholder.map(str::to_owned),
            style: self.style,
            placeholder_style: self.placeholder_style,
        };
        Element {
            id: WidgetId::new(),
            inner: Box::new(renderable),
            layout_style: self.layout_style,
            children: Vec::new(),
        }
    }
}

// ── Unicode utilities ─────────────────────────────────────────────────────────

/// Number of grapheme clusters in `s`.
fn grapheme_len(s: &str) -> usize {
    s.graphemes(true).count()
}

/// Byte offset of the `n`-th grapheme cluster boundary (0-indexed).
fn grapheme_to_byte(s: &str, n: usize) -> usize {
    s.grapheme_indices(true)
        .nth(n)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

/// Display width of the first `n` grapheme clusters.
fn graphemes_display_width(s: &str, n: usize) -> usize {
    s.graphemes(true).take(n).map(|g| g.width()).sum()
}

/// Byte offset just before the grapheme cluster ending at `byte_pos`.
fn prev_grapheme_boundary(s: &str, byte_pos: usize) -> usize {
    s.grapheme_indices(true)
        .rev()
        .find(|(i, g)| i + g.len() == byte_pos)
        .map(|(i, _)| i)
        .unwrap_or(0)
}

/// Byte offset just after the grapheme cluster starting at `byte_pos`.
fn next_grapheme_boundary(s: &str, byte_pos: usize) -> usize {
    s.grapheme_indices(true)
        .find(|(i, _)| *i == byte_pos)
        .map(|(i, g)| i + g.len())
        .unwrap_or(s.len())
}

/// Grapheme index of the previous word boundary (whitespace-delimited).
fn prev_word_boundary(s: &str, col: usize) -> usize {
    let gs: Vec<&str> = s.graphemes(true).collect();
    let mut i = col;
    // Skip whitespace to the left.
    while i > 0 && gs[i - 1].trim().is_empty() {
        i -= 1;
    }
    // Skip non-whitespace.
    while i > 0 && !gs[i - 1].trim().is_empty() {
        i -= 1;
    }
    i
}

/// Grapheme index of the next word boundary (whitespace-delimited).
/// Moves past leading whitespace then to the end of the next word
/// (emacs `forward-word` semantics).
fn next_word_boundary(s: &str, col: usize) -> usize {
    let gs: Vec<&str> = s.graphemes(true).collect();
    let len = gs.len();
    let mut i = col;
    // Skip leading whitespace.
    while i < len && gs[i].trim().is_empty() {
        i += 1;
    }
    // Skip non-whitespace (the word itself).
    while i < len && !gs[i].trim().is_empty() {
        i += 1;
    }
    i
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::plain(code)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::ctrl(c)
    }

    fn alt(c: char) -> KeyEvent {
        KeyEvent::alt(c)
    }

    fn shift_enter() -> KeyEvent {
        KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)
    }

    fn type_str(state: &mut InputState, s: &str) {
        for c in s.chars() {
            state.handle_key(&key(KeyCode::Char(c)));
        }
    }

    // ── Basic insert / value ──────────────────────────────────────────────────

    #[test]
    fn insert_chars_updates_value() {
        let mut s = InputState::new();
        type_str(&mut s, "hello");
        assert_eq!(s.value(), "hello");
        assert_eq!(s.cursor(), (0, 5));
    }

    #[test]
    fn insert_newline_splits_line() {
        let mut s = InputState::new();
        type_str(&mut s, "ab");
        assert_eq!(s.handle_key(&shift_enter()), InputAction::Newline);
        type_str(&mut s, "cd");
        assert_eq!(s.value(), "ab\ncd");
        assert_eq!(s.line_count(), 2);
        assert_eq!(s.cursor(), (1, 2));
    }

    #[test]
    fn enter_returns_submit() {
        let mut s = InputState::new();
        assert_eq!(s.handle_key(&key(KeyCode::Enter)), InputAction::Submit);
    }

    // ── Backspace / delete ────────────────────────────────────────────────────

    #[test]
    fn backspace_deletes_char() {
        let mut s = InputState::new();
        type_str(&mut s, "abc");
        s.handle_key(&key(KeyCode::Backspace));
        assert_eq!(s.value(), "ab");
        assert_eq!(s.cursor(), (0, 2));
    }

    #[test]
    fn backspace_merges_lines() {
        let mut s = InputState::new();
        type_str(&mut s, "ab");
        s.handle_key(&shift_enter());
        type_str(&mut s, "cd");
        // Move cursor to start of second line.
        s.handle_key(&ctrl('a'));
        s.handle_key(&key(KeyCode::Backspace));
        assert_eq!(s.value(), "abcd");
        assert_eq!(s.line_count(), 1);
        assert_eq!(s.cursor(), (0, 2));
    }

    #[test]
    fn delete_forward_removes_char() {
        let mut s = InputState::new();
        type_str(&mut s, "abc");
        s.handle_key(&ctrl('a')); // Home
        s.handle_key(&key(KeyCode::Delete));
        assert_eq!(s.value(), "bc");
    }

    // ── Cursor movement ───────────────────────────────────────────────────────

    #[test]
    fn left_right_move_cursor() {
        let mut s = InputState::new();
        type_str(&mut s, "abc");
        s.handle_key(&key(KeyCode::Left));
        assert_eq!(s.cursor(), (0, 2));
        s.handle_key(&key(KeyCode::Right));
        assert_eq!(s.cursor(), (0, 3));
    }

    #[test]
    fn left_wraps_to_prev_line() {
        let mut s = InputState::new();
        type_str(&mut s, "ab");
        s.handle_key(&shift_enter());
        s.handle_key(&ctrl('a')); // Start of line 2
        s.handle_key(&key(KeyCode::Left));
        assert_eq!(s.cursor(), (0, 2)); // End of line 1
    }

    #[test]
    fn home_end_keys() {
        let mut s = InputState::new();
        type_str(&mut s, "hello");
        s.handle_key(&key(KeyCode::Home));
        assert_eq!(s.cursor(), (0, 0));
        s.handle_key(&key(KeyCode::End));
        assert_eq!(s.cursor(), (0, 5));
    }

    #[test]
    fn ctrl_a_e_move_to_line_bounds() {
        let mut s = InputState::new();
        type_str(&mut s, "hello");
        s.handle_key(&ctrl('a'));
        assert_eq!(s.cursor(), (0, 0));
        s.handle_key(&ctrl('e'));
        assert_eq!(s.cursor(), (0, 5));
    }

    // ── Word movement ─────────────────────────────────────────────────────────

    #[test]
    fn alt_b_f_word_movement() {
        let mut s = InputState::new();
        type_str(&mut s, "foo bar");
        s.handle_key(&alt('b'));
        assert_eq!(s.cursor(), (0, 4)); // Before "bar"
        s.handle_key(&alt('b'));
        assert_eq!(s.cursor(), (0, 0)); // Before "foo"
        s.handle_key(&alt('f'));
        assert_eq!(s.cursor(), (0, 3)); // After "foo"
    }

    // ── Kill / yank ───────────────────────────────────────────────────────────

    #[test]
    fn ctrl_k_kills_to_eol() {
        let mut s = InputState::new();
        type_str(&mut s, "hello world");
        // Move to after "hello "
        for _ in 0..5 {
            s.handle_key(&key(KeyCode::Left));
        }
        s.handle_key(&ctrl('k'));
        assert_eq!(s.value(), "hello ");
    }

    #[test]
    fn ctrl_k_at_eol_merges_next_line() {
        let mut s = InputState::new();
        type_str(&mut s, "ab");
        s.handle_key(&shift_enter());
        type_str(&mut s, "cd");
        // Move cursor to end of first line.
        s.handle_key(&key(KeyCode::Up));
        s.handle_key(&ctrl('e'));
        s.handle_key(&ctrl('k'));
        assert_eq!(s.value(), "abcd");
        assert_eq!(s.line_count(), 1);
    }

    #[test]
    fn ctrl_u_kills_to_bol() {
        let mut s = InputState::new();
        type_str(&mut s, "hello");
        s.handle_key(&ctrl('u'));
        assert_eq!(s.value(), "");
        assert_eq!(s.cursor(), (0, 0));
    }

    #[test]
    fn ctrl_y_yanks() {
        let mut s = InputState::new();
        type_str(&mut s, "hello");
        s.handle_key(&ctrl('k')); // kills "" (nothing after cursor? no - at end)
        // Kill "hello" by going home first
        s.handle_key(&ctrl('a'));
        s.handle_key(&ctrl('k'));
        assert_eq!(s.value(), "");
        s.handle_key(&ctrl('y'));
        assert_eq!(s.value(), "hello");
    }

    // ── Word deletion ─────────────────────────────────────────────────────────

    #[test]
    fn ctrl_w_deletes_word_back() {
        let mut s = InputState::new();
        type_str(&mut s, "foo bar");
        s.handle_key(&ctrl('w'));
        assert_eq!(s.value(), "foo ");
    }

    #[test]
    fn alt_d_deletes_word_forward() {
        let mut s = InputState::new();
        type_str(&mut s, "foo bar");
        s.handle_key(&ctrl('a')); // Home
        s.handle_key(&alt('d'));
        assert_eq!(s.value(), " bar");
    }

    // ── History ───────────────────────────────────────────────────────────────

    #[test]
    fn up_navigates_history() {
        let mut s = InputState::new();
        s.push_history("first".to_string());
        s.push_history("second".to_string());

        s.handle_key(&key(KeyCode::Up));
        assert_eq!(s.value(), "second");

        s.handle_key(&key(KeyCode::Up));
        assert_eq!(s.value(), "first");
    }

    #[test]
    fn down_returns_to_draft() {
        let mut s = InputState::new();
        s.push_history("old".to_string());
        type_str(&mut s, "draft");

        s.handle_key(&key(KeyCode::Up));
        assert_eq!(s.value(), "old");
        s.handle_key(&key(KeyCode::Down));
        assert_eq!(s.value(), "draft");
    }

    #[test]
    fn push_history_deduplicates_adjacent() {
        let mut s = InputState::new();
        s.push_history("foo".to_string());
        s.push_history("foo".to_string());
        s.push_history("bar".to_string());
        assert_eq!(s.history.len(), 2);
    }

    // ── Clear / set_value ─────────────────────────────────────────────────────

    #[test]
    fn clear_resets_state() {
        let mut s = InputState::new();
        type_str(&mut s, "hello");
        s.clear();
        assert!(s.is_empty());
        assert_eq!(s.cursor(), (0, 0));
    }

    #[test]
    fn set_value_handles_multiline() {
        let mut s = InputState::new();
        s.set_value("line1\nline2\nline3");
        assert_eq!(s.line_count(), 3);
        assert_eq!(s.cursor(), (2, 5)); // End of last line
    }

    // ── Render ────────────────────────────────────────────────────────────────

    #[test]
    fn renders_text_into_buffer() {
        use crate::widgets::testing::render_to_lines;
        let mut state = InputState::new();
        state.set_value("hello");
        let element = Input::new(&state).into_element();
        let lines = render_to_lines(element, 10, 1);
        assert_eq!(lines[0].trim_end(), "hello");
    }

    #[test]
    fn renders_placeholder_when_empty() {
        use crate::widgets::testing::render_to_lines;
        let state = InputState::new();
        let element = Input::new(&state)
            .placeholder("Type here...")
            .into_element();
        let lines = render_to_lines(element, 14, 1);
        assert_eq!(lines[0].trim_end(), "Type here...");
    }

    #[test]
    fn renders_multiline() {
        use crate::widgets::testing::render_to_lines;
        let mut state = InputState::new();
        state.set_value("foo\nbar");
        let element = Input::new(&state).into_element();
        let lines = render_to_lines(element, 10, 2);
        assert_eq!(lines[0].trim_end(), "foo");
        assert_eq!(lines[1].trim_end(), "bar");
    }
}
