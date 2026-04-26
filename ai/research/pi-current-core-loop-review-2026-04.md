---
date: 2026-04-26
summary: Current Pi core-loop, /tree, compaction, and UX review with Canto/Ion adoption guidance.
status: active
---

# Current Pi Core Loop Review

## Answer

Ion's design still makes sense: a Go Bubble Tea coding app over Canto's framework primitives is the right split. The Pi review strengthens, not replaces, the current priority: make the solo agent loop deterministic, resilient, and replayable before adding broader SOTA features.

Pi is the strongest reference for core-loop shape right now. Claude Code, Codex, and Droid remain good references for workflow polish and safety surfaces. OpenCode is less useful for Ion's product taste. Amp, Letta, DSPy, GEPA, and similar systems are idea sources, not product templates.

## Reviewed Sources

Local checkout: `/Users/nick/github/badlogic/pi-mono` at `09e9de57` (`fix(tui): stop heading underline leaking into padding`).

Primary files:
- `packages/agent/src/agent-loop.ts`
- `packages/coding-agent/src/core/agent-session.ts`
- `packages/coding-agent/src/core/session-manager.ts`
- `packages/coding-agent/docs/tree.md`
- `packages/coding-agent/docs/compaction.md`
- `packages/coding-agent/src/modes/interactive/components/tree-selector.ts`
- `packages/coding-agent/test/agent-session-tree-navigation.test.ts`
- `packages/coding-agent/test/agent-session-concurrent.test.ts`
- `packages/coding-agent/test/agent-session-runtime-events.test.ts`
- `packages/coding-agent/test/agent-session-retry.test.ts`

## What Pi Gets Right

### 1. Lifecycle Events Are the Contract

Pi's agent loop emits ordered lifecycle events: agent start/end, turn start/end, message start/update/end, and tool execution start/update/end. Event sinks are awaited, so UI, session persistence, and extension hooks observe the same order.

Key lesson for Ion/Canto: terminal turn events must never race ahead of message commit, tool result commit, or terminal error persistence. `message_end` before `turn_end` is the important invariant.

### 2. Streaming State Is Explicit

Pi inserts a partial assistant message into context on stream start, updates it during deltas, and replaces it with the final message on done or error. This makes "what is the current transcript?" answerable throughout a turn.

Ion should keep this principle at the app boundary: the transcript, session log, and backend events should agree about partial, committed, cancelled, and errored messages.

### 3. Steering and Follow-Up Queues Are Different

Pi distinguishes steering messages from follow-up messages:
- steering is injected while the current loop is still active, before the next assistant response
- follow-up runs only when the agent would otherwise stop

Ion's queued input should preserve this distinction eventually. For now, follow-up queuing is enough, but tests should make the current behavior explicit so future steering work does not blur the semantics.

### 4. Tool Calls Preserve Order Without Killing Parallelism

Pi prepares tool calls sequentially first, so blocking/approval hooks happen in deterministic order. Runnable tools can execute concurrently, but finalization and emitted results are restored to original assistant source order. Tool failures become tool-result messages instead of fatal loop panics.

Canto should own the durable tool lifecycle contract. Ion should render that contract without inventing extra ordering rules in the adapter.

### 5. Session Tree Is Append-Only with a Leaf Pointer

Pi's `/tree` is not an editor over old history. It is an append-only JSONL session tree with `id`, `parentId`, and a mutable `leafId` pointer. Context is rebuilt by walking from leaf to root.

That is the right model for Canto once tree navigation becomes a priority. It gives branching, labels, summaries, and replay without mutation or compatibility paths.

### 6. `/tree` UX Is Minimal but Complete

Pi's tree selector supports active-path visibility, folding, labels, filters, user-message selection, and branch summarization. Selecting a user message sets the leaf to its parent and returns the text for re-submission; selecting an assistant or custom message sets the leaf directly.

Ion should copy the behavior model later, not the exact UI. The Rust-era/Bubble Tea visual direction still fits Ion better than Pi's very minimal presentation.

### 7. Compaction and Branch Summaries Share a Format

Pi uses structured summaries with cumulative read/modified file tracking. Branch summarization summarizes the abandoned path from old leaf to common ancestor, stopping at compaction boundaries.

Canto should converge on this as a framework primitive. Ion should expose compact progress and summaries without turning the transcript into a log dump.

### 8. Tests Encode Runtime Semantics

Pi has focused tests for concurrent prompt behavior, retry completion, tree navigation, branch summary cancellation, runtime events, and no-op navigation. These are better references than broad snapshot tests.

Ion's next tests should target event order, queued-turn behavior, cancellation, immediate backend errors, tool failure conversion, persistence, and replay.

## Adopt for Canto

### P0: Core Framework Contracts

- Define event-order invariants: message/tool terminal events precede turn terminal events.
- Represent all terminal states as logged events: success, cancellation, backend error, tool error, budget stop, max steps.
- Add a deterministic provider/test harness instead of mock-heavy loop tests.
- Keep tool lifecycle ordering stable: ordered approval/prepare, optional parallel execution, ordered finalization.
- Make streaming transcript state explicit enough for replay and recovery.

### P1: Session and Context Primitives

- Add append-only session tree primitives with a `leafEventID`.
- Add branch summaries and structured compaction as sibling concepts.
- Track read/modified files across compactions and summaries.
- Add cross-provider message transformation for thinking/tool content.

### Defer

- Hot-reloadable extension runtime.
- Package ecosystem.
- General UI component abstractions.
- Provider API registry changes unless provider boilerplate becomes the bottleneck.

## Adopt for Ion

### P0: Core Loop Reliability

- Fix `CantoBackend` event translation so assistant commit is emitted before `TurnFinished`.
- Add regression tests for event ordering and queued follow-up timing.
- Audit cancellation and budget-stop paths so they commit terminal state before returning to ready.
- Audit tool result ordering and failure rendering against Canto's emitted lifecycle.
- Verify persisted session replay matches the visible transcript after success, cancellation, and error.

### P1: Product UX After Core Stability

- Add explicit queued-input semantics: current follow-up behavior first, steering only when Canto exposes a clean contract.
- Add `/tree` only after Canto has session-tree primitives. Use Pi's behavior model: active path, folds, labels, filters, user-message resubmit, branch summaries.
- Keep Ion's current Bubble Tea visual direction. Pi's behavior is better than its sparse styling for Ion.
- Surface compaction/branch summaries compactly in status or a focused overlay.

### P2: Later Ideas

- Command autocomplete for built-ins.
- Timeline entries for model/thinking changes.
- Branch-aware local state stored in session entries.
- Model/cost routing explanations that remain inspectable and small.

## Reject or Keep Out of the Main Path

- Extension marketplace as a near-term goal.
- OpenCode-style broad mode complexity.
- DSPy/GEPA-driven product shape before the core coding loop is reliable.
- Large configuration cascades unless user-editable config actually requires them.
- Background shell/session managers as a substitute for a reliable foreground loop.

## Immediate Work

1. Finish `tk-96vy` before resuming model-cascade work.
2. Land the Ion event-ordering regression test and fix.
3. Continue the core-loop audit in small slices: cancellation, immediate errors, queued follow-ups, tool failures, persistence/replay.
4. Create Canto tasks for session tree and structured compaction after the Ion loop audit has a green baseline.
