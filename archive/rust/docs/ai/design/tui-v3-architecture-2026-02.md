# TUI v3 Architecture Program (RNK-First, 2026-02)

## Answer First

Yes, `src/tui/` currently has too many files in the wrong shape.

- File count alone is not the problem.
- The real problem is boundary quality: state, event handling, frame planning, and rendering are split by history rather than by responsibility.
- The current structure still carries spike-era seams (`run.rs` + `events.rs` + render methods on `App`) that make resize and scrollback behavior fragile.

This document defines the ideal, idiomatic target architecture for `src/tui/`.

## Architectural Decision

Adopt a **RNK-first two-plane renderer** with strict layering:

1. **Chat plane**: append-only transcript writes, terminal-native wrap/scrollback/search.
2. **UI plane**: bottom-anchored ephemeral rows, RNK-rendered and width-clipped.
3. **Single frame planner**: deterministic plan from state + terminal size.
4. **Single terminal writer**: one place where escape sequences are emitted.

## Core Invariants

1. Exactly one frame planner per tick.
2. Exactly one terminal writer per frame.
3. Chat plane lines are never width-truncated by UI clipping helpers.
4. UI plane lines are always clipped to `width - 1`.
5. Resize never appends/replays the full transcript to scrollback.
6. State mutation and terminal IO are separated (reducer vs runtime executor).

## Target Module Layout

```text
src/tui/
  mod.rs
  run.rs                     # thin entrypoint only

  state/
    mod.rs
    app.rs                   # TuiState root
    chat.rs                  # transcript + chat position machine
    input.rs                 # composer/history-search state
    ui.rs                    # selector/completer/modal state
    task.rs                  # running/retry/tokens/cost timing

  update/
    mod.rs
    action.rs                # normalized events/intents
    reducer.rs               # pure state transitions
    commands.rs              # side effects requested by reducer

  frame/
    mod.rs
    layout.rs                # ui_top + regions
    planner.rs               # FramePlan (pre-ops, chat ops, ui ops)
    ops.rs                   # typed render ops

  render/
    mod.rs
    engine.rs                # only terminal writer for a frame
    line.rs                  # StyledLine/TextStyle + width/display helpers
    rnk_text.rs              # RNK text helpers

    widgets/
      mod.rs
      bottom_ui.rs           # progress/input/status rendering
      selector.rs
      popup.rs
      history_search.rs

    transcript/
      mod.rs
      formatter.rs           # message entry -> logical styled lines
      markdown.rs            # markdown/syntax formatting adapters

  runtime/
    mod.rs
    terminal.rs              # raw mode, sync update, cursor, size
    loop.rs                  # poll/update/plan/render orchestration

  features/
    attachments.rs           # moved from tui root (or lift out of tui entirely)
```

## What Changes From Today

### Files to Keep (mostly as-is)

- `composer/*`
- `render/popup.rs`, `render/selector.rs`, `render/history.rs` (move under `render/widgets/`)
- `rnk_text.rs` + `terminal.rs` concepts (consolidate into `render/line.rs` + `render/rnk_text.rs`)

### Files to Split or Move

- `run.rs` -> `runtime/loop.rs` + `frame/planner.rs` + `runtime/terminal.rs`
- `events.rs` + `input.rs` -> `update/reducer.rs` (+ domain-specific helper modules)
- `chat_renderer.rs` + `highlight/*` -> `render/transcript/*`
- `render/layout.rs` -> `frame/layout.rs`
- `render/direct.rs` -> `render/engine.rs`
- `render_state.rs` -> `state/chat.rs`
- `app_state.rs` + large pieces of `mod.rs` fields -> `state/*`

### Files to Retire (post-migration)

- `render/direct.rs` (replaced by `render/engine.rs`)
- spike-era naming and compatibility shims
- duplicated width helpers spread across render modules

## Chat vs UI Rendering Contract

### Chat Plane

- Accepts logical `StyledLine` records from transcript formatter.
- Writes with newline append semantics; no UI clipping pass.
- Allows terminal-native soft-wrap and scrollback behavior.
- Resize behavior: recompute wraps from canonical transcript and repaint the visible viewport in place (no scrollback append).

### UI Plane

- Fully recomputed each frame from layout + state.
- Always row-addressed and row-cleared.
- Always width-clipped (`width - 1`) to prevent right-edge autowrap drift.

## Resize Semantics (Required)

On terminal resize:

1. Update terminal dimensions in runtime.
2. Recompute wrapped transcript lines from canonical message entries.
3. Recompute layout and visible viewport tail.
4. Repaint visible chat viewport rows in place (row-addressed writes, no newline append).
5. Repaint UI plane.
6. Do not append/reprint full transcript history.

## Why This Is Idiomatic Rust

- Pure reducer and planner functions are easy to unit test.
- Side effects are explicit via command enums.
- Renderer owns IO; domain code does not write to terminal.
- Strong enums (`Action`, `FrameOp`, `ChatPosition`) replace ad-hoc flags.

## Migration Plan

### Phase 1: Structural Cut (no behavior change)

- Introduce `state/`, `update/`, `frame/`, `runtime/`, `render/widgets/`, `render/transcript/`.
- Move files with minimal edits and keep behavior parity.
- Remove spike naming.

### Phase 2: Reducer + Planner Isolation

- Move event branching out of `impl App` methods into reducer/actions.
- Move pre-op/chat-op math from `run.rs` into `frame/planner.rs`.

### Phase 3: Render Engine Unification

- Replace `draw_direct` scatter with single `render::engine` entry.
- Enforce chat-plane vs UI-plane write contracts.

### Phase 4: Transcript/Formatting Cleanup

- Split `message_list.rs` into transcript model vs tool-display helpers.
- Split markdown/diff/syntax formatting into `render/transcript` adapters.

### Phase 5: Hardening

- Add PTY regression tests for resize/multi-monitor movements.
- Add invariants/assertions for planner transitions.

## Acceptance Criteria

1. No duplicate transcript blocks under repeated resize/monitor moves.
2. No growth of blank spacer rows caused by resize churn.
3. No word truncation in chat transcript after resize.
4. Selector/completer/history-search rows always clear correctly.
5. `cargo test` + PTY smoke tests pass.

## Explicit Non-Goals

- Reintroducing ratatui.
- Maintaining dual renderer paths.
- Keeping legacy compatibility shims once migration commits land.
