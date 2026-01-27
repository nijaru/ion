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

## Research Decisions (Completed 2026-01-27)

### Q1-Q2: Diffing & Synchronized Output

**Decision:** No custom diffing needed for bottom UI. Use synchronized output + clear/redraw.

**Rationale:**

- Bottom UI is 5-15 lines - clear + redraw is fast enough
- ~98% of render time is I/O syscalls, diffing only saves ~2%
- Streaming responses need line-level diffing (buffer in managed area)
- Claude Code's lesson: full redraw per streaming chunk caused 4,000-6,700 scroll events/sec

**Pattern:**

```rust
fn render_bottom_ui(&self) -> io::Result<()> {
    execute!(stdout(), BeginSynchronizedUpdate)?;
    execute!(stdout(), MoveTo(0, height - ui_height))?;
    execute!(stdout(), Clear(ClearType::FromCursorDown))?;
    self.render_progress()?;
    self.render_input()?;
    self.render_status()?;
    execute!(stdout(), EndSynchronizedUpdate)?;
    Ok(())
}
```

### Q3: Terminal Resize Handling

**Decision:** Width change = full redraw, Height change = position adjust only.

**Rationale:**

- Width change: terminal reflows scrollback automatically, we must recalculate word wrap
- Height change: only bottom UI position changes, quick adjust
- Always wrap in synchronized output

**Pattern (pi-mono style):**

```
Width change:  \x1b[3J\x1b[2J\x1b[H + full redraw
Height change: Recalculate ui_height, reposition, redraw bottom
```

### Q4: Streaming Response Rendering

**Decision:** Buffer in managed area, differential render, commit to scrollback on complete.

**Rationale:**

- Can't println() each token (floods scrollback)
- Codex pattern: AgentMessageDelta updates in-place, AgentMessageComplete pushes to history
- Line-level diffing for streaming: find first changed line, clear to end, render from there
- Hide input area during streaming, show progress

**Pattern:**

```rust
match event {
    AgentMessageDelta(delta) => {
        self.active_response.push_str(&delta.text);
        self.render_streaming_area()?;  // differential
    }
    AgentMessageComplete => {
        println!("{}", self.format_response(&self.active_response));
        println!();  // blank line separator
        self.active_response.clear();
    }
}
```

### Q5: Selector UI

**Decision:** Replace bottom UI temporarily with selector (Option A).

**Rationale:**

- No alternate screen complexity
- Selector never pollutes scrollback history
- Reuses existing bottom-area rendering infrastructure
- Filter input + navigable list in the space normally used for input

### Q6: LLM Client

**Decision:** Replace llm-connector with custom HTTP client (gradual migration).

**Rationale:**

- llm-connector blocks cache_control for Anthropic (90% cost reduction)
- Cannot extract reasoning_content from Kimi K2/DeepSeek R1
- ~1000 LOC investment, justified by cost savings

**Phases:**

1. Anthropic client with cache_control
2. Reasoning model support (Kimi K2, DeepSeek R1)
3. Migrate remaining providers
4. Remove llm-connector

**Research files:**

- `ai/research/tui-diffing-research.md`
- `ai/research/tui-resize-streaming-research.md`
- `ai/research/tui-selectors-http-research.md`

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

**Phase:** Research complete, implementing Phase 1
**Updated:** 2026-01-27
