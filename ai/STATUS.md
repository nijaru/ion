# ion Status

## Current State

| Metric    | Value               | Updated    |
| --------- | ------------------- | ---------- |
| Phase     | Tool pass           | 2026-02-05 |
| Status    | Planning            | 2026-02-05 |
| Toolchain | stable              | 2026-01-22 |
| Tests     | 323 passing         | 2026-02-05 |
| Clippy    | pedantic clean      | 2026-02-05 |
| TUI Lines | ~9,500 (excl tests) | 2026-02-04 |

## Current Focus

**Tool pass planned. Design at ai/design/tool-pass.md.**

Completed this session:

- TUI: Restored blank line separators between chat entries (trailing trim was too aggressive)
- TUI: Contextual units for collapsed tool results (list→items, grep→matches, read→lines)
- TUI: Fixed queued message prefix inconsistency (> → ›)
- Research: Working directory patterns across agents (ai/research/working-directory-patterns-2026.md)
- Tool audit: Compared with Claude Code, identified gaps and priorities

## Next

**Tool pass (P2):**

- tk-d7jh: Bash directory param + read-mode safe commands
- tk-rxsz: Grep context lines + output modes
- Design: ai/design/tool-pass.md

**New features (P3):**

- tk-75jw: Web search tool (DuckDuckGo scraping, free)
- tk-ltyy: ask_user tool (selector + text input UI)

**Existing (P2-P4):**

- tk-5j06: Memory system (P2)
- tk-epd1: TUI refactor - extract long event handlers (P4)
- tk-2bk7: Pre-ion scrollback preservation on resize (P3)

## Architecture Assessment (2026-02-04)

**Verdict:** Solid foundation (~60% complete), feature-incomplete vs competitors.

| Aspect                  | Status                             |
| ----------------------- | ---------------------------------- |
| Module separation       | Good - clean boundaries            |
| Agent loop              | Good - matches Claude Code pattern |
| Provider abstraction    | Good - 9 providers                 |
| Tool system             | Good - 8 tools, permissions, hooks |
| Memory system           | Missing - claimed differentiator   |
| Session branching       | Missing - linear only              |
| Extensibility           | Basic - hooks only                 |
| Subagent tool filtering | TODO in code                       |

## Deferred

- Plugin system - waiting for core completion
- Memory system (tk-5j06) - P2 after tool pass

## Key References

| Topic                  | Location                                       |
| ---------------------- | ---------------------------------------------- |
| Architecture           | ai/DESIGN.md                                   |
| TUI design             | ai/design/tui-v2.md                            |
| Tool pass design       | ai/design/tool-pass.md                         |
| Agent design           | ai/design/agent.md                             |
| Working dir patterns   | ai/research/working-directory-patterns-2026.md |
| Claude Code comparison | ai/research/claude-code-architecture.md        |
| Pi-mono comparison     | ai/research/pi-mono-architecture-2026.md       |
