# Chat Positioning Design

## Problem

When chat is short, content appears near the bottom with empty lines above:

```
ION
v0.0.0



                        <-- empty lines (bad)


> User message          <-- should be right after header
Agent response
─────────────────────
[input box]
─────────────────────
status line
```

## Root Cause

The current rendering uses `ScrollUp(n)` to make room for new chat:

```rust
execute!(stdout, MoveTo(0, ui_start))?;
execute!(stdout, ScrollUp(line_count))?;
// Print at ui_start - line_count
```

This works when chat fills the screen because:

1. Scroll pushes top content into scrollback
2. New content fills from the top of the gap
3. UI stays at bottom

But for short chat, it creates empty space because:

1. We scroll from near the bottom
2. Content prints near the bottom
3. Space between header and chat is empty

## Desired Behavior

```
ION
v0.0.0

> User message          <-- right after header
Agent response
─────────────────────   <-- UI follows chat
[input box]
─────────────────────
status line




                        <-- empty space at bottom (fine)
```

## States

1. **Startup** (no messages): Header visible, UI at anchor position
2. **Short chat** (fits on screen): Header + chat + UI, no scrolling
3. **Full chat** (exceeds screen): Header scrolls to scrollback, scroll-based insertion

## Solution: Content Row Tracking

Track where the next chat line should go:

```rust
struct RenderState {
    /// Row where next chat line should be printed.
    /// None = use scroll-based positioning (chat fills screen)
    chat_row: Option<u16>,
}
```

### Initialization

- On first message, `chat_row = startup_ui_anchor` (right after header)

### Printing Logic

```rust
if let Some(row) = chat_row {
    let space_needed = row + line_count + ui_height;
    if space_needed <= term_height {
        // Fits - print at row, advance chat_row
        print_at(row, chat_lines);
        chat_row = Some(row + line_count);
    } else {
        // Doesn't fit - transition to scroll mode
        // First, scroll existing content up
        scroll_to_fill_and_print(chat_lines);
        chat_row = None;
    }
} else {
    // Already in scroll mode
    scroll_and_print(chat_lines);
}
```

### UI Positioning

```rust
fn ui_start_row(&self) -> u16 {
    if let Some(chat_row) = self.chat_row {
        // UI follows chat
        chat_row
    } else {
        // UI at bottom
        term_height - ui_height
    }
}
```

### Transition to Scroll Mode

When chat exceeds available space:

1. Calculate how much content needs to scroll
2. Push header + early chat to scrollback
3. Position remaining chat so UI is at bottom
4. Set `chat_row = None`

### Reset on Clear/New Session

When starting fresh:

- Set `chat_row = None`
- Set `startup_ui_anchor` from cursor position after header

## Implementation Plan

1. Add `chat_row: Option<u16>` to App
2. Initialize from `startup_ui_anchor` on first message
3. Update `ui_start_row()` to use `chat_row`
4. Split printing logic: row-based vs scroll-based
5. Handle transition when space runs out
6. Reset on clear/new session

## Edge Cases

- **Resize when short**: Recalculate positions, may need full reflow
- **Very tall input**: UI height changes, may trigger scroll mode early
- **Resumed session with history**: Start in scroll mode (chat_row = None)

## Alternatives Considered

1. **Scroll regions**: Set scroll margins to exclude header area
   - Con: Complex interaction with crossterm, terminal compatibility

2. **Virtual viewport**: Maintain our own scroll position
   - Con: Lose native scrollback benefits, more state to manage

3. **Always reprint**: Clear and reprint all visible content each frame
   - Con: Performance, flicker

The row-tracking approach is simplest and maintains the existing architecture.
