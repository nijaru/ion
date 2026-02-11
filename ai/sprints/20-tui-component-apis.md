# Sprint 20: TUI Component APIs

Goal: Decompose TUI state and establish intuitive, typed component APIs that are easier to reason about and safer to change.

Source: `ai/design/tui-v3-architecture-2026-02.md`

## Tasks

## Task: Decompose App State into Explicit TUI Domains

**Depends on:** none

### Description

Split broad `App`-level TUI fields into explicit domain structs and compose them into a single `TuiState`.

### Acceptance Criteria

- [ ] `InputState`, `ChatState`, `UiState`, `SessionState`, and `RuntimeState` structs exist with clear ownership boundaries.
- [ ] `App` no longer serves as an unstructured state bucket for TUI runtime state.
- [ ] Construction/reset logic remains behavior-compatible with existing session flows.
- [ ] `cargo test -q tui::` passes.

### Technical Notes

Primary files: `src/tui/mod.rs`, `src/tui/app_state.rs`, `src/tui/render_state.rs`, `src/tui/session/*`.

## Task: Introduce Typed ComponentModel Build APIs

**Depends on:** Decompose App State into Explicit TUI Domains

### Description

Define a shared component contract so each renderer builds a typed model from `TuiState` plus `UiLayout`.

### Acceptance Criteria

- [ ] Core components expose `build_model(&TuiState, &UiLayout) -> ComponentModel` style APIs.
- [ ] Components avoid reading unrelated global state.
- [ ] Model construction is side-effect free and unit-testable.
- [ ] `cargo test -q tui::` passes.

### Technical Notes

Primary files: `src/tui/render/mod.rs`, `src/tui/render/*`, `src/tui/render/layout.rs`.

## Task: Isolate Composer Model/Render Boundary

**Depends on:** Introduce Typed ComponentModel Build APIs

### Description

Refactor composer input flow so model build, render, and mutation responsibilities are separated cleanly.

### Acceptance Criteria

- [ ] Input composer rendering no longer mutates unrelated layout/chat tracking state.
- [ ] Composer model build and render stages are independently testable.
- [ ] Multiline grow/shrink path uses explicit inputs/outputs rather than implicit global state reads.
- [ ] `cargo test -q tui::` passes.

### Technical Notes

Primary files: `src/tui/composer/*`, `src/tui/render/input_box.rs`, `src/tui/events.rs`.

## Task: Normalize Popup and Picker Component Boundaries

**Depends on:** Isolate Composer Model/Render Boundary

### Description

Refactor popup/list and picker renderers to use one shared model/build/render contract.

### Acceptance Criteria

- [ ] Popup/history/selector renderers share common row and list rendering helpers.
- [ ] Provider/model/session pickers use the same rendering contract as other popup components.
- [ ] Components consume only the state they declare in their model builders.
- [ ] `cargo test -q tui::` and `cargo clippy -q` pass.

### Technical Notes

Primary files: `src/tui/render/popup.rs`, `src/tui/render/history.rs`, `src/tui/render/selector.rs`, `src/tui/provider_picker.rs`, `src/tui/model_picker.rs`, `src/tui/session_picker.rs`.

## Task: Add State-Boundary and Contract Tests

**Depends on:** Normalize Popup and Picker Component Boundaries

### Description

Add tests that enforce component contract boundaries and guard against accidental broad state coupling.

### Acceptance Criteria

- [ ] New tests validate model builders with fixture `TuiState` and `UiLayout` inputs.
- [ ] Component render methods are testable without terminal I/O.
- [ ] At least one negative test catches invalid cross-component dependency or invariant break.
- [ ] `cargo test -q tui::` passes.

### Technical Notes

Primary files: `src/tui/render/*`, `src/tui/composer/tests.rs`, and new tests in relevant modules.

## Demo

Add a new popup/list-like UI element using existing component APIs in under 100 lines of rendering code without touching unrelated renderer internals.
