# Multi-Line Input Overflow Handling in Terminal TUI Agents

**Research Date:** 2026-01-23
**Purpose:** Investigate how terminal coding agents handle multi-line input that exceeds viewport height

---

## Summary

| Agent/Tool   | Framework    | Approach                          | Large Paste Behavior         |
| ------------ | ------------ | --------------------------------- | ---------------------------- |
| Claude Code  | Ink (React)  | overflow: hidden + scroll offset  | Known issues (flicker, hang) |
| Gemini CLI   | Ink (React)  | External editor fallback (Ctrl-X) | No native multiline support  |
| Codex CLI    | Rust/ratatui | Internal scroll + wrap cache      | Placeholder for large pastes |
| tui-textarea | ratatui      | Viewport-based auto-scroll        | Cursor-following scroll      |
| rat-text     | ratatui      | Ropey-backed, multi-line scroll   | Full editor semantics        |
| fzf          | Go           | Height constraint + scroll        | 1024 char width calc limit   |

**Key Insight:** No terminal agent handles large multi-line input perfectly. The best pattern is:

1. Internal scrolling within a constrained height
2. Placeholder/external editor fallback for very large pastes
3. Cursor-following scroll to keep input visible

---

## Claude Code (Ink/React)

### Architecture

- Uses Ink (React for terminals) with custom renderer
- Anthropic rewrote the renderer from scratch for better incremental updates
- Still keeps React as component model

### Input Overflow Handling

**Mechanism:** Uses `overflow` props on `<Box>` components

```jsx
// Ink overflow API
<Box overflowX="hidden" overflowY="hidden">
  {/* content clipped to box bounds */}
</Box>
```

**Scrolling Pattern (from Ink PR #393):**

```jsx
const Scroller = ({ scrollX, scrollY, width, height, children }) => (
  <Box overflow="hidden" width={width} height={height}>
    <Box marginTop={-scrollY} marginLeft={-scrollX} flexGrow={0} flexShrink={0}>
      {children}
    </Box>
  </Box>
);
```

### Known Issues with Large Input

1. **Terminal scrolling bug** ([#10886](https://github.com/anthropics/claude-code/issues/10886)): Pasting rapidly causes repeated jumps/scrolls
2. **CLI hangs on large paste** ([#1490](https://github.com/anthropics/claude-code/issues/1490)): Unresponsive after large text paste
3. **Memory overflow** ([#2774](https://github.com/anthropics/claude-code/issues/2774)): Crashes on many lines or complex problems
4. **Excessive scrolling in long chats** ([#16040](https://github.com/anthropics/claude-code/issues/16040)): Scrolls through entire history on new prompt

**Technical Analysis:**

> "When processing large inputs, it triggers a massive number of terminal scroll events. In tmux, Claude Code generates 4,000-6,700 scroll events per second."
> "Pasting 31 lines caused 162 MB of data to stdout."

### Multiline Input UX

- **Enter** submits, **Alt+Enter** for newlines
- No native internal scroll for input area
- External editor via `/edit` command

---

## Gemini CLI (Ink/React)

### Architecture

- TypeScript/Node with Ink framework
- Uses alternate screen buffer by default (configurable)

### Input Overflow Handling

**No native multi-line input support.** Users report ([#3103](https://github.com/google-gemini/gemini-cli/issues/3103)):

> "The CLI currently lacks an intuitive way to enter multiline commands."

**Workaround:** Press Ctrl-X to open default editor, edit content, save and it pastes to input.

### Known Issues

1. **Scrollbar unusable in long sessions** ([#13271](https://github.com/google-gemini/gemini-cli/issues/13271)): CLI component takes over scrolling
2. **Multi-line input doesn't work** ([#3189](https://github.com/google-gemini/gemini-cli/issues/3189)): Shift+Enter treated as separate submission
3. **Paste inability on Windows** ([#13121](https://github.com/google-gemini/gemini-cli/issues/13121)): Multiple critical issues

### Configuration

```json
{
  "settings": {
    "Use Alternate Screen Buffer": false // Disabled by default now
  }
}
```

Keyboard scrolling: shift-up/down, page up/down, home/end.

---

## Codex CLI (Rust/ratatui)

### Architecture

- Rust-based TUI using ratatui + crossterm
- Full-screen alternate buffer mode
- Custom textarea implementation

### Key Source Files

```
codex-rs/tui/src/
  bottom_pane/
    textarea.rs        - Textarea rendering + scroll logic
    chat_composer.rs   - Input composer with paste handling
    mod.rs            - Bottom pane container
  wrapping.rs         - Text wrapping helpers
  live_wrap.rs        - Dynamic text wrapping
```

### Input Overflow Implementation

**From `textarea.rs`:**

```rust
struct TextAreaState {
    scroll: u16,  // Index into wrapped lines of first visible line
    // ...
}

fn effective_scroll(&self, cursor_line_idx: u16, area_height: u16) -> u16 {
    // Cursor above viewport: move viewport to cursor
    if cursor_line_idx < self.scroll {
        return cursor_line_idx;
    }
    // Cursor below viewport: adjust so cursor at bottom
    if cursor_line_idx >= self.scroll + area_height {
        return cursor_line_idx + 1 - area_height;
    }
    // Cursor within viewport: maintain position
    self.scroll
}
```

**Wrap Cache:**

- Lazy wrapping through `WrapCache` structure
- Uses `textwrap` crate with `FirstFit` algorithm
- Recalculates only when width changes

### Large Paste Handling

**From `chat_composer.rs`:**

```rust
const LARGE_PASTE_CHAR_THRESHOLD: usize = 1000;

// If paste exceeds threshold, insert placeholder
// "[Pasted Content 1000 chars]"
// Full text stored in pending_pastes, expanded on submit
```

**Burst Detection:**

- Treats rapid character sequences as paste events
- Prevents misinterpretation of multi-line pastes

### Scroll Navigation

- PageUp/PageDown: Limited support initially ([#1064](https://github.com/openai/codex/issues/1064))
- No native terminal scrollback in alt-screen mode
- `CODEX_TUI_RECORD_SESSION=1` for transcript logging

---

## tui-textarea (Recommended Library)

### Overview

ratatui widget providing `<textarea>`-like multi-line editing.

**Crate:** [tui-textarea](https://crates.io/crates/tui-textarea)
**GitHub:** [rhysd/tui-textarea](https://github.com/rhysd/tui-textarea)

### Viewport Management

**Auto-scrolling:** Viewport automatically adjusts to keep cursor visible.

```rust
fn next_scroll_top(cursor_row: usize, scroll: usize, height: usize) -> usize {
    // Cursor above viewport
    if cursor_row < scroll {
        return cursor_row;
    }
    // Cursor below viewport
    if cursor_row >= scroll + height {
        return cursor_row + 1 - height;
    }
    // Within viewport
    scroll
}
```

**Key Features:**

- Line-based rendering (only visible lines rendered)
- Saturating arithmetic for safe scroll bounds
- Scroll state stored in `TextArea` itself (immutable during render)
- Supports Ctrl+V/Alt+V for page-up/page-down

### Usage for Constrained Height

```rust
use tui_textarea::TextArea;

let mut textarea = TextArea::default();

// Render in a constrained area - internal scroll handles overflow
frame.render_widget(&textarea, Rect {
    x: 0,
    y: terminal_height - 5,  // Fixed 5-line input area
    width: terminal_width,
    height: 5,
});
```

### Pros and Cons

**Pros:**

- Automatic viewport scroll
- Only renders visible lines (performance)
- Cursor always visible
- Backend agnostic

**Cons:**

- No explicit max_height API (use layout constraints)
- Viewport determined by render area

---

## General Patterns

### Standard Approaches

| Approach              | Used By             | Pros                   | Cons                          |
| --------------------- | ------------------- | ---------------------- | ----------------------------- |
| Internal scroll       | tui-textarea, Codex | Contained, predictable | Must implement scroll state   |
| External editor       | Gemini CLI          | Full editor features   | Context switch, UX friction   |
| Placeholder + expand  | Codex CLI           | Handles huge pastes    | User can't edit large content |
| Grow viewport         | Simple TUIs         | No scroll logic        | Can exceed terminal height    |
| Line limit + truncate | Some chat apps      | Simple                 | Loses content                 |

### Best Practices

1. **Constrain input height** - Use fixed or max height for input area
2. **Internal scroll with cursor-following** - Always keep cursor visible
3. **Render only visible lines** - Performance for large content
4. **Paste detection** - Distinguish rapid input from typing
5. **External editor fallback** - For very large content
6. **Placeholder expansion** - Store large pastes separately

### Terminal Limitations

- **Alternate screen buffer:** No native scrollback (by design)
- **Large stdout writes:** Can overwhelm terminal buffer
- **ANSI escape storms:** Many scroll events cause flicker
- **Width calculation:** Some tools cap at 1024 chars (fzf)

---

## Recommendations for ion

### Primary Approach: tui-textarea with Constrained Height

```rust
use tui_textarea::TextArea;
use ratatui::layout::{Constraint, Layout};

fn render_input_area(f: &mut Frame, area: Rect, textarea: &TextArea) {
    // Constrain input to max 8 lines, min 3 lines
    let input_height = textarea.lines().len().clamp(3, 8) as u16;

    let chunks = Layout::vertical([
        Constraint::Min(0),           // Chat history
        Constraint::Length(input_height + 2),  // Input + borders
    ]).split(area);

    // tui-textarea handles internal scroll automatically
    f.render_widget(textarea, chunks[1]);
}
```

### Large Paste Handling

```rust
const LARGE_PASTE_THRESHOLD: usize = 500;

fn handle_paste(&mut self, content: &str) {
    if content.len() > LARGE_PASTE_THRESHOLD {
        // Option 1: Show placeholder, expand on submit
        self.pending_paste = Some(content.to_string());
        self.textarea.insert_str("[Pasted content - press Enter to submit]");

        // Option 2: Open external editor
        // self.open_editor_with_content(content);
    } else {
        self.textarea.insert_str(content);
    }
}
```

### Dynamic Height with Max Constraint

```rust
fn input_area_height(&self) -> u16 {
    const MIN_HEIGHT: u16 = 3;
    const MAX_HEIGHT: u16 = 10;  // Never exceed 10 lines

    let content_lines = self.textarea.lines().len() as u16;
    content_lines.clamp(MIN_HEIGHT, MAX_HEIGHT)
}
```

---

## Sources

### Claude Code

- [#10886 - Terminal scrolling on large paste](https://github.com/anthropics/claude-code/issues/10886)
- [#1490 - CLI hangs on large paste](https://github.com/anthropics/claude-code/issues/1490)
- [#2774 - Memory overflow on large input](https://github.com/anthropics/claude-code/issues/2774)
- [#16040 - Excessive scrolling in long conversations](https://github.com/anthropics/claude-code/issues/16040)

### Gemini CLI

- [#3103 - No multiline input](https://github.com/google-gemini/gemini-cli/issues/3103)
- [#13271 - Scrollbar unusable](https://github.com/google-gemini/gemini-cli/issues/13271)
- [#3189 - Multiline input not working](https://github.com/google-gemini/gemini-cli/issues/3189)

### Codex CLI

- [GitHub repo](https://github.com/openai/codex)
- [DeepWiki - Input System](https://deepwiki.com/openai/codex/4.1.3-slash-commands-and-features)
- [#1064 - Scroll navigation feature request](https://github.com/openai/codex/issues/1064)
- [#1247 - Copy/paste improvements](https://github.com/openai/codex/issues/1247)

### Libraries

- [tui-textarea](https://github.com/rhysd/tui-textarea)
- [rat-text](https://github.com/thscharler/rat-salsa/tree/master/rat-text)
- [Ink overflow PR #393](https://github.com/vadimdemedes/ink/pull/393)
- [Ink scrolling issue #222](https://github.com/vadimdemedes/ink/issues/222)
- [ratatui scrollable widgets RFC #174](https://github.com/ratatui/ratatui/issues/174)

### General

- [fzf multi-line tips](https://junegunn.github.io/fzf/tips/processing-multi-line-items/)
- [ratatui Viewport docs](https://docs.rs/ratatui/latest/ratatui/enum.Viewport.html)
