# Sprint 21: TUI Frame Planner and Reflow

Goal: Replace ad hoc render branching with a deterministic frame planner so resize, reflow, clear, and multiline composer flows remain correct.

Source: `ai/design/tui-v3-architecture-2026-02.md`

## Tasks

## Task: Replace Position Flags with ChatPosition State Machine

**Depends on:** none

### Description

Migrate render-position tracking to a single typed `ChatPosition` enum that encodes valid chat/UI placement modes.

### Acceptance Criteria

- [ ] `chat_row`, `startup_ui_anchor`, `last_ui_start`, and `header_inserted` ad hoc combinations are removed or fully wrapped behind `ChatPosition`.
- [ ] State transitions for startup, first message, overflow, clear, and session load are explicit.
- [ ] Unit tests cover all legal transitions and reject invalid ones.
- [ ] `cargo test -q tui::render_state` passes.

### Technical Notes

Primary files: `src/tui/render_state.rs`, `src/tui/run.rs`.

## Task: Implement FramePlan Builder with Single Layout Pass

**Depends on:** Replace Position Flags with ChatPosition State Machine

### Description

Build a per-frame planning stage that computes layout once and outputs planned pre-ops, chat insertion behavior, and bottom UI draw operations.

### Acceptance Criteria

- [ ] One `compute_layout` call is used per frame.
- [ ] Frame planning occurs before terminal writes.
- [ ] Planned operations include screen clear, reflow, selector clear, header insertion/clear, and chat insert modes.
- [ ] `cargo test -q tui::` passes.

### Technical Notes

Primary files: `src/tui/run.rs`, `src/tui/render/layout.rs`, `src/tui/render/direct.rs`.

## Task: Execute FramePlan Atomically Through RenderEngine

**Depends on:** Implement FramePlan Builder with Single Layout Pass

### Description

Execute `FramePlan` in a strict order through `RenderEngine`, then apply post-render bookkeeping as a separate step.

### Acceptance Criteria

- [ ] Decision logic and terminal I/O are no longer interleaved.
- [ ] Post-render state updates happen only after planned operations complete.
- [ ] Rapid resize + streaming + multiline input no longer produce duplicate history/progress lines.
- [ ] `cargo test -q tui::` and manual checklist resize/composer cases pass.

### Technical Notes

Primary files: `src/tui/run.rs`, `src/tui/render/direct.rs`, `src/tui/events.rs`.

## Task: Add Regression Tests for Known Corruption Cases

**Depends on:** Execute FramePlan Atomically Through RenderEngine

### Description

Encode known regressions from dogfood runs into deterministic tests so they do not reappear.

### Acceptance Criteria

- [ ] Tests cover narrow resize wrap integrity and no duplicated redraw artifacts.
- [ ] Tests cover multiline composer grow/shrink preserving history region.
- [ ] Tests cover paste placeholder numbering monotonicity across clears.
- [ ] `cargo test -q tui::` passes.

### Technical Notes

Primary files: `src/tui/composer/tests.rs`, `src/tui/run.rs`, `src/tui/render_state.rs`, plus focused PTY smoke tests if needed.

## Demo

Run the manual checklist in Ghostty with repeated narrow/wide resizes and multiline composer edits; chat history and bottom UI stay consistent with no duplicate redraw artifacts.
