# Ion Roadmap

Updated: 2026-05-02

## Current Focus

I0-I3 are complete. Move into I4 advanced integrations from the cleaned,
green native baseline. Canto stays closed unless Ion evidence proves a
framework-owned defect.

Active umbrella: none. `tk-mmcs` is closed.
Active task: `tk-hz8p` - Subagents: implement explicit context modes before
registration.

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
3. Done - `tk-700w` - define subagent context forking direction; keep default
   registration blocked until explicit `summary`/`fork`/`none` context modes
   and child-session ownership tests exist.
4. Done - `tk-z1kk` - boundary-step steering mode. Queued follow-up remains
   default; `busy_input = "steer"` steers only during active tool calls and
   falls back to queue elsewhere. This also closes `tk-zxgq`.
5. Done - `tk-369n` - typed thinking/provider translation foundation:
   Canto exposes structured reasoning capabilities and Ion filters request
   fields through those capabilities.
6. Done - `tk-t818` - Canto coding primitive adoption audit. Keep Ion-owned
   model-facing wrappers; adopt Canto pieces only where they preserve Ion's
   product tool contract.
7. Done - `tk-g78q` - skills and self-extension boundary plus local browser.
   Keep skills out of the default prompt/toolset; `/skills [query]` lists local
   metadata only; use explicit install, progressive disclosure, and separate
   gates for `read_skill`, `manage_skill`, and marketplace work.
8. Done - `tk-8174` - session branching. `/fork [label]` branches on Canto
   lineage and `/tree` shows lineage plus immediate children. Cross-host
   transfer is split into `tk-4lty`.
9. Done - `tk-4lty` - portable session export/import bundle for cross-host
   transfer. Storage and CLI support are implemented with a subprocess CLI
   smoke. Raw SQLite sync is not exposed as the product surface.
10. Done - `tk-gopd` - external editor handoff. `Ctrl+X` opens the composer in
    `$VISUAL`, `$EDITOR`, or `vi` through Bubble Tea `ExecProcess` and reloads
    edited content back into the composer.
11. Done - `tk-st4q` - Ion as an ACP agent in headless mode. `--agent` runs
    an ACP stdio server that reuses Ion's existing `AgentSession` runtime
    boundary and maps prompt streaming, tool updates, approvals, cancel, and
    session mode updates to ACP.
12. Done - `tk-hfgh` - skills beyond local browsing:
    - Done - first slice: `skill_tools = "read"` opt-in registers
      `read_skill(name)` without adding a prompt inventory or changing the
      default eight-tool surface.
    - Done - safe local install/list CLI: preview by default, explicit
      `--confirm`, staging before install, no remote fetch, no script
      execution, no overwrite.
13. Done - `tk-exeg` - `manage_skill` write gate and undo design:
    - define model-visible actions and non-actions
    - require explicit opt-in, write-capable mode, trusted root, and user
      approval for mutation
    - specify audit/removal/undo behavior before any implementation
    - leave marketplace and self-extension nudges deferred until the write gate
      is boring
    - outcome: spec captured in `ai/specs/instructions-and-skills.md`;
      code implementation is a later slice, not part of this design gate
14. Done - `tk-03hf` - benchmark ripgo search engine replacement:
    - compare current ripgrep-backed behavior against ripgo on semantics first
    - include ignored files, hidden files, `.git` exclusion, cancellation,
      truncation, and large-repo latency before considering replacement
    - outcome: keep rg baseline; ripgo is faster in one small benchmark but
      failed `.git` exclusion parity and lacks a CLI `rg --files` equivalent
15. Done - `tk-n0n4` - privacy PII detection and redaction pipeline:
    - deterministic redaction covers approval/TUI display surfaces from earlier
      slices
    - current closeout adds ACP headless host-display redaction and ACP stderr
      log redaction
    - provider-visible prompt/history redaction remains explicit/future because
      silent mutation would change the task content
16. Done - `tk-aiiz` - protect request cache continuity:
    - identify where provider request cache headers/metadata are composed
    - protect continuity without adding prompt/KV cache machinery
    - outcome: Canto owns the request deep-clone primitive, including tool
      parameter schemas and response schemas; Ion provider-history capture now
      uses that framework clone and imports Canto `62dc906`
17. Done - `tk-a4m1` - evaluate ChatGPT subscription integration path:
    - research current technical and ToS boundaries before implementation
    - keep native API providers and ACP host integration as the current product
      baselines unless evaluation proves a clean supported path
    - outcome: keep ChatGPT/Codex hidden and deferred; no default
      `codex --acp` command; future support would be a Codex app-server bridge
      after a separate design pass
18. Done - `tk-pwsl` - swarm mode alternate-screen operator view:
    - re-evaluate product fit before implementation
    - outcome: keep full alternate-screen swarm mode deferred; current inline
      Plane B subagent rows are the right near-term surface
    - prerequisite for future swarm work is `tk-hz8p`
19. Next - `tk-hz8p` - Subagents: implement explicit context modes before
    registration:
    - add model-visible `summary` / `fork` / `none` context selection to the
      subagent boundary
    - keep child events out of parent provider-visible history except through
      the final returned result
    - cover child replay, parent cancellation, tool-scope allowlists, and
      context snapshot behavior before default registration

## I5+ Deferred Work

Do not reopen during the current cleanup/refactor stream unless directly needed
for a core bug:

- ACP host/client compatibility polish beyond the first headless-agent slice
- ChatGPT/subscription bridge implementation
- memory/wiki, workflows, model routing
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
