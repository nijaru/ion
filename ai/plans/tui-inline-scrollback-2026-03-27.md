# TUI Inline Scrollback Refactor (2026-03-27)

## Goal

Make ion behave like Claude Code / Codex / pi in inline mode:

- startup header prints once into native terminal scrollback
- every committed row prints into native terminal scrollback
- Plane B is only the live redraw area

Target running layout:

```text
> ion
ion v0.0.0 • native
~/project • main

<system / user / agent / tool rows in native scrollback>

<streaming messages>

<progress line>
<textarea>
<status line>
```

## Problem

The earlier refactor attempt moved the committed transcript and startup header into the Bubble Tea view. That fixed some ordering bugs but broke the required interaction model:

- native terminal scrollback and terminal search no longer matched the visible transcript
- startup header and committed rows became part of the redraw area
- runtime/system rows could still feel inconsistent because the app was mixing two models

The correct fix is not “app-owned transcript viewport.” The correct fix is “single committed-output path to native scrollback.”

## Required Model

### Native terminal scrollback owns:

- startup header
- resume marker
- replayed prior history
- committed `system`, `user`, `agent`, and `tool` rows

### Plane B owns only:

- in-flight streaming content
- one blank spacer line
- progress line
- textarea
- status line

## Implementation Notes

1. Keep startup lines and replayed entries as one-time print data, not view state.
2. `View()` must not render startup header or committed transcript history.
3. All committed rows should go through one scrollback command path.
4. Configuration warnings (`No provider configured`, `No model configured`) belong on the progress line, not in startup transcript output.
5. Resume flow should print:
   - `--- resumed ---`
   - runtime header
   - workspace/branch line
   - replayed rows
6. Provider picker remains in Plane B:
   - native APIs first
   - subscriptions second
   - human labels
   - aligned details
   - selecting provider immediately opens model picker

## Follow-up

- Keep polishing the copy and spacing within the committed-output path.
- Keep completed main-model rows under the `agent` term in both code and docs.
