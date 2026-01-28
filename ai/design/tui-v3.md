# TUI v3: Managed History with Exit Dump

## Problem with TUI v2

TUI v2 attempted to use native scrollback during the session:

- Print chat via `println!()` → native scrollback
- Manage bottom UI with cursor positioning

**Issues discovered:**

1. Can't re-render content already in scrollback (terminal owns it)
2. Resize causes terminal rewrap we can't control
3. Scroll/print/clear logic conflicts (header not showing, artifacts)
4. Empty newlines on startup (fullscreen claim with no content)

## What Claude Code Does (Observed)

Based on testing and research:

1. **During session**: Manages rendering of chat history themselves (not native scrollback)
2. **On resize**: ~1s debounce, then re-renders visible portion from memory at new width
3. **On exit**: Dumps formatted history to native scrollback, cleans up UI
4. **Result**: Terminal prompt appears cleanly, history searchable via Cmd+F

```
During session:           After exit:
┌────────────────────┐    $ command
│ [MANAGED AREA]     │    $ another command
│ Chat history       │    > user message
│ (re-renders on     │
│  resize)           │      agent response
├────────────────────┤
│ Bottom UI          │    • tool(args)
│ (input, status)    │      ⎿ result
└────────────────────┘
                          $ ← prompt here, no extra newlines
```

## TUI v3 Architecture

### Render Model

```
┌─────────────────────────────────────┐
│ MANAGED CHAT AREA                   │  ← We render from memory
│ - Visible portion of chat history   │  ← Re-render on resize
│ - Page Up/Down for scrolling        │  ← Virtual scroll
├─────────────────────────────────────┤
│ Progress line                       │  ← 1 line
├─────────────────────────────────────┤
│ Input area (dynamic height)         │  ← Borders + content
├─────────────────────────────────────┤
│ Status line                         │  ← 1 line
└─────────────────────────────────────┘
```

### Key Differences from v2

| Aspect                | TUI v2                      | TUI v3                         |
| --------------------- | --------------------------- | ------------------------------ |
| Chat history          | Native scrollback (println) | Managed rendering from memory  |
| Resize handling       | Can't re-render scrollback  | Re-render visible portion      |
| Scrolling             | Native terminal             | Page Up/Down (virtual scroll)  |
| Search during session | Native Cmd+F                | Not available                  |
| Search after exit     | Native Cmd+F                | Native Cmd+F (history dumped)  |
| Startup               | Fullscreen with empty space | Inline, grows as content added |

### Data Flow

```rust
struct App {
    // Chat history (single source of truth)
    message_list: MessageList,  // Already exists

    // Virtual scroll state
    chat_scroll_offset: usize,  // Lines scrolled up

    // Render state
    last_render_width: Option<u16>,
}

// Render loop
fn render(&mut self, width: u16, height: u16) {
    let chat_area_height = height - self.bottom_ui_height();

    // Calculate visible chat lines
    let all_lines = self.format_chat_history(width);
    let visible_start = all_lines.len().saturating_sub(chat_area_height + self.chat_scroll_offset);
    let visible_lines = &all_lines[visible_start..visible_start + chat_area_height];

    // Render visible chat
    for (row, line) in visible_lines.iter().enumerate() {
        execute!(stdout(), MoveTo(0, row))?;
        write!(stdout(), "{}", line)?;
    }

    // Render bottom UI
    self.render_bottom_ui(width, height)?;
}

// On exit
fn cleanup(&self) {
    // Clear managed area
    execute!(stdout(), Clear(ClearType::All))?;

    // Dump history to native scrollback
    for message in &self.message_list.entries {
        println!("{}", self.format_message(message));
    }
}
```

### Resize Handling

```rust
fn handle_resize(&mut self, new_width: u16, new_height: u16) {
    // Width change: re-render chat at new width (automatic - we render from memory)
    // Height change: adjust visible portion (automatic - we calculate each frame)

    // Just trigger a full redraw
    self.needs_full_redraw = true;
}
```

### Scrolling

```rust
fn handle_key(&mut self, key: KeyEvent) {
    match key.code {
        KeyCode::PageUp => {
            let visible_height = self.chat_area_height();
            self.chat_scroll_offset = self.chat_scroll_offset.saturating_add(visible_height / 2);
        }
        KeyCode::PageDown => {
            let visible_height = self.chat_area_height();
            self.chat_scroll_offset = self.chat_scroll_offset.saturating_sub(visible_height / 2);
        }
        // ... other keys
    }
}
```

## Performance Considerations

### Memory Usage

| Data                    | Size Estimate      | Notes                          |
| ----------------------- | ------------------ | ------------------------------ |
| Raw message text        | 1-10KB per message | User prompts + agent responses |
| Long session (500 msgs) | 0.5-5MB            | Negligible for modern systems  |
| Formatted line cache    | 2-3x raw           | Only for current width         |

**Verdict:** Memory is not a concern. A 1000-message session uses <20MB.

### Render Performance

**Key principle: Only render what's visible**

```
Total messages: 500
Formatted lines: 2000 (after word-wrap)
Visible lines: 40 (terminal height - UI)
Render cost: O(40), not O(2000)
```

### Caching Strategy

```rust
struct FormattedCache {
    width: u16,                    // Invalidate if width changes
    lines: Vec<FormattedLine>,     // Cached formatted output
    message_count: usize,          // Invalidate if new messages
}

struct FormattedLine {
    content: String,               // Pre-formatted with styles
    source_message_idx: usize,     // For scroll position tracking
}
```

**Cache invalidation:**

- Width changes → full re-format (unavoidable, word-wrap changes)
- New message → append to cache (incremental)
- Scroll → no re-format needed, just render different slice

### Operation Costs

| Operation      | Cost             | When           | Mitigation                  |
| -------------- | ---------------- | -------------- | --------------------------- |
| Initial format | O(n) messages    | Session load   | Lazy format on first render |
| Render frame   | O(visible) lines | Every 50ms     | Only visible portion        |
| Width resize   | O(n) messages    | User resizes   | Debounce 100-500ms          |
| Height resize  | O(1)             | User resizes   | Just adjust visible slice   |
| New message    | O(1) message     | Agent responds | Append to cache             |
| Scroll         | O(visible) lines | Page Up/Down   | No re-format, just render   |

### Compaction and Display History

**Important distinction:**

- `message_list` = Display history (what user sees) - KEPT
- Agent context = LLM conversation (gets compacted) - SEPARATE

Compaction does NOT affect display history. User can still scroll through full chat even after context compaction.

```rust
struct App {
    // Display history - persists entire session
    message_list: MessageList,     // Never compacted

    // Agent context - may be compacted
    agent: Agent,                  // Has its own message history
}
```

### Worst Case Analysis

**Scenario:** 1000 messages, 100 chars avg, 80-char terminal width

```
Raw text: 1000 * 100 = 100KB
Formatted lines: ~1250 lines (1.25 lines per message avg)
Format time: <10ms (string ops are fast)
Render time: <1ms (40 visible lines)
```

**Scenario:** Resize from 200 → 80 chars wide

```
Re-format all: <10ms
Debounce: 100ms minimum anyway
User perception: Instant
```

### Optimizations (implement if needed)

1. **Lazy formatting:** Only format messages as they scroll into view
2. **Incremental cache:** Keep old formatted lines, only re-format on width change
3. **Virtual list:** For 10K+ messages, only keep nearby messages formatted
4. **Background formatting:** Format in separate task during idle

**For v0.0.0:** Simple eager formatting is fine. Optimize later if profiling shows need.

## Implementation Plan

### Phase 1: Managed Chat Rendering

1. Add `chat_scroll_offset` to App
2. Implement `format_chat_history(width) -> Vec<String>`
3. Render visible portion instead of println
4. Handle Page Up/Down

### Phase 2: Resize Handling

1. Debounce resize events
2. Full clear + re-render on resize
3. Test with varying widths

### Phase 3: Exit Cleanup

1. Clear screen on exit
2. Dump formatted history to stdout
3. Ensure clean terminal state (cursor visible, raw mode off)

### Phase 4: Polish

1. Scroll position indicator
2. Optimize with line caching
3. Handle edge cases (empty history, very long messages)

## Open Questions

- [ ] How does Claude Code handle mouse scroll? (Might need mouse event handling)
- [ ] Should we show a scroll indicator? (e.g., "↑ 50 more lines")
- [ ] Debounce duration for resize? (Claude Code appears to use ~1s)

## References

- TUI v2 design: `ai/design/tui-v2.md`
- Inline viewport research: `ai/research/inline-viewport-scrollback-2026.md`
- Claude Code architecture: `ai/research/claude-code-architecture.md`

## Status

**Phase:** Design complete, ready for implementation
**Updated:** 2026-01-27
