---
date: 2026-04-28
summary: Reset plan for reaching a minimal reliable Pi-class Ion agent before rebuilding TUI polish and advanced features.
status: active
---

# Core Minimal Agent Reset

## Verdict

The Canto/Ion split is still the right product architecture, but the implementation should be treated as unproven until reviewed subsystem by subsystem. The recent work fixed real bugs, but it was still slice-driven. The next pass is a top-down audit and targeted rewrite/refactor toward a minimal Pi-class agent.

Do not rewrite both repos wholesale. Do rewrite any module that is acting as a second agent loop, second transcript writer, hidden session materializer, or unbounded feature host inside the core path.

Do not merge Canto into Ion for speed. The split is useful, but Canto should behave like Ion-validated internal infrastructure until Ion's minimal native loop is stable. Public framework growth waits; framework-owned bugs still get fixed upstream in Canto and then imported into Ion.

## Context Reviewed

Ion:

- `ai/STATUS.md`, `ai/PLAN.md`, `ai/DESIGN.md`, `ai/DECISIONS.md`
- `ai/design/native-core-loop-architecture.md`
- `ai/review/core-loop-review-tracker-2026-04-28.md`
- `ai/review/core-loop-ai-corpus-synthesis-2026-04-27.md`
- `ai/research/pi-current-core-loop-review-2026-04.md`
- `ai/research/core-agent-reference-delta-2026-04-27.md`

Canto:

- `../canto/ai/README.md`, `STATUS.md`, `PLAN.md`, `DESIGN.md`, `DECISIONS.md`
- `../canto/ai/review/ion-feedback-tracker-2026-04-28.md`
- `../canto/ai/review/canto-ion-roadmap-2026-04.md`
- `../canto/ai/review/test-quality-and-rewrite-gap-2026-04.md`

Reference baseline:

- Pi core loop and session model from `/Users/nick/github/badlogic/pi-mono`
- Codex CLI/exec behavior from prior local reference notes

## If We Rewrote From Scratch Today

The minimal design would be:

```text
Ion input -> local command classifier -> model turn request
          -> Canto Runner -> Canto AgentLoop -> Provider
          -> Canto ToolExecutor -> Canto SessionLog
          -> Ion DisplayProjection -> TUI/CLI output
```

### Canto Kernel

The minimum Canto runtime for Ion needs six boring pieces:

| Piece | Responsibility | Must Not Do |
| --- | --- | --- |
| `SessionLog` | Append transcript, context, hidden lifecycle, tool, and terminal events. | Know Ion UI concepts. |
| `Projector` | Build provider-valid effective history and resume projections. | Mutate old events or leak hidden/display events. |
| `Runner` | Own one active turn per session, cancellation, queueing, and terminal settlement. | Let hosts duplicate coordination. |
| `AgentLoop` | Stream/generate assistant messages, execute tool cycles, and stop for explicit reasons. | Emit blank assistant payloads or rely on host-side terminal guessing. |
| `ToolExecutor` | Validate/preflight, execute, and append ordered tool results. | Let tool failures bypass durable observations. |
| `ProviderBridge` | Convert neutral requests into provider wire shape. | Contaminate durable history with provider-specific rewrites. |

### Ion Product Shell

The minimum Ion product layer needs five pieces:

| Piece | Responsibility | Must Not Do |
| --- | --- | --- |
| Input classifier | Separate slash/local commands from model turns. | Materialize sessions for local commands. |
| Runtime adapter | Open/configure Canto, submit turns, translate Canto events. | Persist user/assistant/tool transcript rows. |
| Display projection | Merge Canto effective entries with Ion display-only entries. | Change provider-visible history. |
| TUI/print surfaces | Render live/replay output, status, errors, and prompt CLI result. | Fork loop semantics between TUI and CLI. |
| Core tools | Provide rooted file/shell/edit/search behavior and approval hooks. | Pull sandbox/memory/subagent/routing complexity into P1. |

## Priority Bands

Use these as rough sequencing guidelines, not exact gates. A feature can move earlier when it directly affects core reliability, and it can move later when it adds complexity without proving the loop.

| Band | Meaning | Examples | Current Treatment |
| --- | --- | --- | --- |
| Core | Minimal loop and stable shell. | submit, stream, core tool call/result, cancel, provider error, retry status, no transcript corruption, scriptable `-p`, basic TUI turn display. | Active now. Review/refactor first. |
| Reliability table stakes | Agent features that make the loop survive real use. | minimal continue/resume correctness, compaction/overflow recovery, durable replay after long sessions. | Include when they affect loop correctness; defer UX polish. |
| Product table stakes | Common agent UX expected after the loop is sane. | robust `/resume`, slash autocomplete, basic permission UX, transcript inspection, provider/model state clarity. | Plan next; do not let it obscure loop bugs. |
| Polish | Useful workflow and presentation improvements. | launch header polish, thinking picker UX, tree/branching, external editor, token/status color, richer help. | Defer until core and reliability paths are stable. |
| Experimental/SOTA | Advanced or secondary architecture. | subagents, skills marketplace, model cascades/routing, privacy redaction pipeline, optimizer/GEPA/DSPy loops, swarm mode, ACP/subscription bridges. | Isolate from the native core path unless a concrete core blocker appears. |

Important nuance: `continue`, `resume`, and compaction are not all-or-nothing. Minimal correctness for resume and compaction belongs in the reliability review because it protects the core loop from corruption or context failure. Polished selectors, branch UX, summary controls, and compaction presentation wait.

## Minimal Pi-Class Feature Floor

Until this floor is green, all other feature work is deferred.

Required:

- `ion -p "prompt"` and piped stdin.
- minimal `--continue -p` and `--resume <id> -p` only as durability probes.
- One active model turn at a time with deterministic queued follow-up behavior.
- Streaming assistant text with a clear terminal event.
- Core tools: shell, read, write/edit, list, search. Tool output may be compact in UI only.
- Tool call/result ordering that survives persistence and resume.
- Cancellation that persists a resumable terminal state.
- Provider error, retry status, provider-limit, and tool-error states that do not wedge the next turn.
- Append-only sessions and replay matching live display.
- No placeholder model/provider/favorite state created by startup.

Not required yet:

- polished continue/resume picker UX, compaction UX polish, ACP bridge polish, subscription bridges, MCP, memory/workflows, subagents, skills, routing/cascades, branching/tree UI, privacy redaction expansion, advanced thinking budget UX, sandbox polish, or rich approval modes.

## Phases

### Phase 0: Reset And Freeze

Goal: stop nonessential code from interfering with the audit.

Work:

- Keep `features.CoreLoopOnly` enabled.
- Verify every `CoreLoopOnly` call site and add gates where P2/P3 code still mutates prompt/session/runtime/display state.
- Gate ACP providers and telemetry startup out of the T1 path while the audit is active. Keep compaction as T1.5 and audit it with the core loop because overflow recovery affects agent reliability.
- Update `ai/STATUS.md`, `ai/PLAN.md`, and `tk-s6p4` to state that prior fixes are evidence, not completion.
- Do not claim Canto has no Ion issues until the audit is complete; `../canto/ai/review/ion-feedback-tracker-2026-04-28.md` stays the place for confirmed framework findings only.

Exit:

- Nonessential paths are disabled, hidden, or isolated from the native loop.
- The audit tracker lists every core file group as pending/in-review/reviewed with no broad claims.

### Phase 1: Canto Minimal Runtime Audit

Goal: prove or rewrite the framework core that Ion depends on.

Review in order:

1. `session`: event model, stores, subscriptions, replay, projections, snapshots.
2. `runtime`: runner, queueing, cancellation, watch/close, resume coordination.
3. `agent`: streaming/non-streaming message commit, terminal states, tool cycles.
4. `tool`: validation, approval/preflight, ordered execution, error result shape.
5. `prompt`/`llm`: effective history, context lanes, provider request transformation.
6. minimal `governor`/retry surfaces that stay active in P1.

Rewrite/refactor trigger:

- More than one writer for provider-visible transcript.
- Terminal state can be returned to host before it is durable.
- Queue/cancel logic requires host compensation.
- Provider-ready history can become invalid from valid durable events.

Exit:

- Canto deterministic tests cover every required terminal state and provider-history invariant.
- Any framework-owned defect is fixed in Canto first, committed/pushed there, then imported into Ion.

### Phase 2: Ion Native Adapter Rewrite/Refactor

Goal: make `internal/backend/canto` a thin adapter, not another loop.

Work:

- Rewrite `SubmitTurn` around explicit phases: materialize, subscribe, send, translate, settle.
- Move translation into small pure functions.
- Make goroutine, cancel, close, and active-turn ownership obvious.
- Remove or block any provider-visible persistence outside Canto.

Exit:

- Backend tests prove event order, duplicate-watch prevention, cancellation, synchronous config failures, retry status, provider errors, and follow-up turn readiness.

### Phase 3: Ion Storage And Replay Rewrite/Refactor

Goal: make replay a projection, not recovery logic.

Work:

- Read Canto effective entries as the source of provider-visible transcript.
- Merge Ion display-only events by event order.
- Compact routine successful tool display only at render/projection time.
- Reject model-visible appends through Ion storage.

Exit:

- Live and resumed transcripts share one renderer and spacing behavior.
- Legacy bad rows cannot corrupt provider history or duplicate display entries.
- Slash commands and picker changes do not create recent sessions.

### Phase 4: CLI Core Harness

Goal: make the minimal loop testable without manual TUI use.

Work:

- Harden `ion -p`, stdin, `--continue -p`, and `--resume <id> -p`.
- Add deterministic fake-provider tests for: text, tool, tool error, provider error, retry status, cancel, resume follow-up.
- Fix live-smoke timeout/retry harness so provider stalls produce a clean diagnostic.

Exit:

- The scriptable CLI proves the core loop without requiring manual terminal inspection.

### Phase 5: TUI Stable Shell

Goal: put a reliable TUI on top of the minimal loop.

Work:

- Review `internal/app` file by file: input, commands, broker, events, model, renderer, viewport, picker/session picker.
- Keep local commands available during turns without touching model transcript.
- Fix stale status/error clearing.
- Fix replay/live transcript spacing after the core loop is stable:
  - `--continue` currently shows awkward spacing around the resumed marker and restored entries.
  - fresh live TUI turns can show too much vertical space between the user prompt and assistant response.
- Improve launch header/help/autocomplete only after the loop state machine is stable.

Exit:

- Manual/tmux TUI smoke can submit, stream, run tools, cancel, resume, and continue without transcript or progress corruption.

### Phase 6: Reintroduce Deferred Features One At A Time

Only after Phases 1-5 pass:

1. Safety/trust/modes/sandbox polish.
2. Thinking capability picker and model/provider UX.
3. Compaction/tree/branching.
4. ACP/subscription bridges.
5. Memory, skills, subagents, routing, privacy expansion, and SOTA optimizer ideas.

Each feature needs an explicit seam and regression gate before it is enabled by default.

## Design Rules For The Refactor

- One model-visible transcript writer: Canto.
- One normal turn path: Ion uses Canto Runner for normal model turns.
- One live/replay display projection: Ion renderer sees the same entry shape for both.
- Terminal states are data, not inferred UI mood.
- Feature gates are temporary audit controls, not permanent architecture.
- If a module is easier to reason about from scratch than through incremental edits, rewrite that module inside the phase.
- If a feature is not part of the Pi-class floor, it must not mutate core prompt/session/runtime state during P1.

## Immediate Next Work

1. Finish Phase 0 by verifying `CoreLoopOnly` call sites and patching missing gates.
2. Start Phase 1 with Canto `session` files, recording findings in the audit tracker before edits.
3. Continue phase-by-phase. Do not switch to picker, ACP, privacy, thinking expansion, or TUI polish until the earlier phase exits.

Do not perform another broad pass over `./ai` or `../canto/ai` unless a concrete subsystem needs it. The planning corpus has been read enough for the reset; the next source of truth is code plus focused tests.
