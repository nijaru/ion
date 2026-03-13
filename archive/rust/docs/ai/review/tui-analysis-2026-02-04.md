# TUI Code Analysis (2026-02-04)

## Summary

**Current:** ~16,750 lines
**After refactoring:** ~12,000 lines estimated (28% reduction)
**Minimal viable:** ~8,000 lines (if rebuilt from scratch)

## Line Count Breakdown

| Category                       | Lines      | Tests   | Code Only | Notes                             |
| ------------------------------ | ---------- | ------- | --------- | --------------------------------- |
| Core (run, mod, types, events) | 1,458      | 0       | 1,458     | events.rs is 702 lines (too long) |
| Input handling (composer/)     | 1,656      | 440     | 1,216     | Good abstraction                  |
| Rendering                      | 1,114      | 0       | 1,114     | Reasonable                        |
| Message display                | 1,332      | 0       | 1,332     | Two related files                 |
| Highlighting                   | 859        | 332     | 527       | Reasonable                        |
| Table rendering                | 567        | 0       | 567       | High for feature                  |
| Pickers/Completers             | 1,586      | 130     | 1,456     | Heavy duplication                 |
| Session                        | 784        | 0       | 784       | Reasonable                        |
| Utilities                      | 866        | 0       | 866       | ~120 lines unused                 |
| **Total**                      | **10,222** | **902** | **9,320** | Excl. redundant module copies     |

## Identified Savings

### 1. Picker Trait Abstraction (~600 lines saved)

Three pickers share 80%+ identical code:

- `provider_picker.rs` (133 lines)
- `model_picker.rs` (404 lines)
- `session_picker.rs` (166 lines)

**Current:** 703 lines
**With `FilterablePicker<T>`:** ~200 lines shared + ~100 per picker = ~400 lines
**Savings:** ~300 lines

### 2. Completer Trait Abstraction (~200 lines saved)

Two completers share 70% identical code:

- `file_completer.rs` (388 lines)
- `command_completer.rs` (272 lines)

**Current:** 660 lines
**With `Completer` trait:** ~150 shared + ~200 per completer = ~500 lines
**Savings:** ~160 lines

### 3. Remove Unused Terminal Struct (~120 lines saved)

`terminal.rs` has 462 lines. `Terminal` struct (lines 19-140) is never imported or used.
`StyledLine`, `StyledSpan`, `LineBuilder` are heavily used (184 occurrences).

**Action:** Remove unused `Terminal` struct and methods (lines 19-140).

### 4. Extract Long Functions (no line savings, better maintainability)

| Function                                            | Lines | Issue                           |
| --------------------------------------------------- | ----- | ------------------------------- |
| `events.rs::handle_input_mode`                      | 420   | Too long, multiple concerns     |
| `events.rs::handle_selector_mode`                   | 155   | Could be split by selector type |
| `highlight/markdown.rs::render_markdown_with_width` | 310   | Could extract tag handlers      |
| `chat_renderer.rs::build_lines`                     | 230   | Could extract by sender type    |

### 5. App Struct Decomposition (no line savings, better clarity)

App has 35+ fields. Grouping into sub-structs improves maintainability:

- `TaskState { start_time, tokens, current_tool, ... }`
- `CompletionState { file_completer, command_completer }`
- `InteractionState { cancel_pending, esc_pending, ... }`

## Critical Bugs (must fix)

| Bug             | File:Line            | Risk                       |
| --------------- | -------------------- | -------------------------- |
| Empty vec panic | `visual_lines.rs:74` | Crash on edge case         |
| Array bounds    | `state.rs:549`       | Crash on edge case         |
| Unwrap panic    | `state.rs:588`       | Crash on edge case         |
| Underflow       | `state.rs:261`       | Logic error                |
| No panic hook   | `run.rs`             | Terminal stuck in raw mode |

## Comparison: What's Essential?

For a minimal coding agent TUI:

| Feature               | Lines Est. | Ion Has         |
| --------------------- | ---------- | --------------- |
| Event loop + terminal | 300        | Yes (449 + 462) |
| Multiline input       | 400        | Yes (1,216)     |
| Message display       | 600        | Yes (1,332)     |
| Markdown rendering    | 300        | Yes (527)       |
| Status/progress       | 100        | Yes (in render) |
| **Minimal Total**     | **1,700**  | **3,986**       |

**Optional but valuable:**
| Feature | Lines Est. | Ion Has |
|---------|-----------|---------|
| Syntax highlighting | 200 | Yes (527) |
| Table rendering | 200 | Yes (567 - over-engineered) |
| File autocomplete | 200 | Yes (388) |
| Command autocomplete | 150 | Yes (272) |
| Model/session picker | 300 | Yes (703 - duplicated) |
| Session management | 400 | Yes (784) |

## Recommendations

### Priority 1: Fix Critical Bugs

The 5 panic-causing bugs should be fixed immediately.

### Priority 2: Extract Picker/Completer Traits

Biggest ROI for code reduction (~500 lines) and bug prevention (single source of truth).

### Priority 3: Remove Dead Code

Terminal struct removal is safe and immediate (~120 lines).

### Priority 4: Decompose App Struct

Improves maintainability without line reduction.

### Consider: Table Simplification

567 lines for markdown tables is high. Consider:

- Using a simpler approach for narrow terminals
- Removing complex width distribution algorithm
- Delegating to external crate if tables are rarely used

## Task Tracking

All items tracked in tk:

- `tk-q48p` - Fix visual_lines.rs panic (P2)
- `tk-glcs` - Fix state.rs array accesses (P2)
- `tk-xznp` - Add panic hook (P2)
- `tk-y9tc` - Extract Picker trait (P3)
- `tk-t46x` - Extract Completer trait (P3)
- `tk-4zp4` - Remove unused Terminal struct (P3)
- `tk-4t1f` - Decompose App struct (P3)
- `tk-epd1` - Extract long event handlers (P4)
- `tk-5zjg` - Minor fixes (resize helper, scroll limit, saturating_add) (P4)
