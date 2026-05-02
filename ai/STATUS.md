# Status: ion

Fast, lightweight terminal coding agent.

**Phase:** I2 minimal shell polish
**Focus:** Polish daily-use TUI/CLI shell behavior on top of the cleaned native baseline.
**Active umbrella:** `tk-mmcs` - core parity plan and queue hygiene.
**Active task:** `tk-wrci` - tools edit-surface research/design.
**Updated:** 2026-05-02

## Current Truth

- Ion has one native baseline path. The old global stabilization split is gone.
- Current P1 tools are `bash`, `read`, `write`, `edit`, `multi_edit`, `list`,
  `grep`, and `glob`; `verify` is not registered by default.
- Canto owns provider-visible history, durable events, turn execution, retry,
  cancellation settlement, and compaction primitives.
- Ion owns TUI/CLI UX, commands, settings/state, display projection, product
  tools, provider selection, and safety/trust policy.
- Canto is clean and remains framework-focused. Ion is the active repo unless a
  test or smoke proves a Canto-owned defect.

## Latest Evidence

- Source/docs dirty baseline committed as `a571cef`:
  `feat(stabilization): close native shell baseline`.
- Gates before that commit passed:
  `go test ./... -count=1 -timeout 300s` and the native race subset.
- A concrete backend watcher bug was fixed during closeout:
  `TestCrossProviderHandoffPreservesPromptTruth` previously timed out because a
  `translateEvents` goroutine could wait forever on an unstopped watch. Focused
  backend tests and the full suite now pass.
- Prompt-budget and tool-surface research have been distilled into specs and
  decisions. Research files remain evidence, not canonical behavior.
- AI context cleanup is committed in Ion and Canto. Ion root `ai/` now uses the
  five canonical files, and Canto feedback intake is folded into its tracker.
- App shell boundary refactor is complete: command catalog, settings,
  session/cost, rewind, picker, and runtime switching helpers are split out of
  `internal/app/commands.go`.
- Latest app-shell gates passed:
  `go test ./internal/app -count=1 -timeout 120s`,
  `go test ./... -count=1 -timeout 300s`, and
  `go test -race ./cmd/ion ./internal/app ./internal/backend/canto ./internal/backend/canto/tools ./internal/storage -count=1 -timeout 300s`.
- CLI/config boundary refactor is complete: flag registration and normalization
  live in `cmd/ion/flags.go`, runtime selection in `cmd/ion/selection.go`, and
  backend/session opening in `cmd/ion/runtime.go`.
- Latest CLI/config gates passed:
  `go test ./cmd/ion ./internal/config -count=1 -timeout 180s`,
  `go test ./... -count=1 -timeout 300s`, and the native race subset.
- File tools are split by responsibility: common workspace/checkpoint helpers
  remain in `file.go`, with read/write/edit/list implementations in focused
  files.
- Latest file-tool gates passed:
  `go test ./internal/backend/canto/tools -count=1 -timeout 180s`,
  `go test ./... -count=1 -timeout 300s`, and the native race subset.
- CantoBackend lifecycle responsibilities are split out of the previous
  monolithic `internal/backend/canto/backend.go`: metadata, lifecycle,
  providers/retry, turns/cancel, event translation, compaction, policy, memory,
  reasoning, and deferred surfaces now live in focused package files.
- Latest backend-lifecycle gates passed:
  `go test ./internal/backend/canto -count=1 -timeout 180s`,
  `go test ./... -count=1 -timeout 300s`, and the native race subset.
- Markdown rendering now preserves GFM task checkboxes, autolinks,
  strikethrough content, and inline code/autolinks inside rendered tables.
- Latest markdown gates passed:
  `go test ./internal/app -count=1 -timeout 180s`,
  `go test ./... -count=1 -timeout 300s`, and the native race subset.
- Edit-surface design is distilled into `ai/specs/tools-and-modes.md` and
  `ai/DECISIONS.md`: keep `write`, `edit`, and `multi_edit` through I2; defer a
  Pi-style merged `edit(edits[])` until there is eval evidence.

## Next Action

1. Commit and close `tk-wrci`.
2. Pick the next I2 shell polish slice under `tk-mmcs`.
3. Prioritize tmux-visible issues: transcript/progress spacing, compact tool
   output defaults, queued input recall, and slash/session command clarity.

## Active Tasks

- `tk-mmcs` - core parity plan and task queue hygiene.
- `tk-wrci` - tools edit-surface research/design.
