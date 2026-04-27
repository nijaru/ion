# ion Roadmap

## Completed foundation

### Runtime

- canto is the native runtime
- SQLite-backed session persistence is in place
- streaming turns, tool calls, cancellation, and resume work
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
- Pi is a rough benchmark for maturity, not a hard parity gate.
- The real gate for advanced orchestration is a stable, feature-complete single-agent inline experience.
- The core solo agent is the product. Subagents, swarm mode, ACP, and other orchestration surfaces are wrappers around that base.
- v0.0.0 has no compatibility debt. If the final design wants a different binding, preset, or config shape, change it directly.

### 1. Core reliability and rollback

Goal:
- keep submit/stream/tool/approval/cancel/error/persist/replay boring and resilient
- prove the native solo loop with a repeatable smoke suite before expanding orchestration

Tracked by:
- `tk-9n7h`
- `tk-5t72`

Includes:
- preserve deterministic submit/stream/tool/approval/cancel/retry/error/persist/replay smoke coverage
- provider registry/model-picker correctness after provider/config changes
- CantoBackend storage and registry cleanup after the current provider/backend surface settles
- keep provider-visible replay free of invalid transcript events, including empty assistant messages after tool-only/no-op model steps
- checkpoint/rewind follow-up only where it improves reliability or rollback confidence

### 2. Safety and execution boundaries

Goal:
- keep deterministic policy and OS enforcement ahead of classifier-driven automation

Tracked by:
- `tk-n0n4`

Includes:
- deterministic policy and existing sandbox posture remain the base layer
- privacy filtering for prompts, logs, traces, tool previews, and approval surfaces
- optional model-assisted classification only after fail-closed behavior and audit logging

Priority:
- Current deterministic approval/tool-preview redaction is enough for now.
- Further privacy work is not on the critical path unless a concrete leak surface appears or telemetry/logging expands.

PII note:
- OpenAI's current public moderation docs document `omni-moderation-latest` for harmful-content classification, not a dedicated PII detector. If OpenAI ships or documents a PII-specific model, treat it as an optional detector behind Ion's own redaction interface, not as the privacy architecture.

### 3. Cost limits and model routing

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

### 4. ACP stabilization

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

### 5. Product depth after the core loop

Goal:
- add higher-level UX only after the solo loop remains reliable under normal and failure cases

Tracked by:
- `tk-00km`
- `tk-g78q`
- `tk-8174`
- `tk-gopd`
- `tk-369n`
- `tk-5cqs`

Includes:
- Slack/email HITL notifier delivery and audit
- skills/self-extension nudges without hiding behavior
- cross-host sync and TUI branching
- external editor handoff
- typed thinking capabilities and provider translation
- slash command surface review

### 6. Pi + Claude guardrails for ion

Goal:
- keep Pi and Claude Code findings grounded in idiomatic Go and Bubble Tea v2
- adopt only the portable UX and orchestration patterns
- avoid React/JSX-shaped abstractions or framework mimicry

Documented in:
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
- tab completion
- request cache continuity
- auto thinking budget mode
- canto upstreaming tasks

These are real tasks, but they are not on the critical path.
