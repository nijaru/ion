# Sprint 17: Dogfood Agent Usability

Goal: Agent behavior is reliable and safe enough to use ion as the primary tool for ion development.

Source: `ai/design/dogfood-readiness-2026-02.md`

## Tasks

## Task: Implement OS Sandbox Execution

**Depends on:** none

### Description

Complete `tk-oh88` so tool execution has stable sandbox behavior aligned with ion permission modes.

### Acceptance Criteria

- [ ] Sandbox behavior matches documented read/write/no-sandbox expectations.
- [ ] Common development commands (build/test/format/search) work in expected modes.
- [ ] Clear error messaging exists for denied operations.
- [ ] `cargo test` and relevant sandbox tests pass.

### Technical Notes

Primary focus is reliability and predictability, not maximum strictness.

## Task: Harden Cancel/Retry/Interrupt Semantics

**Depends on:** Implement OS Sandbox Execution

### Description

Ensure cancellation, retry states, and task completion transitions are robust under rapid user interaction and tool failures.

### Acceptance Criteria

- [ ] Esc cancel path never leaves stale running state.
- [ ] Retry status and completion status reset cleanly between runs.
- [ ] No duplicate or missing terminal state transitions after interrupted runs.
- [ ] `cargo test` passes.

### Technical Notes

Focus files: `src/tui/session/update.rs`, `src/tui/events.rs`, `src/agent/*`.

## Task: Add Failure Tracking Across Compaction

**Depends on:** Harden Cancel/Retry/Interrupt Semantics

### Description

Implement lightweight failure tracking so the agent avoids repeating recent failed patterns after compaction.

### Acceptance Criteria

- [ ] Recent failures are summarized and injected post-compaction.
- [ ] Tracker has bounded size and deterministic eviction.
- [ ] Unit tests cover classification and prompt injection behavior.
- [ ] No major token overhead increase in standard sessions.

### Technical Notes

Follow `ai/review/system-design-evaluation-2026-02.md` guidance; keep scope minimal.

## Task: Improve Session Reliability Under Model/Provider Changes

**Depends on:** Implement OS Sandbox Execution

### Description

Harden session load/save behavior when provider or model differs from current runtime state.

### Acceptance Criteria

- [ ] Session load preserves usable state and emits clear warnings on provider mismatch.
- [ ] Provider/model switches do not corrupt active session state.
- [ ] Session persistence remains stable across multiple turns and restarts.
- [ ] `cargo test` passes.

### Technical Notes

Focus files: `src/tui/session/lifecycle.rs`, `src/tui/session/providers.rs`, `src/session/store.rs`.

## Demo

Use ion for a 2+ hour dev session (multiple tasks, cancels, retries, restarts) with no workflow-blocking failures.
