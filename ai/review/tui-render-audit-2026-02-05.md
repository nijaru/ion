# TUI Rendering System Audit (2026-02-05)

## Scope

Deep architectural audit of the TUI rendering pipeline: state machine, main loop efficiency, rendering correctness, stdout safety, error handling, and dead code.

Files reviewed: `render_state.rs`, `render/direct.rs`, `render/chat.rs`, `render/layout.rs`, `render/mod.rs`, `render/widgets.rs`, `render_selector.rs`, `run.rs`, `mod.rs`, `terminal.rs`, `events.rs`, `session/update.rs`, `session/lifecycle.rs`, `input.rs`, `composer/state.rs`

---

## Critical (must fix)

### 1. Triple allocation + triple computation every frame in render_input_direct (95% confidence)

**File:** `src/tui/render/direct.rs:384-447`

Every frame (20fps), `render_input_direct` does:

1. `self.input_buffer.get_content()` (line 393) -- allocates String from Rope
2. `self.input_state.calculate_cursor_pos()` (line 398) -- calls `get_content()` again internally (`composer/state.rs:534`) and `build_visual_lines` (`composer/state.rs:545`)
3. `build_visual_lines(&content, content_width)` (line 403) -- third computation of the same line map
4. `self.input_state.visual_line_count()` (line 404) -- calls `get_content()` AGAIN (`composer/state.rs:584`) and `build_visual_lines` AGAIN (`composer/state.rs:585`)

**Impact:** 3 String allocations from Rope, 3 `build_visual_lines` computations per frame, even when input hasn't changed. For large inputs (multi-line pastes), this is measurable.

**Fix:** Cache the visual line map in `ComposerState` (invalidated when content or width changes). `calculate_cursor_pos` already tracks `last_width` but doesn't cache the line map. Single `get_content()` call passed to all three consumers.

### 2. draw_direct renders full UI every frame regardless of changes (90% confidence)

**File:** `src/tui/run.rs:398`, `src/tui/render/direct.rs:19-124`

The main loop calls `app.draw_direct(&mut stdout, ...)` unconditionally on EVERY iteration (every 50ms). This performs:

- Layout calculations (`calculate_ui_height`, `ui_start_row`)
- Terminal escape sequences: `MoveTo`, `Clear(ClearType::FromCursorDown)` or `Clear(ClearType::All)`
- Border drawing: `"---".repeat(width)` allocation x2 (`draw_horizontal_border` at lines 86, 94)
- Full input re-render (see issue #1)
- Status line re-render
- Cursor positioning

When idle (no input, no events, no streaming), none of this output changes between frames, but every terminal escape sequence is still emitted and flushed. This is ~20 syscalls/frame for no visual change.

**Fix:** Track a "dirty" flag. Only call `draw_direct` when input changed, mode changed, frame_count changed (spinner), terminal resized, or chat content changed. Most frames when idle can skip rendering entirely.

---

## Important (should fix)

### 3. /clear does not clear old chat content from viewport (85% confidence)

**File:** `src/tui/events.rs:387-412`, `src/tui/render_state.rs:102-109`

The `/clear` command resets the data model (`message_list.clear()`, `render_state.reset_for_new_conversation()`) but never issues any terminal clear command. Old messages remain visible in the viewport above the UI. The header is re-printed at the cursor's current position (inside the old UI area), creating a visually incoherent screen with old messages above and a fresh header in the middle.

**Scenario:**

1. User has a conversation (messages fill viewport)
2. User types `/clear`
3. Header re-appears mid-screen, old messages remain above
4. UI re-renders at the bottom -- visually confusing

**Fix:** After `reset_for_new_conversation()`, emit `Clear(ClearType::All)` or `Clear(ClearType::Purge)` to clear the viewport. This needs to happen in the main loop since events.rs doesn't have the stdout handle. Add a `needs_full_clear` flag to `RenderState` (similar to `needs_selector_clear`).

### 4. Resize invalidates startup_ui_anchor causing header to be orphaned (85% confidence)

**File:** `src/tui/events.rs:45`

On resize, `startup_ui_anchor` is set to `None`. If the user is at the startup screen (no messages, header shown), the anchor is lost but `header_inserted` remains `true`. The header text stays at its old position in the viewport, but the UI jumps to the bottom of screen (scroll mode). The header floats mid-screen disconnected from the UI below.

**Scenario:**

1. Launch ion, header appears with UI below it
2. Resize terminal (e.g., make taller)
3. Header stays at old row, UI jumps to bottom -- gap between them

**Fix:** On resize when `header_inserted && entries.is_empty()`, either re-emit the header at the new position or set `header_inserted = false` to let the next frame re-print it.

### 5. Selector render_selector clears from start_row down, overlapping chat content (80% confidence)

**File:** `src/tui/render_selector.rs:44-45`

```rust
execute!(w, MoveTo(0, start_row), Clear(ClearType::FromCursorDown))?;
```

This clears everything from `start_row` to the bottom of screen. But `render_selector_direct` is called from within `draw_direct` AFTER borders and input have already been drawn (lines 86-98 in direct.rs). The selector then overwrites the already-drawn input area. The input drawing (borders, content, status at lines 82-98) is wasted work when in selector mode.

Additionally, `draw_direct` draws borders and input content unconditionally (lines 82-98), then conditionally renders the selector on top at line 101. This means in selector mode, the input borders and content are drawn and immediately overwritten by the selector clear.

**Fix:** Skip input/border/status rendering when `self.mode == Mode::Selector`. Move the mode check earlier in `draw_direct`.

### 6. String allocation in draw_horizontal_border every frame (80% confidence)

**File:** `src/tui/render/widgets.rs:19`

```rust
Print("---".repeat(width as usize))
```

This allocates a new String on every call. Called twice per frame (top and bottom border). For width=200, that's 600 bytes x 2 = 1.2KB allocated and freed 20 times/second.

**Fix:** Use a pre-allocated border string cached at the terminal width, or write the border character-by-character.

---

## Low Priority / Notes

### 7. resume_session and list_recent_sessions appear unused (80% confidence)

**File:** `src/tui/session/lifecycle.rs:14-28`

`resume_session()` (line 14) and `list_recent_sessions()` (line 26) are defined but have no callers. `load_session()` (line 31) is used instead. These may be remnants from an earlier API.

**Status:** Compiler didn't warn (they're `pub`), but they have no call sites in the codebase.

### 8. Error handling in draw_direct: partial writes leave terminal inconsistent (70% confidence)

**File:** `src/tui/render/direct.rs:19-124`

If any `execute!` or `write!` call fails mid-render (e.g., `render_progress_direct` succeeds but `render_input_direct` fails), the synchronized update is still ended in `run.rs:401` via `EndSynchronizedUpdate`, and state like `last_ui_start` has already been updated. The terminal shows a partial render.

The `?` operator propagates errors up through the main loop, which then runs `cleanup_terminal`. The `cleanup_terminal` calls `EndSynchronizedUpdate` (line 118) as a safety net. This is reasonable -- the partial write is visible for at most one frame before cleanup runs.

**Assessment:** Not a practical problem since write errors to stdout are extremely rare (pipe closed = terminal closed). The safety net in cleanup is correct.

### 9. FocusGained while selector is open sets chat_row to None (70% confidence)

**File:** `src/tui/events.rs:48-50`

```rust
Event::FocusGained => {
    self.render_state.chat_row = None;
}
```

This unconditionally drops row tracking. If the user is in selector mode (which covers the full bottom area), this is harmless -- the selector re-renders on top anyway. When the selector closes, `needs_selector_clear` fires. This is probably fine but could cause a momentary visual glitch if the terminal gained focus during selector mode and content had been in row-tracking mode.

### 10. cursor::position() can fail silently on startup (70% confidence)

**File:** `src/tui/run.rs:305`

```rust
if let Ok((_x, y)) = crossterm::cursor::position() {
```

If `cursor::position()` fails, `startup_ui_anchor` and `chat_row` remain `None`. The UI falls through to scroll mode (bottom of screen). The header was already printed but has no anchor. This is a graceful degradation, not a crash, but the header would be disconnected from the UI.

---

## Architecture Summary

The rendering system uses a hybrid positioning model (row-tracking vs scroll mode) that is clever but creates a large surface area for state inconsistencies. Key observations:

1. **State coupling**: `chat_row`, `startup_ui_anchor`, `last_ui_start`, `header_inserted`, and `rendered_entries` are interdependent but updated in different code paths (events, main loop, draw_direct). The reset methods (`reset_for_new_conversation`, `reset_for_session_load`, `mark_reflow_complete`) help but don't cover all transitions (e.g., resize + startup anchor).

2. **Every-frame rendering**: The main loop does full rendering on every frame unconditionally. This is the single biggest efficiency concern. A dirty-flag system would eliminate ~90% of rendering work during idle periods.

3. **stdout discipline**: Good. The old `println!` issues have been fixed. All remaining `io::stdout()` handles are in appropriate contexts (panic hook, early bail-out). The `println!()` in cleanup is after `disable_raw_mode()`.

4. **Synchronized updates**: Correctly bracketed with `BeginSynchronizedUpdate`/`EndSynchronizedUpdate` in the main loop, with a safety-net `EndSynchronizedUpdate` in cleanup.
