# ion Status

## Current State

| Metric    | Value               | Updated    |
| --------- | ------------------- | ---------- |
| Phase     | UX Polish           | 2026-02-04 |
| Status    | Backlog burndown    | 2026-02-04 |
| Toolchain | stable              | 2026-01-22 |
| Tests     | 323 passing         | 2026-02-04 |
| Clippy    | pedantic clean      | 2026-02-04 |
| TUI Lines | ~9,500 (excl tests) | 2026-02-04 |

## Current Focus

**UX polish + OAuth fix (tk-f564).** Completed this session:

- Provider selector: "⚠ unofficial" warning for OAuth providers (dim yellow)
- Provider selector: Local provider sorts to bottom
- OAuth login: One-time warning about unofficial status
- Ollama → Local rename with ION_LOCAL_URL env var
- OpenRouter: Added `openrouter/free` as first model option

**In Progress:** Gemini OAuth fix (tk-f564)

- Root cause: Using Antigravity credentials/format instead of Gemini CLI
- gemini-cli source analyzed at `/Users/nick/github/google-gemini/gemini-cli`
- Fix needed: Update OAuth creds + request format (see tk-f564 log)

Next priorities:

- Fix Gemini OAuth (tk-f564) - credentials + request format
- Fix ChatGPT OAuth - similar investigation needed
- Memory system (tk-5j06) - P2
- Web search tool (tk-1y3g)

## Architecture Assessment (2026-02-04)

**Verdict:** Solid foundation (~60% complete), feature-incomplete vs competitors.

| Aspect                  | Status                             |
| ----------------------- | ---------------------------------- |
| Module separation       | Good - clean boundaries            |
| Agent loop              | Good - matches Claude Code pattern |
| Provider abstraction    | Good - 9 providers                 |
| Tool system             | Good - permissions, hooks          |
| Memory system           | Missing - claimed differentiator   |
| Session branching       | Missing - linear only              |
| Extensibility           | Basic - hooks only                 |
| Subagent tool filtering | TODO in code                       |

## Sprint 14 Progress

**Phase 1 ✓** - Panic fixes (5 bugs)
**Phase 2 ✓** - Dead code removal (-130 lines, Terminal struct)
**Phase 3 ✓** - Picker trait extraction (FilterablePicker<T>, ProviderPicker, SessionPicker)
**Phase 4 ✓** - Completer trait extraction (CompleterState<T>, FileCompleter, CommandCompleter)
**Phase 5 ✓** - App decomposition (TaskState, InteractionState)

See `ai/sprints/14-tui-refactoring.md` for full plan.

## Deferred

- Plugin system - waiting for core completion
- Memory system (tk-5j06) - P2 after OAuth fixed

## Key References

| Topic                  | Location                                 |
| ---------------------- | ---------------------------------------- |
| Architecture           | ai/DESIGN.md                             |
| TUI design             | ai/design/tui-v2.md                      |
| Agent design           | ai/design/agent.md                       |
| Claude Code comparison | ai/research/claude-code-architecture.md  |
| Pi-mono comparison     | ai/research/pi-mono-architecture-2026.md |
