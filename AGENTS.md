# Agent Instructions

Ion is an early-stage terminal coding agent. This file is stable operating
policy — not a changelog.

## Session Start

1. Read `ai/brief.md` (required — active context snapshot)
2. Read `ai/STATUS.md` (current phase and blockers)
3. Run `tk ready` to see next task
4. Before investigating: check `tk show <id>`, relevant `ai/` files, git history

**Update `ai/` as you work**, not just at session end. If you discover
something that changes the picture, update the relevant file immediately.

## Project

- Ion is the product: CLI/TUI UX, session management, provider integration,
  settings, and coding tools.
- Product roadmap: Pi-level core first, then Pi+ extras that improve the
  coding workflow without destabilizing the core loop.
- P1 bar: `submit → stream → tool call/result → cancel/error → persist →
  replay/resume`. This must work under tmux, race detector, and live providers.
- Ion is `v0.0.0`. Clean breaks are allowed. No compatibility shims, fallback
  branches, or migration scaffolding unless explicitly asked.
- The native Go path is primary. ACP, subagents, background agents, sandbox
  modes, workflows, and other Pi+ features stay parked until `ai/PLAN.md` and
  `tk` promote them.

## Work Method

**Always refactor towards the optimal solution.** Early-stage development means
every piece of debt compounds. Never leave cruft, half-baked workarounds, or
"good enough" patches. If code could be cleaner, simpler, or more correct —
fix it now.

**Read before changing.** Target file, immediate callers, shared utilities,
`ai/` context, and the active `tk` task.

**Contract-first fixes over patch-first.** When fixing core loop bugs:
1. Identify the owner (which layer owns the guarantee).
2. Add or update focused tests for the invariant.
3. Fix at the right layer; simplify duplicates after.

**Pi-parity work** (features or fixes that match Pi's behavior):

## Verification Standard

**Every claim of "done" needs evidence.** This applies to parity items,
bug fixes, and feature implementations.

### What counts as evidence
- **Source comparison**: Pi file + line numbers vs Ion file + line numbers
- **Test output**: command run + pass/fail result
- **Behavioral proof**: terminal output showing the feature works

### What does NOT count
- "I implemented a function with that name"
- "The checklist says ✅"
- "I remember doing this"
- "It should work based on the code I wrote"

### Pi-parity workflow
1. **Read Pi's source FIRST** — find the exact function in `pi-agent-core/dist/`
2. **Read Ion's implementation** — find the corresponding code
3. **Compare behavior** — same inputs → same outputs? Same edge cases handled?
4. **Record evidence** — Pi source lines, Ion source lines, test output
5. **Update PI-PARITY-CHECKLIST.md** — mark verified with evidence

### When you find a gap
- Add it to `ai/PI-PARITY-CHECKLIST.md` with status ❌ or ⚠️
- If it's a structural issue, add to `ai/PI-ARCHITECTURE-GAP-ANALYSIS.md`
- Log in `tk` with the finding
- Don't mark it done until verified

**Find the Go idiom** — JS exceptions vs Go error returns, EventStream vs
channels, try/finally vs defer. Same invariant, different mechanism.

**Implement with clear ownership** — one layer owns each guarantee. Document
it in comments where non-obvious. Verify with tests and race detector.

**Report honestly** — if you can't verify an item, say so. If an item is
partially done, say what's missing. Never round up to "done."

**Aggressive rewrites.** When a module has duplicate ownership, hidden state,
or architecture that has repeatedly let bugs through — rewrite it. The boundary
is relevance to correctness, not line-count.

**Short prompts** like `proceed`, `what's next`, or `hows it look` mean: verify
repo truth first, select the next clear slice from `ai/`/`tk`, and execute it.

**Use `tk` for all multi-step work.** Log concrete findings while they are
fresh: files reviewed, root cause, fix, commands run, residual risk.

## Dogfood Regressions

User-reported behavior bugs are regressions until proven otherwise. Handle with
evidence, not memory.

- Before answering "is this fixed" or "what was the last issue", search `tk`,
  `ai/STATUS.md`, `ai/PLAN.md`, and recent commits for the exact symptom.
- If no record exists, create a focused `tk` task. Don't guess.
- Each regression task captures: exact error snippet, provider/model/session,
  expected vs actual, root cause, fix, verification commands, remaining caveats.

## TUI

- Uses Bubble Tea v2. Use the `bubbletea` skill for non-trivial changes.
- The TUI is projection/control over runtime events. It must not own a second
  agent loop, second transcript, or hidden session materializer.
- Test TUI behavior in tmux or another PTY capture. Unit tests for reducers;
  tmux smoke for terminal integration.

## Config And State

- Global files under `~/.ion/`: `config.toml` (user settings), `state.toml`
  (runtime state), `credentials.toml` (API keys, private permissions).
- Don't persist provider/model at startup. Only explicit user edits or TUI
  actions should write settings/state.
- Hardcode defaults. Add persistent state only when it genuinely needs to
  survive sessions.

## Verification

```bash
# Standard gates (always run)
go test ./... -count=1 -timeout 300s
go vet ./...
git diff --check

# Concurrency-sensitive changes (streaming, state-machine, TUI, storage)
go test -race ./internal/agent/ ./session/ ./app/ -count=1 -timeout 120s

# TUI behavior changes
tmux smoke scripts

# Provider/tool-loop changes
# Use live provider from ai/STATUS.md or user
```

Report exact commands. If a gate is skipped, say why.

## Commands

```bash
tk ready          # what's next
tk show <id>      # task detail
tk log <id> "msg" # record finding
tk done <id>      # mark complete
```

## References

**Active ai/ files** (update as you work):
- `ai/brief.md` — active context snapshot (read every session)
- `ai/STATUS.md` — current phase, focus, blockers
- `ai/PLAN.md` — active work sequence
- `ai/decisions.md` — principles + decision log
- `ai/architecture.md` — system overview
- `ai/journal.md` — session findings (append-only)
- `ai/PI-PARITY-CHECKLIST.md` — parity tracking
- `ai/PI-ARCHITECTURE-GAP-ANALYSIS.md` — known gaps

**Archived** (historical reference only): `ai/archive/`

**Pi source** (read on-demand, not all at once):
`/Users/nick/.pi/agent/npm/node_modules/@earendil-works/pi-coding-agent/node_modules/@earendil-works/pi-agent-core/dist/`
