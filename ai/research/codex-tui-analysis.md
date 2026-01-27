# Codex CLI TUI Implementation Analysis

Deep analysis of OpenAI Codex CLI's terminal interface implementation, focusing on their "terminal-native" approach, viewport handling, and the TUI2 experiment abandonment.

**Date:** 2026-01-26

## Summary

| Aspect       | Codex Approach                               |
| ------------ | -------------------------------------------- |
| Framework    | ratatui + crossterm (Rust)                   |
| Primary mode | Inline viewport (not alternate screen)       |
| Overlays     | Alternate screen for fullscreen views        |
| Scrollback   | `insert_history_lines()` with scroll regions |
| Key feature  | `scrolling-regions` ratatui feature          |
| TUI2 status  | Removed in PR #9640 (2026-01-22)             |

**Key insight:** Codex maintains a hybrid approach - inline viewport for the main chat interface with native terminal scrollback, switching to alternate screen only for fullscreen overlays (transcript viewer, diffs, approvals).

## Terminal-Native Definition

"Terminal-native" in Codex's context means:

1. **Scrollback preservation** - Chat history lives in the terminal's native scrollback buffer, not a virtual buffer
2. **Native selection** - Text selection uses the terminal emulator's built-in selection, not app-managed
3. **Native copy** - Copy operations work as users expect from their terminal
4. **Multiplexer compatibility** - Works with tmux, Zellij, iTerm tabs without special handling

From [Issue #8344](https://github.com/openai/codex/issues/8344):

> "terminal functionality that works uniformly everywhere trumps sophisticated but environment-specific features"

## Inline Viewport vs Fullscreen

### Main Interface (Inline)

Codex uses an inline viewport for the primary chat interface:

```rust
// From init() in tui.rs - notably NO EnterAlternateScreen
pub fn init() -> Result<Terminal> {
    set_modes()?;
    set_panic_hook();
    let backend = CrosstermBackend::new(stdout());
    let tui = CustomTerminal::with_options(backend)?;
    Ok(tui)
}
```

The viewport dynamically adjusts height based on content:

```rust
// From app.rs
tui.draw(
    self.chat_widget.desired_height(tui.terminal.size()?.width),
    |frame| {
        self.chat_widget.render(frame.area(), frame.buffer);
        if let Some((x, y)) = self.chat_widget.cursor_pos(frame.area()) {
            frame.set_cursor_position((x, y));
        }
    },
)?;
```

### Overlay Mode (Alternate Screen)

Alternate screen is used only for fullscreen overlays:

```rust
pub fn enter_alt_screen(&mut self) -> Result<()> {
    if !self.alt_screen_enabled {
        return Ok(());
    }
    let _ = execute!(self.terminal.backend_mut(), EnterAlternateScreen);
    // Enable "alternate scroll" so terminals may translate wheel to arrows
    let _ = execute!(self.terminal.backend_mut(), EnableAlternateScroll);
    if let Ok(size) = self.terminal.size() {
        self.alt_saved_viewport = Some(self.terminal.viewport_area);
        self.terminal.set_viewport_area(Rect::new(0, 0, size.width, size.height));
        let _ = self.terminal.clear();
    }
    self.alt_screen_active.store(true, Ordering::Relaxed);
    Ok(())
}
```

Triggers for alternate screen (from app.rs):

- `Ctrl+T` - Transcript viewer
- Approval requests (patches, exec commands, MCP elicitation)
- Diff/patch display

### Configuration Option

Codex provides a config option to disable alternate screen entirely:

```rust
pub fn set_alt_screen_enabled(&mut self, enabled: bool) {
    self.alt_screen_enabled = enabled;
}
```

This addresses [Issue #2836](https://github.com/openai/codex/issues/2836) for Zellij/multiplexer users who want scrollback to work normally.

## Ratatui Viewport Approach

### Not Using Viewport::Inline

Codex does NOT use ratatui's built-in `Viewport::Inline`. Instead, they use a custom terminal implementation (`custom_terminal.rs`) that:

1. Manages viewport area manually with `set_viewport_area()`
2. Tracks cursor position for resize handling
3. Implements custom scrollback insertion

From `custom_terminal.rs`:

```rust
// Terminal manages double-buffered rendering with:
// - viewport_area: Current drawable region
// - last_known_cursor_pos: For resize coordination
// - last_known_screen_size: For detecting resizes
```

### Custom Viewport Management

The `pending_viewport_area()` method handles resize heuristics:

```rust
fn pending_viewport_area(&mut self) -> Result<Option<Rect>> {
    let screen_size = terminal.size()?;
    let last_known_screen_size = terminal.last_known_screen_size;
    if screen_size != last_known_screen_size
        && let Ok(cursor_pos) = terminal.get_cursor_position()
    {
        let last_known_cursor_pos = terminal.last_known_cursor_pos;
        // If we resized AND the cursor moved, we adjust the viewport area
        // This is a heuristic that seems to work well at least in iTerm2
        if cursor_pos.y != last_known_cursor_pos.y {
            let offset = Offset {
                x: 0,
                y: cursor_pos.y as i32 - last_known_cursor_pos.y as i32,
            };
            return Ok(Some(terminal.viewport_area.offset(offset)));
        }
    }
    Ok(None)
}
```

## Scrollback Coordination

### Insert History Lines

The key scrollback mechanism is `insert_history_lines()`:

```rust
pub fn insert_history_lines<B>(
    terminal: &mut Terminal<B>,
    lines: Vec<Line>,
) -> io::Result<()>
where
    B: Backend + Write,
{
    // Pre-wrap lines using word-aware wrapping
    let wrapped = word_wrap_lines_borrowed(&lines, area.width.max(1) as usize);

    // Set scroll region above viewport
    queue!(writer, SetScrollRegion(1..area.top()))?;
    queue!(writer, MoveTo(0, cursor_top))?;

    // Write lines with proper styling
    for line in wrapped {
        queue!(writer, Print("\r\n"))?;
        // ... style and content output
        write_spans(writer, merged_spans.iter())?;
    }

    queue!(writer, ResetScrollRegion)?;
    queue!(writer, MoveTo(last_cursor_pos.x, last_cursor_pos.y))?;
    Ok(())
}
```

### Scroll Region Commands

Custom crossterm commands for scroll regions:

```rust
pub struct SetScrollRegion(pub std::ops::Range<u16>);

impl Command for SetScrollRegion {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write!(f, "\x1b[{};{}r", self.0.start, self.0.end)
    }
}

pub struct ResetScrollRegion;

impl Command for ResetScrollRegion {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write!(f, "\x1b[r")
    }
}
```

### Viewport Scrolling

Dynamic viewport adjustment in `draw()`:

```rust
// If the viewport has expanded, scroll everything else up to make room
if area.bottom() > size.height {
    terminal
        .backend_mut()
        .scroll_region_up(0..area.top(), area.bottom() - size.height)?;
    area.y = size.height - area.height;
}
```

## Ratatui Features Enabled

From `Cargo.toml`:

```toml
ratatui = { workspace = true, features = [
    "scrolling-regions",
    "unstable-backend-writer",
    "unstable-rendered-line-info",
    "unstable-widget-ref",
] }
```

| Feature                       | Purpose                                                                               |
| ----------------------------- | ------------------------------------------------------------------------------------- |
| `scrolling-regions`           | Enables `scroll_region_up/down` backend methods for flicker-free scrollback insertion |
| `unstable-backend-writer`     | Direct access to backend for custom ANSI commands                                     |
| `unstable-rendered-line-info` | Line rendering metadata                                                               |
| `unstable-widget-ref`         | Render widgets by reference                                                           |

### scrolling-regions Feature

From [ratatui PR #1341](https://github.com/ratatui/ratatui/pull/1341):

> Uses terminal scrolling regions to implement `Terminal::insert_before` without flickering. When a scroll ANSI sequence is sent to the terminal with a non-default scrolling region, the terminal scrolls just inside that region.

This is why Codex can insert history lines above the viewport without visible flicker.

## Why TUI2 Was Abandoned

### The Experiment

TUI2 was an experimental alternate-screen-based UI that:

- Owned the entire viewport
- Managed its own transcript/scrollback
- Provided high-fidelity code copying
- Better resize/rewrap behavior

### The Problems

From [PR #9640](https://github.com/openai/codex/pull/9640) removing TUI2:

> "Making a full-screen, transcript-owned TUI feel truly native across real-world environments is far more complex than it appears."

Specific issues:

1. **Environment matrix explosion** - Terminal emulators, OSes, tmux, input devices, keyboard layouts/IME, fonts, overlays
2. **Scrolling regressions** - Users reported broken scrolling behavior
3. **Selection/copy issues** - Copy operations didn't work as expected
4. **CPU usage** - High CPU from tight render loops ([Issue #8176](https://github.com/openai/codex/issues/8176))

### The Decision

From [Issue #8344](https://github.com/openai/codex/issues/8344):

> Rather than abandon their learnings, Codex is pursuing a "redraw-based approach" that preserves "native terminal scrolling, selection, and copy behavior" while addressing resize and correctness issues.

The philosophy:

> "terminal functionality that works uniformly everywhere trumps sophisticated but environment-specific features"

## Key Implementation Patterns

### 1. Synchronized Updates

All drawing happens within a synchronized update block:

```rust
stdout().sync_update(|_| {
    // All terminal operations here
    // Prevents partial renders being visible
})?;
```

### 2. Deferred History Lines

History lines are queued and flushed during draw:

```rust
pub fn insert_history_lines(&mut self, lines: Vec<Line<'static>>) {
    self.pending_history_lines.extend(lines);
    self.frame_requester().schedule_frame();
}

// In draw():
if !self.pending_history_lines.is_empty() {
    insert_history_lines(terminal, self.pending_history_lines.clone())?;
    self.pending_history_lines.clear();
}
```

### 3. Event Stream Pause/Resume

For external program execution:

```rust
pub fn pause_events(&mut self) {
    self.event_broker.pause_events();
}

pub fn resume_events(&mut self) {
    self.event_broker.resume_events();
}
```

### 4. External Program Restoration

Temporarily restore terminal state:

```rust
pub async fn with_restored<R, F, Fut>(&mut self, mode: RestoreMode, f: F) -> R {
    self.pause_events();
    let was_alt_screen = self.is_alt_screen_active();
    if was_alt_screen {
        let _ = self.leave_alt_screen();
    }
    mode.restore()?;

    let output = f().await;

    set_modes()?;
    flush_terminal_input_buffer();
    if was_alt_screen {
        let _ = self.enter_alt_screen();
    }
    self.resume_events();
    output
}
```

### 5. Alternate Scroll Mode

Custom ANSI commands for wheel-to-arrow translation:

```rust
struct EnableAlternateScroll;

impl Command for EnableAlternateScroll {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write!(f, "\x1b[?1007h")  // DEC private mode 1007
    }
}
```

## Architecture Diagram

```
+--Terminal Screen----------------------------------+
|                                                   |
| [Scrollback History - managed by terminal]        |
| > Previous user message                           |
| > Previous assistant response                     |
| > Tool output                                     |
|                                                   |
+--- Scroll Region Boundary ------------------------+
|                                                   |
| [Viewport - managed by Codex]                     |
| +-----------------------------------------------+ |
| | Current streaming response...                 | |
| | ...                                           | |
| +-----------------------------------------------+ |
| | > Input prompt                                | |
| +-----------------------------------------------+ |
|                                                   |
+---------------------------------------------------+
```

When in alternate screen (overlay mode):

```
+--Alternate Screen---------------------------------+
|                                                   |
| [Full Transcript / Diff / Approval View]          |
| (All content managed by Codex)                    |
|                                                   |
| [Navigation: j/k, Ctrl+D/U, etc.]                |
|                                                   |
+---------------------------------------------------+
```

## Recommendations for ion

Based on Codex's approach:

1. **Use inline viewport by default** - Main chat stays in terminal scrollback
2. **Enable `scrolling-regions` feature** - For flicker-free history insertion
3. **Implement `insert_history_lines`** - Push completed messages to scrollback
4. **Use alternate screen sparingly** - Only for fullscreen overlays
5. **Provide config option** - Allow users to disable alternate screen entirely
6. **Track viewport position** - Handle resizes with cursor position heuristics
7. **Synchronized updates** - Use crossterm's `SynchronizedUpdate` for flicker-free rendering

## References

- [Codex TUI Source](https://github.com/openai/codex/tree/main/codex-rs/tui/src)
- [PR #9640 - Remove TUI2](https://github.com/openai/codex/pull/9640)
- [Issue #8344 - TUI Native Concerns](https://github.com/openai/codex/issues/8344)
- [Issue #2836 - Alt-Screen in Multiplexers](https://github.com/openai/codex/issues/2836)
- [Issue #8176 - TUI2 CPU Usage](https://github.com/openai/codex/issues/8176)
- [ratatui scrolling-regions PR #1341](https://github.com/ratatui/ratatui/pull/1341)
- [ratatui Viewport::Inline docs](https://docs.rs/ratatui/latest/ratatui/enum.Viewport.html)
