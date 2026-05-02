# Ion Roadmap

Updated: 2026-05-02

## Current Focus

I0-I3 are complete. Move into I4 advanced integrations from the cleaned,
green native baseline. Canto stays closed unless Ion evidence proves a
framework-owned defect.

Active umbrella: none. `tk-mmcs` is closed.

## Phases

| Phase | Goal | Status |
| --- | --- | --- |
| I0 | Close dirty baseline and clean AI context | Done |
| I1 | Refactor native core boundaries without behavior drift | Done |
| I2 | Polish minimal TUI/CLI shell for daily use | Done |
| I3 | Restore safety, trust, sandbox, and policy table stakes | Done |
| I4 | Add advanced agent features: subagents, memory, skills, routing, ACP | Active |
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

Status: complete.

Covered:

- transcript readability and markdown rendering quality
- queued input and recall UX
- slash command grouping and completion
- `/resume`, `/session`, `/provider`, `/model`, and `/thinking` clarity
- compact tool display defaults with full-output controls preserved

## I3: Safety, Trust, Sandbox, And Policy

Status: complete.

- review current mode/trust/sandbox/policy behavior against
  `ai/specs/tools-and-modes.md`
- remove stale stabilization-language leftovers from safety paths
- ensure read mode hides or blocks write/execute tools consistently at the
  right boundary (provider-visible request tools and UI tool summaries are now
  mode-filtered)
- restore workspace trust gating around mode changes (`prompt`/`strict` start
  unknown workspaces in READ; `off` disables the gate)
- keep sandbox reporting clear in startup, footer, and `/tools` (sandbox posture
  is now cached in app state and displayed in all three surfaces)
- avoid advanced LLM-judge, escalation, privacy, ACP, subagents, or routing work
  during I3 unless a core safety defect requires it

## I4: Advanced Integrations

Start with the highest-priority ready ACP bridge correctness tasks:

1. Done - `tk-2ffy` - filter/log ACP stderr separately instead of emitting
   `session.Error`.
2. Done - `tk-o0iw` - add initial session context at ACP `Open()`.
3. Done - `tk-6zy3` - map ACP token usage events into Ion usage.

ACP bridge correctness is no longer the active blocker.

1. Done - `tk-h9u6` - evaluate merged edit `edits[]` surface; keep the current
   split until local edit eval evidence proves a merged tool is better.
2. Done - `tk-90ft` - background bash monitor workflow design; keep one
   model-visible `bash` tool and add explicit background job actions later.
3. Next - subagents/context forking or boundary-step steering.
4. Later - typed
   thinking/provider translation.

## I5+ Deferred Work

Do not reopen during the current cleanup/refactor stream unless directly needed
for a core bug:

- ACP bridge polish and Ion-as-ACP-agent mode
- ChatGPT/subscription bridge evaluation
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
