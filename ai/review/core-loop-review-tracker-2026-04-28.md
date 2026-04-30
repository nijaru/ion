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

## Active Sequence Log

- 2026-04-29 — A1 native event ownership patched in Ion: `AgentMessage` now marks the assistant transcript committed for the active turn, and late assistant/thinking deltas are ignored so streamed deltas cannot recreate pending state or be committed again on `TurnFinished`. Focused app/backend tests, full Ion tests, focused race gate, and OpenRouter Minimax fallback smoke passed. Fedora was unreachable for the local live smoke.
- 2026-04-30 — A2 provider-visible history patched in Canto and imported into Ion. Canto now rejects future role=`tool` messages unless their `tool_id` matches an unresolved assistant tool call, and `EffectiveEntries` omits legacy orphan, duplicate, or late tool rows while preserving matched tool results. Canto `91a3149` is pushed. Ion storage fixtures were corrected to model real assistant-call/tool-result pairs. Canto full tests, Ion full tests, native race gate, and OpenRouter Minimax live tool/resume/follow-up smoke passed; Fedora timed out.
- 2026-04-30 — A3 lazy lifecycle patched in Ion: `LazySession.Append` no longer materializes a durable session for display-only rows before a real backend submit. The CantoBackend native submit path still explicitly materializes through `Ensure`, and focused storage/app/backend tests prove pre-turn status/slash display stays local while real submit creates exactly one session.
- 2026-04-30 — A3 storage surface simplified: removed the unused legacy `fileStore` implementation and unused background `Scanner` helper so Ion has one active storage semantics for the native loop: Canto-backed event storage plus lazy materialization.
- 2026-04-30 — A3 reviewed after cleanup: startup/resume/session-picker/runtime-switch/app command paths hold the lifecycle invariants. Slash/local commands do not materialize sessions, session picker filters non-conversation rows, runtime switches preserve only materialized sessions, app storage writes are UI-local only, and CantoStore rejects model-visible appends. Next subsystem is A4 core tool boundary.
- 2026-04-30 — A4 core tool boundary patched in Ion: file tools now perform workspace file operations through `os.Root`, blocking symlink escapes; grep rejects empty patterns and treats no matches as a normal no-results response; glob output is sorted; bash rejects empty commands and returns non-zero exits as tool errors while preserving output; verify rejects empty commands.
- 2026-04-30 — A4 reviewed: deterministic full/race gates and OpenRouter Minimax live tool/resume/follow-up smoke passed after tool-boundary changes. Fedora timed out. Next subsystem is A5 CLI smoke harness.
- 2026-04-30 — A5 CLI smoke harness reviewed with no code change. Existing coverage handles print flag normalization, stdin prompt composition, text/JSON output, approval rejection/auto-approval, timeout cancellation, submit/session errors, early stream closure, empty assistant completion, bare resume picker rejection in print mode, startup resume/continue selection, and live tool/resume/follow-up smoke. Next subsystem is A6 minimal TUI baseline.
- 2026-04-30 — A6 patched a replay/display ownership defect: `CantoStore.Entries` no longer summarizes routine read/list/glob/grep output before rendering. Storage preserves full Canto-derived tool content, while `RenderEntries` / `renderEntry` decide collapsed/full/hidden display from UI config. Focused app/storage tests and full Ion tests passed; tmux text capture remains before marking A6 reviewed.
- 2026-04-30 — A6 transcript spacing corrected after live TUI feedback: startup leaves one blank row before the progress/composer shell, resumed marker placement is header -> marker -> transcript, and shared `RenderEntries` inserts one blank row between top-level transcript entries. Storage also ignores transient progress labels such as `Running read...` on resume while preserving retry statuses. Focused/full gates, tmux fresh/continue captures, and OpenRouter Minimax live tool/resume/follow-up smoke passed. A6 is reviewed for the current P1 baseline.
- 2026-04-30 — Canto/Ion boundary acceptance coverage added after Canto `83c4d30`: Ion storage tests now prove Canto-derived replay recovers missing provider-visible tool results from durable `ToolCompleted` lifecycle data and drops dangling assistant tool calls before replay/follow-up. Focused storage tests, focused native package tests, and full Ion tests passed.
- 2026-04-30 — Canto runtime coordinator bug fixed and imported as `24f2ed9`: `LocalCoordinator.Await` now removes a queued ticket when its wait context is canceled or deadlined before lease grant. This prevents an abandoned timed-out turn from staying at the front of a session lane and blocking later turns. Canto runtime/core/full tests and Ion full tests passed.

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
| C1 | Session event model and message payload validity | `../canto/session/event.go`, `message.go`, `codec.go`, `history.go`, `projection.go` | reviewed | Write-side empty assistant rejection is pushed in Canto `5576f4d`; projection/effective history sanitizes raw, snapshot, and post-snapshot invalid assistant rows while preserving content, reasoning, thinking-block-only, and tool-call-only assistants. Canto `91a3149` also filters legacy unmatched tool rows from effective history. Projection snapshots build from sanitized effective entries. | Continue A3 in Ion; no extra compatibility/migration path needed. |
| C2 | Session store, subscription, and replay ordering | `../canto/session/session.go`, `sqlite.go`, `jsonl.go`, `rebuilder.go`, `replayer.go`, `subscription.go`, `writethrough.go` | patched | SQLite/JSONL public Save paths reject invalid assistant writes. Replayer intentionally allows legacy rows so effective projection can sanitize old logs. Canto `d37beda` makes raw `LastAssistantMessage` skip legacy invalid assistant rows. Canto `91a3149` makes `Session.Append` reject future unmatched role=`tool` messages and keeps effective replay from exposing orphan/duplicate/late tool rows to providers. Canto session/group/full tests and session race gate are green. | Continue A3 in Ion; revisit write-through drain semantics only if an active runtime path depends on it for P1 durability. |
| C3 | Runtime queue and runner lifecycle | `../canto/runtime/runner.go`, `coordinator.go`, `coordinator_exec.go`, `options.go`, `bootstrap.go`, `hitl.go` | reviewed | Queue wait vs execution context root cause fixed in Canto `595380a`. Runner/coordinator execution split was read; wait timeout and execution timeout are separate, queued wait failures append durable `TurnCompleted`, lease ack/nack uses non-canceling contexts, bootstrap appends durable context outside raw transcript, and HITL input gate is not on Ion's minimal native path. Canto `24f2ed9` fixes local coordinator wait-timeout cleanup so canceled queued tickets do not poison a session lane. | Keep HITL UX deferred unless Ion activates it in the native loop. |
| C4 | Agent turn loop and streaming commits | `../canto/agent/agent.go`, `loop.go`, `stream.go`, `message.go`, `turnstate.go`, `turnstop.go`, `escalation.go` | patched | Empty assistant and canceled terminal-event fixes exist. Canto `5ce3c1f` makes streaming turns check cancellation before each loop step and makes streaming/non-streaming `StepCompleted` append through `context.WithoutCancel`, so canceled writer-backed sessions retain step terminal state. Imported into Ion and verified. | Continue C6/C7 and return for final reviewed marking after prompt/retry surfaces are checked. |
| C5 | Tool execution lifecycle | `../canto/agent/tools.go`, `../canto/tool/tool.go`, `func.go`, `registry.go`, `metadata.go`, `search.go` | patched | Tool failure durability fixed in Canto `a5878ab`. Canto `5ce3c1f` makes `ToolCompleted` and resulting tool messages append through `context.WithoutCancel` after tool execution begins, preventing canceled tool turns from leaving unresolved assistant tool calls in provider-visible history. Imported into Ion and verified. | Continue C6/C7 and return for final reviewed marking after provider-visible prompt construction is checked. |
| C6 | Prompt and provider-visible history construction | `../canto/prompt/**/*.go`, `../canto/llm/**/*.go` | reviewed | Fedora system-message and display-only history issues have targeted coverage. Core prompt path uses `Instructions -> LazyTools -> History -> Ion request processors -> CacheAligner`; providers prepare cloned requests for capabilities and validate privileged message ordering. `go test ./prompt ./llm ./llm/providers/openai ./llm/providers/anthropic -count=1` is green. | Keep non-core memory/masking prompt processors deferred while `CoreLoopOnly` is on. |
| C7 | Retry, compaction, and budget surfaces that remain active | `../canto/governor/queue.go`, `guard.go`, `recovery.go`, `summarizer.go`, `manual.go`, `offloader.go` | reviewed | Ion has tests around proactive failure settlement. Audit found Ion was using provider-level overflow recovery, which compacted but retried the same already-built oversized request. Ion now uses `runtime.WithOverflowRecovery` so retry re-enters the agent turn and rebuilds provider-visible history from the compacted session; intermediate overflow `TurnCompleted` events are ignored while recovery is active. Canto `governor`/`runtime` gates are green. | Treat compaction UX/control polish as the next reliability-table-stakes layer, not a reason to reopen backend core. |

## Ion Audit Order

| Order | Area | Files | Status | Evidence To Date | Next Check |
| --- | --- | --- | --- | --- | --- |
| I1 | Feature freeze enforcement | `internal/features/features.go`, all `features.CoreLoopOnly` call sites | reviewed | ACP providers, telemetry startup, advanced slash commands, memory/subagent/MCP/reflexion processors, policy config, escalation config, and agent-visible compact tooling are gated. Manual `/compact` plus proactive/overflow compaction remain active as reliability work. `@file` prompt expansion was gated because it silently mutates user prompts during the core-loop audit. | Keep new call sites behind `CoreLoopOnly` unless they are explicitly table-stakes for native loop reliability. |
| I2 | CLI startup, resume, continue, and print lifecycle | `cmd/ion/main.go`, `startup.go`, `selection.go`, `print_mode.go`, `mode.go`, `escalation.go`, `live_smoke_test.go` | reviewed | Startup/resume/print paths have tests for lazy session materialization, bare `--resume`, invalid config resume, print prompts/stdin/JSON/approval/timeout, submit failures, backend session errors, early stream close, empty assistant completion, and resumed marker order. External policy config and escalation config are gated during `CoreLoopOnly`. OpenRouter Minimax live tool/resume/follow-up smoke passed after A4. | Keep future CLI work scoped to table-stakes automation gaps; do not reopen ACP/subscription CLI behavior before native parity is stable. |
| I3 | Backend contract and Canto event translation | `internal/backend/backend.go`, `unconfigured.go`, `internal/backend/canto/backend.go`, `compaction.go`, `processors.go`, `prompt.go` | patched | Event translation has focused tests for active-turn clearing, canceled terminal suppression, tool IDs/errors/output deltas, provider error recovery, retry status, proactive/overflow compaction, and valid resumed tool history. Direct MCP registration was only UI-gated and is now backend-gated. | Continue reviewing SendStream synchronous-error handling and display-only write boundaries in I4 before marking reviewed. |
| I4 | Storage, lazy session, and replay projection | `internal/storage/canto_store.go`, `lazy_session.go`, `storage.go`, `internal/session/*.go` | reviewed | Canto-backed storage rejects model-visible appends. Lazy sessions now materialize only through explicit backend `Ensure`, not through UI-local `System`/`Status`/routing appends, so display-only events before the first model turn cannot create conversation rows. The unused legacy `fileStore` was removed to avoid a second transcript writer with different semantics, and the unused background `Scanner` was removed from the core storage package. Display-only rows interleave by raw event order after materialization, legacy empty assistants are fixture-injected below public write APIs, routine successful tool output is preserved for the renderer instead of pre-collapsed in storage, and Ion now has acceptance coverage for Canto-recovered tool results plus dangling tool-call drops. Store list/input scans return `rows.Err()` instead of silently dropping errors. | Keep replay projection unchanged unless A6 exposes another renderer/display ownership issue. |
| I5 | App input, commands, and turn lifecycle | `internal/app/model.go`, `events.go`, `broker.go`, `commands.go`, `input.go`, `session_picker.go`, `picker.go`, `presets.go` | reviewed | Slash commands stay local during active turns, queued follow-ups/cancel/session errors/runtime switches have focused tests, and slash commands before first turn do not materialize lazy sessions. Pre-turn display-only backend events also stay local against lazy storage. Runtime-changing commands require idle state, session picker filters non-conversation rows, current session is preserved only after materialization, and `submitText` persists routing metadata only after accepted submission. Direct `/provider <name>` clears stale progress errors like provider-picker selection does. | Start A4 core tool boundary. |
| I6 | Transcript rendering and replay formatting | `internal/app/render.go`, `viewport.go`, `markdown.go`, `styles.go`, `history_test.go`, `stabilization_test.go` | reviewed | Startup replay and live transcript share `RenderEntries`/`renderEntry`; resumed marker order and replay ordering have focused tests. Routine successful tool display is a renderer-only choice: default rendering compacts routine tool output, `ToolVerbosity=full` expands replayed output, and tool errors preserve output. Startup/replay output now uses one blank row before the shell and one blank row between top-level transcript entries. Tmux fresh/continue captures verify the current layout, and stale transient progress labels are not restored as durable status. | Move remaining tool/thinking controls and steering-vs-queue UX into table-stakes TUI tasks unless another P1 transcript corruption issue appears. |
| I7 | Core tool implementations and approval boundary | `internal/backend/canto/tools/bash.go`, `file.go`, `search.go`, `approver.go`, `sandbox.go`, `verify.go`, `internal/backend/bash_policy.go`, `policy.go` | reviewed | Tool boundary review patched model-argument validation gaps: `read` accepted negative offsets, `list` ignored malformed JSON, `grep`/`glob` could address paths outside the workspace, and edit tools accepted empty/no-op replacement strings. The current A4 pass moved file operations onto `os.Root` so symlinks cannot escape the workspace, made grep no-match a normal result, sorted glob output, and made bash non-zero exits produce Canto tool errors while preserving output. Bash/verify cancellation kills the command process group, and approval requests are registered before they are published to the UI. Sandbox/approval UX polish remains deferred. | Start A5 CLI smoke harness. |
| I8 | Config/provider state that affects core loop startup | `internal/config/config.go`, `internal/providers/catalog.go`, `internal/backend/registry/*.go` | reviewed | Config/state separation has focused tests; invalid provider config opens an unconfigured backend without creating sessions; custom endpoints no longer leak to default providers. `zai` had no default endpoint and is now wired to Z.AI's documented OpenAI-compatible endpoint so provider/model commands do not fail with "no configured endpoint" for that provider. Picker/favorites polish stays deferred. | Re-run provider/registry and full gates; defer remaining picker UX bugs until TUI/product-table-stakes pass. |
| I9 | Non-native/deferred packages | `internal/backend/acp/*.go`, `internal/privacy/*.go`, `internal/subagents/*.go`, `internal/telemetry/*.go`, workspace rollback/checkpoint code | reviewed | ACP backend construction is disabled through provider resolution while `CoreLoopOnly` is enabled. Telemetry startup is skipped. MCP, memory, manual compact, subagent, and rewind commands are hidden/blocked; backend-level MCP registration is also blocked. Privacy redaction remains a display-only transform for approval/tool text, and checkpoints remain active only as write-tool rollback metadata. | Keep these isolated; do not expand ACP/privacy/subagent/checkpoint UX before the native loop gate is closed. |

## Known Slice Evidence

These are useful regressions, not a substitute for the audit above:

- Canto rejects or filters empty assistant payloads and preserves reasoning/tool-call-only assistant payloads.
- Canto rejects unmatched tool-result writes and filters legacy orphan/duplicate/late tool-result rows from effective provider-visible history.
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

- Manual TUI output on 2026-04-28 and 2026-04-30 showed incorrect vertical spacing in both continued sessions and fresh live turns. The current P1 rule is compact but readable: one blank row between top-level transcript entries, one blank row after the startup/replay block before the progress/composer shell, and no per-line indentation for tool output beyond what the renderer intentionally applies.
- A6 found storage was pre-collapsing routine tool replay output before config-aware rendering. That is now fixed so replay can honor `ToolVerbosity=full`; default UI compaction remains in `renderEntry`.
- A6 spacing pass now centralizes blank rows in shared startup/replay rendering. Resumed replay prints with one blank row between top-level transcript entries in tmux capture; stale `Running ...` labels are not replayed as status. Further spacing changes should preserve that rule unless a new live TUI capture proves it wrong.
- `tk-rg23` display-control pass keeps the default transcript quieter: completed thinking/reasoning is hidden by default, in-flight thinking still shows a dim marker, and reasoning-only assistant rows render a safe `Thinking` marker without leaking reasoning. Tool display default is now described as `auto` because routine read/list/search tools compact while non-routine tools can still show detail.
- `tk-zxgq` queue UX first slice keeps the core loop stable: busy-turn text remains next-turn queued, but the TUI now shows queued text above the composer/progress shell and `Ctrl+G` pulls queued turns back into the composer before they are sent. `tk-8jz2` defines the safe steering contract: true active-turn steering can only happen at a Canto-owned provider-call boundary and must downgrade to queueing when that cannot be proven.
- `tk-nwqu` tightened display-only tool presentation: routine tools compact while in-flight as well as after completion, default compact output stays on one line, full output remains available through settings, and replay can use Canto `ToolStarted` args for readable tool titles.
- `tk-dl1v` moved that replay metadata join to the framework boundary: Canto `09140f7` annotates effective tool-result history entries with optional `ToolHistory` derived from durable `ToolStarted`/`ToolCompleted` events. Ion storage now consumes `HistoryEntry.Tool` instead of scanning raw Canto events itself.
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
- Canto A2 provider-history patch passed `go test ./session -count=1`, `go test ./runtime ./agent ./tool ./prompt ./llm ./governor -count=1`, `go test ./... -count=1`, and `go test -race ./session -count=1`; Canto `91a3149` was pushed and imported into Ion.
- Ion passed after importing Canto `91a3149`: `go test ./cmd/ion ./internal/backend/canto ./internal/storage ./internal/app -count=1 -timeout 120s`, `go test ./... -count=1 -timeout 300s`, `go test -race ./cmd/ion ./internal/app ./internal/backend/canto ./internal/storage -count=1 -timeout 300s`, and OpenRouter `minimax/minimax-m2.5:free` live tool/resume/follow-up smoke. Fedora timed out for this live smoke.
- Ion A3 focused lifecycle tests passed after lazy-session materialization tightening: `go test ./internal/app ./internal/storage ./internal/backend/canto -count=1`.
- Ion A3 storage-surface cleanup passed focused native package tests, full Ion suite, and native race gate after removing legacy `fileStore` and unused `Scanner`.
- Ion A4 tool-boundary patch passed `go test ./internal/backend/canto ./internal/backend/canto/tools ./internal/backend -count=1`, `go test ./... -count=1 -timeout 300s`, and `go test -race ./cmd/ion ./internal/app ./internal/backend/canto ./internal/backend/canto/tools ./internal/storage -count=1 -timeout 300s`.
- A4 OpenRouter fallback live smoke passed after tool-boundary changes: `ION_LIVE_SMOKE=1 ION_SMOKE_PROVIDER=openrouter ION_SMOKE_MODEL=minimax/minimax-m2.5:free go test ./cmd/ion -run TestLiveSmokeTurnAndToolCall -count=1 -v -timeout 180s`. Fedora timed out.
- A6 replay/display patch passed focused tests and full Ion tests: `go test ./internal/storage ./internal/app -run 'TestCantoStoreEntriesPreserveRoutineToolOutput|TestRenderRoutineToolEntry|TestRenderEntriesCanExpandReplayedRoutineToolOutput|TestStartupPrintLinesIncludesReplayHistory' -count=1`, `go test ./internal/app ./internal/storage -count=1`, `go test ./cmd/ion ./internal/app ./internal/storage ./internal/backend/canto ./internal/backend/canto/tools -count=1 -timeout 180s`, and `go test ./... -count=1 -timeout 300s`.
- A6 spacing patch passed focused app tests, focused native tests, full Ion tests, native race gate, tmux text capture for live and resumed TUI output, and OpenRouter live smoke: `go test ./internal/app -run 'TestRenderEntriesCanExpandReplayedRoutineToolOutput|TestViewDoesNotInsertBlankLineBeforeProgress|TestStartupPrintLinesIncludesReplayHistory|TestRunningProgressLinePutsElapsedAfterTokenCounters|TestQueuedFollowUpSubmitsAfterTurnFinished' -count=1`, `go test ./cmd/ion ./internal/app ./internal/storage ./internal/backend/canto ./internal/backend/canto/tools -count=1 -timeout 180s`, `go test ./... -count=1 -timeout 300s`, `go test -race ./cmd/ion ./internal/app ./internal/backend/canto ./internal/backend/canto/tools ./internal/storage -count=1 -timeout 300s`, and `ION_LIVE_SMOKE=1 ION_SMOKE_PROVIDER=openrouter ION_SMOKE_MODEL=minimax/minimax-m2.5:free go test ./cmd/ion -run TestLiveSmokeTurnAndToolCall -count=1 -v -timeout 180s`.
- `tk-rg23` display-control patch passed focused app/config and native tests: `go test ./internal/app -run 'TestRenderThinking|TestRenderReasoningOnly|TestRenderPlaneBThinking|TestSettingsCommandShowsDisplayDefaults|TestSettingsCommandShowsCommonSettings|TestSettingsToolAutoClearsStableOverride|TestRenderRoutineToolEntry' -count=1`, `go test ./internal/app ./internal/config -count=1`, and `go test ./cmd/ion ./internal/app ./internal/storage ./internal/backend/canto ./internal/backend/canto/tools -count=1 -timeout 180s`.
- `tk-zxgq` queue UX first slice passed focused app/native tests: `go test ./internal/app -run 'TestQueuedFollowUp|TestCtrlGRecallsQueuedTurns|TestComposerQueuesWhileCompacting|TestEscapeCancelClearsQueuedFollowUps' -count=1`, `go test ./internal/app ./internal/config -count=1`, and `go test ./cmd/ion ./internal/app ./internal/storage ./internal/backend/canto ./internal/backend/canto/tools -count=1 -timeout 180s`.
- `tk-nwqu` tool-formatting slice passed focused app/storage tests, focused native package tests, full Ion tests, and tmux replay capture: `go test ./internal/app ./internal/storage -run 'TestRenderRoutineToolEntry|TestRenderPendingRoutineToolEntry|TestRenderEntriesCanExpandReplayedRoutineToolOutput|TestCantoStoreEntriesPreserveRoutineToolOutput|TestCantoStoreEntriesPreserveFullAgentContent' -count=1`, `go test ./internal/app ./internal/storage ./cmd/ion ./internal/backend/canto ./internal/backend/canto/tools -count=1 -timeout 180s`, and `go test ./... -count=1 -timeout 300s`.
- Post-`tk-nwqu` native race gate passed: `go test -race ./cmd/ion ./internal/app ./internal/backend/canto ./internal/backend/canto/tools ./internal/storage -count=1 -timeout 300s`. Live provider smoke was unavailable: Fedora timed out, and OpenRouter Minimax stayed in retry status until the smoke test timed out.
- `tk-dl1v` Canto projection metadata patch passed Canto `go test ./session -count=1`, Canto `go test ./runtime ./agent ./tool ./prompt ./llm ./governor -count=1`, Canto `go test ./... -count=1`, Ion focused native packages, Ion `go test ./... -count=1 -timeout 300s`, and Ion native race gate.
- TUI shell spacing patch passed focused app coverage and full Ion tests: `go test ./internal/app -count=1`, `go test ./cmd/ion ./internal/app -count=1`, and `go test ./... -count=1`.
- Stale provider-error UI patch passed focused app coverage and full Ion tests: `go test ./internal/app -run 'TestProviderCommandClearsStaleError|TestProviderPickerSelectingNonListingProviderClearsStaleError|TestRuntimeSwitchClearsQueuedTurns|TestViewSeparatesPrintedTranscriptFromProgress' -count=1` and `go test ./... -count=1`.
- Manual TUI replay smoke passed the P1 presentation baseline: `go run ./... --continue` showed the startup header, resumed marker, replayed transcript, and progress/composer shell in the expected order with readable blank rows.
- Final gate bundle passed before closing `tk-s6p4`: `go test -race ./cmd/ion ./internal/app ./internal/backend/canto ./internal/backend/canto/tools -count=1`, Fedora endpoint discovery for `qwen3.6:27b`, and live `local-api` / `qwen3.6:27b` smoke with bash approval, persistence, resume, and follow-up.
