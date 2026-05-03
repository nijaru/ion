# Ion Roadmap

Updated: 2026-05-03

## Current Focus

The active priority is not more feature parity. It is the harness-boundary
refactor after the minimal-core consolidation pass: make Ion a thin product
host over one Canto runtime/session stream while keeping product policy in Ion.

Next task: reassess after final C2 smoke.

Flue, Pi, OpenAI Agents SDK, and Mendral stay in the plan as architecture
constraints, not implementation scope. They are useful because they clarify
where the harness boundary belongs; they do not justify adding new tools,
remote sandboxes, memory namespaces, or multi-agent features before the default
core is solid.

## Phases

| Phase | Goal | Status |
| --- | --- | --- |
| I0 | Close dirty baseline and clean AI context | Done |
| I1 | Refactor native core boundaries without behavior drift | Done |
| I2 | Polish minimal TUI/CLI shell for daily use | Done |
| I3 | Restore safety, trust, sandbox, and policy table stakes | Implemented, not active |
| I4 | Add advanced agent features: subagents, memory, skills, routing, ACP | Implemented/deferred, not active |
| C0 | Minimal native core consolidation | Done |
| C1 | Refactor around a clear headless harness boundary | Done |
| C2 | Refactor local execution around the executor boundary | Active |
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
19. Done - `tk-hz8p` - Subagents: implement explicit context modes before
    registration:
    - added model-visible `summary` / `fork` / `none` context selection to the
      subagent boundary
    - keeps child events out of parent provider-visible history except through
      the final returned result
    - covers schema mapping, none-mode context rejection, and fork-mode child
      provider-visible history when the parent has an in-flight tool call
20. Done - `tk-29xj` - Subagents: expose gated subagent tool after
    context-mode smoke:
    - `subagent_tools = "on"` is the explicit opt-in registration path
    - the default tool surface remains the eight core coding tools
    - READ mode hides `subagent`; EDIT and AUTO use the normal sensitive-tool
      boundary
    - built-in personas use only registered Ion tools
    - fast-slot personas fall back to the primary model when no fast model is
      configured
    - deterministic smoke proves a child result returns to the parent without
      leaking parent prompt context into `none` mode
21. Done - `tk-6prx` - Markdown: Cache goldmark parser instance:
    - cached the Goldmark/GFM renderer instead of rebuilding it for each
      markdown render
    - focused app tests passed
22. Done - `tk-w5uj` - TUI: Status bar shows git diff stats (+N/-N):
    - status line shows best-effort cached `+N/-M` workspace diff stats
    - stats load on startup and refresh after completed turns, not on every
      render
23. Done - `tk-lya7` - TUI: Token usage color by percentage (green/yellow/red):
    - context usage keeps the same text but renders green below 50%, yellow at
      50-79%, and red at 80%+
24. Done - `tk-lggk` - TUI: AskUser tool - interactive question prompt:
    - no Ion-only `ask_user` tool added
    - models can ask normal assistant questions today
    - future implementation belongs behind a Canto elicitation/pause-resume
      primitive with explicit TUI/CLI/ACP behavior
25. Done - `tk-ritc` - Evaluate collapsing internal/storage wrapper into canto:
    - keep `internal/storage` as Ion's app adapter over Canto
    - Canto owns reusable event, ancestry, and effective-history primitives
    - Ion keeps cwd/branch/model indexes, input history, lazy materialization,
      TUI replay projection, and portable bundle UX

## C0: Minimal Native Core Consolidation

Status: done.

Goal: make the default path easy to explain and hard to break:

```text
TUI/CLI input -> AgentSession -> CantoBackend adapter -> Canto runner
-> eight core tools -> Canto events -> Ion display/storage projection
```

Work in this order:

1. Map the hot path in source: `cmd/ion` startup, runtime construction,
   `internal/backend/canto` open/submit/cancel/event translation, tool
   registration, storage/replay projection, and TUI progress/transcript
   rendering.
2. Remove or relocate stale/default-off clutter that can confuse the default
   path, starting with dead model-visible tool code, duplicate render paths,
   stale command/test references, and optional feature hooks that run even when
   their feature is not visible.
3. Keep advanced surfaces behind explicit boundaries: ACP host mode,
   subagents, skill tools, memory, sandbox/trust, routing, and steering must
   not shape the default loop unless the user opts in.
4. Re-run the core gates: focused package tests, `go test ./...`, native race
   subset, tmux smoke for fresh/turn/tool/continue/follow-up, and Fedora
   live smoke with provider-history capture.

Acceptance:

- The default model-visible tool list is exactly the intended eight tools.
- There is one provider-visible history owner and one transcript display
  projection.
- Optional features are either absent from the default request or rejected at
  their owning boundary.
- No global stabilization flag, compatibility branch, or second loop remains.
- The hot path is documented enough that the next refactor can be mechanical,
  not speculative.

## C1: Harness Boundary Refactor

Status: active.

Motivation: Flue is not a TUI replacement, but its `init -> agent -> session`
shape, Pi's small core, OpenAI's model-native harness direction, and Mendral's
harness/sandbox split all reinforce that Ion should be a host over a clear
runtime facade.

Order:

1. Done - Canto `canto-2vxb` - review and design the framework harness facade:
   agent/runtime/session/session-env/tool/command/sandbox/interrupt boundaries.
2. Done - Ion `tk-ezms` - align `CantoBackend` to that facade and keep Ion
   product policy in Ion:
   - Canto `e880c1c` imported.
   - `Open()` constructs a Canto `Harness`.
   - `SubmitTurn()` consumes the harness `PromptStream` stream instead of
     maintaining separate watch/send goroutines.
   - Cached runner/agent ownership was removed from `CantoBackend`.
   - OpenRouter DeepSeek Flash live smoke and tmux TUI smoke passed; Fedora
     probe timed out from this machine.
3. Done - Ion `tk-0r23` - design future virtual tool namespaces for
   skills/memory without bloating the model-facing tool surface:
   - non-workspace context mounts behind explicit namespace resolvers such as
     `skill://`, `memory://`, and `artifact://`
   - default workspace tools do not read or write those namespaces
   - model-visible access is opt-in and narrow (`read_skill` today; possible
     shared `read_resource`/`search_resource` later if evidence justifies it)
4. Done - Ion `tk-vv4y` - refresh sandbox/trust design around executor and
   credential boundaries:
   - trust is workspace eligibility
   - mode is approval posture
   - sandbox is executor enforcement
   - provider credentials stay out of tool subprocesses by default

## C2: Local Executor Boundary

Status: active.

Goal: make local tool execution match the design without adding features:

```text
tool request -> policy/mode/trust -> executor -> sandbox -> process
```

Completed first slice:

1. Done - move local bash process planning/execution behind a small Ion executor
   object.
2. Done - preserve current `bash` schema, sandbox modes, streaming, truncation,
   cancellation, and process-group cleanup.

Next slice:

1. Done - `tk-fhds` - design executor environment and secret-injection policy before
   changing subprocess environment behavior.
2. Done - `tk-k5yp` - expose the active executor environment posture in
   approval previews and `/tools` without listing values or changing current
   environment inheritance.
3. Done - `tk-kxpa` - implement provider-key environment filtering as an explicit
   local bash environment policy.
4. Done - `tk-lux7` - design named tool-secret injection with approval, redaction,
   audit, and remote-executor behavior before implementation.
5. Later hardening: add `minimal` and `allowlist` environment modes if real
   usage shows provider-key filtering is not enough.

Acceptance:

- no new model-visible tools
- no remote sandbox implementation
- no new permission mode
- existing bash/sandbox tests still pass
- focused tool tests, full tests, and native race subset pass

Acceptance:

- Canto exposes one obvious headless harness authoring path for a coding agent,
  without requiring callers to manually wire every primitive.
- Ion's native runtime boundary has no duplicated provider-history ownership,
  no second transcript writer, and no hidden session lifecycle policy.
- The default eight-tool surface stays stable unless eval evidence justifies a
  change.
- TUI/CLI/ACP hosts share the same runtime event and interrupt semantics.

## I5+ Deferred Work

Do not reopen during the current cleanup/refactor stream unless directly needed
for a core bug:

- ACP host/client compatibility polish beyond the first headless-agent slice
- ChatGPT/subscription bridge implementation
- memory/wiki, workflows, model routing
- prompt cache/KV cache experiments
- ripgo integration or merged edit-tool redesign
- Canto elicitation primitive for future `ask_user`

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
