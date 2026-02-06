# ion Status

## Current State

| Metric    | Value               | Updated    |
| --------- | ------------------- | ---------- |
| Phase     | Feature dev         | 2026-02-06 |
| Status    | Ready               | 2026-02-06 |
| Toolchain | stable              | 2026-01-22 |
| Tests     | 325 passing         | 2026-02-06 |
| Clippy    | pedantic clean      | 2026-02-06 |
| TUI Lines | ~9,500 (excl tests) | 2026-02-04 |

## Current Focus

**Tool pass complete.** All 4 items shipped:

- Bash: `directory` parameter (resolve relative to project root, sandbox check)
- Bash: Read-mode safe command allowlist (~50 prefixes, chain-aware)
- Grep: `output_mode` parameter (content/files/count)
- Grep: `context_before`/`context_after` with custom Sink implementation

## Next

**P2:**

- tk-5j06: Memory system (claimed differentiator, still missing)

**P3 features:**

- tk-75jw: Web search tool (DuckDuckGo scraping, free)
- tk-ltyy: ask_user tool (selector + text input UI)
- tk-2bk7: Pre-ion scrollback preservation on resize

**P4:**

- tk-epd1: TUI refactor - extract long event handlers

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
