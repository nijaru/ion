# TUI v2 Architecture

**Date:** 2026-01-26
**Status:** Planning

## Overview

Replace ratatui's `Viewport::Inline` with custom terminal management for proper native scrollback support.

## Architecture

```
┌─────────────────────────────────────────┐
│  Native Terminal Scrollback             │  ← Just println!()
│  - Chat history                         │  ← Terminal manages this
│  - Tool output                          │  ← Searchable with Cmd+F
│  - Agent responses                      │  ← Persists after exit
├─────────────────────────────────────────┤
│  Managed Bottom Area                    │  ← We control this
│  - [Optional] Autocomplete candidates   │
│  - [Optional] Selector UI               │
│  - Progress line                        │
│  - Input area (TOP|BOTTOM borders)      │
│  - Status line                          │
└─────────────────────────────────────────┘
```

## Key Principles

1. **Native scrollback is native** - just print to stdout, terminal handles it
2. **No Viewport abstractions** - we manage cursor position directly
3. **Dynamic bottom area** - grows/shrinks for selectors, autocomplete
4. **Synchronized output** - atomic updates prevent flicker

## Rendering Approach

### Chat Output (Native Scrollback)

```rust
// Simple - just print
println!("{}", format_user_message(&msg));
println!("{}", format_agent_response(&response));
println!("{}", format_tool_output(&output));
```

### Bottom Area (Managed)

```rust
fn render_bottom_ui(&mut self) -> Result<()> {
    let height = self.calculate_ui_height();
    let start_row = self.terminal_height - height;

    // Begin synchronized output (flicker prevention)
    write!(stdout, "\x1b[?2026h")?;

    // Clear our area
    execute!(stdout, MoveTo(0, start_row), Clear(ClearType::FromCursorDown))?;

    // Draw components
    self.render_progress(start_row)?;
    self.render_input(start_row + progress_height)?;
    self.render_status(start_row + progress_height + input_height)?;

    // End synchronized output
    write!(stdout, "\x1b[?2026l")?;

    stdout.flush()
}
```

### Height Management

```rust
fn calculate_ui_height(&self) -> u16 {
    let mut height = 0;

    // Progress: 0-2 lines
    height += self.progress_height();

    // Input: 3+ lines (grows with multi-line)
    height += self.input_height();

    // Status: 1 line
    height += 1;

    // Selector: 0 or 10-15 lines when open
    if self.selector_open {
        height += self.selector_height();
    }

    // Autocomplete: 0 or N lines when showing
    height += self.autocomplete_height();

    height
}
```

### Shrinking Without Artifacts

When UI shrinks, we must clear the old larger area:

```rust
fn render_with_height_change(&mut self) -> Result<()> {
    let new_height = self.calculate_ui_height();
    let clear_height = new_height.max(self.previous_ui_height);
    let clear_start = self.terminal_height - clear_height;

    // Clear from where the larger UI was
    execute!(stdout, MoveTo(0, clear_start), Clear(ClearType::FromCursorDown))?;

    // Draw at new position
    let draw_start = self.terminal_height - new_height;
    self.render_at(draw_start)?;

    self.previous_ui_height = new_height;
    Ok(())
}
```

## Flicker Prevention

### Primary: Synchronized Output (CSI 2026)

```rust
write!(stdout, "\x1b[?2026h")?;  // Begin - terminal buffers
// ... all drawing ...
write!(stdout, "\x1b[?2026l")?;  // End - terminal displays atomically
```

**Supported by:** Ghostty, Kitty, iTerm2, Windows Terminal, WezTerm, Warp

### Research Needed: Diffing vs Redraw

For 5-15 line bottom area, options:

| Approach                  | Pros                      | Cons                                     |
| ------------------------- | ------------------------- | ---------------------------------------- |
| Full redraw + sync output | Simple, no state          | May have issues on unsupported terminals |
| Line-level diffing        | Only update changed lines | More complex, track previous state       |
| Cell-level diffing        | Minimal updates           | Most complex, ratatui does this          |

**TODO:** Research and benchmark. See `ai/research/tui-rendering-research.md`

## Components

### Input Area

- TOP | BOTTOM borders only (for copy-paste UX)
- Multi-line support with dynamic height
- Cursor positioning within input
- Gutter with `>` prompt

### Progress Line

- Spinner animation
- Elapsed time
- Token count
- Cancel hint
- Thinking indicator

### Status Line

- Tool mode indicator [READ/WRITE/AGI]
- Model name
- Context usage (tokens/max)
- Help hint

### Selector UI

When open, expands bottom area:

```
┌─ Filter: gpt ──────────────────────────┐
│ > gpt-4o                               │
│   gpt-4o-mini                          │
│   gpt-4-turbo                          │
│   gpt-3.5-turbo                        │
└────────────────────────────────────────┘
```

- Fuzzy filtering
- Arrow key navigation
- Enter to select, Esc to cancel

### Autocomplete

Appears above input when triggered:

```
  /help
  /clear
  /model
> /provider
───────────────────
> /pr|
```

## OS Keybindings

Must handle terminal-specific escape sequences:

| Action      | macOS    | Sequence (Ghostty)      |
| ----------- | -------- | ----------------------- |
| Word left   | Option+← | `\x1b[1;3D` or `\x1bb`  |
| Word right  | Option+→ | `\x1b[1;3C` or `\x1bf`  |
| Delete word | Option+⌫ | `\x1b\x7f`              |
| Line start  | Cmd+←    | `\x1b[H` or `\x1b[1;2D` |
| Line end    | Cmd+→    | `\x1b[F` or `\x1b[1;2C` |

**TODO:** Research terminal-specific sequences. See task tk-bmd0.

## Migration Path

1. **Phase 1:** Research rendering (diffing vs redraw)
2. **Phase 2:** Implement basic bottom area management
3. **Phase 3:** Port input handling from current TUI
4. **Phase 4:** Port selector UI
5. **Phase 5:** Add autocomplete
6. **Phase 6:** Remove ratatui dependency (optional)

## Open Questions

1. Should we keep ratatui for widgets (Block, Paragraph) or go pure crossterm?
2. What's the minimum terminal support we need? (sync output fallback?)
3. How do we handle terminal resize during selector?

## References

- `ai/research/inline-tui-patterns-2026.md` - Pattern research
- `ai/research/codex-tui-analysis.md` - Codex approach
- `ai/research/tui-state-of-art-2026.md` - SOTA survey
