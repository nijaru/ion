# TUI Refactor Plan Review

**Date:** 2026-02-09
**Reviewer:** claude-opus-4-6
**Scope:** 4-phase TUI improvement plan (popup unification, layout pass, file split, scroll regions)
**Codebase state:** Builds clean, 145 TUI tests pass, 20,460 lines across TUI module

---

## Executive Summary

The plan is largely sound. Phases 1 and 3 are clear wins with low risk. Phase 2 (layout pass) is the most valuable but needs careful design to avoid introducing new bugs during the transition. Phase 4 (scroll regions) should be deferred or marked experimental -- crossterm does not ship DECSTBM support in stable releases (PR #918 is still open/unmerged as of Feb 2026), and raw escape sequences bring terminal compatibility risk.

**Verdict:** Proceed with Phases 1-3 in order. Defer Phase 4 until crossterm merges scroll region support or until flicker is a measurable user complaint.

---

## Phase 1: Unified Popup Rendering

**Assessment: Good -- low risk, clear value**

### Will it work?

Yes. The three popup implementations (command_completer.rs:111-178, file_completer.rs:141-209, history search in direct.rs:508-592) share an identical rendering pattern:

1. Calculate popup height from candidate count
2. Compute `popup_start = input_start.saturating_sub(popup_height)`
3. Loop over items, render with `MoveTo(1, row)` + `Clear(CurrentLine)`
4. Apply `Attribute::Reverse` for selected item, `Attribute::Dim` for others

The differences are only in item content rendering (command+description vs path+icon vs history entry). A `PopupItem` trait or struct with `label`, optional `hint`, and style flags captures all three.

### Concrete suggestion

```rust
struct PopupItem<'a> {
    primary: &'a str,
    secondary: Option<&'a str>,  // description/hint
    icon: Option<&'a str>,       // file icon
    is_selected: bool,
    color: Color,                // primary text color
}

fn render_popup<W: Write>(
    w: &mut W,
    items: &[PopupItem],
    anchor_row: u16,      // row below which popup renders
    width: u16,
) -> io::Result<()>
```

### Edge case to watch

The command completer calculates popup_width from max command + description length (line 131), while file completer uses max path length (line 156). The shared function should accept a width hint or auto-calculate from items. History search is different enough (it has a search prompt row at the bottom) that it may not fit cleanly into the same function -- consider whether it should use the shared popup for the match list but render its own prompt row separately.

### Risk: Low

Rendering-only change. If the popup renders wrong, it is immediately visible and easy to revert. No state changes.

---

## Phase 2: Layout Pass (`compute_layout` + `UiLayout`)

**Assessment: Most valuable phase, but highest risk. Needs careful sequencing.**

### Will it fix the popup clearing bug?

Yes, with a critical caveat. The root cause of the popup clearing bug was:

1. Popup rendered above `ui_start` (at `ui_start - popup_height`)
2. `Clear(FromCursorDown)` at `ui_start` did not reach those rows
3. On dismiss, popup rows persisted as ghost artifacts

The current fix (commit history shows 3 iterations) works by including popup_height in `calculate_ui_height`, which pushes `ui_start` up to cover the popup area, and using `old_ui_start.min(ui_start)` to clear stale rows when the area shrinks (direct.rs:46-54).

A `UiLayout` with explicit `Region` for each component would formalize this -- the clear region would be derived from the union of all component regions, making it impossible for a component to render outside the cleared area. This is structurally better than the current approach of layering corrections.

### Specific concerns

**Concern 1: The clear_from logic (direct.rs:46-54) is the real fix, not layout per se**

The `old_ui_start.min(ui_start)` trick is what prevents ghost artifacts. A `UiLayout` struct alone does not solve this unless the clearing logic explicitly tracks the previous frame's layout bounds. The plan must include storing the previous `UiLayout` (or just the previous topmost row) and clearing from `min(old_top, new_top)`.

**Concern 2: Mode transitions create discontinuities**

The current code has three distinct layout paths:

- `Mode::Input` with completer (direct.rs:32-42): popup + progress + input + status
- `Mode::Selector` (direct.rs:88-89): completely different layout (render_selector_direct takes over)
- `Mode::HistorySearch` (direct.rs:90-91): replaces status with search overlay

A single `compute_layout()` must handle all three. The selector is particularly tricky because it uses a different `SELECTOR_OVERHEAD` constant and its own height calculation (render/mod.rs:31-36). The `UiLayout` struct should probably use an enum for the body region:

```rust
enum BodyLayout {
    Input { popup: Option<Region>, progress: Region, input: Region, status: Region },
    Selector { selector: Region },
    HistorySearch { popup: Region, progress: Region, input: Region, search: Region },
}
```

**Concern 3: Resize during streaming**

During streaming (`is_running = true`), the run.rs main loop (lines 406-473) calculates `ui_height` independently of `draw_direct`. If `compute_layout()` becomes the single source of truth, it must be callable from both the chat insertion code and the draw code, and must produce consistent results when called multiple times in the same frame. Currently `calculate_ui_height` is called 3 times per frame in some paths (run.rs:307, 354, 407) -- these would need to call `compute_layout` once and pass the result through.

**Concern 4: Row tracking mode vs scroll mode**

The layout depends on which positioning mode is active (render_state.rs:66-68). In row-tracking mode, `ui_start` follows `chat_row`; in scroll mode, it is pinned to `term_height - ui_height`. The `compute_layout` function needs both `chat_row` and terminal dimensions as inputs. This is fine but must be explicit in the API.

### Incremental migration strategy

Do NOT try to replace all position calculations at once. Instead:

1. Create `compute_layout()` that returns `UiLayout` but **do not change `draw_direct` yet**
2. Add assertions that `UiLayout` matches current calculations (debug mode only)
3. Migrate `draw_direct` to use `UiLayout` for positioning (component by component)
4. Migrate run.rs chat insertion code to use the same `UiLayout`
5. Remove the standalone `calculate_ui_height` and `ui_start_row` methods

This lets you catch any divergence between old and new calculations before they become visible bugs.

### "27 scattered position calculations" claim

I count approximately 20 position-related calculations across direct.rs and run.rs (not 27), but the point stands. The calculations are scattered and the same values (ui_height, ui_start) are recomputed multiple times per frame.

### Risk: Medium-high

Layout bugs are the hardest TUI bugs to debug because they manifest as visual artifacts that are terminal-dependent. The assertion-first approach mitigates this significantly.

---

## Phase 3: Split direct.rs

**Assessment: Pure organization, no behavior change. Do it.**

### Current state

`direct.rs` is 593 lines with 7 methods on `App`:

- `draw_direct` (orchestrator, 117 lines)
- `selector_data` (data extraction, 113 lines)
- `render_selector_direct` (delegation, 18 lines)
- `render_progress_direct` (delegation, 10 lines)
- `render_progress_running` (60 lines)
- `render_progress_completed` (50 lines)
- `render_input_direct` (65 lines)
- `render_status_direct` (55 lines)
- `render_history_search` (85 lines)

### Proposed split

| File        | Content                                                        | Lines (approx) |
| ----------- | -------------------------------------------------------------- | -------------- |
| direct.rs   | `draw_direct` orchestrator                                     | ~100           |
| popup.rs    | `render_popup` (from Phase 1) + history search popup rendering | ~120           |
| progress.rs | `render_progress_direct/running/completed`                     | ~120           |
| input.rs    | `render_input_direct`                                          | ~70            |
| status.rs   | `render_status_direct`                                         | ~60            |
| selector.rs | Move `render_selector.rs` content into `render/selector.rs`    | ~245           |

### Consideration

`selector_data()` (lines 120-229) is data extraction, not rendering. It should stay on `App` or move to wherever the picker state lives, not into a render module. Do not conflate data preparation with rendering.

### Dependency on Phase 2

If Phase 2 introduces `UiLayout`, each component renderer receives its `Region` as a parameter. This pairs well with the file split since each file gets a self-contained function signature like `fn render_progress(w: &mut W, region: &Region, state: &ProgressState)`. Without Phase 2, the split still works but the methods remain `&self` on `App`, which is less clean.

### Risk: Low

Mechanical refactor. `cargo test` catches regressions. No semantic changes.

---

## Phase 4: Scroll Regions (DECSTBM)

**Assessment: Defer. High risk, low urgency, dependency gap.**

### The dependency problem

crossterm does not expose DECSTBM in its stable API. The PR adding region-scrolling commands (crossterm-rs/crossterm#918, August 2024) has not been merged as of February 2026. This means Phase 4 requires either:

1. Raw escape sequence writes (`\x1b[{top};{bottom}r`) bypassing crossterm
2. Forking or patching crossterm
3. Waiting for the PR to merge

Option 1 is fragile: crossterm may buffer or reorder output in ways that conflict with raw writes. Option 2 is maintenance burden. Option 3 has no timeline.

### Terminal compatibility concerns

DECSTBM is well-supported in modern terminals (xterm, iTerm2, WezTerm, Alacritty, kitty), but:

- Windows Terminal has historically incomplete VT support
- tmux/screen add a layer of complexity (they implement their own scroll regions)
- Resizing while a scroll region is active requires resetting and re-establishing the region
- If the process crashes without resetting the scroll region, the terminal is left in a broken state (the panic hook in run.rs:222-226 would need to reset it)

### Is the flicker actually a problem?

The current code already uses `BeginSynchronizedUpdate`/`EndSynchronizedUpdate` (run.rs:404, 480), which eliminates flicker in terminals that support it (most modern terminals). The `ScrollUp(n)` approach only flickers in terminals that do not support synchronized output -- and those same terminals are unlikely to handle DECSTBM better.

### What it would actually fix

The current scroll insertion pattern (run.rs:457-473):

1. Clear old UI at `ui_start`
2. `ScrollUp(line_count)` to make space
3. Print new lines at `ui_start - line_count`

With DECSTBM:

1. Set scroll region to `[0, ui_start)`
2. Scroll within region
3. Print new lines
4. Reset scroll region

The difference is that DECSTBM prevents the UI area from moving during the scroll, eliminating the clear-scroll-redraw sequence. But with synchronized output, all three steps are batched and presented atomically anyway.

### Recommendation

Mark Phase 4 as "future/experimental." Revisit if:

- crossterm merges scroll region support
- Users report flicker in terminals that support DECSTBM but not synchronized output (unlikely intersection)
- The project targets a terminal environment where synchronized output is unavailable

---

## Skipped Items Assessment

### Buffer diffing -- Correct to skip

The bottom UI is 5-15 lines. At 50 FPS (20ms tick), `Clear(FromCursorDown)` + full redraw with synchronized output is imperceptible. Diffing adds complexity for zero visible benefit. The tui-v2 design doc already validated this (ai/design/tui-v2.md:126-148).

### Virtual screen abstraction -- Correct to skip

This would reintroduce a ratatui-style abstraction layer. The entire point of TUI v2 was to drop that layer (ai/design/tui-v2.md:1-5). The current direct-write approach is working.

### Alternate screen mode -- Correct to skip

Codex CLI tried and reverted this. Users lose scrollback, which is a core feature of the inline TUI model.

---

## Missing Items / Gaps

### 1. History search does not fit the popup pattern cleanly

The history search popup (direct.rs:508-592) has a fundamentally different structure from command/file completers:

- It renders a search prompt row at the bottom of the popup (not just items)
- It calculates its own `popup_start` from `input_start`, not from `ui_start`
- It is triggered in a different mode (`Mode::HistorySearch`) while the completers are active during `Mode::Input`

The plan says "extract from history_search" but history search is actually rendered as a mode replacement (direct.rs:90-91), not as an overlay on the input mode. Be explicit about whether history search is part of Phase 1 or a separate concern.

### 2. The popup height is duplicated between layout and rendering

Currently, popup height is calculated in two places:

- `calculate_ui_height` (layout.rs:65-71) -- to include in total UI height
- `draw_direct` (direct.rs:32-42) -- to position components below the popup

Phase 2's `compute_layout` should eliminate this duplication, but Phase 1 alone does not. If Phase 1 ships before Phase 2, the duplication remains.

### 3. Selector mode layout is a completely separate code path

`render_selector_direct` (direct.rs:232-249) ignores the popup/progress/input/status layout entirely and delegates to `render_selector::render_selector` which does its own `Clear(FromCursorDown)` at `start_row` (render_selector.rs:45). The plan should address whether the selector gets its own `Region` in `UiLayout` or remains a separate path.

### 4. No mention of testing strategy

The current code has no rendering tests (the existing tests cover state management: completer navigation, fuzzy matching, etc). For a layout refactor, consider:

- Unit tests for `compute_layout` with various terminal sizes, modes, and popup states
- Property: `popup.end == progress.start` (no gaps/overlaps between regions)
- Property: `layout.top >= 0 && layout.bottom <= term_height`
- Property: `layout with popup active` covers a superset of `layout without popup`

This is where bugs will hide. Layout is pure arithmetic -- test it.

### 5. Frame-level consistency

Currently `calculate_ui_height` is called multiple times per frame in different code paths (run.rs:307, 354, 407 + draw_direct). If state changes between calls (e.g., completer deactivates between calls), the layout is inconsistent within a single frame. Phase 2 should compute layout once at frame start, pass it to all consumers.

---

## Idiomatic Rust Assessment

### Region struct

A `Region { row: u16, col: u16, width: u16, height: u16 }` is idiomatic. Consider `Copy` derive since it is small.

```rust
#[derive(Debug, Clone, Copy)]
struct Region {
    row: u16,
    height: u16,
    // col and width are almost always 0 and term_width for this TUI
}
```

Since all components are full-width, you could simplify to just `row` and `height`, passing `width` separately. This avoids storing redundant data.

### UiLayout lifetime

`UiLayout` should be a value type computed fresh each frame, not stored on `App`. Storing it would create another piece of state to keep synchronized. Compute it, pass it down, discard it.

```rust
fn compute_layout(&self, width: u16, height: u16) -> UiLayout { ... }

// In draw_direct:
let layout = self.compute_layout(width, height);
self.render_progress(w, &layout.progress)?;
self.render_input(w, &layout.input)?;
// etc.
```

### Component renderers as free functions vs methods

The current renderers are `&self` or `&mut self` methods on `App`, which means they can access all of App's 40+ fields. Phase 3's file split is a good opportunity to narrow the interface:

```rust
// Instead of:
impl App {
    fn render_progress(&self, w: &mut W, width: u16) -> io::Result<()> { ... }
}

// Consider:
mod progress {
    pub fn render(w: &mut W, region: Region, state: &ProgressState) -> io::Result<()> { ... }
}
```

Where `ProgressState` is a small struct or references extracted from `App`. This makes the render functions testable without constructing an entire `App`. However, this is a larger refactor that should come after the split, not during it.

---

## Recommended Order and Effort

| Phase                | Risk        | Effort    | Dependency            | Recommendation            |
| -------------------- | ----------- | --------- | --------------------- | ------------------------- |
| 1: Popup unification | Low         | 1-2 hours | None                  | Do first                  |
| 3: File split        | Low         | 1-2 hours | None (easier after 1) | Do second                 |
| 2: Layout pass       | Medium-high | 3-5 hours | Benefits from 1+3     | Do third, with assertions |
| 4: Scroll regions    | High        | 3-5 hours | crossterm PR #918     | Defer                     |

Rationale for reordering 3 before 2: Splitting the file first makes the layout refactor easier to review, since changes are localized to smaller files. The layout refactor touches every component's positioning code, so having it in separate files reduces merge conflicts and makes diffs more readable.

---

## Key Files Referenced

| File                                                          | Lines | Role                                    |
| ------------------------------------------------------------- | ----- | --------------------------------------- |
| `/Users/nick/github/nijaru/ion/src/tui/render/direct.rs`      | 593   | Main renderer, orchestrates bottom UI   |
| `/Users/nick/github/nijaru/ion/src/tui/render/layout.rs`      | 95    | Height calculations, ui_start_row       |
| `/Users/nick/github/nijaru/ion/src/tui/render_state.rs`       | 147   | Position tracking, mode flags           |
| `/Users/nick/github/nijaru/ion/src/tui/command_completer.rs`  | 264   | Slash command popup (render at L111)    |
| `/Users/nick/github/nijaru/ion/src/tui/file_completer.rs`     | 379   | File path popup (render at L141)        |
| `/Users/nick/github/nijaru/ion/src/tui/render_selector.rs`    | 244   | Provider/model/session picker UI        |
| `/Users/nick/github/nijaru/ion/src/tui/run.rs`                | 533   | Main loop, chat insertion, scroll logic |
| `/Users/nick/github/nijaru/ion/src/tui/render/mod.rs`         | 37    | Constants, selector_height helper       |
| `/Users/nick/github/nijaru/ion/src/tui/completer_state.rs`    | 181   | Shared completer state machine          |
| `/Users/nick/github/nijaru/ion/src/tui/render/chat.rs`        | 155   | Chat line building, scrollback reprint  |
| `/Users/nick/github/nijaru/ion/ai/design/tui-v2.md`           | 305   | TUI v2 architecture decisions           |
| `/Users/nick/github/nijaru/ion/ai/design/chat-positioning.md` | 164   | Row tracking vs scroll mode design      |

---

## Quality

**Date:** 2026-02-09
**Reviewer:** claude-opus-4-6
**Scope:** Post-implementation quality review of TUI rendering refactor (popup.rs extraction, direct.rs split, compute_layout)
**Codebase state:** Builds clean, 403 tests pass, clippy clean

---

### Overall Assessment

The refactor is well-executed. The popup extraction genuinely reduces duplication (3 independent render loops -> 1 shared function). The file split is clean with no circular dependencies. The `compute_layout` function centralizes layout arithmetic correctly. Several issues remain, ranging from important design debt to minor cleanups.

---

### Critical

None.

---

### Important

**[IMPORTANT] layout.rs + popup.rs: Duplicate Region types (confidence: 95%)**

`Region` in `layout.rs:9` and `PopupRegion` in `popup.rs:31` are structurally identical (`row: u16, height: u16`). They represent the same concept. The popup module was written before the layout module landed its `Region` type, and the two were never unified.

```
-> Use layout::Region everywhere. Delete PopupRegion. Update render_popup signature and all callers
   (command_completer.rs:5, file_completer.rs:5, history.rs:3).
```

**[IMPORTANT] layout.rs:168 + run.rs:303,408: calculate_ui_height still called alongside compute_layout (confidence: 95%)**

`calculate_ui_height` duplicates the total-height arithmetic from `compute_layout` (both sum popup + progress + input + status for Input mode, both call `selector_height` for Selector mode). It persists because `run.rs` lines 303 and 408 call it directly for chat insertion spacing, while `draw_direct` now uses `compute_layout`. This means two independent calculations of the same value per frame, which could diverge if either is modified independently.

```
-> Extract ui_height from compute_layout's result (it is top subtracted from the terminal height,
   or sum the region heights). Replace calculate_ui_height calls in run.rs with
   compute_layout().total_height() or equivalent. Then delete calculate_ui_height.
```

**[IMPORTANT] popup.rs:80: Padding uses byte length, not display width (confidence: 90%)**

`content_len = 1 + item.primary.len() + item.secondary.len()` uses `.len()` (byte count), but terminal rendering uses display columns. For ASCII-only content this is fine. For file_completer items containing the nerd font icon (U+F0251, 4 bytes, ~1-2 display columns), this over-counts by 2-3 bytes, resulting in under-padding of the reverse-video highlight bar.

Pre-existing issue (old code had the same behavior), but the refactor was an opportunity to fix it. The `unicode-width` crate's `UnicodeWidthStr::width()` gives correct display width.

```
-> Use unicode_display_width for content_len, or document the limitation.
   If the project does not use unicode-width yet, consider whether the visual
   artifact is noticeable enough to warrant the dependency.
```

**[IMPORTANT] popup.rs:80: Padding wrong when secondary is non-empty but show_secondary_dimmed is false (confidence: 95%)**

The padding always subtracts `item.secondary.len()` from the available width, even when secondary text is not rendered (because `show_secondary_dimmed` is false). No current caller triggers this (file completer and history search both use empty secondary strings), but it is a latent bug in the API.

```
-> Change line 80 to:
   let secondary_len = if style.show_secondary_dimmed { item.secondary.len() } else { 0 };
   let content_len = 1 + item.primary.len() + secondary_len;
```

---

### Should Fix

**[SHOULD-FIX] command_completer.rs:130-136, file_completer.rs:157-174, history.rs:64-82: Unnecessary Vec<String> allocations (confidence: 90%)**

Each render call allocates a `Vec<String>` for formatted display strings, then a `Vec<PopupItem>` that borrows from it. These are constructed every frame (~50 FPS) for each active popup. The allocations are small (max 7-8 items) and the frame rate is low, so this is not a performance problem in practice. But the pattern is avoidable: the formatting could be done inline during item construction, or the formatted strings could be stack-allocated via `arrayvec` or similar.

```
-> Low priority. If perf profiling shows allocation pressure, refactor to avoid
   the intermediate Vec<String>. Not urgent at current scale.
```

**[SHOULD-FIX] direct.rs:32: render_selector_direct receives hardcoded 0 for \_height (confidence: 90%)**

```rust
self.render_selector_direct(w, selector.row, layout.width, 0)?;
```

The `_height` parameter on `render_selector_direct` (direct.rs:204) is unused. The selector computes its own height from `selector_height()`. This dead parameter should be removed.

```
-> Remove _height parameter from render_selector_direct signature and the
   hardcoded 0 at the call site.
```

**[SHOULD-FIX] layout.rs:44: compute_layout is a method on App but only reads ~5 fields (confidence: 85%)**

`compute_layout` accesses `self.mode`, `self.selector_page`, `self.provider_picker`, `self.model_picker`, `self.session_picker`, `self.command_completer`, `self.file_completer`, and calls `self.calculate_input_height` and `self.ui_start_row`. It does not mutate anything. A free function taking narrower parameters would be more testable and make dependencies explicit.

However, the number of parameters it needs (mode, selector_page, popup_height, input_height, ui_start_row result) is large enough that a free function would have a wide signature. The current approach is pragmatic.

```
-> Leave as-is for now. If compute_layout gains tests that need to construct
   App (currently the tests use a manual builder to avoid this), consider
   extracting LayoutInputs { mode, popup_height, input_height, ... } struct.
```

**[SHOULD-FIX] layout.rs:207-357: Tests reimplement layout logic instead of testing it (confidence: 90%)**

The `test_input_layout` helper manually builds a `UiLayout` using the same arithmetic as `compute_layout`. This means the tests verify the arithmetic against itself -- if the logic has a bug, the test helper has the same bug. The tests would not catch a regression where `compute_layout` changes but the manual builder does not.

The tests should call `compute_layout` on a real or minimal `App` instance, or at minimum test properties (adjacency, bounds) on layouts constructed by the production code path. The `test_layout_selector_mode` test (line 337) is the most obvious example: it manually constructs a `UiLayout` and then asserts properties of what it just constructed.

```
-> Rewrite tests to call compute_layout (requires constructing App, which may
   be heavy) OR test only structural properties (region adjacency, no overlap,
   fits-in-terminal) on the manually-built layouts, which is what the current
   tests mostly do. The selector test is the main offender since it does not
   exercise any production code.
```

---

### Minor / Uncertain

**[MINOR] PopupStyle boolean fields (confidence: 70%)**

`PopupStyle` has 3 fields: `primary_color: Color`, `show_secondary_dimmed: bool`, `dim_unselected: bool`. Two booleans in a style struct is on the edge of "accumulating booleans" smell. Currently manageable, but if a fourth flag is added, consider an enum for popup variants instead.

```
-> Monitor. If a fourth flag is needed, refactor to enum PopupVariant { Command, File, History }.
```

**[MINOR] color_override: Option<Color> on PopupItem (confidence: 75%)**

Only the file completer uses `color_override` (to distinguish directories from files with Blue vs Reset). The other two callers always pass `None`. This is a reasonable escape hatch for per-item styling, but it adds a field to every `PopupItem` that is unused 2/3 of the time. An alternative is to encode the color in the primary text style, but that would require the popup renderer to accept a per-item callback, which is worse.

```
-> Acceptable. The Option<Color> is 5 bytes overhead per item, negligible.
   The alternative (callback or trait) would be more complex for no visible benefit.
```

**[MINOR] history.rs: Does not use the layout popup Region (confidence: 80%)**

History search calculates its own `popup_start` and `popup_height` from `input_start` (history.rs:27-28), completely independent of the `UiLayout` popup region. This is because history search is a different mode (`Mode::HistorySearch`) and its popup is not tracked by `active_popup_height()` (which only checks `Mode::Input`). The layout system does not account for the history search popup, so when history search is active, the UI area size does not include the popup rows. This could cause the history search popup to overlap with chat content.

```
-> Investigate whether history search popup overlaps with chat scrollback.
   If it does, add HistorySearch mode handling to active_popup_height().
```

---

### Module Organization Assessment

The split is clean:

| File         | Lines | Responsibility                     | Dependencies          |
| ------------ | ----- | ---------------------------------- | --------------------- |
| direct.rs    | 214   | Orchestrator + selector data       | layout, selector, all |
| layout.rs    | 357   | Region types + compute_layout      | render constants      |
| popup.rs     | 93    | Shared popup renderer              | crossterm only        |
| selector.rs  | 244   | Selector UI (moved from root)      | crossterm only        |
| progress.rs  | 137   | Spinner + completion stats         | util, crossterm       |
| input_box.rs | 75    | Input content + scrolling          | composer, crossterm   |
| status.rs    | 65    | Mode/model/tokens line             | util, crossterm       |
| history.rs   | 113   | Ctrl+R search overlay              | popup, crossterm      |
| mod.rs       | 43    | Constants + selector_height helper | --                    |

No circular dependencies. Each component file depends only on `crossterm` and possibly `popup` or `render` constants. The orchestrator (`direct.rs`) depends on everything else but nothing depends on it. Clean DAG.

The `selector_data()` method on `App` in `direct.rs` is data preparation, not rendering. It would be cleaner in a separate module, but its current location is acceptable since it is only called from `render_selector_direct` in the same file.

---

### Summary

| Category  | Count | Items                                                                          |
| --------- | ----- | ------------------------------------------------------------------------------ |
| Critical  | 0     | --                                                                             |
| Important | 4     | Duplicate Region types, calculate_ui_height duplication, padding bugs (2)      |
| Should    | 4     | Vec allocs, dead \_height param, compute_layout on App, tests reimplement code |
| Minor     | 3     | Boolean flags, color_override, history popup not in layout                     |

---

## Safety

**Date:** 2026-02-09
**Reviewer:** claude-opus-4-6
**Scope:** Error handling, robustness, panic paths, terminal state safety in the 3-commit TUI refactor
**Build:** Clean (cargo build, cargo clippy, cargo test -- all pass, including 5 layout tests)

---

### Critical

**[CRITICAL] `/Users/nick/github/nijaru/ion/src/tui/command_completer.rs`:127 -- Unsigned integer underflow on narrow terminals**

```rust
let popup_width = (max_cmd_len + max_desc_len + 6).min(width as usize - 4) as u16;
```

`width as usize - 4` is unsigned subtraction. If the terminal width is < 4 (possible during resize or in edge-case terminal emulators), this panics in debug mode and wraps to `usize::MAX` in release mode. The file completer at `/Users/nick/github/nijaru/ion/src/tui/file_completer.rs`:153 correctly uses `width.saturating_sub(4)`.

**Pre-existing** but the refactor touched this file and missed the fix.

```
-> Fix: (max_cmd_len + max_desc_len + 6).min((width as usize).saturating_sub(4)) as u16
```

**Confidence:** 95%

---

### Important

**[IMPORTANT] `/Users/nick/github/nijaru/ion/src/tui/render/direct.rs`:40-42 -- Progress line now renders in HistorySearch mode (behavior change)**

Old code guarded progress rendering with `self.mode == Mode::Input`:

```rust
if progress_height > 0 && self.mode == Mode::Input {
    self.render_progress_direct(w, width)?;
}
```

New code renders progress unconditionally within the `BodyLayout::Input` branch, which includes `HistorySearch`:

```rust
execute!(w, MoveTo(0, progress.row), Clear(ClearType::CurrentLine))?;
self.render_progress_direct(w, layout.width)?;
```

This is not dangerous (the progress region is properly allocated by the layout), but it is an unintended behavior change. The progress spinner or completion stats will now be visible during history search.

```
-> Either guard with `if self.mode != Mode::HistorySearch` or add a comment
   confirming this is intentional.
```

**Confidence:** 90%

---

**[IMPORTANT] `/Users/nick/github/nijaru/ion/src/tui/render/popup.rs`:80 -- Padding calculation uses byte length, not display width**

```rust
let content_len = 1 + item.primary.len() + item.secondary.len();
let padding = (popup_width as usize).saturating_sub(content_len);
```

`str::len()` returns byte count. Multi-byte UTF-8 characters (nerd font icons in file completer, non-ASCII paths) cause `content_len` to overcount, resulting in insufficient padding. The reverse-video highlight bar will be shorter than `popup_width` for items with multi-byte characters.

**Pre-existing** issue consolidated from the old per-completer code.

```
-> Use chars().count() at minimum, or unicode-width for correct terminal column count.
   The fix applies to both item.primary and item.secondary.
```

**Confidence:** 85%

---

**[IMPORTANT] `/Users/nick/github/nijaru/ion/src/tui/render/popup.rs`:80 -- Secondary text length counted in padding even when not rendered**

```rust
let content_len = 1 + item.primary.len() + item.secondary.len();
```

When `style.show_secondary_dimmed` is false and `item.secondary` is non-empty, the secondary text is NOT printed (line 70 skips it), but its byte length IS subtracted from the available padding. This produces an incorrect highlight width. No current caller triggers this (file completer and history search both pass empty secondary strings), but it is a latent bug in the shared API.

```
-> let sec_len = if style.show_secondary_dimmed && !item.secondary.is_empty() {
       item.secondary.len()
   } else {
       0
   };
   let content_len = 1 + item.primary.len() + sec_len;
```

**Confidence:** 95%

---

### Verified Safe

| Area                                                 | Verdict                                                                                                                                                                                                                                                                                                                                                                                                         |
| ---------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Attribute leaks in popup.rs**                      | `Reverse` set at line 53 and `Dim` at line 55 are always reset at line 87 via `SetAttribute(Attribute::Reset)`. The condition `item.is_selected \|\| style.dim_unselected` covers every path where an attribute was set. `ResetColor` at line 66 handles foreground color independently. `NormalIntensity` at line 75 resets Dim for secondary text without affecting Reverse. No attribute leaks across items. |
| **Attribute leaks in selector.rs**                   | Each item ends with `SetAttribute(Attribute::Reset), ResetColor` at line 217. Warning rendering at lines 229-237 has its own Reset+ResetColor. Tab bar rendering at lines 50-88 resets after each label. No leaks.                                                                                                                                                                                              |
| **popup_width = 0**                                  | `saturating_sub` at line 81 yields 0 padding. The loop renders items without padding. Visual imperfection but no crash or terminal corruption.                                                                                                                                                                                                                                                                  |
| **region.height = 0**                                | `.take(0)` at line 47 produces an empty iterator. The function returns `Ok(())` immediately. Safe.                                                                                                                                                                                                                                                                                                              |
| **Empty items slice**                                | The loop body does not execute. Safe.                                                                                                                                                                                                                                                                                                                                                                           |
| **`region.row + i as u16` overflow**                 | `i` is bounded by `region.height` (u16, max 65535). `region.row` is computed via `height.saturating_sub(total)`, so `region.row + region.height <= height`. No overflow possible for valid terminal dimensions.                                                                                                                                                                                                 |
| **Error propagation**                                | All render functions return `io::Result<()>` and propagate errors with `?`. No `unwrap()` on IO operations. Iterator `unwrap_or` calls (e.g., `.max().unwrap_or(10)`) provide safe defaults for empty collections.                                                                                                                                                                                              |
| **cleanup_terminal refactor**                        | `/Users/nick/github/nijaru/ion/src/tui/run.rs`:122-125 uses `compute_layout` + `Clear(FromCursorDown)`. Functionally equivalent to the old row-by-row clearing. The layout computation at cleanup time uses `app.render_state.last_ui_start` to include stale popup rows in the clear region.                                                                                                                   |
| **Panic hook coverage**                              | The panic hook at `/Users/nick/github/nijaru/ion/src/tui/run.rs`:218-222 restores raw mode and cursor visibility. It does not depend on layout state, so the refactor does not affect its behavior.                                                                                                                                                                                                             |
| **Terminal state on partial render failure**         | If `draw_direct` returns `Err` partway through, the synchronized update block is NOT ended (the `EndSynchronizedUpdate` at run.rs:486 is skipped). However, `cleanup_terminal` at run.rs:119 does `let _ = execute!(stdout, EndSynchronizedUpdate)` as a safety net. Any attribute leaks from a partial render are cleared by the next frame's `Clear(FromCursorDown)` or by terminal reset on exit.            |
| **`last_ui_start` tracking**                         | Stored at `/Users/nick/github/nijaru/ion/src/tui/render/direct.rs`:21. The `compute_layout` function uses `last_top.map_or(top, \|old\| old.min(top))` at layout.rs:74 to ensure `clear_from` always covers the maximum extent of the previous and current UI areas. Popup dismiss, mode change, and resize all produce correct clear regions.                                                                  |
| **`input.height.saturating_sub(2)` at direct.rs:45** | `calculate_input_height` enforces `MIN_HEIGHT = 3`, so `input.height >= 3`. `saturating_sub(2)` yields >= 1. No zero-height content area.                                                                                                                                                                                                                                                                       |
| **Selector `0` height argument**                     | The `_height` parameter at `/Users/nick/github/nijaru/ion/src/tui/render/direct.rs`:204 is explicitly unused. Passing `0` has no effect.                                                                                                                                                                                                                                                                        |
| **History search popup positioning**                 | `/Users/nick/github/nijaru/ion/src/tui/render/history.rs`:28 uses `input_start.saturating_sub(popup_height)`. Saturating arithmetic prevents underflow. The history search popup renders independently within the UI area's input region.                                                                                                                                                                       |
| **Division by zero in status.rs**                    | `/Users/nick/github/nijaru/ion/src/tui/render/status.rs`:56-58: `(used * 100) / max` is guarded by `if max > 0`. Safe.                                                                                                                                                                                                                                                                                          |
| **`compute_layout` total overflow**                  | `/Users/nick/github/nijaru/ion/src/tui/render/layout.rs`:71: `popup_height + progress_height + input_height + status_height`. All u16. Theoretical max: ~65545 (viewport_height + 10). In practice, terminal heights are < 500. No real overflow risk.                                                                                                                                                          |

---

### Summary

| Severity      | Count | Items                                                                            |
| ------------- | ----- | -------------------------------------------------------------------------------- |
| Critical      | 1     | command_completer underflow on narrow terminal (pre-existing, missed fix)        |
| Important     | 3     | Progress in HistorySearch mode, byte-length padding, secondary length in padding |
| Verified safe | 14    | All other focus areas checked and confirmed correct                              |

---

## Correctness

**Date:** 2026-02-08
**Reviewer:** claude-opus-4-6
**Scope:** Correctness review of 3-commit TUI rendering refactor (678192d, 2d065e0, ff76b8f)
**Method:** Line-by-line comparison of old vs new code, build verification, test execution
**Result:** Build clean, all 403 tests pass (including 5 layout-specific tests), clippy clean

---

### Critical

None.

---

### Important (should fix)

**[IMPORTANT] `/Users/nick/github/nijaru/ion/src/tui/file_completer.rs`:160-173,180-184 -- Directory path text now colored Blue, was previously default (confidence: 95%)**

Old code applied `SetForegroundColor(Color::Blue)` to the icon only, then `ResetColor`, then printed the path in default color:

```rust
// OLD (678192d^)
SetForegroundColor(if candidate.is_dir { Color::Blue } else { Color::Reset }),
Print(icon),      // icon in Blue
ResetColor,        // reset
Print(display),    // path in default color
```

New code combines icon and path into `primary = format!("{icon}{display}")` and renders the entire string in the per-item color:

```rust
// NEW (file_completer.rs:172, popup.rs:62-66)
color_override: Some(if c.is_dir { Color::Blue } else { Color::Reset }),
// render_popup prints all of item.primary in that color
```

Result: directory entries now render with both icon AND path text in Blue, whereas before only the icon was Blue. Files are unaffected (Color::Reset renders as default in both cases).

```
-> Either accept as deliberate visual change (Blue paths for dirs is arguably
   more readable), or split into icon-primary + path-secondary with separate
   color handling.
```

---

### Verified Correct (confidence >= 90%)

**compute_layout clear_from timing matches old draw_direct behavior**

Old `draw_direct` read `last_ui_start` before writing, computed `clear_from = min(old, new)`, then updated `last_ui_start`. New code passes `last_ui_start` as parameter to `compute_layout`, which computes `clear_from = last_top.map_or(top, |old| old.min(top))`, then `draw_direct` writes `layout.top` into `last_ui_start`. Sequence is identical: read -> compute min -> write new.

Verified at:

- `/Users/nick/github/nijaru/ion/src/tui/render/layout.rs`:74 (clear_from calculation)
- `/Users/nick/github/nijaru/ion/src/tui/render/direct.rs`:21 (last_ui_start update)
- `/Users/nick/github/nijaru/ion/src/tui/run.rs`:478-483 (call sequence)

**Region adjacency is correct (popup -> progress -> input -> status)**

Layout arithmetic at `/Users/nick/github/nijaru/ion/src/tui/render/layout.rs`:76-101 uses `row += height` pattern ensuring zero-gap adjacency. Confirmed by unit tests `test_layout_regions_adjacent` and `test_layout_with_popup_regions_adjacent` at lines 267 and 289.

**Popup dismiss clears stale rows correctly**

When popup deactivates, `active_popup_height()` returns 0, `compute_layout` produces higher `top` (larger row number), and `clear_from = min(old_top_with_popup, new_top_without)` = old_top_with_popup, so `Clear(FromCursorDown)` covers stale popup rows. Confirmed by test `test_layout_popup_dismiss_clears` at line 311.

**Command completer render_popup equivalence**

Traced old inline rendering (MoveTo, Clear, Reverse, Print cmd in Cyan, padding, Dim desc, padding, NoReverse) against new render_popup (same sequence with `Attribute::Reset` instead of `Attribute::NoReverse`). At the point of attribute cleanup, only Reverse is active, so Reset and NoReverse produce the same visible result. Padding arithmetic is identical (verified algebraically: both produce `content_len = max_cmd_len + desc.len() + 3`).

**History search render_popup equivalence**

Old code: inline loop with Reverse for selected, Dim for unselected, Print(" ") + Print(display), Reset. New code: builds `Vec<PopupItem>` with primary=display, calls render_popup with `dim_unselected: true`. The additional `SetForegroundColor(Reset)` and `ResetColor` in render_popup around the primary text do not affect visible output since Dim is an attribute (not a color) and remains active through color resets.

One minor behavioral difference: new code adds full-width padding to reverse-video highlights (`popup_width = width - 2`), whereas old code only highlighted the text content. This is a visual enhancement, not a regression.

**Selector mode dispatch is correct**

Old `draw_direct` rendered progress/input/status for all modes, then overlaid selector. New code uses `match layout.body { Selector => ..., Input => ... }` which skips the input rendering in selector mode. Visually identical because the selector's `render_selector` does `Clear(FromCursorDown)` at its start row, which would have overwritten the input UI anyway.

**cleanup_terminal produces equivalent clear behavior**

Old code: row-by-row `Clear(CurrentLine)` from `ui_start` to `ui_end`. New code: single `Clear(FromCursorDown)` from `layout.top`. Functionally equivalent (clears same or larger range).

**needs_selector_clear block works correctly despite using layout.top instead of layout.clear_from**

The block at `/Users/nick/github/nijaru/ion/src/tui/run.rs`:348-362 uses `sel_layout.top` for clearing. The subsequent `draw_direct` call (line 483) recomputes layout with the same `last_ui_start` (since the selector-clear block does not update it) and its `clear_from` covers the full stale range.

**active_popup_height mode guard is correct**

The new `active_popup_height()` at `/Users/nick/github/nijaru/ion/src/tui/render/layout.rs`:117 checks `self.mode == Mode::Input`. Completers are only active during Input mode (deactivated on mode transitions), so the guard produces the same result as the old unguarded code.

**Attribute::Reset vs Attribute::NoReverse is safe in all three popup callers**

At the point of attribute cleanup in render_popup (line 87), only Reverse or Dim is active (mutually exclusive per the if/else-if at lines 52-56). NormalIntensity already cleared Dim from secondary text. Reset and NoReverse both produce a clean state.

---

### Edge Cases Verified

| Case                          | Result                                                                                                             |
| ----------------------------- | ------------------------------------------------------------------------------------------------------------------ |
| Empty popup (0 candidates)    | Both completers return early on empty; `active_popup_height` returns 0; no popup region allocated                  |
| Zero-height input             | `calculate_input_height` clamps to MIN_HEIGHT=3; cannot be zero                                                    |
| Terminal smaller than UI      | `saturating_sub` prevents underflow; `top` becomes 0; UI renders at row 0                                          |
| History search with 0 matches | popup_height = 1 (search prompt only), render_popup not called, just the prompt row                                |
| History entry returns None    | New code: empty string displayed. Old code: nothing printed. Functionally identical since line was already cleared |

---

### Summary

| Category         | Count | Items                                                                                                                                         |
| ---------------- | ----- | --------------------------------------------------------------------------------------------------------------------------------------------- |
| Critical         | 0     | --                                                                                                                                            |
| Important        | 1     | File completer directory path now colored Blue (was default)                                                                                  |
| Verified correct | 9     | clear_from timing, region adjacency, popup dismiss, command completer, history search, selector dispatch, cleanup, selector-clear, mode guard |
| Edge cases       | 5     | All verified safe                                                                                                                             |
