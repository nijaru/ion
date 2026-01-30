# Sprint: render.rs Refactoring

**Source:** Code analysis of `src/tui/render.rs`
**Generated:** 2026-01-30
**Completed:** 2026-01-30
**Goal:** Reduce complexity and improve maintainability of TUI rendering code

## Context

The render.rs file exceeds the 400-line threshold and contains `render_selector_direct` at 268 lines. Recent bug fixes in this area suggest the code would benefit from better structure.

---

## Task 1: Consolidate Magic Numbers

**Depends on:** none

**Description:** Move duplicated constants to module level. `MAX_VISIBLE_ITEMS` appears at lines 73 and 527.

**Acceptance Criteria:**

- [ ] `MAX_VISIBLE_ITEMS` defined once at module level
- [ ] `SELECTOR_OVERHEAD` extracted as named constant
- [ ] All inline constants in `calculate_ui_height` moved to module level
- [ ] `cargo clippy` passes
- [ ] Manual test: selector opens and displays correctly

**Files:** `src/tui/render.rs`

**Technical Notes:**

- Constants at lines 73 and 527 must match
- Keep existing grouping comment style for constants

---

## Task 2: Extract Progress Rendering Helpers

**Depends on:** none

**Description:** Split `render_progress_direct` into `render_progress_running` and `render_progress_completed` helper methods.

**Acceptance Criteria:**

- [ ] `render_progress_running` handles spinner/tool display (~30 lines)
- [ ] `render_progress_completed` handles summary display (~30 lines)
- [ ] Main function is dispatcher only (~10 lines)
- [ ] `cargo clippy` passes
- [ ] Manual test: progress shows during run, summary shows after completion

**Files:** `src/tui/render.rs:609-691`

**Technical Notes:**

- Each helper takes `&self, w: &mut W`
- Move spinner array to constant or keep in running helper

---

## Task 3: Extract Selector Data Model

**Depends on:** Task 1

**Description:** Create `SelectorData` and `SelectorItem` structs to separate data extraction from rendering in `render_selector_direct`.

**Acceptance Criteria:**

- [ ] `SelectorData` struct with title, description, items, selected_idx, filter_text, show_tabs
- [ ] `SelectorItem` struct with label, is_valid, hint
- [ ] `fn selector_data(&self) -> SelectorData` method (~40 lines)
- [ ] Match arms in `render_selector_direct` replaced with single call
- [ ] `cargo clippy` passes
- [ ] Manual test: all three selector pages (provider/model/session) work

**Files:** `src/tui/render.rs:338-420`

**Technical Notes:**

- Put structs near top of file or in `types.rs`
- `show_tabs: bool` differentiates session page from provider/model
- Keep auth hint logic in data extraction, not rendering

---

## Task 4: Extract Selector Rendering Helpers

**Depends on:** Task 3

**Description:** Break `render_selector_direct` into focused helper methods for each UI section.

**Acceptance Criteria:**

- [ ] `render_selector_tabs` - tab bar rendering (~30 lines)
- [ ] `render_selector_search_box` - filter input box (~40 lines)
- [ ] `render_selector_list` - item list with scroll (~50 lines)
- [ ] `render_selector_hint` - hint line (~10 lines)
- [ ] Main function orchestrates calls (~30 lines)
- [ ] `cargo clippy` passes
- [ ] Manual test: selector navigation, filtering, selection all work

**Files:** `src/tui/render.rs:338-606`

**Technical Notes:**

- Pass `SelectorData` by reference to helpers
- Return cursor position from search_box for final positioning
- List helper handles scroll offset calculation

---

## Task 5: Move Selector to Separate Module

**Depends on:** Task 4

**Description:** Extract all selector rendering code to `src/tui/selector_render.rs`.

**Acceptance Criteria:**

- [ ] New file `src/tui/selector_render.rs` (~200 lines)
- [ ] Contains: `SelectorData`, `SelectorItem`, all `render_selector_*` methods
- [ ] `render.rs` drops to ~550 lines
- [ ] `mod selector_render` added to `src/tui/mod.rs`
- [ ] `cargo clippy` passes
- [ ] Manual test: selector functionality unchanged

**Files:**

- `src/tui/selector_render.rs` (new)
- `src/tui/render.rs`
- `src/tui/mod.rs`

**Technical Notes:**

- Methods become standalone functions or impl on a trait
- May need to pass `App` state or extract relevant fields
- Consider whether `SelectorData` belongs in `types.rs` instead

---

## Validation

After all tasks:

- [ ] `cargo test` passes
- [ ] `cargo clippy` passes
- [ ] Manual test: full conversation with tool calls, selector usage, resize

## Risk

**Safe** - All changes are structural refactoring with no behavior change. Each task can be committed and tested independently.
