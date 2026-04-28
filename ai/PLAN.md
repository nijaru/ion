# Ion v0 Core Loop Plan

## Goal

Make Ion reliable enough that normal coding-agent use is uneventful: submit a turn, stream output, run tools, cancel cleanly, handle provider/network failures, persist state, resume later, and keep going without transcript or provider-history corruption.

Pi is the simple core-loop floor. Codex is the richer open-source CLI/session reference. Claude Code is a public behavior reference for command, approval, and workflow expectations. Ion should use those references to clarify behavior, not to chase feature parity before the loop is stable.

## Source Of Truth

- Current phase and next action: [STATUS.md](STATUS.md)
- Detailed reviewed/refactored/pending matrix: [review/core-loop-review-tracker-2026-04-28.md](review/core-loop-review-tracker-2026-04-28.md)
- Target architecture: [design/native-core-loop-architecture.md](design/native-core-loop-architecture.md)
- Task state and issue log: `tk`

Do not duplicate the tracker matrix here. Update this file only when gates, priorities, or ownership change.

## Gates

### Gate 0: Queue And Context Hygiene

Status: active, ongoing

Exit criteria:

- `ai/STATUS.md`, this plan, and `tk ready` agree on the active blocker.
- Every user-reported core-loop bug is logged under `tk-s6p4` or a P1 child task before implementation continues.
- P2/P3 feature docs remain reference material, not active work.

### Gate 1: Session Replay And Model History

Status: complete for the known resume/provider-history failure class

Exit criteria:

- `--continue` and `/resume` restore transcripts with the live renderer.
- Restored messages have readable spacing and routine successful tool output is compact by default.
- Canto omits empty/no-payload assistant messages from effective model history, including legacy rows and snapshots.
- Canto write paths do not create future whitespace-only assistant rows.
- Resumed sessions can accept a new user turn after a tool turn without provider-history errors.

### Gate 2: Native Core Agent Loop

Status: active (`tk-s6p4`)

Exit criteria:

- User, assistant, tool, and terminal events are ordered and durable.
- Tool calls preserve deterministic approval/finalization order.
- Tool failures become durable tool-result/error entries.
- Cancellation, retry-until-cancelled, budget stops, immediate provider errors, and provider-limit errors leave sessions resumable.
- `ion -p` and `--resume <id> -p` are reliable enough for automated smoke tests.
- Deterministic tests cover the contract; a live local-api smoke is run when a live provider is intentionally available.

### Gate 3: TUI Baseline

Status: after Gate 2

Exit criteria:

- Composer remains responsive during streaming, retry, compaction, and queued follow-up states.
- Slash command completion/help is clear.
- Local commands that should work during active turns are allowlisted and never create model sessions.
- Routine tool output stays compact by default with an explicit inspection path.
- Thinking is shown as state, not hidden reasoning text.

### Gate 4: Config, Provider, And Session Hygiene

Status: after Gate 2

Exit criteria:

- `~/.ion/config.toml`, `~/.ion/state.toml`, `~/.ion/trusted_workspaces.json`, and `~/.ion/ion.db` have clear ownership.
- Startup never invents placeholder favorites or persists provider/model choices implicitly.
- Custom OpenAI-compatible endpoints do not contaminate unrelated provider presets.
- Provider errors clear on relevant state changes.

### Gate 5: Safety And Execution Boundary

Status: after the native loop is green

Exit criteria:

- READ, EDIT, and AUTO semantics are clear and enforced.
- Trust controls workspace write posture without duplicating read-only flags.
- Sandbox posture is visible and failure modes are explicit.
- Policy and approval behavior is deterministic before classifier/LLM-as-judge expansion.

## Deferred Until Gate 2 Passes

- ACP bridge polish and headless ACP agent mode
- ChatGPT subscription bridge evaluation
- sandboxing, approvals, and mode polish beyond minimum loop safety
- thinking capability matrix beyond minimal UI state
- privacy expansion beyond concrete leak surfaces
- skills marketplace/self-extension
- cross-host sync and branching
- swarm/alternate-screen orchestration
- model cascades or optimizer-style routing beyond current retry/budget basics

## Immediate Work Order

1. Continue `tk-s6p4` from the core-loop review tracker.
2. Audit startup/resume/continue materialization with real stores.
3. Audit local command and runtime-switch behavior during active turns.
4. Run a provider-history shape pass after compaction and tool turns.
5. Extend deterministic tests for each concrete gap.
6. Re-run full tests, then use a live provider smoke only when a suitable provider is available.
