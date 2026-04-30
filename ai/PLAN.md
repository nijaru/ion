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

## Operating Rule

Do not treat core-loop regressions as independent bug slices. The `tk-s6p4` comprehensive audit produced useful evidence, but the P1 gate is reopened under `tk-mmcs`. Every native-loop change must be tied to the active subsystem sequence and invariant in the tracker before implementation.

Do not collapse Canto into Ion as a development shortcut. Keep the repo split, but treat Ion as Canto's acceptance test until the minimal native loop is stable. Canto public-framework expansion is deferred; Canto changes during this pass should come from concrete Ion-proven framework needs.

## Priority Bands

These are rough sequencing guidelines, not exact gates. Move work earlier only when it protects the core loop from corruption, wedging, or unusable context growth.

| Band | Meaning | Examples | Status |
| --- | --- | --- | --- |
| Core | Minimal loop plus stable shell. | submit, stream, core tools, cancel, provider error, retry status, persistence correctness, `-p`, basic TUI turn display. | Active: validated slices exist, but live TUI defects reopened the gate. |
| Reliability table stakes | Features that keep the loop usable in real sessions. | minimal continue/resume correctness, compaction/overflow recovery, durable replay after long sessions. | Include when they protect core reliability. |
| Product table stakes | Common agent UX after the loop is sane. | robust resume UX, slash autocomplete, basic permission UX, transcript inspection. | Active next under `tk-mmcs`. |
| Polish | Workflow and presentation improvements. | provider/model picker polish, launch header, thinking picker, tree/branching, external editor, richer status/help. | Deferred. |
| Experimental/SOTA | Advanced or secondary architecture. | subagents, skills, ACP/subscription bridges, routing/cascades, privacy pipeline, optimizer loops, swarm mode. | Isolated from native P1 unless directly blocking. |

`continue`, `resume`, and compaction straddle bands: minimal correctness is part of reliability; polished UX and controls wait.

## Gates

### Gate 0: Queue And Context Hygiene

Status: complete for the P1 audit

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

### Gate 1.5: Nonessential Path Freeze

Status: complete

Exit criteria:

- `features.CoreLoopOnly` is verified against actual call sites, not assumed.
- ACP, MCP, memory/workflows, subagents, reflexion, model-routing experiments, privacy expansion, skills, branching, and advanced thinking expansion are disabled, hidden, or bypassed unless needed for the basic native loop. Compaction stays in the reliability review when it protects context survival.
- Remaining active paths are named explicitly: CLI/TUI startup, print mode, CantoBackend, Canto session/history/projection, Canto agent/tool loop, core tools, display replay, cancel/error/retry terminal states, and durable session lifecycle.
- Any active nonessential path is either removed from the core path or documented as a blocker in `tk-s6p4`.

### Gate 2: Native Core Agent Loop File Audit

Status: reopened under `tk-mmcs` for current live TUI/core defects

Exit criteria:

- Canto session, runtime, agent, prompt, provider, tool, and minimal governor files in the tracker have been reviewed file by file.
- Ion command/startup, print, CantoBackend, storage, app loop, renderer, core tools, config boundary, and smoke harness files in the tracker have been reviewed file by file.
- User, assistant, tool, and terminal events are ordered and durable.
- Tool calls preserve deterministic approval/finalization order.
- Tool failures become durable tool-result/error entries.
- Cancellation, retry-until-cancelled, immediate provider errors, and provider-limit errors leave sessions resumable.
- `ion -p` and `--resume <id> -p` are reliable enough for automated smoke tests.
- Deterministic tests cover the contract; a live local-api or funded-provider smoke is run when a live provider is intentionally available.

### Gate 3: TUI Baseline

Status: active after core correctness fixes; UI should be minimal like Claude Code/Droid while the loop remains Pi-simple

Exit criteria:

- Composer remains responsive during streaming, retry, compaction, and queued follow-up states.
- Slash command completion/help is clear.
- Local commands that should work during active turns are allowlisted and never create model sessions.
- Routine tool output stays compact by default with an explicit inspection path.
- Thinking is shown as state, not hidden reasoning text.

### Gate 4: Config, Provider, And Session Hygiene

Status: active next under `tk-mmcs`

Exit criteria:

- `~/.ion/config.toml`, `~/.ion/state.toml`, `~/.ion/trusted_workspaces.json`, and `~/.ion/ion.db` have clear ownership.
- Startup never invents placeholder favorites or persists provider/model choices implicitly.
- Custom OpenAI-compatible endpoints do not contaminate unrelated provider presets.
- Provider errors clear on relevant state changes.

### Gate 5: Safety And Execution Boundary

Status: deferred behind P1. While `CoreLoopOnly` is active, trust downgrade and policy approval hooks must not affect the native loop.

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

1. Follow the active sequence in [review/core-loop-review-tracker-2026-04-28.md](review/core-loop-review-tracker-2026-04-28.md): A1 native event ownership, A2 provider-visible history, A3 storage/lazy lifecycle, A4 core tools, A5 CLI harness, A6 minimal TUI baseline.
2. Keep `CoreLoopOnly` on as the default freeze, but make it a true minimal-agent path: no trust downgrade, no approval/policy hook, no ACP/subagent/privacy/routing detours. `/compact` stays open because context survival is reliability work.
3. Fix P1 correctness before UI polish: event ownership, tool execution/result durability, resume/follow-up, provider-history validity, and automation coverage.
4. Then tighten the TUI shell to a minimal Claude Code/Droid-like presentation: one separator between committed entries, compact routine tools by default, readable markdown, and tmux text capture as the primary visual check.
5. Promote table-stakes items into focused tasks before reopening deferred P2/P3 codepaths.
6. Re-run full/race tests and Fedora/local-api live smoke after any native-loop or CLI/session behavior change. Prefer Fedora `local-api` / `qwen3.6:27b` when reachable. When local testing is unavailable, use current free OpenRouter targets first (`openrouter/owl-alpha`, `tencent/hy3-preview:free`), then `minimax/minimax-m2.5:free`; use cheap paid models such as `deepseek/deepseek-v4-flash` only when a separate-provider check is useful.

No more broad `ai/` corpus passes by default. Use the existing context docs as an index, then read source. Reopen `ai/` only for a specific subsystem decision or when docs conflict with code.
