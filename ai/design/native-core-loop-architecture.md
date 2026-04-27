---
date: 2026-04-27
summary: Target architecture and refactor sequence for the native Canto/Ion core loop.
status: active
---

# Native Core Loop Architecture

## Purpose

Ion must reach Pi/Codex/Claude-level reliability by design, not by isolated bug fixes. The native loop is the product floor:

`Ion CLI/TUI -> CantoBackend -> Canto runtime/agent/session -> provider API`

ACP, sandboxing, subagents, routing, broad privacy, skills, and richer thinking controls are downstream. They may add policy and presentation, but they must not change the core loop's ownership model.

## Verdict

Do not rewrite Canto or Ion wholesale. The Canto/Ion split still makes sense, but the native path needs a top-down refactor because current Ion code lets too many concerns share the same functions:

- `Canto` already owns append-only model-visible history and the agent/tool loop.
- `Ion` should be a thin product adapter around Canto, plus TUI/CLI UX.
- The unstable parts are adapter boundaries, replay/display projection, and lifecycle coordination, not the idea of a framework/application split.

The refactor target is a smaller spine with explicit phases and one durable owner for every event type.

## Non-Negotiable Invariants

| Invariant | Owner | Consequence |
| --- | --- | --- |
| Provider-visible history is always valid. | Canto | No empty assistant messages; tool results pair with assistant tool calls; system/developer/context placement is provider-compatible. |
| Model-visible transcript has one writer. | Canto | Ion never persists user/assistant/tool transcript duplicates. |
| UI-local events are separate from model history. | Ion | Status, cancellation notices, routing metadata, subagent breadcrumbs, and UI errors may persist only as display events. |
| Local commands are not model turns. | Ion | Slash commands, picker changes, `/resume`, and startup never materialize sessions or append model-visible transcript. |
| Every turn has one terminal shape. | Canto primary, Ion display | Success, cancellation, provider error, provider-limit stop, tool error, and compaction failure must all resume and accept a follow-up turn. |
| Replay equals live display. | Ion | Resume, continue, and live streaming share the same entry renderer and spacing rules. |
| Policy pauses cannot corrupt the loop. | Canto primitives, Ion policy | Approval wait/deny/error affects tool execution, not provider-history validity. |

## High-Level Flow

### 1. Input Classification

Ion classifies input before touching Canto:

- local command: route to command handler
- local state change: update config/state/session picker only
- model turn: continue to session materialization

Only model turns may call `SubmitTurn`.

### 2. Session Materialization

Ion owns product session selection and metadata, but Canto owns durable transcript.

- Startup creates no durable session.
- `/resume`, bare `--resume`, and `--continue` select a session but do not append transcript.
- The first model-visible turn materializes the lazy Ion/Canto session.
- Metadata updates happen once per real model turn and must not imply transcript writes.

### 3. Turn Submission

Ion calls one Canto entry point for model-visible turn execution:

1. append user message
2. emit turn start
3. build provider request from effective history
4. stream/generate assistant response
5. append assistant message if it has payload
6. execute tools and append tool results
7. emit terminal turn completion

Ion should not reimplement any of those steps.

### 4. Event Translation

Canto emits durable session events. Ion translates them into product display events:

- user message -> user entry
- assistant content -> assistant entry/delta
- reasoning/thinking -> compact thinking state by default
- tool start/result -> compact routine tool entry by default
- turn terminal/error/retry/status -> status/error entry

Translation must be deterministic and idempotent. A replayed event and a live event must produce the same display shape.

### 5. Display Persistence

Ion's storage wrapper reads Canto effective entries and appends Ion-only display events.

Allowed Ion display events:

- local system notices
- cancellation notice
- provider/routing/status metadata
- token usage snapshots
- subagent breadcrumbs
- UI-only error/status entries

Disallowed Ion display events:

- duplicate model-visible user messages
- duplicate assistant messages
- duplicate tool results
- slash-command transcript entries that masquerade as model history

### 6. Resume And Continue

Resume is a projection operation:

1. load selected session
2. read Canto effective entries
3. merge Ion display-only events
4. normalize and compact for UI
5. render with the same renderer as live entries
6. accept a follow-up model turn through the normal submission path

No legacy cleanup mutates old sessions. Projection sanitizes bad legacy rows; write-side tests prevent new bad rows.

## Canto Target Design

Canto remains the framework/mechanism layer.

### Session

`session` is the canonical model-visible log and projection layer.

- Append-only events remain the truth.
- `EffectiveMessages` and `EffectiveEntries` are the only provider-visible projections Ion should consume.
- Projection sanitizes legacy invalid assistant rows from raw events, snapshots, and post-snapshot appends.
- Session tests must cover raw, snapshot, and post-snapshot invalid assistant rows.

### Runtime

`runtime.Runner` is the canonical host entry point for turns.

- `SendStream` appends the user message and runs the agent.
- `RunStream` is advanced/manual and should not be used by Ion for normal turns.
- `Watch` exposes session events for UI translation.
- Per-session coordination stays inside Canto.

### Agent

`agent` owns request build, provider stream/generate, assistant commit, tool orchestration, and turn terminal events.

- Streaming and non-streaming paths must share assistant-payload validation.
- Tool-only, reasoning-only, and thinking-only assistant messages are valid.
- Tool results must preserve provider IDs and names.
- Error handling must append terminal turn events even when provider/tool execution fails.

### Middleware

Middleware hooks belong at explicit Canto boundaries:

- before provider request
- before tool call
- after tool result
- turn terminal observer

They may transform requests/results or emit audit/status, but may not append duplicate model-visible transcript outside the core path.

## Ion Target Design

Ion remains the application/product layer.

### Backend Adapter

`internal/backend/canto` should become a small native-loop adapter with three responsibilities:

- bootstrap Canto with Ion tools, config, policy, memory, and providers
- submit model turns through `runtime.Runner.SendStream`
- translate Canto session events and stream chunks into `internal/session.Event`

Refactor direction:

- split `SubmitTurn` into named phases: ensure session, subscribe, start runner, translate stream chunks
- move event translation into small, tested functions
- keep goroutine/cancel ownership obvious and bounded
- do not persist model-visible transcript here

### Storage/Replay

`internal/storage` owns Ion display projection.

- consume `EffectiveEntries` for model-visible history
- merge display-only Ion events
- apply UI-only compaction for routine tools
- normalize invalid display-only assistant entries defensively
- expose the same entry list used by live rendering

It must not compensate for Canto history bugs except by relying on Canto's effective projections.

### App/Broker

`internal/app` owns TUI state, command routing, and rendering.

- classify slash/local commands before model submission
- print the user entry once in live UI
- do not persist the user entry as model history
- clear stale errors/status when session/model/provider changes
- allow safe local commands during active turns without touching Canto transcript

### CLI

`cmd/ion` owns automation-friendly execution.

- `-p`, `--print`, stdin, `--continue -p`, and `--resume <id> -p` use the same native loop.
- JSON output remains stable enough for regression smoke tests.
- Bare `--resume` opens the selector in TUI mode and never creates a session by itself.

## Test Matrix

### Canto Deterministic

- streaming empty assistant is not appended
- non-streaming empty assistant is not appended
- tool-only assistant is preserved
- reasoning/thinking-only assistant is preserved
- raw/snapshot/post-snapshot invalid assistant rows are omitted from effective history
- tool result ordering and IDs survive replay
- provider error and context overflow leave a terminal turn event

### Ion Deterministic

- slash command before first model turn creates no session
- submit text turn prints/persists user message exactly once
- assistant/tool entries are not duplicated across live and replay paths
- cancellation persists and replays a terminal display state
- provider error, provider-limit stop, retry status, and tool error replay cleanly
- resumed tool session can accept a follow-up turn
- startup/resume marker ordering and blank-line spacing match live rendering
- stale progress errors clear on provider/model/session changes

### Ion Live Fedora/Local-API

Use the scriptable CLI before manual TUI checks:

```sh
go run ./... -p --json --timeout 60s 'reply with the single word ok'
go run ./... --mode auto -p --json --timeout 60s 'Use the bash tool exactly once to run `echo ion-smoke`, then reply with the single word done.'
go run ./... --resume <session-id> -p --json --timeout 60s 'reply continued'
```

Live smoke proves provider compatibility and broad path health; deterministic tests prove contracts.

## Refactor Sequence

### Phase 0: Coordination Reset

- Mark broad "core loop is green" claims stale.
- Keep `tk-s6p4` as the active P1 design/refactor task.
- Keep feature polish tasks open but downstream.

### Phase 1: Canto Write/Projection Audit

- Verify streaming and non-streaming assistant commit paths.
- Add missing Canto tests for write-side invalid assistant prevention and terminal error events.
- Fix Canto first when the bug is framework-owned, commit, push, and import the revision into Ion.

### Phase 2: Ion Native Backend Spine

- Refactor `CantoBackend.SubmitTurn` around explicit phases.
- Add tests around event ordering, cancellation, and close/wait behavior.
- Ensure stream chunk display is display-only and does not compete with durable event replay.

### Phase 3: Ion Storage/Replay Projection

- Simplify storage projection around Canto `EffectiveEntries`.
- Assert no duplicate model-visible entries.
- Keep routine tool compaction purely display-side.

### Phase 4: App/CLI Lifecycle

- Prove slash/local commands do not materialize sessions.
- Harden `--continue`, bare `--resume`, and `--resume <id>`.
- Clear stale errors on state changes.
- Expand print CLI smoke coverage until manual TUI testing is only needed for visual polish.

### Phase 5: TUI Baseline Polish

- Improve launch header readability.
- Keep slash autocomplete/help readable.
- Keep thinking/tool output compact and inspectable.

## Stop Conditions

Stop and redesign again only if:

- Canto's session data model cannot express the required terminal states without duplicate writer paths.
- Ion cannot render live and replay with one projection model.
- A public Canto API change is needed that would break intended framework behavior.

Otherwise, continue with targeted refactors and tests until the matrix passes.
