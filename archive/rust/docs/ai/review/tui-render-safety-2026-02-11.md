# TUI Render Pipeline Safety Review

**Date:** 2026-02-11
**Scope:** Error handling, resource management, defensive coding in TUI rendering
**Files:** run.rs, render_state.rs, render/direct.rs, render/layout.rs, render/progress.rs, chat_renderer.rs

## Critical (must fix)

### [ERROR] run.rs:439-457 - BeginSynchronizedUpdate leaked on error paths in render_frame

If any `?` between `BeginSynchronizedUpdate` (line 439) and `EndSynchronizedUpdate` (line 457) returns an error, the terminal remains in synchronized update mode. The error propagates up via `?` on line 810 in the main loop, hitting `cleanup_terminal` -- which does have a safety-net `EndSynchronizedUpdate` on line 614. However, if the `?` on line 810 propagates past `cleanup_terminal` entirely (the error goes straight to the caller), the terminal stays in sync mode.

Same issue in `reprint_loaded_session` (lines 480-504): errors from `write_lines` (481), `cursor::position` (484), or `ScrollUp` (492) exit without calling `EndSynchronizedUpdate`.

```
-> Fix: Use a scope guard pattern (or explicit cleanup) so EndSynchronizedUpdate
   is always written, regardless of which operation fails within the sync block.
   Simplest fix: wrap the sync block body in a closure and always end sync:

   execute!(stdout, BeginSynchronizedUpdate)?;
   let result = (|| -> io::Result<()> {
       apply_pre_ops(stdout, app, &pre_ops, layout, term_height)?;
       // ... rest of sync block ...
       Ok(())
   })();
   let _ = execute!(stdout, EndSynchronizedUpdate);
   result?;
   stdout.flush()?;
```

**Confidence: 95%** -- The cleanup_terminal safety net partially mitigates this for the normal quit path, but errors during render can bypass cleanup_terminal.

### [ERROR] run.rs:720-723 - Panic hook does not end synchronized update mode

The panic hook only calls `disable_raw_mode()` and `Show`. If a panic occurs while inside a `BeginSynchronizedUpdate` block (e.g., during `apply_pre_ops` or `draw_direct`), the terminal remains in synchronized update mode. This means all subsequent output (including the panic message itself) may be buffered invisibly by the terminal, making debugging impossible.

```
-> Fix: Add EndSynchronizedUpdate to the panic hook:

   std::panic::set_hook(Box::new(move |info| {
       let _ = execute!(io::stdout(), EndSynchronizedUpdate);
       let _ = disable_raw_mode();
       let _ = execute!(io::stdout(), DisableBracketedPaste, DisableFocusChange);
       let _ = execute!(io::stdout(), Show);
       (hook_for_panic)(info);
   }));
```

**Confidence: 95%** -- A panic during the synchronized block is rare but not impossible (e.g., index out of bounds in chat rendering code), and the consequence is severe: an invisible terminal.

## Important (should fix)

### [WARN] run.rs:369 - lines.len() as u16 truncation silently corrupts layout arithmetic

`plan_chat_insert` casts `lines.len() as u16` which silently truncates if there are more than 65535 lines. This is unlikely for a single chat insert but not impossible (e.g., a tool output dumps thousands of lines). The truncated `line_count` would cause incorrect scroll calculations, potentially scrolling the wrong amount and leaving garbage on screen.

```
-> Fix: Cap the vec at u16::MAX before entering plan_chat_insert,
   or use u16::try_from(lines.len()).unwrap_or(u16::MAX).
```

**Confidence: 85%** -- The streaming incremental rendering (holding back 2 lines) limits batch sizes in practice, but a single completed tool entry can be arbitrarily large.

### [WARN] render_state.rs:289 - line_count as u16 truncation in position_after_reprint

`position_after_reprint` accepts `line_count: usize` and casts it to u16 on line 289 (`next_row: line_count as u16`). If `line_count > 65535`, this silently wraps, setting `next_row` to a garbage value. This could cause the UI to render in the wrong position.

```
-> Fix: Use line_count.min(u16::MAX as usize) as u16 before assignment.
```

**Confidence: 85%** -- Same scenario as above: reprint of very large chat history.

### [WARN] run.rs:484 - cursor::position() failure propagated with ? inside sync block

In `reprint_loaded_session`, `crossterm::cursor::position()?` on line 484 can fail (e.g., when stdin is not a tty, or the terminal doesn't respond to the DSR query). This error propagates via `?`, leaving `BeginSynchronizedUpdate` active (covered by the first ERROR above). But beyond the sync leak, the entire session reprint silently fails, leaving the render state in an inconsistent position (position was not set, but lines were written).

```
-> Fix: Use a fallback position instead of propagating the error.
   let cursor_y = crossterm::cursor::position()
       .map(|(_, y)| y)
       .unwrap_or(lines.len().min(u16::MAX as usize) as u16);
```

**Confidence: 90%** -- cursor::position() is known to fail on non-interactive terminals and inside certain multiplexers.

### [WARN] run.rs:182 - cursor::position failure silently skips header position tracking

In `apply_pre_ops`, `PreOp::PrintHeader` uses `if let Ok((_x, y)) = crossterm::cursor::position()` which silently ignores failure. If cursor::position() fails, `app.render_state.position` is never updated from its prior state. This means the header was printed but the position state machine doesn't know about it. Subsequent chat insertions could overlap the header or appear at wrong positions.

```
-> Fix: At minimum, set a fallback position based on the known header line count.
   More robust: track the cursor position from the number of lines written.
```

**Confidence: 85%** -- The header is only a few lines at startup, so the impact is limited, but the state machine inconsistency is real.

### [WARN] layout.rs:31 - UiLayout::height() uses non-saturating subtraction

`status.row + status.height - self.top` uses plain subtraction. While the layout construction guarantees `status.row >= self.top` (since status is built by accumulating from top), a bug in layout construction could cause underflow panic in debug mode or wrap in release mode.

```
-> Fix: Use saturating arithmetic:
   (status.row + status.height).saturating_sub(self.top)
```

**Confidence: 80%** -- The invariant is maintained by construction today, but defensive coding would prevent a panic if layout logic changes.

### [WARN] layout.rs:87 - total height computation can overflow u16

`popup_height + progress_height + input_height + status_height` uses plain u16 addition. If `popup_height` is very large (many completion candidates) and `input_height` is also large, this could overflow u16 on a large terminal. The overflow would cause `ui_start_row` to receive a tiny `total`, placing the UI at the wrong position.

```
-> Fix: Use saturating_add chain or cap popup_height to a reasonable maximum.
```

**Confidence: 80%** -- `active_popup_height` already limits candidates to what's visible, and completion lists are typically small. But the `len() as u16` cast on line 137/139 of layout.rs has no cap.

## Uncertain (verify)

### [NIT] run.rs:720-723 - Panic hook missing DisableBracketedPaste and PopKeyboardEnhancementFlags

Minor terminal state leaks on panic. Bracketed paste mode and keyboard enhancement flags are not restored. Most terminals recover from these on process exit, but some (especially multiplexers) may not.

**Confidence: 70%** -- Terminal behavior varies; most modern terminals reset on the controlling process exit.

### [NIT] chat_renderer.rs:68 - content_width could be 0 when wrap_width < 2

`wrap_width.saturating_sub(2)` can produce 0 when wrap_width is 0 or 1. This value is passed to `highlight_markdown_with_width`, which may or may not handle width=0 gracefully. (The outer `build_lines` has a `wrap_width == 0` early return at line 232, but this is _after_ the main rendering loop.)

**Confidence: 70%** -- Would need to verify what highlight_markdown_with_width does with width=0.

## Summary

| Severity | Count | Key theme                                                               |
| -------- | ----- | ----------------------------------------------------------------------- |
| ERROR    | 2     | Synchronized update mode leaked on error/panic                          |
| WARN     | 5     | u16 truncation, cursor::position fallibility, non-saturating arithmetic |
| NIT      | 2     | Incomplete panic hook, edge case width handling                         |

The most impactful issues are the synchronized update leaks. When `EndSynchronizedUpdate` is not called, the terminal silently buffers all output, making the application appear frozen. The panic hook fix is a one-liner. The render_frame fix requires restructuring the sync block to guarantee cleanup.
