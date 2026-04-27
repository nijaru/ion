# ion Roadmap

## Completed foundation

### Runtime

- canto is the native runtime
- SQLite-backed session persistence is in place
- streaming turns, tool calls, cancellation, and resume exist; resume/new-turn correctness is proven for the former empty-assistant corruption path
- layered project instructions are implemented

### TUI

- inline scrollback architecture is in place
- committed transcript rows go to terminal scrollback
- Plane B is limited to live UI
- provider/model switching works in-session
- model and provider pickers are usable and catalog-driven

### Provider foundation

- provider catalog is the source of truth
- broad native provider coverage is wired
- local and custom OpenAI-compatible endpoints are separate concepts
- direct native model fetchers exist for the main providers

## Active roadmap

Execution rule:
- The core solo agent is the product.
- Pi is the minimum taste/reliability floor for the loop; Codex is the richer open-source CLI/TUI reference; Claude Code is a public behavior reference.
- Advanced orchestration, ACP, subscription bridges, skills, privacy expansion, model cascades, and swarm mode are blocked behind a stable single-agent inline loop.
- v0.0.0 has no compatibility debt. If the final design wants a different binding, preset, or config shape, change it directly.

### 0. Core parity plan and queue hygiene

Goal:
- keep planning, tasks, and implementation sequenced around the core loop instead of scattered feature work

Tracked by:
- `tk-mmcs`

Includes:
- maintain `ai/PLAN.md` as the active v0 parity plan
- keep `tk ready` aligned with `ai/STATUS.md`
- reopen tasks when live evidence disproves completion
- use Pi/Codex/Claude references to define behavior expectations, not implementation templates

### 1. Core session replay and model history

Goal:
- keep submit/stream/tool/approval/cancel/error/persist/replay boring and resilient
- prove restored sessions can continue without poisoning provider history

Status:
- complete for the empty-assistant resume blocker; keep regression coverage current as Gate 2 expands

Tracked by:
- `tk-izo7`
- `tk-5t72`

Includes:
- keep provider-visible replay free of invalid transcript events, including empty assistant messages after tool-only/no-op model steps
- make `--continue`, bare `--resume`, `/resume`, and resumed-new-turn behavior deterministic
- render restored transcript with the same visual rules as live transcript
- compact routine `list`/`read` output without hiding semantically important tool failures
- add tests for legacy corrupted rows and future clean sessions

### 2. Core loop contract

Goal:
- prove the native solo loop with a repeatable smoke suite before expanding orchestration

Tracked by:
- `tk-zz5i`
- follow-up tasks split from `tk-96vy` as bugs are found

Includes:
- preserve deterministic submit/stream/tool/approval/cancel/retry/error/persist/replay smoke coverage
- event ordering: message/tool terminal events before turn terminal events
- cancellation, immediate backend errors, provider-limit errors, and retry-until-cancelled remain resumable
- tool failure conversion and ordering match Canto lifecycle events
- checkpoint/rewind follow-up only where it improves reliability or rollback confidence

### 3. TUI baseline

Goal:
- make the normal interface feel coherent before adding deeper product layers

Tracked by:
- `tk-5cqs`
- `tk-kvqv`
- `tk-tilu`

Includes:
- slash command autocomplete and clear `/help`
- commands/settings/model changes that work during active turns where appropriate
- routine tool output collapsed by default with explicit detail access
- thinking/progress state shown without dumping hidden reasoning
- readable transcript spacing for live and replayed entries

### 4. Config, provider, and session hygiene

Goal:
- remove confusing state/config/provider behavior after the core loop is safe

Tracked by:
- `tk-9n7h`

Includes:
- provider registry/model-picker correctness after provider/config changes
- no placeholder favorites or implicit provider/model persistence at startup
- clear primary/fast selection semantics
- custom endpoint isolation for local OpenAI-compatible servers
- provider errors clear on state changes

### 5. Safety and execution boundaries

Goal:
- keep deterministic policy and OS enforcement ahead of classifier-driven automation

Tracked by:
- `tk-n0n4`

Includes:
- deterministic policy and existing sandbox posture remain the base layer
- READ/EDIT/AUTO semantics stay small and obvious
- privacy filtering continues only for concrete leak surfaces or broader telemetry/logging
- optional model-assisted classification only after fail-closed behavior and audit logging

### 6. Cost, limits, and subscription paths

Goal:
- handle API/subscription limits and model budgets without turning Ion into an optimizer workbench

Tracked by:
- `tk-90mp`
- `tk-a4m1`

Includes:
- budget enforcement
- only simple, inspectable model cascade policy where it clearly improves reliability or cost control
- routing trace visibility
- graceful provider quota/rate-limit handling
- explicit ChatGPT subscription evaluation as a separate bridge path, not a native API assumption

### 7. ACP stabilization

Goal:
- keep ACP useful for subscription/CLI bridges without letting it drive native Ion design

Tracked by:
- `tk-o0iw`
- `tk-2ffy`
- `tk-6zy3`

Includes:
- initial session context at `Open`
- stderr routing separate from transcript events
- token usage event mapping where available
- session continuity/resume decision
- headless Ion-as-ACP-agent mode stays P3 until the bridge path is stable

### 8. Product depth after the core loop

Goal:
- add higher-level UX only after the solo loop remains reliable under normal and failure cases

Tracked by:
- `tk-g78q`
- `tk-8174`
- `tk-gopd`
- `tk-369n`

Includes:
- skills/self-extension nudges without hiding behavior
- cross-host sync and TUI branching
- external editor handoff
- typed thinking capabilities and provider translation

### 9. Pi + Claude + Codex guardrails for Ion

Goal:
- keep Pi, Codex, and Claude Code findings grounded in idiomatic Go and Bubble Tea v2
- adopt only the portable UX and orchestration patterns
- avoid React/JSX-shaped abstractions or framework mimicry

Documented in:
- `ai/PLAN.md`
- `ai/research/pi-current-core-loop-review-2026-04.md`
- `ai/design/cross-pollination.md`
- `ai/plans/ion-go-bubbletea-guardrails-2026-04-01.md`

Includes:
- command dispatch timing
- queued input during long operations
- overlay/modal boundaries
- one app-state partition per lifecycle concern
- clear progress/status surfaces

## Explicitly lower priority

- token usage color bands
- git diff stats in footer
- AskUser UI
- broad tab completion beyond slash command baseline
- request cache continuity
- auto thinking budget mode
- canto upstreaming tasks

These are real tasks, but they are not on the critical path.
