# Inline Viewport and Scrollback Management in TUI Coding Agents

Research on how terminal coding agents handle dynamic viewport content without leaking into scrollback.

**Date:** 2026-01-23

## Executive Summary

There are two fundamental approaches to TUI rendering in coding agents:

| Approach              | Tools                                   | Pros                                       | Cons                                                   |
| --------------------- | --------------------------------------- | ------------------------------------------ | ------------------------------------------------------ |
| **Alternate Screen**  | opencode, Amp                           | Clean separation, no scrollback corruption | Loses scrollback on exit, must implement custom search |
| **Scrollback Native** | Claude Code, Codex, Gemini CLI, pi-mono | Native scroll/search, history preservation | Resize handling is hard, viewport "leaking" possible   |

**Key insight:** Codex tried a hybrid TUI2 approach but reverted to terminal-native UI due to complexity. Claude Code has active bugs around resize content loss (#18493).

## The Core Problem: Viewport Resize and Scrollback Leaking

When using inline viewport mode (not alternate screen), resizing the terminal causes content corruption:

1. User resizes terminal horizontally
2. Terminal emulator wraps lines that no longer fit
3. Content gets pushed UP into scrollback by the terminal itself
4. Application continues drawing viewport in original position, creating misalignment

**This is a terminal emulator behavior, not an application bug.** The application cannot prevent the terminal from reflowing content when width changes.

## Approach 1: Alternate Screen (Full Ownership)

Used by: **opencode**, **Amp**, **Gemini CLI** (optional mode)

```rust
// Enter alternate screen - complete ownership
execute!(stderr(), EnterAlternateScreen)?;
```

**How it works:**

- Application owns the entire screen buffer
- Terminal scrollback is completely separate
- Resize events simply trigger full redraw at new size
- No content leaking possible

**Gemini CLI's approach:**

- Uses alternate screen buffer by default
- "Your complete chat history is still accessible in your standard terminal after you exit"
- Prints transcript to scrollback on exit (like Codex's suspend behavior)

**Trade-offs:**

- Must implement custom scrolling (can't use terminal's native scroll)
- Must implement custom search/selection
- Clean resize handling

## Approach 2: Scrollback Native (Append-Only)

Used by: **Claude Code**, **Codex**, **pi-mono**

### Claude Code (Ink/React)

Claude Code uses a custom React renderer (originally based on Ink) with incremental rendering:

```typescript
// From Claude Code's architecture
const instance = render(<App />, {
    incrementalRendering: true,  // Only update changed lines
});
```

**Key technique:**

- Write to terminal like any CLI program
- Move "rendering cursor" back up within visible viewport for dynamic content (spinners, input)
- `<Static>` component marks content that won't be re-rendered (becomes scrollback)

**Limitation:**

> "If the first changed line is above the visible viewport (the user scrolled up), they have to do a full clear and re-render. The terminal doesn't let you write to the scrollback buffer above the viewport."

**Known issues (as of 2026-01):**

- Issue #18493: Terminal resize causes content loss when shrinking window (regression in 2.1.9)
- Issue #8618: TUI rendering corruption
- Issue #3648: Terminal scrolling uncontrollably

### OpenAI Codex TUI2 (Abandoned Experiment)

Codex attempted a "TUI2" redesign with these principles:

1. **In-memory transcript is single source of truth** (not terminal state)
2. **Append-only scrollback** - only written on suspend/exit, never during runtime
3. **Cell-based high-water mark** tracks what's been printed to scrollback
4. **Display-time wrapping** - content reflows on resize (no pre-wrapped storage)

```
// From Codex TUI2 architecture
Rendering pipeline:
1. Compute transcript region (terminal height minus input area)
2. Flatten all cells into visual lines with metadata
3. Use scroll state to determine visible slice
4. Clear and redraw only visible portion
```

**Why it was abandoned:**

> "The TUI2 experiment and its related config/docs were removed, keeping Codex on the terminal-native UI (#9640)"

The complexity of managing transcript, selection, scrolling, and terminal state proved too high.

### pi-mono (Custom Differential Rendering)

pi-mono implements a clean scrollback-native approach:

**Key techniques:**

1. **Synchronized output** prevents flicker:

   ```
   \x1b[?2026h ... content ... \x1b[?2026l
   ```

   Terminal buffers output and displays atomically.

2. **Backbuffer tracking** - remembers what was written to know what to diff:

   ```typescript
   // Rendering algorithm
   1. Initial render: output all lines
   2. Width changes: complete clear + re-render
   3. Normal updates: find first changed line, re-render from there
   ```

3. **Viewport boundary enforcement:**
   > "If the first changed line is above the visible viewport (user scrolled up), we have to do a full clear and re-render."

## Approach 3: ratatui Inline Viewport

ratatui provides `Viewport::Inline(height)` with `insert_before()` for scrollback management.

### Current State (v0.29+)

**Scrolling regions feature** reduces flicker in `insert_before`:

```rust
// Uses terminal scrolling regions internally
// Creates region above viewport, scrolls up, draws new lines
scroll_region_up(n) // New backend method
scroll_region_down(n) // New backend method
```

### Known Limitations

**Horizontal resize problem** (Issue #2086):

When terminal shrinks horizontally:

1. Terminal wraps lines that no longer fit
2. Content gets pushed UP
3. Viewport position becomes misaligned

**Proposed solutions:**

1. **Scroll Down:** Keep viewport in original location by scrolling back
   - Creates whitespace gap in history
   - Requires `scrolling-regions` feature

2. **Move Viewport Up:** Stick to prompt
   - No extra terminal features needed
   - Causes "app creeping upward over time"

3. **Draft PR #2355:** Clear entire screen and move viewport to top when window shrinks

**Vertical resize** works better with `set_viewport_height` (PR #1964):

- Growing: pushes scroll buffer down to make room
- Shrinking: content above viewport preserved

### Example Pattern

```rust
let mut terminal = ratatui::init_with_options(TerminalOptions {
    viewport: Viewport::Inline(8),  // Fixed 8-line viewport
});

// Dynamic content in viewport
terminal.draw(|frame| {
    let [input_area, status_area] = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(1),
    ]).areas(frame.area());

    frame.render_widget(input_widget, input_area);
    frame.render_widget(status_line, status_area);
})?;

// Completed content pushed to scrollback
terminal.insert_before(1, |buf| {
    Paragraph::new(completed_message).render(buf.area, buf);
})?;
```

## Recommendations for ion

### Option A: Alternate Screen with Exit Transcript (Recommended)

Like Gemini CLI's approach:

1. Use `Viewport::Fullscreen` (alternate screen)
2. Implement custom scrolling within the TUI
3. On exit/suspend: print full transcript to scrollback
4. Clean resize handling, no content leaking

```rust
impl Tui {
    fn suspend(&mut self) {
        // Exit alternate screen
        execute!(stderr(), LeaveAlternateScreen)?;

        // Print transcript to scrollback
        for message in &self.transcript {
            println!("{}", format_message(message));
        }

        // Re-enter alternate screen
        execute!(stderr(), EnterAlternateScreen)?;
    }

    fn exit(&mut self) {
        execute!(stderr(), LeaveAlternateScreen)?;
        terminal::disable_raw_mode()?;

        // Final transcript dump
        for message in &self.transcript {
            println!("{}", format_message(message));
        }
    }
}
```

### Option B: Inline Viewport with Resize Workaround

If native scrollback during session is required:

1. Use `Viewport::Inline(height)` for status/input area only
2. Handle resize by clearing and moving viewport to top
3. Accept some visual disruption on resize
4. Use synchronized output for flicker prevention

```rust
impl Tui {
    fn handle_resize(&mut self, new_width: u16, new_height: u16) {
        if new_width < self.last_width {
            // Horizontal shrink - full redraw from top
            execute!(
                stderr(),
                terminal::Clear(ClearType::All),
                cursor::MoveTo(0, 0)
            )?;
            // Recreate viewport at new position
            self.recreate_viewport(new_height)?;
        }
        self.last_width = new_width;
    }
}
```

### Option C: Hybrid Mode (User Configurable)

Offer both modes via config:

```toml
[tui]
mode = "fullscreen"  # or "inline"
```

## Key Implementation Details

### Synchronized Output (Flicker Prevention)

```rust
// Wrap rendering in synchronized output
write!(stderr(), "\x1b[?2026h")?;  // Begin synchronized
// ... all rendering ...
write!(stderr(), "\x1b[?2026l")?;  // End synchronized
```

**Note:** Not all terminals support this. Fallback: rate-limit renders.

### Input Box Anchoring

For inline mode, the input area must stay at bottom:

```rust
// Calculate input area position
let input_y = terminal_height.saturating_sub(INPUT_HEIGHT);

// Always draw input at this position
execute!(stderr(), cursor::MoveTo(0, input_y))?;
```

### Content-Anchored Selection (Codex TUI2 Insight)

If implementing selection:

- Track selection by (cell_id, offset) not (screen_row, screen_col)
- When content scrolls, selection moves with it
- Prevents selection from "jumping" to wrong content

## Sources

- [ratatui Issue #2086 - Resize for inline viewport](https://github.com/ratatui/ratatui/issues/2086)
- [ratatui PR #1964 - set_viewport_height](https://github.com/ratatui/ratatui/pull/1964)
- [ratatui PR #1341 - scrolling-regions](https://github.com/ratatui/ratatui/pull/1341)
- [ratatui Issue #984 - Allow inline viewport resize](https://github.com/ratatui/ratatui/issues/984)
- [OpenAI Codex TUI2 Rework](https://gitmemories.com/openai/codex/issues/7601)
- [Codex Release Notes - TUI2 removed](https://github.com/openai/codex/releases/tag/rust-v0.78.0)
- [Claude Code Issue #18493 - Resize content loss](https://github.com/anthropics/claude-code/issues/18493)
- [Gemini CLI - Making Terminal Beautiful](https://developers.googleblog.com/en/making-the-terminal-beautiful-one-pixel-at-a-time/)
- [pi-mono TUI Architecture](https://mariozechner.at/posts/2025-11-30-pi-coding-agent/)
- [ratatui Inline Example](https://ratatui.rs/examples/apps/inline/)
