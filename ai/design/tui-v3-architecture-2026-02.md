# TUI v3 Architecture Program (2026-02)

## Objective

Create a TUI architecture that is:

- Correct under resize, reflow, streaming, and long sessions
- High performance with stable frame times and low allocation churn
- Intuitive to modify with small, explicit renderer APIs
- Resistant to autowrap/scrollback corruption bugs

This document defines the target structure and migration plan.

## Scope

### In Scope

- `src/tui/*` runtime architecture and renderer boundaries
- Render pipeline invariants and width-safe output guarantees
- State ownership and modular APIs for components/widgets
- Test strategy for correctness and performance regressions

### Out of Scope

- Product-surface feature expansion unrelated to TUI architecture
- Moving away from `crossterm`
- Replacing existing provider/agent architecture

## Core Design

### 1) Single Render Authority

All terminal writes must flow through one rendering surface:

- `RenderEngine` owns frame composition
- Components return typed draw data, not direct terminal writes
- `RenderEngine` enforces row clipping and ordering

No component may call terminal output APIs directly after migration.

### 2) Deterministic Frame Pipeline

Every frame follows a strict pipeline:

1. Poll input/events
2. Reduce events into `TuiState` mutations
3. Build `FrameModel` from state and terminal size
4. Render `FrameModel` via `RenderEngine`
5. Commit post-render bookkeeping

This avoids mixed state/read/write ordering bugs.

### 3) Width-Safe Rendering Contract

All rendered rows must satisfy:

- Display width <= `terminal_width - 1`
- No renderer emits raw unbounded strings
- Unicode width rules are used for clipping and cursor placement

Central helpers are required for:

- display width measurement
- clipping/truncation
- row padding
- cursor clamping

### 4) State Decomposition

Split `App` responsibilities into explicit domains:

- `InputState`: composer buffer, cursor, scroll, history search
- `ChatState`: message list, streaming progress, viewport anchors
- `UiState`: mode, selector/completer state, status/progress model
- `SessionState`: provider/model/session metadata
- `RuntimeState`: running/cancel/retry/task lifecycle flags

Use `TuiState` as a composed root object.

### 5) Typed Component APIs

Each component should expose:

- `fn build_model(&TuiState, &UiLayout) -> ComponentModel`
- `fn render(&ComponentModel, &mut RowBuffer)`

No component should read unrelated global state.

### 6) Chat + Bottom UI Coordination

The chat/scrollback and bottom UI relationship remains hybrid, but formalized:

- Chat insertion strategy is planned from one `ChatPosition` state machine
- Bottom UI anchor is derived from a single layout computation per frame
- Reflow is explicit and idempotent for any terminal resize

## Invariants

1. Exactly one layout computation per frame.
2. Exactly one terminal writer per frame.
3. No unbounded row writes.
4. Reflow paths and incremental paths share the same clipping helpers.
5. Component renderers are side-effect free outside their row buffers.
6. Mode switches cannot bypass clear/repaint sequencing rules.

## Performance Targets

| Metric | Target |
| --- | --- |
| Idle frame CPU | < 1.5 ms on M3 Max |
| Active streaming frame CPU | < 5 ms p95 |
| Heap allocations per frame (steady-state) | bounded and non-growing |
| Reflow on resize | no visible corruption, no panic |

## Correctness Strategy

### Unit-Level

- width-clipping/property tests
- chat position transition tests
- frame planning tests (given state + size -> expected ops)

### Integration-Level

- PTY smoke tests for startup, resize, clear, resume, selector, multiline input
- deterministic regression tests for previously observed corruption cases

### Manual Gate

- Keep and expand `ai/review/tui-manual-checklist-2026-02.md`
- Require narrow-width and rapid-resize checks for TUI-touching changes

## API Ergonomics Rules

1. Renderer functions accept explicit typed models, not broad `App` references.
2. Helper names describe rendering constraints (`clip_row`, `pad_row_to_width`, `safe_cursor`).
3. Shared behavior (popup/list rows/filter prompt) is centralized, not duplicated.
4. New render paths require tests or checklist updates in the same change.

## Migration Plan

| Phase | Outcome |
| --- | --- |
| Phase 1 | Render safety primitives and width-safe enforcement everywhere |
| Phase 2 | App state decomposition and component API boundaries |
| Phase 3 | Unified frame planner + chat/UI coordination hardening |
| Phase 4 | Performance + observability budgets and release gates |

This maps to sprint files `19` through `22`.

## Risks

1. Mid-migration mixed render paths can reintroduce ordering bugs.
2. Over-abstracting renderer traits can reduce velocity.
3. PTY tests can become brittle if they assert visual details too tightly.

## Risk Controls

1. Keep each migration step shippable and demoable.
2. Prefer structural refactors with behavior parity before behavior changes.
3. Assert invariants at boundaries (debug asserts + targeted tests).
4. Keep visual tests focused on lifecycle correctness, not pixel snapshots.

