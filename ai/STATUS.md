# Status: ion (Go)

Fast, lightweight terminal coding agent.

## Current Focus

Ion is back in core-loop stabilization. Gate 1 is green; do not expand into ACP, sandboxing, routing, or SOTA work until the core-loop contract and TUI baseline gates are also covered.

Current active blockers:
- `tk-mmcs` — Keep the Pi/Codex/Claude core parity plan, roadmap, and task queue synchronized while the loop is stabilized.

Next core-parity work:
- `tk-9n7h` — Provider/model picker correctness. This is the next P2 product hygiene task after the resume/model-history blocker.
- Current slice: remove implicit catalog-derived fast model selection so only explicitly configured primary/fast models appear as configured presets.

Captured lower-priority polish:
- `tk-5cqs` — Slash commands: autocomplete and command surface review (P3)
- `tk-c037` — TUI: question-mark help shortcut (completed)
- `tk-hase` — Thinking UI/config slice (completed; Canto capability follow-up split to `tk-369n`)
- `tk-n0n4` — Privacy display redaction slice completed; remaining privacy work is P4 until a concrete leak blocks a release.
- `tk-j6gh` — TUI startup copy polish (completed)
- `tk-5gtk` — CLI continue/resume separator handling (completed)
- `tk-ekw5` — Compare Pi/Codex UX references after core loop is stable; local repos are `/Users/nick/github/openai/codex` and `/Users/nick/github/badlogic/pi-mono`. Claude Code and `/Users/nick/github/ultraworkers/claw-code` are also useful product references when the comparison is relevant.
- `tk-kvqv` — Collapse routine tool output by default (P3)
- `tk-tilu` — Show thinking state without dumping hidden reasoning (P3)

Near-term tracks after the active blocker:
- core loop contract tests: resumed new turn, tool-only assistant turns, cancellation/error persistence, retry status, provider-limit recovery
- noninteractive prompt mode is now scriptable with text/JSON output and should be used as the automated local-api/Fedora smoke surface
- TUI baseline: compact routine tool output, slash command autocomplete/help, thinking state display
- config/provider hygiene: no placeholder favorites, custom endpoint isolation, clear state/config/trust ownership
- approvals, sandboxing, trust, modes, and broader safety polish are secondary to Pi-style core loop parity

Previously completed tracks that need regression coverage kept current:
- `tk-zz5i` — Core loop: scripted resilience smoke suite (Completed)
- `tk-wqhg` — Permission UX: trust and mode semantics (Completed)
- `tk-0kip` — Provider/model picker: non-listing providers and preset clarity (Completed)
- `tk-hs3m` — Local API: keep system messages template-compatible (Completed through Canto context primitive integration)
- `tk-a5ds` — Config UX umbrella (Completed)
- `tk-8188` — Settings storage: split stable config from mutable state (Completed)
- `tk-2wrb` — Context: Compaction UX & Summarization Prompts (Completed)
- `tk-96vy` — Core loop: reliability and resilience audit (Completed)
- `tk-j3ap` — HITL: Permission Modes UX & Escalation (Completed; notifier delivery split to `tk-00km`)
- `tk-wzt6` — Observability: OTel Exporter & Dashboards (Completed)
- `tk-tyww` — Workflow: Workflow Definitions & Recovery (Completed)
- `tk-txju` — Eval: Golden Datasets & Regression Gates (Completed)
- `tk-zbxk` — Security: Policy Config & LLM-as-Judge (Deterministic config complete; LLM judge split)
- `tk-9lws` — Security: LLM-as-judge classifier and circuit breaker (Completed foundation; model adapter deferred)
- `tk-r5jr` — Subagent: Agent Personas & Model Routing (Completed)
- `tk-z2cb` — Workspace: Trust UX & Visual Rollback (Trust complete; rewind split)
- `tk-yf7v` — Tool Execution: Tool Loading UX & Approval Tiers (Completed)
- `tk-gxfu` — Memory: Karpathy-Style Knowledge Base & Search UX (Search UX complete; wiki split)
- `tk-90mp` — Streaming: Cost Limits & Model Cascades (Active P2)
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
1. Continue `tk-9n7h` provider/model picker cleanup: verify explicit primary/fast semantics, non-listing providers, and custom/local endpoint isolation.
2. Continue Gate 2 coverage: cancellation/error persistence, retry status, provider-limit recovery, and resumed tool-session invariants.
3. Keep slash autocomplete and routine tool display behind provider/core hygiene unless they block normal testing.

*(Note: Older P3 TUI refinement tasks like configurable verbosity, skill layering, and status line context have been subsumed by their respective SOTA epics).*

## Completed (Recent)
- [x] **Config UX cleanup (`tk-a5ds`)** — Fixed confusing provider/model picker state, moved mutable selections to state, added focused `/settings`, improved help readability, and left broader slash-command review in `tk-5cqs`.
- [x] **Settings storage split (`tk-8188`)** — Stable config now stays in `~/.ion/config.toml`, mutable provider/model/thinking/active-preset state lives in `~/.ion/state.toml`, and both files use atomic temp-file replacement.
- [x] **Core-loop smoke suite (`tk-zz5i`)** — Added deterministic app-level smoke coverage for submit/stream/tool persistence and replay, approval, cancel, retry-status persistence, and provider-limit stop traces.
- [x] **Cost/limit resilience (`tk-90mp`)** — Budget enforcement, routing decision traces, provider-limit classification, Fedora local-api smoke, and transport-only endless retry are complete; richer model cascades are deferred until a concrete policy is needed.
- [x] **Privacy display-surface redaction (`tk-n0n4`)** — Added deterministic redaction for obvious secrets/PII and applied it to approval prompt descriptions/args, Slack/email approval notification text, and tool-call preview args.
- [x] **Startup copy polish (`tk-j6gh`)** — Startup now says `Workspace is not trusted. Starting in READ mode...` and `%d tools registered`.
- [x] **Empty assistant replay fix (`tk-ify2`)** — Fedora/local-api rejected replay after a tool turn because Canto persisted an assistant message with `content=""` and no `tool_calls`; Canto `192bfdf` skips those empty messages while preserving usage.
- [x] **Continue CLI separator fix (`tk-5gtk`)** — Correct command is `ion --continue` or `go run ./cmd/ion --continue`; Ion also accepts a leading `--` before flags to avoid silent fresh sessions.
- [x] **Session lifecycle correction (`tk-8o7r`)** — Startup now uses a lazy storage session, slash commands do not persist to durable conversation storage, `--resume` without an ID opens the picker, and `--continue` skips old empty/slash-only sessions.
- [x] **Session picker null-name fix (`tk-0s5a`)** — Session listing tolerates legacy rows with `NULL` names.
- [x] **Dual transcript persistence fix (`tk-5t72`)** — Canto now owns model-visible user/assistant/tool transcript persistence; Ion only live-renders those events and keeps UI-local metadata/status/usage writes. Verified with `go test ./...`, a Fedora/local-api print smoke, SQLite event inspection, and `--continue` on the new session.
- [x] **Resume transcript rendering (`tk-izo7`)** — Canto commit `927e482` filters invalid assistant rows from effective history, Ion imports that pseudo-version, replay/live transcript entries use shared spacing, the resumed marker appears after the startup header, backend close waits for turn goroutines, and routine tool replay is compact by default. Verified with Canto `go test ./...`, Ion `go test ./...`, `go run ./... --continue --print --timeout 30s --prompt hi`, and `go run ./... --continue --print --output json --timeout 30s --prompt "reply with the single word ok"` against the live local-api session.
- [x] **Thinking control Ion slice (`tk-hase`)** — Ion preserves `auto/off/minimal/low/medium/high/xhigh/max`, exposes common named levels in `/thinking`, and only sends named effort when Canto reports support; richer provider translation is split to `tk-369n`.
- [x] **Transport-only endless retry (`tk-90mp`)** — Canto `f71205f` added transport-only endless retry; Ion wires `retry_until_cancelled` to that path so disconnects can retry until Ctrl+C while rate/quota/server failures stay bounded and readable.
- [x] **Retry-until-cancel resilience slice (`tk-lm25`)** — Canto now supports retry-until-context-cancel and raw transport transient classification; Ion defaults `retry_until_cancelled` on, emits visible retry status, and persists those status events without transcript spam.
- [x] **HITL notifier delivery slice (`tk-00km`)** — Approval requests now attempt Slack webhook and SMTP email notification when ESCALATE.md channels and credentials are configured, while auditing sent/failed/skipped outcomes without blocking the local approval prompt.
- [x] **Provider-limit resilience slice (`tk-90mp`)** — Rate/quota/context/capacity provider failures now get readable UI prefixes while preserving raw provider text and append durable `routing_decision` stop traces. Verified with `go test ./...` and Fedora `local-api` smoke against `qwen3.6:27b-uncensored`.
- [x] **Compaction UX slice (`tk-2wrb`)** — Added visible compacting progress, follow-up queueing while manual compaction runs, and Ion summarizer guidance that preserves goals, paths, task IDs, decisions, failures, and verification status.
- [x] **Policy classifier foundation (`tk-9lws`)** — Added optional EDIT-mode classifier hook for existing `ask` decisions, with timeout/model-error/invalid-action fallback to `ask`, hard-boundary protection, and auditable policy events.
- [x] **Sandbox hardening (`tk-kfno`)** — Explicit Seatbelt/bubblewrap modes fail closed when unavailable, bubblewrap planning skips missing platform paths, and sandbox posture is visible at startup and through `/tools`.
- [x] **Checkpoint rewind (`tk-8e2x`)** — Native file tools create durable pre-change checkpoints; `/rewind <id>` previews restore actions and `/rewind <id> --confirm` restores with transcript start/completion entries.
- [x] **Core loop reliability audit (`tk-96vy`)** — Completed final approval/session-switch review; approval bridge failures now surface as session errors and unknown tool result IDs cannot clear another pending tool.
- [x] **Permission mode startup slice (`tk-j3ap`)** — READ mode is now non-escalating even with stale session approvals; `--mode auto`/`--yolo` startup selection now applies to TUI and print sessions, with non-interactive approvals limited to AUTO.
- [x] **ESCALATE.md host slice (`tk-j3ap`)** — Ion now loads root `ESCALATE.md` via Canto's workspace parser and surfaces declared email/Slack channels plus approval timeout in approval prompts.
- [x] **HITL task closure (`tk-j3ap`)** — Closed the safe host scope and split actual Slack/email delivery into `tk-00km` pending credential, timeout, and audit design.
- [x] **Observability exporter/dashboard (`tk-wzt6`)** — Added config/env-driven OTLP trace and metric export for Canto telemetry plus a Grafana starter dashboard; `go test ./...` passes.
- [x] **Workflow topology spec (`tk-tyww`)** — Defined Ion-owned Code Review and Bug Fix workflow DAGs, checkpoint recovery policy, and human-gate rules on top of Canto graph primitives.
- [x] **Eval golden gate (`tk-txju`)** — Moved prompt quality checks into `evals/golden/prompt_quality.toml`, kept them enforced by `go test ./...`, and documented future eval artifact policy.
- [x] **Deterministic policy config (`tk-zbxk`)** — Added `policy_path`/`~/.ion/policy.yaml` YAML rules for exact tools and categories across Canto/ACP backends; READ remains non-weakenable and LLM-as-judge is split to a follow-up.
- [x] **Subagent personas and model routing (`tk-r5jr`)** — Registered the native `subagent` tool with built-in explorer/reviewer/worker personas, global YAML-frontmatter overrides, fast/primary model-slot routing, and scoped child tool registries.
- [x] **Workspace trust (`tk-z2cb`)** — Added user-global trusted workspace state, startup downgrade to READ for untrusted checkouts, `/trust`, and docs; visual rewind is split pending checkpoint semantics.
- [x] **Tool loading UX (`tk-yf7v`)** — Surfaced Canto lazy-tool state in startup and `/tools`; kept approval tiers to READ/EDIT/AUTO plus policy rules instead of adding redundant modes.
- [x] **Memory search UX (`tk-gxfu`)** — Added `/memory` tree/search over Canto workspace memory and documented wiki/collection-management deferral.
- [x] **Core loop audit slices (`tk-96vy`)** — Fixed native/ACP commit-before-finish ordering, sticky error/cancel terminal states, cancellation queue clearing, full transcript replay, tool error replay, backend tool ID propagation, interleaved tool tracking, and fail-closed proactive compaction recovery; `go test ./...` passes.
- [x] **Canto dependency refresh foundation (`tk-fblb`)** — Updated Ion to Canto `f47e7de`; migrated request processors from `canto/context` to `canto/prompt` and hooks from `Hook/NewFunc` to `Handler/FromFunc`; `go test ./...` passes.
- [x] **Current Pi core-loop review (`tk-ulfg`)** — Added `ai/research/pi-current-core-loop-review-2026-04.md`; Pi remains the strongest loop reference, but core reliability gates `/tree`, compaction polish, and SOTA routing work.
- [x] **Subagents: runtime semantics and lifecycle (`tk-5vrj`)** — Implemented multiplexed subagent tracking, durable breadcrumbs, and multiplexed event routing in broker.
- [x] **Subagents: inline Plane B presentation (`tk-arhu`)** — Compact worker rows, collapse rules, and parent waiting states implemented in viewport.
- [x] **TUI: boundary-respecting history navigation** — `Up`/`Down`/`Ctrl+P`/`Ctrl+N` now only trigger history navigation at the top/bottom of the multiline composer.
- [x] **Stabilize inline agent loop and TUI (`tk-7kga`)** — Verified streaming, tool lifecycle, approval flow, and error presentation with new tests.
- [x] **Model selector: provider/model tabs (`tk-di6d`)** — Provider/model picker with configured presets at the top.
- [x] **Model selector: page navigation (`tk-9pr1`)** — PgUp/PgDn support in picker.
- [x] **Sessions: lightweight titles and summaries (`tk-4ywr`)** — Metadata-based titles and summaries implemented in storage and picker.
- [x] **Modularize Ion TUI (`tk-2b79`)** — Componentized `internal/app/model.go` into `Viewport`, `Input`, `Broker`, `Picker`, and `Progress`.
- [x] **Approval UX overhaul (`tk-k4hv`)** — Redesigned 3-mode system (READ/EDIT/AUTO) and category-scoped auto-approval ("Always" key) implemented.
- [x] **Agent Compaction Tool (`tk-pw3s`)** — `compact` tool implemented in Ion.
- [x] **RPC/print mode (`tk-r1wx`)** — One-shot query mode and JSONL-friendly scripting surface implemented.
- [x] **Sandbox support (`tk-8s0h`)** — Opt-in bash sandbox planning added with `off`/`auto`/`seatbelt`/`bubblewrap` modes.
- [x] **Retry behavior (`tk-kz3k`)** — Native providers retry transient generation and streaming errors automatically before surfacing a final failure.
- [x] **Canto Context Governor (`tk-4ft8`)** — Runtime now auto-compacts on overflow and proactively compacts before a turn when session usage is near the context limit.
- [x] **Agent Loop: UX Streaming & Reflection Prompts (`tk-hgp4`)** — Background compaction, configurable tool/thinking verbosity, reflexion processor for failed tool calls.
- [x] **Review fixes (`tk-uzoz`, `tk-c0ci`, `tk-l9ag`)** — Registered reflexionProcessor, fixed compaction-failure hang, unified Plane A/B verbosity, added normalizeVerbosity validation.

## Active Tasks
See `tk ls` for the full list. Current active priority:
- `tk-mmcs` — Core parity plan and task queue hygiene

Remaining P2 work:
- `tk-9n7h` — Provider/model picker correctness
- `tk-o0iw` — ACP: Add initial session context at `Open()`
- `tk-2ffy` — ACP: Filter/log stderr separately instead of emitting `session.Error`
- `tk-6zy3` — ACP: Add `token_usage` event mapping

P3 follow-ups:
- `tk-369n` — Canto typed thinking capabilities and provider translation
- `tk-5cqs` — Slash command surface review
- `tk-kvqv` — Collapse routine tool output by default
- `tk-tilu` — Show thinking state without exposing hidden reasoning
- `tk-vxet` — Noninteractive prompt mode for automated agent-loop testing (completed JSON/text foundation; keep extending with fixtures as Gate 2 grows)
- `tk-st4q` — ACP agent/headless mode after bridge correctness
- `tk-g78q`, `tk-8174` — Skills marketplace and cross-host branching after the solo loop is proven

P4 follow-ups:
- `tk-n0n4` — Privacy: continue only for concrete leak surfaces or before broader logging/telemetry expansion

## Blockers
- None for Gate 1. Core-loop reliability work remains active under Gate 2.

## Topic Files
- `ai/SOTA-REQUIREMENTS.md` — The 14 core SOTA product responsibilities.
- `ai/research/canto-dspy-app-patterns-2026-04.md` — Future Ion patterns from Canto authoring work; DSPy is one reference.
- `ai/research/pi-current-core-loop-review-2026-04.md` — Current Pi core-loop, `/tree`, compaction, and UX review.
- `ai/review/canto-research-delta-2026-04-26.md` — Recent Canto ai/ findings that affect Ion sequencing.
- `ai/specs/tools-and-modes.md` — Permission modes spec
- `ai/specs/status-and-config.md` — Status line, model picker metadata, and config/state/trust layout
- `ai/specs/security-policy.md` — YAML policy config and LLM judge deferral boundary
- `ai/specs/subagent-personas-and-routing.md` — Subagent personas, YAML frontmatter, and model routing
- `ai/specs/workspace-trust-and-rollback.md` — Workspace trust state and rollback deferral boundary
- `ai/specs/tool-loading-and-approval-tiers.md` — search_tools UX and approval tier policy
- `ai/specs/memory-search-and-wiki.md` — /memory UX and wiki deferral boundary
- `ai/specs/swarm-mode-and-inline-subagents.md` — Inline subagent rendering, future swarm mode
- `ai/research/pi-architecture.md` — Pi-mono architecture analysis
- `ai/research/ion-architecture.md` — Ion architecture analysis
- `ai/design/cross-pollination.md` — Pi → canto/ion actionable insights
- `ai/DESIGN.md` — Architecture and event flow (Merged with SOTA)
- `ai/DECISIONS.md` — Decision log
