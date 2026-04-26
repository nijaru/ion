# Status: ion (Go)

Fast, lightweight terminal coding agent.

## Current Focus

Ion has been reconciled with the current stabilized Canto surface. The core loop audit, HITL permission-mode hardening, observability exporter slice, workflow topology spec, and first eval regression gate are complete. Current work is moving through the remaining P1 downstream reliability/UX epics.

Near-term tracks:
- `tk-96vy` — Core loop: reliability and resilience audit (Completed)
- `tk-j3ap` — HITL: Permission Modes UX & Escalation (Completed; notifier delivery split to `tk-00km`)
- `tk-wzt6` — Observability: OTel Exporter & Dashboards (Completed)
- `tk-tyww` — Workflow: Workflow Definitions & Recovery (Completed)
- `tk-txju` — Eval: Golden Datasets & Regression Gates (Completed)
- `tk-zbxk` — Security: Policy Config & LLM-as-Judge (Next)
- `tk-90mp` — Streaming: Cost Limits & Model Cascades (Paused after first budget/trace slices)
- `tk-fblb` — Migrate Ion to current Canto surface (Completed)
- `tk-ulfg` — Research: current Pi core loop and feature review (Completed)
- `tk-arhu` / `tk-5vrj` — Verified subagent multiplexing and durable breadcrumbs (Completed)
- TUI refinements — Fixed history navigation boundary behavior (Completed)

Everything else is downstream of the solo agent core. SOTA epics are important, but they do not outrank a reliable submit/stream/tool/approval/cancel/error/persist/replay loop.

Provider work to keep in mind:
- most providers remain API-key or custom-endpoint integrations
- subscription/OAuth providers need explicit treatment
- OpenAI ChatGPT subscription support for third-party apps was raised; verify from official sources before design depends on it.
- **Model cascades (SOTA 14):** Enforcing cost limits and routing tasks to cheaper models dynamically.

Design rule:
- v0.0.0 has no compatibility debt; current bindings and config shapes are allowed to change directly if the end-state is better. New SOTA plans supersede older designs where there is conflict.
- Do not overfit cost/routing work to DSPy, GEPA, or optimizer-centric designs. Keep Ion closer to pi's simple clever model: explicit presets, inspectable decisions, and only the routing knobs that improve the coding workflow.
- Similar agents are references, not feature-parity requirements. Adopt from pi, Claude Code, Codex, OpenCode, Cursor, Droid, Letta, and others only when the idea strengthens Ion's core coding loop or preserves a simple, inspectable UX.

## Next Steps
1. Start `tk-zbxk` with deterministic policy config before considering any LLM-as-judge path.
2. Keep adding focused regressions before broadening any SOTA epic.
3. Use the current Pi review as a reference for loop contracts and future `/tree`, but keep concrete fixes on the current solo loop.
4. Resume `tk-90mp` after the P1 observability slice; budget enforcement and routing trace slices are already committed.
5. Treat older `Canto: contribute ...` tasks as re-triaged: no default grep/glob or preset coding-tool bundles; only concrete reusable extension packages should move upstream.

*(Note: Older P3 TUI refinement tasks like configurable verbosity, skill layering, and status line context have been subsumed by their respective SOTA epics).*

## Completed (Recent)
- [x] **Core loop reliability audit (`tk-96vy`)** — Completed final approval/session-switch review; approval bridge failures now surface as session errors and unknown tool result IDs cannot clear another pending tool.
- [x] **Permission mode startup slice (`tk-j3ap`)** — READ mode is now non-escalating even with stale session approvals; `--mode`/`--yolo` startup selection now applies to TUI and print sessions, with non-interactive approvals limited to YOLO.
- [x] **ESCALATE.md host slice (`tk-j3ap`)** — Ion now loads root `ESCALATE.md` via Canto's workspace parser and surfaces declared email/Slack channels plus approval timeout in approval prompts.
- [x] **HITL task closure (`tk-j3ap`)** — Closed the safe host scope and split actual Slack/email delivery into `tk-00km` pending credential, timeout, and audit design.
- [x] **Observability exporter/dashboard (`tk-wzt6`)** — Added config/env-driven OTLP trace and metric export for Canto telemetry plus a Grafana starter dashboard; `go test ./...` passes.
- [x] **Workflow topology spec (`tk-tyww`)** — Defined Ion-owned Code Review and Bug Fix workflow DAGs, checkpoint recovery policy, and human-gate rules on top of Canto graph primitives.
- [x] **Eval golden gate (`tk-txju`)** — Moved prompt quality checks into `evals/golden/prompt_quality.toml`, kept them enforced by `go test ./...`, and documented future eval artifact policy.
- [x] **Core loop audit slices (`tk-96vy`)** — Fixed native/ACP commit-before-finish ordering, sticky error/cancel terminal states, cancellation queue clearing, full transcript replay, tool error replay, backend tool ID propagation, interleaved tool tracking, and fail-closed proactive compaction recovery; `go test ./...` passes.
- [x] **Canto dependency refresh foundation (`tk-fblb`)** — Updated Ion to Canto `f47e7de`; migrated request processors from `canto/context` to `canto/prompt` and hooks from `Hook/NewFunc` to `Handler/FromFunc`; `go test ./...` passes.
- [x] **Current Pi core-loop review (`tk-ulfg`)** — Added `ai/research/pi-current-core-loop-review-2026-04.md`; Pi remains the strongest loop reference, but core reliability gates `/tree`, compaction polish, and SOTA routing work.
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
- [x] **Agent Loop: UX Streaming & Reflection Prompts (`tk-hgp4`)** — Background compaction, configurable tool/thinking verbosity, reflexion processor for failed tool calls.
- [x] **Review fixes (`tk-uzoz`, `tk-c0ci`, `tk-l9ag`)** — Registered reflexionProcessor, fixed compaction-failure hang, unified Plane A/B verbosity, added normalizeVerbosity validation.

## Active Tasks
See `tk ls` for the full list. Current active priority:
- `tk-zbxk` — Security: Policy Config & LLM-as-Judge (next)

P1 SOTA epics remain important but are downstream of `tk-96vy`:
- `tk-90mp` — Streaming: Cost Limits & Model Cascades
- `tk-j3ap` — HITL: Permission Modes UX & Escalation
- `tk-wzt6` — Observability: OTel Exporter & Dashboards (Completed)
- `tk-tyww` — Workflow: Workflow Definitions & Recovery (Completed)
- `tk-txju` — Eval: Golden Datasets & Regression Gates (Completed)
- `tk-zbxk` — Security: Policy Config & LLM-as-Judge
- `tk-r5jr` — Subagent: Agent Personas & Model Routing
- `tk-z2cb` — Workspace: Trust UX & Visual Rollback
- `tk-yf7v` — Tool Execution: Tool Loading UX & Approval Tiers
- `tk-gxfu` — Memory: Karpathy-Style Knowledge Base & Search UX

## Blockers
- None.

## Topic Files
- `ai/SOTA-REQUIREMENTS.md` — The 14 core SOTA product responsibilities.
- `ai/research/canto-dspy-app-patterns-2026-04.md` — Future Ion patterns from Canto authoring work; DSPy is one reference.
- `ai/research/pi-current-core-loop-review-2026-04.md` — Current Pi core-loop, `/tree`, compaction, and UX review.
- `ai/review/canto-research-delta-2026-04-26.md` — Recent Canto ai/ findings that affect Ion sequencing.
- `ai/specs/tools-and-modes.md` — Permission modes spec
- `ai/specs/swarm-mode-and-inline-subagents.md` — Inline subagent rendering, future swarm mode
- `ai/research/pi-architecture.md` — Pi-mono architecture analysis
- `ai/research/ion-architecture.md` — Ion architecture analysis
- `ai/design/cross-pollination.md` — Pi → canto/ion actionable insights
- `ai/DESIGN.md` — Architecture and event flow (Merged with SOTA)
- `ai/DECISIONS.md` — Decision log
