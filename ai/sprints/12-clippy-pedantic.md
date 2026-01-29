# Sprint 12: Clippy Pedantic Refactoring

**Goal:** Fix remaining clippy pedantic warnings that improve code quality
**Source:** `cargo clippy -- -W clippy::pedantic` (139 warnings remaining)
**Status:** PENDING

## Analysis

| Category                | Count | Decision                              |
| ----------------------- | ----- | ------------------------------------- |
| Missing `# Errors` docs | 56    | **Skip** - docs task, low ROI         |
| Missing `# Panics` docs | 4     | **Skip** - docs task                  |
| Functions too long      | 11    | **Fix** - split largest, allow others |
| Unused self argument    | 4     | **Fix** - make associated functions   |
| Casting warnings        | ~30   | **Allow** - intentional for TUI       |
| More than 3 bools       | 3     | **Allow** - CLI/TUI state             |
| Wildcard single variant | 4     | **Allow** - defensive coding          |
| Other style             | ~27   | **Case-by-case**                      |

## Demoable Outcomes

- [ ] `cargo clippy -- -W clippy::pedantic` < 100 warnings
- [ ] Largest functions split (>200 lines)
- [ ] Unused self converted to associated functions
- [ ] `#[allow]` attributes added for intentional patterns

---

## Task: S12-1 Fix unused self arguments

**Sprint:** 12
**Depends on:** none
**Status:** PENDING
**Effort:** Quick (15 min)

### Description

Convert methods with unused `self` to associated functions.

### Locations

| File                 | Method                 | Line |
| -------------------- | ---------------------- | ---- |
| provider/registry.rs | `sort_models`          | 382  |
| tui/input.rs         | `startup_header_lines` | 46   |
| tui/render.rs        | `progress_height`      | 22   |
| tui/table.rs         | `render_border`        | 185  |

### Implementation

For each method:

1. Remove `&self` parameter
2. Change `self.method()` calls to `Self::method()` or `StructName::method()`
3. If method needs instance data, add explicit parameter

### Acceptance Criteria

- [ ] All 4 methods converted or have explicit `#[allow]` with reason
- [ ] `cargo build` passes
- [ ] `cargo test` passes

---

## Task: S12-2 Split run_tui function

**Sprint:** 12
**Depends on:** none
**Status:** PENDING
**Effort:** Medium (30 min)

### Description

Split `run_tui()` in main.rs (208 lines) into focused functions.

### Current Structure

```
run_tui() - 208 lines
├── Terminal setup (30 lines)
├── App creation and resume handling (50 lines)
├── Main event loop (100 lines)
└── Cleanup (28 lines)
```

### Target Structure

```rust
async fn run_tui(...) -> Result<()> {
    let mut terminal = setup_terminal()?;
    let mut app = create_app_with_resume(...)?;
    run_event_loop(&mut app, &mut terminal).await?;
    cleanup_terminal(&mut terminal)?;
    Ok(())
}
```

### Acceptance Criteria

- [ ] `setup_terminal()` extracts terminal init
- [ ] `create_app_with_resume()` handles app creation + resume logic
- [ ] Main loop stays in `run_tui()` (complex control flow)
- [ ] `cleanup_terminal()` extracts cleanup
- [ ] Each function < 100 lines
- [ ] No behavior change

---

## Task: S12-3 Split run_inner function (CLI)

**Sprint:** 12
**Depends on:** none
**Status:** PENDING
**Effort:** Medium (30 min)

### Description

Split `run_inner()` in cli.rs (210 lines) into focused functions.

### Current Structure

```
run_inner() - 210 lines
├── Config/provider setup (80 lines)
├── Agent creation (30 lines)
├── Event handling loop (80 lines)
└── Output/exit code (20 lines)
```

### Target Structure

```rust
async fn run_inner(args: RunArgs, auto_approve: bool) -> Result<ExitCode> {
    let (agent, session, abort_token) = setup_cli_agent(&args, auto_approve).await?;
    let (response, interrupted) = run_cli_agent(agent, session, &args, abort_token).await?;
    output_result(&response, &args, interrupted)
}
```

### Acceptance Criteria

- [ ] `setup_cli_agent()` extracts config/provider/agent setup
- [ ] `run_cli_agent()` handles event loop
- [ ] `output_result()` handles final output formatting
- [ ] Each function < 100 lines
- [ ] No behavior change

---

## Task: S12-4 Add #[allow] for intentional patterns

**Sprint:** 12
**Depends on:** none
**Status:** PENDING
**Effort:** Quick (20 min)

### Description

Add `#[allow(clippy::...)]` attributes with documentation for intentional patterns.

### Patterns to Allow

1. **Casting warnings in TUI code** - Terminal APIs require u16
   - Add file-level `#![allow(clippy::cast_possible_truncation)]` to render.rs, events.rs

2. **More than 3 bools** - State structs need flags
   - Add `#[allow(clippy::struct_excessive_bools)]` to App, Cli, ModelInfo

3. **Wildcard single variant** - Defensive for future variants
   - Add `#[allow(clippy::match_wildcard_for_single_variants)]` at match sites

### Format

```rust
#[allow(clippy::cast_possible_truncation)] // Terminal APIs require u16
let line_count = chat_lines.len() as u16;
```

### Acceptance Criteria

- [ ] All intentional patterns have `#[allow]` with comment
- [ ] No blanket allows at crate level
- [ ] `cargo clippy -- -W clippy::pedantic` shows only docs warnings

---

## Task: S12-5 Review large TUI functions

**Sprint:** 12
**Depends on:** S12-1, S12-2, S12-3, S12-4
**Status:** PENDING
**Effort:** Low (20 min)

### Description

Review remaining large functions and decide: split or allow.

### Functions to Review

| File                      | Function               | Lines | Decision                           |
| ------------------------- | ---------------------- | ----- | ---------------------------------- |
| tui/events.rs             | `handle_event`         | 217   | Review - may be inherently complex |
| tui/events.rs             | `handle_selector_mode` | 157   | Review                             |
| tui/render.rs             | `draw_direct`          | 208   | Review                             |
| tui/session.rs            | `update`               | 182   | Review                             |
| tui/session.rs            | `handle_agent_event`   | 135   | Review                             |
| tui/highlight.rs          | `render_markdown`      | 235   | Review                             |
| tui/chat_renderer.rs      | `render_entry`         | 203   | Review                             |
| tool/builtin/edit.rs      | `execute`              | 102   | Allow - near threshold             |
| tool/builtin/web_fetch.rs | `execute`              | 125   | Allow - near threshold             |

### Decision Criteria

- **Split if:** Clear logical sections, reusable parts
- **Allow if:** Complex control flow, tightly coupled logic

### Acceptance Criteria

- [ ] Each function reviewed
- [ ] Decision documented (split or allow with reason)
- [ ] Functions > 150 lines either split or have `#[allow]` with reason
- [ ] No new `too_many_lines` warnings without justification

---

## Task: S12-6 Final verification

**Sprint:** 12
**Depends on:** S12-1 through S12-5
**Status:** PENDING

### Description

Verify all changes and document final state.

### Checklist

- [ ] `cargo build` passes
- [ ] `cargo test` passes (122 tests)
- [ ] `cargo clippy -- -W clippy::pedantic` < 100 warnings
- [ ] All warnings are either:
  - Fixed
  - Allowed with documented reason
  - Documentation (# Errors, # Panics) - deferred
- [ ] ai/STATUS.md updated
- [ ] Commit changes

### Acceptance Criteria

- [ ] Warning count documented
- [ ] Remaining warnings categorized
- [ ] STATUS.md reflects current state
