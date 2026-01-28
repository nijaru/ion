# ion - TUI Code Review

Fast, lightweight TUI coding agent in Rust. Review the TUI v2 implementation.

## Quick Start

```bash
cargo build              # Build
cargo test               # Test (113 passing)
cargo clippy             # Lint warnings
cargo run                # Manual testing
```

## Project Structure

```
src/
├── main.rs          # TUI entry, render loop, insert_before logic
├── tui/             # Terminal interface (pure crossterm)
│   ├── mod.rs       # App state
│   ├── render.rs    # Bottom UI rendering
│   ├── events.rs    # Event handling
│   ├── terminal.rs  # StyledLine/StyledSpan primitives
│   ├── composer/    # Custom text input (ropey-backed)
│   ├── chat_renderer.rs  # Message formatting
│   └── highlight.rs # Markdown + syntax highlighting
├── agent/           # Multi-turn conversation loop
├── provider/        # LLM providers via llm crate
├── tool/            # Built-in tools + MCP client
└── session/         # SQLite persistence

ai/                  # Session context
├── STATUS.md        # Current state, known issues
├── design/tui-v2.md # TUI architecture spec
└── research/        # Reference material
```

## TUI v2 Architecture

Pure crossterm, native terminal scrollback:

1. **Chat** → insert_before pattern (ScrollUp + print above UI)
2. **Bottom UI** → cursor positioning at `height - ui_height`
3. **Rendering** → Synchronized output (Begin/EndSynchronizedUpdate)
4. **Resize** → clear screen, reprint all chat from message_list
5. **Exit** → clear bottom UI only, chat stays in scrollback

## Files to Review

### HIGH Priority

| File                      | Purpose                                       |
| ------------------------- | --------------------------------------------- |
| `src/main.rs:57-200`      | TUI setup, render loop, insert_before logic   |
| `src/tui/render.rs`       | Bottom UI: progress, input, status, selectors |
| `src/tui/mod.rs`          | App struct, state fields                      |
| `src/tui/events.rs`       | Event handling, mode transitions              |
| `src/tui/composer/mod.rs` | ComposerState, cursor, scroll                 |
| `src/tui/input.rs`        | Key event handling, startup header            |

### MEDIUM Priority

| File                         | Purpose                          |
| ---------------------------- | -------------------------------- |
| `src/tui/terminal.rs`        | StyledLine/StyledSpan primitives |
| `src/tui/chat_renderer.rs`   | Message → StyledLine conversion  |
| `src/tui/composer/buffer.rs` | ComposerBuffer (ropey-backed)    |

## Known Issues to Investigate

### 1. Version Line 3-Space Indent

**Location:** `src/tui/input.rs:46-53`

Version line appears with 3 spaces before it on startup. Check:

- `startup_header_lines()` creation
- insert_before logic in `src/main.rs`
- MoveTo positioning before printing

### 2. Cursor Position on Wrapped Lines

**Location:** `src/tui/composer/mod.rs`

Cursor position off-by-one on wrapped lines (accumulates). Check:

- `calculate_cursor_position()` logic
- Visual line wrapping vs cursor column calculation
- Byte vs grapheme counting consistency

### 3. Progress Line During Tab Switch

**Location:** `src/tui/render.rs`

Multiple progress lines when switching terminal tabs during streaming. Check:

- UI clear before redraw
- Synchronized update correctness
- Terminal state after tab switch

## Review Checklist

### Correctness

- [ ] insert_before: ScrollUp count matches line count
- [ ] MoveTo positions: 0-indexed, within bounds
- [ ] Synchronized output: Begin/End pairs balanced
- [ ] Cursor: hidden during render, shown correctly after
- [ ] Clear operations: correct ClearType used

### Input Handling

- [ ] Grapheme-aware cursor movement
- [ ] Word navigation respects Unicode
- [ ] Scroll offset keeps cursor visible
- [ ] Large paste handling (blob storage)
- [ ] History navigation at edges

### State Consistency

- [ ] `rendered_entries` tracks printed content
- [ ] `header_inserted` prevents duplicates
- [ ] `is_running` reflects agent state
- [ ] Mode transitions clean up properly

### Edge Cases

- [ ] Empty input
- [ ] Lines wider than terminal
- [ ] Rapid resize events
- [ ] Paste during streaming
- [ ] Ctrl+C in different modes

## Code Patterns to Verify

### Insert Before (src/main.rs)

```rust
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
execute!(stdout, BeginSynchronizedUpdate)?;
// ... all drawing ...
execute!(stdout, EndSynchronizedUpdate)?;
```

## Output Format

For each file, report:

```markdown
## [filename]

### Issues Found

- [HIGH/MEDIUM/LOW] Description
  - Location: line X-Y
  - Impact: what breaks
  - Fix: suggested approach

### Observations

- Pattern X is correct
- Consider refactoring Y
```

## After Review

1. Create tasks: `tk add "[BUG] description" -p 2`
2. Update: `ai/STATUS.md` with findings
3. Commit notes to: `ai/review/tui-review-2026-01.md`
