# Inline Viewport TUI Design

**Task:** tk-h8iw
**Status:** Design
**Research:** ai/research/inline-viewport-2026.md

## Overview

Migrate ion TUI from alternate screen mode to inline viewport mode using ratatui's `Viewport::Inline`.

## Decision Summary

- **Primary mode**: Inline viewport is the default and the only fully supported UI mode.
- **Alternate screen**: Prefer removal to avoid dual rendering paths and duplicated QA.
- **Selectors**: Use a single bottom-anchored selector UI for help/config/provider/model/plugin.
- **Status line**: Model + context usage on left, help hotkey on right. No cwd/path, no git by default.
- **Messages**: User messages use a `>` prefix; no model header for agent messages. Model changes are logged inline when they occur.

## Current vs Target

| Aspect        | Current (Alternate Screen) | Target (Inline Viewport) |
| ------------- | -------------------------- | ------------------------ |
| Buffer        | Separate alternate screen  | Main terminal buffer     |
| History       | App-managed MessageList    | Terminal scrollback      |
| Selection     | Requires Shift+click       | Native terminal          |
| Scroll        | App handles (buggy)        | Terminal handles         |
| Mouse capture | Required for scroll        | Not needed               |
| Exit behavior | Returns to previous        | Output persists          |

## Inline Scrollback Issues (Current)

**Symptoms**

- Scrolling up stops early; older conversation is cut off.
- Up/Down history navigation feels misaligned due to extra blank lines between messages.
- Terminal native scrollback is not fully usable because the app still renders a chat viewport.

**Root Cause**

- Chat history is still rendered inside the TUI viewport with app-managed scroll state.
- The viewport is effectively a self-managed scroll region, preventing terminal-native scrollback.

**Plan**

- Remove chat history rendering from the viewport entirely.
- Use ratatui `insert_before` to append completed messages into terminal scrollback.
- Keep only the input + progress + status lines in the viewport.
- Remove app-managed scroll controls and any internal scroll clamping.

## Alternate Screen Compatibility

- The bottom-anchored selector UI can be implemented in alternate screen mode, but it adds a second rendering path and doubles QA surface.
- Maintaining both modes increases risk: duplicated layout logic, higher bug count, and more edge cases in resize/scroll.
- Recommendation: focus on inline as primary. If alternate is retained, treat it as best-effort and explicitly document gaps.

## Target UI Layout

```
[terminal scrollback - handled by terminal]
... previous messages scroll up naturally ...

  Ionizing line / task state                 |
─ [Write] ───────────────────────────────────
> [input area - single or multi-line]        |
─────────────────────────────────────────────
  Model · tokens/limit               ? help  |
```

Viewport height: ~4-6 lines (configurable)

- Input area: 1-3 lines (expandable)
- Status bar: 1 line (model/tokens left, help hotkey right)
- Ionizing line: 1 line (also indicates completion)
- Separators: 2 lines (mode header, input bottom)

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

    // Split: ionizing line + input + status
    let [ionizing_area, input_area, status_area] = Layout::vertical([
        Constraint::Length(1), // Ionizing/task state
        Constraint::Min(1),    // Input expands
        Constraint::Length(1), // Model/tokens + help hotkey
    ])
    .areas(area);

    // Render input
    let input = Paragraph::new(&self.input)
        .block(Block::new().borders(Borders::TOP | Borders::BOTTOM));
    frame.render_widget(input, input_area);

    // Render ionizing line + status
    self.render_ionizing(frame, ionizing_area);
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
- **Chat history box label** - remove (ionizing line covers task state)

## Modal and Picker Behavior

- **Inline default**: Use a single bottom-anchored takeover UI for all selectors and config screens.
- **Scope**: Help and provider/model selection share the same selector shell (search box, list, hint line).
- **Settings**: No settings viewport for MVP; use config file edits for now. Add settings view once there are multiple settings worth editing in-app.
- **Behavior**: Replaces the viewport (input + status) while open, leaving terminal scrollback untouched.
- **Rendering mode**: Inline only (no alternate screen). Full-screen model selector uses a temporarily expanded inline viewport height.
- **Insert buffering**: While selector is open, buffer `insert_before` chat output and flush on close to avoid interleaving into the selector.
- **Optional guard**: Consider blocking selector open while a task is running to reduce edge cases.
- **Exit**: Escape returns to the normal viewport.

## Selector Shell (Shared UI)

```
─────────────────────────────────────────────
  Settings:  Status   Config   Usage

  Configure ion preferences

  ╭────────────────────────────────────────╮
  │ ⌕ Search settings...                   │
  ╰────────────────────────────────────────╯

  ❯ Auto-compact                            true
    Show tips                               true
    ...

  Type to filter · Enter/↓ to select · Esc to close
```

- **Layout**: Title line, optional tabs, description, search input, list, hint line.
- **Pages**: Provider/Model share one selector with two pages (tabs).
  - `provider` command opens Provider page.
  - `model` command opens Model page.
  - On startup: if no provider configured, open Provider page. If provider set but no model, open Model page.
- **State**: query string, filtered results, selection index, scroll offset.
- **Input**: type-to-filter, arrows for navigation, Enter to apply, Esc to exit.
- **Search**: Fuzzy match for provider/model, filename `@` inclusion, and slash commands.

## Provider Selection Commit Semantics

**Goal:** Avoid changing the active provider until a model from that provider is actually selected.

**Rules**

- If a provider is already configured, selecting a provider in the selector does **not** persist or switch providers yet.
- The provider is committed only when a model is chosen from that provider.
- If no provider is set (first-time setup), selecting a provider sets it immediately to allow model discovery.

**Rationale**

- Prevents partial state (provider changed without a valid model).
- Reduces edge cases in model loading and API key flow.

## Input Handling

- **Goal**: Avoid custom cursor/selection edge cases (graphemes, emojis, multi-line).
- **Plan**: Use `rat-text::TextArea` for the main multi-line input and selector search fields to minimize future rewrites. Keep custom input history and queue-edit behavior at the app layer.

## Input UI Variants

**Current**

- Custom top/bottom bars with a prompt gutter and no side borders.

**Alternative**

- Use a standard `Block` with `Borders::ALL` around the input area.

**Plan**

- Keep both implementations available (bars vs block).
- Refactor input rendering into helper functions (header/border, prompt gutter, textarea) to reduce layout duplication.

## Message Queue Behavior

- **Queueing**: While a task is running, Enter enqueues messages (FIFO).
- **Submission**: All queued messages are inserted into the session on the next turn and submitted together.
- **Editing**: Pressing Up when input is empty pulls all queued messages back into the input editor for batch editing.

## Message Formatting

- **User**: `>` prefix; optional background tint for readability.
- **Agent**: No header by default; messages render inline with normal styling.
- **Thinking**: Dimmed text.
- **System notices**: Dimmed and bracketed (e.g., `[Model: claude-haiku-4.5]`).

## Migration Steps

1. **Create inline terminal wrapper**
   - New struct or modify App to use Viewport::Inline
   - Handle raw mode setup/teardown

2. **Implement message push**
   - Add method to push completed messages via insert_before
   - Handle multi-line messages properly

3. **Simplify draw()**
   - Render ionizing line + input + status in viewport
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
5. **Selector scope**: Confirm help uses the same bottom-anchored selector UI
6. **Alternate mode**: Confirm removal vs legacy fallback flag

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
