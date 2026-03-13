# TUI Rendering Pipeline Quality Review

**Date:** 2026-02-11
**Scope:** run.rs, render_state.rs, render/direct.rs, render/layout.rs, render/progress.rs, chat_renderer.rs
**Method:** Static analysis, build verification, test execution (28/28 pass), clippy (2 warnings)

## Summary

The render pipeline is well-structured around a prepare-plan-render frame model with explicit state machine transitions. The main risks are accumulated complexity from incremental patches, dead code from refactoring, and missing test coverage in the chat renderer. No correctness bugs found; all issues are maintainability/performance.

---

## Findings

### ERROR (must fix)

None.

### WARN (should fix)

**W1. Dead code: `reprint_chat_scrollback` (chat.rs:121-143)**
Defined as `pub` but never called. `reprint_loaded_session` in `run.rs` and the `Reflow` PreOp replaced it. The doc comment on `mark_reflow_complete` (render_state.rs:302) still references it.
-> Delete `reprint_chat_scrollback`. Update the doc comment on `mark_reflow_complete`.

**W2. Dead method: `ChatPosition::is_tracking` (render_state.rs:127-129)**
Defined but never called anywhere in the codebase (confirmed by grep).
-> Delete `is_tracking()`.

**W3. `Overflow` and `ScrollInsert` have identical execution (run.rs:211-240)**
Both variants in `apply_chat_insert` execute the same 3 operations: clear-from-row, scroll-up, write-lines-at-row, set Scrolling. The only difference is semantics around where the clear row comes from, but the field names already encode that.
-> Merge into a single `ScrollInsert { clear_row, scroll_amount, print_row, lines }` variant. Keep `plan_chat_insert` producing it from different inputs. This removes 10 lines and a match arm.

**W4. `wrap_styled_line` flattens to per-char Vec -- O(n) allocation per long line (chat_renderer.rs:522-526)**
Every character gets a `(char, ContentStyle)` tuple collected into a Vec. For a 1000-char line, that is 1000 \* 24 bytes = 24KB. This runs on every line exceeding wrap width during full reflow, meaning a large session reflow hits this for every wrapped line.
-> Consider operating on span boundaries instead: track span index + char offset rather than materializing all chars. Alternatively, accept this as reasonable for now but document the trade-off.

**W5. Unnecessary String allocation in `continuation_indent_width` (chat_renderer.rs:498)**
`trimmed.chars().skip(digits).collect::<String>()` allocates a String just to call `.starts_with(". ")`. Since `digits` counts only ASCII digits, byte indexing is safe.
-> Replace with `trimmed[digits..].starts_with(". ")`.

**W6. No tests for chat_renderer.rs (651 lines, 0 tests)**
`wrap_line`, `wrap_styled_line`, `split_words`, `continuation_indent_width`, `parse_ansi_line`, `collapse_blank_runs`, and `chars_to_styled_line` are all untested. These are pure functions that are trivial to test and have caused regressions (per STATUS.md history).
-> Add unit tests for at minimum: `wrap_line`, `wrap_styled_line`, `continuation_indent_width`, `parse_ansi_line`. These are pure functions with clear inputs/outputs.

**W7. `compute_layout` called 3 times per frame (run.rs:262, 789, 454)**
Call #1 (in `prepare_frame`) and #2 (in main loop) may return different results since `prepare_frame` can mutate position state via Reflow. Call #3 (post-insert) is necessary. However, when no reflow occurs (the common case), calls #1 and #2 produce identical results.
-> Return the layout from `prepare_frame` when reflow did not fire, avoiding the redundant #2 call.

**W8. `ChatRenderer::build_lines` is 240 lines with #[allow(clippy::too_many_lines)] (chat_renderer.rs:11-255)**
Each `Sender` variant has distinct rendering logic. Extracting per-sender methods (e.g., `render_user_entry`, `render_tool_entry`, `render_agent_entry`, `render_system_entry`) would improve readability and make each branch independently testable.
-> Split into 4 private methods, one per Sender variant.

### NIT (optional)

**N1. Clippy warnings: `map_or` -> `is_some_and` (chat_renderer.rs:538, 591)**
Clippy reports these. Trivial fix.
-> Apply `cargo clippy --fix`.

**N2. `collapse_blank_runs` clones every line (chat_renderer.rs:456)**
Builds a new Vec by cloning each StyledLine. Could drain + filter in-place.
-> Use `lines.dedup_by(|a, b| line_is_blank(a) && line_is_blank(b))` or an in-place approach. Low priority since this only runs once per full render.

**N3. `RenderState` field `last_ui_top` shadows `ChatPosition::last_ui_top()` method (render_state.rs:179 vs 94)**
The field on `RenderState` has the same name as the method on `ChatPosition`. The `RenderState::last_ui_top()` method (line 246) chains them with `.or()`. This naming collision makes it easy to accidentally reference the wrong one.
-> Rename the field to `fallback_ui_top` to clarify its role as the fallback for Empty/Header states.

**N4. `PreOp::ClearHeaderArea` is a no-op in `apply_pre_ops` (run.rs:186)**
The variant is handled in `clear_header_areas` (line 124) before the sync block, but the match arm in `apply_pre_ops` does nothing. This is intentional but could confuse readers.
-> Add a comment on the empty match arm: "Handled in clear_header_areas outside sync block".

**N5. `prepare_frame` is 100 lines (run.rs:251-352)**
Acceptable but approaching the threshold. The flag-consumption section (lines 257-345) is a linear sequence of if-blocks that's easy to follow.
-> No action required unless it grows further.

**N6. Naming: `PreOp`, `ChatInsert`, `FramePrep`**
These names are clear in context. `PreOp` reads as "pre-operation" which accurately describes what it does. `FramePrep` bundles the frame preparation output. No concerns.

---

## Architecture Observations (not issues)

1. **State machine is well-designed.** `ChatPosition` with 4 variants cleanly encodes the lifecycle (Empty -> Header -> Tracking -> Scrolling). Transitions are explicit and tested. The `ui_drawn_at` tracking in Tracking/Scrolling is necessary for clear_from computation.

2. **clear_from in layout is actively used.** Despite draw_direct using `layout.top`, `render_frame` (run.rs:445-449) uses `clear_from` to clear stale rows between the old and new UI top. This handles popup dismiss, selector exit, and input height changes. It serves a real purpose.

3. **Duplication between `reprint_loaded_session` and Reflow is minimal.** Both call `build_chat_lines` and `position_after_reprint`/`mark_reflow_complete`, but `reprint_loaded_session` uses `crossterm::cursor::position()` to detect overflow (since it prints naturally from wherever the cursor is), while Reflow always starts from row 0 after scrolling. These are genuinely different code paths.

4. **`last_ui_top` field on RenderState is still needed.** When position is `Empty` or `Header`, `ChatPosition::last_ui_top()` returns `None`, but we still need to know where the UI was last drawn (e.g., for the first frame after a `/clear` that transitions to `Empty`). The field serves as the fallback for exactly this case.

---

## Test Coverage Assessment

| Module                         | Tests    | Coverage                                       |
| ------------------------------ | -------- | ---------------------------------------------- |
| render_state.rs (ChatPosition) | 10 tests | Good - all variants, transitions, edge cases   |
| render_state.rs (RenderState)  | 5 tests  | Good - reset, selector clear, fallback         |
| run.rs (plan_chat_insert)      | 7 tests  | Good - all position variants, overflow         |
| layout.rs                      | 6 tests  | Good - regions, popup, selector                |
| chat_renderer.rs               | 0 tests  | **Gap** - 651 lines of untested pure functions |
| render/direct.rs               | 0 tests  | Acceptable - I/O heavy, hard to unit test      |
| render/progress.rs             | 0 tests  | Acceptable - I/O heavy                         |

---

## Priority Order

1. W6 (chat_renderer tests) - Highest ROI, prevents regressions
2. W1 (dead code) + W2 (dead method) - Quick cleanup
3. W5 (unnecessary allocation) - One-line fix
4. N1 (clippy warnings) - One command
5. W3 (merge Overflow/ScrollInsert) - Simplifies code path
6. W8 (split build_lines) - Improves maintainability
7. W7 (reduce compute_layout calls) - Minor perf
8. W4 (wrap_styled_line allocation) - Profile first
