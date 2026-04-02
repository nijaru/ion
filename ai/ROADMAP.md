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

### 1. Provider validation

Goal:
- turn broad structural provider support into validated provider support

Tracked by:
- `tk-ekao`
- `tk-a4m1`

Includes:
- auth/env reality
- endpoint defaults
- `/models` behavior
- manual-entry fallbacks where live listing is weak
- auth-mode classification:
  - API key providers
  - subscription/OAuth providers
  - CLI bridge providers
  - custom OpenAI-compatible endpoints

### 2. Instruction and skills surface

Goal:
- keep instruction layering clear
- define whether and how ion should expose first-class skills

Tracked by:
- `tk-lmhg`

### 3. Session navigation and agent breadth

Tracked by:
- `tk-7kga` — stabilize inline agent loop and TUI
- `tk-4ft8` — context governor / compaction robustness (overflow recovery wired; proactive trigger still under review)
- `tk-4ywr` — session titles and lightweight summaries for picker/resume UX
- `tk-0dwv` — session tree navigation
- `tk-5vrj` — subagents: runtime semantics and lifecycle
- `tk-arhu` — subagents: inline Plane B presentation
- `tk-pwsl` — swarm mode: alternate-screen operator view
- `tk-st4q` — ion as an ACP agent

Order:
- first stabilize the inline single-agent loop
- then land the remaining runtime primitives that make that path trustworthy
- then build subagent runtime semantics
- then add inline subagent presentation
- then consider swarm mode later as a dedicated operator view

Product ladder:
- solo agent
- dependent runtime features
- orchestration wrappers

TUI surface:
- `tk-7kga` — core inline stability
- `tk-i207` — status line and context presentation
- `tk-4ywr` — session titles and lightweight summaries
- `tk-gmhw` — transcript verbosity controls
- `tk-arhu` — inline child presentation
- `tk-pwsl` — swarm/operator view later

### 4. Footer and settings restraint

Goal:
- keep footer/settings useful without turning ion into a control panel

Tracked by:
- `tk-i207`

### 5. Pi + Claude guardrails for ion

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
