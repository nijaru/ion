---
date: 2026-04-27
summary: Synthesis of Ion and Canto ai/ docs for the native core-loop redesign plan.
status: active
---

# Core Loop AI Corpus Synthesis

## Scope Reviewed

Full inventory pass:

- `./ai/` root, plans, specs, research, review, and sprint notes.
- `../canto/ai/` root, design, review, research, sprint notes, and Ion friction files.

Deep-read pass focused on documents that constrain the native loop:

- Ion: `STATUS.md`, `PLAN.md`, `DESIGN.md`, `DECISIONS.md`.
- Ion: `review/core-loop-contract.md`, `review/core-loop-review.md`, `review/canto-research-delta-2026-04-26.md`.
- Ion: `specs/testing-models.md`, `specs/tui-architecture.md`, `specs/status-and-config.md`, `specs/tools-and-modes.md`, `specs/system-prompt.md`.
- Ion: `research/pi-current-core-loop-review-2026-04.md`, `research/core-agent-reference-delta-2026-04-27.md`.
- Canto: `STATUS.md`, `PLAN.md`, `DESIGN.md`, `DECISIONS.md`.
- Canto: `review/canto-ion-roadmap-2026-04.md`, `review/framework-readiness-2026-04-20.md`, `review/test-quality-and-rewrite-gap-2026-04.md`, `review/load-bearing-coverage-audit-2026-04.md`.
- Canto: legacy Ion friction files that are now consolidated into `review/ion-feedback-tracker-2026-04-28.md`.
- Canto: `research/message-boundary-research-2026-04.md`, `research/model-context-contracts-2026-04.md`, `design/model-context-contract-2026-04.md`, `design/identity-first-workspace-and-projections-2026-04.md`.
- Shared research pair: `agent-loop-orchestration-sota-2026-04.md`, `tool-execution-orchestration-sota-2026-04-04.md`, `session-durability-sota-2026-04.md`.

The remaining docs were indexed by summary/headings so they can be pulled in if a later module touches their topic.

## Synthesis

### 1. The Split Is Correct, But The Active Design Needs To Be More Concrete

Canto/Ion should not be collapsed into one codebase. The corpus consistently says:

- Canto is mechanism: durable event log, transcript/context/hidden lanes, effective projections, prompt construction, provider prep, agent loop, tool lifecycle, approval wait-state seams, context/governor behavior, and framework examples.
- Ion is product: TUI/CLI control plane, command catalog, settings/state/trust, product tool choices, approval copy/policy defaults, display projection, and user workflow.

The problem is not the existence of the split. The problem is that Ion's native adapter has been allowed to become a second agent loop/display-store hybrid.

### 2. The Real Boundary Is Transcript / Context / Hidden Events

The most important Canto design correction is the message-boundary work:

- `MessageAdded` is transcript.
- `ContextAdded` is model-visible non-transcript context.
- lifecycle, approval, routing, audit, projection, and UI status are hidden unless explicitly summarized into context.
- provider requests are late projections, not durable state.

Ion's storage/replay and live rendering must preserve this lane separation. Several observed bugs fit this pattern: duplicated messages, slash commands creating session artifacts, stale progress errors, and replay formatting drift are all symptoms of unclear lane ownership.

### 3. Pi/Codex/Claude References Agree On A Simple Hardened Loop

The useful reference pattern is not a large feature surface. It is a small reliable loop:

1. classify input
2. append user turn
3. build provider request from projected history
4. stream assistant output
5. execute tools with deterministic approval/finalization order
6. append observations
7. stop with an explicit terminal reason
8. resume from durable state and continue

Pi is the best core-loop behavior reference. Codex is the scripting/CLI reference. Claude Code is useful for public UX expectations and safety surfaces, but Ion should avoid copying complex modes or extension machinery before the loop is stable.

### 4. Canto Should Own More Of The Turn Contract Than Ion Currently Assumes

Canto owns:

- turn start/finish events
- step start/finish events
- assistant payload validation
- tool-call/tool-result pairing
- retry/withholding/escalation mechanics
- provider-visible history validity
- per-session execution coordination

Ion should consume these facts. It should not infer terminal state from scattered live UI events when Canto can provide a durable event.

### 5. Ion Still Needs Product-Side State Machines

Ion cannot be a passive renderer. It owns state machines for:

- local command dispatch while idle or active
- composer/follow-up queue behavior
- progress/status display
- approval prompt UI
- settings/model/provider picker UI
- startup/resume/session picker flow
- print CLI final text/JSON output

Those state machines must be kept outside model-visible transcript persistence.

## Target Design Refinement

The native loop should be designed as three stacked projections:

| Projection | Owner | Input | Output | Must Not Do |
| --- | --- | --- | --- | --- |
| Provider projection | Canto | durable transcript/context events | provider-ready `llm.Request` | mutate durable state or include hidden events |
| Host event projection | Ion backend adapter | Canto session events + stream chunks | `internal/session.Event` | persist model-visible transcript |
| Display projection | Ion storage/app | Canto effective entries + Ion display events | rendered transcript entries | alter provider-visible history |

This is the core design. Every refactor should remove code that crosses these projection boundaries.

## Proposed Module Responsibilities

### Canto

| Package | Responsibility In Core Loop | Review Questions Before Coding |
| --- | --- | --- |
| `session` | Append-only transcript/context/hidden events; effective history; snapshots/projections. | Are invalid legacy rows sanitized in every effective view? Are context markers typed instead of parsed? |
| `prompt` | Build per-turn neutral request with stable/dynamic context and cache-prefix metadata. | Can processors mutate history incorrectly? Are cache-prefix inserts helper-based? |
| `llm` / providers | Provider capability prep and wire conversion. | Is preparation copy-based? Are tool/thinking/role invariants validated before send? |
| `agent` | Step/turn loop, assistant commit, tool execution, terminal stop reasons. | Do streaming and non-streaming paths share payload validation and terminal events? |
| `runtime` | Host entry points, session loading, coordination, watch/submit orchestration. | Is `SendStream` the single normal host path? Are cancellation and queue semantics explicit? |
| `tool` / `approval` | Tool lifecycle, ordered preflight/finalization, wait-state seams. | Can tool failures become durable observations without corrupting the loop? |

### Ion

| Package | Responsibility In Core Loop | Review Questions Before Coding |
| --- | --- | --- |
| `internal/backend/canto` | Bootstrap Canto and adapt native events to Ion events. | Is `SubmitTurn` only orchestration, not a second loop? Are goroutine/cancel/close semantics testable? |
| `internal/storage` | Merge Canto effective entries with Ion display-only events. | Are duplicate model-visible entries impossible? Are routine compactions display-only? |
| `internal/app` | TUI state, command routing, display, approval UI, progress. | Do slash commands avoid sessions? Do active-turn commands avoid transcript writes? |
| `cmd/ion` | Startup, print CLI, resume/continue selection, smoke harness. | Does every automation path use the same native loop? Does startup create no session? |
| `internal/session` | Host-facing event/entry vocabulary. | Does it distinguish durable display events from provider-visible transcript? |
| `internal/backend/*/tools` | Ion-owned coding tools and product policy. | Are product tools separate from Canto framework primitives and approval seams? |

## Design Decisions To Lock Before Implementation

### Decision 1: No Dual Transcript Writer

Ion may print a live user row, but Canto is the only durable writer of user, assistant, and tool transcript. Ion display persistence is limited to display-only rows.

### Decision 2: Resume Is Projection, Not Recovery Mutation

Legacy bad sessions become safe because effective projection is strict. Ion must not patch individual corrupted sessions as a workaround.

### Decision 3: One Normal Turn Path

Normal Ion turns use Canto `Runner.SendStream`. `RunStream` remains advanced/manual. If Ion needs preflight behavior, it should happen before calling `SubmitTurn` without appending transcript.

### Decision 4: Terminal State Is Durable Before Ready

Ion should not return visually to ready until the relevant terminal state is durable or a failure is explicitly UI-only.

### Decision 5: Scriptable CLI Is The Automation Gate

The TUI remains manually/tmux-smoke tested for visual behavior, but `ion -p`, `--continue -p`, and `--resume <id> -p` are the core regression harness.

## Refactor Plan Before Coding

### Gate A: Design Closure

Exit criteria:

- `ai/design/native-core-loop-architecture.md` and this synthesis agree on projections, ownership, and module boundaries.
- `ai/PLAN.md`, `ai/STATUS.md`, and `tk ready` identify `tk-s6p4` as the blocker.
- Any implementation scratch is either committed as tests after approval or explicitly left uncommitted.

### Gate B: Canto Contract Audit

Output before code changes:

- A short Canto review note listing exact contract gaps by package.
- For each gap, classify as:
  - test-only proof
  - framework bug
  - Ion adapter misuse
  - deferred non-core feature

No Canto implementation should start until this classification is written.

### Gate C: Ion Native Adapter Design

Output before code changes:

- A file-level refactor map for `internal/backend/canto`.
- The intended `SubmitTurn` phase list.
- The event translation table from Canto event type to Ion event type.
- The close/cancel/wait semantics.

### Gate D: Ion Storage/Replay Design

Output before code changes:

- The display projection contract for Canto effective entries plus Ion-only display events.
- Duplicate-prevention invariants.
- Replay/live rendering path diagram.

### Gate E: App/CLI Lifecycle Design

Output before code changes:

- Command classification table.
- Startup/resume/session materialization table.
- Print CLI smoke matrix.
- Progress/error clearing rules.

### Gate F: Implementation Slices

Implementation starts only after Gates A-E are written. Each slice needs:

- one owner boundary
- deterministic tests first or paired with the change
- live Fedora/local-api smoke only after deterministic tests pass
- commit/push per coherent green slice

## Priority Ordering

1. Finish design closure and contract audit.
2. Canto contract tests/fixes only where Canto owns the gap.
3. Ion backend adapter spine.
4. Ion storage/replay projection.
5. Ion app/CLI lifecycle.
6. TUI header/help/autocomplete polish.
7. Safety/modes/sandbox/ACP/subagents/thinking expansion.

## Current Scratch State

There is uncommitted Canto test scratch in `../canto/agent/agent_test.go` from an interrupted implementation attempt. Do not build on it until the Canto contract audit is written. It can later be kept, rewritten, or removed as part of a deliberate Canto test slice.
