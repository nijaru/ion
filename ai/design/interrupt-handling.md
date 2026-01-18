# Design: Ctrl+C Interrupt Handling

**Task:** tk-3jba
**Status:** Open (Bug)
**Priority:** High

## Problem

When a tool (especially bash) is executing, Ctrl+C triggers the double-tap quit flow instead of interrupting the running tool. Users expect Ctrl+C to cancel the current operation, not quit the app.

## Current Behavior

```
User presses Ctrl+C during tool execution
  → TUI checks cancel_pending timestamp
  → If within CANCEL_WINDOW, cancels abort_token
  → If not, sets cancel_pending = Some(Instant::now())
  → Shows yellow border as "pending cancel" indicator
```

The abort_token.cancel() does propagate to bash via `kill_on_drop`, but:

1. First Ctrl+C just sets pending state
2. Second Ctrl+C actually cancels
3. UX is confusing - user doesn't know first press did anything useful

## Desired Behavior

```
User presses Ctrl+C during agent execution
  → If tool is running: immediately cancel the tool
  → If LLM is streaming: immediately stop stream
  → If input is not empty: clear input
  → If input is empty and not running: quit (or double-tap to quit)
```

## Implementation

### 1. Track Tool Execution State

```rust
// src/tui/mod.rs
pub struct App {
    // ...existing...
    /// Currently executing tool name (if any)
    pub current_tool: Option<String>,
}
```

Update via AgentEvent:

```rust
AgentEvent::ToolCallStart(id, name) => {
    self.current_tool = Some(name.clone());
    // ...existing...
}
AgentEvent::ToolCallResult(..) => {
    self.current_tool = None;
    // ...existing...
}
```

### 2. Simplify Ctrl+C Logic

```rust
KeyCode::Char('c') if ctrl => {
    if self.is_running {
        // Immediately cancel - no double-tap needed during execution
        self.session.abort_token.cancel();
        self.cancel_pending = None;

        // Show feedback
        if let Some(tool) = &self.current_tool {
            // Could add a system message: "Cancelled {tool}"
        }
    } else if !self.input.is_empty() {
        // Clear input
        self.input.clear();
        self.cursor_pos = 0;
    } else {
        // Empty input, not running - quit or double-tap quit
        if let Some(when) = self.cancel_pending {
            if when.elapsed() <= CANCEL_WINDOW {
                self.quit();
            } else {
                self.cancel_pending = Some(Instant::now());
            }
        } else {
            self.cancel_pending = Some(Instant::now());
        }
    }
}
```

### 3. Ensure Cancellation Propagates

The bash tool already has `kill_on_drop(true)`:

```rust
let child = Command::new("bash")
    // ...
    .kill_on_drop(true)  // Already present
    .spawn()?;
```

And checks abort signal:

```rust
tokio::select! {
    res = child.wait_with_output() => { ... }
    _ = ctx.abort_signal.cancelled() => {
        return Err(ToolError::Cancelled);
    }
}
```

This should work, but verify:

1. `kill_on_drop` sends SIGKILL when the `Child` is dropped
2. The `select!` drops the child when abort is triggered
3. Confirm child processes are actually killed (test with `sleep 100`)

### 4. Visual Feedback

Show cancellation in progress line:

```rust
if self.cancel_pending.is_some() && self.is_running {
    // Show "Cancelling..." instead of "Ionizing..."
    progress_spans.push(Span::styled(
        "Cancelling...",
        Style::default().fg(Color::Red),
    ));
}
```

## Testing

1. Run `ion`, ask it to execute `sleep 100`
2. Press Ctrl+C during execution
3. Verify: sleep process is killed, agent stops, TUI remains usable
4. Press Ctrl+C twice quickly when idle to quit

## Edge Cases

1. **Nested processes**: `bash -c "bash -c 'sleep 100'"` - kill_on_drop should handle via process group
2. **Rapid Ctrl+C**: Debounce or handle gracefully
3. **Tool vs LLM**: Both should cancel on first Ctrl+C when running

## Migration

This is a behavior change:

- Before: Double-tap Ctrl+C always
- After: Single Ctrl+C during execution, double-tap when idle

Consider: Add config option for "aggressive cancel" vs "confirm cancel"?
