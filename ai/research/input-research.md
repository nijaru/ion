# Input System Research

Comprehensive research on text input, fuzzy matching, and multi-line handling for terminal agents.

**Updated:** 2026-01-27 (consolidated from input-\*.md, fuzzy-search.md, multi-line-input-overflow-2026.md)

---

## Summary

| Category          | Evaluated Options                       | ion Decision            |
| ----------------- | --------------------------------------- | ----------------------- |
| Text editing      | rat-text, tui-input, reedline, custom   | Custom Composer (ropey) |
| Fuzzy matching    | fuzzy-matcher, nucleo                   | TBD                     |
| Multi-line scroll | tui-textarea, internal scroll, external | Custom scroll logic     |

**Key insight:** No off-the-shelf library met all requirements. ion uses a custom Composer with ropey-backed buffer, grapheme-aware cursor, and internal scroll.

---

## 1. Text Editing Crates

### rat-text

- **Repo:** https://github.com/thscharler/rat-salsa
- **Features:** Multi-line, selection, word/line navigation, undo/redo, clipboard, ropey backend
- **Verdict:** Most complete, but heavy dependency surface

### tui-input

- **Repo:** https://github.com/sayanarijit/tui-input
- **Features:** Lightweight, multi-backend support
- **Verdict:** Single-line oriented, insufficient for chat composer

### reedline (Nushell)

- **Repo:** https://github.com/nushell/reedline
- **Features:**
  - Multi-line via `Validator` trait (incomplete input continues)
  - Concurrent output via `ExternalPrinter`
  - History with Ctrl+R search
  - Vi/Emacs keybindings
- **Verdict:** Recommended for readline-style apps, but doesn't integrate with ratatui widgets

```rust
// reedline multi-line example
impl Validator for MultiLineValidator {
    fn validate(&self, line: &str) -> ValidationResult {
        if line.ends_with('\\') || line.ends_with('{') {
            ValidationResult::Incomplete
        } else {
            ValidationResult::Complete
        }
    }
}
```

### rustyline-async

- **Repo:** https://github.com/zyansheep/rustyline-async
- **Features:** SharedWriter for concurrent output
- **Verdict:** NOT VIABLE - single-line only

### Custom (Codex CLI approach)

- Custom `TextArea` in `codex-rs/tui2/src/bottom_pane/textarea.rs`
- Full control, high maintenance cost
- ion adopted this approach with ropey backend

---

## 2. Fuzzy Matching

### fuzzy-matcher

- **Crate:** https://docs.rs/fuzzy-matcher
- **License:** MIT
- **Verdict:** Simple, good for small-to-medium lists (provider/model selection)

### nucleo

- **Crate:** https://docs.rs/nucleo
- **License:** MPL-2.0
- **Verdict:** Best-in-class scoring (used by Helix), heavier API

### Codex approach

- Custom case-insensitive subsequence matcher in `codex-rs/common/src/fuzzy_match.rs`
- Simpler than nucleo, closer to fuzzy-matcher

**Recommendation:** Start with fuzzy-matcher, upgrade to nucleo if scoring quality becomes limiting.

---

## 3. Multi-Line Input Overflow

### How agents handle large input

| Agent        | Framework    | Approach                         | Large Paste Behavior         |
| ------------ | ------------ | -------------------------------- | ---------------------------- |
| Claude Code  | Ink (React)  | overflow: hidden + scroll offset | Known issues (flicker, hang) |
| Gemini CLI   | Ink (React)  | External editor fallback         | No native multiline          |
| Codex CLI    | Rust/ratatui | Internal scroll + wrap cache     | Placeholder for large pastes |
| tui-textarea | ratatui      | Viewport-based auto-scroll       | Cursor-following scroll      |

### Claude Code issues

- Terminal scrolling bug (#10886): Pasting causes repeated jumps
- CLI hangs on large paste (#1490): Unresponsive after large text
- Generates 4,000-6,700 scroll events/second in tmux
- 31-line paste caused 162 MB of stdout data

### Codex CLI implementation

```rust
// From textarea.rs
fn effective_scroll(&self, cursor_line_idx: u16, area_height: u16) -> u16 {
    if cursor_line_idx < self.scroll {
        return cursor_line_idx;  // Cursor above viewport
    }
    if cursor_line_idx >= self.scroll + area_height {
        return cursor_line_idx + 1 - area_height;  // Below viewport
    }
    self.scroll  // Within viewport
}

// Large paste handling
const LARGE_PASTE_CHAR_THRESHOLD: usize = 1000;
// Insert placeholder, expand on submit
```

### tui-textarea pattern

```rust
// Auto-scroll to keep cursor visible
fn next_scroll_top(cursor_row: usize, scroll: usize, height: usize) -> usize {
    if cursor_row < scroll { return cursor_row; }
    if cursor_row >= scroll + height { return cursor_row + 1 - height; }
    scroll
}
```

### Best practices

1. **Constrain input height** - Fixed or max height for input area
2. **Internal scroll with cursor-following** - Always keep cursor visible
3. **Render only visible lines** - Performance for large content
4. **Paste detection** - Distinguish rapid input from typing
5. **External editor fallback** - For very large content
6. **Placeholder expansion** - Store large pastes separately

---

## 4. ion Implementation

ion uses a custom Composer (see `src/tui/composer/`):

- **ComposerBuffer** - ropey-backed text buffer with blob storage for large pastes
- **ComposerState** - Grapheme-aware cursor, word navigation, scroll offset
- **Key bindings:**
  - Shift+Enter: newline
  - Enter: submit
  - Ctrl+G: external editor
  - Esc Esc: clear
  - Cmd+Left/Right (macOS): visual line start/end
  - Opt+Left/Right (macOS): word movement

### Scroll handling

```rust
// Keep cursor visible in constrained viewport
fn scroll_to_cursor(&mut self, visible_height: usize) {
    if self.cursor_row < self.scroll_offset {
        self.scroll_offset = self.cursor_row;
    } else if self.cursor_row >= self.scroll_offset + visible_height {
        self.scroll_offset = self.cursor_row + 1 - visible_height;
    }
}
```

---

## Sources

### Libraries

- [rat-text](https://github.com/thscharler/rat-salsa/tree/master/rat-text)
- [reedline](https://docs.rs/reedline)
- [rustyline-async](https://docs.rs/rustyline-async)
- [tui-textarea](https://github.com/rhysd/tui-textarea)
- [fuzzy-matcher](https://docs.rs/fuzzy-matcher)
- [nucleo](https://docs.rs/nucleo)

### Agent Issues

- [Claude Code #10886 - Terminal scrolling](https://github.com/anthropics/claude-code/issues/10886)
- [Claude Code #1490 - CLI hangs on paste](https://github.com/anthropics/claude-code/issues/1490)
- [Gemini CLI #3103 - No multiline](https://github.com/google-gemini/gemini-cli/issues/3103)
- [Codex #1064 - Scroll navigation](https://github.com/openai/codex/issues/1064)
