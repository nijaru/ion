# Status: ion (Go)

Fast, lightweight terminal coding agent.

## Current Focus
Near-term tracks:

- `tk-arhu` / `tk-5vrj` — Verified subagent multiplexing and durable breadcrumbs
- TUI refinements — Fixed history navigation boundary behavior

Everything else is downstream of the solo agent core and now-stable subagent primitives.

Provider work to keep in mind:
- most providers remain API-key or custom-endpoint integrations
- subscription/OAuth providers need explicit treatment
- `tk-a4m1` exists only as a later evaluation track for ChatGPT subscription support

Deferred idea to remember:
- `tk-hase` — auto thinking budget mode, analogous to primary/fast model selection

Design rule:
- v0.0.0 has no compatibility debt; current bindings and config shapes are allowed to change directly if the end-state is better.

## Next Steps
1. **`tk-pwsl`** — Swarm mode: alternate-screen operator view
2. **`tk-gmhw`** — TUI: configurable tool/thinking transcript verbosity
3. **`tk-lmhg`** — Agent: skill support and instruction layering
4. **`tk-gopd`** — TUI: external editor handoff (DEFERRED)

## Completed (Recent)
- [x] **Subagents: runtime semantics and lifecycle (`tk-5vrj`)** — Implemented multiplexed subagent tracking, durable breadcrumbs, and multiplexed event routing in broker.
- [x] **Subagents: inline Plane B presentation (`tk-arhu`)** — Compact worker rows, collapse rules, and parent waiting states implemented in viewport.
- [x] **TUI: boundary-respecting history navigation** — `Up`/`Down`/`Ctrl+P`/`Ctrl+N` now only trigger history navigation at the top/bottom of the multiline composer.
- [x] **Stabilize inline agent loop and TUI (`tk-7kga`)** — Verified streaming, tool lifecycle, approval flow, and error presentation with new tests.
- [x] **Model selector: provider/model tabs (`tk-di6d`)** — Provider/model picker with favorites at the top.
- [x] **Model selector: page navigation (`tk-9pr1`)** — PgUp/PgDn support in picker.
- [x] **Sessions: lightweight titles and summaries (`tk-4ywr`)** — Metadata-based titles and summaries implemented in storage and picker.
- [x] **Modularize Ion TUI (`tk-2b79`)** — Componentized `internal/app/model.go` into `Viewport`, `Input`, `Broker`, `Picker`, and `Progress`.
- [x] **Approval UX overhaul (`tk-k4hv`)** — Redesigned 3-mode system (READ/EDIT/YOLO) and category-scoped auto-approval ("Always" key) implemented.
- [x] **Agent Compaction Tool (`tk-pw3s`)** — `compact` tool implemented in Ion.
- [x] **RPC/print mode (`tk-r1wx`)** — One-shot query mode and JSONL-friendly scripting surface implemented.
- [x] **Sandbox support (`tk-8s0h`)** — Opt-in bash sandbox planning added with `off`/`auto`/`seatbelt`/`bubblewrap` modes.
- [x] **Retry behavior (`tk-kz3k`)** — Native providers retry transient generation and streaming errors automatically before surfacing a final failure.
- [x] **Canto Context Governor (`tk-4ft8`)** — Runtime now auto-compacts on overflow and proactively compacts before a turn when session usage is near the context limit.

## Active Tasks
- `tk-arhu` [p4 done] Subagents: inline Plane B presentation
- `tk-pwsl` [p4 open] Swarm mode: alternate-screen operator view
- `tk-gmhw` [p3 open] TUI: configurable tool/thinking transcript verbosity
- `tk-lmhg` [p3 open] Agent: skill support and instruction layering
- `tk-i207` [p3 open] Status line: reasoning budget and context presence
- `tk-0dwv` [p3 open] Core: session tree navigation and branching

## Blockers
- None.

## Topic Files
- `ai/specs/tools-and-modes.md` — Permission modes spec (authoritative for tk-k4hv)
- `ai/specs/swarm-mode-and-inline-subagents.md` — Inline subagent rendering, future swarm mode, and session title/summary direction
- `ai/specs/tui-hotkeys.md` — Current picker/navigation key semantics
- `ai/research/approval-ux-survey-2026-03-30.md` — Competitor approval UX research
- `ai/research/pi-architecture.md` — Pi-mono architecture analysis
- `ai/research/ion-architecture.md` — Ion architecture analysis
- `ai/design/cross-pollination.md` — Pi → canto/ion actionable insights
- `ai/DESIGN.md` — Architecture and event flow
- `ai/DECISIONS.md` — Decision log
