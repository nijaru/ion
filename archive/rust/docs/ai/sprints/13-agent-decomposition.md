# Sprint 13: Agent Loop Decomposition

**Goal:** Break 785-line agent/mod.rs into focused modules with clear responsibilities.
**Status:** PLANNING
**Task:** tk-mmpr

## Current State

```
agent/mod.rs (785 lines)
├── Error helpers (29-106)      - is_retryable_error, categorize_error
├── Agent struct (109-160)      - Core state + new()
├── Builder methods (163-255)   - with_*, activate_skill, clear_plan
├── run_task (261-331)          - Main loop (70 lines)
├── execute_turn (333-393)      - Single turn (60 lines)
├── stream_response (395-450)   - Request building (55 lines)
├── stream_with_retry (454-569) - Streaming + retry (115 lines)
├── complete_with_retry (572-659) - Non-streaming + retry (87 lines)
├── execute_tools_parallel (661-759) - Tool execution (100 lines)
└── AgentEvent (762-785)        - Event enum
```

## Target State

```
agent/
├── mod.rs (~250 lines)    - Agent struct, constructors, run_task, execute_turn
├── events.rs (~30 lines)  - AgentEvent enum
├── retry.rs (~100 lines)  - Error classification, retry logic
├── stream.rs (~200 lines) - stream_response, stream_with_retry, complete_with_retry
├── tools.rs (~110 lines)  - execute_tools_parallel
├── context.rs             - (unchanged)
├── designer.rs            - (unchanged)
├── explorer.rs            - (unchanged)
├── instructions.rs        - (unchanged)
└── subagent.rs            - (unchanged)
```

## Tasks

### Task 1: Extract AgentEvent to events.rs

**Depends on:** none

**Description:** Move `AgentEvent` enum to its own module. Simplest extraction, establishes pattern.

**Files:**

- Create `src/agent/events.rs`
- Update `src/agent/mod.rs`

**Acceptance Criteria:**

- [ ] AgentEvent enum in events.rs
- [ ] Re-exported from mod.rs (`pub use events::AgentEvent`)
- [ ] `cargo build` succeeds
- [ ] `cargo test` passes

**Technical Notes:**

- Lines 762-785 in mod.rs
- No internal dependencies, pure data type

---

### Task 2: Extract retry logic to retry.rs

**Depends on:** none (can parallel with Task 1)

**Description:** Move error classification and retry helpers to dedicated module.

**Files:**

- Create `src/agent/retry.rs`
- Update `src/agent/mod.rs`

**Contents to move:**

- `is_retryable_error()` (lines 29-67)
- `categorize_error()` (lines 71-106)

**Acceptance Criteria:**

- [ ] Both functions in retry.rs
- [ ] Functions are `pub(crate)` (used by stream.rs later)
- [ ] `cargo build` succeeds
- [ ] `cargo test` passes

**Technical Notes:**

- These are pure functions, no state
- Will be used by stream.rs after Task 3

---

### Task 3: Extract tool execution to tools.rs

**Depends on:** Task 1 (needs AgentEvent import path)

**Description:** Move `execute_tools_parallel` to dedicated module.

**Files:**

- Create `src/agent/tools.rs`
- Update `src/agent/mod.rs`

**Acceptance Criteria:**

- [ ] `execute_tools_parallel` in tools.rs
- [ ] Called from `execute_turn` in mod.rs
- [ ] `cargo build` succeeds
- [ ] `cargo test` passes

**Technical Notes:**

- Lines 661-759
- Needs: `AgentEvent`, `ToolContext`, `ContentBlock`, `ToolCallEvent`
- Self-contained async function, takes explicit params

---

### Task 4: Extract streaming to stream.rs

**Depends on:** Task 1, Task 2

**Description:** Move all streaming/completion logic to dedicated module.

**Files:**

- Create `src/agent/stream.rs`
- Update `src/agent/mod.rs`

**Contents to move:**

- `stream_response()` (lines 395-450)
- `stream_with_retry()` (lines 454-569)
- `complete_with_retry()` (lines 572-659)

**Acceptance Criteria:**

- [ ] All three functions in stream.rs
- [ ] Uses retry.rs for error classification
- [ ] Called from `execute_turn` in mod.rs
- [ ] `cargo build` succeeds
- [ ] `cargo test` passes

**Technical Notes:**

- Largest extraction (~200 lines)
- Functions share retry logic pattern
- May refactor retry into shared helper (optional)

---

### Task 5: Clean up mod.rs

**Depends on:** Tasks 1-4

**Description:** Final cleanup of mod.rs - verify structure, add module docs.

**Acceptance Criteria:**

- [ ] mod.rs is ~250 lines or less
- [ ] Clear module structure with doc comments
- [ ] All re-exports in place
- [ ] `cargo clippy` clean
- [ ] `cargo test` passes

---

## Execution Order

```
Task 1 (events.rs) ──┬──> Task 3 (tools.rs) ──┬──> Task 5 (cleanup)
Task 2 (retry.rs) ───┴──> Task 4 (stream.rs) ─┘
```

Tasks 1 & 2 can run in parallel. Tasks 3 & 4 can run in parallel after their deps.

## Validation

After completion:

- [ ] `cargo build` succeeds
- [ ] `cargo test` - all 202 tests pass
- [ ] `cargo clippy` - no new warnings
- [ ] mod.rs ≤ 250 lines
- [ ] Each new module has single responsibility
- [ ] No functionality changes (pure refactor)
