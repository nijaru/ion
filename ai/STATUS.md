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

**UX backlog burndown.** Completed this session:

- tk-6k9u ✓ Scrollback audit
- tk-9s5m ✓ Blank screen gap verification
- tk-cslh ✓ CLI behavior review
- tk-qy6g ✓ Destructive command guard (rm -rf, git reset --hard, etc.)
- tk-6ydy ✓ Tool output format (✓/✗ icons, multi-param display)
- tk-g3dt ✓ Ctrl+R fuzzy history search
- tk-kwxn ✓ Deferred provider change until model selection
- tk-a4q5 ✓ Provider selector shows config id
- tk-vrmx ✓ CLI config subcommand (ion config get/set/path)
- tk-5zjg ✓ TUI minor fixes (resize helper, scroll limit)
- tk-u133 ✓ Model context window display (128K, 1M format)
- tk-r9c7 ✓ Model sorting (org → newest) - already implemented
- tk-x3zf ✓ Model display format - already correct
- tk-wj4b ✓ Newest sort option - already implemented
- tk-le7i ✓ Retry countdown timer (yellow spinner, countdown)
- tk-3xov ✓ Ctrl+A - already implemented in input.rs

Next priorities:

- Memory system (tk-5j06) - P2
- Web search tool (tk-1y3g) - matches Claude Code
- Settings selector UI (tk-4gm9)
- Remaining UX backlog (~6 tasks)

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

- OAuth subscription (ChatGPT, Gemini) - unofficial, deprioritized
- Plugin system - waiting for core completion
- Memory system (tk-5j06) - P2 after bugs fixed

## Key References

| Topic                  | Location                                 |
| ---------------------- | ---------------------------------------- |
| Architecture           | ai/DESIGN.md                             |
| TUI design             | ai/design/tui-v2.md                      |
| Agent design           | ai/design/agent.md                       |
| Claude Code comparison | ai/research/claude-code-architecture.md  |
| Pi-mono comparison     | ai/research/pi-mono-architecture-2026.md |
