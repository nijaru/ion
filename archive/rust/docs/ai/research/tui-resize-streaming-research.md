# Terminal Resize & Streaming Text Research

**Date:** 2026-01-27
**Purpose:** Research Q3 (resize handling) and Q4 (streaming rendering) for TUI v2 design
**Context:** Native scrollback + managed bottom area architecture

---

## Executive Summary

| Question                | Answer                                                                                                                                                    |
| ----------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Q3: Resize handling     | Terminal reflows scrollback automatically; recalculate bottom UI position on SIGWINCH; full redraw on width change, position-only adjust on height change |
| Q4: Streaming rendering | Buffer in managed area, use carriage return for updates, commit to scrollback on completion                                                               |

---

## Q3: Terminal Resize Handling

### What Happens to Scrollback on Resize

**Width change:**

- Terminal emulator automatically reflows all text in scrollback
- Soft-wrapped lines rewrap to new width
- This happens BEFORE the application receives SIGWINCH
- Application cannot control or influence this reflow
- Content may shift vertically due to rewrapping

**Height change:**

- Scrollback content is unaffected
- Visible viewport changes (more/less lines visible)
- Cursor position may need adjustment

**Key insight from zsh developers:**

> "Signals are asynchronous at the OS level so the emulator can't control whether the shell has a chance to respond to the signal before it redraws the text."

### How to Recalculate Bottom UI Position

**Pattern used by Codex CLI:**

```rust
fn pending_viewport_area(&mut self) -> Result<Option<Rect>> {
    let screen_size = terminal.size()?;
    if screen_size != self.last_known_screen_size {
        if let Ok(cursor_pos) = terminal.get_cursor_position() {
            // Cursor moved = terminal reflowed content
            if cursor_pos.y != self.last_known_cursor_pos.y {
                let offset = cursor_pos.y as i32 - self.last_known_cursor_pos.y as i32;
                return Ok(Some(self.viewport_area.offset(Offset { x: 0, y: offset })));
            }
        }
    }
    Ok(None)
}
```

**Pattern used by pi-mono/TauTUI (three strategies):**

| Trigger                        | Strategy            | Commands                                                                               |
| ------------------------------ | ------------------- | -------------------------------------------------------------------------------------- |
| First render                   | Full output         | `CSI ?2026 h` ... lines ... `CSI ?2026 l`                                              |
| Width change                   | Full clear + redraw | `CSI 3J` (clear scrollback) + `CSI 2J` (clear screen) + `CSI H` (home) + redraw        |
| Height change / content change | Differential        | Find first changed line, move cursor, `CSI J` (clear to end), render from changed line |

### What Other Tools Do

**Codex CLI (Rust/ratatui):**

- Tracks `last_known_cursor_pos` and `last_known_screen_size`
- On resize: queries cursor position, calculates vertical offset
- Adjusts viewport area by the offset
- Uses scroll regions to avoid disrupting scrollback above viewport

**Pi-mono (TypeScript):**

- On width change: full clear including scrollback (`\x1b[3J\x1b[2J\x1b[H`)
- Acknowledges limitation: "If the first changed line is above the visible viewport (user scrolled up), we have to do a full clear and re-render. The terminal doesn't let you write to the scrollback buffer above the viewport."

**TauTUI (Swift port of pi-mono):**

- Same three-strategy approach
- `CSI 3J, CSI 2J, full redraw` on resize/viewport change
- Differential updates for content-only changes

**Claude Code (Ink/React):**

- Full redraw on resize
- Had flickering issues; fixed with differential renderer (Jan 2026)
- Upstream patches to VS Code terminal and tmux for CSI 2026 support

### Recommendation for ion

```
On SIGWINCH:
1. Get new terminal dimensions
2. Compare to stored dimensions

Width changed:
  - Full redraw of bottom UI (input lines need rewrapping)
  - Optionally: clear scrollback if managed content would be corrupted
  - Recalculate all visual line counts

Height changed only:
  - Recalculate bottom UI position (term_height - ui_height)
  - Redraw bottom UI at new position
  - Scrollback is unaffected

Always:
  - Wrap render in synchronized output (CSI ?2026 h/l)
  - Update stored dimensions
```

---

## Q4: Streaming LLM Response Rendering

### The Problem

Streaming LLM responses arrive token-by-token. Options:

| Approach                | Pros                           | Cons                                                   |
| ----------------------- | ------------------------------ | ------------------------------------------------------ |
| `println!()` each token | Simple                         | Floods scrollback with partial lines; breaks word wrap |
| Buffer until complete   | Clean scrollback               | No live feedback; feels slow                           |
| Managed preview area    | Live updates; clean scrollback | More complex; need to track streaming state            |
| Carriage return updates | Live updates on single line    | Limited to one line; multi-line needs more work        |

### What Claude Code Does

Claude Code uses **React/Ink** for terminal rendering:

1. **Streaming chunks update managed area** - Response streams into a component in the managed viewport
2. **Full redraw per chunk** (original) - Caused severe flickering (4,000-6,700 scrolls/second)
3. **Differential renderer** (Jan 2026 fix) - Only updates changed cells; 85% reduction in flickering
4. **On completion** - Content moves to scrollback (static output)

From Anthropic's [differential renderer announcement](https://news.ycombinator.com/item?id=46699072):

> "We rewrote their rendering system from scratch, and only ~1/3 of sessions see at least a flicker now."

**Key feature: `<Static>` component** - Ink's mechanism to push completed content to scrollback, removing it from the managed viewport.

### What Codex CLI Does

Codex uses **ratatui** with streaming in the managed viewport:

1. **ChatWidget** displays conversation history in the viewport
2. **Active cell mutation** - `AgentMessageDelta` events update the current response cell in-place
3. **On completion** - `insert_history_lines()` pushes completed message to scrollback above viewport
4. **Scroll regions** - Uses `DECSTBM` to insert above viewport without disturbing it

```rust
// Streaming: update active cell
match event {
    AgentMessageDelta(delta) => {
        self.active_response.push_str(&delta.text);
        self.schedule_frame(); // Trigger redraw
    }
    AgentMessageComplete => {
        let lines = self.format_response(&self.active_response);
        self.insert_history_lines(lines); // Push to scrollback
        self.active_response.clear();
    }
}
```

### What Pi-mono Does

Pi-mono streams into a **Markdown component** in the managed area:

1. **Markdown component** receives text updates
2. **Invalidation** - Component marks itself dirty on content change
3. **Differential render** - Only changed lines redrawn
4. **Component caching** - Completed markdown blocks are cached; not re-parsed
5. **On completion** - Content stays in place; scrolls up as new content appears

Key insight from author:

> "Stores an entire scrollback buffer worth of previously rendered lines... on computers younger than 25 years, this is not a big deal (a few hundred kilobytes)."

### What Toad Does (Textual)

From [Toad announcement](https://simonwillison.net/2025/Jul/23/announcing-toad/):

> "Toad streams Markdown responses efficiently, remaining responsive for large outputs while rendering tables and syntax-highlighted code fences."

Textual (the framework behind Toad):

- Segment tree diffing for 120 FPS rendering
- 85% bandwidth reduction vs full redraws
- Cell-level updates for streaming content

### Carriage Return Pattern (Simple CLI)

For simpler CLIs without managed areas, the carriage return pattern works:

```bash
# Print token, return to line start, clear line, repeat
while read token; do
    printf "\r\033[K%s" "$accumulated_text"
done
```

Limitations:

- Only works for single-line output
- Multi-line requires cursor positioning
- Not suitable for rich formatting

### Recommendation for ion

**Architecture:**

```
During streaming:
+--Terminal Scrollback (native)--+
| Previous messages...           |
| ...                            |
+--Managed Bottom Area-----------+
| [STREAMING RESPONSE HERE]      |  <- Response area (grows/shrinks)
| ................................. |
+--------------------------------+
| > input area                   |  <- Input (hidden during stream)
+--------------------------------+
| [status line]                  |
+--------------------------------+

On completion:
+--Terminal Scrollback (native)--+
| Previous messages...           |
| Completed response (println)   |  <- Moved to scrollback
|                                |
+--Managed Bottom Area-----------+
| > input area                   |  <- Input visible again
+--------------------------------+
| [status line]                  |
+--------------------------------+
```

**Implementation:**

1. **Streaming state** - Track whether currently streaming
2. **Response buffer** - Accumulate tokens in memory
3. **Managed render area** - Render buffered response in bottom area (above input)
4. **Carriage return + clear** - Use `\r\033[K` for single-line updates within managed area
5. **Multi-line**: Position cursor, clear to end of screen, render all lines
6. **On completion** - `println!()` the complete response (blank line after), clear response area

**Pseudocode:**

```rust
fn handle_streaming_token(&mut self, token: &str) {
    self.response_buffer.push_str(token);
    self.render_streaming_response();
}

fn render_streaming_response(&mut self) {
    let (width, height) = terminal::size()?;
    let response_lines = wrap_text(&self.response_buffer, width);
    let response_height = response_lines.len() as u16;

    // Calculate positions
    let status_height = 1;
    let input_height = if self.streaming { 0 } else { self.input_lines() + 2 };
    let progress_height = 1;
    let response_start = height - status_height - input_height - progress_height - response_height;

    // Position and render
    execute!(stdout(), MoveTo(0, response_start))?;
    execute!(stdout(), Clear(ClearType::FromCursorDown))?;

    for line in &response_lines {
        println!("{}", line);
    }

    // Render progress line
    self.render_progress()?;

    stdout().flush()?;
}

fn complete_streaming(&mut self) {
    // Push to native scrollback
    println!("{}", self.format_response(&self.response_buffer));
    println!(); // Blank line separator

    self.response_buffer.clear();
    self.streaming = false;

    // Now render just progress + input + status
    self.render_bottom_ui()?;
}
```

---

## Synchronized Output Integration

Both resize handling and streaming rendering should use synchronized output:

```rust
fn render_with_sync<F: FnOnce() -> io::Result<()>>(f: F) -> io::Result<()> {
    execute!(stdout(), BeginSynchronizedUpdate)?;
    let result = f();
    execute!(stdout(), EndSynchronizedUpdate)?;
    result
}
```

**Terminal support matrix:**

| Terminal         | CSI 2026 Support         |
| ---------------- | ------------------------ |
| Ghostty          | Full (since 1.0.0)       |
| Kitty            | Full                     |
| iTerm2           | Full                     |
| WezTerm          | Full                     |
| Alacritty        | Full (since 0.13.0)      |
| Windows Terminal | Full (since 1.23+)       |
| Warp             | Full                     |
| tmux             | Passthrough              |
| VS Code          | Partial (being improved) |
| GNOME Terminal   | Limited                  |

**Fallback:** Unsupported terminals ignore the sequences harmlessly.

---

## Key Takeaways

### For Resize (Q3):

1. **Accept terminal reflow** - Cannot control scrollback reflow on width change
2. **Track dimensions** - Compare old vs new to choose strategy
3. **Width change = full redraw** - Word wrap changes, need complete recalculation
4. **Height change = position adjust** - Just move bottom UI, content unchanged
5. **Query cursor position** - Codex pattern: detect vertical shift from reflow

### For Streaming (Q4):

1. **Buffer in managed area** - Not scrollback
2. **Differential updates** - Only redraw changed lines
3. **Commit on completion** - `println!()` to scrollback, clear managed buffer
4. **Hide input during streaming** - Show progress line instead
5. **Synchronized output** - Wrap all renders in CSI 2026

---

## Sources

### Resize Handling

- [crossterm terminal docs](https://docs.rs/crossterm/latest/crossterm/terminal/index.html)
- [ratatui inline viewport resize issue #2086](https://github.com/ratatui/ratatui/issues/2086)
- [TauTUI - Swift port of pi-tui](https://github.com/steipete/TauTUI)
- [zsh SIGWINCH prompt redrawing issues](https://zsh-workers.zsh.narkive.com/mU3d0tNb/prompt-redrawing-issues-with-wrapped-prompt-on-sigwinch)

### Streaming Rendering

- [Claude Code flickering fix (HN)](https://news.ycombinator.com/item?id=46699072)
- [Claude Code streaming issue #4346](https://github.com/anthropics/claude-code/issues/4346)
- [Toad announcement](https://simonwillison.net/2025/Jul/23/announcing-toad/)
- [OpenCode TUI docs](https://opencode.ai/docs/tui/)
- [Pi-mono TUI package](https://github.com/badlogic/pi-mono/tree/main/packages/tui)

### Synchronized Output

- [Terminal Synchronized Output Spec](https://gist.github.com/christianparpart/d8a62cc1ab659194337d73e399004036)
- [Contour synchronized output docs](https://contour-terminal.org/vt-extensions/synchronized-output/)
- [Bubbletea Mode 2026 discussion](https://github.com/charmbracelet/bubbletea/discussions/1320)

### Prior Research (ion)

- [pi-mono-tui-analysis.md](/Users/nick/github/nijaru/ion/ai/research/pi-mono-tui-analysis.md)
- [codex-tui-analysis.md](/Users/nick/github/nijaru/ion/ai/research/codex-tui-analysis.md)
- [tui-rendering-research.md](/Users/nick/github/nijaru/ion/ai/research/tui-rendering-research.md)
