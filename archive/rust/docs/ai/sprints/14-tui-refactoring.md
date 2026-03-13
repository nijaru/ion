# Sprint 14: TUI Refactoring

**Goal:** Fix critical bugs, reduce code duplication, improve maintainability
**Status:** COMPLETE (2026-02-04)
**Source:** ai/review/tui-analysis-2026-02-04.md
**Target:** Reduce TUI from ~9,300 to ~7,500 lines (~20% reduction)

## Overview

Code review identified:

- 5 panic-causing bugs in composer/state.rs and visual_lines.rs
- ~500 lines of duplicated picker/completer code
- ~120 lines of dead code (unused Terminal struct)
- App struct with 35+ fields needing decomposition

## Phases

| Phase              | Goal                   | Lines Saved | tk Tasks                  |
| ------------------ | ---------------------- | ----------- | ------------------------- |
| 1. Panic Fixes     | Eliminate crash bugs   | 0 (safety)  | tk-q48p, tk-glcs, tk-xznp |
| 2. Dead Code       | Remove unused Terminal | ~120        | tk-4zp4                   |
| 3. Picker Trait    | Unify 3 pickers        | ~300        | tk-y9tc                   |
| 4. Completer Trait | Unify 2 completers     | ~160        | tk-t46x                   |
| 5. App Decompose   | Group App fields       | 0 (clarity) | tk-4t1f                   |

---

## Phase 1: Panic Fixes

**Must complete first - these are crash bugs.**

### Task 1.1: Fix visual_lines.rs empty vec panic

**tk:** tk-q48p
**Depends on:** none
**File:** `src/tui/composer/visual_lines.rs:62-75`

**Description:**
`find_visual_line_and_col` accesses `lines[last]` without checking if `lines` is empty. When `width=0` edge case occurs, this panics.

**Current code:**

```rust
let last = lines.len().saturating_sub(1);
(last, char_idx.saturating_sub(lines[last].0))  // panics if lines empty
```

**Fix:**

```rust
pub fn find_visual_line_and_col(lines: &[(usize, usize)], char_idx: usize) -> (usize, usize) {
    if lines.is_empty() {
        return (0, 0);
    }
    // ... rest of function
}
```

**Acceptance Criteria:**

- [ ] Guard added at function start
- [ ] Test added for empty lines case
- [ ] All existing tests pass

---

### Task 1.2: Fix state.rs array bounds issues

**tk:** tk-glcs
**Depends on:** none
**File:** `src/tui/composer/state.rs` (lines 261, 549, 588)

**Description:**
Three unchecked accesses that can panic:

1. **Line 261** - Underflow in `move_up_logical`:

```rust
let prev_line_len = line_start - prev_line_start - 1;  // underflows
```

Fix: `line_start.saturating_sub(prev_line_start).saturating_sub(1)`

2. **Line 549** - Array access in `calculate_cursor_pos`:

```rust
let line_start = lines[line_idx].0;  // line_idx can == lines.len()
```

Fix: `lines.get(line_idx).map_or(0, |l| l.0)`

3. **Line 588** - Unwrap in `visual_line_count`:

```rust
let last_line = lines.last().unwrap();  // panics if empty
```

Fix: `lines.last().unwrap_or(&(0, 0))`

**Acceptance Criteria:**

- [ ] All three issues fixed with safe alternatives
- [ ] No new clippy warnings
- [ ] All existing tests pass

---

### Task 1.3: Add terminal panic hook

**tk:** tk-xznp
**Depends on:** none
**File:** `src/tui/run.rs`

**Description:**
If the TUI panics, terminal remains in raw mode, leaving user with broken terminal.

**Fix:** Add panic hook before entering raw mode:

```rust
pub async fn run(...) -> Result<...> {
    // Set panic hook to restore terminal
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::terminal::LeaveAlternateScreen,
            crossterm::cursor::Show
        );
        original_hook(info);
    }));

    // ... rest of function

    // Restore original hook on clean exit
    std::panic::set_hook(original_hook);
}
```

**Acceptance Criteria:**

- [ ] Panic hook set before raw mode enabled
- [ ] Terminal restored on panic (manual test)
- [ ] Original hook restored on clean exit
- [ ] All tests pass

---

## Phase 2: Dead Code Removal

### Task 2.1: Remove unused Terminal struct

**tk:** tk-4zp4
**Depends on:** Phase 1 complete
**File:** `src/tui/terminal.rs`

**Description:**
`Terminal` struct (lines 19-137) is never used - direct crossterm calls are used instead. `StyledLine`, `StyledSpan`, `LineBuilder` (lines 139-462) ARE used (184 occurrences).

**Fix:**

1. Delete `Terminal` struct and its `impl` block (lines 18-137)
2. Verify no imports break
3. Update module doc comment

**Acceptance Criteria:**

- [ ] Terminal struct removed
- [ ] No compilation errors
- [ ] ~120 lines removed
- [ ] All tests pass

---

## Phase 3: Picker Trait Extraction

### Task 3.1: Create FilterablePicker<T> generic

**tk:** tk-y9tc
**Depends on:** Phase 2 complete
**File:** New `src/tui/picker.rs`

**Description:**
Three pickers share 80% identical code:

- `ProviderPicker` (133 lines)
- `ModelPicker` (404 lines) - more complex, has two-stage selection
- `SessionPicker` (166 lines)

Common patterns:

- `filter_input: FilterInputState`
- `list_state: SelectionState`
- `items: Vec<T>`, `filtered: Vec<T>`
- `move_up/down`, `jump_to_top/bottom` (identical implementations)
- `apply_filter()` (similar structure)

**Design:**

```rust
/// Generic filterable picker with fuzzy search.
pub struct FilterablePicker<T: Clone> {
    /// All items
    items: Vec<T>,
    /// Filtered items after search
    filtered: Vec<T>,
    /// Filter input state
    filter_input: FilterInputState,
    /// Selection state
    selection: SelectionState,
}

impl<T: Clone> FilterablePicker<T> {
    pub fn new() -> Self { ... }
    pub fn set_items(&mut self, items: Vec<T>) { ... }
    pub fn apply_filter(&mut self, matcher: impl Fn(&T, &str) -> bool) { ... }
    pub fn move_up(&mut self, count: usize) { ... }
    pub fn move_down(&mut self, count: usize) { ... }
    pub fn jump_to_top(&mut self) { ... }
    pub fn jump_to_bottom(&mut self) { ... }
    pub fn selected(&self) -> Option<&T> { ... }
    pub fn filtered(&self) -> &[T] { ... }
    pub fn filter_input(&self) -> &FilterInputState { ... }
    pub fn filter_input_mut(&mut self) -> &mut FilterInputState { ... }
}
```

**Acceptance Criteria:**

- [ ] `FilterablePicker<T>` created in new `picker.rs`
- [ ] Unit tests for navigation methods
- [ ] No functionality changes yet (just the generic)

---

### Task 3.2: Migrate ProviderPicker to FilterablePicker

**tk:** tk-y9tc (continued)
**Depends on:** Task 3.1
**File:** `src/tui/provider_picker.rs`

**Description:**
Simplest picker - migrate first as proof of concept.

**Current:** 133 lines
**Target:** ~50 lines (wrapper around FilterablePicker)

```rust
pub struct ProviderPicker {
    picker: FilterablePicker<ProviderStatus>,
}

impl ProviderPicker {
    pub fn refresh(&mut self) {
        let providers = ProviderStatus::sorted(ProviderStatus::detect_all());
        self.picker.set_items(providers);
        self.apply_filter();
    }

    pub fn apply_filter(&mut self) {
        let matcher = SkimMatcherV2::default().ignore_case();
        self.picker.apply_filter(|p, filter| {
            matcher.fuzzy_match(p.provider.name(), filter).is_some()
                || matcher.fuzzy_match(p.provider.description(), filter).is_some()
        });
    }

    // Delegate navigation to picker
}
```

**Acceptance Criteria:**

- [ ] ProviderPicker uses FilterablePicker internally
- [ ] All existing provider picker tests pass
- [ ] Manual test: provider selection works unchanged
- [ ] ~80 lines removed

---

### Task 3.3: Migrate SessionPicker to FilterablePicker

**tk:** tk-y9tc (continued)
**Depends on:** Task 3.2
**File:** `src/tui/session_picker.rs`

**Current:** 166 lines
**Target:** ~60 lines

**Acceptance Criteria:**

- [ ] SessionPicker uses FilterablePicker internally
- [ ] Session selection works unchanged
- [ ] ~100 lines removed

---

### Task 3.4: Migrate ModelPicker to FilterablePicker

**tk:** tk-y9tc (continued)
**Depends on:** Task 3.3
**File:** `src/tui/model_picker.rs`

**Note:** ModelPicker is more complex with two-stage selection (provider → model). May need `FilterablePicker` for the model list only, keeping provider selection separate.

**Current:** 404 lines
**Target:** ~200 lines

**Acceptance Criteria:**

- [ ] ModelPicker uses FilterablePicker for model selection
- [ ] Two-stage flow preserved
- [ ] All model picker functionality works
- [ ] ~200 lines removed

---

## Phase 4: Completer Trait Extraction

### Task 4.1: Create Completer trait

**tk:** tk-t46x
**Depends on:** Phase 3 complete
**File:** Extend `src/tui/picker.rs` or new `src/tui/completer.rs`

**Description:**
Two completers share 70% identical code:

- `FileCompleter` (388 lines)
- `CommandCompleter` (272 lines)

Common patterns:

- `active: bool`, `query: String`, `selected: usize`
- `activate()`, `deactivate()`, `set_query()`
- `move_up()`, `move_down()`
- `is_active()`, `selected()`, `visible_candidates()`

**Design:**

```rust
pub trait Completer {
    type Item;

    fn is_active(&self) -> bool;
    fn activate(&mut self);
    fn deactivate(&mut self);
    fn set_query(&mut self, query: &str);
    fn move_up(&mut self);
    fn move_down(&mut self);
    fn selected_index(&self) -> usize;
    fn visible_candidates(&self) -> &[Self::Item];
}

/// Base completer state that can be embedded.
pub struct CompleterState<T> {
    active: bool,
    query: String,
    candidates: Vec<T>,
    filtered: Vec<T>,
    selected: usize,
    max_visible: usize,
}
```

**Acceptance Criteria:**

- [ ] Completer trait defined
- [ ] CompleterState<T> base type created
- [ ] Navigation methods tested

---

### Task 4.2: Migrate CommandCompleter

**tk:** tk-t46x (continued)
**Depends on:** Task 4.1
**File:** `src/tui/command_completer.rs`

**Current:** 272 lines
**Target:** ~150 lines

**Acceptance Criteria:**

- [ ] CommandCompleter uses CompleterState internally
- [ ] Command completion works unchanged
- [ ] ~120 lines removed

---

### Task 4.3: Migrate FileCompleter

**tk:** tk-t46x (continued)
**Depends on:** Task 4.2
**File:** `src/tui/file_completer.rs`

**Note:** FileCompleter has additional complexity (directory scanning, caching). Core completer logic should still use trait.

**Current:** 388 lines
**Target:** ~280 lines

**Acceptance Criteria:**

- [ ] FileCompleter uses CompleterState internally
- [ ] File completion works unchanged
- [ ] ~100 lines removed

---

## Phase 5: App Struct Decomposition

### Task 5.1: Extract TaskState from App

**tk:** tk-4t1f
**Depends on:** Phase 4 complete
**File:** `src/tui/mod.rs`, new `src/tui/app_state.rs`

**Description:**
App has 35+ fields. Group task-related fields:

```rust
/// State for the current agent task.
pub struct TaskState {
    pub task_start_time: Option<Instant>,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub current_tool: Option<String>,
    pub retry_status: Option<String>,
    pub thinking_start: Option<Instant>,
    pub last_thinking_duration: Option<Duration>,
}
```

**Acceptance Criteria:**

- [ ] TaskState struct created
- [ ] App.task: TaskState field added
- [ ] All task-related code updated to use App.task.\*
- [ ] All tests pass

---

### Task 5.2: Extract InteractionState from App

**tk:** tk-4t1f (continued)
**Depends on:** Task 5.1
**File:** `src/tui/mod.rs`, `src/tui/app_state.rs`

```rust
/// State for user interaction tracking.
pub struct InteractionState {
    pub cancel_pending: bool,
    pub esc_pending: bool,
    pub editor_requested: bool,
    pub last_esc_time: Option<Instant>,
    pub last_cancel_time: Option<Instant>,
}
```

**Acceptance Criteria:**

- [ ] InteractionState struct created
- [ ] App.interaction: InteractionState field added
- [ ] All interaction code updated
- [ ] All tests pass

---

## Verification

After each phase:

```bash
cargo test --all-features
cargo clippy -- -W clippy::pedantic
wc -l src/tui/**/*.rs  # Track line reduction
```

## Success Metrics

| Metric                 | Before     | Target                      |
| ---------------------- | ---------- | --------------------------- |
| TUI lines (excl tests) | ~9,300     | ~7,500                      |
| Panic-causing bugs     | 5          | 0                           |
| Picker files           | 3 separate | 1 generic + 3 thin wrappers |
| Completer files        | 2 separate | 1 trait + 2 implementations |
| App fields             | 35+        | 15 + 3 sub-structs          |
