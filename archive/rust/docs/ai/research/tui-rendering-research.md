# Terminal UI Rendering: Flicker Prevention and Optimal Updates

**Research Date:** 2026-01-26
**Purpose:** Best practices for terminal rendering in ion's TUI redesign
**Focus:** Synchronized output, diffing strategies, production implementations

---

## Executive Summary

| Topic                   | Key Finding                                                                      |
| ----------------------- | -------------------------------------------------------------------------------- |
| Synchronized Output     | Widely supported (CSI 2026); use unconditionally with graceful degradation       |
| Diffing for Small Areas | Worth it for 60+ FPS; ratatui's cell-level diffing is efficient                  |
| Production Approach     | Line-level diff + synchronized output (pi-mono); cell-level diff (ratatui/Codex) |
| Main Bottleneck         | Terminal I/O syscalls (~98% of time), not diffing logic                          |

**Recommendation:** Use ratatui's built-in cell diffing with `scrolling-regions` feature. Wrap renders in synchronized output. For ion's small bottom area (5-15 lines), the complexity of custom diffing is not justified.

---

## 1. Synchronized Output (CSI 2026)

### Protocol

```
\x1b[?2026h    # Begin synchronized update (BSU)
... rendering ...
\x1b[?2026l    # End synchronized update (ESU)
```

Terminal buffers all output during synchronized mode, displays atomically on ESU. Prevents tearing and partial-frame visibility.

### Terminal Support Matrix

| Terminal           | Support | Notes                     |
| ------------------ | ------- | ------------------------- |
| Ghostty            | Full    | Since 1.0.0               |
| Kitty              | Full    | Modern versions           |
| iTerm2             | Full    | Inspired the spec         |
| Windows Terminal   | Full    | Since v1.23+              |
| WezTerm            | Full    | DECSET 2026               |
| Alacritty          | Full    | Since v0.13.0             |
| Warp               | Full    | Since v0.2025.01+         |
| Contour            | Full    | Documented                |
| foot               | Full    | Native support            |
| mintty             | Full    | Native support            |
| tmux               | Full    | Passthrough support       |
| Zellij             | Full    | Passthrough support       |
| xterm.js (VS Code) | Partial | Being improved            |
| GNOME Terminal/VTE | Limited | Returns mode 4 (disabled) |

### Feature Detection

```
CSI ? 2026 $ p    # DECRQM query
```

Responses:

- `CSI ? 2026 ; 1 $ y` = Active
- `CSI ? 2026 ; 2 $ y` = Supported, inactive
- `CSI ? 2026 ; 0 $ y` = Not supported
- `CSI ? 2026 ; 4 $ y` = Permanently disabled (GNOME Terminal)
- No response = Not implemented

### Recommendation

**Use synchronized output unconditionally.** Unsupported terminals simply ignore the sequences. No feature detection needed unless implementing timeout-based fallbacks.

```rust
// Crossterm provides this via SynchronizedUpdate
stdout().sync_update(|_| {
    // All terminal operations here
})?;
```

### Gotchas and Limitations

1. **No timeout consensus:** Spec doesn't mandate timeout for hung BSU. Some terminals may buffer indefinitely.
2. **Alternate screen behavior:** Unclear if sync mode persists across buffer switches.
3. **VS Code xterm.js:** Known issues being patched upstream (patches accepted Jan 2026).
4. **tmux:** Works via passthrough but may introduce slight latency.

---

## 2. Diffing vs Full Redraw

### The Question

For ion's bottom area (5-15 lines: progress, input, status), is diffing worth the complexity?

### Answer: Yes, But Use Ratatui's Built-in Diffing

**Performance Data:**

| Metric                     | Value                       | Source                                                                                                     |
| -------------------------- | --------------------------- | ---------------------------------------------------------------------------------------------------------- |
| Terminal I/O overhead      | ~98% of render time         | [ratatui discussions](https://github.com/ratatui/ratatui/discussions/579)                                  |
| Diffing algorithm overhead | ~2% of render time          | [ratatui discussions](https://github.com/ratatui/ratatui/discussions/579)                                  |
| Textual (Python)           | 120 FPS with diffing        | [Textual benchmarks](https://www.textualize.io/blog/7-things-ive-learned-building-a-modern-tui-framework/) |
| OpenTUI (Zig)              | Sub-millisecond frame times | [DeepWiki](https://deepwiki.com/sst/opentui)                                                               |
| ratatui                    | 60+ FPS typical             | [ratatui docs](https://ratatui.rs/concepts/rendering/)                                                     |

**Key insight from ratatui maintainers:**

> "The biggest bottleneck is actually writing to the terminal which is a system call. Ratatui's diffing algorithm is efficient and you can probably rely on it instead of implementing your own."

### Cell-Level vs Line-Level Diffing

| Approach       | Pros                                    | Cons                                | Used By                      |
| -------------- | --------------------------------------- | ----------------------------------- | ---------------------------- |
| **Cell-level** | Minimal writes, handles partial changes | More complex, cursor math           | ratatui, blessed.js, ncurses |
| **Line-level** | Simpler, good for append-only           | More writes on partial line changes | pi-mono, Ink                 |

**For ion's use case (small bottom area):**

- Cell-level via ratatui is the right choice
- Already implemented, well-tested
- Handles cursor optimization automatically

### When Line-Level Makes Sense

Line-level diffing is simpler when:

- Content is append-only (chat history)
- Lines change completely or not at all
- Implementing from scratch without a framework

Pi-mono's approach:

```
1. Compare new lines to previous frame
2. Find first differing line
3. Move cursor to that line
4. Clear to end of screen
5. Render from changed line onward
```

### What Production TUIs Do

| Application     | Framework         | Diffing Strategy           | Notes                                |
| --------------- | ----------------- | -------------------------- | ------------------------------------ |
| **Codex CLI**   | ratatui           | Cell-level (built-in)      | Uses `scrolling-regions` feature     |
| **pi-mono**     | Custom TypeScript | Line-level differential    | "Find first diff, render from there" |
| **Claude Code** | Ink (React)       | Full redraw per chunk      | Caused flickering, being fixed       |
| **OpenTUI**     | Zig + TypeScript  | Cell-level with RLE        | Sub-ms frame times                   |
| **Textual**     | Python/Rich       | Segment tree diffing       | 120 FPS, 85% bandwidth reduction     |
| **blessed.js**  | Node.js           | Cell-level + damage buffer | "Only draws the changes"             |

---

## 3. Production Implementation Analysis

### Codex CLI (Rust/ratatui) - Best Reference

**Architecture:**

```
Native scrollback (terminal handles)
├── Completed messages via insert_history_lines()
│
Inline viewport (Codex manages)
├── Streaming response
├── Input area
└── Status line
```

**Key techniques:**

1. **Scroll regions (DECSTBM)** for flicker-free history insertion:

```rust
queue!(writer, SetScrollRegion(1..area.top()))?;  // Region above viewport
queue!(writer, MoveTo(0, cursor_top))?;
// Write lines - region scrolls up
queue!(writer, ResetScrollRegion)?;
queue!(writer, MoveTo(last_cursor_pos.x, last_cursor_pos.y))?;
```

2. **Enabled ratatui features:**

```toml
ratatui = { features = [
    "scrolling-regions",        # Flicker-free insert_before
    "unstable-backend-writer",  # Direct backend access
] }
```

3. **Synchronized output via crossterm's API:**

```rust
stdout().sync_update(|_| {
    // Render operations
})?;
```

4. **Deferred history lines:**

```rust
// Queue history lines, flush during draw
self.pending_history_lines.extend(lines);
self.frame_requester().schedule_frame();
```

**Why TUI2 was abandoned:**

> "Making a full-screen, transcript-owned TUI feel truly native across real-world environments is far more complex than it appears."

Codex tried and failed to manage their own scrollback. Terminal-native won.

### Pi-Mono (TypeScript) - Line-Level Approach

**Three rendering strategies:**

```
Strategy 1 (Initial):      Output all lines
Strategy 2 (Width change): Full clear + repaint
Strategy 3 (Incremental):  Diff from first changed line
```

**Differential rendering algorithm:**

```typescript
// Pseudocode
for (let i = 0; i < newLines.length; i++) {
  if (newLines[i] !== previousLines[i]) {
    moveCursor(0, i);
    clearToEndOfScreen();
    renderFrom(i);
    break;
  }
}
```

**Key insight from author:**

> "Stores an entire scrollback buffer worth of previously rendered lines... on computers younger than 25 years, this is not a big deal (a few hundred kilobytes)."

**Performance characteristics:**

- Component caching for static content
- Markdown re-parsing avoided for completed messages
- Works well in Ghostty/iTerm2, occasional flicker in VS Code terminal

### Claude Code (Ink/React) - The Cautionary Tale

**Original problem:**

> "Claude Code performs a full terminal redraw on every chunk of streaming output rather than doing incremental updates."

Result: 4,000-6,700 scrolls/second, severe flickering, especially on Windows.

**The fix (Jan 2026):**

- Rewrote terminal rendering system
- 85% reduction in flickering
- Upstream patches to VS Code terminal and tmux for synchronized output support

**Lessons:**

1. Full redraws per streaming chunk = disaster
2. Synchronized output is essential
3. Even React/Ink can be efficient with proper `<Static>` usage

### OpenTUI (Zig + TypeScript) - Performance Reference

**Performance claims:**

- Sub-millisecond frame times
- 60+ FPS for complex UIs
- Cell-level diffing with run-length encoding

**Architecture:**

- Zig for performance-critical rendering
- TypeScript API for ergonomics
- FFI bridge for high-frequency operations

**Known issue:**
O(n) rendering loop in long sessions caused CPU spikes. Shows that even optimized renderers need bounded complexity.

---

## 4. Best Practices for Terminal Rendering

### Write Batching

**Problem:** Each write syscall is expensive (~98% of render time).

**Solution:** Use crossterm's queue API:

```rust
// Bad - multiple flushes
execute!(stdout, MoveTo(0, 0))?;
execute!(stdout, Print("Hello"))?;

// Good - single flush
queue!(stdout, MoveTo(0, 0))?;
queue!(stdout, Print("Hello"))?;
stdout.flush()?;
```

### stdout vs stderr Performance

| Stream | Buffering                  | Performance           |
| ------ | -------------------------- | --------------------- |
| stdout | Line-buffered (LineWriter) | ~2x faster            |
| stderr | Unbuffered                 | Slower, more syscalls |

**Recommendation:** Use stdout for TUI output. stderr only for error messages that must appear immediately.

### Cursor Movement Optimization

Ratatui automatically optimizes cursor movement:

1. Tracks current position
2. Only emits MoveTo when next cell isn't adjacent
3. Uses relative movements (CUF, CUB) when shorter

**Don't implement manually** unless bypassing ratatui entirely.

### ANSI Sequence Efficiency

1. **Run-length encoding:** Combine adjacent cells with same style
2. **SGR coalescing:** Batch style changes
3. **Relative vs absolute positioning:** Use shorter sequence

Example of inefficient vs efficient:

```
// Inefficient
\x1b[1;1H\x1b[31mH\x1b[32me\x1b[33ml\x1b[34ml\x1b[35mo

// Efficient (single color per segment)
\x1b[1;1H\x1b[31mHello
```

### Frame Rate Limiting

**Problem:** Unthrottled renders can overwhelm terminal.

**Solution:** Cap frame rate (Ink uses configurable FPS):

```rust
const TARGET_FPS: u32 = 60;
const FRAME_TIME: Duration = Duration::from_millis(1000 / TARGET_FPS);
```

For ion's bottom area, even 30 FPS is likely sufficient.

---

## 5. Recommendations for Ion

### Architecture

```
Native scrollback (println!)
├── Chat history
├── Tool output
├── Completed messages
│
Managed bottom area (crossterm + ratatui)
├── Progress indicator (during streaming)
├── Input area (multi-line)
└── Status line
```

### Implementation Checklist

| Priority | Task                                | Rationale                       |
| -------- | ----------------------------------- | ------------------------------- |
| P1       | Use `scrolling-regions` feature     | Flicker-free history insertion  |
| P1       | Wrap renders in synchronized output | Prevents tearing                |
| P2       | Trust ratatui's cell diffing        | Already optimized, ~2% overhead |
| P2       | Use stdout, not stderr              | 2x faster due to buffering      |
| P3       | Cap frame rate at 30-60 FPS         | Prevents CPU waste              |
| P3       | Batch pending history lines         | Reduces render frequency        |

### Code Pattern

```rust
use crossterm::terminal::BeginSynchronizedUpdate;
use crossterm::terminal::EndSynchronizedUpdate;
use ratatui::prelude::*;

fn render_frame(&mut self, terminal: &mut Terminal<impl Backend>) -> Result<()> {
    // Begin synchronized output
    execute!(stdout(), BeginSynchronizedUpdate)?;

    // Flush any pending history to scrollback
    if !self.pending_history.is_empty() {
        terminal.insert_before(self.pending_history.len(), |buf| {
            // Render completed messages
        })?;
        self.pending_history.clear();
    }

    // Draw managed bottom area
    terminal.draw(|frame| {
        // ratatui handles cell-level diffing automatically
        self.render_bottom_area(frame);
    })?;

    // End synchronized output
    execute!(stdout(), EndSynchronizedUpdate)?;

    Ok(())
}
```

### What to Avoid

1. **Full redraw per streaming chunk** - Claude Code's original problem
2. **Manual cell diffing** - ratatui already does this
3. **stderr for TUI output** - 2x slower than stdout
4. **Fighting terminal reflow** - Accept horizontal resize disruption
5. **Managing own scrollback** - Codex TUI2 abandoned this approach

---

## Sources

### Specifications

- [Synchronized Output Spec](https://gist.github.com/christianparpart/d8a62cc1ab659194337d73e399004036) - Christian Parpart
- [Contour Synchronized Output](https://contour-terminal.org/vt-extensions/synchronized-output/)

### Frameworks

- [ratatui rendering docs](https://ratatui.rs/concepts/rendering/under-the-hood/)
- [ratatui discussions #579](https://github.com/ratatui/ratatui/discussions/579) - Performance best practices
- [ratatui scrolling-regions PR #1341](https://github.com/ratatui/ratatui/pull/1341)
- [Ink GitHub](https://github.com/vadimdemedes/ink) - React for CLI
- [blessed.js](https://github.com/chjj/blessed) - Node.js TUI library
- [crossterm docs](https://docs.rs/crossterm/latest/crossterm/)

### Production Implementations

- [Codex CLI TUI](https://deepwiki.com/oaiagicorp/codex/3.2-tui-implementation)
- [Pi-mono TUI blog](https://mariozechner.at/posts/2025-11-30-pi-coding-agent/)
- [OpenTUI](https://deepwiki.com/sst/opentui)
- [Textual 7 things learned](https://www.textualize.io/blog/7-things-ive-learned-building-a-modern-tui-framework/)

### Performance Analysis

- [stdout vs stderr performance](https://blog.orhun.dev/stdout-vs-stderr/) - Orhun Parmaksiz
- [Claude Code flickering thread](https://www.threads.com/@boris_cherny/post/DSZbZatiIvJ/)
- [Claude Code issue #1913](https://github.com/anthropics/claude-code/issues/1913)

### Prior Research (ion)

- [tui-state-of-art-2026.md](./tui-state-of-art-2026.md)
- [inline-tui-patterns-2026.md](./inline-tui-patterns-2026.md)
- [pi-mono-tui-analysis.md](./pi-mono-tui-analysis.md)
- [codex-tui-analysis.md](./codex-tui-analysis.md)
