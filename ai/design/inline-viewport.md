# Inline Viewport TUI Design

**Task:** tk-h8iw
**Status:** Design
**Research:** ai/research/inline-viewport-2026.md

## Overview

Migrate ion TUI from alternate screen mode to inline viewport mode using ratatui's `Viewport::Inline`.

## Current vs Target

| Aspect        | Current (Alternate Screen) | Target (Inline Viewport) |
| ------------- | -------------------------- | ------------------------ |
| Buffer        | Separate alternate screen  | Main terminal buffer     |
| History       | App-managed MessageList    | Terminal scrollback      |
| Selection     | Requires Shift+click       | Native terminal          |
| Scroll        | App handles (buggy)        | Terminal handles         |
| Mouse capture | Required for scroll        | Not needed               |
| Exit behavior | Returns to previous        | Output persists          |

## Target UI Layout

```
[terminal scrollback - handled by terminal]
... previous messages scroll up naturally ...

─────────────────────────────────────────────
> [input area - single or multi-line]        |  viewport
─────────────────────────────────────────────|  (fixed
  Model · tokens/limit  |  [branch] · path   |  height)
  permission mode indicator                  |
─────────────────────────────────────────────
```

Viewport height: ~4-6 lines (configurable)

- Input area: 1-3 lines (expandable)
- Status bar: 1 line
- Separator/padding: 1-2 lines

## Architecture Changes

### 1. Terminal Setup (main.rs)

```rust
// Before
execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
let terminal = Terminal::new(backend)?;

// After
terminal::enable_raw_mode()?;
let terminal = Terminal::with_options(
    CrosstermBackend::new(io::stderr()),
    TerminalOptions {
        viewport: Viewport::Inline(VIEWPORT_HEIGHT),
    },
)?;
// No alternate screen, no mouse capture
```

### 2. Message Rendering

```rust
// Before: Store in MessageList, render in draw()
self.message_list.push_entry(entry);
// ... later in draw()
message_list_widget.render(area, buf);

// After: Push directly to terminal scrollback
terminal.insert_before(line_count, |buf| {
    render_message(&entry, buf);
})?;
```

### 3. Streaming Content

During streaming, content updates in viewport. On completion:

```rust
// When message completes
let lines = render_message_to_lines(&message);
terminal.insert_before(lines.len(), |buf| {
    for (i, line) in lines.iter().enumerate() {
        line.render(Rect { y: i as u16, ..buf.area }, buf);
    }
})?;
// Clear viewport for next interaction
```

### 4. Input Area

Stays in viewport, similar to current but simpler:

```rust
fn draw_viewport(&mut self, frame: &mut Frame) {
    let area = frame.area();

    // Split: input + status
    let [input_area, status_area] = Layout::vertical([
        Constraint::Min(1),    // Input expands
        Constraint::Length(2), // Status fixed
    ]).areas(area);

    // Render input
    let input = Paragraph::new(&self.input)
        .block(Block::bordered());
    frame.render_widget(input, input_area);

    // Render status bar
    self.render_status(frame, status_area);
}
```

## Components to Modify

| File                    | Changes                                                |
| ----------------------- | ------------------------------------------------------ |
| src/main.rs             | Remove alternate screen, use Viewport::Inline          |
| src/tui/mod.rs          | Simplify draw(), add insert_before() calls             |
| src/tui/message_list.rs | Remove or repurpose (may keep for tool collapse state) |
| src/tui/highlight.rs    | Keep - still needed for syntax highlighting            |

## Components to Remove/Simplify

- **MessageList scroll tracking** - terminal handles
- **Mouse scroll handling** - not needed
- **Chat history rendering** - replaced by insert_before
- **Auto-scroll logic** - not needed

## Migration Steps

1. **Create inline terminal wrapper**
   - New struct or modify App to use Viewport::Inline
   - Handle raw mode setup/teardown

2. **Implement message push**
   - Add method to push completed messages via insert_before
   - Handle multi-line messages properly

3. **Simplify draw()**
   - Only render input area + status in viewport
   - Remove message list rendering

4. **Handle streaming**
   - Show streaming content in viewport
   - Push to scrollback on completion

5. **Remove alternate screen code**
   - Remove EnterAlternateScreen/LeaveAlternateScreen
   - Remove mouse capture
   - Remove scroll handling

6. **Update external editor flow**
   - May need adjustment for inline mode

## Open Questions

1. **Viewport height**: Fixed or dynamic based on input size?
2. **Streaming display**: In viewport or temporary lines above?
3. **Tool output**: Collapsed in scrollback or full output?
4. **Resize handling**: How to handle terminal resize?

## Risks

| Risk                   | Mitigation                            |
| ---------------------- | ------------------------------------- |
| Widget compatibility   | Test existing widgets in inline mode  |
| Terminal compatibility | Test on iTerm2, Terminal.app, Ghostty |
| Resize edge cases      | Use terminal.autoresize()             |
| Input focus            | Ensure cursor positioning works       |

## References

- Research: ai/research/inline-viewport-2026.md
- ratatui inline example: https://ratatui.rs/examples/apps/inline/
- Viewport docs: https://docs.rs/ratatui/latest/ratatui/enum.Viewport.html
