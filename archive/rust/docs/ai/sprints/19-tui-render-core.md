# Sprint 19: TUI Render Core

Goal: Build a single-authority render core with width-safe row primitives so resize and redraw corruption cannot recur.

Source: `ai/design/tui-v3-architecture-2026-02.md`

## Tasks

## Task: Introduce Width-Safe Row Primitives

**Depends on:** none

### Description

Create one shared row-safety module that owns display-width measurement, clipping, padding, and cursor clamping rules.

### Acceptance Criteria

- [ ] One reusable API exists for `display_width`, `clip_row`, `pad_row_to_width`, and safe cursor X clamping.
- [ ] No renderer slices strings by byte index for visible width control.
- [ ] Unit tests cover ASCII, wide Unicode glyphs, combining marks, and zero-width edge cases.
- [ ] `cargo test -q tui::` passes.

### Technical Notes

Primary files: `src/tui/util.rs`, `src/tui/render/widgets.rs`, and any new helper module under `src/tui/render/`.

## Task: Introduce RenderEngine as the Single Terminal Writer

**Depends on:** Introduce Width-Safe Row Primitives

### Description

Add a `RenderEngine` boundary that owns terminal write ordering and synchronized update lifecycle.

### Acceptance Criteria

- [ ] Bottom UI rendering paths write through `RenderEngine` methods instead of ad hoc direct terminal calls.
- [ ] Begin/end synchronized update is managed in one place.
- [ ] Existing baseline behavior is preserved on standard-width terminals.
- [ ] `cargo test -q tui::` passes.

### Technical Notes

Primary files: `src/tui/render/direct.rs`, `src/tui/run.rs`, `src/tui/terminal.rs`.

## Task: Convert Status/Progress/Input Renderers to RowBuffer Output

**Depends on:** Introduce RenderEngine as the Single Terminal Writer

### Description

Refactor high-frequency bottom-UI renderers to return typed row data consumed by `RenderEngine` instead of issuing terminal writes directly.

### Acceptance Criteria

- [ ] `status`, `progress`, and `input_box` rendering paths produce row buffers/models.
- [ ] Converted rows are width-clamped before write.
- [ ] Manual check at widths 141, 100, 80, and 60 shows no autowrap corruption for status/progress/input rows.
- [ ] `cargo test -q tui::` and `cargo clippy -q` pass.

### Technical Notes

Primary files: `src/tui/render/status.rs`, `src/tui/render/progress.rs`, `src/tui/render/input_box.rs`, `src/tui/render/direct.rs`.

## Task: Convert Popup/History/Selector Renderers to RowBuffer Output

**Depends on:** Convert Status/Progress/Input Renderers to RowBuffer Output

### Description

Refactor popup/list renderers to follow the same row-buffer contract and shared width-safe helpers.

### Acceptance Criteria

- [ ] `popup`, `history`, and `selector` paths return row models consumed by `RenderEngine`.
- [ ] Shared list-row helper is used for truncation/padding/highlight behavior.
- [ ] Narrow-width list rendering avoids duplicated redraw/autowrap artifacts.
- [ ] `cargo test -q tui::` and `cargo clippy -q` pass.

### Technical Notes

Primary files: `src/tui/render/popup.rs`, `src/tui/render/history.rs`, `src/tui/render/selector.rs`, `src/tui/render/widgets.rs`.

## Task: Add Render Invariant Guards

**Depends on:** Convert Popup/History/Selector Renderers to RowBuffer Output

### Description

Enforce render invariants at the write boundary so oversized rows or illegal cursor coordinates fail fast in debug and degrade safely in release.

### Acceptance Criteria

- [ ] Debug builds assert on invalid row width or cursor positions before terminal write.
- [ ] Release builds clamp invalid rows/coordinates safely without panic.
- [ ] Narrow resize stress run (rapid widen/shrink) shows no duplicated bottom-UI lines.
- [ ] `cargo test -q tui::` passes.

### Technical Notes

Primary files: `src/tui/render/direct.rs`, `src/tui/terminal.rs`, `src/tui/render/layout.rs`.

## Demo

Run ion, rapidly resize between narrow and wide widths while streaming responses, and confirm no truncated/autowrapped bottom-UI rows or redraw duplication.
