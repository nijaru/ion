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

**Sprint 15: OAuth subscription support.**

Completed:

- tk-f564 ✓ Gemini OAuth (credentials, model format, SSE parsing)
- Selector exit no longer clears screen
- Provider selector: warnings, Local last, column alignment

Next:

- tk-5q6b: Fix ChatGPT OAuth (investigate Codex CLI source)
- tk-67su: Review TUI clearing/repaint logic
- tk-5j06: Memory system (P2)

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
