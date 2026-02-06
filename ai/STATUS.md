# ion Status

## Current State

| Metric    | Value               | Updated    |
| --------- | ------------------- | ---------- |
| Phase     | UX Polish           | 2026-02-05 |
| Status    | TUI stabilization   | 2026-02-05 |
| Toolchain | stable              | 2026-01-22 |
| Tests     | 323 passing         | 2026-02-05 |
| Clippy    | pedantic clean      | 2026-02-05 |
| TUI Lines | ~9,500 (excl tests) | 2026-02-04 |

## Current Focus

**TUI rendering stabilization (tk-67su).**

Completed this session:

- Fixed draw_direct clearing chat in row-tracking mode (min(old,new) regression)
- Fixed FocusGained resetting chat_row (gap on tab switch)
- Added dirty tracking to skip idle redraws (~20 saved/sec)
- Resize: Clear(All) + reprint all chat at new width
- All rendering paths verified: normal chat, resize, tab switch, selector, /clear

TUI rendering is now stable. Two-mode system (row-tracking + scroll) working correctly.

## Next

- tk-2bk7: Pre-ion scrollback preservation on resize (terminal limitation, low priority)
- tk-5j06: Memory system (P2)
- tk-epd1: TUI refactor - extract long event handlers (P4)

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

## Deferred

- Plugin system - waiting for core completion
- Memory system (tk-5j06) - P2 after TUI stable

## Key References

| Topic                  | Location                                 |
| ---------------------- | ---------------------------------------- |
| Architecture           | ai/DESIGN.md                             |
| TUI design             | ai/design/tui-v2.md                      |
| Agent design           | ai/design/agent.md                       |
| Claude Code comparison | ai/research/claude-code-architecture.md  |
| Pi-mono comparison     | ai/research/pi-mono-architecture-2026.md |
