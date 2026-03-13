# Sprint 18: Dogfood Hardening

Goal: Lock in stability with lightweight safeguards and targeted refactors that reduce maintenance risk.

Source: `ai/design/dogfood-readiness-2026-02.md`

## Tasks

## Task: Add Lightweight Non-Visual TUI Smoke Check

**Depends on:** none

### Description

Add a minimal automated smoke check for TUI startup/command/exit behavior without introducing a heavy terminal assertion harness.

### Acceptance Criteria

- [ ] Smoke check runs in CI and catches crash-level regressions.
- [ ] Check is fast and low-maintenance.
- [ ] No new runtime abstractions introduced only for testing.

### Technical Notes

Target only lifecycle correctness (launch, command input, clean shutdown), not pixel/row-perfect rendering.

## Task: Decompose High-Risk TUI Event Paths

**Depends on:** Add Lightweight Non-Visual TUI Smoke Check

### Description

Refactor high-complexity event handling into smaller functions where it improves correctness and readability without changing behavior.

### Acceptance Criteria

- [ ] `handle_input_mode` and selector handling become easier to reason about.
- [ ] Behavior remains unchanged (verified by existing tests and manual checklist).
- [ ] No abstraction layers added unless they reduce concrete complexity.

### Technical Notes

Target local extraction only; avoid broad architecture changes.

## Task: Establish Dogfood Release Gate

**Depends on:** Decompose High-Risk TUI Event Paths

### Description

Define and enforce a simple pre-merge/pre-release stability gate for TUI and agent-critical areas.

### Acceptance Criteria

- [ ] Gate includes required automated tests and manual checklist pass.
- [ ] Gate is documented in `ai/STATUS.md` and/or release docs.
- [ ] Failures create immediate `tk` tasks with reproducible notes.

### Technical Notes

Keep the process lightweight to avoid slowing normal development.

## Demo

Two consecutive weeks of ion-on-ion development with no recurring critical TUI regressions.
