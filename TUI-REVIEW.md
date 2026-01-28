# TUI Code Review Guide

Review the TUI v2 implementation for correctness, bugs, and improvement opportunities.

## Architecture Overview

**Core model (pure crossterm, no ratatui):**

1. **Chat history** → Printed to stdout via insert_before pattern, terminal handles scrollback
2. **Bottom UI** → We manage: progress, input composer, status line
3. **Rendering** → Synchronized output (BeginSynchronizedUpdate/EndSynchronizedUpdate)
4. **Resize** → Clear screen, reprint all chat from message_list
5. **Exit** → Clear bottom UI only, chat stays in scrollback

## Files to Review

### Core Rendering

| File                       | Purpose                                          | Priority |
| -------------------------- | ------------------------------------------------ | -------- |
| `src/main.rs:57-200`       | TUI setup, main render loop, insert_before logic | HIGH     |
| `src/tui/render.rs`        | Bottom UI: progress, input, status, selectors    | HIGH     |
| `src/tui/terminal.rs`      | StyledLine/StyledSpan primitives                 | MEDIUM   |
| `src/tui/chat_renderer.rs` | Message → StyledLine conversion                  | MEDIUM   |
| `src/tui/highlight.rs`     | Markdown + syntax highlighting                   | LOW      |

### Input System

| File                         | Purpose                            | Priority |
| ---------------------------- | ---------------------------------- | -------- |
| `src/tui/composer/mod.rs`    | ComposerState, cursor, scroll      | HIGH     |
| `src/tui/composer/buffer.rs` | ComposerBuffer (ropey-backed)      | MEDIUM   |
| `src/tui/input.rs`           | Key event handling, startup header | HIGH     |

### State Management

| File                 | Purpose                          | Priority |
| -------------------- | -------------------------------- | -------- |
| `src/tui/mod.rs`     | App struct, state fields         | HIGH     |
| `src/tui/events.rs`  | Event handling, mode transitions | HIGH     |
| `src/tui/session.rs` | Session initialization           | LOW      |

## Known Issues to Investigate

### 1. Version Line 3-Space Indent

**Location:** `src/tui/input.rs:46-53` (`startup_header_lines()`)

**Symptom:** Version line appears with 3 spaces before it on startup.

**Check:**

- How header lines are created in `startup_header_lines()`
- How they're printed in `src/main.rs` insert_before logic
- MoveTo positioning before printing

```rust
// src/tui/input.rs:46-53
pub(super) fn startup_header_lines(&self) -> Vec<StyledLine> {
    let version = format!("v{}", env!("CARGO_PKG_VERSION"));
    vec![
        StyledLine::new(vec![StyledSpan::bold("ION")]),
        StyledLine::new(vec![StyledSpan::dim(version)]),
        StyledLine::empty(),
    ]
}
```

### 2. Cursor Position on Wrapped Lines

**Location:** `src/tui/composer/mod.rs`

**Symptom:** Cursor position off-by-one on wrapped lines (accumulates with more wraps).

**Check:**

- `calculate_cursor_position()` logic
- How visual line wrapping interacts with cursor column calculation
- Whether byte vs grapheme counting is consistent

### 3. Progress Line During Tab Switch

**Location:** `src/tui/render.rs` (`render_progress_direct()`)

**Symptom:** Multiple progress lines when switching terminal tabs during streaming.

**Check:**

- Is the UI properly cleared before redraw?
- Are synchronized updates working correctly?
- Terminal state after tab switch (resize event?)

## Review Checklist

### Correctness

- [ ] insert_before pattern: ScrollUp count matches line count being inserted
- [ ] MoveTo positions: 0-indexed, within terminal bounds
- [ ] Synchronized output: Begin/End pairs balanced in all code paths
- [ ] Cursor visibility: Hidden during render, shown at correct position after
- [ ] Clear operations: Correct ClearType used (FromCursorDown vs All)

### Input Handling

- [ ] Grapheme-aware cursor movement (not byte-based)
- [ ] Word navigation respects Unicode boundaries
- [ ] Scroll offset keeps cursor visible
- [ ] Large paste handling (blob storage threshold)
- [ ] History navigation at buffer edges

### State Consistency

- [ ] `rendered_entries` tracks what's been printed to scrollback
- [ ] `header_inserted` prevents duplicate headers
- [ ] `is_running` correctly reflects agent state
- [ ] Mode transitions (Normal, Selector, etc.) clean up properly

### Edge Cases

- [ ] Empty input handling
- [ ] Very long single lines (wider than terminal)
- [ ] Rapid resize events
- [ ] Paste during streaming
- [ ] Ctrl+C during different modes

## Patterns to Verify

### StyledLine Rendering

```rust
// Should output clean ANSI with proper resets
let line = StyledLine::new(vec![
    StyledSpan::bold("bold"),
    StyledSpan::dim(" dim"),
]);
line.println()?;
```

### Insert Before Pattern

```rust
// src/main.rs - verify this sequence
let ui_start = term_height.saturating_sub(ui_height);
execute!(stdout, MoveTo(0, ui_start))?;
execute!(stdout, ScrollUp(line_count))?;
execute!(stdout, MoveTo(0, ui_start.saturating_sub(line_count)))?;
for line in &chat_lines {
    line.println()?;
}
```

### Synchronized Output

```rust
// All render paths should use this
execute!(stdout, BeginSynchronizedUpdate)?;
// ... all drawing ...
execute!(stdout, EndSynchronizedUpdate)?;
```

## Output Format

For each file reviewed, report:

```markdown
## [filename]

### Issues Found

- [severity: HIGH/MEDIUM/LOW] Description of issue
  - Location: line X-Y
  - Impact: what breaks
  - Fix: suggested approach

### Observations

- Pattern X is used correctly
- Consider refactoring Y for clarity

### No Issues

- (if file looks correct)
```

## Commands

```bash
cargo build              # Verify it compiles
cargo test               # Run tests (113 should pass)
cargo clippy             # Check for warnings
cargo run                # Manual testing
```

## After Review

1. Create tk tasks for any HIGH severity issues found
2. Update ai/STATUS.md with findings
3. Commit review notes to ai/review/tui-review-YYYY-MM.md
