# Agent Instructions

## Project

Ion is a terminal coding agent in Go, modeled on Pi (Node.js). Goal: a fast,
native daily-driver coding agent. Pi is the **reference implementation** — read
its source for behavior, but implement idiomatically in Go. Not strict parity.

Ion is `v0.0.0`. Clean breaks allowed. No shims, no compatibility layers, no v2
files. Fix the actual code.

**P1 bar:** `submit → stream → tool call/result → cancel/error → persist →
replay/resume`. This must work under tmux, race detector, and live providers.

## Session Start

1. `cat ai/brief.md` — current state
2. `tk ready` — next task
3. `git log --oneline -10` — recent changes

Update `ai/` as you work when state changes materially.

## Project Layout

- `internal/agent/` — agent loop, stream, tools, config (the core)
- `internal/harness/` — harness with hooks (wraps the agent)
- `session/` — session tree (source of truth), events, settings, projection
- `app/` — TUI (Bubble Tea v2), markdown, syntax highlighting
- `llm/` — LLM message types and providers (anthropic, openai, openrouter, etc.)
- `tool/` — tool interface and implementations (bash, edit, read, glob, grep)
- `tool/mcp/` — MCP client (stdio) and first-party MCP server
- `config/` — settings loading
- `archive/` — stale code, ignored (tests may fail there, not blocking)

## Reference Posture

**Pi is a reference, not a spec to copy line-by-line.** Pi source:
`~/.pi/agent/npm/node_modules/@earendil-works/pi-agent-core/dist/`

1. Read Pi's source for the feature you're building
2. Understand the invariant (what guarantee it provides)
3. Express it idiomatically in Go (errors not exceptions, channels not
   EventStream, defer not try/finally)
4. Document intentional divergence

## Implementation Rules

1. **Read Pi source first** — find the exact function, understand the behavior
2. **Fix actual code** — no shims, wrappers, or v2 files at v0
3. **Test behavior, not existence** — verify `GetLabel("x")` returns the right
   string, not just that it compiles
4. **One layer owns each guarantee** — no duplicate ownership across files
5. **Surgical changes** — no opportunistic reformat
6. **Root causes, not symptoms** — when a module repeatedly lets bugs through,
   rewrite it

## Verification

### What counts as evidence
- **Pi source**: file.js:line_number
- **Ion source**: file.go:line_number
- **Test output**: command + pass/fail with actual output
- **Behavioral proof**: terminal output showing it works

### What does NOT count
- "I implemented a function with that name"
- "The checklist says ✅"
- "Tests pass" (without showing which tests)
- "I remember doing this"

### Done workflow
1. Read Pi source for the feature
2. Implement in Ion's actual code (no shims)
3. Write behavioral test
4. Run test, show output
5. If substantial: spawn parallel reviewers for source comparison
6. Only then claim done

### Red flags — stop and re-audit
- "Phase 1 is complete" (said 3 times, found bugs each time)
- "I think it's done" without source comparison
- Checklist has all ✅ but no behavioral verification
- User says "are you sure?" — answer is "let me verify"

## Work Style

- Let errors propagate. Catch only to recover or add context.
- Commit after each coherent change set.
- `tk` for all multi-step work. Log findings while fresh.
- Short prompts (`proceed`, `what's next`) mean: verify repo truth first, pick
  the next slice from `ai/`/`tk`, execute.

## Architecture Constraints

- **Session tree is the source of truth.** All state (messages, model changes,
  thinking level, compaction) flows through the tree.
- **TUI is projection/control over runtime events.** It must not own a second
  agent loop, second transcript, or hidden session materializer.
- **Hooks are extension points.** Keep the core small; optional capabilities
  live behind explicit hook boundaries.

## Config And State

- Global files under `~/.ion/`: `config.toml` (settings), `state.toml` (runtime
  state), `credentials.toml` (API keys).
- Don't persist provider/model at startup. Only explicit user edits or TUI
  actions write settings/state.

## Commands

```bash
# Verification
go test ./... -count=1 -timeout 300s
go test -race ./internal/agent/ ./session/ ./app/ -count=1 -timeout 120s
go vet ./...

# Task tracking
tk ready          # what's next
tk show <id>      # task detail
tk log <id> "msg" # record finding
tk done <id>      # mark complete

# TUI changes: test in tmux. Unit tests for reducers; tmux smoke for integration.
```

Report exact commands run. If a gate is skipped, say why.

## Dogfood Regressions

User-reported behavior bugs are regressions until proven otherwise. Before
answering "is this fixed", search `tk`, `ai/STATUS.md`, recent commits. If no
record exists, create a `tk` task. Don't guess.

## References

**Active `ai/` files** (update as you work):
- `ai/brief.md` — current state (read every session)
- `ai/STATUS.md` — phase, focus, blockers
- `ai/PLAN.md` — active work sequence
- `ai/decisions.md` — principles + decision log
- `ai/architecture.md` — system overview
- `ai/journal.md` — session findings (append-only)
- `ai/COMPREHENSIVE-AUDIT.md` — source-level findings

**Archived** (historical only): `ai/archive/`
