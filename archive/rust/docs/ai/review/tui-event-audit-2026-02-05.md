# TUI Event Handling & App State Audit

**Date:** 2026-02-05
**Files reviewed:** events.rs, mod.rs, input.rs, run.rs, types.rs, chat_renderer.rs, render/chat.rs, render/direct.rs, render/layout.rs, session/update.rs, session/tasks.rs, session/providers.rs, session/lifecycle.rs, composer/state.rs, composer/buffer.rs, highlight/markdown.rs, render_state.rs, app_state.rs, message_list.rs, util.rs

## Summary

The TUI event handling is well-structured overall. Mode transitions are clean, event dispatch is complete, and the code is idiomatic Rust. No critical bugs found. Several important issues and a few lower-confidence items below.

---

## Critical (must fix)

None found.

---

## Important (should fix)

### 1. Ctrl+C does nothing while agent is running (events.rs:230-244)

**Confidence: 95%**

When `is_running` is true AND the input is non-empty, Ctrl+C clears input -- that is correct. But when `is_running` is true AND input is empty, nothing happens because the double-tap quit check is gated behind `!self.is_running`:

```rust
// events.rs:234
} else if !self.is_running {
    // Only quit when idle (double-tap)
```

This means Ctrl+C with empty input during an active agent task is a silent no-op. This is likely intentional (Esc cancels the agent, not Ctrl+C per the comment on line 229), but it is surprising behavior -- most CLI tools treat Ctrl+C as the primary interrupt. At minimum, the user gets no feedback that their Ctrl+C was ignored. Consider either:

- Showing a hint like "Press Esc to cancel" when Ctrl+C is pressed while running
- Or canceling the agent on Ctrl+C too (most users expect this)

### 2. Ctrl+P has overloaded behavior based on `is_running` (events.rs:293-298)

**Confidence: 90%**

```rust
KeyCode::Char('p') if ctrl => {
    if !self.is_running {
        self.open_provider_selector();
    } else {
        self.prev_history();
    }
}
```

When `is_running`, Ctrl+P acts as prev_history instead of opening the provider selector. This is undocumented and surprising -- the user has no way to know that the same keybinding does completely different things depending on whether the agent is active. Ctrl+P for previous history is unusual (that's what Up arrow does). This looks like leftover code from before the arrow key history was implemented.

### 3. `run.rs` duplicates event dispatch unnecessarily (run.rs:256-272)

**Confidence: 85%**

The main loop in run.rs manually matches on event types and re-wraps them before passing to `handle_event()`, but `handle_event()` already does the same match:

```rust
// run.rs:256-272
match evt {
    event::Event::Key(key) => {
        app.handle_event(event::Event::Key(key));
    }
    event::Event::Paste(text) => {
        app.handle_event(event::Event::Paste(text));
    }
    // ... etc
}
```

This could simply be `app.handle_event(evt)` -- the current code just reconstructs the same events. The only purpose is to capture `Resize` dimensions into `term_width`/`term_height` before dispatching, but that could be done with an `if let` before the dispatch. Not a bug, but adds unnecessary complexity.

### 4. `Box<dyn std::error::Error>` used instead of `anyhow` in run.rs (run.rs:39,64,158,216)

**Confidence: 95%**

Per project conventions (AGENTS.md: "Errors: anyhow (apps)"), `run.rs` should use `anyhow::Result` instead of `Result<(), Box<dyn std::error::Error>>`. The file uses `Box<dyn std::error::Error>` in four function signatures:

- `setup_terminal()` (line 39)
- `handle_resume()` (line 64)
- `open_editor()` (line 158)
- `run()` (line 216)
- `cleanup_terminal()` (line 116)

### 5. Thinking content is cached in markdown but never rendered (message_list.rs:256-276)

**Confidence: 85%**

`MessageEntry::update_cache()` formats thinking blocks into the markdown cache with `> *Reasoning*` prefix and blockquote format. But `chat_renderer.rs:74-77` explicitly skips `MessagePart::Thinking`:

```rust
MessagePart::Thinking(_) => {
    // Don't render thinking content in chat
}
```

Meanwhile `content_as_markdown()` returns the cached string that INCLUDES thinking. So:

- Agent text rendering via `highlight_markdown_with_width` uses `wrap_width` and gets the right text (no thinking)
- But `content_as_markdown()` is used elsewhere (e.g. `push_entry` scroll offset calculation at line 448) and DOES include thinking, inflating the scroll offset estimate

This is a minor inconsistency but could cause scroll position jitter when thinking is present.

---

## Moderate Issues

### 6. User message wrap_width is inconsistent with Agent (chat_renderer.rs:31-41)

**Confidence: 85%**

For User messages, the first line uses `available_width = wrap_width - 2` (for "> " prefix), but continuation lines use `wrap_width` (the full width). For Agent messages, `highlight_markdown_with_width` receives the raw `wrap_width`. Both then go through the post-pass `wrap_styled_line` at line 231-236.

This means User continuation lines can be slightly wider than Agent lines. Not visually critical since the post-pass wraps everything, but the intermediate `wrap_line()` call on continuation lines at a wider width could produce slightly different break points than if they were wrapped at the narrower width.

### 7. `trim_leading_blank_lines` uses `remove(0)` in a loop (chat_renderer.rs:372-376)

**Confidence: 90%**

```rust
fn trim_leading_blank_lines(lines: &mut Vec<StyledLine>) {
    while lines.first().is_some_and(line_is_blank) {
        lines.remove(0);
    }
}
```

`Vec::remove(0)` is O(n) per call, making this O(n\*k) where k is the number of leading blanks. For typical chat output k is 0-2 so it is not a real performance issue, but it is worth noting as un-idiomatic. `drain(..k)` would be cleaner.

### 8. History search popup may render above terminal top (render/direct.rs:523)

**Confidence: 80%**

```rust
let popup_start = input_start.saturating_sub(popup_height);
```

When `input_start` is small (e.g., terminal is very short or in row-tracking mode with UI near top), `popup_start` could be row 0 or very close. The match rendering loop at line 559 starts from `popup_start` which could overwrite chat content or the header. No bounds check against the visible chat area.

---

## Low Confidence / Uncertain

### 9. `needs_setup` + Esc can cause selector re-open loop (update.rs:17-23)

**Confidence: 70%**

```rust
if self.needs_setup && self.mode == Mode::Input {
    if self.config.provider.is_none() {
        self.open_provider_selector();
    } else {
        self.open_model_selector();
    }
}
```

Every frame, if `needs_setup` is true and mode is `Input`, the selector re-opens. The only way out is to complete setup (selecting a model sets `needs_setup = false` at events.rs:616) or quit. Pressing Esc in the selector during setup sends you back to Input mode (events.rs:649-652) which re-triggers the selector. This is probably intentional (force first-time setup), but makes it impossible to use the tool without selecting a provider/model. If a user's API key is missing, they may be stuck in a loop.

### 10. `Error` event skips `push_event` for cancelled tasks (update.rs:63-68)

**Confidence: 75%**

When `was_cancelled` is true, the error event is NOT pushed to `message_list`. This means the user sees no feedback in chat that cancellation occurred. The progress bar shows "Canceled" (render/direct.rs:358) but only until the next task replaces `last_task_summary`. This is probably fine UX, but could confuse users who cancel and then wonder if it worked.

### 11. `FocusLost` event is silently ignored

**Confidence: 70%**

`EnableFocusChange` is called in `setup_terminal()`, meaning the terminal sends `FocusGained`/`FocusLost` events. `FocusGained` is handled (events.rs:47-50, run.rs:268-269) to trigger redraw, but `FocusLost` falls through to `_ => {}` in both `handle_event()` and the main loop. This is probably fine -- no action needed on focus loss.

---

## Idiomatic Rust

### Already good

- Consistent use of `&str` parameters where ownership isn't needed
- `crate::` imports throughout (no `super::`)
- No unnecessary `pub use` re-exports (except mod.rs which is intentional API surface)
- Error handling is consistent within modules
- Pattern matching is exhaustive with sensible defaults

### Minor opportunities

- **run.rs**: `Box<dyn Error>` -> `anyhow::Result` (covered above)
- **chat_renderer.rs:372-376**: `remove(0)` loop -> `drain`
- **events.rs:101**: `format!("{cmd} ")` creates a temporary String to pass to `insert_str` which takes `&str` -- fine, no way around it
- **message_list.rs:655-668**: Test uses verbose match-based vector construction instead of `(0..10).map(|i| format!("line{i}")).collect()` -- tests only, not worth changing
