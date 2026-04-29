---
date: 2026-04-28
summary: Active audit tracker for the native Canto/Ion core loop.
status: active
---

# Core Loop Audit Tracker

This is the active scan-first tracker for `tk-mmcs`. It replaces the earlier optimistic reviewed/refactored matrix and prevents future work from drifting into isolated bug slices.

The previous `tk-s6p4` audit produced useful evidence, but live TUI failures on 2026-04-29 reopened the P1 gate. Treat older `reviewed` rows as prior evidence, not permission to skip the current subsystem pass. Do not reopen deferred feature work just because one slice is green.

## Current Operating Mode

Work from subsystem review to implementation:

1. Pick the next subsystem in the active sequence below.
2. Read the named files end to end and state the invariant being checked.
3. Log findings under `tk-mmcs` before editing.
4. Patch only defects or simplifications needed for that subsystem invariant.
5. Verify with focused tests, full tests, race gate when the native loop changed, and Fedora/local-api live smoke when the behavior is user-visible.
6. Only then move to the next subsystem.

The current product target is:

- **Core agent:** Pi-like minimal loop first.
- **TUI shell:** minimal Claude Code/Droid-style presentation on top of the stable loop.
- **Deferred:** trust/permissions/sandbox polish, ACP/subscription backends, privacy expansion, subagents, skills, routing, and advanced thinking controls.

## Active Review Sequence

| Seq | Subsystem | Why It Is Next | Files | Exit Evidence |
| --- | --- | --- | --- | --- |
| A1 | Native event ownership | Recent duplicate assistant output proved live/committed ownership can still drift. | `internal/backend/canto/backend.go`, `internal/app/broker.go`, `internal/app/viewport.go`, `internal/app/util.go`, `internal/session/event.go` | One event owner for assistant/tool/terminal states; focused app/backend tests; tmux TUI turn with tool use. |
| A2 | Canto provider-visible history | Every resume/follow-up bug becomes provider-history corruption if this is wrong. | `../canto/session/*`, `../canto/runtime/*`, `../canto/agent/*`, `../canto/prompt/*` | Empty/no-payload assistant impossible on writes; tool call/result ordering preserved; Canto tests plus Ion resume/follow-up smoke. |
| A3 | Ion storage and lazy lifecycle | Startup, slash commands, resume, and display-only rows must not materialize or corrupt sessions. | `internal/storage/*`, `cmd/ion/*.go`, `internal/app/commands.go`, `internal/app/model.go` | Slash/local commands before first model turn stay local; resume/continue accept follow-up; no duplicate model-visible rows. |
| A4 | Core tool boundary | Tool calls are the minimum useful coding-agent behavior and must be boring. | `internal/backend/canto/tools/*`, `internal/backend/canto/backend.go`, `internal/backend/policy.go` | Read/list/search/bash/verify deterministic tests; absolute/relative path containment; cancellation leaves no orphan process. |
| A5 | CLI smoke harness | CLI is the automation surface for proving the loop repeatedly. | `cmd/ion/print_mode.go`, `cmd/ion/main.go`, `cmd/ion/live_smoke_test.go` | `-p`, stdin, JSON, resume, tool call, timeout/error exit codes covered and live-smoked. |
| A6 | Minimal TUI baseline | TUI becomes priority only after the loop is not corrupting state. | `internal/app/render.go`, `viewport.go`, `events.go`, `input.go`, `commands.go` | Single separator, no duplicates, compact tools, queued/steering behavior defined, tmux captures clean. |

Do not start P2/P3 UI features until A1-A5 are green after the latest code changes. A6 can fix presentation defects that hide or confuse the core loop.

## Core Definition

The audit is limited to the native loop:

```text
Ion CLI/TUI -> CantoBackend -> Canto session/runtime/agent/tools -> provider API
```

Required behavior:

- submit a user turn
- stream assistant output
- run a tool and persist tool call/result state
- handle approval decisions where currently active
- cancel without corrupting state
- surface provider/network errors without wedging the UI
- retry transient failures without hiding terminal failures
- persist and resume
- accept a follow-up turn after resume
- render replay with the same display rules as live transcript

## Split And Rewrite Policy

- Keep Canto and Ion as separate repos.
- Do not merge Canto into Ion for speed.
- Treat Ion as Canto's acceptance test during core-loop stabilization.
- Defer Canto public-framework/SOTA expansion until Ion's minimal native loop is stable.
- Rewrite/refactor targeted modules when a file group violates the desired final shape; do not keep layering symptom fixes over a flawed module.
- Use `ai/` as an index now. Do not do more broad context passes unless a specific file group needs a design source.

## Freeze Policy

`features.CoreLoopOnly` is on, but it must be verified against actual call sites.

Keep active for the audit:

- Canto session event log, projection, effective history, subscriptions, and SQLite store.
- Canto runtime queue/runner, agent loop, provider call path, tool execution, terminal events, and minimal retry.
- Ion `cmd/ion` startup, print mode, resume/continue, and smoke harness.
- Ion `CantoBackend`, app input/event/render loop, storage/replay, core coding tools, and local command routing needed to drive the loop.

Disable, hide, or bypass until Gate 2 is green:

- ACP bridge behavior beyond ensuring it is not on the native path.
- MCP, memory/workflows, subagents, reflexion, branching, skills, model cascades/routing, privacy expansion, advanced thinking expansion, and sandbox/approval polish.
- Provider picker/config polish except where bad provider state blocks startup, resume, or print smoke.
- Compaction controls/polish unless compaction directly affects context survival, provider-history validity, or resumability.

## Priority Bands

These bands guide sequencing; they are not exact feature tiers.

| Band | Scope | Audit Policy |
| --- | --- | --- |
| Core | Minimal agent loop plus stable shell: submit, stream, core tools, cancel, provider error, retry status, no transcript corruption, print CLI, basic TUI display. | Active now. |
| Reliability table stakes | Continue/resume correctness, compaction/overflow recovery, durable replay in long sessions. | Include when it protects core reliability; defer UX polish. |
| Product table stakes | Robust resume UX, slash autocomplete, basic permission UX, transcript inspection. | Plan next. |
| Polish | Picker/header/help/status/thinking/tree/editor polish. | Defer. |
| Experimental/SOTA | ACP/subscriptions, subagents, skills, routing, privacy pipeline, optimizer loops, swarm. | Disable or isolate from native P1 unless directly blocking. |
 
`continue`, `resume`, and compaction straddle bands. Their minimal correctness belongs in the reliability audit; their UI/control polish waits.

## Audit Standard

For each row:

- Read the named files, not just tests or prior commits.
- State the core-loop invariants in the notes.
- Classify codepaths as kept, disabled, or deferred.
- Record findings in `tk-mmcs` before editing.
- Add or update deterministic tests for each bug fixed.
- Run the smallest relevant test first, then the broader package/full suite.

Status values:

- `pending` — not yet reviewed file by file.
- `in_review` — currently being read; no broad stability claims allowed.
- `finding` — reviewed enough to identify a concrete defect or overactive nonessential path.
- `patched` — defect fixed with focused tests, but group may still need review.
- `reviewed` — file group reviewed against invariants with tests or explicit no-change rationale.

## Canto Audit Order

| Order | Area | Files | Status | Evidence To Date | Next Check |
| --- | --- | --- | --- | --- | --- |
| C1 | Session event model and message payload validity | `../canto/session/event.go`, `message.go`, `codec.go`, `history.go`, `projection.go` | reviewed | Write-side empty assistant rejection is pushed in Canto `5576f4d`; projection/effective history sanitizes raw, snapshot, and post-snapshot invalid assistant rows while preserving content, reasoning, thinking-block-only, and tool-call-only assistants. Projection snapshots build from sanitized effective entries. | Continue C2/C3; no extra compatibility/migration path needed. |
| C2 | Session store, subscription, and replay ordering | `../canto/session/session.go`, `sqlite.go`, `jsonl.go`, `rebuilder.go`, `replayer.go`, `subscription.go`, `writethrough.go` | patched | SQLite/JSONL public Save paths reject invalid assistant writes. Replayer intentionally allows legacy rows so effective projection can sanitize old logs. Canto `d37beda` now makes raw `LastAssistantMessage` skip legacy invalid assistant rows, preventing turn finalization from reading a row that provider history omits. Session/store tests are green. | Continue runtime/agent C3-C5 review; revisit write-through drain semantics only if an active runtime path depends on it for P1 durability. |
| C3 | Runtime queue and runner lifecycle | `../canto/runtime/runner.go`, `coordinator.go`, `coordinator_exec.go`, `options.go`, `bootstrap.go`, `hitl.go` | reviewed | Queue wait vs execution context root cause fixed in Canto `595380a`. Runner/coordinator execution split was read; wait timeout and execution timeout are separate, queued wait failures append durable `TurnCompleted`, lease ack/nack uses non-canceling contexts, bootstrap appends durable context outside raw transcript, and HITL input gate is not on Ion's minimal native path. | Keep HITL UX deferred unless Ion activates it in the native loop. |
| C4 | Agent turn loop and streaming commits | `../canto/agent/agent.go`, `loop.go`, `stream.go`, `message.go`, `turnstate.go`, `turnstop.go`, `escalation.go` | patched | Empty assistant and canceled terminal-event fixes exist. Canto `5ce3c1f` makes streaming turns check cancellation before each loop step and makes streaming/non-streaming `StepCompleted` append through `context.WithoutCancel`, so canceled writer-backed sessions retain step terminal state. Imported into Ion and verified. | Continue C6/C7 and return for final reviewed marking after prompt/retry surfaces are checked. |
| C5 | Tool execution lifecycle | `../canto/agent/tools.go`, `../canto/tool/tool.go`, `func.go`, `registry.go`, `metadata.go`, `search.go` | patched | Tool failure durability fixed in Canto `a5878ab`. Canto `5ce3c1f` makes `ToolCompleted` and resulting tool messages append through `context.WithoutCancel` after tool execution begins, preventing canceled tool turns from leaving unresolved assistant tool calls in provider-visible history. Imported into Ion and verified. | Continue C6/C7 and return for final reviewed marking after provider-visible prompt construction is checked. |
| C6 | Prompt and provider-visible history construction | `../canto/prompt/**/*.go`, `../canto/llm/**/*.go` | reviewed | Fedora system-message and display-only history issues have targeted coverage. Core prompt path uses `Instructions -> LazyTools -> History -> Ion request processors -> CacheAligner`; providers prepare cloned requests for capabilities and validate privileged message ordering. `go test ./prompt ./llm ./llm/providers/openai ./llm/providers/anthropic -count=1` is green. | Keep non-core memory/masking prompt processors deferred while `CoreLoopOnly` is on. |
| C7 | Retry, compaction, and budget surfaces that remain active | `../canto/governor/queue.go`, `guard.go`, `recovery.go`, `summarizer.go`, `manual.go`, `offloader.go` | reviewed | Ion has tests around proactive failure settlement. Audit found Ion was using provider-level overflow recovery, which compacted but retried the same already-built oversized request. Ion now uses `runtime.WithOverflowRecovery` so retry re-enters the agent turn and rebuilds provider-visible history from the compacted session; intermediate overflow `TurnCompleted` events are ignored while recovery is active. Canto `governor`/`runtime` gates are green. | Treat compaction UX/control polish as the next reliability-table-stakes layer, not a reason to reopen backend core. |

## Ion Audit Order

| Order | Area | Files | Status | Evidence To Date | Next Check |
| --- | --- | --- | --- | --- | --- |
| I1 | Feature freeze enforcement | `internal/features/features.go`, all `features.CoreLoopOnly` call sites | reviewed | ACP providers, telemetry startup, advanced slash commands, memory/subagent/MCP/reflexion processors, policy config, escalation config, and agent-visible compact tooling are gated. Manual `/compact` plus proactive/overflow compaction remain active as reliability work. `@file` prompt expansion was gated because it silently mutates user prompts during the core-loop audit. | Keep new call sites behind `CoreLoopOnly` unless they are explicitly table-stakes for native loop reliability. |
| I2 | CLI startup, resume, continue, and print lifecycle | `cmd/ion/main.go`, `startup.go`, `selection.go`, `print_mode.go`, `mode.go`, `escalation.go`, `live_smoke_test.go` | reviewed | Startup/resume/print paths have tests for lazy session materialization, bare `--resume`, invalid config resume, print prompts/stdin/JSON/approval/timeout, submit failures, backend session errors, and resumed marker order. External policy config and escalation config are gated during `CoreLoopOnly`. OpenRouter Minimax JSON print smoke passed. | Keep future CLI work scoped to table-stakes automation gaps; do not reopen ACP/subscription CLI behavior before native parity is stable. |
| I3 | Backend contract and Canto event translation | `internal/backend/backend.go`, `unconfigured.go`, `internal/backend/canto/backend.go`, `compaction.go`, `processors.go`, `prompt.go` | patched | Event translation has focused tests for active-turn clearing, canceled terminal suppression, tool IDs/errors/output deltas, provider error recovery, retry status, proactive/overflow compaction, and valid resumed tool history. Direct MCP registration was only UI-gated and is now backend-gated. | Continue reviewing SendStream synchronous-error handling and display-only write boundaries in I4 before marking reviewed. |
| I4 | Storage, lazy session, and replay projection | `internal/storage/canto_store.go`, `lazy_session.go`, `scanner.go`, `storage.go`, `file_store.go`, `internal/session/*.go` | patched | Canto-backed storage rejects model-visible appends, lazy sessions do not materialize before non-noop appends, display-only rows interleave by raw event order, legacy empty assistants are fixture-injected below public write APIs, and routine successful tools compact only as display projection. Store list/input scans now return `rows.Err()` instead of silently dropping errors. | Continue I5 app-loop review, then re-run full storage/app gates before marking reviewed. |
| I5 | App input, commands, and turn lifecycle | `internal/app/model.go`, `events.go`, `broker.go`, `commands.go`, `input.go`, `session_picker.go`, `picker.go`, `presets.go` | patched | Slash commands stay local during active turns, queued follow-ups/cancel/session errors/runtime switches have focused tests, and slash commands before first turn do not materialize lazy sessions. `submitText` persisted routing metadata before `SubmitTurn` succeeded; that now happens only after accepted submission. Direct `/provider <name>` now clears stale progress errors like provider-picker selection does. | Continue I6 rendering/replay audit, then run full app and core-loop smoke tests before marking reviewed. |
| I6 | Transcript rendering and replay formatting | `internal/app/render.go`, `viewport.go`, `markdown.go`, `styles.go`, `history_test.go`, `stabilization_test.go` | reviewed | Startup replay and live transcript share `RenderEntries`/`renderEntry`; resumed marker order and replay blank lines have focused tests. Routine successful tool display is compacted only as a UI transform; tool errors and full verbosity preserve output. `View()` now always separates already-printed transcript/replay rows from the progress/composer shell with one blank row. Manual `--continue` smoke shows expected header/resumed-marker/transcript/progress spacing. | Keep future TUI polish under product-table-stakes work unless it corrupts state or replay. |
| I7 | Core tool implementations and approval boundary | `internal/backend/canto/tools/bash.go`, `file.go`, `search.go`, `approver.go`, `sandbox.go`, `verify.go`, `internal/backend/bash_policy.go`, `policy.go` | reviewed | Tool boundary review patched model-argument validation gaps: `read` accepted negative offsets, `list` ignored malformed JSON, `grep`/`glob` could address paths outside the workspace, and edit tools accepted empty/no-op replacement strings. Bash/verify cancellation now kills the command process group during cancellation, and approval requests are registered before they are published to the UI. Sandbox/approval UX polish remains deferred. | Re-run full CLI/backend/storage/app gates and keep later permission polish out of P1 unless a core turn blocks on it. |
| I8 | Config/provider state that affects core loop startup | `internal/config/config.go`, `internal/providers/catalog.go`, `internal/backend/registry/*.go` | reviewed | Config/state separation has focused tests; invalid provider config opens an unconfigured backend without creating sessions; custom endpoints no longer leak to default providers. `zai` had no default endpoint and is now wired to Z.AI's documented OpenAI-compatible endpoint so provider/model commands do not fail with "no configured endpoint" for that provider. Picker/favorites polish stays deferred. | Re-run provider/registry and full gates; defer remaining picker UX bugs until TUI/product-table-stakes pass. |
| I9 | Non-native/deferred packages | `internal/backend/acp/*.go`, `internal/privacy/*.go`, `internal/subagents/*.go`, `internal/telemetry/*.go`, workspace rollback/checkpoint code | reviewed | ACP backend construction is disabled through provider resolution while `CoreLoopOnly` is enabled. Telemetry startup is skipped. MCP, memory, manual compact, subagent, and rewind commands are hidden/blocked; backend-level MCP registration is also blocked. Privacy redaction remains a display-only transform for approval/tool text, and checkpoints remain active only as write-tool rollback metadata. | Keep these isolated; do not expand ACP/privacy/subagent/checkpoint UX before the native loop gate is closed. |

## Known Slice Evidence

These are useful regressions, not a substitute for the audit above:

- Canto rejects or filters empty assistant payloads and preserves reasoning/tool-call-only assistant payloads.
- Canto persists terminal events for cancellation and tool failures.
- Canto queue wait-timeout no longer cancels active execution context.
- Ion rejects duplicate active `CantoBackend` submits and avoids duplicate Watch streams.
- Ion storage no longer accepts model-visible transcript writes from app display code.
- Ion print mode fails invalid no-prompt invocations before runtime/session creation.
- Ion slash commands stay local during active turns and `/help` before first model turn does not materialize a session.
- Ion replay ordering/spacing has targeted coverage.

## Current Blockers

1. `tk-s6p4` is closed after deterministic, race, manual TUI replay, and Fedora/local-api live-smoke gates.
2. Provider/model picker, help readability, slash autocomplete, permissions/trust polish, ACP, privacy, subagents, skills, and routing remain deferred until reopened by table-stakes parity tasks.
3. Any future Canto or native-loop patch must keep the deterministic gates and Fedora live smoke green.

## Phase 0 Freeze Findings

| Finding | Action |
| --- | --- |
| ACP providers were still reachable from normal provider resolution. | Gate ACP backend creation while `CoreLoopOnly` is enabled. ACP remains T4/secondary bridge work. |
| Proactive and overflow compaction are active in the native backend. | Keep in the reliability audit because context survival matters, but do not expand compaction UX/control work during the core-loop pass. |
| Telemetry setup ran during process startup. | Disable telemetry initialization while `CoreLoopOnly` is enabled so T1 startup cannot fail on observability config/network issues. |
| `@file` prompt expansion was active in the native request processor stack. | Gate it while `CoreLoopOnly` is enabled; the read tool is the explicit core path for file content. |
| External policy and escalation config loaded on startup. | Gate both during `CoreLoopOnly`; default mode policy remains active, but optional config files cannot block the minimal loop. |
| MCP registration could still be invoked directly through the session interface. | Add a backend-level `CoreLoopOnly` guard so deferred tool-surface mutation is blocked below the slash-command layer. |

## Tool Boundary Findings

| Finding | Action |
| --- | --- |
| `read` accepted negative `offset`/`limit`, which could panic on a model-supplied range. | Reject negative range values at the tool boundary. |
| `list` swallowed malformed JSON and silently listed the workspace root. | Return JSON parse errors; only an omitted path defaults to `.`. |
| `grep` accepted caller paths without workspace containment checks. | Resolve search paths under the workspace before invoking `rg`. |
| `glob` ran against `os.DirFS` without rejecting `..` or absolute patterns. | Reject glob patterns that can escape the workspace. |
| `edit`/`multi_edit` accepted empty or no-op replacement strings. | Reject empty `old_string`, identical replacement strings, and empty edit batches before reading or writing files. |
| Bash/verify cancellation relied on process cleanup after `Wait`, which can leave child processes alive while pipes stay open. | Register a context cancellation hook that kills the command process group while the command is still active. |
| Approval events were published before the backend registered the pending approval channel. | Register the approval wait channel first so an immediate UI approval cannot be dropped. |

## Provider Startup Findings

| Finding | Action |
| --- | --- |
| `zai` was configured as a native OpenAI-family provider without any default endpoint. | Set the default endpoint to Z.AI's documented OpenAI-compatible base URL, `https://api.z.ai/api/paas/v4`. |

## TUI Findings

- Manual TUI output on 2026-04-28 showed incorrect vertical spacing in both continued sessions and fresh live turns. The first TUI shell patch now separates printed transcript/replay rows from the live progress/composer shell with one blank row and keeps that behavior under app coverage.
- Manual replay is acceptable for the P1 shell baseline after the spacing patch. Remaining terminal polish should move to product-table-stakes work unless it affects replay correctness.

## Verification Log

- Preferred live smoke target is Fedora local-api with `qwen3.6` when the endpoint is running; it is the free/local validation path.
- OpenRouter fallback models are `deepseek/deepseek-v4-flash` for cheap smoke and `deepseek/deepseek-v4-pro` only when the discounted higher-quality path is worth the cost.
- Live smoke should run after deterministic gates, not as the first diagnostic, so provider/network noise does not mask local loop bugs.
- Latest deterministic gates should be rerun after importing Canto `5576f4d`.
- `go test ./internal/backend/canto/tools -count=1` passed after I7 boundary validation fixes.
- `go test ./internal/backend/canto ./internal/backend/canto/tools ./internal/backend -count=1` passed after the approval registration fix.
- `go test ./internal/providers ./internal/backend/registry -count=1` passed after adding the Z.AI provider endpoint.
- Full deterministic gate passed after I7: `go test ./cmd/ion ./internal/backend/canto ./internal/backend/canto/tools ./internal/storage ./internal/app -count=1 && go test ./... -count=1`.
- Canto `d37beda` was pushed and imported into Ion after full Canto gates passed: `go test ./session -count=1`, `go test ./runtime ./agent ./tool ./prompt ./llm ./governor -count=1`, and `go test ./... -count=1`.
- Ion deterministic gates passed after importing Canto `d37beda`: focused native packages plus `go test ./... -count=1`.
- Fedora live smoke passed against `local-api` / `qwen3.6:27b` at `http://fedora:8080/v1`: bash tool call, approval, persistence, resume, and follow-up turn.
- Race-focused native gate passed: `go test -race ./cmd/ion ./internal/app ./internal/backend/canto ./internal/backend/canto/tools -count=1`.
- Canto C4/C5 cancellation/tool-result patch passed `go test ./agent -count=1`, `go test ./session -count=1`, `go test ./runtime ./agent ./tool ./prompt ./llm ./governor -count=1`, and `go test ./... -count=1`.
- Canto `5ce3c1f` was pushed and imported into Ion.
- Ion deterministic gates passed after importing Canto `5ce3c1f`: focused native packages plus `go test ./... -count=1`.
- Fedora live smoke passed again after importing Canto `5ce3c1f`: `local-api` / `qwen3.6:27b` called bash once, persisted, resumed, and answered the follow-up with `continued`.
- Ion overflow recovery patch passed focused backend coverage, focused native package gates, `go test ./... -count=1`, and Fedora live smoke. The overflow test now asserts the retry request contains the compaction summary and no longer contains pre-compaction history.
- Canto final C6/C7 narrow gates passed: `go test ./prompt ./llm ./llm/providers/openai ./llm/providers/anthropic ./governor ./runtime -count=1`.
- TUI shell spacing patch passed focused app coverage and full Ion tests: `go test ./internal/app -count=1`, `go test ./cmd/ion ./internal/app -count=1`, and `go test ./... -count=1`.
- Stale provider-error UI patch passed focused app coverage and full Ion tests: `go test ./internal/app -run 'TestProviderCommandClearsStaleError|TestProviderPickerSelectingNonListingProviderClearsStaleError|TestRuntimeSwitchClearsQueuedTurns|TestViewSeparatesPrintedTranscriptFromProgress' -count=1` and `go test ./... -count=1`.
- Manual TUI replay smoke passed the P1 presentation baseline: `go run ./... --continue` showed the startup header, resumed marker, replayed transcript, and progress/composer shell in the expected order with readable blank rows.
- Final gate bundle passed before closing `tk-s6p4`: `go test -race ./cmd/ion ./internal/app ./internal/backend/canto ./internal/backend/canto/tools -count=1`, Fedora endpoint discovery for `qwen3.6:27b`, and live `local-api` / `qwen3.6:27b` smoke with bash approval, persistence, resume, and follow-up.
