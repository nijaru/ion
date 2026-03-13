# Pi-Mono TUI Implementation Analysis

**Research Date**: 2026-01-26
**Purpose**: Understand pi-mono's TUI approach for potential adaptation to ion (Rust/ratatui)
**Source**: https://github.com/badlogic/pi-mono/tree/main/packages/tui

---

## Executive Summary

Pi-mono implements a **scrollback-native inline TUI** with differential rendering. Key architectural decisions:

| Aspect                | Pi-Mono Approach                                                       |
| --------------------- | ---------------------------------------------------------------------- |
| Rendering Model       | Inline (scrollback-native), NOT alternate screen                       |
| Viewport Coordination | Container-based hierarchy, TUI manages positioning automatically       |
| Dynamic Height        | Components return variable-length line arrays; TUI aggregates          |
| Input Area            | Fixed position at container bottom; overlays for modals                |
| Artifact Prevention   | Synchronized output (CSI 2026), explicit spacers, differential updates |

**Applicability to Rust TUI**: The architecture is sound but relies on TypeScript's flexibility and a custom rendering engine. Rust/ratatui would need significant adaptation, particularly around dynamic viewport sizing which ratatui does not natively support for inline viewports.

---

## 1. Viewport/Scrollback Coordination

### The Core Insight

Pi-mono does NOT "manage" scrollback in the traditional sense. Instead:

1. **Content scrolls naturally** - As new content is added, terminal scrollback grows
2. **TUI tracks rendered state** - Maintains `maxLinesRendered` and `cursorRow` internally
3. **Viewport is computed, not fixed** - `viewportTop = max(0, maxLinesRendered - terminalHeight)`

### Key State Variables

```typescript
cursorRow; // End of rendered content (logical position)
hardwareCursorRow; // Actual terminal cursor (for IME)
maxLinesRendered; // Terminal's working area (monotonically grows)
previousLines; // Cached previous render for diffing
previousViewportTop; // For computing cursor movements between frames
```

### Scrollback Preservation

- **Normal updates**: Only changed lines are redrawn, scrollback untouched
- **Width changes**: Full clear with `\x1b[3J\x1b[2J\x1b[H` (clears scrollback, screen, homes cursor)
- **Scrolled-up user**: If first changed line is above viewport, full redraw required

**Critical limitation acknowledged by author**:

> "If the first changed line is above the visible viewport (user scrolled up), we have to do a full clear and re-render. The terminal doesn't let you write to the scrollback buffer above the viewport."

---

## 2. Rendering Model (Inline vs Fullscreen)

### Deliberate Choice: Inline

Pi-mono explicitly chose inline rendering over fullscreen. From the author's blog post:

> "There are basically two ways to build a terminal user interface. One is to take ownership of the terminal viewport and treat it like a pixel buffer... The drawback is that you lose the scrollback buffer, which means you have to implement custom search."

**Fullscreen TUIs** (Amp, opencode): Alternate screen, custom scrolling, lose terminal features.

**Inline TUIs** (pi-mono, Claude Code, Codex): Append to scrollback, preserve terminal features.

### Three-Strategy Rendering

```
Strategy 1 (Initial):     Output all lines, no clear
Strategy 2 (Width Change): \x1b[3J\x1b[2J\x1b[H + full repaint
Strategy 3 (Incremental):  Move to first changed line, clear to EOS, render diff
```

All strategies wrapped in synchronized output (`\x1b[?2026h` ... `\x1b[?2026l`).

### Why This Works for Chat UIs

The inline model maps naturally to chat interfaces:

- User prompt -> agent response -> tool calls -> results
- Sequential, append-only content flow
- Native scrollback = native search (Cmd+F)
- Mouse scroll "just works"

---

## 3. Dynamic Content Height

### Component Contract

```typescript
interface Component {
  render(width: number): string[]; // Returns N lines, must fit width
  handleInput?(data: string): void; // Optional input handler
  invalidate?(): void; // Clear cached state
}
```

Components return variable-length line arrays. The TUI aggregates all children vertically:

```
TUI.render():
  lines = []
  for child in children:
    lines.extend(child.render(width))
  return lines
```

### No Fixed Viewport Height

Unlike ratatui's `Viewport::Inline(height)`, pi-mono has no fixed viewport concept. The "viewport" is simply the terminal's visible area, and content grows/shrinks naturally.

**Key difference from ratatui**:

- ratatui: Fixed viewport height, content renders within bounds
- pi-mono: Content determines height, terminal manages scrollback

### Editor Component Height

The Editor component calculates its own height dynamically:

```typescript
const maxVisibleLines = Math.max(5, Math.floor(terminalRows * 0.3));
```

Reserves ~30% of terminal for display, with minimum 5 lines. Implements internal scrolling with "N more" indicators when content exceeds visible area.

---

## 4. Input Area at Bottom

### Container Hierarchy

```
TUI (root)
  |-- Spacer(1)
  |-- Header
  |-- Spacer(1)
  |-- chatContainer        // Messages accumulate here
  |-- pendingMessagesContainer
  |-- statusContainer      // Loading/status
  |-- widgetContainerAbove // Extension widgets
  |-- editorContainer      // INPUT AREA
  |-- widgetContainerBelow
  |-- Footer
```

### How It Stays at Bottom

The architecture relies on **vertical stacking order**:

1. Messages (`chatContainer`) accumulate, pushing content down
2. Input area (`editorContainer`) is added AFTER message containers
3. As messages grow, they scroll up into scrollback
4. Input area remains at logical "bottom" of rendered content

**No absolute positioning**. The input stays at bottom because it's the last rendered component (before footer).

### Hot-Swappable Input

The `editorContainer` supports dynamic replacement:

- Selectors overlay the editor temporarily
- Extension inputs replace default editor
- Original state preserved across switches

---

## 5. Gap/Artifact Prevention

### Technique 1: Synchronized Output

```
\x1b[?2026h  // Begin synchronized output
... all rendering ...
\x1b[?2026l  // End synchronized output
```

Terminal buffers all output, displays atomically. Prevents partial render visibility.

**Note**: Not all terminals support CSI 2026. Fallback: rate-limit renders.

### Technique 2: Explicit Spacers

Gaps between logical sections use `Spacer(n)` components:

```typescript
this.ui.addChild(new Spacer(1));
this.ui.addChild(this.chatContainer);
// ...
```

This ensures consistent spacing rather than relying on implicit gaps.

### Technique 3: Differential Updates

Only changed lines are redrawn:

1. Compare `previousLines` with new render
2. Find first differing line index
3. Move cursor to that line
4. Clear from cursor to end of screen (`\x1b[J`)
5. Output changed lines

**No full-screen clear on every frame**.

### Technique 4: Line Width Enforcement

From the source:

> "CRITICAL: Always verify and truncate to terminal width. This is the final safeguard against width overflow which would crash the TUI."

Components must respect width parameter. Utilities like `truncateToWidth()` enforce compliance.

### Technique 5: Render Batching

```typescript
requestRender(force = false) {
  if (this.renderRequested) return;
  this.renderRequested = true;
  process.nextTick(() => {
    this.renderRequested = false;
    this.doRender();
  });
}
```

Multiple render requests within same event loop tick are deduplicated.

---

## Key Utilities

### Width Calculation

```typescript
visibleWidth(str); // Display width excluding ANSI codes
graphemeWidth(g); // Single grapheme cluster width
sliceByColumn(); // Extract column range preserving ANSI
```

Uses `Intl.Segmenter` for grapheme clustering, handles emoji and CJK correctly.

### Line Wrapping

```typescript
wrapTextWithAnsi(text, width); // Word-wrap preserving ANSI styling
```

Critical feature: ANSI styling carried across line breaks via `AnsiCodeTracker`.

### ANSI Management

Each line gets full SGR reset and OSC 8 reset at end. Styles don't persist across lines - each line requires independent styling.

---

## Adaptation to Rust/Ratatui

### The Fundamental Problem

Ratatui's `Viewport::Inline(height)` has a **fixed height**. Pi-mono's architecture assumes:

- No fixed viewport height
- Content determines rendered area
- TUI grows/shrinks naturally

**This is incompatible with ratatui's inline viewport model**.

### Option A: Fullscreen with Exit Transcript

Abandon inline viewport entirely:

```rust
// Use alternate screen
let mut terminal = Terminal::new(backend)?;

// On exit, dump transcript to scrollback
fn cleanup(&self) {
    execute!(stderr(), LeaveAlternateScreen)?;
    for msg in &self.transcript {
        println!("{}", format_message(msg));
    }
}
```

**Tradeoff**: No live scrollback search, but clean implementation.

### Option B: Raw Crossterm + Ratatui Widgets

Bypass ratatui's viewport, use widgets for rendering only:

```rust
// Chat history: print directly
for line in chat_lines {
    println!("{}", line);
}

// Status area: render to buffer, position manually
let mut buf = Buffer::empty(status_rect);
render_widgets(&mut buf);

execute!(stdout, SavePosition)?;
execute!(stdout, MoveTo(0, term_height - status_height))?;
output_buffer(&buf);
execute!(stdout, RestorePosition)?;
```

**Tradeoff**: Lose viewport abstraction, gain pi-mono-like flexibility.

### Option C: Fork Ratatui with Dynamic Viewport

PR #1964 adds `set_viewport_height()` but isn't merged. Could:

1. Wait for merge
2. Fork with the PR applied
3. Implement similar functionality

**Tradeoff**: Maintenance burden, but preserves ratatui ecosystem.

### Option D: Accept Fixed Viewport

Keep current ratatui approach, minimize gaps:

```rust
// Calculate minimum needed height
let min_height = progress + input + status;
const VIEWPORT_HEIGHT: u16 = 8;  // Small fixed size

// Accept small gaps as acceptable tradeoff
```

**Tradeoff**: Some visual imperfection, but simplest implementation.

---

## Recommendations for ion

| Priority | Recommendation                                                             |
| -------- | -------------------------------------------------------------------------- |
| 1        | Implement synchronized output (`\x1b[?2026h/l`) for flicker prevention     |
| 2        | Use explicit spacers rather than gap calculations                          |
| 3        | Batch render requests via tokio task scheduling                            |
| 4        | Enforce width limits on all rendered content                               |
| 5        | Consider Option B (raw crossterm + widgets) for full pi-mono-like behavior |
| 6        | Or Option A (fullscreen + exit transcript) for simplest implementation     |

### What Won't Translate

1. **Component return arrays** - Ratatui widgets render to fixed buffers
2. **No fixed viewport** - Ratatui requires explicit viewport sizing
3. **TypeScript flexibility** - Rust needs more explicit type handling
4. **process.nextTick batching** - Need tokio equivalent

### What Will Translate

1. **Synchronized output** - Just ANSI escape sequences
2. **Differential rendering concept** - Ratatui already does this for buffers
3. **Container hierarchy** - Layout composability works similarly
4. **Width enforcement** - Already doing this
5. **Spacer components** - Trivial to implement

---

## Sources

- [pi-mono Repository](https://github.com/badlogic/pi-mono)
- [pi-mono TUI Package](https://github.com/badlogic/pi-mono/tree/main/packages/tui)
- [Mario Zechner's Blog: Building a Coding Agent](https://mariozechner.at/posts/2025-11-30-pi-coding-agent/)
- [DeepWiki: pi-mono Architecture](https://deepwiki.com/badlogic/pi-mono)
