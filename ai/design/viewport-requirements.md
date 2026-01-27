# Viewport Requirements

## Goal

Ion should feel like a native terminal application. Chat history lives in terminal scrollback (searchable, persists after exit). The input area lives in a dynamic viewport at the bottom.

## Architecture

```
┌─────────────────────────────────┐
│  Terminal Scrollback            │  ← Native terminal buffer
│  - Previous commands            │  ← Searchable with Cmd+F
│  - Chat history                 │  ← Inserted via insert_before
│  - User messages                │
│  - Agent responses              │
│  - Tool output                  │
├─────────────────────────────────┤
│  Viewport (dynamic height)      │  ← Ratatui inline viewport
│  - Progress line (0-2 lines)    │
│  - Input box (3+ lines)         │
│  - Status line (1 line)         │
└─────────────────────────────────┘
```

## Requirements

### R1: Viewport Size = UI Size (No Gaps)

The viewport height must exactly match the UI content at all times:

- Idle: progress(0-1) + input(3) + status(1) = 4-5 lines
- Running: progress(2) + input(3) + status(1) = 6 lines
- Multi-line input: progress + input(N+2 for borders) + status

**No empty lines between chat history and viewport.**

### R2: Dynamic Viewport Resizing

Viewport must resize when:

- Input grows (user types multi-line)
- Input shrinks (user submits or deletes)
- Progress state changes (running vs idle)

Resizing must:

- Preserve scrollback content above
- Not cause visual glitches
- Not lose content

### R3: Chat History in Native Scrollback

All chat content goes into terminal scrollback via `insert_before`:

- User messages
- Agent responses (streamed)
- Tool calls and output
- System messages

Benefits:

- Terminal search (Cmd+F / Ctrl+Shift+F) works
- Native scroll (mouse, keyboard)
- Content persists after ion exits
- No custom scrollback implementation needed

### R4: Multi-line Input Support

Users frequently write multi-line prompts. Requirements:

- Input box grows as user types (up to reasonable max)
- Viewport grows to accommodate
- When input exceeds max visible lines, input scrolls internally
- Ctrl+G opens external editor for very long input

Suggested limits:

- Min input: 1 line (3 with borders)
- Max input before internal scroll: ~10-12 lines
- Max viewport: terminal_height - 2 (leave room for context)

### R5: Startup Behavior

On launch:

1. Print header to scrollback (ION banner, version)
2. Create viewport at current cursor position
3. Viewport sized for idle state (~5 lines)
4. No large gaps or empty space

### R6: Terminal Resize Handling

When terminal resizes:

- Viewport width adjusts automatically
- Viewport height recalculates based on content
- Scrollback content reflows naturally (terminal handles this)
- No content loss

### R7: Native Terminal Features

Must preserve:

- Search (Cmd+F, Ctrl+Shift+F)
- Copy/paste selection
- Mouse scroll
- Scrollback buffer
- Terminal themes/colors

## UI States

### Idle (no task running)

```
[scrollback with chat history]
┌─────────────────────────┐
│ >                       │  ← Input (3 lines with borders)
└─────────────────────────┘
 model · tokens · ? help     ← Status (1 line)
```

Viewport: 4 lines

### Idle with completion summary

```
[scrollback with chat history]
 ✓ Completed (5s · ↑ 10k)    ← Progress (1 line)
┌─────────────────────────┐
│ >                       │
└─────────────────────────┘
 model · tokens · ? help
```

Viewport: 5 lines

### Running

```
[scrollback with chat history]
 ⠸ Ionizing... (5s · Esc)    ← Progress (1 line)
┌─────────────────────────┐
│ >                       │
└─────────────────────────┘
 model · tokens · ? help
```

Viewport: 5 lines

### Running with queued messages

```
[scrollback with chat history]
 ↳ 2 messages queued         ← Queued indicator (1 line)
 ⠸ Ionizing... (5s · Esc)    ← Progress (1 line)
┌─────────────────────────┐
│ >                       │
└─────────────────────────┘
 model · tokens · ? help
```

Viewport: 6 lines

### Multi-line input

```
[scrollback with chat history]
┌─────────────────────────┐
│ > First line            │
│   Second line           │
│   Third line            │
│   Fourth line           │
└─────────────────────────┘
 model · tokens · ? help
```

Viewport: 7 lines (5 content + 2 borders + 1 status)

## Edge Cases

### E1: Input exceeds max visible lines

- Input box stops growing at max height
- Content scrolls within input box
- User can still type unlimited content
- Ctrl+G for external editor recommended for very long input

### E2: Viewport would exceed terminal height

- Cap viewport at terminal_height - 1
- Input scrolls internally
- Never push viewport off screen

### E3: Very rapid resize (typing fast)

- Debounce or batch resize operations
- Avoid flickering

### E4: Content inserted while user is scrolled up

- New content still goes to scrollback
- User's scroll position preserved
- Indicator that new content exists? (optional)

### E5: External editor (Ctrl+G)

- Temporarily exit viewport
- Restore terminal to normal mode
- Launch $EDITOR
- On return, restore viewport with editor content

## What We Need from Ratatui

1. **`set_viewport_height()`** - Dynamic resize (PR #1964)
2. **`insert_before()`** - Already exists, works well
3. **Scroll region support** - `scrolling-regions` feature for efficiency

## Alternatives Considered

### Fullscreen viewport

- Would require custom scrollback implementation
- Lose native terminal search
- More complex, more bugs
- **Rejected**

### Fixed large viewport

- Always reserve max space (15 lines)
- Gaps when UI is small
- **Current approach, causes gap bugs**

### Scrolling within viewport

- Chat history in viewport, not scrollback
- Custom scroll handling
- Lose native features
- **Rejected**

## Implementation Path

1. Enable `scrolling-regions` feature in ratatui (if not already)
2. Either:
   a. Wait for PR #1964 to merge, or
   b. Fork ratatui and cherry-pick PR #1964, or
   c. Implement `set_viewport_height` ourselves using the PR as reference
3. Modify main loop to calculate needed viewport height each frame
4. Call `set_viewport_height` before `draw` when height changes
5. Remove positioning hacks (UI at bottom of viewport)
6. Test all edge cases
