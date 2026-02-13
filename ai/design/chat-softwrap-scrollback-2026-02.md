# Chat Soft-Wrap + Viewport Separation (2026-02)

## Decision

Adopt a two-plane TUI model regardless of RNK choice:

1. Chat history is append-only and soft-wrapped by the terminal (no width-baked hard-wrap lines).
2. Bottom UI is ephemeral and bottom-anchored (recomputed each frame).
3. Resize should reflow from canonical transcript state but repaint only the visible viewport (never append a second full transcript block).

## Why

| Goal | Benefit |
| --- | --- |
| Native scrollback/search | Terminal owns wrapping and search text semantics |
| Resize stability | Avoid duplicate-history artifacts from width reflow |
| Simpler mental model | Chat is immutable history, UI is mutable overlay |
| RNK optionality | Same architecture works with crossterm-only or RNK bottom/UI |

## Constraints

1. Do not rewrite old terminal scrollback on width change.
2. Keep streaming smooth with minimal line churn.
3. Preserve existing startup/header/session-resume behavior.
4. Keep selector/popup/input growth from corrupting history rows.

## Architecture

### Plane A: Chat History (append-only)

- Emit logical lines only (no renderer-level hard-wrap based on terminal width).
- Append new user/assistant/tool lines to terminal history.
- Streaming writes append committed delta lines; mutable tail is limited to active in-viewport content.

### Plane B: Bottom UI (ephemeral)

- Compute `ui_top` from terminal height and bottom-ui content height each frame.
- Clear/redraw only UI-plane rows.
- UI-plane includes progress, input box, status, selector, completers, history-search prompt.

## Resize Model

On resize:

1. Recompute layout (`ui_top`, component heights).
2. Rebuild wrapped chat lines from canonical message entries at the new width.
3. Repaint the visible chat viewport rows in place (absolute-row writes, no newline append).
4. Repaint UI plane.
5. Do not rewrite/append historical scrollback.
6. If tracked chat rows would be overlapped by UI growth, transition to scrolling mode and scroll just enough to preserve separation.

## Streaming Model

1. Keep incremental append for stable committed lines.
2. Keep a minimal volatile tail for unfinished content where wrapping/markdown closure can still change.
3. Never trigger whole-history rebuild during stream updates.

## Implementation Plan

| Phase | Scope | Exit Criteria |
| --- | --- | --- |
| P1 | Make resize reflow explicit: canonical transcript rewrap + in-place viewport repaint (no newline append writes) | Resize no longer duplicates chat history and markdown wraps update correctly |
| P2 | Convert chat renderer output to logical-line emission (soft-wrap-friendly) and narrow mutable tail semantics | Streaming stable across resize and long outputs |
| P3 | Replace overlap-triggered "full reflow" with position-state transition/scroll arithmetic | Input/popup growth does not erase or duplicate chat |
| P4 | Expand manual checks + add targeted unit tests for new planner transitions | Checklist passes in Ghostty narrow/resize stress |

## Required Tests

1. `--continue` startup with long history, then rapid width changes.
2. Streaming response while resizing wider/narrower repeatedly.
3. Multiline input growth/shrink during idle and during stream.
4. Selector open/close + completer open/close with accumulated chat.
5. `/clear`, `/resume`, cancel, and editor suspend/resume after resize.

## Keep/Kill Criteria

Keep this architecture if:

- Native terminal search finds historical lines after repeated resize.
- No duplicate-history or blank-gap regressions across checklist.
- No whole-history redraw required for routine resize.

Kill/revisit if:

- Terminal-specific wrapping behavior makes tool/markdown output unreadable without hard-wrap.
- Streaming tail mutation causes persistent visual corruption across major terminals.

## Task Link

- `tk-bcau` - `[ARCH] Soft-wrap chat history + bottom-ui viewport separation`
