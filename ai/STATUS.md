# ion Status

## Current State

| Metric    | Value                | Updated    |
| --------- | -------------------- | ---------- |
| Phase     | UX Polish            | 2026-02-05 |
| Status    | TUI stable, reviewed | 2026-02-05 |
| Toolchain | stable               | 2026-01-22 |
| Tests     | 323 passing          | 2026-02-05 |
| Clippy    | pedantic clean       | 2026-02-05 |
| TUI Lines | ~9,500 (excl tests)  | 2026-02-04 |

## Current Focus

**TUI rendering stabilization complete (tk-67su done).**

Session work:

- Fixed draw_direct clearing chat in row-tracking mode (min(old,new) regression)
- Fixed FocusGained resetting chat_row (gap on tab switch)
- Fixed duplicate UI in scrollback during overflow transition (clear from chat_row)
- Fixed duplicate tool display in ChatGPT provider (removed response.output_item.added)
- Removed trailing blank line between chat and progress bar
- Changed prompt symbol from > to › (all locations including queued preview)
- Added dirty tracking to skip idle redraws
- Removed dead code: stash, colored_bold, italic, last_render_width
- Simplified draw_direct: removed width_decreased/startup_ui_anchor paths
- Review passed (3 subagents): no critical issues

TUI rendering is stable. Two-mode system (row-tracking + scroll) working correctly.

## Next

- tk-2bk7: Pre-ion scrollback preservation on resize (terminal limitation, low priority)
- tk-5j06: Memory system (P2)
- tk-epd1: TUI refactor - extract long event handlers (P4)
  - Duplicated skip-agent logic in 3 places
  - run.rs main loop at 528 lines
  - u16 truncation edge cases (chat_lines.len(), render_input_direct)

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
