# Ion v0 Core Parity Plan

## Goal

Make Ion reliable enough that normal coding-agent use is boring: submit a turn, stream output, run tools, cancel cleanly, survive provider/network failures, persist state, resume later, and keep going without transcript or provider-history corruption.

Pi is the simple core-loop floor. Codex is the richer open-source CLI/TUI reference. Claude Code is a public behavior reference for command, approval, and workflow expectations. Ion should not clone any of them; it should reach their core reliability bar while keeping the Go/Bubble Tea + Canto split clean.

## Current Truth

Ion Gate 1 is green for the resume/model-history failure that was blocking normal testing:

- Canto commit `927e482` filters empty/no-payload assistant rows from effective model history, including legacy rows and snapshots.
- Canto commit `52206f2` also fixes the write-side assistant payload predicate so future whitespace-only assistant rows are not durably appended.
- Canto commit `c22da5e` makes canceled streaming and non-streaming turns persist terminal `TurnCompleted` events even after the caller context is canceled.
- Canto commit `a5878ab` adds structured error text to failed tool completion events, so Ion can render and replay failed tool results without text heuristics.
- Ion imports that Canto pseudo-version through normal `go.mod` resolution.
- Ion's native adapter now treats Canto `MessageAdded` assistant rows as the only assistant commit signal, preserves tool output IDs, binds turn execution to the caller context, keeps the watch alive until terminal events arrive, and closes one-shot print handles explicitly.
- Ion app lifecycle now closes newly opened runtime handles on switch/resume post-open failure and keeps the old runtime open until replay/state validation succeeds.
- Ion replay compacts routine successful tool output by default but preserves full errored routine output for debugging after resume.
- Ion's Canto-backed storage compatibility API now skips empty assistant appends before lazy session materialization, while preserving reasoning-only assistant rows.
- Ion TUI `/resume` runtime switches now place `--- resumed ---` after the launch header and before restored transcript rows.
- Ion print CLI validates print arguments and prompt/stdin presence before opening storage/runtime.
- Ion Canto backend submit metadata preserves provider-qualified model names during recency updates.
- Ion live transcript rendering preserves committed assistant messages even without prior streaming deltas or after tool-result pending rows are cleared.
- Ion print CLI fails closed if the event stream closes before `TurnFinished`.
- Ion app distinguishes immediate submit failures from backend terminal errors, avoiding a wait for terminal events that cannot arrive.
- Ion print CLI rejects bare `--resume` without a session ID because print mode cannot open the resume picker.
- Live Fedora/local-api smoke is currently deferred because Fedora is off; deterministic review remains the active proof path.

Ion is still not broadly core-stable. The next gate is a top-down design/refactor against `ai/design/native-core-loop-architecture.md`, then deterministic coverage for cancellation, retries, provider-limit failures, session lifecycle, and TUI command/display polish.

While Gate 2 is active, Ion runs with a temporary CoreLoopOnly gate. Advanced surfaces are disabled in startup registration and slash command UX so the P1 loop can be debugged without memory prompting, subagents, MCP registration, manual compaction, rewind, or reflexion prompt mutation changing the shape of a turn.

## Priority Gates

### Gate 0: Planning And Queue Hygiene

Status: active, ongoing

Exit criteria:

- `ai/STATUS.md`, `ai/ROADMAP.md`, and `tk ready` agree on the active blocker.
- Provider/model picker, ACP, thinking, privacy, skills, approvals, sandboxing, modes, and routing work are blocked or deferred until the core loop passes the gates below.
- Any user-reported bug gets a `tk` task or a log entry before implementation continues.

### Gate 1: Session Replay And Model History

Status: complete (`tk-izo7`)

Progress:

- Canto effective-history filtering is committed and pushed as `927e482`, then imported by Ion.
- Ion replay spacing/resumed marker placement is implemented and covered by Ion tests.
- Ion backend close now cancels/waits for turn goroutines before closing the public event stream.
- Live Fedora/local-api resume/new-turn smoke passed through Ion's normal dependency path.

Exit criteria:

- `--continue` and `/resume` restore the selected transcript with the same renderer used for live messages.
- Restored messages have readable spacing and no raw full routine `list`/`read` dumps by default.
- Empty/no-payload assistant messages are omitted from Canto effective model history, including legacy rows and projection snapshots.
- A resumed session can accept a new user message after a tool turn without provider-history errors.
- Tests cover both future clean sessions and legacy corrupted sessions.

Ownership:

- Canto owns durable event projection and model-visible history.
- Ion owns display compaction, transcript spacing, and startup/resume placement.

### Gate 2: Core Agent Loop Contract

Status: active (`tk-s6p4`)

Current scope freeze:

- Default native tool registration is narrowed to bash, read/write/edit/multiedit, list, grep, glob, and verify.
- Advanced commands are blocked and hidden from help/completion: `/mcp`, `/compact`, `/memory`, and `/rewind`.
- Memory recall/remember tools, the memory prompt processor, subagent tool, and reflexion processor are disabled by `features.CoreLoopOnly`.
- Automatic context overflow recovery and proactive compaction stay enabled because resumable long-running sessions are part of the core loop contract.

Exit criteria:

- Ordered lifecycle is tested: user commit, assistant/tool events, terminal turn state.
- Tool calls preserve deterministic approval/finalization order while allowing safe internal concurrency.
- Tool failures become durable tool-result/error entries, not ambiguous loop panics.
- Cancellation, retry-until-cancelled, budget stops, immediate provider errors, and provider-limit errors all leave the session resumable.
- `go test ./...` covers these cases with deterministic fake providers/tools and at least one local-api smoke path when available.
- Noninteractive prompt mode is reliable enough for automated local-api/Fedora smoke tests, so core loop regressions do not depend on manual TUI testing.

Current design target:

- Canto owns append-only model-visible transcript, effective provider history, agent/tool execution, and terminal turn events.
- Ion owns input classification, command UX, product policy, display projection, and CLI/TUI rendering.
- The native backend adapter should be a small spine: ensure/select session, subscribe, submit via `Runner.SendStream`, translate events, and stop cleanly.
- Storage/replay should consume Canto `EffectiveEntries`, merge only Ion display events, and render replay with the live renderer.
- Pre-implementation gates are now tracked in `ai/review/core-loop-ai-corpus-synthesis-2026-04-27.md`; do not start more code changes until Canto contract audit, Ion adapter design, storage/replay design, and app/CLI lifecycle design are written.

References:

- Pi: lifecycle events, explicit streaming state, ordered tool finalization.
- Codex: robust interrupt/exit behavior, transcript/composer state separation, sandbox/approval clarity.
- Claude Code: immediate local commands during active work and visible command/control surfaces.

### Gate 3: TUI Baseline

Status: next after Gate 1/2

Exit criteria:

- Composer remains responsive during streaming, retry, compaction, and queued follow-up states.
- Slash commands have basic autocomplete and clear help text.
- Local commands that should work during an active turn are allowlisted and do not create model sessions.
- Routine tool output is compact by default, with an explicit path to inspect details.
- Thinking is shown as a small state such as `Thinking...` when available; hidden reasoning is not dumped.

### Gate 4: Config, Provider, And Session Hygiene

Status: after Gate 1/2, before broader provider features

Exit criteria:

- `~/.ion/config.toml`, `~/.ion/state.toml`, `~/.ion/trusted_workspaces.json`, and `~/.ion/ion.db` have clear ownership.
- Startup never invents placeholder favorites or persists provider/model choices implicitly.
- Custom OpenAI-compatible endpoints can be selected without contaminating unrelated provider presets.
- `--continue`, `--resume <id>`, and bare `--resume` behave predictably.
- Provider errors clear when the state changes and do not stick in the progress surface.

### Gate 5: Safety And Execution Boundary

Status: after core loop is green

Exit criteria:

- READ, EDIT, and AUTO semantics are documented in the UI and enforced in Canto/Ion boundaries.
- Trust controls workspace write posture without duplicating read-only flags.
- Sandbox remains opt-in/visible until platform support is reliable; failure modes are explicit.
- Policy and approval behavior is deterministic before classifier/LLM-as-judge ideas are expanded.

## Deferred Until Gates Pass

- approvals, sandboxing, and permission-mode polish beyond the minimum needed not to break the core loop
- ACP bridge polish and headless ACP agent mode
- ChatGPT subscription bridge evaluation
- typed thinking capability matrix beyond the minimal UI
- privacy redaction expansion beyond concrete leak surfaces
- skills marketplace/self-extension
- cross-host sync and branching
- swarm/alternate-screen orchestration
- model cascades or optimizer-style routing beyond current retry/budget basics

## Immediate Work Order

1. Treat `tk-s6p4` as the active blocker and keep `tk-mmcs` synchronized.
2. Keep the CoreLoopOnly gate in place until Gate 2 is proven by deterministic tests plus live local-api smoke.
3. Implement the Ion refactor in order: backend spine, storage/replay projection, app/CLI lifecycle.
4. Extend deterministic and print CLI smoke coverage around cancellation/error persistence, retry status, provider-limit recovery, tool errors, runtime switch cleanup, and resumed follow-up turns.
5. Only then resume TUI polish such as startup header readability, slash autocomplete, thinking display, and routine tool output.

Documentation hygiene follow-up:

- `tk-xrgc` tracks deduplicating and reorganizing `./ai` and `../canto/ai`. This is needed for future agent efficiency, but it stays behind the core-loop design/refactor unless stale docs actively block implementation.
