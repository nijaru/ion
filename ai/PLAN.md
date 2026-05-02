# Ion Roadmap

Updated: 2026-05-02

## Current Focus

I1 boundary refactors are complete. Move into I2 shell polish from the cleaned
architecture. Canto stays closed unless Ion evidence proves a framework-owned
defect.

Active umbrella: `tk-mmcs`.

## Phases

| Phase | Goal | Status |
| --- | --- | --- |
| I0 | Close dirty baseline and clean AI context | Done |
| I1 | Refactor native core boundaries without behavior drift | Done |
| I2 | Polish minimal TUI/CLI shell for daily use | Active |
| I3 | Restore safety, trust, sandbox, and policy table stakes | Deferred |
| I4 | Add advanced agent features: subagents, memory, skills, routing, ACP | Deferred |
| I5 | Add eval-driven optimization and SOTA experiments | Deferred |

## I0: Dirty Baseline And Context Hygiene

Exit criteria:

- Current verified source bundle is committed in a green slice.
- Root `ai/` has only `README.md`, `STATUS.md`, `DESIGN.md`,
  `DECISIONS.md`, and `PLAN.md`.
- Redundant root roadmap/SOTA/sprint files and stale one-off plan docs are
  deleted after their useful content is distilled.
- `ai/README.md` is a strict index with live links only.
- `tk ready` points at the real next refactor sequence.

## I1: Native Boundary Refactor

Status: complete.

Work in this order:

1. `tk-he9p` - app shell boundary files: command catalog/dispatch, queue and
   progress state, picker helpers, transcript rendering, and settings commands.
2. `tk-e2a5` - CLI startup and config boundary: flag parsing, runtime selection,
   backend construction, and process-local overrides.
3. `tk-7sna` - file tools split: read/write/edit/list responsibilities with
   shared workspace-safe helpers.
4. `tk-eyvq` - CantoBackend lifecycle files: construction/open, submit/cancel,
   event translation, compaction hooks, and deferred-surface guards.

Each slice must preserve behavior unless a concrete bug is found during the
refactor.

## I2: Minimal Shell Polish

Focus now that I1 slices are green:

- transcript readability and markdown rendering quality
- queued input and recall UX
- slash command grouping and completion
- `/resume`, `/session`, `/provider`, `/model`, and `/thinking` clarity
- compact tool display defaults with full-output controls preserved

## I3+ Deferred Work

Do not reopen during the current cleanup/refactor stream unless directly needed
for a core bug:

- ACP bridge polish and Ion-as-ACP-agent mode
- ChatGPT/subscription bridge evaluation
- broad permissions, trust, policy, and sandbox redesign
- subagents, memory/wiki, skills, workflows, model routing
- prompt cache/KV cache experiments
- ripgo integration or merged edit-tool redesign

## Verification Standard

For each code slice:

- focused package tests first
- `go test ./... -count=1 -timeout 300s`
- native race subset:
  `go test -race ./cmd/ion ./internal/app ./internal/backend/canto ./internal/backend/canto/tools ./internal/storage -count=1 -timeout 300s`
- tmux text capture for TUI-affecting changes
- Fedora `local-api/qwen3.6:27b-uncensored` live smoke when reachable;
  OpenRouter DeepSeek Flash is the cheap fallback

Commit each coherent green slice. Do not push unless requested.
