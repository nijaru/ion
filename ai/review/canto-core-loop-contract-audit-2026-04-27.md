---
date: 2026-04-27
summary: Canto-owned contract audit for Ion native core-loop stabilization.
status: active
---

# Canto Core Loop Contract Audit

## Purpose

Classify Canto-side responsibilities before Ion refactors against them. This is a design/audit document, not an implementation record.

## Reviewed Inputs

- Canto docs: `ai/DESIGN.md`, `ai/STATUS.md`, `ai/PLAN.md`, `ai/DECISIONS.md`.
- Canto boundary docs: `research/message-boundary-research-2026-04.md`, `research/model-context-contracts-2026-04.md`, `design/model-context-contract-2026-04.md`.
- Canto/Ion reviews: `review/canto-ion-roadmap-2026-04.md`, `review/framework-readiness-2026-04-20.md`, `ion-framework-issues.md`.
- Canto code hotspots: `session/rebuilder.go`, `session/history.go`, `agent/loop.go`, `agent/stream.go`, `agent/tools.go`, `runtime/runner.go`.
- Ion consumer hotspots: `internal/backend/canto/backend.go`, `internal/storage/canto_store.go`.

## Current Canto Contract

| Contract | Status | Notes |
| --- | --- | --- |
| Transcript/context/hidden event lanes | Solid design, active code path | `MessageAdded` is transcript, `ContextAdded` is model-visible context, lifecycle/status/audit events are hidden unless explicitly projected. |
| Effective history projection | Mostly solid | `EffectiveEntries` / `EffectiveMessages` are the only provider-visible history Ion should consume. |
| Provider request prep | Solid direction | Provider-specific adaptation belongs at send boundary on request copies. |
| Normal host turn path | Solid direction | Ion should use `runtime.Runner.SendStream` for ordinary turns. |
| Agent terminal events | Needs explicit audit/test proof | `TurnCompleted` should exist for success and terminal errors before Ion returns to ready. |
| Assistant payload validation | Needs write/projection alignment proof | Projection validation trims content/reasoning; write-side behavior must be audited against that exact rule. |
| Tool event payload schema | Needs audit | Ion currently decodes raw event payloads manually. Canto should expose or document stable event data fields enough for adapters to avoid schema drift. |

## Implementation Progress

| Gap | Resolution |
| --- | --- |
| Write-side assistant payload validation | Fixed in Canto `52206f2`; write-side predicate trims content/reasoning and preserves reasoning-only payloads. |
| Terminal cancellation durability | Fixed in Canto `c22da5e`; streaming and non-streaming canceled turns append `TurnCompleted` with `context.WithoutCancel(ctx)`. |
| Tool event payload stability | Partially resolved; Ion now uses Canto typed accessors for started/completed events and preserves `ToolOutputDelta` IDs. |
| Tool error state | Fixed in Canto `a5878ab`; failed tool completions include structured `Error` text, imported by Ion for live and replay display. |

## Gap Classification

### Gap 1: Write-Side Assistant Payload Validation

Classification: possible framework bug or test-only proof.

Risk:

- Projection sanitation protects provider history, but if Canto can still append whitespace-only assistant messages, bad rows remain durable and every consumer must defensively filter.

Design requirement:

- Streaming and non-streaming assistant commit paths share exactly one payload predicate.
- Valid assistant payload means non-blank content, non-blank reasoning, thinking blocks, or tool calls.
- Tool-only, reasoning-only, and thinking-only assistant messages remain valid.

Pre-code proof needed:

- Audit `agent/message.go`, `agent/loop.go`, `agent/stream.go`, and matching tests.
- Decide whether this is already correct enough or needs a small Canto test/fix slice.

### Gap 2: Terminal Error Event Durability

Classification: framework contract proof.

Risk:

- Ion must not infer durable terminal state from a live error channel if Canto already has `TurnCompleted` error data.

Design requirement:

- Provider error, context overflow after recovery exhaustion, max-step stop, budget stop, tool error, and cancellation each leave a durable terminal shape.
- `TurnCompleted` with error/stop reason should occur before adapter emits final ready/complete state where possible.

Pre-code proof needed:

- Audit agent loop defers and runtime runner error paths.
- Add or identify tests that assert terminal events survive provider errors.

### Gap 3: Tool Event Payload Stability

Classification: framework/API clarity issue or Ion adapter misuse.

Risk:

- Ion decodes `ToolStarted`, `ToolCompleted`, and `ToolOutputDelta` via anonymous structs. If field names diverge between Canto-generated events and Ion UI-local saved events, tool rows disappear or lose IDs.

Design requirement:

- Canto event data accessors should be used where available.
- If accessors do not exist for tool lifecycle events, either add them in Canto or centralize Ion decoding in one tested adapter.

Pre-code proof needed:

- Audit Canto `session` tool event constructors/accessors.
- Build an event translation table before changing code.

### Gap 4: Retry/Withholding Semantics

Classification: framework-owned, likely already implemented enough for current gate.

Risk:

- Ion can show retry status, but retry policy and transient classification belong in Canto/provider wrappers.

Design requirement:

- Transport-only retry-until-cancel remains Canto-owned.
- Provider quota/rate/context errors must stop boundedly and durably, not retry forever.
- Ion may persist display status but must not mutate provider history during retry.

Pre-code proof needed:

- Keep current retry tests green.
- Add Ion tests only for status display and replay.

### Gap 5: Context/Instruction Boundary

Classification: framework-owned, recently corrected.

Risk:

- The Fedora Jinja/system-message issue came from privileged messages crossing into the wrong provider position.

Design requirement:

- Canto keeps privileged instructions request-prefix only.
- Durable summaries, working sets, file refs, memory, and bootstrap context replay as non-privileged context.
- Ion product prompts/instructions should enter through Canto prompt/request surfaces, not ad hoc durable `system` transcript rows.

Pre-code proof needed:

- Do not redesign unless new provider-history failures reproduce.

## Canto Work Allowed Before Ion Refactor

Only these Canto changes should happen before Ion backend refactor:

1. Contract tests that prove existing behavior.
2. Minimal framework fixes for confirmed contract gaps above.
3. Stable accessors for event payloads if anonymous decoding is the source of Ion fragility.

Everything else stays deferred.

## Canto Work Not Allowed In This Gate

- Session tree or `/tree` primitives.
- New tool search/deferred-loading work.
- New subagent/graph work.
- Broader DSPy/GEPA optimization surfaces.
- Provider thinking capability expansion unless it directly blocks the core loop.

## Output Needed Before Implementation

Before touching Canto code again:

- list exact tests to add or preserve
- identify exact files/functions
- state whether each change is proof-only or a behavior change
- if behavior changes, commit and push Canto before importing into Ion
