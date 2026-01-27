# TUI v2: Drop ratatui, Use crossterm Directly

## Problem

`Viewport::Inline(15)` creates a fixed 15-line viewport. Our UI needs dynamic height (input box grows/shrinks). This mismatch causes gaps and bugs.

## Decision

Drop ratatui. Use crossterm directly for terminal I/O.

## Architecture

```
┌─────────────────────────────────────┐
│ ion                                 │
│ v0.0.0                              │
│                                     │
│ > user message                      │
│                                     │  ← Blank line after each message
│  agent response here                │
│                                     │  ← Blank line after each message
│ • tool(args)                        │
│   ⎿ result                          │
│                                     │  ← Blank line (last message)
├─────────────────────────────────────┤  ← Progress starts here (no extra gap needed)
│ ⠋ Ionizing... (5s · Esc to cancel)  │  ← Progress line (1)
├─────────────────────────────────────┤
│  > multiline input                  │  ← Input area (dynamic height)
│    continues here                   │
├─────────────────────────────────────┤
│ [WRITE] · model · 45%               │  ← Status line (1)
└─────────────────────────────────────┘

SCROLLBACK (native terminal)          MANAGED BOTTOM AREA
├── Header (ion, version)             ├── Progress (1 line)
├── Chat history                      ├── Input (dynamic, min 1 line + borders)
├── Tool output                       └── Status (1 line)
└── Blank line after each entry
```

## Render Model

### Chat History → stdout (native scrollback)

```rust
// Each message ends with blank line
println!("{}", styled_message);
println!();  // blank line separator
```

- Terminal manages scrollback
- Native scroll, search (Cmd+F), copy works
- Persists after exit
- Header ("ion", "v0.0.0") is first thing printed, scrolls up with history

### Bottom UI → cursor positioning

```rust
fn render_bottom_ui(&self) -> io::Result<()> {
    let (term_width, term_height) = terminal::size()?;
    let ui_height = self.calculate_ui_height();

    // Position at bottom
    execute!(stdout(), MoveTo(0, term_height - ui_height))?;

    // Render each component
    self.render_progress()?;
    self.render_input_borders_and_content()?;
    self.render_status()?;

    // Position cursor in input area
    execute!(stdout(), MoveTo(cursor_x, cursor_y))?;

    stdout().flush()
}
```

### Height Calculation

```rust
fn calculate_ui_height(&self) -> u16 {
    let progress_height = 1;
    let input_height = self.input_visual_lines() + 2;  // content + top/bottom borders
    let status_height = 1;
    progress_height + input_height + status_height
}
```

## What We Keep

| Component           | Source             | Notes                                            |
| ------------------- | ------------------ | ------------------------------------------------ |
| Word wrap algorithm | `composer/mod.rs`  | Already implemented, works well                  |
| Cursor positioning  | `composer/mod.rs`  | `build_visual_lines()`, `calculate_cursor_pos()` |
| Syntax highlighting | `highlight.rs`     | Keep as-is                                       |
| Chat formatting     | `chat_renderer.rs` | Adapt to return strings instead of ratatui Lines |

## What We Remove

| Component                     | Why                                    |
| ----------------------------- | -------------------------------------- |
| `Viewport::Inline`            | Root cause of all viewport bugs        |
| ratatui dependency            | Not providing value, causing friction  |
| `MessageList.scroll_offset`   | Dead code - terminal handles scrolling |
| `MessageList.scroll_up/down`  | Dead code                              |
| `UI_VIEWPORT_HEIGHT` constant | No fixed viewport                      |
| `insert_before()`             | Replace with println!()                |

## Open Questions (Need Research)

### Q1: Double Buffering / Cell Diffing

**Question:** Is cell diffing worth implementing ourselves?

**Context:** ratatui provides automatic cell diffing (only send changed cells to terminal). Research said this is ~2% overhead, 98% is I/O.

**Options:**

- A: No diffing - just redraw bottom UI each frame (simpler)
- B: Implement simple line-based diffing (medium)
- C: Full cell diffing like ratatui (complex)

**To research:** What does Pi-mono do? What does Codex do? Is the flicker noticeable without diffing if we use synchronized output?

### Q2: Synchronized Output

**Question:** Is CSI 2026 (synchronized output) sufficient to prevent flicker without diffing?

**Context:** We already use `BeginSynchronizedUpdate` / `EndSynchronizedUpdate`. If this prevents flicker, we may not need diffing at all.

**To research:** Test with synchronized output only, no diffing. Does it flicker?

### Q3: Terminal Resize Handling

**Question:** How to handle terminal resize cleanly?

**Context:** When terminal resizes:

- Width change: text reflows in scrollback (terminal handles), our word wrap needs recalculation
- Height change: our bottom UI position changes

**Options:**

- A: Redraw everything on SIGWINCH
- B: Only recalculate bottom UI position
- C: Something smarter?

**To research:** What do other tools do? Is there terminal-dependent behavior?

### Q4: Streaming Response Rendering

**Question:** How to render streaming responses that aren't yet in scrollback?

**Context:** While agent is responding:

- Response streams in token by token
- Can't println!() each token (would flood scrollback with partial lines)
- Need to show progress somewhere

**Options:**

- A: Buffer in progress area until complete, then println!()
- B: Show streaming in a "preview" area above progress
- C: Show in input area (like Claude Code's thinking display)

**To research:** What does Claude Code do? What does Codex do?

### Q5: Selector UI (Model Picker, etc.)

**Question:** How to handle modal selectors without Viewport?

**Context:** Currently selectors render in the viewport. Without viewport:

- Could overlay on bottom area
- Could use alternate screen temporarily
- Could be inline expanding list

**Options:**

- A: Replace bottom UI temporarily with selector
- B: Push selector to scrollback, use bottom area for filter input
- C: Something else

**To research:** Prototype and see what feels right.

### Q6: LLM Client (llm-connector)

**Question:** Should we replace llm-connector with our own HTTP client?

**Context:** Kimi K2.5 returns responses in `reasoning` field, not `content`. llm-connector doesn't handle this. Other models may have quirks too.

**Options:**

- A: Keep llm-connector, post-process responses
- B: Fork llm-connector, add features we need
- C: Write our own thin client (it's just HTTP + JSON)

**To research:** How many edge cases exist? Is llm-connector actively maintained?

## Implementation Plan

### Phase 1: Minimal Viable Refactor

1. Create `src/tui/terminal.rs` - raw crossterm wrapper
2. Adapt `chat_renderer.rs` to return styled strings
3. Implement bottom UI rendering with cursor positioning
4. Remove ratatui dependency
5. Test basic flow works

### Phase 2: Polish

1. Add synchronized output wrapper
2. Handle resize properly
3. Implement streaming response display
4. Port selector UI

### Phase 3: Cleanup

1. Remove dead code (scroll methods, viewport constants)
2. Update tests
3. Document architecture

## Files to Modify

| File                       | Change                                   |
| -------------------------- | ---------------------------------------- |
| `Cargo.toml`               | Remove ratatui, keep crossterm           |
| `src/main.rs`              | Remove Terminal/Viewport, use raw stdout |
| `src/tui/mod.rs`           | Simplify App struct                      |
| `src/tui/terminal.rs`      | NEW - crossterm wrapper                  |
| `src/tui/render.rs`        | Rewrite for cursor-based rendering       |
| `src/tui/chat_renderer.rs` | Return strings instead of ratatui Lines  |
| `src/tui/message_list.rs`  | Remove scroll_offset and scroll methods  |
| `src/tui/composer/mod.rs`  | Keep logic, change output format         |

## References

- Research: `ai/research/viewport-investigation-2026-01.md`
- Research: `ai/research/pi-mono-tui-analysis.md`
- Research: `ai/research/codex-tui-analysis.md`
- Research: `ai/research/tui-rendering-research.md`
- Previous design: `ai/design/tui-architecture.md`

## Status

**Phase:** Design complete, ready for research on open questions
**Next:** Research Q1-Q6, then implement Phase 1
