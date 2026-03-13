# Sprint 22: TUI Perf and Regression Gates

Goal: Lock in TUI correctness with measurable performance budgets and lightweight regression gates that run continuously.

Source: `ai/design/tui-v3-architecture-2026-02.md`

## Tasks

## Task: Add Frame-Time and Allocation Telemetry

**Depends on:** none

### Description

Instrument frame lifecycle points to measure planning/render costs and detect allocation churn in steady-state usage.

### Acceptance Criteria

- [ ] Frame telemetry captures p50/p95 planning and render timing in debug/diagnostic mode.
- [ ] Allocation trend metrics are captured for steady-state streaming and idle states.
- [ ] Telemetry overhead is negligible when disabled.
- [ ] `cargo test -q tui::` passes.

### Technical Notes

Primary files: `src/tui/run.rs`, `src/tui/render/direct.rs`, `src/tui/terminal.rs`.

## Task: Reuse Row Buffers to Bound Per-Frame Allocation

**Depends on:** Add Frame-Time and Allocation Telemetry

### Description

Refactor render paths to reuse row/model buffers across frames and prevent unbounded growth under long sessions.

### Acceptance Criteria

- [ ] Core renderer buffers are reused rather than repeatedly reallocated each frame.
- [ ] No monotonic memory growth across a 30-minute streaming dogfood run.
- [ ] Streaming frame p95 CPU stays under target from architecture doc.
- [ ] `cargo test -q tui::` and `cargo clippy -q` pass.

### Technical Notes

Primary files: `src/tui/render/*`, `src/tui/chat_renderer.rs`, `src/tui/message_list.rs`.

## Task: Add Deterministic Non-Visual TUI Smoke Suite

**Depends on:** Reuse Row Buffers to Bound Per-Frame Allocation

### Description

Create a compact PTY-driven smoke suite that validates lifecycle and resize/composer correctness without brittle snapshot assertions.

### Acceptance Criteria

- [ ] CI smoke suite covers startup, submit, cancel, resize, multiline composer grow/shrink, and clean exit.
- [ ] Tests assert lifecycle and state invariants, not row-perfect visuals.
- [ ] Suite runtime remains low enough for pre-merge usage.
- [ ] `cargo test` includes the smoke suite in CI mode.

### Technical Notes

Primary files: new TUI smoke test module(s) under `tests/` or `src/tui/` with harness utilities.

## Task: Define and Enforce TUI Release Gate

**Depends on:** Add Deterministic Non-Visual TUI Smoke Suite

### Description

Document and enforce a release gate that combines automated smoke checks, targeted unit tests, and manual checklist verification.

### Acceptance Criteria

- [ ] Gate definition is documented in `ai/STATUS.md` and/or release docs.
- [ ] Gate includes pass criteria for automated suite + manual checklist.
- [ ] Any gate failure requires a linked `tk` bug task before merge/release.
- [ ] Team can run the gate in under 15 minutes.

### Technical Notes

Keep the gate minimal and repeatable; avoid adding heavyweight visual diff infrastructure.

## Demo

Complete one full week of ion-on-ion development with the gate enabled and no recurring high-severity TUI rendering regressions.
