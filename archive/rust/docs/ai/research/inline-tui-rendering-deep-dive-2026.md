# Inline Terminal TUI Rendering: Deep Dive

**Date:** 2026-02-05
**Purpose:** Comprehensive research on inline TUI rendering patterns, resize handling, scrollback management, and state-of-the-art approaches for chat-like terminal applications.

---

## 1. Claude Code's TUI Approach

### Architecture

Claude Code uses React as its component model with a **custom terminal renderer** (not stock Ink). The rendering pipeline:

```
React tree -> layout elements -> rasterize to 2D screen buffer -> diff against previous screen -> generate ANSI sequences from diff
```

This runs within a ~16ms frame budget, with ~5ms for React-to-terminal conversion.

### Scrollback Strategy

Claude Code uses the **main screen** (not alternate screen) to preserve native scrollback. Key design choices:

- **`<Static>` component**: Ink's mechanism for permanently rendering content above the dynamic UI. Once rendered, Static content enters the terminal's scrollback and is never re-rendered.
- **Dynamic area**: The bottom portion (input, progress, status) is redrawn each frame using cursor repositioning.
- **Fundamental constraint**: "There's no way to incrementally update scrollback in a terminal." When scrollback changes, the system must "clear entirely and redraw everything when it changes."

### Differential Renderer (shipped Dec 2025)

The original Ink renderer caused severe flickering (4,000-6,700 scroll events/second in tmux). The rewrite:

1. **Double buffering**: Front buffer (current screen) and back buffer (previous screen). Only cells that differ between buffers produce ANSI output.
2. **Packed TypedArrays**: Screen buffers stored as packed arrays to reduce GC pressure.
3. **Better memoization**: Prevents unnecessary re-renders of static subtrees.
4. **Result**: ~85% reduction in flickering; only ~1/3 of sessions see any flicker at all.

### Resize Handling

Claude Code performs a **full clear and redraw** on resize. Known issues:

- Issue #11260: Terminal scrollback not cleared on `/clear`, old content reappears on resize.
- Issue #18493: Content loss when shrinking window (regression in 2.1.9).
- The terminal reflows scrollback before the app receives SIGWINCH, causing misalignment.

### Synchronized Output

Anthropic pushed upstream patches to VS Code terminal and tmux for DEC mode 2026 support. When available, `CSI ?2026 h` / `CSI ?2026 l` "totally eliminates flickering."

---

## 2. Other Inline Terminal Applications

### fzf (`--height` mode)

fzf's `LightRenderer` (`src/tui/light.go`) is the canonical example of inline terminal rendering done right.

**How `--height` works:**

1. Query cursor position via DSR (`\x1b[6n`) to find where the prompt is.
2. Calculate available space below cursor based on `--height` value.
3. Make space: print enough newlines to push content down, creating room.
4. Track `yoffset` for mouse events and rendering coordinates.
5. Render within the allocated region using **relative cursor movement** (`\x1b[nA`, `\x1b[nB`, `\x1b[nC`, `\r`).

**Key: fzf does NOT use DECSTBM scroll regions.** Instead it uses absolute positioning within its allocated window boundaries, with wrapping handled per-window.

**Cleanup on exit:**

- Restore cursor to saved position (`\x1b[u`).
- Clear from cursor down (`\x1b[J`).
- This removes fzf's UI while preserving everything above it.

**Resize:** `updateTerminalSize()` recalculates dimensions but does not actively monitor SIGWINCH; relies on explicit refresh calls. The `maxHeightFunc` callback allows dynamic height recalculation.

### Atuin

Atuin opens its TUI **below the cursor** (inline), pushing the shell prompt up. It uses DECSTBM-free rendering similar to fzf's approach. Known issue: the TUI shifts the prompt up when opening, which some users find disorienting.

### lazygit / Alternate Screen Tools

lazygit, htop, vim, and similar tools use the **alternate screen buffer**. They own the entire viewport, implement custom scrolling, and restore the main screen on exit. This sidesteps all inline rendering challenges.

### pi-mono (pi-tui)

Mario Zechner's approach, directly relevant to ion:

1. **Retained mode components**: Each component has `render(width)` returning `string[]` with ANSI codes. Components cache output when unchanged.
2. **Three-strategy rendering**:
   - Initial render: full output to terminal
   - Width change: full clear (`\x1b[3J\x1b[2J\x1b[H`) and re-render
   - Content change: find first changed line, cursor there, re-render from that point
3. **Synchronized output**: All rendering wrapped in `CSI ?2026 h` / `CSI ?2026 l`.
4. **Acknowledged limitation**: "If the first changed line is above the visible viewport (user scrolled up), we have to do a full clear and re-render."

---

## 3. Terminal Escape Sequence Best Practices

### Clearing Content Without Destroying Scrollback

| Sequence               | Effect                                                           | Scrollback Impact                     |
| ---------------------- | ---------------------------------------------------------------- | ------------------------------------- |
| `CSI 2J` (Clear All)   | Clears visible screen                                            | Scrollback preserved                  |
| `CSI 3J`               | Clears scrollback buffer                                         | **Destroys scrollback**               |
| `CSI J` (Clear Below)  | Clears from cursor to end of screen                              | Scrollback preserved                  |
| `CSI 1J` (Clear Above) | Clears from cursor to top of screen                              | Scrollback preserved                  |
| `CSI K` (Clear Line)   | Clears current line from cursor                                  | Scrollback preserved                  |
| `ScrollUp(n)`          | Pushes top n rows into scrollback, creates blank space at bottom | Content enters scrollback (preserved) |

**Best practice for inline apps:**

- Never use `CSI 3J` unless explicitly clearing history.
- Use `ScrollUp(n)` to push content into scrollback (preserves it, user can scroll up).
- Use `CSI J` (clear below cursor) to erase only your app's UI area.
- Use `CSI K` for surgical line-level clearing.

### ScrollUp vs Clear vs MoveTo

```
ScrollUp(n):
  - Pushes top n lines of viewport INTO scrollback (preserved)
  - Creates n blank lines at bottom of viewport
  - All cursor-addressed content shifts up by n lines
  - Use for: committing content to scrollback, making room for new content

Clear(FromCursorDown):
  - Erases everything from cursor position to end of screen
  - Does NOT affect scrollback
  - Does NOT move content
  - Use for: clearing your app's UI area before redraw

MoveTo(col, row):
  - Absolute positioning within viewport (0-indexed)
  - Row 0 is top of visible viewport, NOT top of scrollback
  - Use for: positioning cursor to draw UI elements
```

### Tracking Your App's Position

**Pattern 1: Cursor Position Query (DSR)**

```
Send: \x1b[6n
Receive: \x1b[row;colR
```

Tells you where the cursor currently is. Used by fzf to find the starting position. Caveat: requires reading from stdin, which means you need to distinguish terminal responses from user input.

**Pattern 2: Row Tracking (ion's current approach)**
Track a `chat_row` value that represents where your content ends. The UI draws at `chat_row` and below. When content is added, `chat_row` advances. When it hits the bottom, transition to scroll mode.

**Pattern 3: Anchor-Based (ratatui inline viewport)**
The viewport is a fixed-height region. `insert_before()` pushes content above it. The viewport stays at the bottom. The system tracks how many rows have been inserted to calculate its position.

### Row-Tracking to Scroll-Mode Transition

This is the critical moment. When content grows beyond what fits between the top of the viewport and the UI area:

```
Before (row-tracking mode):
Row 0: [header]
Row 1: [message 1]
Row 2: [message 2]
Row 3: [chat_row = 3] <-- UI starts here
...
Row N: [bottom of screen]

After overflow (scroll mode):
[scrollback: header, message 1, ...]
Row 0: [recent messages...]
Row K: [UI starts at height - ui_height]
Row N: [bottom of screen]
```

The transition requires:

1. Clear old UI area (so border/status lines don't get pushed into scrollback).
2. `ScrollUp(overflow_amount)` to push excess content into scrollback.
3. Print remaining content above the new UI position.
4. Set `chat_row = None` (scroll mode).

---

## 4. Fundamental Constraints

### Can You Selectively Clear Parts of Scrollback?

**No.** The terminal's scrollback buffer is opaque to applications. You can:

- Clear the entire scrollback (`CSI 3J`) - destructive
- Push new content into scrollback (`ScrollUp`)
- Print content that enters scrollback naturally

You **cannot**:

- Modify specific lines in scrollback
- Read what's in scrollback
- Selectively delete scrollback entries
- Re-render content that has scrolled off the viewport

### What Does Terminal Reflow Actually Do on Resize?

**Width decrease:**

1. Lines wider than new width get soft-wrapped onto multiple lines.
2. This increases the total line count.
3. Content shifts DOWN (more lines = content pushes below viewport).
4. Content may get pushed INTO scrollback.
5. This happens BEFORE the application receives SIGWINCH.

**Width increase:**

1. Previously soft-wrapped lines may rejoin (unwrap).
2. Total line count decreases.
3. Content shifts UP.
4. Blank lines may appear at bottom.

**Key insight:** "Scrollback is essentially frozen during reflow." Only the visible screen participates in reflow. However, some modern terminals (kitty, ghostty) do reflow scrollback too, with varying correctness.

### Cursor Position After Resize

Behavior varies wildly across terminals:

| Terminal         | Cursor After Resize                       |
| ---------------- | ----------------------------------------- |
| xterm            | Stays at same row/col (may be off-screen) |
| kitty            | Reflowed with content (usually correct)   |
| ghostty          | Reflowed with content (some edge cases)   |
| iTerm2           | Stays at same row (col may shift)         |
| tmux             | Inconsistent; depends on version          |
| Windows Terminal | Reflowed; known issues with DECSC/DECRC   |

**Saved cursor (DECSC/DECRC):** "The behavior of the saved cursor position in the context of text reflow is unspecified and varies wildly between terminals." Do not rely on saved cursor surviving a resize.

### Standard Behavior for Inline Apps on Resize

There is no standard. The practical approach used by working applications:

1. **Detect resize** via SIGWINCH or polling terminal size.
2. **Do NOT trust cursor position** after resize.
3. **Full redraw for width changes** (content reflow invalidates all positioning).
4. **Recalculate UI position** for height changes (simpler, content unchanged).
5. **Accept some visual disruption** during resize (flicker, momentary misalignment).
6. **Use synchronized output** to minimize visible artifacts.

---

## 5. State-of-the-Art Patterns for Inline Chat TUIs

### The Ideal Architecture

Based on analyzing Claude Code, pi-mono, Codex, ion, and ratatui:

```
Terminal Structure:
+============================+ <- Top of scrollback (not accessible)
| Completed messages         |    (native terminal scrollback)
| Completed tool outputs     |    (user scrolls here naturally)
| ...                        |
+============================+ <- Top of visible viewport
| Recent completed content   |    (in viewport but will scroll up)
| ...                        |
+----------------------------+
| [Streaming response area]  |    (managed: redrawn each frame)
+----------------------------+
| progress: spinner elapsed  |    (managed: redrawn each frame)
| ========================== |    (border)
|  > user input              |    (managed: redrawn each frame)
| ========================== |    (border)
| [status] model tokens      |    (managed: redrawn each frame)
+----------------------------+ <- Bottom of screen
```

### Pattern: Two-Mode Rendering (ion's current approach, validated)

**Row-tracking mode** (content fits on screen):

- Print content at tracked absolute positions.
- UI follows content (drawn just below last message).
- No scrolling needed.
- Clean, no visual artifacts.

**Scroll mode** (content exceeds screen):

- Clear UI area first (prevent border leaking into scrollback).
- `ScrollUp(n)` to make room.
- Print new content in the cleared space.
- Redraw UI at fixed bottom position.

**Transition:** When `chat_row + new_lines + ui_height > term_height`, switch from row-tracking to scroll mode.

### Pattern: Synchronized Output Wrapping

```rust
// All visual updates within sync boundaries
execute!(w, BeginSynchronizedUpdate)?;
// ... all rendering (chat inserts, UI redraw) ...
execute!(w, EndSynchronizedUpdate)?;
w.flush()?;
```

Terminal support is now broad (Ghostty, Kitty, iTerm2, WezTerm, Alacritty, Windows Terminal, Warp, Zellij). Unsupported terminals ignore the sequences harmlessly.

### Pattern: Resize Handling (Three Strategies)

**Strategy 1: Width change (full reflow)**

```rust
// Push everything to scrollback, blank the viewport, reprint
ScrollUp(term_height);
MoveTo(0, 0);
// Reprint last N lines of chat that fit in viewport
// Re-enter row-tracking mode
```

**Strategy 2: Height change only (reposition)**

```rust
// Just recalculate UI position
let new_ui_start = new_height.saturating_sub(ui_height);
// Redraw UI at new position
```

**Strategy 3: Content update (differential)**

```rust
// Only update changed lines in the managed area
// Scrollback content is untouched
```

### Pattern: Chat Insert (insert_before equivalent)

The core primitive for inline chat TUIs. When new content arrives:

```rust
// In scroll mode:
let ui_height = calculate_ui_height();
let ui_start = term_height - ui_height;

// 1. Clear old UI (prevent it from entering scrollback)
execute!(w, MoveTo(0, ui_start), Clear(FromCursorDown))?;

// 2. Scroll up to make room for new content
execute!(w, ScrollUp(line_count))?;

// 3. Print new content in the gap
let print_start = ui_start - line_count;
for (i, line) in new_lines.iter().enumerate() {
    execute!(w, MoveTo(0, print_start + i))?;
    line.writeln(w)?;
}

// 4. Redraw UI at bottom
draw_ui(w, term_width, term_height)?;
```

This is exactly what ion does today (validated by this research as the correct pattern).

### Anti-Pattern: DECSTBM Scroll Regions for Scrollback

Using `CSI t ; b r` to set scroll regions seems like the obvious solution for a fixed bottom area. However, **content scrolled out of a DECSTBM scroll region does NOT enter the terminal's native scrollback buffer**. It is simply lost.

DECSTBM is useful for:

- Preventing UI elements from scrolling when inserting lines (ratatui uses this internally for `insert_before`).
- Creating contained scroll areas within fullscreen apps.

DECSTBM is NOT useful for:

- Preserving chat history in native scrollback while maintaining a fixed bottom area.

### Anti-Pattern: Trusting Cursor Position After Resize

Cursor position after resize is unreliable across terminals. Instead:

- Track position in application state (ion's `chat_row` / `RenderState`).
- On resize, recalculate from scratch rather than adjusting the saved position.
- Exception: One-time DSR query at startup to find initial position (fzf pattern) is fine.

---

## 6. Ion's Current Implementation Assessment

Ion's implementation in `/Users/nick/github/nijaru/ion/src/tui/run.rs` and `/Users/nick/github/nijaru/ion/src/tui/render_state.rs` already implements the state-of-the-art pattern:

| Feature                | Ion                 | Claude Code          | pi-mono           | Codex              |
| ---------------------- | ------------------- | -------------------- | ----------------- | ------------------ |
| Inline (no alt screen) | Yes                 | Yes                  | Yes               | Yes                |
| Row-tracking mode      | Yes                 | No (always scroll)   | No                | No                 |
| Scroll mode            | Yes                 | Yes                  | Yes               | Yes                |
| Two-mode transition    | Yes                 | N/A                  | N/A               | N/A                |
| Synchronized output    | Yes                 | Patching upstream    | Yes               | No                 |
| Clear UI before scroll | Yes                 | N/A (React)          | N/A               | Via scroll regions |
| Width-change reflow    | Yes (full repaint)  | Full redraw          | Full clear+redraw | Full redraw        |
| Differential rendering | No (full UI redraw) | Yes (2D buffer diff) | Yes (line diff)   | Via ratatui        |

### What ion does well:

1. **Two-mode rendering** is elegant and correct. Row-tracking for small conversations, scroll mode for long ones. No other tool does this.
2. **Clear UI before ScrollUp** prevents border/status lines from leaking into scrollback.
3. **Synchronized output** wrapping is correctly placed around all visual updates.
4. **Startup anchor** tracking provides clean initial state before first message.
5. **Reflow on resize** correctly rebuilds chat at new width using ScrollUp + reprint.

### Potential improvements:

1. **Differential UI rendering**: Currently redraws the entire bottom UI each frame. A back-buffer comparison could skip unchanged cells, reducing flicker on terminals without CSI 2026 support.
2. **Debounced resize**: Multiple rapid SIGWINCH events cause multiple reflows. Debouncing (e.g., 100ms) would batch these.
3. **Streaming area**: Currently, streaming responses are buffered until complete. Rendering them in the managed area (above input, below scrollback) would provide live feedback. This is the biggest UX gap vs Claude Code / pi-mono.

---

## Sources

### Claude Code

- [Claude Chill flickering fix (HN discussion with Anthropic engineers)](https://news.ycombinator.com/item?id=46699072)
- [Boris Cherny thread on differential renderer](https://www.threads.com/@boris_cherny/post/DSZbZatiIvJ/)
- [Claude Code scrollback issues (#2479)](https://github.com/anthropics/claude-code/issues/2479)
- [Terminal scrollback not cleared on resize (#11260)](https://github.com/anthropics/claude-code/issues/11260)
- [Excessive scroll events in multiplexers (#9935)](https://github.com/anthropics/claude-code/issues/9935)
- [Peter Steinberger: The Signature Flicker](https://steipete.me/posts/2025/signature-flicker)

### Terminal Fundamentals

- [XTerm Control Sequences (canonical reference)](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html)
- [Ghostty DECSTBM documentation](https://ghostty.org/docs/vt/csi/decstbm)
- [Synchronized Output Terminal Spec](https://gist.github.com/christianparpart/d8a62cc1ab659194337d73e399004036)
- [Richer CLI Applications (ballingt.com)](https://ballingt.com/rich-terminal-applications-2/)
- [Terminal specs for TUI development (dev.to)](https://dev.to/bmf_san/understanding-terminal-specifications-to-help-with-tui-development-749)

### Resize Behavior

- [Kitty: Cursor position incorrect after resize with reflow (#8325)](https://github.com/kovidgoyal/kitty/issues/8325)
- [Ghostty: Resize with reflow and saved cursor (#5718)](https://github.com/ghostty-org/ghostty/issues/5718)
- [Microsoft Terminal: ResizeWithReflow (#4200)](https://github.com/microsoft/terminal/issues/4200)
- [mintty: Line rebreaking on resize (#82)](https://github.com/mintty/mintty/issues/82)

### Inline TUI Implementations

- [fzf LightRenderer (src/tui/light.go)](https://github.com/junegunn/fzf/blob/master/src/tui/light.go)
- [fzf --height mode documentation](https://github.com/junegunn/fzf/blob/master/ADVANCED.md)
- [pi-mono TUI architecture](https://mariozechner.at/posts/2025-11-30-pi-coding-agent/)
- [ratatui inline viewport: Terminal::insert_before](https://docs.rs/ratatui/latest/ratatui/struct.Terminal.html)
- [ratatui insert_lines_before proposal (#1426)](https://github.com/ratatui/ratatui/issues/1426)
- [TerminalScrollRegionsDisplay (Python DECSTBM demo)](https://github.com/pdanford/TerminalScrollRegionsDisplay)
- [Ink React terminal renderer](https://github.com/vadimdemedes/ink)

### Scrollback Constraints

- [Alternate screen should not write to scrollback (MS Terminal #3492)](https://github.com/microsoft/terminal/issues/3492)
- [Notcurses: Scrolling rendered mode discussion](https://github.com/dankamongmen/notcurses/discussions/1853)
- [Codex inline rendering request (#2836)](https://github.com/openai/codex/issues/2836)

### Prior Ion Research

- [/Users/nick/github/nijaru/ion/ai/research/inline-viewport-scrollback-2026.md](inline-viewport-scrollback-2026.md)
- [/Users/nick/github/nijaru/ion/ai/research/tui-resize-streaming-research.md](tui-resize-streaming-research.md)
- [/Users/nick/github/nijaru/ion/ai/research/pi-mono-tui-analysis.md](pi-mono-tui-analysis.md)
