# Status: ion (Go)

Fast, lightweight terminal coding agent.

## Current Focus

We are integrating the SOTA findings (14 core product epics) with our stable minimalist Pi/Claude-Code architecture.

Near-term tracks (SOTA implementation):
- Transitioning from architecture design into implementing the Priority 1 SOTA epics tracked in `tk ls`.
- `tk-arhu` / `tk-5vrj` — Verified subagent multiplexing and durable breadcrumbs (Completed)
- TUI refinements — Fixed history navigation boundary behavior (Completed)

Everything else is downstream of the solo agent core and now-stable subagent primitives.

Provider work to keep in mind:
- most providers remain API-key or custom-endpoint integrations
- subscription/OAuth providers need explicit treatment
- **Model cascades (SOTA 14):** Enforcing cost limits and routing tasks to cheaper models dynamically.

Design rule:
- v0.0.0 has no compatibility debt; current bindings and config shapes are allowed to change directly if the end-state is better. New SOTA plans supersede older designs where there is conflict.

## Next Steps
1. **SOTA Epic Selection:** Begin implementation of a Priority 1 SOTA task (e.g., `tk-hgp4` UX Streaming, `tk-j3ap` Permission Modes UX, or `tk-8174` Session Branching).
2. **`tk-pwsl`** — Swarm mode: alternate-screen operator view (Downstream of SOTA loop stability)
3. **`tk-gopd`** — TUI: external editor handoff (DEFERRED)

*(Note: Older P3 TUI refinement tasks like configurable verbosity, skill layering, and status line context have been subsumed by their respective SOTA epics).*

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
The task tracker (`tk ls`) has been updated to reflect the 14 SOTA epics as the primary active tasks.
- `tk-hgp4` [p1 open] Agent Loop: UX Streaming & Reflection Prompts
- `tk-90mp` [p1 open] Streaming: Cost Limits & Model Cascades
- `tk-j3ap` [p1 open] HITL: Permission Modes UX & Escalation
- `tk-zbxk` [p1 open] Security: Policy Config & LLM-as-Judge
- `tk-8174` [p2 open] Session: Cross-Host Sync & TUI Branching
- `tk-g78q` [p2 open] Skills: Self-Extension Nudges & Marketplace
- `tk-pwsl` [p4 open] Swarm mode: alternate-screen operator view

*(Tasks `tk-gmhw`, `tk-lmhg`, `tk-i207`, `tk-0dwv`, and `tk-xdx5` have been merged into the SOTA epics above).*

## Blockers
- None.

## Topic Files
- `ai/SOTA-REQUIREMENTS.md` — The 14 core SOTA product responsibilities.
- `ai/specs/tools-and-modes.md` — Permission modes spec
- `ai/specs/swarm-mode-and-inline-subagents.md` — Inline subagent rendering, future swarm mode
- `ai/research/pi-architecture.md` — Pi-mono architecture analysis
- `ai/research/ion-architecture.md` — Ion architecture analysis
- `ai/design/cross-pollination.md` — Pi → canto/ion actionable insights
- `ai/DESIGN.md` — Architecture and event flow (Merged with SOTA)
- `ai/DECISIONS.md` — Decision log