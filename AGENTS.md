# ion Agent Instructions

Ion is a terminal coding agent built on the Canto framework. Keep this file
durable: it should explain how agents work in this repo, not what happened in
the latest session.

## Source Of Truth

- Start every non-trivial session by reading `ai/README.md`, `ai/STATUS.md`,
  `ai/PLAN.md`, and `tk ready`.
- Treat `AGENTS.md` as stable operating policy.
- Treat `ai/STATUS.md` as current phase/focus/blockers.
- Treat `ai/PLAN.md` as the active work sequence.
- Treat `ai/DESIGN.md` and `ai/design/*` as architecture and contract detail.
- Treat `tk` as the ledger for exact work items, bug reports, root causes, and
  verification evidence.
- Keep `ai/` and `.tasks/` local-only operating context. Do not commit them
  unless the user explicitly changes that policy.
- Do not put session-specific task ids, bug transcripts, command output, or
  completion claims in this file. Put those in `tk` and, when they affect the
  project state, summarize them in `ai/STATUS.md` or `ai/PLAN.md`.

## Project Boundary

- Ion is the product: CLI/TUI UX, slash commands, provider/model selection,
  settings/state, display projection, workspace policy, and coding-tool UX.
- Ion's product roadmap is Pi -> Pi+: stabilize a Pi-level core first, then
  add extra capabilities only when they make the coding workflow better without
  destabilizing the core loop.
- Canto is the framework: agent loop, ordered runtime events, durable session
  history, provider-visible context, tool lifecycle, retry/cancel settlement,
  provider transforms, compaction, and harness primitives.
- Keep the Canto/Ion split. If Ion needs to duplicate framework-owned behavior,
  fix or extend Canto first, import the exact revision, then simplify Ion.
- The native Canto path is the primary path. ACP, subscription bridges,
  subagents, background agents, sandbox modes, branch views, workflows, and
  other Pi+ features stay parked unless `ai/PLAN.md` and `tk` explicitly
  promote one.
- Ion is `v0.0.0`. Clean breaks are allowed; do not add compatibility shims,
  fallback branches, or migration scaffolding unless the user asks for them.

## Core Product Bar

Phase 1 targets a stable Pi-level core, not a visual clone of Pi:

```text
submit -> stream -> tool call/result -> cancel/error -> persist -> replay/resume
```

- Use Pi as the primary internal reference for session history,
  provider-visible context, event ordering, tool lifecycle, cancel/error
  settlement, persistence/replay, and runtime state ownership.
- Use Codex app, Codex CLI, Claude Code, Amp, Droid, OpenCode, Cursor, Zed,
  and similar tools only as references for specific tradeoffs. Do not widen the
  core surface because another agent has a feature.
- Keep the default native path small and trusted until the core loop and TUI are
  boring under deterministic tests, tmux, and live-provider dogfood.

## Work Method

- Read before changing: target file, immediate callers, shared utilities,
  relevant Canto boundary code, `ai/` context, and the active `tk` task.
- Use the `go-expert` skill for non-trivial Go changes.
- Reproduce user-reported behavior before fixing when reproduction is possible.
- Prefer contract-first fixes over patch-first fixes in the core loop:
  1. identify the owner, Canto or Ion;
  2. add or update focused tests for the invariant;
  3. fix Canto first for framework-owned defects;
  4. import the exact Canto revision into Ion;
  5. simplify the Ion adapter/reducer after the contract is fixed.
- Make targeted rewrites of broken modules when they remove duplicate ownership
  or hidden state. Do not do broad churn unrelated to the task.
- Use `tk` for all multi-step work. Log concrete findings while they are fresh:
  files reviewed, root cause, fix, commands run, residual risk.
- Short prompts like `proceed`, `what's next`, or `hows it look` mean: verify
  repo truth first, select the next clear slice from `ai/`/`tk`, and execute it.

## Dogfood Regressions

User-reported Ion behavior bugs are dogfood regressions until proven otherwise.
Handle them with evidence, not memory.

- Before answering "do you remember", "is this fixed", "what was the last
  issue", or "how close are we", search `tk`, `.tasks/`, `ai/STATUS.md`,
  `ai/PLAN.md`, and recent commits for the exact symptom, error text,
  provider/model, command, session id, or file path.
- If no record exists, say so and create a focused `tk` task instead of
  guessing.
- Each regression task should capture: exact transcript/error snippet,
  provider/model/endpoint/command/session when known, expected versus actual
  behavior, owner area, likely files, repro, root cause, fix summary,
  deterministic/tmux/live evidence, and remaining caveats.
- Broad audit tasks are not substitutes for focused regression tasks. Close a
  regression only after the focused task records the fix, code/test paths,
  exact verification commands, and any scenario-matrix update.

## TUI Work

- Ion uses Bubble Tea v2. Use the `bubbletea` skill for non-trivial TUI
  changes.
- Keep the TUI as projection/control over runtime events. It must not own a
  second provider-visible transcript, second agent loop, or hidden session
  materializer.
- For TUI behavior, use tmux or another PTY text capture to test the real
  inline terminal path: launch header, composer, multiline input, transcript
  commits, separators, status/footer, pickers, cancel, resume, and resize.
- Unit tests are still required for reducer/state-machine behavior; tmux smoke
  catches terminal integration bugs those tests cannot see.

## Config And State

- Keep Ion global files under `~/.ion/`.
- Stable user-editable settings belong in `~/.ion/config.toml`.
- Mutable runtime choices belong in `~/.ion/state.toml`.
- Provider API keys entered through Ion belong in `~/.ion/credentials.toml`
  with private file permissions.
- Do not persist provider/model automatically at startup. Only explicit user
  edits or TUI actions should write settings/state.
- Prefer hardcoded defaults for Ion-owned behavior. Add persistent state only
  when the value is machine-owned and genuinely needs to survive sessions.

## Verification

- Run focused tests for the changed owner first.
- For normal Go changes, run `go test ./... -count=1 -timeout 300s`,
  `go vet ./...`, and `git diff --check`.
- Run race subsets for state-machine, cancellation, streaming, storage, or TUI
  concurrency changes.
- Run tmux smoke for TUI behavior changes.
- Run live provider smoke last for provider/tool-loop changes. Use the live
  provider named by `ai/STATUS.md`, the active task, or the user.
- Report exact commands. If a gate is skipped, say why.

## Commands

```bash
go test ./... -count=1 -timeout 300s
go vet ./...
git diff --check

tk ready
tk show <id>
tk log <id> "finding"
tk done <id>
```

## References

- `ai/README.md` - index of active context files.
- `ai/STATUS.md` - current phase, focus, blockers, and latest evidence.
- `ai/PLAN.md` - active work sequence and deferred work.
- `ai/DESIGN.md` - current architecture.
- `ai/DECISIONS.md` - durable decisions and recent decision log.
- `/Users/nick/github/badlogic/pi-mono` - primary phase-1 internal reference.
