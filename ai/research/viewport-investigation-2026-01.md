# Viewport Investigation - January 2026

## The Problem

Ion uses ratatui's `Viewport::Inline(15)` for the TUI. This creates a fixed 15-line viewport at the bottom of the terminal. Chat history is inserted above via `insert_before()` into native terminal scrollback.

**Bugs caused by this approach:**

1. 10-line gap below completed response (viewport=15, UI=5)
2. 2 empty lines after sending message (trailing blank + viewport top gap)
3. Gap on startup before any chat
4. All stem from: **fixed viewport size > dynamic UI size**

**What we want:**

- Chat history in native terminal scrollback (searchable with Cmd+F, persists after exit)
- Dynamic input area that grows/shrinks with multi-line input
- No visual gaps
- Native terminal features preserved (search, scroll, copy)

## Current Implementation

```
┌─────────────────────────────┐
│ Terminal Scrollback         │ ← Native buffer, insert_before()
│ - Chat history              │
│ - User messages             │
│ - Agent responses           │
├─────────────────────────────┤
│ Viewport (FIXED 15 lines)   │ ← Ratatui Viewport::Inline(15)
│ - Progress (0-2 lines)      │
│ - Input box (3+ lines)      │
│ - Status line (1 line)      │
│ - EMPTY SPACE = GAP         │ ← The bug
└─────────────────────────────┘
```

Key files:

- `src/main.rs:121` - `const UI_VIEWPORT_HEIGHT: u16 = 15`
- `src/main.rs:125` - `Viewport::Inline(UI_VIEWPORT_HEIGHT)`
- `src/main.rs:156` - `terminal.insert_before()` for chat
- `src/tui/render.rs` - UI rendering, gap positioning hacks

## Ratatui Inline Viewport Internals

From reading ratatui-core source (`~/.cargo/registry/src/*/ratatui-core-0.1.0/src/terminal/terminal.rs`):

**How it works:**

1. `compute_inline_size()` calculates viewport position based on cursor
2. `insert_before()` scrolls content above viewport into scrollback
3. Two implementations: with/without `scrolling-regions` feature
4. Viewport height is FIXED at creation - no resize API

**PR #1964** adds `set_viewport_height()`:

- Would allow dynamic resizing
- NOT MERGED - has issues with `scrolling-regions` feature
- `scroll_up()` method doesn't exist when scrolling-regions enabled
- Maintainers want more tests before merging

**Scrolling-regions feature:**

- Uses DECSTBM (scroll region escape sequences) for efficiency
- Available in ratatui 0.30 but we're not using it
- Codex CLI uses it: `features = ["scrolling-regions", "unstable-backend-writer", ...]`

## Research: How Other Tools Handle This

### Codex CLI (OpenAI)

**Tech stack:** Rust, ratatui, crossterm

**History:**

1. Legacy TUI - Inline viewport cooperating with scrollback
2. TUI2 experiment - Alternate screen, in-memory transcript
3. Current (0.89.0) - Removed TUI2, back to "terminal-native"

**Key finding from their docs:**

> "The legacy TUI attempted to 'cooperate' with terminal scrollback, leading to terminal-dependent behavior, resize failures, content loss, and other issues."

**TUI2 approach:**

> "The new model uses an in-memory transcript as the single source of truth, with scrollback becoming an output target rather than a managed data structure."

But users complained about alternate screen losing scrollback, so they reverted.

**Current approach:** Need to investigate what "terminal-native" means in their current implementation.

**Source:** https://github.com/openai/codex/tree/main/codex-rs/tui

- Uses ratatui with scrolling-regions
- Uses crossterm with bracketed-paste, event-stream

### Claude Code (Anthropic)

**Tech stack:** TypeScript, React, Ink (custom renderer)

**Key points:**

- Originally used Ink's renderer
- Rewrote renderer from scratch for fine-grained incremental updates
- Still had flickering issues (common with Ink)
- Uses React component model but custom terminal output

**Source:** https://newsletter.pragmaticengineer.com/p/how-claude-code-is-built

### Pi-mono (badlogic)

**Tech stack:** TypeScript API → FFI → Zig native

**Architecture:**

- Three-tier: TypeScript API → FFI Boundary → Zig Native
- Differential rendering (only updates changed regions)
- CSI 2026 synchronized output for atomic updates (no flicker)
- `ProcessTerminal` abstraction for low-level I/O
- Frame diffing in Zig, sub-millisecond frame times

**Key design:**

- Components implement `render(width)` returning array of strings
- Editor component has "height-aware scrolling" with TUI reference
- Overlay system with anchor-based positioning

**Source:** https://github.com/badlogic/pi-mono/tree/main/packages/tui

**TODO:** Deep dive into their viewport/scrollback handling

### OpenTUI (SST)

**Tech stack:** TypeScript API → FFI → Zig native (similar to pi-mono)

**Key features:**

- Frame diffing in Zig compares only changed cells
- ANSI generation uses run-length encoding
- Sub-millisecond frame times, 60+ FPS
- Headless testing support

**Source:** https://typevar.dev/articles/sst/opentui

**TODO:** Investigate their viewport model

## Options Identified

### Option A: Fullscreen Mode (Alternate Screen)

Like Codex's TUI2 experiment.

```rust
// Instead of Viewport::Inline
Terminal::new(backend)  // Uses alternate screen by default
```

**Pros:**

- No viewport sizing issues
- Simpler implementation
- Full control over rendering

**Cons:**

- Loses terminal scrollback
- No native Cmd+F search
- Content doesn't persist after exit
- Users complained when Codex tried this

### Option B: Inline + Dynamic Resize (PR #1964)

Use ratatui's PR #1964 for `set_viewport_height()`.

```rust
// Each frame
let needed_height = progress_height + input_height + status_height;
terminal.set_viewport_height(needed_height)?;
terminal.draw(|f| app.draw(f))?;
```

**Pros:**

- Preserves scrollback
- Native terminal features work
- Uses official ratatui (eventually)

**Cons:**

- PR not merged, has bugs
- Would need to fork ratatui or wait
- Still "cooperating with scrollback" - may have terminal-dependent issues

### Option C: Raw Crossterm (No Ratatui)

Drop ratatui entirely, use crossterm directly.

```rust
// Print chat normally
println!("{}", chat_message);

// Position cursor for status area
execute!(stdout, MoveTo(0, term_height - 5))?;
// Draw status area manually
```

**Pros:**

- Full control
- No abstraction fighting us
- Can implement exactly what we need

**Cons:**

- Lose widget abstractions (Paragraph, Block, borders)
- More code to write
- Still need to solve the fundamental scrollback coordination problem

### Option D: Ratatui Widgets + Manual Positioning

Keep ratatui widgets for rendering, but skip `Viewport::Inline`.

```rust
// Print chat to stdout normally
for line in chat_lines {
    println!("{}", line);
}

// Render widgets to buffer
let mut buffer = Buffer::empty(Rect::new(0, 0, width, status_height));
render_widgets(&mut buffer);

// Output buffer at specific position using crossterm
execute!(stdout, SavePosition)?;
execute!(stdout, MoveTo(0, term_height - status_height))?;
output_buffer(&buffer);
execute!(stdout, RestorePosition)?;
```

**Pros:**

- Keep nice widget abstractions
- Full control over positioning
- Scrollback preserved

**Cons:**

- Still need to coordinate with scrollback
- May have same terminal-dependent issues Codex found
- Flicker potential when clearing/redrawing status area

### Option E: Hybrid (In-Memory + Scrollback Dump)

Like Codex's TUI2 but dumping to scrollback.

- Render everything to in-memory buffer
- Use alternate screen for display
- On exit (or periodically), dump transcript to scrollback

**Pros:**

- Clean rendering during use
- Scrollback available after exit

**Cons:**

- No live scrollback search during use
- Complex state management

## The Fundamental Tension

**We want:**

1. Native terminal scrollback (search, persistence)
2. Dynamic viewport that resizes
3. No visual artifacts

**The problem:**

- Inline viewport means ratatui "owns" a fixed portion of screen
- Scrollback is managed by terminal, not us
- Coordinating between the two is inherently fragile
- Different terminals behave differently

**Codex learned this the hard way:**

> "cooperating with terminal scrollback leads to terminal-dependent behavior, resize failures, content loss"

## Questions to Answer

1. **What does Codex's current "terminal-native" UI actually do?**
   - They removed TUI2, went back to something simpler
   - Need to read their current tui/ source

2. **How does pi-mono handle viewport/scrollback?**
   - They have sophisticated differential rendering
   - Do they use inline viewport or something else?

3. **Is there a way to make inline viewport work reliably?**
   - Enable scrolling-regions feature?
   - Smaller fixed viewport (accept small gaps)?
   - Dynamic resize when PR #1964 lands?

4. **Should we accept tradeoffs?**
   - Small gaps might be acceptable
   - Fullscreen with scrollback dump on exit?

## Next Steps

1. **Deep dive into pi-mono's TUI implementation**
   - How do they handle scrollback?
   - What's their viewport model?
   - Source: https://github.com/badlogic/pi-mono/tree/main/packages/tui

2. **Read Codex CLI's current tui/ source**
   - What does "terminal-native" mean?
   - How do they handle the problems they identified?
   - Source: https://github.com/openai/codex/tree/main/codex-rs/tui/src

3. **Look at OpenTUI's approach**
   - Similar architecture to pi-mono
   - May have solved same problems

4. **Prototype Option D**
   - Try ratatui widgets without Viewport::Inline
   - See if manual positioning works

5. **Consider enabling scrolling-regions**
   - Already available in ratatui 0.30
   - Codex uses it
   - Might improve insert_before behavior

## Key Files in Our Codebase

| File                                 | Purpose                                      |
| ------------------------------------ | -------------------------------------------- |
| `src/main.rs:115-170`                | Terminal setup, viewport creation, main loop |
| `src/tui/render.rs`                  | All rendering, layout calculation, gap hacks |
| `src/tui/mod.rs`                     | App struct, state management                 |
| `ai/design/viewport-requirements.md` | Full requirements doc                        |

## References

- Ratatui inline viewport: https://ratatui.rs/examples/apps/inline/
- PR #1964 (set_viewport_height): https://github.com/ratatui/ratatui/pull/1964
- Issue #984 (resize request): https://github.com/ratatui/ratatui/issues/984
- Codex CLI: https://github.com/openai/codex/tree/main/codex-rs
- Pi-mono TUI: https://github.com/badlogic/pi-mono/tree/main/packages/tui
- OpenTUI: https://deepwiki.com/sst/opentui
- Claude Code architecture: https://newsletter.pragmaticengineer.com/p/how-claude-code-is-built
