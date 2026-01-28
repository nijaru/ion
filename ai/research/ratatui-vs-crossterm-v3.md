# Ratatui vs Raw Crossterm for TUI v3 Architecture

**Research Date:** 2026-01-27
**Purpose:** Evaluate trade-offs for ion's TUI v3 requirements

---

## Executive Summary

| Requirement                         | Ratatui Viewport::Inline | Raw Crossterm | Hybrid (Codex-style) |
| ----------------------------------- | ------------------------ | ------------- | -------------------- |
| Managed chat history (from memory)  | Limited                  | Full control  | Full control         |
| Re-render on resize                 | Problematic              | Manual        | Manual + helpers     |
| Print history to scrollback on exit | Supported                | Manual        | Supported            |
| Fixed bottom UI                     | Supported                | Manual        | Supported            |
| Complexity                          | Low                      | High          | Medium               |
| Production examples                 | Limited                  | pi-mono       | Codex CLI            |

**Recommendation:** Use **Hybrid approach** - custom terminal management with ratatui widgets for rendering. This is what Codex CLI does successfully.

---

## 1. Ratatui Viewport::Inline Analysis

### How It Works

```rust
let mut terminal = ratatui::init_with_options(TerminalOptions {
    viewport: Viewport::Inline(8),  // Fixed 8-line viewport at bottom
});

// Draw in viewport area
terminal.draw(|frame| {
    frame.render_widget(widget, frame.area());
})?;

// Push content above viewport to scrollback
terminal.insert_before(1, |buf| {
    Paragraph::new("Goes to scrollback").render(buf.area, buf);
})?;
```

### Resize Handling - The Critical Problem

**The fundamental issue:** When the terminal shrinks horizontally, the terminal emulator (not ratatui) wraps lines that no longer fit. This pushes content UP into scrollback automatically. Ratatui continues drawing the viewport in its original position, creating misalignment.

From [Issue #2086](https://github.com/ratatui/ratatui/issues/2086):

| Event             | Terminal Behavior              | Ratatui Limitation                  |
| ----------------- | ------------------------------ | ----------------------------------- |
| Horizontal shrink | Lines wrap, content scrolls up | Cannot track how much content moved |
| Horizontal expand | Lines unwrap, gaps may appear  | No automatic gap filling            |
| Vertical resize   | Works with `autoresize()`      | Generally OK                        |

**Proposed solutions (none fully satisfactory):**

1. **Scroll Down** - Keep viewport in place by scrolling content down
   - Creates whitespace gaps in history
   - Requires `scrolling-regions` feature

2. **Move Viewport Up** - Attach to where terminal put the content
   - Application "creeps upward" over time
   - Loses position tracking

3. **Clear Screen** - [Draft PR #2355](https://github.com/ratatui/ratatui/pull/2355) clears entire screen on horizontal shrink
   - Destroys scrollback history
   - Disruptive user experience

### What Viewport::Inline Does NOT Support

| Feature                       | Status                                                             |
| ----------------------------- | ------------------------------------------------------------------ |
| Dynamic viewport height       | Not supported (PR #1964 adds `set_viewport_height` but not merged) |
| Managed content re-rendering  | No - content in scrollback cannot be changed                       |
| Horizontal resize recovery    | No clean solution                                                  |
| Content-based viewport sizing | Must specify fixed height at creation                              |

### What It DOES Handle Well

- Fixed-height bottom area rendering
- Cell-level diffing for minimal terminal writes
- Automatic cursor optimization
- Widget ecosystem (borders, styling, layout)
- `insert_before()` for scrollback insertion
- Vertical resize with `autoresize()`

### Performance

From [ratatui discussions #579](https://github.com/ratatui/ratatui/discussions/579):

> "The biggest bottleneck is actually writing to the terminal which is a system call. Ratatui's diffing algorithm is efficient... ~2% overhead vs ~98% terminal I/O."

- Cell-level diffing adds minimal CPU overhead
- Double-buffer comparison prevents unnecessary writes
- 60+ FPS achievable for typical UIs

---

## 2. Raw Crossterm Analysis

### Available Primitives

```rust
// Cursor positioning
execute!(stdout, MoveTo(0, row))?;
execute!(stdout, SavePosition)?;
execute!(stdout, RestorePosition)?;

// Scrolling
execute!(stdout, ScrollUp(n))?;
execute!(stdout, ScrollDown(n))?;

// Screen management
execute!(stdout, Clear(ClearType::All))?;
execute!(stdout, Clear(ClearType::FromCursorDown))?;
execute!(stdout, EnterAlternateScreen)?;
execute!(stdout, LeaveAlternateScreen)?;

// Synchronized output
execute!(stdout, BeginSynchronizedUpdate)?;
execute!(stdout, EndSynchronizedUpdate)?;
```

### What Crossterm Does NOT Provide

| Feature                  | Status                            |
| ------------------------ | --------------------------------- |
| Scroll regions (DECSTBM) | Not exposed - must write raw ANSI |
| Cell diffing             | Must implement manually           |
| Layout management        | Manual positioning                |
| Widget rendering         | None - raw strings only           |

### Manual Scroll Region Implementation

```rust
// Custom DECSTBM command (what Codex implements)
pub struct SetScrollRegion(pub std::ops::Range<u16>);

impl crossterm::Command for SetScrollRegion {
    fn write_ansi(&self, f: &mut impl std::fmt::Write) -> std::fmt::Result {
        write!(f, "\x1b[{};{}r", self.0.start, self.0.end)
    }
}

pub struct ResetScrollRegion;

impl crossterm::Command for ResetScrollRegion {
    fn write_ansi(&self, f: &mut impl std::fmt::Write) -> std::fmt::Result {
        write!(f, "\x1b[r")
    }
}
```

### What You Must Manage Yourself

1. **Cursor position tracking** - Know where you are at all times
2. **Content measurement** - Calculate wrapped line heights for all widths
3. **Viewport positioning** - Determine where viewport sits relative to screen
4. **Resize detection** - Query terminal size, detect changes
5. **Scroll region setup** - Set DECSTBM before scrolling operations
6. **Line clearing** - Clear old content before redrawing
7. **Style state** - Track SGR state across renders

### Known Pitfalls

| Pitfall                           | Consequence                              |
| --------------------------------- | ---------------------------------------- |
| Forgetting to reset scroll region | Terminal behaves unexpectedly after exit |
| Not handling resize atomically    | Partial renders visible during resize    |
| Incorrect cursor save/restore     | Position drift over time                 |
| Not clearing to end of line       | Ghost characters from previous content   |
| Unbuffered writes                 | Severe flickering                        |

---

## 3. Hybrid Approach (Codex-style)

### Architecture

Codex CLI uses a **hybrid approach**:

1. Custom terminal management (NOT `Viewport::Inline`)
2. Ratatui widgets for rendering to buffers
3. Manual scroll region handling
4. `scrolling-regions` feature for flicker-free insertion

```rust
// NOT using Viewport::Inline - custom terminal
pub fn init() -> Result<Terminal> {
    set_modes()?;
    let backend = CrosstermBackend::new(stdout());
    let tui = CustomTerminal::with_options(backend)?;  // Custom, not ratatui default
    Ok(tui)
}
```

### Key Implementation: Custom Viewport Tracking

```rust
struct CustomTerminal {
    viewport_area: Rect,
    last_known_cursor_pos: Position,
    last_known_screen_size: Size,
}

fn pending_viewport_area(&mut self) -> Result<Option<Rect>> {
    let screen_size = terminal::size()?;
    let cursor_pos = self.get_cursor_position()?;

    // Heuristic: if resize AND cursor moved, adjust viewport
    if screen_size != self.last_known_screen_size
        && cursor_pos.y != self.last_known_cursor_pos.y
    {
        let offset = cursor_pos.y as i32 - self.last_known_cursor_pos.y as i32;
        return Ok(Some(self.viewport_area.offset(Offset { x: 0, y: offset })));
    }
    Ok(None)
}
```

### Key Implementation: History Line Insertion

```rust
pub fn insert_history_lines(terminal: &mut Terminal, lines: Vec<Line>) -> io::Result<()> {
    // Set scroll region to area ABOVE viewport
    queue!(writer, SetScrollRegion(1..viewport_area.top()))?;
    queue!(writer, MoveTo(0, cursor_top))?;

    // Write lines - region scrolls automatically
    for line in wrapped_lines {
        queue!(writer, Print("\r\n"))?;
        write_styled_content(writer, &line)?;
    }

    // Reset and restore
    queue!(writer, ResetScrollRegion)?;
    queue!(writer, MoveTo(saved_cursor.x, saved_cursor.y))?;
    writer.flush()?;
    Ok(())
}
```

### Ratatui Features Required

```toml
ratatui = { features = [
    "scrolling-regions",           # Flicker-free scroll region operations
    "unstable-backend-writer",     # Direct backend access for custom commands
] }
```

### What This Approach Handles

| Requirement              | How Solved                                               |
| ------------------------ | -------------------------------------------------------- |
| Chat history from memory | Render visible portion directly, manage own scrollback   |
| Re-render on resize      | Full clear + re-render from memory at new width          |
| Print history on exit    | Already in terminal scrollback OR dump from memory       |
| Fixed bottom UI          | Manual viewport positioning, ratatui widgets for content |

---

## 4. Claude Code's Approach

### Architecture

Claude Code uses **Ink** (React terminal renderer) with a custom differential renderer:

| Component    | Implementation                                   |
| ------------ | ------------------------------------------------ |
| Framework    | React + custom renderer (rewrote Ink's renderer) |
| Rendering    | Differential line updates, not full redraws      |
| Scrollback   | Native terminal scrollback                       |
| Managed area | Bottom UI (input, status)                        |

### How They Handle Managed Area vs Scrollback

```
During session:           After exit:
+-----------------------+  $ previous commands
| [MANAGED AREA]        |  $ another command
| Chat history          |  > user message
| (re-renders on        |    assistant response
|  resize)              |
+-----------------------+  $ prompt appears here
| Bottom UI             |
| (input, status)       |
+-----------------------+
```

From [The Signature Flicker](https://steipete.me/posts/2025/signature-flicker):

> "Anthropic rewrote the renderer from scratch while maintaining React as their component model... They value maintaining this native experience [native scrolling, selection, search]."

### Key Decisions

1. **Not alternate screen** - Preserves native terminal features (Cmd+F search, selection)
2. **Differential rendering** - Only update changed lines, not full redraws
3. **Custom renderer** - Ink's default was too crude, caused 4000-6700 scroll events/second
4. **Accept resize quirks** - ~1s debounce, full re-render from memory

### Known Issues (as reference)

From [Issue #18493](https://github.com/anthropics/claude-code/issues/18493):

- Terminal resize causes content loss when shrinking window
- This is fundamentally hard - Claude Code also struggles with it

From [Issue #11260](https://github.com/anthropics/claude-code/issues/11260):

- `/clear` doesn't properly clear scrollback
- Resize causes chat history to reappear

**Lesson:** Even with significant engineering effort, inline terminal UIs have inherent resize challenges.

---

## 5. Comparison Table

### Requirements vs Approaches

| Requirement            | Viewport::Inline                  | Raw Crossterm | Hybrid (Codex)         | Managed + Exit Dump |
| ---------------------- | --------------------------------- | ------------- | ---------------------- | ------------------- |
| **Chat from memory**   | No - scrollback owned by terminal | Yes           | Yes                    | Yes                 |
| **Resize re-render**   | Broken for horizontal             | Manual        | Heuristic-based        | Full redraw         |
| **Exit to scrollback** | insert_before                     | Manual        | insert_before + native | Dump on exit        |
| **Fixed bottom UI**    | Yes                               | Manual        | Yes                    | Yes                 |
| **Complexity**         | Low                               | Very High     | Medium                 | Low-Medium          |
| **Production proven**  | No                                | pi-mono (TS)  | Codex (Rust)           | Gemini CLI          |

### Pros/Cons Summary

| Approach                | Pros                                   | Cons                                              |
| ----------------------- | -------------------------------------- | ------------------------------------------------- |
| **Viewport::Inline**    | Simple setup, good for small fixed UIs | Horizontal resize broken, can't re-render history |
| **Raw Crossterm**       | Full control, minimal overhead         | Must implement everything, many edge cases        |
| **Hybrid (Codex)**      | Best of both, production-proven        | Requires custom terminal code, more complex       |
| **Managed + Exit Dump** | Clean resize handling, simple model    | No live scrollback search during session          |

---

## 6. Recommendation for ion TUI v3

### Primary Recommendation: Managed History + Exit Dump

Given ion's requirements:

1. Render chat history from memory (not native scrollback during session)
2. Re-render on resize
3. Print history to native scrollback on exit
4. Fixed bottom UI

The cleanest approach is **Managed History with Exit Dump**:

```rust
struct TuiV3 {
    // Single source of truth for display
    message_list: MessageList,

    // Virtual scroll state
    scroll_offset: usize,

    // Format cache (invalidate on width change)
    formatted_cache: FormattedCache,

    // Bottom UI height
    bottom_height: u16,
}

impl TuiV3 {
    fn render(&mut self, width: u16, height: u16) {
        let chat_height = height - self.bottom_height;

        // Get formatted lines at current width
        let lines = self.formatted_cache.get_or_format(&self.message_list, width);

        // Calculate visible range
        let visible_start = lines.len().saturating_sub(chat_height + self.scroll_offset);
        let visible_lines = &lines[visible_start..visible_start.min(visible_start + chat_height)];

        // Render chat area
        for (row, line) in visible_lines.iter().enumerate() {
            execute!(stdout, MoveTo(0, row as u16))?;
            execute!(stdout, Clear(ClearType::CurrentLine))?;
            print!("{}", line);
        }

        // Render bottom UI (using ratatui widgets)
        self.render_bottom_ui(width, height)?;
    }

    fn handle_resize(&mut self, new_width: u16, new_height: u16) {
        // Invalidate cache if width changed
        if Some(new_width) != self.formatted_cache.width {
            self.formatted_cache.invalidate();
        }
        // Full redraw happens naturally on next render
    }

    fn cleanup(&mut self) {
        // Clear managed area
        execute!(stdout, Clear(ClearType::All))?;
        execute!(stdout, MoveTo(0, 0))?;

        // Dump history to native scrollback
        for message in &self.message_list.entries {
            println!("{}", self.format_for_exit(message));
        }
    }
}
```

### Why This Approach

| Factor              | Rationale                                                    |
| ------------------- | ------------------------------------------------------------ |
| **Resize handling** | Full re-render from memory avoids all terminal reflow issues |
| **Simplicity**      | No scroll region complexity, no cursor tracking heuristics   |
| **Search**          | Native Cmd+F works after exit (history dumped)               |
| **Proven pattern**  | Similar to Gemini CLI's approach                             |
| **Clean state**     | Terminal returns to normal state on exit                     |

### Trade-offs Accepted

| What We Lose                             | Mitigation                                                   |
| ---------------------------------------- | ------------------------------------------------------------ |
| Live scrollback search during session    | Page Up/Down for virtual scroll, search available after exit |
| Native terminal scrolling during session | Virtual scroll with Page Up/Down/Home/End                    |
| Mouse wheel scroll                       | Can implement if needed later                                |

### Implementation Notes

1. **Use ratatui widgets** for the bottom UI rendering (input, status, progress)
2. **Use raw crossterm** for chat area (direct positioning, no viewport abstraction)
3. **Use synchronized output** for flicker prevention
4. **Debounce resize events** (~100-500ms) to avoid thrashing
5. **Cache formatted lines** per width to avoid re-formatting on every scroll

### Alternative: Codex-style Hybrid

If live scrollback becomes a hard requirement:

1. Adopt Codex's custom terminal pattern
2. Implement scroll region handling for `insert_history_lines`
3. Accept resize heuristics may not be perfect
4. Accept some Claude Code-like issues (content loss on aggressive resize)

---

## Sources

### Specifications and Documentation

- [ratatui Viewport::Inline docs](https://docs.rs/ratatui/latest/ratatui/enum.Viewport.html)
- [ratatui Issue #2086 - Resize for inline viewport](https://github.com/ratatui/ratatui/issues/2086)
- [ratatui PR #1964 - set_viewport_height](https://github.com/ratatui/ratatui/pull/1964)
- [ratatui PR #1341 - scrolling-regions](https://github.com/ratatui/ratatui/pull/1341)
- [crossterm terminal docs](https://docs.rs/crossterm/latest/crossterm/terminal/index.html)

### Production Implementations

- [Codex CLI TUI](https://github.com/openai/codex) - Rust/ratatui hybrid approach
- [pi-mono TUI](https://github.com/badlogic/pi-mono) - TypeScript raw terminal
- [Ink](https://github.com/vadimdemedes/ink) - React terminal renderer (Claude Code base)

### Analysis and Discussion

- [The Signature Flicker](https://steipete.me/posts/2025/signature-flicker) - Claude Code rendering analysis
- [How Claude Code is Built](https://newsletter.pragmaticengineer.com/p/how-claude-code-is-built) - Architecture overview
- [ratatui discussions #579](https://github.com/ratatui/ratatui/discussions/579) - Performance best practices
- [Claude Code Issue #18493](https://github.com/anthropics/claude-code/issues/18493) - Resize content loss

### Prior ion Research

- [inline-viewport-scrollback-2026.md](/Users/nick/github/nijaru/ion/ai/research/inline-viewport-scrollback-2026.md)
- [pi-mono-tui-analysis.md](/Users/nick/github/nijaru/ion/ai/research/pi-mono-tui-analysis.md)
- [codex-tui-analysis.md](/Users/nick/github/nijaru/ion/ai/research/codex-tui-analysis.md)
- [tui-rendering-research.md](/Users/nick/github/nijaru/ion/ai/research/tui-rendering-research.md)

---

**Updated:** 2026-01-27
