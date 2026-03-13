# TUI Rendering: Diffing vs Synchronized Output

**Research Date:** 2026-01-27
**Purpose:** Answer whether cell/line diffing is needed when using synchronized output (CSI 2026)

---

## Executive Summary

| Question                                     | Answer                                                          |
| -------------------------------------------- | --------------------------------------------------------------- |
| Q1: Is diffing needed for flicker-free UIs?  | **No for small areas, Yes for large/streaming content**         |
| Q2: Is synchronized output sufficient alone? | **Yes for small, infrequently updated areas. No for streaming** |

**Recommendation for ion:** Synchronized output alone is sufficient for the bottom UI area (progress + input + status = ~5-15 lines). No custom diffing needed. For streaming responses, use line-level diffing similar to pi-mono.

---

## Q1: Is Diffing Needed for Flicker-Free UIs?

### Short Answer

**It depends on the update frequency and content size.**

| Scenario                         | Diffing Needed? | Rationale                           |
| -------------------------------- | --------------- | ----------------------------------- |
| Small fixed area (5-15 lines)    | No              | Synchronized output sufficient      |
| Streaming text (token-by-token)  | Yes             | Full redraws cause 4000+ scroll/sec |
| Large content (100+ lines)       | Yes             | I/O becomes bottleneck              |
| Infrequent updates (<30 FPS)     | No              | Terminal handles it fine            |
| High-frequency updates (60+ FPS) | Yes             | Reduces I/O overhead significantly  |

### Evidence: Claude Code's Problem

Claude Code initially performed full terminal redraws on every streaming chunk:

> "Claude Code sends entire screen redraws in these sync blocks - often thousands of lines. Your terminal receives a 5000-line atomic update when only 20 lines are visible. This causes lag, flicker, or jitters."
> -- [GitHub Issue #9935](https://github.com/anthropics/claude-code/issues/9935)

Result: **4,000-6,700 scroll events per second** in terminal multiplexers. Even with synchronized output, the sheer volume of writes overwhelmed terminals.

The fix required Anthropic to rewrite their rendering system:

> "We've rewritten Claude Code's terminal rendering system to reduce flickering by roughly 85%."
> -- [Thariq on X](https://x.com/trq212/status/2001439019713073626)

Key insight: **Synchronized output prevents tearing, but does not prevent performance problems from excessive writes.**

### Evidence: Performance Benchmarks

| Framework        | Approach             | Performance     |
| ---------------- | -------------------- | --------------- |
| Textual (Python) | Segment tree diffing | 120 FPS         |
| curses           | Full redraw          | 20 FPS          |
| ratatui          | Cell-level diffing   | 60+ FPS typical |

From [Textual benchmarks](https://www.textualize.io/blog/7-things-ive-learned-building-a-modern-tui-framework/):

> "Textual achieves 120 FPS renders (vs curses' 20 FPS) via Rich's segment trees, which delta-update only dirty regions."

### Evidence: Where Diffing Overhead Lives

From [ratatui discussions #579](https://github.com/ratatui/ratatui/discussions/579):

> "The biggest bottleneck is actually writing to the terminal which is a system call. Ratatui's diffing algorithm is efficient and you can probably rely on it instead of implementing your own."

Breakdown:

- ~98% of render time: Terminal I/O syscalls
- ~2% of render time: Diffing algorithm

**Conclusion:** Diffing's CPU cost is negligible. Its value is reducing I/O volume.

---

## Q2: What Do Modern Terminal Apps Do?

### Pi-Mono (TypeScript) - Line-Level Diffing

**Approach:** Three-strategy rendering with line diffing.

```
Strategy 1 (Initial):      Output all lines
Strategy 2 (Width change): Full clear + repaint
Strategy 3 (Incremental):  Diff from first changed line
```

**Algorithm:**

1. Compare new lines to previous frame
2. Find first differing line index
3. Move cursor to that line
4. Clear from cursor to end of screen
5. Render changed lines

**Why it works:**

> "Stores an entire scrollback buffer worth of previously rendered lines... on computers younger than 25 years, this is not a big deal (a few hundred kilobytes)."
> -- [Mario Zechner](https://mariozechner.at/posts/2025-11-30-pi-coding-agent/)

All rendering wrapped in synchronized output (`CSI ?2026h ... CSI ?2026l`).

**Results:**

- Ghostty/iTerm2: No flicker
- VS Code terminal: Occasional flicker (terminal limitation)

### OpenAI Codex (Rust/ratatui) - Cell-Level Diffing

**Approach:** Uses ratatui's built-in cell diffing with `scrolling-regions` feature.

```rust
// Uses ratatui features
ratatui = { features = [
    "scrolling-regions",        // Flicker-free insert_before
    "unstable-backend-writer",  // Direct backend access
] }
```

**Key technique:** Scroll regions for flicker-free history insertion:

```rust
queue!(writer, SetScrollRegion(1..area.top()))?;
queue!(writer, MoveTo(0, cursor_top))?;
// Write lines - region scrolls up
queue!(writer, ResetScrollRegion)?;
```

**Why they use ratatui:**

> "terminal functionality that works uniformly everywhere trumps sophisticated but environment-specific features"
> -- [Codex Issue #8344](https://github.com/openai/codex/issues/8344)

### Bubbletea (Go) - Renderer Optimization

**Approach:** Focus on renderer optimization over diffing.

> "The bottleneck is actually in the rendering, not vsync."

Bubbletea v2 added synchronized output (Mode 2026) but emphasized internal renderer improvements:

> "The next pre-release of Bubble Tea v2 includes another overhaul to the renderer which we've seen speed up rendering and lower rendering bandwidth exponentially."
> -- [Bubbletea Discussion #1320](https://github.com/charmbracelet/bubbletea/discussions/1320)

### Claude Code (Ink/React) - The Cautionary Tale

**Original approach:** Full redraw per streaming chunk.

**Problem:**

- 4,000-6,700 scrolls/second
- Severe flickering on Windows
- Terminal multiplexer jitter

**Fix (Jan 2026):**

- Rewrote terminal rendering system
- 85% reduction in flickering
- Upstream patches to VS Code terminal and tmux

**Tool workaround (claude-chill):**

> "Sits between your terminal and Claude Code, intercepting sync blocks and using a VT100 emulator to track screen state and render only the differences."
> -- [claude-chill GitHub](https://github.com/davidbeesley/claude-chill)

---

## Synchronized Output: When It's Sufficient

### How CSI 2026 Works

```
\x1b[?2026h    # Begin synchronized update (BSU)
... all rendering commands ...
\x1b[?2026l    # End synchronized update (ESU)
```

Terminal buffers all output during synchronized mode, displays atomically on ESU. Prevents tearing and partial-frame visibility.

### Terminal Support

| Terminal           | Support | Notes                         |
| ------------------ | ------- | ----------------------------- |
| Ghostty            | Full    | Since 1.0.0                   |
| Kitty              | Full    | Modern versions               |
| iTerm2             | Full    | Inspired the spec             |
| Windows Terminal   | Full    | Since v1.23+                  |
| WezTerm            | Full    | DECSET 2026                   |
| Alacritty          | Full    | Since v0.13.0                 |
| Warp               | Full    | Since v0.2025.01+             |
| tmux               | Full    | Passthrough support (patched) |
| Zellij             | Full    | Passthrough support           |
| VS Code terminal   | Partial | Patches accepted upstream     |
| GNOME Terminal/VTE | Limited | Returns mode 4 (disabled)     |

### When Synchronized Output Alone Is Sufficient

1. **Small update areas** (< 50 lines per frame)
2. **Low update frequency** (< 30 FPS)
3. **Complete content replacement** (no need to preserve unchanged cells)
4. **No streaming content** (updates are discrete, not continuous)

### When You Need Diffing Too

1. **Streaming text** - Token-by-token updates would flood terminal
2. **Large areas** - Full redraws exceed terminal bandwidth
3. **High FPS** - Diffing reduces write volume significantly
4. **Terminal multiplexers** - Extra sensitive to scroll events

---

## Recommendations for ion

### Architecture Recap

```
Native scrollback (stdout)         Managed bottom area (crossterm)
+-- Header (ion, version)          +-- Progress (1 line)
+-- Chat history                   +-- Input (dynamic height)
+-- Tool output                    +-- Status (1 line)
+-- Blank line after each
```

### Bottom UI Area: No Diffing Needed

The managed bottom area is small (3-15 lines):

- Progress: 1 line
- Input: 1-10 lines (typical)
- Status: 1 line

**Recommendation:** Use synchronized output only.

```rust
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};

fn render_bottom_ui(&self) -> io::Result<()> {
    execute!(stdout(), BeginSynchronizedUpdate)?;

    // Clear and redraw entire bottom area
    let (_, height) = terminal::size()?;
    let ui_height = self.calculate_ui_height();
    execute!(stdout(), MoveTo(0, height - ui_height))?;
    execute!(stdout(), Clear(ClearType::FromCursorDown))?;

    self.render_progress()?;
    self.render_input()?;
    self.render_status()?;

    execute!(stdout(), EndSynchronizedUpdate)?;
    Ok(())
}
```

**Rationale:**

- Small area = minimal I/O even with full redraw
- Simpler implementation
- No state tracking needed
- Works across all terminals

### Streaming Responses: Line-Level Diffing Recommended

For streaming responses (token-by-token), use pi-mono's approach:

```rust
struct StreamRenderer {
    previous_lines: Vec<String>,
}

impl StreamRenderer {
    fn render(&mut self, new_lines: &[String]) -> io::Result<()> {
        execute!(stdout(), BeginSynchronizedUpdate)?;

        // Find first differing line
        let first_diff = self.previous_lines.iter()
            .zip(new_lines.iter())
            .position(|(a, b)| a != b)
            .unwrap_or(new_lines.len().min(self.previous_lines.len()));

        if first_diff < new_lines.len() {
            // Move to first diff, clear to end, render from there
            execute!(stdout(), MoveTo(0, first_diff as u16))?;
            execute!(stdout(), Clear(ClearType::FromCursorDown))?;

            for line in &new_lines[first_diff..] {
                println!("{}", line);
            }
        }

        self.previous_lines = new_lines.to_vec();
        execute!(stdout(), EndSynchronizedUpdate)?;
        Ok(())
    }
}
```

**Rationale:**

- Streaming can be 10-50 tokens/second
- Full redraws would overwhelm terminal
- Line-level is simpler than cell-level
- Matches pi-mono's proven approach

### Summary Decision Matrix

| Component          | Diffing?   | Synchronized Output? | Rationale                  |
| ------------------ | ---------- | -------------------- | -------------------------- |
| Header             | No         | No                   | Printed once, scrolls away |
| Chat history       | No         | No                   | println! to scrollback     |
| Tool output        | No         | No                   | println! to scrollback     |
| Progress line      | No         | Yes                  | 1 line, infrequent updates |
| Input area         | No         | Yes                  | Small, user-paced updates  |
| Status line        | No         | Yes                  | 1 line, infrequent updates |
| Streaming response | Yes (line) | Yes                  | High-frequency updates     |

---

## What to Avoid

1. **Full redraw per streaming token** - Claude Code's original problem
2. **Cell-level diffing from scratch** - Complex, ratatui does this well
3. **Diffing for small areas** - Overhead not worth it
4. **Ignoring synchronized output** - Essential for any redraw approach

---

## Sources

### Specifications

- [Synchronized Output Spec](https://gist.github.com/christianparpart/d8a62cc1ab659194337d73e399004036) - Christian Parpart
- [Contour Synchronized Output](https://contour-terminal.org/vt-extensions/synchronized-output/)

### Framework Documentation

- [ratatui discussions #579](https://github.com/ratatui/ratatui/discussions/579) - Performance best practices
- [ratatui issues #1116](https://github.com/ratatui/ratatui/issues/1116) - Bypassing diff discussion
- [Bubbletea Discussion #1320](https://github.com/charmbracelet/bubbletea/discussions/1320) - Mode 2026 support

### Production Implementations

- [Pi-mono TUI blog](https://mariozechner.at/posts/2025-11-30-pi-coding-agent/) - Mario Zechner
- [Codex Issue #8344](https://github.com/openai/codex/issues/8344) - TUI native concerns
- [Claude Code Issue #9935](https://github.com/anthropics/claude-code/issues/9935) - Scroll event flooding

### Performance Analysis

- [Textual benchmarks](https://www.textualize.io/blog/7-things-ive-learned-building-a-modern-tui-framework/)
- [Claude Code flickering fix](https://x.com/trq212/status/2001439019713073626) - Thariq thread
- [claude-chill](https://github.com/davidbeesley/claude-chill) - Third-party workaround tool

### Prior ion Research

- [tui-rendering-research.md](./tui-rendering-research.md)
- [pi-mono-tui-analysis.md](./pi-mono-tui-analysis.md)
- [codex-tui-analysis.md](./codex-tui-analysis.md)
