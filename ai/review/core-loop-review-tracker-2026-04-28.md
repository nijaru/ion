---
date: 2026-04-28
summary: Review/refactor coverage tracker for the native Canto/Ion core loop.
status: active
---

# Core Loop Review Tracker

Use this as the scan-first checklist for `tk-s6p4`. `Reviewed` means the area had code review plus deterministic proof. `Refactored` means code changed to enforce the target contract. `Pending` means the area still needs a focused pass before P2/P3 feature work resumes.

## Target Contract

- Canto owns provider-visible history, effective projection, agent execution, tool lifecycle events, queueing, retries, terminal durability, and compaction primitives.
- Ion owns input classification, TUI/CLI lifecycle, display projection, local status/error rows, mode/trust UX, and provider/config selection.
- Native path is the active product path. ACP remains secondary until the native loop is stable.

## Canto Coverage

| Area | State | Evidence | Notes |
| --- | --- | --- | --- |
| Session effective history projection | Reviewed/refactored | Canto filters invalid empty assistant rows; Ion imports projection sanitation fixes. | Projection is legacy/corrupt-history defense, not the only guard. |
| Assistant write-side validation | Reviewed/refactored | Canto `52206f2`; `go test ./...` in Canto and Ion passed after import. | Whitespace-only assistant payloads are rejected; reasoning/thinking-only payloads preserved. |
| Tool failure durability | Reviewed/refactored | Canto `a5878ab`; Ion maps `ToolCompletedData.Error` to live/replay error display. | Errored routine tool output stays expanded in Ion replay. |
| Cancellation terminal events | Reviewed/refactored | Canto `c22da5e`; Ion suppresses wrapped context-canceled provider error. | Canceled streaming and non-streaming turns persist `TurnCompleted`. |
| Serial queue wait vs execution context | Reviewed/refactored | Canto `595380a`; Ion imported and full suite passed. | Fixed wait-timeout context canceling active turns or later executing with expired contexts. |
| Provider request construction / system-message ordering | Reviewed/refactored | Fedora/local-api system-message issue fixed via Canto context primitives; Ion request-shape tests exclude display-only events. | Provider-visible history stays in Canto; Ion status/system display rows stay local. |
| Retry classification/runtime retry loop | Partially reviewed | Transport-only retry-until-cancel path exists; OpenRouter 429 only proved status path. | Live validation remains provider/environment blocked. |
| Compaction primitives | Partially reviewed | Proactive/manual compaction paths have deterministic Ion coverage. | Keep enabled as core resilience, but avoid P2 compaction UX until core loop gate is green. |
| Memory/workflow/subagent primitives | Deferred | Disabled in Ion via `CoreLoopOnly`. | Re-enable only after native loop gate is green. |

## Ion Coverage

| Area | State | Evidence | Notes |
| --- | --- | --- | --- |
| CantoBackend assistant commit translation | Reviewed/refactored | `TurnCompleted` no longer creates empty assistant display commit; assistant rows come from real Canto `MessageAdded`. | Prevents empty assistant replay/provider-history bugs. |
| CantoBackend terminal error ordering | Reviewed/refactored | Provider errors translate to one `Error` then `TurnFinished`. | Avoids racing `SendStream` returned errors. |
| CantoBackend cancellation handling | Reviewed/refactored | `context canceled` terminal events settle as `TurnFinished`, not provider error. | Preserves user-cancel state. |
| CantoBackend single active turn | Reviewed/refactored | Overlapping `SubmitTurn` rejected; watcher exits on `TurnCompleted`; race-focused backend tests passed. | Prevents duplicate watchers and duplicate translated events. |
| CantoBackend terminal active-state clearing | Reviewed/refactored | Active state clears before emitting `TurnFinished`; full suite passed. | Queued/immediate follow-up turns no longer race terminal settlement. |
| Ion storage model-visible writes | Reviewed/refactored | `1b0e3e1`; storage rejects non-empty `User`/`Agent`/`ToolUse`/`ToolResult` appends. | Canto is now the only provider-history writer. |
| Display replay ordering | Reviewed/refactored | Ion display-only events interleave with Canto effective history by raw event order. | Fixes cancel/error/system rows replaying after later turns. |
| Routine tool display compaction | Reviewed/refactored | Routine success output collapses; error output remains expanded. | UI transform only, not provider-history mutation. |
| Print CLI preflight | Reviewed/refactored | Invalid print args and missing prompt/stdin fail before runtime/storage init. | Prevents no-prompt `-p` from creating sessions. |
| Print CLI settlement | Reviewed/refactored | Event-stream close before `TurnFinished` is an error; print closes runtime handles. | `ion -p` is the automation surface. |
| TUI shutdown cleanup | Reviewed/refactored | `063e7a5`; TUI closes agent session, storage session, and store after Bubble Tea exits. | Matches print-mode cleanup. |
| Slash/local command persistence | Reviewed/refactored | Slash errors use local UI error path; real-store `/help` lazy-session regression passes. | Slash commands do not create model-visible transcript or recent session rows. |
| Slash/local commands during active turns | Reviewed/refactored | Slash commands now bypass follow-up queueing during active turns; provider picker and unknown-command regressions pass. | Slash-like input must never be sent to the model as a queued follow-up. |
| Runtime switch/resume failure cleanup | Reviewed/refactored | Switch/resume closes newly opened handles on save/replay failure and preserves old runtime. | Needs final command-path review, but root leak class has coverage. |
| Provider/model metadata preservation | Reviewed/refactored | Submit metadata preserves provider-qualified model names. | Keeps `/resume <id>` working for local/custom providers. |
| ACP prompt completion | Reviewed/refactored | ACP no longer emits empty assistant commit after prompt completion. | ACP remains secondary and still has P2 follow-ups. |
| Startup/resume rendering | Reviewed/refactored | Resumed marker after launch header; replay entries use shared renderer spacing. | Header visual polish is P3. |
| Startup/continue/resume materialization | Reviewed/refactored | Real-store `openRuntime` tests cover fresh lazy startup, invalid-provider startup, and invalid-provider explicit resume. | Local session selection no longer depends on provider config being immediately usable. |
| App queued follow-up lifecycle | Reviewed/refactored | Queued follow-up after `TurnFinished` covered. | Keep in regression set while reviewing command paths. |
| Provider-history shape after tool turns | Reviewed/refactored | Resumed tool follow-up asserts assistant tool-call before matching tool-result, no empty assistant rows, and durable prior/new user turns. | Covers the provider-history failure class that previously broke Fedora/local-api. |
| Trust/mode/approval UX | Partially reviewed | Basic mode/trust paths covered; `CoreLoopOnly` keeps advanced surfaces down. | Secondary to stable submit/stream/tool/cancel/error/persist/replay. |
| Live local-api/OpenRouter validation | Blocked/partial | Fedora off; OpenRouter DeepSeek hit 402; Minimax free model entered retry/response wait and hit the 90s smoke deadline. | Deterministic tests are the proof path until a live provider is available. |

## Current Gaps

1. ACP bridge P2s: stderr filtering, initial session context, token usage mapping. Keep behind native-loop gate unless ACP blocks tests.
2. Live smoke when Fedora or a funded model is available: tool call, persist, resume, follow-up turn, and `ion -p --resume <id>`.

## Latest Verification

- `go test ./... -count=1`
- `go test -race ./cmd/ion ./internal/app ./internal/backend/canto -count=1`
- `ION_LIVE_SMOKE=1 ION_SMOKE_PROVIDER=openrouter ION_SMOKE_MODEL='minimax/minimax-m2.5:free' go test ./cmd/ion -run TestLiveSmokeTurnAndToolCall -count=1 -v` opened runtime and started the turn, then timed out at the 90s live-smoke deadline after provider retry status.
