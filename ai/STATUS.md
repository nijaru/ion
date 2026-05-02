# Status: ion

Fast, lightweight terminal coding agent.

**Phase:** I0 design/context cleanup and dirty-bundle closeout
**Focus:** Clean the source of truth, then refactor Ion from the full product design.
**Active umbrella:** `tk-mmcs` - core parity plan and queue hygiene.
**Active task:** `tk-l6cj` - AI context consolidation and dirty bundle closeout.
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

## Next Action

1. Finish `ai/` pruning in this pass and commit it as its own green slice.
2. Lightly prune Canto `ai/` so Ion feedback and Canto framework roadmap stay
   clear.
3. Align `tk ready` with the active path.
4. Start `tk-he9p` app shell boundary refactor.

## Active Tasks

- `tk-l6cj` - AI context consolidation and dirty bundle closeout.
- `tk-mmcs` - core parity plan and task queue hygiene.
- `tk-he9p` - refactor app shell boundary files.
- `tk-e2a5` - clean CLI startup and config boundary.
- `tk-eyvq` - refactor CantoBackend lifecycle files.
- `tk-7sna` - split file tools by responsibility.
