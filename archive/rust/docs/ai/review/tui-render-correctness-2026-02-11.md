# TUI Render Pipeline Correctness Review

**Date:** 2026-02-11
**Scope:** Rendering correctness -- logic errors, off-by-one, boundary conditions in position state machine, clear behavior, scroll amounts, cursor positioning
**Files:** run.rs, render_state.rs, render/direct.rs, render/layout.rs, render/progress.rs, chat_renderer.rs
**Method:** Line-by-line trace of frame pipeline with concrete numeric examples, terminal auto-scroll modeling

## Critical (must fix)

### [ERROR] render_state.rs:294-296 - position_after_reprint scrolls one line too many when line_count >= term_height

`position_after_reprint` computes excess scroll as `min(line_count, term_height) - available`. This overcounts by 1 when `line_count >= term_height` because `write_lines` (which prints via `writeln` with trailing `\r\n`) triggers one extra terminal auto-scroll when the last line fills the bottom row.

**Trace:**

- Terminal H=40, ui_height=6, available=34
- Write 40 lines from row 0. Line 40 at row 39, `\r\n` triggers scroll. Visible: lines 2-40 at rows 0-38, cursor at row 39. 39 visible content lines.
- `position_after_reprint` returns excess = min(40,40) - 34 = **6**
- Actual correct excess = 39 - 34 = **5**
- `ScrollUp(6)` pushes one extra line to scrollback, leaving a blank row between chat and UI

**Impact:** After a Reflow (terminal resize), one line of chat content is unnecessarily pushed off-screen. Visible as a blank gap between the last chat line and the bottom UI.

**Confidence: 90%**

The sibling function `reprint_loaded_session` (run.rs:484) avoids this by using actual `cursor::position()` instead of arithmetic, confirming the auto-scroll behavior is real.

```
-> Fix in render_state.rs:294-296:
   // write_lines triggers one extra auto-scroll when content fills the screen
   let visible = if line_count >= term_height as usize {
       (term_height as usize).saturating_sub(1)
   } else {
       line_count
   };
   let excess = (visible.saturating_sub(available)) as u16;

-> Update tests:
   position_after_reprint_overflows: assert_eq!(excess, 5) not 6
   position_after_reprint_content_exceeds_terminal: assert_eq!(excess, 5) not 6
```

---

## Important (should fix)

### [WARN] run.rs:487-494 - reprint_loaded_session enters Scrolling on exact boundary without scrolling

When `cursor_y == available` (chat fills exactly to the UI boundary), `excess = 0` but position is set to `Scrolling`. Subsequent frames use `ScrollInsert` path for new chat, which works but is suboptimal -- the `Tracking` path (which anchors UI below chat) would produce the same visual with less overhead.

**Impact:** Not a visual bug but a premature state transition. Next message arriving when exactly 1 line would fit in Tracking mode instead takes the ScrollInsert path (clear UI, scroll, reprint). Functionally correct but less efficient.

**Confidence: 85%**

```
-> Fix: Use strict > instead of >=:
   if cursor_y > available {
```

---

## Architecture Observations (not issues)

1. **`write_lines` vs `write_lines_at` auto-scroll semantics differ.** `write_lines` prints sequentially and relies on terminal auto-scroll. `write_lines_at` uses explicit `MoveTo` per line, avoiding auto-scroll. The reflow path uses `write_lines` (auto-scroll prone), while incremental chat insertion uses `write_lines_at` (safe). This difference is the root cause of the position_after_reprint off-by-one.

2. **State machine transitions are well-guarded.** All 7 places that assign `render_state.position` are in response to concrete rendering actions (header print, chat insert, reflow, session reprint, screen clear). No orphan transitions.

3. **clear_from computation is conservative and correct.** `min(last_top, current_top)` ensures stale rows between old and new UI positions are cleared. The `.min(height - 1)` clamp prevents clearing below the terminal.

4. **Header-to-Tracking transition is safe.** `ClearHeaderArea` outside sync block + `AtRow` inside sync block correctly clears header, prints chat, and transitions state in one frame.

5. **Scrolling mode never transitions back to Tracking** (except through reflow/reprint). This is correct -- once chat overflows, only a full reprint can determine if it still overflows at the new width.

---

## Verified Correct

| Area                                            | Verification                                                                                                                   |
| ----------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------ |
| plan_chat_insert AtRow math                     | Traced: next_row + line_count + ui_height <= term_height. Confirmed.                                                           |
| plan_chat_insert Overflow math                  | Traced with 3 examples. scroll_amount and print_row produce correct layout.                                                    |
| plan_chat_insert ScrollInsert math              | Traced. ui_start - line_count correctly positions new content above UI.                                                        |
| draw_direct Clear(FromCursorDown) at layout.top | Verified: layout.top >= chat next_row in Tracking; layout.top == term_height - ui_height in Scrolling. No chat content erased. |
| Clear(ClearType::All) usage                     | Only when position is Empty (no chat). Safe -- does not erase terminal scrollback.                                             |
| render_frame stale row clearing                 | clear_from..top range correctly covers popup dismiss, selector exit.                                                           |
| Selector buffering during chat insert           | take_chat_inserts buffers lines when Selector active, drains on mode exit.                                                     |
| streaming_lines_rendered skip                   | Prevents duplicate rendering when tool call interrupts streaming entry.                                                        |
| Chat position state transitions                 | 7 assignment sites reviewed -- no orphan transitions, no inconsistent states.                                                  |
| plan_chat_insert Overflow scroll_amount         | Fully traced with scroll semantics. Content, new lines, and UI correctly partitioned post-scroll.                              |
